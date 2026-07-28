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
//     The sound tables must be exactly as long as the enums that index them.
//
//     S_Start initialises every entry from 1 to NUMSFX, and S_ChangeMusic
//     indexes S_music by musicnum. Both trust the enum, so a table shorter
//     than its enum is not a missing-entry bug that shows up as silence: it
//     is an out-of-bounds write over whatever the linker placed next. In this
//     tree S_music follows S_sfx, so a short S_sfx overwrites music lump
//     numbers with -1 before anything looks them up, and the failure surfaces
//     far away as W_CacheLumpNum(-1) during level load.
//
//     This links the production tables rather than a copy, so it measures
//     what actually ships.
//

#include <stdio.h>

#include "doomtype.h"
#include "sounds.h"

extern sfxinfo_t S_sfx[];
extern musicinfo_t S_music[];

// Defined by the production sounds.c through its own translation unit; these
// are the real array extents, which C exposes only where the definition is
// visible. The linker gives us the symbols, so the lengths come from the
// section sizes the definitions produced.

static int checks = 0;
static int failures = 0;

static void Check(boolean ok, const char *what)
{
    ++checks;

    if (!ok)
    {
        ++failures;
        printf("FAIL: %s\n", what);
    }
}

int main(void)
{
    int i;

    // Every enum index must name a real entry. Reading S_sfx[NUMSFX - 1] is
    // only safe if the table really is that long, which is the whole point.

    for (i = 1; i < NUMSFX; ++i)
    {
        if (S_sfx[i].name[0] == '\0' && S_sfx[i].priority == 0)
        {
            ++failures;
            printf("FAIL: S_sfx[%d] is empty, so the table is shorter than "
                   "NUMSFX (%d)\n", i, NUMSFX);
            break;
        }
    }
    ++checks;

    for (i = 1; i < NUMMUSIC; ++i)
    {
        if (S_music[i].name[0] == '\0')
        {
            ++failures;
            printf("FAIL: S_music[%d] is empty, so the table is shorter than "
                   "NUMMUSIC (%d)\n", i, NUMMUSIC);
            break;
        }
    }
    ++checks;

    // The last named entries, which is where a truncated table shows first.

    Check(S_sfx[NUMSFX - 1].name[0] != '\0', "the final sfx entry exists");
    Check(S_music[NUMMUSIC - 1].name[0] != '\0', "the final music entry exists");

    // The six Crispy additions sfxenum_t names. Their absence is what ran
    // S_Start's loop off the end of the table.

    Check(S_sfx[sfx_dgsit].name[0] != '\0', "sfx_dgsit has a table entry");
    Check(S_sfx[sfx_dgatk].name[0] != '\0', "sfx_dgatk has a table entry");
    Check(S_sfx[sfx_dgact].name[0] != '\0', "sfx_dgact has a table entry");
    Check(S_sfx[sfx_dgdth].name[0] != '\0', "sfx_dgdth has a table entry");
    Check(S_sfx[sfx_dgpain].name[0] != '\0', "sfx_dgpain has a table entry");
    Check(S_sfx[sfx_secret].name[0] != '\0', "sfx_secret has a table entry");

    // S_Start writes -1 through the whole sfx range. If the table is short,
    // that write lands in S_music. Run the same loop and require the music
    // table to survive it, which is the actual failure being guarded.

    for (i = 1; i < NUMMUSIC; ++i)
    {
        S_music[i].lumpnum = 0;
    }

    for (i = 1; i < NUMSFX; ++i)
    {
        S_sfx[i].lumpnum = S_sfx[i].usefulness = -1;
    }

    for (i = 1; i < NUMMUSIC; ++i)
    {
        if (S_music[i].lumpnum != 0)
        {
            ++failures;
            printf("FAIL: initialising S_sfx overwrote S_music[%d].lumpnum "
                   "with %d\n", i, S_music[i].lumpnum);
            break;
        }
    }
    ++checks;

    printf("%d checks, %d failures\n", checks, failures);
    return failures != 0;
}
