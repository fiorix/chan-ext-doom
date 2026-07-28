//
// Copyright(C) 1993-1996 Id Software, Inc.
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
//     Host notifications for a native build, where there is no host page.
//
//     These are deliberately silent rather than logged. Every one of them
//     duplicates something the engine already reports through its ordinary
//     output or already wrote to disk, so printing them again would add noise
//     to a terminal for no reader. I_Error is the clearest case: the caller
//     prints the same message immediately afterwards.
//
//     The file notifications are the interesting ones. In the browser the
//     engine writes into a virtual filesystem the user cannot reach, so the
//     file has to be handed to the page and then unlinked. Natively the file
//     is already on disk where it was asked for, so the correct native
//     behaviour is to leave it exactly there and say nothing.
//

#include "i_host.h"

void I_HostLevelLoaded(const char *mapname)
{
    (void) mapname;
}

void I_HostLevelCompleted(const char *mapname, int maxkills, int maxitems,
                          int maxsecret, int partime, int killcount,
                          int itemcount, int secretcount, int leveltime)
{
    (void) mapname;
    (void) maxkills;
    (void) maxitems;
    (void) maxsecret;
    (void) partime;
    (void) killcount;
    (void) itemcount;
    (void) secretcount;
    (void) leveltime;
}

void I_HostGameStarted(int skill, int episode, int map)
{
    (void) skill;
    (void) episode;
    (void) map;
}

void I_HostKill(const char *mapname, const char *source)
{
    (void) mapname;
    (void) source;
}

void I_HostFileReady(const char *event, const char *filename, const char *mime)
{
    // Left on disk on purpose. See the note above.

    (void) event;
    (void) filename;
    (void) mime;
}

void I_HostSaveWritten(const char *filename)
{
    (void) filename;
}

void I_HostError(const char *message)
{
    (void) message;
}

void I_HostEndoom(void)
{
}

void I_HostResizeCanvas(boolean textmode)
{
    (void) textmode;
}

boolean I_HostIsMobile(void)
{
    return false;
}
