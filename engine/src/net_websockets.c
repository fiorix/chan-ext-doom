//
// Copyright(C) 2005-2014 Simon Howard
// Copyright(C) 2021 Cloudflare - celso@cloudflare.com
//
// This program is free software; you can redistribute it and/or
// modify it under the terms of the GNU General Public License
// as published by the Free Software Foundation; either version 2
// of the License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// DESCRIPTION:
//     Networking module which carries the Chocolate wire protocol over a
//     WebSocket to a room router.
//
//     The router is a dumb relay keyed on node ids, so the envelope names
//     both endpoints. See net_ws_frame.h for the frame layout; the codec and
//     the receive ring live there because they are the parts worth testing
//     off-target.
//
//     Addresses follow the same refcount contract as net_sdl.c: the table
//     owns each entry and FreeAddress returns the slot, because net_io.c
//     calls FreeAddress once an address' refcount reaches zero.
//

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <emscripten/websocket.h>

#include "doomtype.h"
#include "i_system.h"
#include "m_argv.h"
#include "m_misc.h"
#include "net_defs.h"
#include "net_io.h"
#include "net_packet.h"
#include "net_websockets.h"
#include "net_ws_frame.h"
#include "z_zone.h"

typedef struct
{
    net_addr_t net_addr;
    uint32_t node;
} addrpair_t;

typedef enum
{
    WS_CLOSED,
    WS_CONNECTING,
    WS_OPEN,
    WS_FAILED,
} ws_state_t;

static EMSCRIPTEN_WEBSOCKET_T websocket;
static ws_state_t ws_state = WS_CLOSED;

// Set once the local role is known. The host has to announce itself to the
// router, but it can only do so on an open socket, so the announce is
// deferred rather than sent into a connecting socket and lost.

static uint32_t local_node;
static boolean local_node_set = false;
static boolean announce_pending = false;

static net_ws_queue_t recv_queue;

static addrpair_t **addr_table;
static int addr_table_size = -1;

// Counted rather than silently swallowed: a nonzero count is the first thing
// worth knowing when a session desyncs.

static unsigned int drops_bad_frame;
static unsigned int drops_not_open;

static void FreePacketItem(void *item)
{
    NET_FreePacket((net_packet_t *)item);
}

//
// Address table
//

static void InitAddrTable(void)
{
    addr_table_size = 16;

    addr_table = Z_Malloc(sizeof(addrpair_t *) * addr_table_size, PU_STATIC, 0);
    memset(addr_table, 0, sizeof(addrpair_t *) * addr_table_size);
}

static net_addr_t *FindAddress(uint32_t node)
{
    addrpair_t *new_entry;
    int empty_entry = -1;
    int i;

    if (addr_table_size < 0)
    {
        InitAddrTable();
    }

    for (i = 0; i < addr_table_size; ++i)
    {
        if (addr_table[i] != NULL && addr_table[i]->node == node)
        {
            return &addr_table[i]->net_addr;
        }

        if (empty_entry < 0 && addr_table[i] == NULL)
            empty_entry = i;
    }

    if (empty_entry < 0)
    {
        addrpair_t **new_addr_table;
        int new_addr_table_size;

        empty_entry = addr_table_size;

        new_addr_table_size = addr_table_size * 2;
        new_addr_table = Z_Malloc(sizeof(addrpair_t *) * new_addr_table_size,
                                  PU_STATIC, 0);
        memset(new_addr_table, 0, sizeof(addrpair_t *) * new_addr_table_size);
        memcpy(new_addr_table, addr_table,
               sizeof(addrpair_t *) * addr_table_size);
        Z_Free(addr_table);
        addr_table = new_addr_table;
        addr_table_size = new_addr_table_size;
    }

    new_entry = Z_Malloc(sizeof(addrpair_t), PU_STATIC, 0);

    new_entry->node = node;
    new_entry->net_addr.refcount = 0;
    new_entry->net_addr.handle = &new_entry->node;
    new_entry->net_addr.module = &net_websockets_module;

    addr_table[empty_entry] = new_entry;

    return &new_entry->net_addr;
}

static void NET_Websockets_FreeAddress(net_addr_t *addr)
{
    int i;

    for (i = 0; i < addr_table_size; ++i)
    {
        if (addr_table[i] != NULL && addr == &addr_table[i]->net_addr)
        {
            Z_Free(addr_table[i]);
            addr_table[i] = NULL;
            return;
        }
    }

    I_Error("NET_Websockets_FreeAddress: Attempted to remove an unused address!");
}

//
// Node identity
//

// A collision is not cosmetic: the router keys its session table on the node
// id, so two nodes sharing one id steal each other's traffic. Ids 0 and 1 are
// reserved, so a reserved draw is retried.

static uint32_t RandomNodeId(void)
{
    uint32_t node = 0;
    int attempt;

    for (attempt = 0; attempt < 16; ++attempt)
    {
        if (getentropy(&node, sizeof(node)) != 0)
        {
            node = 0;
        }
        else
        {
            node &= 0xffff;

            if (node != NET_WS_NODE_ROUTER && node != NET_WS_NODE_HOST)
            {
                return node;
            }
        }
    }

    // getentropy is backed by the browser CSPRNG and does not fail in
    // practice. A fixed fallback would guarantee the collision this is
    // avoiding, so refuse instead.

    I_Error("NET_Websockets: unable to draw a node id");
    return 0;
}

static void SetLocalNode(uint32_t node)
{
    if (local_node_set)
    {
        return;
    }

    local_node = node;
    local_node_set = true;
}

//
// WebSocket callbacks
//

static void SendAnnounce(void)
{
    byte frame[NET_WS_SEND_HEADER];
    size_t len;

    len = NET_WS_BuildFrame(frame, sizeof(frame), NET_WS_NODE_ROUTER,
                            local_node, NULL, 0);

    if (len == 0 ||
        emscripten_websocket_send_binary(websocket, frame, len) < 0)
    {
        printf("NET_Websockets: failed to announce node %u\n", local_node);
    }
}

static EM_BOOL OnOpen(int eventType, const EmscriptenWebSocketOpenEvent *e,
                      void *userData)
{
    ws_state = WS_OPEN;
    printf("NET_Websockets: connected as node %u\n", local_node);

    if (announce_pending)
    {
        announce_pending = false;
        SendAnnounce();
    }

    return EM_TRUE;
}

static EM_BOOL OnClose(int eventType, const EmscriptenWebSocketCloseEvent *e,
                       void *userData)
{
    printf("NET_Websockets: closed (clean=%d code=%d reason=%s)\n",
           e->wasClean, e->code, e->reason);

    ws_state = WS_CLOSED;

    // Anything still queued belongs to the session that just ended.

    NET_WS_QueueDrain(&recv_queue);

    return EM_TRUE;
}

static EM_BOOL OnError(int eventType, const EmscriptenWebSocketErrorEvent *e,
                       void *userData)
{
    printf("NET_Websockets: socket error\n");
    ws_state = WS_FAILED;
    return EM_TRUE;
}

static EM_BOOL OnMessage(int eventType, const EmscriptenWebSocketMessageEvent *e,
                         void *userData)
{
    net_packet_t *packet;
    const byte *payload;
    size_t payload_len;
    uint32_t from;

    if (e->isText)
    {
        // The router only sends binary frames. A text frame is its error
        // channel, so surface it rather than parsing it as a packet.

        printf("NET_Websockets: router said: %.*s\n", (int)e->numBytes,
               (const char *)e->data);
        return EM_TRUE;
    }

    if (!NET_WS_ParseFrame((const byte *)e->data, e->numBytes, &from,
                           &payload, &payload_len))
    {
        ++drops_bad_frame;
        return EM_TRUE;
    }

    packet = NET_NewPacket(payload_len);
    memcpy(packet->data, payload, payload_len);
    packet->len = payload_len;

    NET_WS_QueuePush(&recv_queue, packet, from);

    return EM_TRUE;
}

//
// Connection
//

static boolean InitWebSockets(void)
{
    EmscriptenWebSocketCreateAttributes attr;
    int wss;

    if (ws_state == WS_OPEN || ws_state == WS_CONNECTING)
    {
        return true;
    }

    if (ws_state == WS_FAILED)
    {
        return false;
    }

    if (!emscripten_websocket_is_supported())
    {
        printf("NET_Websockets: WebSockets are not available\n");
        ws_state = WS_FAILED;
        return false;
    }

    //!
    // @category net
    // @arg <url>
    //
    // Connect to the room router at the given WebSocket URL.
    //

    wss = M_CheckParmWithArgs("-wss", 1);

    if (wss <= 0)
    {
        printf("NET_Websockets: -wss <url> is required for a network game\n");
        ws_state = WS_FAILED;
        return false;
    }

    emscripten_websocket_init_create_attributes(&attr);
    attr.url = myargv[wss + 1];

    websocket = emscripten_websocket_new(&attr);

    if (websocket <= 0)
    {
        printf("NET_Websockets: unable to open %s\n", attr.url);
        ws_state = WS_FAILED;
        return false;
    }

    NET_WS_QueueInit(&recv_queue, FreePacketItem);

    emscripten_websocket_set_onopen_callback(websocket, NULL, OnOpen);
    emscripten_websocket_set_onclose_callback(websocket, NULL, OnClose);
    emscripten_websocket_set_onerror_callback(websocket, NULL, OnError);
    emscripten_websocket_set_onmessage_callback(websocket, NULL, OnMessage);

    // Deliberately not blocking. The reference module spins on
    // emscripten_sleep waiting for the socket, which requires ASYNCIFY and
    // stalls the whole module. The chocolate client already retries its SYN,
    // so returning now and dropping sends until the socket opens converges
    // without that cost.

    ws_state = WS_CONNECTING;
    printf("NET_Websockets: connecting to %s\n", attr.url);

    return true;
}

static boolean NET_Websockets_InitClient(void)
{
    SetLocalNode(RandomNodeId());

    return InitWebSockets();
}

static boolean NET_Websockets_InitServer(void)
{
    SetLocalNode(NET_WS_NODE_HOST);

    if (!InitWebSockets())
    {
        return false;
    }

    // The router reads a frame addressed to node 0 as the host claiming the
    // room, so it has to arrive on an open socket.

    if (ws_state == WS_OPEN)
    {
        SendAnnounce();
    }
    else
    {
        announce_pending = true;
    }

    return true;
}

static void NET_Websockets_SendPacket(net_addr_t *addr, net_packet_t *packet)
{
    byte *frame;
    size_t frame_len;
    size_t capacity;
    uint32_t to;

    if (addr == NULL || addr->handle == NULL)
    {
        return;
    }

    if (!InitWebSockets() || ws_state != WS_OPEN)
    {
        ++drops_not_open;
        return;
    }

    to = *((uint32_t *)addr->handle);
    capacity = packet->len + NET_WS_SEND_HEADER;
    frame = Z_Malloc(capacity, PU_STATIC, 0);

    frame_len = NET_WS_BuildFrame(frame, capacity, to, local_node,
                                  packet->data, packet->len);

    if (frame_len == 0)
    {
        // Only reachable if the packet exceeds the frame bound, which means
        // a caller bug rather than anything the peer did.

        ++drops_bad_frame;
        Z_Free(frame);
        return;
    }

    if (emscripten_websocket_send_binary(websocket, frame, frame_len) < 0)
    {
        // The socket is gone. Mark it closed so the next call reconnects
        // instead of sending into a dead handle forever.

        printf("NET_Websockets: send failed, dropping connection\n");
        ws_state = WS_CLOSED;
    }

    Z_Free(frame);
}

static boolean NET_Websockets_RecvPacket(net_addr_t **addr,
                                         net_packet_t **packet)
{
    void *item;
    uint32_t from;

    if (!NET_WS_QueuePop(&recv_queue, &item, &from))
    {
        return false;
    }

    *packet = (net_packet_t *)item;
    *addr = FindAddress(from);
    NET_ReferenceAddress(*addr);

    return true;
}

static void NET_Websockets_AddrToString(net_addr_t *addr, char *buffer,
                                        int buffer_len)
{
    M_snprintf(buffer, buffer_len, "ws node %u", *((uint32_t *)addr->handle));
}

// The only address a client resolves is the room host, named by node id.

static net_addr_t *NET_Websockets_ResolveAddress(const char *address)
{
    unsigned long node;
    char *end;

    if (address == NULL)
    {
        return FindAddress(NET_WS_NODE_HOST);
    }

    node = strtoul(address, &end, 10);

    if (end == address || *end != '\0' || node > 0xffffffffUL)
    {
        printf("NET_Websockets: '%s' is not a node id\n", address);
        return NULL;
    }

    return FindAddress((uint32_t)node);
}

net_module_t net_websockets_module = {
    NET_Websockets_InitClient,
    NET_Websockets_InitServer,
    NET_Websockets_SendPacket,
    NET_Websockets_RecvPacket,
    NET_Websockets_AddrToString,
    NET_Websockets_FreeAddress,
    NET_Websockets_ResolveAddress,
};
