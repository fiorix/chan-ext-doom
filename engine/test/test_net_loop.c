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
//     Host test for the loopback transport's bounded queue.
//
//     net_loop carries the in-WASM host talking to its own client, so it sits
//     on the browser network path. Both of its send paths hand the queue a
//     NET_PacketDup, which the queue owns. Upstream drops that packet on a
//     full ring without freeing it, leaking one per overflow.
//
//     The allocator is stubbed with counters, so the test asserts the real
//     property: every packet the transport takes ownership of is eventually
//     released.
//

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net_defs.h"
#include "net_loop.h"
#include "net_packet.h"

// net_loop.c's ring, mirrored here so the test can overflow it deliberately.
#define LOOP_QUEUE_SIZE 16

static int checks;
static int failures;

static void Check(boolean ok, const char *what)
{
    ++checks;

    if (!ok)
    {
        ++failures;
        printf("FAIL: %s\n", what);
    }
}

//
// Allocator stub. z_native.c would drag in the whole zone allocator and
// i_system; counting mallocs is what this test actually needs.
//

int live_allocs;

void *Z_Malloc(int size, int tag, void *user)
{
    (void) tag;
    (void) user;
    ++live_allocs;
    return malloc(size);
}

void Z_Free(void *ptr)
{
    --live_allocs;
    free(ptr);
}

void I_Error(const char *error, ...)
{
    printf("FAIL: unexpected I_Error: %s\n", error);
    exit(1);
}

// Stubbed rather than linked: m_misc.c reaches i_swap.h and so needs SDL
// headers, which this host does not have. Neither helper is on the path
// under test.

boolean M_StringCopy(char *dest, const char *src, size_t dest_size)
{
    if (dest_size == 0)
    {
        return false;
    }

    strncpy(dest, src, dest_size - 1);
    dest[dest_size - 1] = '\0';

    return strlen(src) < dest_size;
}

int M_snprintf(char *buf, size_t buf_len, const char *s, ...)
{
    (void) s;

    if (buf_len > 0)
    {
        buf[0] = '\0';
    }

    return 0;
}

int main(void)
{
    net_addr_t *addr;
    net_packet_t *packet;
    net_packet_t *received;
    int i;

    net_loop_client_module.InitClient();
    net_loop_server_module.InitServer();

    addr = net_loop_client_module.ResolveAddress(NULL);

    // A ring of N slots holds N-1 entries, so pushing well past that
    // guarantees the overflow path runs many times.
    {
        int before;
        int per_packet;

        // Measured, not assumed: a packet costs more than one allocation
        // (the struct and its data buffer), and that is an implementation
        // detail this test should not hardcode.
        before = live_allocs;
        packet = NET_NewPacket(4);
        per_packet = live_allocs - before;
        NET_FreePacket(packet);

        Check(per_packet > 0, "the allocator stub sees packet allocations");

        before = live_allocs;

        for (i = 0; i < LOOP_QUEUE_SIZE * 4; ++i)
        {
            packet = NET_NewPacket(4);
            packet->len = 4;
            net_loop_client_module.SendPacket(addr, packet);
            NET_FreePacket(packet);
        }

        // Whatever the ring kept is bounded by its capacity; whatever it
        // refused was freed rather than leaked. Without the fix this grows
        // with the iteration count instead.
        Check(live_allocs - before <= per_packet * (LOOP_QUEUE_SIZE - 1),
              "overflowing the loopback queue does not grow allocations "
              "beyond the ring");
    }

    // Drain what the ring did keep, and confirm the transport ends up owning
    // nothing: with the upstream leak this count stays stuck above zero.
    {
        int drained = 0;

        while (net_loop_server_module.RecvPacket(&addr, &received))
        {
            NET_FreePacket(received);
            ++drained;
        }

        Check(drained > 0, "the queue delivered the packets it accepted");
        Check(drained <= LOOP_QUEUE_SIZE - 1,
              "the queue never held more than its bound");
        Check(live_allocs == 0,
              "every packet the loopback transport took ownership of is freed");
    }

    printf("%d checks, %d failures\n", checks, failures);

    return failures == 0 ? 0 : 1;
}
