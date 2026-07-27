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
//     SHA-256 is implemented here rather than reusing the tree's sha1.c.
//     The canary is a verification signal, and a truncated 160-bit hash is
//     the wrong tool for something whose whole job is to be trusted when it
//     says two recordings match. It is also self-contained, which keeps the
//     host test free of the SDL headers sha1.c drags in.
//

#include <stdio.h>
#include <string.h>

#include "d_democanary.h"

typedef struct
{
    uint32_t state[8];
    uint64_t bits;
    byte block[64];
    size_t used;
} sha256_t;

static const uint32_t sha256_k[64] = {
    0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u,
    0x3956c25bu, 0x59f111f1u, 0x923f82a4u, 0xab1c5ed5u,
    0xd807aa98u, 0x12835b01u, 0x243185beu, 0x550c7dc3u,
    0x72be5d74u, 0x80deb1feu, 0x9bdc06a7u, 0xc19bf174u,
    0xe49b69c1u, 0xefbe4786u, 0x0fc19dc6u, 0x240ca1ccu,
    0x2de92c6fu, 0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau,
    0x983e5152u, 0xa831c66du, 0xb00327c8u, 0xbf597fc7u,
    0xc6e00bf3u, 0xd5a79147u, 0x06ca6351u, 0x14292967u,
    0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu, 0x53380d13u,
    0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u,
    0xa2bfe8a1u, 0xa81a664bu, 0xc24b8b70u, 0xc76c51a3u,
    0xd192e819u, 0xd6990624u, 0xf40e3585u, 0x106aa070u,
    0x19a4c116u, 0x1e376c08u, 0x2748774cu, 0x34b0bcb5u,
    0x391c0cb3u, 0x4ed8aa4au, 0x5b9cca4fu, 0x682e6ff3u,
    0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u,
    0x90befffau, 0xa4506cebu, 0xbef9a3f7u, 0xc67178f2u,
};

static uint32_t Ror(uint32_t v, unsigned int n)
{
    return (v >> n) | (v << (32 - n));
}

static void Sha256Compress(sha256_t *ctx, const byte *p)
{
    uint32_t w[64];
    uint32_t a, b, c, d, e, f, g, h;
    unsigned int i;

    for (i = 0; i < 16; ++i)
    {
        w[i] = ((uint32_t) p[i * 4] << 24) | ((uint32_t) p[i * 4 + 1] << 16)
             | ((uint32_t) p[i * 4 + 2] << 8) | (uint32_t) p[i * 4 + 3];
    }

    for (i = 16; i < 64; ++i)
    {
        uint32_t s0 = Ror(w[i - 15], 7) ^ Ror(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = Ror(w[i - 2], 17) ^ Ror(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }

    a = ctx->state[0]; b = ctx->state[1]; c = ctx->state[2]; d = ctx->state[3];
    e = ctx->state[4]; f = ctx->state[5]; g = ctx->state[6]; h = ctx->state[7];

    for (i = 0; i < 64; ++i)
    {
        uint32_t s1 = Ror(e, 6) ^ Ror(e, 11) ^ Ror(e, 25);
        uint32_t ch = (e & f) ^ ((~e) & g);
        uint32_t t1 = h + s1 + ch + sha256_k[i] + w[i];
        uint32_t s0 = Ror(a, 2) ^ Ror(a, 13) ^ Ror(a, 22);
        uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        uint32_t t2 = s0 + maj;

        h = g; g = f; f = e; e = d + t1;
        d = c; c = b; b = a; a = t1 + t2;
    }

    ctx->state[0] += a; ctx->state[1] += b; ctx->state[2] += c;
    ctx->state[3] += d; ctx->state[4] += e; ctx->state[5] += f;
    ctx->state[6] += g; ctx->state[7] += h;
}

static void Sha256Init(sha256_t *ctx)
{
    ctx->state[0] = 0x6a09e667u; ctx->state[1] = 0xbb67ae85u;
    ctx->state[2] = 0x3c6ef372u; ctx->state[3] = 0xa54ff53au;
    ctx->state[4] = 0x510e527fu; ctx->state[5] = 0x9b05688cu;
    ctx->state[6] = 0x1f83d9abu; ctx->state[7] = 0x5be0cd19u;
    ctx->bits = 0;
    ctx->used = 0;
}

static void Sha256Update(sha256_t *ctx, const byte *data, size_t len)
{
    size_t i;

    for (i = 0; i < len; ++i)
    {
        ctx->block[ctx->used++] = data[i];

        if (ctx->used == 64)
        {
            Sha256Compress(ctx, ctx->block);
            ctx->used = 0;
        }
    }

    ctx->bits += (uint64_t) len * 8;
}

static void Sha256Final(sha256_t *ctx, byte out[32])
{
    uint64_t bits = ctx->bits;
    unsigned int i;
    byte pad = 0x80;
    byte zero = 0x00;
    byte length[8];

    Sha256Update(ctx, &pad, 1);
    ctx->bits = bits;

    while (ctx->used != 56)
    {
        Sha256Update(ctx, &zero, 1);
        ctx->bits = bits;
    }

    for (i = 0; i < 8; ++i)
    {
        length[i] = (byte) ((bits >> (56 - i * 8)) & 0xff);
    }

    Sha256Update(ctx, length, 8);

    for (i = 0; i < 8; ++i)
    {
        out[i * 4] = (byte) (ctx->state[i] >> 24);
        out[i * 4 + 1] = (byte) (ctx->state[i] >> 16);
        out[i * 4 + 2] = (byte) (ctx->state[i] >> 8);
        out[i * 4 + 3] = (byte) ctx->state[i];
    }
}

// Hashed as two spans with a fixed byte between them, rather than by copying
// the demo and patching it. A multiplayer recording runs to tens of kilobytes
// and this runs at level exit, where a large transient allocation is the last
// thing wanted.

static void DigestToHex(const byte digest[32], char *out, size_t out_len)
{
    unsigned int i;

    for (i = 0; i < 32; ++i)
    {
        snprintf(out + i * 2, out_len - i * 2, "%02x", digest[i]);
    }
}

boolean D_DemoCanaryRawDigest(const byte *data, size_t len, char *out,
                              size_t out_len)
{
    sha256_t ctx;
    byte digest[32];

    if (data == NULL || out == NULL || out_len < DEMO_CANARY_DIGEST_LEN)
    {
        return false;
    }

    Sha256Init(&ctx);
    Sha256Update(&ctx, data, len);
    Sha256Final(&ctx, digest);
    DigestToHex(digest, out, out_len);

    return true;
}

boolean D_DemoCanaryDigest(const byte *demo, size_t anchor_len, char *out,
                           size_t out_len)
{
    sha256_t ctx;
    byte digest[32];
    byte normalized = 0;

    if (demo == NULL || anchor_len < DEMO_HEADER_LEN)
    {
        return false;
    }

    if (out == NULL || out_len < DEMO_CANARY_DIGEST_LEN)
    {
        return false;
    }

    Sha256Init(&ctx);
    Sha256Update(&ctx, demo, DEMO_CONSOLEPLAYER_OFFSET);
    Sha256Update(&ctx, &normalized, 1);
    Sha256Update(&ctx, demo + DEMO_CONSOLEPLAYER_OFFSET + 1,
                 anchor_len - DEMO_CONSOLEPLAYER_OFFSET - 1);
    Sha256Final(&ctx, digest);

    DigestToHex(digest, out, out_len);

    return true;
}
