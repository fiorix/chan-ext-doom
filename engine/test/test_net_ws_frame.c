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
//     Host tests for the room-router envelope codec and receive ring.
//
//     These run natively: the unit under test carries no emscripten
//     dependency precisely so the length checks and the drop path can be
//     exercised without a browser.
//
//     Each check that guards against a specific defect also asserts what the
//     unguarded arithmetic would have produced, so a regression that removes
//     the guard fails here rather than passing quietly.
//

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net_ws_frame.h"

static int failures;
static int checks;

static void Check(boolean ok, const char *what)
{
    ++checks;

    if (!ok)
    {
        ++failures;
        printf("FAIL: %s\n", what);
    }
}

static void TestByteOrder(void)
{
    byte buf[4];
    uint32_t v = 0x01020304u;

    NET_WS_WriteU32LE(buf, v);

    Check(buf[0] == 0x04 && buf[1] == 0x03 && buf[2] == 0x02 && buf[3] == 0x01,
          "u32 is written little-endian regardless of host order");
    Check(NET_WS_ReadU32LE(buf) == v, "u32 round-trips");

    // High bit set: read through a signed char would sign-extend and corrupt
    // the value, which is how the reference implementation built its ids.
    {
        byte high[4] = { 0x00, 0x00, 0x00, 0x80 };
        Check(NET_WS_ReadU32LE(high) == 0x80000000u,
              "high bit does not sign-extend");
    }
}

static void TestBuildFrame(void)
{
    byte out[NET_WS_MAX_FRAME];
    byte payload[8] = { 1, 2, 3, 4, 5, 6, 7, 8 };
    size_t len;

    len = NET_WS_BuildFrame(out, sizeof(out), 0x11223344u, 0x55667788u,
                            payload, sizeof(payload));

    Check(len == sizeof(payload) + NET_WS_SEND_HEADER, "frame length is payload + 8");
    Check(NET_WS_ReadU32LE(&out[0]) == 0x11223344u, "'to' lands first");
    Check(NET_WS_ReadU32LE(&out[4]) == 0x55667788u, "'from' lands second");
    Check(memcmp(&out[NET_WS_SEND_HEADER], payload, sizeof(payload)) == 0,
          "payload follows the envelope");

    // A bare envelope is the host announce frame, and must be well formed.
    len = NET_WS_BuildFrame(out, sizeof(out), NET_WS_NODE_ROUTER,
                            NET_WS_NODE_HOST, NULL, 0);
    Check(len == NET_WS_SEND_HEADER, "announce frame is exactly the envelope");

    // Refuses rather than truncating when the destination cannot hold it.
    len = NET_WS_BuildFrame(out, 4, 1, 2, payload, sizeof(payload));
    Check(len == 0, "undersized output buffer is refused");

    // Refuses a payload that would exceed the frame bound. The output buffer
    // here is deliberately large enough to hold it, because that is what
    // production does: SendPacket sizes the buffer from the packet, so the
    // capacity check can never fire and the frame bound is the only thing
    // standing between an oversize packet and the wire.
    {
        static byte big[NET_WS_MAX_FRAME * 2];
        static byte roomy[NET_WS_MAX_FRAME * 2 + NET_WS_SEND_HEADER];

        len = NET_WS_BuildFrame(roomy, sizeof(roomy), 1, 2, big, sizeof(big));
        Check(len == 0, "oversize payload is refused even when the buffer fits");
    }
}

static void TestParseFrameRejectsShort(void)
{
    byte frame[NET_WS_RECV_HEADER] = { 0xaa, 0xbb, 0xcc, 0xdd };
    const byte *payload;
    size_t payload_len;
    uint32_t from;
    size_t len;

    // The defect this guards: frame_len is unsigned, so any length below the
    // envelope width wraps when the header is subtracted. Assert both that
    // we reject it and that the unguarded expression really would have
    // produced an enormous length, so this check cannot be deleted as
    // redundant.
    for (len = 0; len < NET_WS_RECV_HEADER; ++len)
    {
        size_t unguarded = len - NET_WS_RECV_HEADER;

        Check(unguarded > (size_t)NET_WS_MAX_FRAME,
              "unguarded subtraction really does underflow");
        Check(!NET_WS_ParseFrame(frame, len, &from, &payload, &payload_len),
              "short frame is rejected before the subtraction");
    }
}

static void TestParseFrameRejectsOversize(void)
{
    static byte frame[NET_WS_MAX_FRAME + 16];
    const byte *payload;
    size_t payload_len;
    uint32_t from;

    Check(!NET_WS_ParseFrame(frame, sizeof(frame), &from, &payload, &payload_len),
          "frame beyond the bound is rejected");
    Check(NET_WS_ParseFrame(frame, NET_WS_MAX_FRAME, &from, &payload, &payload_len),
          "frame exactly at the bound is accepted");
}

static void TestParseFrameAccepts(void)
{
    byte frame[NET_WS_RECV_HEADER + 3];
    const byte *payload;
    size_t payload_len;
    uint32_t from;

    NET_WS_WriteU32LE(frame, 0xdeadbeefu);
    frame[4] = 9;
    frame[5] = 8;
    frame[6] = 7;

    Check(NET_WS_ParseFrame(frame, sizeof(frame), &from, &payload, &payload_len),
          "well-formed frame is accepted");
    Check(from == 0xdeadbeefu, "'from' is decoded");
    Check(payload_len == 3, "payload length excludes the envelope");
    Check(payload[0] == 9 && payload[1] == 8 && payload[2] == 7,
          "payload points past the envelope");

    // A bare envelope carries no payload but is still valid.
    Check(NET_WS_ParseFrame(frame, NET_WS_RECV_HEADER, &from, &payload, &payload_len),
          "bare envelope is accepted");
    Check(payload_len == 0, "bare envelope has an empty payload");
}

//
// Queue
//

static int live_items;

static void FreeItem(void *item)
{
    --live_items;
    free(item);
}

static void *NewItem(int value)
{
    int *item = malloc(sizeof(int));
    *item = value;
    ++live_items;
    return item;
}

static void TestQueueFifo(void)
{
    net_ws_queue_t queue;
    void *item;
    uint32_t from;
    int i;

    live_items = 0;
    NET_WS_QueueInit(&queue, FreeItem);

    Check(!NET_WS_QueuePop(&queue, &item, &from), "empty queue pops nothing");

    for (i = 0; i < 10; ++i)
    {
        Check(NET_WS_QueuePush(&queue, NewItem(i), (uint32_t)(100 + i)),
              "push into a queue with room succeeds");
    }

    for (i = 0; i < 10; ++i)
    {
        Check(NET_WS_QueuePop(&queue, &item, &from), "pop returns an item");
        Check(*(int *)item == i, "items come back in order");
        Check(from == (uint32_t)(100 + i), "sender travels with the item");
        FreeItem(item);
    }

    Check(live_items == 0, "no items leaked");
}

static void TestQueueOverflowDropsAndFrees(void)
{
    net_ws_queue_t queue;
    int i;

    live_items = 0;
    NET_WS_QueueInit(&queue, FreeItem);

    // A ring of N slots holds N-1 items: the full and empty states are told
    // apart by head == tail.
    for (i = 0; i < NET_WS_RECV_QUEUE - 1; ++i)
    {
        Check(NET_WS_QueuePush(&queue, NewItem(i), 0),
              "queue accepts up to capacity");
    }

    Check(live_items == NET_WS_RECV_QUEUE - 1, "all accepted items are live");
    Check(queue.drops == 0, "nothing dropped yet");

    // The defect this guards: the reference implementation returned early on
    // a full queue without freeing, leaking one packet per overflow.
    for (i = 0; i < 50; ++i)
    {
        Check(!NET_WS_QueuePush(&queue, NewItem(i), 0),
              "push into a full queue is refused");
    }

    Check(queue.drops == 50, "every refusal is counted");
    Check(live_items == NET_WS_RECV_QUEUE - 1,
          "refused items are freed, not leaked");

    NET_WS_QueueDrain(&queue);
    Check(live_items == 0, "drain releases everything");
}

int main(void)
{
    TestByteOrder();
    TestBuildFrame();
    TestParseFrameRejectsShort();
    TestParseFrameRejectsOversize();
    TestParseFrameAccepts();
    TestQueueFifo();
    TestQueueOverflowDropsAndFrees();

    printf("%d checks, %d failures\n", checks, failures);

    return failures == 0 ? 0 : 1;
}
