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
//     Waiting for the room host to start the game.
//
//     Upstream draws a lobby with the textscreen widget library. This tree
//     has no textscreen library, and the surfaces that embed this engine own
//     their own lobby anyway: the browser loader page in front of the WASM
//     module, and the host program for a native embedding. So the wait is
//     headless here and the lobby state it would have drawn is reported on
//     stdout, which is the browser console.
//

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "doomtype.h"
#include "i_system.h"
#include "i_timer.h"
#include "m_argv.h"
#include "net_client.h"
#include "net_gui.h"
#include "net_query.h"
#include "net_server.h"

static int expected_nodes;

// Reported once, not every poll: at 10 Hz a mismatch would otherwise bury
// the rest of the startup log.

static boolean had_warning;

static void PrintSHA1Digest(const char *s, const byte *digest)
{
    unsigned int i;

    printf("%s: ", s);

    for (i = 0; i < sizeof(sha1_digest_t); ++i)
    {
        printf("%02x", digest[i]);
    }

    printf("\n");
}

// A mismatch here is the usual cause of an immediate desync, because the
// peers are simulating different data. It is a warning rather than a fatal
// error to match upstream, which lets the player continue at their own risk.

static void CheckSHA1Sums(void)
{
    boolean correct_wad, correct_deh;
    boolean same_freedoom;

    if (!net_client_received_wait_data || had_warning)
    {
        return;
    }

    correct_wad = memcmp(net_local_wad_sha1sum,
                         net_client_wait_data.wad_sha1sum,
                         sizeof(sha1_digest_t)) == 0;
    correct_deh = memcmp(net_local_deh_sha1sum,
                         net_client_wait_data.deh_sha1sum,
                         sizeof(sha1_digest_t)) == 0;
    same_freedoom = net_client_wait_data.is_freedoom == net_local_is_freedoom;

    if (correct_wad && correct_deh && same_freedoom)
    {
        return;
    }

    if (!correct_wad)
    {
        printf("Warning: WAD SHA1 does not match server:\n");
        PrintSHA1Digest("Local", net_local_wad_sha1sum);
        PrintSHA1Digest("Server", net_client_wait_data.wad_sha1sum);
    }

    if (!same_freedoom)
    {
        printf("Warning: %s and server is %s\n",
               net_local_is_freedoom ? "Client is Freedoom" : "Client is not Freedoom",
               net_client_wait_data.is_freedoom ? "Freedoom" : "not Freedoom");
    }

    if (!correct_deh)
    {
        printf("Warning: Dehacked SHA1 does not match server:\n");
        PrintSHA1Digest("Local", net_local_deh_sha1sum);
        PrintSHA1Digest("Server", net_client_wait_data.deh_sha1sum);
    }

    printf("Warning: This may cause the game to desync.\n");

    had_warning = true;
}

static void ParseCommandLineArgs(void)
{
    int i;

    //!
    // @arg <n>
    // @category net
    //
    // Autostart the netgame when n nodes (clients) have joined the server.
    //

    i = M_CheckParmWithArgs("-nodes", 1);

    if (i > 0)
    {
        expected_nodes = atoi(myargv[i + 1]);
    }
}

static void CheckAutoLaunch(void)
{
    int nodes;

    if (net_client_received_wait_data
     && net_client_wait_data.is_controller
     && expected_nodes > 0)
    {
        nodes = net_client_wait_data.num_players
              + net_client_wait_data.num_drones;

        if (nodes >= expected_nodes)
        {
            printf("NET_WaitForLaunch: starting with %d nodes\n", nodes);
            NET_CL_LaunchGame();
            expected_nodes = 0;
        }
    }
}

void NET_WaitForLaunch(void)
{
    ParseCommandLineArgs();
    had_warning = false;

    while (net_waiting_for_launch)
    {
        CheckAutoLaunch();
        CheckSHA1Sums();

        NET_CL_Run();
        NET_SV_Run();

        if (!net_client_connected)
        {
            I_Error("Lost connection to server");
        }

        I_Sleep(100);
    }
}
