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
//     Deterministic exit-state canary (DCS1).
//
//     The demo canary proves two peers saw the same inputs. It cannot prove
//     they reached the same state: this fork writes no consistancy bytes, so
//     a simulation that diverged on identical inputs passes it. This one
//     serializes the simulation itself at the same shared anchor and hashes
//     that, so the two claims stay separate and each stays honest.
//
//     Everything is explicit fixed-width little-endian. No raw structs, no
//     padding, no addresses: state references become states[] indices,
//     players become player index + 1, mobj cross-references become indices
//     into the enumerated live-mobj list, and special thinkers name their
//     sector by index. An address that leaked in would differ between peers
//     for no simulation reason and make the canary useless.
//
//     What is deliberately left out is per-peer presentation: view and
//     camera fields, colormaps, damage and bonus flashes, messages, the
//     console and display player, demo and network bookkeeping, and the
//     per-instance PRNG. Those legitimately differ between two peers of the
//     same healthy game.
//

#ifndef D_STATECANARY_H
#define D_STATECANARY_H

#include "doomtype.h"

#define DCS_MAGIC_0 'D'
#define DCS_MAGIC_1 'C'
#define DCS_MAGIC_2 'S'
#define DCS_MAGIC_3 '1'

#define DCS_FORMAT_VERSION 1

// 64 hex characters plus a terminator.
#define DCS_DIGEST_LEN 65

// Reserved mobj reference values. A null target and a target that could not
// be resolved to a live mobj are different facts and must not collapse to
// the same encoding: the first is normal, the second means the enumeration
// and the reference disagree, which is worth seeing.

#define DCS_MOBJ_NULL        0xffffffffu
#define DCS_MOBJ_UNRESOLVED  0xfffffffeu

// Class tags for the special-thinker section, matching p_saveg's tc_* set
// so the two stay recognisably related.

typedef enum
{
    dcs_ceiling,
    dcs_door,
    dcs_floor,
    dcs_plat,
    dcs_flash,
    dcs_strobe,
    dcs_glow,
} dcs_special_class_t;

// Serializes the current simulation state into buf. Returns the number of
// bytes written, or 0 if buf is too small; a partial serialization is never
// reported as a length, because a truncated stream would hash to something
// that looks like a legitimate difference.

size_t D_StateCanarySerialize(byte *buf, size_t buf_len);

// Serializes and digests in one step. Writes a lowercase hex SHA-256 and
// reports the serialized length. Returns false without touching either
// output if the state does not fit the scratch buffer.

boolean D_StateCanaryDigest(char *digest, size_t digest_len, size_t *byte_count);

// Emits the canary line. Separately labelled from the input canary so the
// two claims never blur together.

void D_StateCanaryReport(const char *at);

#endif /* #ifndef D_STATECANARY_H */
