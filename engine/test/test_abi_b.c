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
//     Translation unit B of the boolean ABI regression.
//
//     Reaches the Doom headers BEFORE <stdbool.h>, the way p_setup.c does.
//     See test_abi_a.c for why the pair exists.
//

#include <stddef.h>

#include "doomtype.h"
#include "d_player.h"

#include <stdbool.h>

size_t AbiB_SizeofBoolean(void)   { return sizeof(boolean); }
size_t AbiB_SizeofPlayer(void)    { return sizeof(player_t); }
size_t AbiB_OffsetKillcount(void) { return offsetof(player_t, killcount); }
size_t AbiB_OffsetCards(void)     { return offsetof(player_t, cards); }
size_t AbiB_OffsetDidsecret(void) { return offsetof(player_t, didsecret); }
