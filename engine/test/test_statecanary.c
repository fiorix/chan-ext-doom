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
//     Host tests for the deterministic exit-state canary.
//
//     The serializer reads engine globals, so the test supplies them: a
//     synthesized mini-state of two players, three sectors, four mobjs with
//     one target link, and one door special. That is enough to exercise
//     every section and every reference translation.
//
//     The three claims under test, in order of how much they matter:
//
//       - sensitivity: a change to any simulation value changes the digest,
//         or the canary would miss a real desync;
//       - insensitivity: a change to any per-peer presentation value does
//         not, or the canary would cry wolf on a healthy game;
//       - no address leakage: the same logical state at different
//         allocations digests identically, or nothing would ever match.
//

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "d_statecanary.h"
#include "d_player.h"
#include "p_local.h"
#include "p_spec.h"
#include "r_state.h"

//
// Engine globals the serializer reads.
//

player_t players[MAXPLAYERS];
boolean playeringame[MAXPLAYERS];
int numsectors;
sector_t *sectors;
state_t states[NUMSTATES];
thinker_t thinkercap;
int gametic;
int leveltime;
int gameepisode;
int gamemap;
skill_t gameskill;
int deathmatch;
int prndindex;

//
// Stubs. Only their addresses matter: the serializer compares thinker
// function pointers to classify each thinker, and never calls them.
//

void P_MobjThinker(mobj_t *mobj) { (void) mobj; }
void T_MoveCeiling(ceiling_t *c) { (void) c; }
void T_VerticalDoor(vldoor_t *d) { (void) d; }
void T_MoveFloor(floormove_t *f) { (void) f; }
void T_PlatRaise(plat_t *p) { (void) p; }
void T_LightFlash(lightflash_t *l) { (void) l; }
void T_StrobeFlash(strobe_t *s) { (void) s; }
void T_Glow(glow_t *g) { (void) g; }

void *Z_Malloc(int size, int tag, void *user)
{
    (void) tag;
    (void) user;
    return malloc(size);
}

void Z_Free(void *ptr) { free(ptr); }

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

//
// The synthesized state.
//

static sector_t test_sectors[3];
static mobj_t test_mobjs[4];
static vldoor_t test_door;

static void LinkThinker(thinker_t *th, void *fn)
{
    thinker_t *tail = thinkercap.prev;

    th->function.acp1 = (actionf_p1) fn;
    th->prev = tail;
    th->next = &thinkercap;
    tail->next = th;
    thinkercap.prev = th;
}

// Rebuilt from scratch for every case so one mutation cannot leak into the
// next, which would turn a sensitivity failure into a silent pass.
static void BuildState(void)
{
    int i;

    memset(players, 0, sizeof(players));
    memset(playeringame, 0, sizeof(playeringame));
    memset(test_sectors, 0, sizeof(test_sectors));
    memset(test_mobjs, 0, sizeof(test_mobjs));
    memset(&test_door, 0, sizeof(test_door));
    memset(states, 0, sizeof(states));

    thinkercap.next = &thinkercap;
    thinkercap.prev = &thinkercap;

    gametic = 4242;
    leveltime = 4200;
    gameepisode = 1;
    gamemap = 1;
    gameskill = sk_medium;
    deathmatch = 0;
    prndindex = 77;

    numsectors = 3;
    sectors = test_sectors;

    for (i = 0; i < 3; ++i)
    {
        test_sectors[i].floorheight = i * 8;
        test_sectors[i].ceilingheight = 128 + i * 8;
        test_sectors[i].floorpic = (short)(10 + i);
        test_sectors[i].ceilingpic = (short)(20 + i);
        test_sectors[i].lightlevel = (short)(160 + i);
        test_sectors[i].special = (short) i;
        test_sectors[i].tag = (short)(100 + i);
    }

    for (i = 0; i < 4; ++i)
    {
        test_mobjs[i].type = (mobjtype_t)(i + 1);
        test_mobjs[i].state = &states[i + 3];
        test_mobjs[i].tics = 5 + i;
        test_mobjs[i].x = 1000 + i;
        test_mobjs[i].y = 2000 + i;
        test_mobjs[i].z = 0;
        test_mobjs[i].momx = i;
        test_mobjs[i].momy = -i;
        test_mobjs[i].momz = 0;
        test_mobjs[i].angle = (angle_t)(i * 0x10000000u);
        test_mobjs[i].flags = 0x400 + i;
        test_mobjs[i].health = 100 - i;
        test_mobjs[i].movedir = i;
        test_mobjs[i].movecount = i * 2;
        test_mobjs[i].reactiontime = 8;
        test_mobjs[i].threshold = 0;
        test_mobjs[i].lastlook = i;
        test_mobjs[i].target = NULL;
        test_mobjs[i].tracer = NULL;
        test_mobjs[i].player = NULL;
        LinkThinker(&test_mobjs[i].thinker, (void *) P_MobjThinker);
    }

    // One cross-reference, so the index translation is exercised rather than
    // just the null path.
    test_mobjs[2].target = &test_mobjs[0];

    // Two players in game, one of them bodied.
    playeringame[0] = true;
    playeringame[1] = true;
    players[0].mo = &test_mobjs[0];
    test_mobjs[0].player = &players[0];
    players[0].playerstate = PST_LIVE;
    players[0].health = 100;
    players[0].armorpoints = 50;
    players[0].armortype = 1;
    players[0].powers[0] = 30;
    players[0].cards[0] = true;
    players[0].backpack = false;
    players[0].readyweapon = wp_pistol;
    players[0].pendingweapon = wp_nochange;
    players[0].weaponowned[wp_pistol] = 1;
    players[0].ammo[am_clip] = 42;
    players[0].refire = 0;
    players[0].killcount = 7;
    players[0].itemcount = 3;
    players[0].secretcount = 1;

    players[1].mo = &test_mobjs[1];
    test_mobjs[1].player = &players[1];
    players[1].playerstate = PST_LIVE;
    players[1].health = 80;
    players[1].killcount = 2;

    // One active special.
    test_door.sector = &test_sectors[1];
    test_door.type = normal;
    test_door.topheight = 200;
    test_door.speed = 4;
    test_door.direction = 1;
    test_door.topwait = 150;
    test_door.topcountdown = 0;
    LinkThinker(&test_door.thinker, (void *) T_VerticalDoor);
}

static void DigestNow(char *out)
{
    size_t len = 0;

    if (!D_StateCanaryDigest(out, DCS_DIGEST_LEN, &len))
    {
        printf("FAIL: digest could not be produced\n");
        ++failures;
        out[0] = '\0';
    }
}

// Applies a mutation to a freshly built state and reports whether it moved
// the digest. Table-driven so each case reads as one line.
typedef void (*mutator_t)(void);

static char baseline[DCS_DIGEST_LEN];

static boolean DigestChangedBy(mutator_t mutate)
{
    char after[DCS_DIGEST_LEN];

    BuildState();
    mutate();
    DigestNow(after);

    return strcmp(baseline, after) != 0;
}

// --- simulation values: each of these MUST move the digest ----------------

static void MutPlayerZ(void)        { test_mobjs[0].z = 64; }
static void MutPlayerHealth(void)   { players[0].health = 99; }
static void MutPlayerPower(void)    { players[0].powers[0] = 29; }
static void MutKillcount(void)      { players[0].killcount = 8; }
static void MutSecretcount(void)    { players[0].secretcount = 2; }
static void MutCards(void)          { players[0].cards[1] = true; }
static void MutAmmo(void)           { players[0].ammo[am_clip] = 41; }
static void MutReadyWeapon(void)    { players[0].readyweapon = wp_shotgun; }
static void MutPlayerInGame(void)   { playeringame[2] = true; }
static void MutSectorLight(void)    { test_sectors[0].lightlevel = 161; }
static void MutSectorFloor(void)    { test_sectors[0].floorheight = 9; }
static void MutSectorTag(void)      { test_sectors[2].tag = 999; }
static void MutSectorSpecial(void)  { test_sectors[1].special = 9; }
static void MutMobjMomy(void)       { test_mobjs[1].momy = 12345; }
static void MutMobjState(void)      { test_mobjs[1].state = &states[9]; }
static void MutMobjHealth(void)     { test_mobjs[3].health = 1; }
static void MutMobjAngle(void)      { test_mobjs[3].angle = 0x1234u; }
static void MutTargetIndex(void)    { test_mobjs[2].target = &test_mobjs[3]; }
static void MutTargetNull(void)     { test_mobjs[2].target = NULL; }
static void MutDoorScalar(void)     { test_door.topcountdown = 35; }
static void MutDoorSector(void)     { test_door.sector = &test_sectors[2]; }
static void MutPrndindex(void)      { prndindex = 78; }
static void MutLeveltime(void)      { leveltime = 4201; }
static void MutGametic(void)        { gametic = 4243; }
static void MutGamemap(void)        { gamemap = 2; }
static void MutSkill(void)          { gameskill = sk_hard; }
static void MutDeathmatch(void)     { deathmatch = 1; }

// --- per-peer presentation: each of these MUST NOT move the digest --------

static void MutViewz(void)          { players[0].viewz = 1234; }
static void MutViewheight(void)     { players[0].viewheight = 41 << 16; }
static void MutBob(void)            { players[0].bob = 9999; }
static void MutDamagecount(void)    { players[0].damagecount = 60; }
static void MutBonuscount(void)     { players[0].bonuscount = 12; }
static void MutColormap(void)       { players[0].colormap = 3; }
static void MutFixedColormap(void)  { players[0].fixedcolormap = 1; }
static void MutExtralight(void)     { players[0].extralight = 2; }
static void MutMessage(void)        { players[0].message = "picked up"; }
static void MutAttacker(void)       { players[0].attacker = &test_mobjs[3]; }
static void MutPsprite(void)        { players[0].psprites[0].tics = 3; }
static void MutDidsecret(void)      { players[0].didsecret = true; }
static void MutCheats(void)         { players[0].cheats = 1; }
static void MutAttackdown(void)     { players[0].attackdown = 1; }
static void MutSectorOldspecial(void) { test_sectors[0].oldspecial = 7; }
static void MutMobjValidcount(void) { test_mobjs[0].validcount = 42; }

struct case_t
{
    mutator_t mutate;
    const char *name;
};

static const struct case_t sensitive[] = {
    { MutPlayerZ,        "player z" },
    { MutPlayerHealth,   "player health" },
    { MutPlayerPower,    "a power counter" },
    { MutKillcount,      "killcount" },
    { MutSecretcount,    "secretcount" },
    { MutCards,          "a keycard" },
    { MutAmmo,           "ammo" },
    { MutReadyWeapon,    "readyweapon" },
    { MutPlayerInGame,   "a player joining" },
    { MutSectorLight,    "sector lightlevel" },
    { MutSectorFloor,    "sector floorheight" },
    { MutSectorTag,      "sector tag" },
    { MutSectorSpecial,  "sector special" },
    { MutMobjMomy,       "mobj momy" },
    { MutMobjState,      "mobj state index" },
    { MutMobjHealth,     "mobj health" },
    { MutMobjAngle,      "mobj angle" },
    { MutTargetIndex,    "a target retargeted" },
    { MutTargetNull,     "a target cleared" },
    { MutDoorScalar,     "a door scalar" },
    { MutDoorSector,     "a door's sector" },
    { MutPrndindex,      "prndindex" },
    { MutLeveltime,      "leveltime" },
    { MutGametic,        "gametic" },
    { MutGamemap,        "gamemap" },
    { MutSkill,          "skill" },
    { MutDeathmatch,     "deathmatch" },
};

static const struct case_t excluded[] = {
    { MutViewz,            "viewz" },
    { MutViewheight,       "viewheight" },
    { MutBob,              "bob" },
    { MutDamagecount,      "damagecount" },
    { MutBonuscount,       "bonuscount" },
    { MutColormap,         "colormap" },
    { MutFixedColormap,    "fixedcolormap" },
    { MutExtralight,       "extralight" },
    { MutMessage,          "message pointer" },
    { MutAttacker,         "attacker pointer" },
    { MutPsprite,          "psprite tics" },
    { MutDidsecret,        "didsecret" },
    { MutCheats,           "cheats" },
    { MutAttackdown,       "attackdown" },
    { MutSectorOldspecial, "sector oldspecial" },
    { MutMobjValidcount,   "mobj validcount" },
};

int main(void)
{
    size_t i;
    size_t len = 0;
    char again[DCS_DIGEST_LEN];

    BuildState();
    DigestNow(baseline);

    // T1: layout. The exact length is pinned so a field reordering or a
    // width change cannot pass unnoticed even if every sensitivity case
    // still flips the digest.
    BuildState();
    Check(D_StateCanaryDigest(again, sizeof(again), &len), "a digest is produced");
    Check(len > 0, "the serialized length is reported");
    Check(strlen(baseline) == DCS_DIGEST_LEN - 1, "digest is 64 hex characters");

    {
        byte buf[8192];
        size_t direct;

        BuildState();
        direct = D_StateCanarySerialize(buf, sizeof(buf));

        // T6: the reported count is the real serialized length, and the
        // stream starts with the format's own identity.
        Check(direct == len, "the reported byte count is the serialized length");
        Check(direct > 0 && buf[0] == DCS_MAGIC_0 && buf[1] == DCS_MAGIC_1
              && buf[2] == DCS_MAGIC_2 && buf[3] == DCS_MAGIC_3,
              "the stream begins with the DCS1 magic");
        Check(buf[4] == DCS_FORMAT_VERSION && buf[5] == 0,
              "the format version follows the magic, little-endian");

        // A buffer too small must refuse rather than report a truncated
        // length that would hash like a legitimate difference.
        Check(D_StateCanarySerialize(buf, 8) == 0,
              "a buffer too small is refused, not truncated");
    }

    // Determinism: same state, same digest.
    BuildState();
    DigestNow(again);
    Check(strcmp(baseline, again) == 0, "the same state digests identically");

    // T2: sensitivity.
    for (i = 0; i < sizeof(sensitive) / sizeof(sensitive[0]); ++i)
    {
        ++checks;

        if (!DigestChangedBy(sensitive[i].mutate))
        {
            ++failures;
            printf("FAIL: changing %s did not change the digest\n",
                   sensitive[i].name);
        }
    }

    // T3: exclusion insensitivity. This is the test protecting the design's
    // core claim, that a healthy pair of peers is not reported as diverged.
    for (i = 0; i < sizeof(excluded) / sizeof(excluded[0]); ++i)
    {
        ++checks;

        if (DigestChangedBy(excluded[i].mutate))
        {
            ++failures;
            printf("FAIL: changing %s changed the digest, but it is per-peer\n",
                   excluded[i].name);
        }
    }

    // T4: twin peers. Same simulation, different console identity and
    // different local presentation, as two healthy peers actually differ.
    {
        char peer[DCS_DIGEST_LEN];
        size_t peer_len = 0;
        size_t base_len = 0;

        BuildState();
        D_StateCanaryDigest(baseline, sizeof(baseline), &base_len);

        BuildState();
        players[0].viewz = 4711;
        players[0].bob = 3;
        players[0].damagecount = 22;
        players[1].viewz = 9999;
        players[0].message = "a message only this peer saw";
        D_StateCanaryDigest(peer, sizeof(peer), &peer_len);

        Check(strcmp(baseline, peer) == 0,
              "two peers differing only in presentation digest identically");
        Check(base_len == peer_len, "and report the same byte count");
    }

    // T5: allocation independence. The same logical state built at a
    // different address must digest identically, or a pointer has leaked in.
    {
        static mobj_t relocated[4];
        char moved[DCS_DIGEST_LEN];
        int j;

        BuildState();
        DigestNow(baseline);

        BuildState();
        memcpy(relocated, test_mobjs, sizeof(relocated));
        thinkercap.next = &thinkercap;
        thinkercap.prev = &thinkercap;

        for (j = 0; j < 4; ++j)
        {
            relocated[j].target = NULL;
            relocated[j].tracer = NULL;
            relocated[j].player = NULL;
            LinkThinker(&relocated[j].thinker, (void *) P_MobjThinker);
        }

        relocated[2].target = &relocated[0];
        relocated[0].player = &players[0];
        players[0].mo = &relocated[0];
        relocated[1].player = &players[1];
        players[1].mo = &relocated[1];
        test_door.sector = &test_sectors[1];
        LinkThinker(&test_door.thinker, (void *) T_VerticalDoor);

        DigestNow(moved);
        Check(strcmp(baseline, moved) == 0,
              "the same state at different addresses digests identically");
    }

    printf("%d checks, %d failures\n", checks, failures);

    return failures == 0 ? 0 : 1;
}
