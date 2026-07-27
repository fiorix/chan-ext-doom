//
// Copyright(C) 2005-2014 Simon Howard
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
//     Room-router envelope codec and the bounded receive ring.
//
//     Split out of net_websockets.c so it carries no emscripten dependency:
//     these are the parts that must reject a malformed frame and must drop
//     rather than grow without bound, so they are the parts worth testing on
//     the host.
//

#ifndef NET_WS_FRAME_H
#define NET_WS_FRAME_H

#include "doomtype.h"

// Envelope widths, in bytes. The router strips the "to" field on the way
// back, so the two directions differ.

#define NET_WS_SEND_HEADER 8
#define NET_WS_RECV_HEADER 4

// Reserved node ids. 0 is the router itself: a frame addressed to it is the
// host claiming the room, which resets any session already there.

#define NET_WS_NODE_ROUTER 0
#define NET_WS_NODE_HOST   1

// Longest frame accepted in either direction. A Chocolate packet never comes
// close; the bound exists so a corrupt or hostile length cannot drive an
// unbounded allocation.

#define NET_WS_MAX_FRAME 4096

// Receive ring capacity. At 35 Hz lockstep there is no value in buffering
// further: a peer this far behind is already desynced, so dropping is the
// honest outcome and memory stays bounded.

#define NET_WS_RECV_QUEUE 128

void NET_WS_WriteU32LE(byte *p, uint32_t v);
uint32_t NET_WS_ReadU32LE(const byte *p);

// Builds an outbound frame: [to][from][payload]. Returns the frame length,
// or 0 if the payload does not fit, so the caller cannot send a truncated
// frame by ignoring the result.

size_t NET_WS_BuildFrame(byte *out, size_t out_len, uint32_t to, uint32_t from,
                         const byte *payload, size_t payload_len);

// Parses an inbound frame: [from][payload]. Returns false and touches no
// output on a frame that is too short to hold the envelope or too long to
// trust. payload_len may be 0; a bare envelope is well formed.

boolean NET_WS_ParseFrame(const byte *frame, size_t frame_len, uint32_t *from,
                          const byte **payload, size_t *payload_len);

// Bounded ring of received items. Items are opaque pointers so this stays
// free of the packet allocator; the owner supplies free_item so a dropped
// item is released rather than leaked.

typedef void (*net_ws_free_item_t)(void *item);

typedef struct
{
    void *items[NET_WS_RECV_QUEUE];
    uint32_t froms[NET_WS_RECV_QUEUE];
    int head, tail;
    net_ws_free_item_t free_item;
    unsigned int drops;
} net_ws_queue_t;

void NET_WS_QueueInit(net_ws_queue_t *queue, net_ws_free_item_t free_item);

// Returns false when the ring is full, having freed the item. Full is a
// normal condition under load, not an error the caller must handle.

boolean NET_WS_QueuePush(net_ws_queue_t *queue, void *item, uint32_t from);

boolean NET_WS_QueuePop(net_ws_queue_t *queue, void **item, uint32_t *from);

void NET_WS_QueueDrain(net_ws_queue_t *queue);

#endif /* #ifndef NET_WS_FRAME_H */
