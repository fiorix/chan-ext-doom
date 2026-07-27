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
#include "r_defs.h"
#include "p_local.h"
#include "p_spec.h"
#include "net_defs.h"
#include "deh_mapping.h"

#include <stdbool.h>

// Every struct @@proto's audit named as carrying a boolean field across
// translation units. player_t was the one that actually stomped memory; the
// rest are the same class and would have been the next symptom.

size_t AbiB_SizeofBoolean(void)      { return sizeof(boolean); }
size_t AbiB_SizeofPlayer(void)       { return sizeof(player_t); }
size_t AbiB_OffsetKillcount(void)    { return offsetof(player_t, killcount); }
size_t AbiB_OffsetCards(void)        { return offsetof(player_t, cards); }
size_t AbiB_OffsetDidsecret(void)    { return offsetof(player_t, didsecret); }
size_t AbiB_SizeofWbPlayer(void)     { return sizeof(wbplayerstruct_t); }
size_t AbiB_SizeofWbStart(void)      { return sizeof(wbstartstruct_t); }
size_t AbiB_OffsetWbDidsecret(void)  { return offsetof(wbstartstruct_t, didsecret); }
size_t AbiB_SizeofVertex(void)       { return sizeof(vertex_t); }
size_t AbiB_SizeofAnim(void)         { return sizeof(anim_t); }
size_t AbiB_SizeofCeiling(void)      { return sizeof(ceiling_t); }
size_t AbiB_OffsetCeilingCrush(void) { return offsetof(ceiling_t, crush); }
size_t AbiB_SizeofFloorMove(void)    { return sizeof(floormove_t); }
size_t AbiB_SizeofFullTiccmd(void)   { return sizeof(net_full_ticcmd_t); }
size_t AbiB_OffsetTiccmdIngame(void) { return offsetof(net_full_ticcmd_t, playeringame); }
size_t AbiB_SizeofDehMapping(void)   { return sizeof(deh_mapping_entry_t); }
