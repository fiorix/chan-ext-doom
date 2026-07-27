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

#include <string.h>

#include "net_ws_frame.h"

// Byte at a time rather than a cast: the frame buffer has no alignment
// guarantee, and reading through a uint32_t pointer would also inherit the
// host's byte order. The wire is little-endian regardless of host.

void NET_WS_WriteU32LE(byte *p, uint32_t v)
{
    p[0] = (byte)(v & 0xff);
    p[1] = (byte)((v >> 8) & 0xff);
    p[2] = (byte)((v >> 16) & 0xff);
    p[3] = (byte)((v >> 24) & 0xff);
}

uint32_t NET_WS_ReadU32LE(const byte *p)
{
    return (uint32_t)p[0]
         | ((uint32_t)p[1] << 8)
         | ((uint32_t)p[2] << 16)
         | ((uint32_t)p[3] << 24);
}

size_t NET_WS_BuildFrame(byte *out, size_t out_len, uint32_t to, uint32_t from,
                         const byte *payload, size_t payload_len)
{
    size_t frame_len;

    if (payload_len > NET_WS_MAX_FRAME - NET_WS_SEND_HEADER)
    {
        return 0;
    }

    frame_len = payload_len + NET_WS_SEND_HEADER;

    if (frame_len > out_len)
    {
        return 0;
    }

    NET_WS_WriteU32LE(&out[0], to);
    NET_WS_WriteU32LE(&out[4], from);

    if (payload_len > 0)
    {
        memcpy(&out[NET_WS_SEND_HEADER], payload, payload_len);
    }

    return frame_len;
}

boolean NET_WS_ParseFrame(const byte *frame, size_t frame_len, uint32_t *from,
                          const byte **payload, size_t *payload_len)
{
    // Checked before any arithmetic. frame_len is unsigned, so a frame
    // shorter than the envelope would wrap into a huge payload length and
    // take the copy with it.

    if (frame_len < NET_WS_RECV_HEADER)
    {
        return false;
    }

    if (frame_len > NET_WS_MAX_FRAME)
    {
        return false;
    }

    *from = NET_WS_ReadU32LE(frame);
    *payload = frame + NET_WS_RECV_HEADER;
    *payload_len = frame_len - NET_WS_RECV_HEADER;

    return true;
}

void NET_WS_QueueInit(net_ws_queue_t *queue, net_ws_free_item_t free_item)
{
    memset(queue, 0, sizeof(*queue));
    queue->free_item = free_item;
}

boolean NET_WS_QueuePush(net_ws_queue_t *queue, void *item, uint32_t from)
{
    int new_tail;

    new_tail = (queue->tail + 1) % NET_WS_RECV_QUEUE;

    if (new_tail == queue->head)
    {
        // The item is ours once pushed, so dropping it means freeing it.
        // Returning without freeing is how the reference implementation
        // leaked one packet per overflow.

        if (queue->free_item != NULL)
        {
            queue->free_item(item);
        }

        ++queue->drops;
        return false;
    }

    queue->items[queue->tail] = item;
    queue->froms[queue->tail] = from;
    queue->tail = new_tail;

    return true;
}

boolean NET_WS_QueuePop(net_ws_queue_t *queue, void **item, uint32_t *from)
{
    if (queue->tail == queue->head)
    {
        return false;
    }

    *item = queue->items[queue->head];
    *from = queue->froms[queue->head];
    queue->items[queue->head] = NULL;
    queue->head = (queue->head + 1) % NET_WS_RECV_QUEUE;

    return true;
}

void NET_WS_QueueDrain(net_ws_queue_t *queue)
{
    void *item;
    uint32_t from;

    while (NET_WS_QueuePop(queue, &item, &from))
    {
        if (queue->free_item != NULL)
        {
            queue->free_item(item);
        }
    }
}
