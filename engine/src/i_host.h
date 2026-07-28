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
//     The seam between the engine and whatever is hosting it.
//
//     The browser host is a page that listens for CustomEvents and reads
//     finished files out of the Emscripten FS. A native host is a terminal:
//     there is no page to notify, and its files are already on a real disk
//     where the user asked for them. Every point where the engine tells its
//     host something goes through here, so a native build carries no browser
//     API and no EM_ASM at all.
//
//     Adding a notification means adding it to both implementations. That is
//     the point: the alternative is an #ifdef at each call site, and those
//     rot silently because nothing makes the native side fail to build.
//

#ifndef __I_HOST__
#define __I_HOST__

#include "doomtype.h"

#ifdef __EMSCRIPTEN__

#include <emscripten.h>

#else

// Marks a symbol the browser host calls in through. A native build exports
// nothing and links normally without it.

#define EMSCRIPTEN_KEEPALIVE

#endif

// Progress through a game. The browser page drives its own UI from these.

void I_HostLevelLoaded(const char *mapname);

void I_HostLevelCompleted(const char *mapname, int maxkills, int maxitems,
                          int maxsecret, int partime, int killcount,
                          int itemcount, int secretcount, int leveltime);

void I_HostGameStarted(int skill, int episode, int map);

void I_HostKill(const char *mapname, const char *source);

// A file the engine has finished writing into its filesystem. The browser
// hands it to the page as an object URL and unlinks it; a native host leaves
// it on disk, which is where it was wanted in the first place.

void I_HostFileReady(const char *event, const char *filename,
                     const char *mime);

void I_HostSaveWritten(const char *filename);

// Lifecycle.

void I_HostError(const char *message);

void I_HostEndoom(void);

// The browser canvas rescales when the framebuffer geometry changes, once for
// the game view and once for the wider ENDOOM text screen.

void I_HostResizeCanvas(boolean textmode);

boolean I_HostIsMobile(void);

#endif
