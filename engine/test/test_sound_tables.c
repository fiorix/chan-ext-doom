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
//     Semantic checks over the production sound tables.
//
//     The tables' lengths are NOT proven here. sounds.c carries a
//     compile-time guard requiring sizeof(S_sfx)/sizeof(S_sfx[0]) to equal
//     NUMSFX, and the same for S_music and NUMMUSIC, so a mismatched table is
//     a compile error in production and never reaches this file. A length
//     cannot be established from this side anyway: the arrays arrive here as
//     incomplete externs, so indexing them to the enum bound to infer an
//     extent would be undefined behaviour exactly when they are short.
//
//     What is left here is what a size check cannot say: that the entries the
//     enum names are actually populated, and that running S_Start's own
//     initialisation loop leaves the music table alone. Those indices are
//     valid only because the production guard has already established the
//     lengths.
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

    // Populated-entry checks. Valid indices, per the production guard.

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

    // S_Start writes -1 through the whole sfx range. Run that same loop and
    // require the music table to be untouched afterwards.

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
            printf("FAIL: initialising S_sfx disturbed S_music[%d].lumpnum "
                   "(%d)\n", i, S_music[i].lumpnum);
            break;
        }
    }
    ++checks;

    printf("%d checks, %d failures\n", checks, failures);
    return failures != 0;
}
