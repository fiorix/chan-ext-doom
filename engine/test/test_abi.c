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
//     boolean ABI regression across translation units.
//
//     Two objects reach <stdbool.h> and the Doom headers in opposite orders
//     and are linked together. If boolean's width follows the include order,
//     they disagree about sizeof(player_t), and a write to players[i] in one
//     object uses a stride the other did not allocate for. That is not a
//     theoretical hazard: it overran into gameepisode and deathmatch and
//     surfaced as a demo header recording the wrong level, which took a full
//     round to trace back here.
//

#include <stddef.h>
#include <stdio.h>

#include "doomtype.h"

size_t AbiA_SizeofBoolean(void);
size_t AbiA_SizeofPlayer(void);
size_t AbiA_OffsetKillcount(void);
size_t AbiA_OffsetCards(void);
size_t AbiA_OffsetDidsecret(void);

size_t AbiB_SizeofBoolean(void);
size_t AbiB_SizeofPlayer(void);
size_t AbiB_OffsetKillcount(void);
size_t AbiB_OffsetCards(void);
size_t AbiB_OffsetDidsecret(void);

static int checks;
static int failures;

static void CheckEqual(size_t a, size_t b, const char *what)
{
    ++checks;

    if (a != b)
    {
        ++failures;
        printf("FAIL: %s: stdbool-first=%zu doom-first=%zu\n", what, a, b);
    }
}

int main(void)
{
    // The width itself. Four bytes is this lineage's historical layout, and
    // pinning the number keeps a future "just use bool" change from silently
    // reintroducing the split in a way the equality checks alone would miss,
    // since both objects would shrink together.
    ++checks;
    if (AbiA_SizeofBoolean() != 4)
    {
        ++failures;
        printf("FAIL: boolean is %zu bytes, expected 4\n", AbiA_SizeofBoolean());
    }

    CheckEqual(AbiA_SizeofBoolean(), AbiB_SizeofBoolean(),
               "sizeof(boolean) agrees across include orders");
    CheckEqual(AbiA_SizeofPlayer(), AbiB_SizeofPlayer(),
               "sizeof(player_t) agrees across include orders");

    // Offsets, not just the total size. Two layouts can coincidentally share
    // a size while placing members differently.
    CheckEqual(AbiA_OffsetCards(), AbiB_OffsetCards(),
               "offsetof(player_t, cards) agrees");
    CheckEqual(AbiA_OffsetDidsecret(), AbiB_OffsetDidsecret(),
               "offsetof(player_t, didsecret) agrees");
    CheckEqual(AbiA_OffsetKillcount(), AbiB_OffsetKillcount(),
               "offsetof(player_t, killcount) agrees");

    printf("%d checks, %d failures\n", checks, failures);

    return failures == 0 ? 0 : 1;
}
