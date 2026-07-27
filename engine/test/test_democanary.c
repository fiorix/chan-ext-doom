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
//     Host test for the demo desync canary.
//
//     Three properties, matching the legitimate and illegitimate ways two
//     peers' recordings can differ:
//
//       - only byte 8 differs (each peer's own consoleplayer): equal;
//       - trailing bytes differ past the exit anchor (the humans quit at
//         different moments): equal, because the digest is bounded;
//       - any byte differs inside the anchored span: not equal.
//

#include <stdio.h>
#include <string.h>

#include "d_democanary.h"

static int checks;
static int failures;

static void Check(boolean ok, const char *what)
{
    ++checks;

    if (!ok)
    {
        ++failures;
        printf("FAIL: %s\n", what);
    }
}

// A header plus a plausible ticcmd stream.
static void BuildDemo(byte *demo, size_t len, byte consoleplayer, byte salt)
{
    size_t i;

    demo[0] = 109;          // version
    demo[1] = 2;            // skill
    demo[2] = 1;            // episode
    demo[3] = 1;            // map
    demo[4] = 0;            // deathmatch
    demo[5] = 0;            // respawn
    demo[6] = 0;            // fast
    demo[7] = 0;            // nomonsters
    demo[8] = consoleplayer;
    demo[9] = 1;            // playeringame[0]
    demo[10] = 1;           // playeringame[1]
    demo[11] = 0;
    demo[12] = 0;

    for (i = DEMO_HEADER_LEN; i < len; ++i)
    {
        demo[i] = (byte) (i * 7 + 3 + salt);
    }
}

int main(void)
{
    enum { ANCHOR = 128, TOTAL = 256 };

    byte a[TOTAL];
    byte b[TOTAL];
    char da[DEMO_CANARY_DIGEST_LEN];
    char db[DEMO_CANARY_DIGEST_LEN];

    // Two peers, same tics, different player index, and different amounts of
    // recording after the exit because they quit at different moments. This
    // is exactly the pair the canary must call equal.
    BuildDemo(a, sizeof(a), 0, 0);
    BuildDemo(b, sizeof(b), 1, 0);
    memset(a + ANCHOR, 0xaa, sizeof(a) - ANCHOR);
    memset(b + ANCHOR, 0x55, sizeof(b) - ANCHOR);

    Check(D_DemoCanaryDigest(a, ANCHOR, da, sizeof(da)), "digest is produced");
    Check(D_DemoCanaryDigest(b, ANCHOR, db, sizeof(db)),
          "digest is produced for the peer");
    Check(strcmp(da, db) == 0,
          "same inputs, different consoleplayer and different trailing bytes "
          "give the same digest");
    Check(strlen(da) == DEMO_CANARY_DIGEST_LEN - 1,
          "digest is 64 hex characters");

    // Every byte inside the anchored span must matter, or a real desync
    // could pass unnoticed.
    {
        byte c[TOTAL];
        char dc[DEMO_CANARY_DIGEST_LEN];
        size_t at;

        for (at = 0; at < ANCHOR; ++at)
        {
            if (at == DEMO_CONSOLEPLAYER_OFFSET)
            {
                continue;
            }

            BuildDemo(c, sizeof(c), 0, 0);
            memset(c + ANCHOR, 0xaa, sizeof(c) - ANCHOR);
            c[at] ^= 0xff;

            D_DemoCanaryDigest(c, ANCHOR, dc, sizeof(dc));

            if (strcmp(da, dc) == 0)
            {
                printf("FAIL: byte %u inside the anchor does not affect the digest\n",
                       (unsigned int) at);
                ++failures;
            }
            ++checks;
        }
    }

    // A single mid-stream flip, called out separately because it is the case
    // a real input divergence looks like.
    {
        byte c[TOTAL];
        char dc[DEMO_CANARY_DIGEST_LEN];

        BuildDemo(c, sizeof(c), 0, 0);
        memset(c + ANCHOR, 0xaa, sizeof(c) - ANCHOR);
        c[ANCHOR / 2] ^= 0x01;

        Check(D_DemoCanaryDigest(c, ANCHOR, dc, sizeof(dc)), "flipped demo digests");
        Check(strcmp(da, dc) != 0, "one flipped mid-stream bit changes the digest");
    }

    // A longer anchor is a different recording: the canary must not be blind
    // to tics one side has and the other does not.
    {
        char dlong[DEMO_CANARY_DIGEST_LEN];
        Check(D_DemoCanaryDigest(a, ANCHOR + 4, dlong, sizeof(dlong)),
              "a longer anchor digests");
        Check(strcmp(da, dlong) != 0, "the anchored length changes the digest");
    }

    // Too short to hold the header: refuse rather than compare two things
    // that are not comparable.
    {
        char dshort[DEMO_CANARY_DIGEST_LEN];
        Check(!D_DemoCanaryDigest(a, DEMO_HEADER_LEN - 1, dshort, sizeof(dshort)),
              "an anchor shorter than the header is refused");
        Check(D_DemoCanaryDigest(a, DEMO_HEADER_LEN, dshort, sizeof(dshort)),
              "a header-only recording is accepted");
        Check(!D_DemoCanaryDigest(a, 0, dshort, sizeof(dshort)),
              "an empty demo is refused");
        Check(!D_DemoCanaryDigest(NULL, ANCHOR, dshort, sizeof(dshort)),
              "a null demo is refused");
    }

    // An undersized output buffer must not be partially written.
    {
        char tiny[8];
        Check(!D_DemoCanaryDigest(a, ANCHOR, tiny, sizeof(tiny)),
              "an undersized output buffer is refused");
    }

    // Pinned against an independent implementation: this exact digest was
    // produced by python hashlib over the same normalized bytes.
    {
        byte fixed[DEMO_HEADER_LEN];
        char df[DEMO_CANARY_DIGEST_LEN];

        memset(fixed, 0, sizeof(fixed));
        Check(D_DemoCanaryDigest(fixed, sizeof(fixed), df, sizeof(df)),
              "the pinned vector digests");
        Check(strcmp(df,
                     "dd46c3eebb1884ff3b5258c0a2fc9398e560a29e0780d4b53869b6254aa46a96") == 0,
              "SHA-256 matches an independent implementation");
    }

    printf("%d checks, %d failures\n", checks, failures);

    return failures == 0 ? 0 : 1;
}
