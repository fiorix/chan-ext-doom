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
//     Desync canary over a recorded demo.
//
//     Every peer in a net game records every in-game player's ticcmd, in
//     player order, from the same fanned-out netcmds array. Two peers that
//     agreed on every tic therefore hold byte-identical recordings, so
//     comparing a digest is an auditable check on the input history. Each
//     instance prints its own digest, so nothing has to be extracted from
//     WASM memory.
//
//     Two differences between peers are legitimate and must not trip it:
//
//       - byte 8 of the header is consoleplayer, which is by definition each
//         peer's own index, so it is normalized to zero before hashing;
//       - the recording keeps growing after the level ends and the two
//         humans quit at different moments, so the digest is bounded at the
//         exit anchor rather than taken over the whole buffer.
//
//     What it proves is that both peers saw the same inputs. It does not
//     prove their simulations agree: this fork writes no consistancy bytes,
//     so a simulation bug fed identical inputs passes.
//

#ifndef D_DEMOCANARY_H
#define D_DEMOCANARY_H

#include "doomtype.h"

// The v109 header: version, skill, episode, map, deathmatch, respawn, fast,
// nomonsters, consoleplayer, then playeringame[0..3].

#define DEMO_HEADER_LEN 13
#define DEMO_CONSOLEPLAYER_OFFSET 8

// 64 hex characters plus a terminator.
#define DEMO_CANARY_DIGEST_LEN 65

// Writes a lowercase hex SHA-256 over the header with consoleplayer zeroed
// followed by the stream up to anchor_len. Returns false, writing nothing,
// unless anchor_len covers at least the header: a digest over a partial
// header would compare two things that are not comparable.

boolean D_DemoCanaryDigest(const byte *demo, size_t anchor_len, char *out,
                           size_t out_len);

#endif /* #ifndef D_DEMOCANARY_H */
