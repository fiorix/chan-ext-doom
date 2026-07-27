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

#include <stdio.h>
#include <string.h>

#include "d_statecanary.h"
#include "d_democanary.h"

#include "doomstat.h"
#include "d_player.h"
#include "info.h"
#include "m_random.h"

#include "p_local.h"
#include "p_spec.h"
#include "r_state.h"
#include "z_zone.h"

// m_random.h exposes the generators but not their state. prndindex is the
// simulation RNG's position, and it belongs in the digest: two peers can look
// identical and still diverge on the very next roll if it differs.

extern int prndindex;

// Big enough for E1M1 by a wide margin: 147 sectors and a few hundred mobjs
// come to roughly 30 KB. Allocated once on demand rather than kept as a
// static, since it is touched exactly twice in a session.

#define DCS_SCRATCH_LEN (512 * 1024)

//
// Writers. Explicit widths and byte order, and every one of them checks the
// remaining space before touching the buffer, so a serialization that would
// overrun stops rather than truncating into a plausible-looking digest.
//

typedef struct
{
    byte *buf;
    size_t len;
    size_t pos;
    boolean overflow;
} dcs_writer_t;

static void DcsU8(dcs_writer_t *w, unsigned int v)
{
    if (w->overflow || w->pos + 1 > w->len)
    {
        w->overflow = true;
        return;
    }

    w->buf[w->pos++] = (byte)(v & 0xff);
}

static void DcsU16(dcs_writer_t *w, unsigned int v)
{
    DcsU8(w, v);
    DcsU8(w, v >> 8);
}

static void DcsU32(dcs_writer_t *w, unsigned int v)
{
    DcsU8(w, v);
    DcsU8(w, v >> 8);
    DcsU8(w, v >> 16);
    DcsU8(w, v >> 24);
}

static void DcsS32(dcs_writer_t *w, int v)
{
    DcsU32(w, (unsigned int) v);
}

static void DcsS16(dcs_writer_t *w, int v)
{
    DcsU16(w, (unsigned int)(v & 0xffff));
}

//
// Live mobj enumeration.
//
// target and tracer are pointers, and a pointer is exactly what must never
// reach the digest. They are translated to an index into the thinker list's
// own order, which P_AddThinker makes deterministic because it appends and
// removal only unlinks. The pointers are used purely as lookup keys inside
// one instance and never serialized.
//

static unsigned int DcsMobjIndex(mobj_t *mobj)
{
    thinker_t *th;
    unsigned int index = 0;

    if (mobj == NULL)
    {
        return DCS_MOBJ_NULL;
    }

    for (th = thinkercap.next; th != &thinkercap && th != NULL; th = th->next)
    {
        if (th->function.acp1 == (actionf_p1) P_MobjThinker)
        {
            if ((mobj_t *) th == mobj)
            {
                return index;
            }

            ++index;
        }
    }

    // Non-null but not on the live list. Distinct from null on purpose: it
    // means the reference and the enumeration disagree, which is a fact
    // worth surfacing rather than smoothing over.

    return DCS_MOBJ_UNRESOLVED;
}

static void DcsWritePlayers(dcs_writer_t *w)
{
    int i;
    int j;

    DcsU8(w, MAXPLAYERS);

    for (i = 0; i < MAXPLAYERS; ++i)
    {
        player_t *p = &players[i];
        unsigned int cards = 0;
        unsigned int owned = 0;

        DcsU8(w, playeringame[i] ? 1 : 0);

        // An absent slot contributes its marker and nothing else. Its
        // storage is stale rather than zero, and p->mo is not safe to
        // dereference, so there is nothing here worth hashing.

        if (!playeringame[i])
        {
            continue;
        }

        DcsS32(w, (int) p->playerstate);

        if (p->mo != NULL)
        {
            DcsU8(w, 1);
            DcsS32(w, p->mo->x);
            DcsS32(w, p->mo->y);
            DcsS32(w, p->mo->z);
            DcsU32(w, (unsigned int) p->mo->angle);
        }
        else
        {
            // In game but with no body is a real transient state, so it is
            // recorded rather than assumed impossible.
            DcsU8(w, 0);
        }

        DcsS32(w, p->health);
        DcsS32(w, p->armorpoints);
        DcsS32(w, p->armortype);

        for (j = 0; j < NUMPOWERS; ++j)
        {
            DcsS32(w, p->powers[j]);
        }

        for (j = 0; j < NUMCARDS; ++j)
        {
            if (p->cards[j])
            {
                cards |= 1u << j;
            }
        }

        DcsU8(w, cards);
        DcsU8(w, p->backpack ? 1 : 0);

        for (j = 0; j < MAXPLAYERS; ++j)
        {
            DcsS32(w, p->frags[j]);
        }

        DcsS32(w, (int) p->readyweapon);
        DcsS32(w, (int) p->pendingweapon);

        for (j = 0; j < NUMWEAPONS; ++j)
        {
            if (p->weaponowned[j])
            {
                owned |= 1u << j;
            }
        }

        DcsU16(w, owned);

        for (j = 0; j < NUMAMMO; ++j)
        {
            DcsS32(w, p->ammo[j]);
        }

        DcsS32(w, p->refire);
        DcsS32(w, p->killcount);
        DcsS32(w, p->itemcount);
        DcsS32(w, p->secretcount);
    }
}

static void DcsWriteSectors(dcs_writer_t *w)
{
    int i;

    DcsU32(w, (unsigned int) numsectors);

    for (i = 0; i < numsectors; ++i)
    {
        sector_t *s = &sectors[i];

        DcsS32(w, s->floorheight);
        DcsS32(w, s->ceilingheight);
        DcsS16(w, s->floorpic);
        DcsS16(w, s->ceilingpic);
        DcsS16(w, s->lightlevel);
        DcsS16(w, s->special);
        DcsS16(w, s->tag);
    }
}

static void DcsWriteMobjs(dcs_writer_t *w)
{
    thinker_t *th;
    unsigned int count = 0;

    for (th = thinkercap.next; th != &thinkercap && th != NULL; th = th->next)
    {
        if (th->function.acp1 == (actionf_p1) P_MobjThinker)
        {
            ++count;
        }
    }

    DcsU32(w, count);

    for (th = thinkercap.next; th != &thinkercap && th != NULL; th = th->next)
    {
        mobj_t *mobj;

        if (th->function.acp1 != (actionf_p1) P_MobjThinker)
        {
            continue;
        }

        mobj = (mobj_t *) th;

        DcsU16(w, (unsigned int) mobj->type);

        // The state is a table entry, so its index is the portable name for
        // it. sprite and frame come from the same entry and would only
        // repeat what the index already says.
        DcsU32(w, (unsigned int)(mobj->state - states));
        DcsS32(w, mobj->tics);

        DcsS32(w, mobj->x);
        DcsS32(w, mobj->y);
        DcsS32(w, mobj->z);
        DcsS32(w, mobj->momx);
        DcsS32(w, mobj->momy);
        DcsS32(w, mobj->momz);

        DcsU32(w, (unsigned int) mobj->angle);
        DcsS32(w, mobj->flags);
        DcsS32(w, mobj->health);

        DcsS32(w, mobj->movedir);
        DcsS32(w, mobj->movecount);
        DcsS32(w, mobj->reactiontime);
        DcsS32(w, mobj->threshold);
        DcsS32(w, mobj->lastlook);

        DcsU32(w, DcsMobjIndex(mobj->target));
        DcsU32(w, DcsMobjIndex(mobj->tracer));

        DcsU8(w, mobj->player != NULL
                 ? (unsigned int)((mobj->player - players) + 1) : 0);
    }
}

static unsigned int DcsSectorIndex(sector_t *sector)
{
    if (sector == NULL)
    {
        return DCS_MOBJ_NULL;
    }

    return (unsigned int)(sector - sectors);
}

static void DcsWriteSpecials(dcs_writer_t *w)
{
    thinker_t *th;
    unsigned int count = 0;

    for (th = thinkercap.next; th != &thinkercap && th != NULL; th = th->next)
    {
        actionf_p1 fn = th->function.acp1;

        if (fn == (actionf_p1) T_MoveCeiling || fn == (actionf_p1) T_VerticalDoor
         || fn == (actionf_p1) T_MoveFloor || fn == (actionf_p1) T_PlatRaise
         || fn == (actionf_p1) T_LightFlash || fn == (actionf_p1) T_StrobeFlash
         || fn == (actionf_p1) T_Glow)
        {
            ++count;
        }
    }

    DcsU32(w, count);

    for (th = thinkercap.next; th != &thinkercap && th != NULL; th = th->next)
    {
        actionf_p1 fn = th->function.acp1;

        if (fn == (actionf_p1) T_MoveCeiling)
        {
            ceiling_t *c = (ceiling_t *) th;

            DcsU8(w, dcs_ceiling);
            DcsU32(w, DcsSectorIndex(c->sector));
            DcsS32(w, (int) c->type);
            DcsS32(w, c->bottomheight);
            DcsS32(w, c->topheight);
            DcsS32(w, c->speed);
            DcsU8(w, c->crush ? 1 : 0);
            DcsS32(w, c->direction);
            DcsS32(w, c->tag);
            DcsS32(w, c->olddirection);
        }
        else if (fn == (actionf_p1) T_VerticalDoor)
        {
            vldoor_t *d = (vldoor_t *) th;

            DcsU8(w, dcs_door);
            DcsU32(w, DcsSectorIndex(d->sector));
            DcsS32(w, (int) d->type);
            DcsS32(w, d->topheight);
            DcsS32(w, d->speed);
            DcsS32(w, d->direction);
            DcsS32(w, d->topwait);
            DcsS32(w, d->topcountdown);
        }
        else if (fn == (actionf_p1) T_MoveFloor)
        {
            floormove_t *f = (floormove_t *) th;

            DcsU8(w, dcs_floor);
            DcsU32(w, DcsSectorIndex(f->sector));
            DcsS32(w, (int) f->type);
            DcsU8(w, f->crush ? 1 : 0);
            DcsS32(w, f->direction);
            DcsS32(w, f->newspecial);
            DcsS16(w, f->texture);
            DcsS32(w, f->floordestheight);
            DcsS32(w, f->speed);
        }
        else if (fn == (actionf_p1) T_PlatRaise)
        {
            plat_t *p = (plat_t *) th;

            DcsU8(w, dcs_plat);
            DcsU32(w, DcsSectorIndex(p->sector));
            DcsS32(w, p->speed);
            DcsS32(w, p->low);
            DcsS32(w, p->high);
            DcsS32(w, p->wait);
            DcsS32(w, p->count);
            DcsS32(w, (int) p->status);
            DcsS32(w, (int) p->oldstatus);
            DcsU8(w, p->crush ? 1 : 0);
            DcsS32(w, p->tag);
            DcsS32(w, (int) p->type);
        }
        else if (fn == (actionf_p1) T_LightFlash)
        {
            lightflash_t *l = (lightflash_t *) th;

            DcsU8(w, dcs_flash);
            DcsU32(w, DcsSectorIndex(l->sector));
            DcsS32(w, l->count);
            DcsS32(w, l->maxlight);
            DcsS32(w, l->minlight);
            DcsS32(w, l->maxtime);
            DcsS32(w, l->mintime);
        }
        else if (fn == (actionf_p1) T_StrobeFlash)
        {
            strobe_t *s = (strobe_t *) th;

            DcsU8(w, dcs_strobe);
            DcsU32(w, DcsSectorIndex(s->sector));
            DcsS32(w, s->count);
            DcsS32(w, s->minlight);
            DcsS32(w, s->maxlight);
            DcsS32(w, s->darktime);
            DcsS32(w, s->brighttime);
        }
        else if (fn == (actionf_p1) T_Glow)
        {
            glow_t *g = (glow_t *) th;

            DcsU8(w, dcs_glow);
            DcsU32(w, DcsSectorIndex(g->sector));
            DcsS32(w, g->minlight);
            DcsS32(w, g->maxlight);
            DcsS32(w, g->direction);
        }
    }
}

size_t D_StateCanarySerialize(byte *buf, size_t buf_len)
{
    dcs_writer_t w;

    memset(&w, 0, sizeof(w));
    w.buf = buf;
    w.len = buf_len;

    DcsU8(&w, DCS_MAGIC_0);
    DcsU8(&w, DCS_MAGIC_1);
    DcsU8(&w, DCS_MAGIC_2);
    DcsU8(&w, DCS_MAGIC_3);
    DcsU16(&w, DCS_FORMAT_VERSION);
    DcsU16(&w, 0);

    DcsS32(&w, gametic);
    DcsS32(&w, leveltime);

    DcsU8(&w, (unsigned int) gameepisode);
    DcsU8(&w, (unsigned int) gamemap);
    DcsU8(&w, (unsigned int) gameskill);
    DcsU8(&w, (unsigned int) deathmatch);

    // The simulation RNG index. Two peers can look identical and still
    // diverge on the very next roll if this differs, so it is not optional.
    // M_Random's rndindex is presentation-side and stays out.
    DcsU8(&w, (unsigned int)(prndindex & 0xff));

    DcsWritePlayers(&w);
    DcsWriteSectors(&w);
    DcsWriteMobjs(&w);
    DcsWriteSpecials(&w);

    if (w.overflow)
    {
        return 0;
    }

    return w.pos;
}

boolean D_StateCanaryDigest(char *digest, size_t digest_len, size_t *byte_count)
{
    byte *scratch;
    size_t len;
    boolean ok;

    if (digest == NULL || byte_count == NULL || digest_len < DCS_DIGEST_LEN)
    {
        return false;
    }

    scratch = Z_Malloc(DCS_SCRATCH_LEN, PU_STATIC, 0);
    len = D_StateCanarySerialize(scratch, DCS_SCRATCH_LEN);

    if (len == 0)
    {
        Z_Free(scratch);
        return false;
    }

    // Same digest primitive as the input canary, over a different stream.
    ok = D_DemoCanaryRawDigest(scratch, len, digest, digest_len);
    Z_Free(scratch);

    if (!ok)
    {
        return false;
    }

    *byte_count = len;
    return true;
}

void D_StateCanaryReport(const char *at)
{
    char digest[DCS_DIGEST_LEN];
    size_t len = 0;

    if (!D_StateCanaryDigest(digest, sizeof(digest), &len))
    {
        printf("STATE CANARY: %s: state did not fit, no digest\n", at);
        return;
    }

    printf("STATE CANARY: %s episode=%d map=%d gametic=%d bytes=%u sha256=%s\n",
           at, gameepisode, gamemap, gametic, (unsigned int) len, digest);
}
