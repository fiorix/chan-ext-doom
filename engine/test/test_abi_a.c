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
//     Translation unit A of the boolean ABI regression.
//
//     Reaches <stdbool.h> BEFORE the Doom headers, the way g_game.c does by
//     including <emscripten.h> first. Its partner reaches them in the other
//     order. If boolean's width depends on that, these two objects disagree
//     about the layout of every struct containing one, and a write through
//     an array of such structs in one object overruns an allocation made by
//     the other.
//

#include <stdbool.h>
#include <stddef.h>

#include "doomtype.h"
#include "d_player.h"

size_t AbiA_SizeofBoolean(void)   { return sizeof(boolean); }
size_t AbiA_SizeofPlayer(void)    { return sizeof(player_t); }
size_t AbiA_OffsetKillcount(void) { return offsetof(player_t, killcount); }
size_t AbiA_OffsetCards(void)     { return offsetof(player_t, cards); }
size_t AbiA_OffsetDidsecret(void) { return offsetof(player_t, didsecret); }
