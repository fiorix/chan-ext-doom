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

// dup/dup2/fileno, used to capture the report output under -std=c99.
#define _POSIX_C_SOURCE 200809L

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "d_statecanary.h"
#include "d_democanary.h"
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

// Computed outside this program, over the bytes D_StateCanarySerialize
// produced for the synthesized state:
//
//   $ ./dump_state > golden.bin
//   $ python3 -c "import hashlib; b=open('golden.bin','rb').read();
//                    print(len(b), hashlib.sha256(b).hexdigest())"
//   655 fef8b579617702eb735402472813a352916bbd0d02bf409378d91d647235c38f
//
// Regenerate deliberately if the format changes, never to make a red test
// green.

#define DCS_GOLDEN_LEN 655
#define DCS_GOLDEN_SHA256 \
    "fef8b579617702eb735402472813a352916bbd0d02bf409378d91d647235c38f"

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

// One of every other special class the serializer writes. The golden fixture
// deliberately holds only the door, so these are linked in by the focused
// cases below rather than by BuildState.
static ceiling_t test_ceiling;
static floormove_t test_floor;
static plat_t test_plat;
static lightflash_t test_flash;
static strobe_t test_strobe;
static glow_t test_glow;

static void RemoveThinker(thinker_t *th)
{
    th->prev->next = th->next;
    th->next->prev = th->prev;
}

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

// Capture one D_StateCanaryReport line.
//
// The capture is an anonymous tmpfile(), never a named path. An earlier
// version wrote a fixed /tmp file, which collided between concurrent runs and,
// worse, made freopen() follow whatever symlink happened to sit at that public
// name and truncate the target. A test must not carry a clobber primitive.
//
// Returns false on any setup failure rather than leaving the caller to assert
// on an empty buffer: "no sha256= token" passes trivially against no output at
// all, so a broken capture must be a failure, not a quiet pass.
static boolean CaptureReport(const char *at, char *out, size_t cap)
{
    FILE *tmp;
    int saved;
    long n;

    if (out == NULL || cap == 0)
    {
        return false;
    }

    out[0] = '\0';

    tmp = tmpfile();
    if (tmp == NULL)
    {
        return false;
    }

    saved = dup(fileno(stdout));
    if (saved < 0)
    {
        fclose(tmp);
        return false;
    }

    fflush(stdout);
    if (dup2(fileno(tmp), fileno(stdout)) < 0)
    {
        /* stdout was never redirected, so only the saved copy needs closing */
        close(saved);
        fclose(tmp);
        return false;
    }

    D_StateCanaryReport(at);
    fflush(stdout);

    /* restore on every path out from here, including the read failures */
    if (dup2(saved, fileno(stdout)) < 0)
    {
        close(saved);
        fclose(tmp);
        return false;
    }
    close(saved);
    clearerr(stdout);

    if (fseek(tmp, 0, SEEK_END) != 0)
    {
        fclose(tmp);
        return false;
    }

    n = ftell(tmp);
    rewind(tmp);

    if (n <= 0 || (size_t) n >= cap)
    {
        fclose(tmp);
        return false;
    }

    if (fread(out, 1, (size_t) n, tmp) != (size_t) n)
    {
        fclose(tmp);
        return false;
    }

    out[n] = '\0';
    fclose(tmp);
    return true;
}

#define DCS_SCRATCH_LEN_TEST (512 * 1024)

static unsigned int ReadU32(const byte *buf, size_t offset)
{
    return (unsigned int) buf[offset]
         | ((unsigned int) buf[offset + 1] << 8)
         | ((unsigned int) buf[offset + 2] << 16)
         | ((unsigned int) buf[offset + 3] << 24);
}

// Serialize the current state, then read the U32 at a known offset.
static unsigned int ReadU32At(byte *buf, size_t cap, size_t offset)
{
    size_t len = D_StateCanarySerialize(buf, cap);

    if (len < offset + 4)
    {
        return 0xffffffffu;
    }

    return ReadU32(buf, offset);
}

// One linker per special class. Each names its own sector field, so a struct
// whose layout differs from its neighbours cannot be silently mis-assigned.

static void LinkCeiling(void)
{
    memset(&test_ceiling, 0, sizeof(test_ceiling));
    test_ceiling.sector = &test_sectors[0];
    test_ceiling.topheight = 128;
    LinkThinker(&test_ceiling.thinker, (void *) T_MoveCeiling);
}

static void LinkFloor(void)
{
    memset(&test_floor, 0, sizeof(test_floor));
    test_floor.sector = &test_sectors[0];
    test_floor.floordestheight = 64;
    LinkThinker(&test_floor.thinker, (void *) T_MoveFloor);
}

static void LinkPlat(void)
{
    memset(&test_plat, 0, sizeof(test_plat));
    test_plat.sector = &test_sectors[0];
    test_plat.wait = 105;
    LinkThinker(&test_plat.thinker, (void *) T_PlatRaise);
}

static void LinkFlash(void)
{
    memset(&test_flash, 0, sizeof(test_flash));
    test_flash.sector = &test_sectors[0];
    test_flash.maxtime = 64;
    LinkThinker(&test_flash.thinker, (void *) T_LightFlash);
}

static void LinkStrobe(void)
{
    memset(&test_strobe, 0, sizeof(test_strobe));
    test_strobe.sector = &test_sectors[0];
    test_strobe.darktime = 15;
    LinkThinker(&test_strobe.thinker, (void *) T_StrobeFlash);
}

static void LinkGlow(void)
{
    memset(&test_glow, 0, sizeof(test_glow));
    test_glow.sector = &test_sectors[0];
    test_glow.direction = 1;
    LinkThinker(&test_glow.thinker, (void *) T_Glow);
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

    // T1 golden vector. The length and digest below are checked-in
    // literals: the bytes came from D_StateCanarySerialize, but the hash was
    // computed by an independent implementation (python hashlib) over those
    // bytes, not by D_StateCanaryDigest. That matters, because a baseline
    // generated by the code under test moves when the code moves, so a field
    // reorder or a width change would update both sides of the comparison
    // and stay green. This is the assertion that does not.
    {
        byte buf[8192];
        size_t direct;
        char golden[DCS_DIGEST_LEN];

        BuildState();
        direct = D_StateCanarySerialize(buf, sizeof(buf));

        ++checks;
        if (direct != DCS_GOLDEN_LEN)
        {
            ++failures;
            printf("FAIL: golden layout: serialized %u bytes, expected %u\n",
                   (unsigned int) direct, (unsigned int) DCS_GOLDEN_LEN);
        }

        D_DemoCanaryRawDigest(buf, direct, golden, sizeof(golden));

        ++checks;
        if (strcmp(golden, DCS_GOLDEN_SHA256) != 0)
        {
            ++failures;
            printf("FAIL: golden layout: digest %s, expected %s\n",
                   golden, DCS_GOLDEN_SHA256);
        }
    }

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

    // FU16 robustness: a null reference and a non-null one that does not
    // resolve to a live mobj are different facts. Collapsing them would hide
    // the case where a reference and the enumeration disagree, which is
    // exactly the situation worth seeing.
    {
        static mobj_t orphan;
        char with_null[DCS_DIGEST_LEN];
        char with_orphan[DCS_DIGEST_LEN];

        BuildState();
        test_mobjs[2].target = NULL;
        DigestNow(with_null);

        BuildState();
        // Not linked into the thinker list, so it is live memory the
        // enumeration will not find.
        memset(&orphan, 0, sizeof(orphan));
        orphan.state = &states[0];
        test_mobjs[2].target = &orphan;
        DigestNow(with_orphan);

        Check(strcmp(with_null, with_orphan) != 0,
              "a null target and an unresolvable target are distinguished");
    }

    // FU16 robustness: an inactive slot must contribute its marker and
    // nothing else. Its storage is stale, and mo may hold a pointer that is
    // not safe to follow, so the serializer must never dereference it.
    // Under the sanitized build this case also fails loudly if it does.
    {
        char clean[DCS_DIGEST_LEN];
        char stale[DCS_DIGEST_LEN];

        BuildState();
        playeringame[3] = false;
        players[3].mo = NULL;
        DigestNow(clean);

        BuildState();
        playeringame[3] = false;
        // Deliberately not a valid object. If the serializer reads through
        // it, this crashes rather than quietly hashing garbage.
        players[3].mo = (mobj_t *) (void *) &checks;
        players[3].health = 12345;
        players[3].killcount = 999;
        DigestNow(stale);

        Check(strcmp(clean, stale) == 0,
              "an inactive slot's stale storage and unsafe mo are not serialized");
    }

    // --- every special-writer branch ---------------------------------------

    // The golden fixture carries only a door, so the other six writers were
    // never executed. Executing them is not enough either: a branch that
    // wrote nothing would still "run", so each case mutates a scalar that
    // only that class serializes and requires the digest to move.
    {
        // Each class is linked by a small function that also gives it a real
        // sector. Assigning through a pointer offset would be shorter and
        // wrong: sector is not the first field after the thinker in every one
        // of these structs, so it would silently write into type instead.
        size_t c;

        struct { const char *name; void (*link)(void); } classes[] = {
            { "ceiling", LinkCeiling },
            { "floor",   LinkFloor },
            { "plat",    LinkPlat },
            { "flash",   LinkFlash },
            { "strobe",  LinkStrobe },
            { "glow",    LinkGlow },
        };

        for (c = 0; c < sizeof(classes) / sizeof(classes[0]); ++c)
        {
            char without[DCS_DIGEST_LEN];
            char with[DCS_DIGEST_LEN];
            size_t len_without = 0;
            size_t len_with = 0;

            BuildState();
            D_StateCanaryDigest(without, sizeof(without), &len_without);

            BuildState();
            classes[c].link();
            D_StateCanaryDigest(with, sizeof(with), &len_with);

            ++checks;
            if (len_with <= len_without)
            {
                ++failures;
                printf("FAIL: the %s writer added no bytes\n", classes[c].name);
            }

            ++checks;
            if (strcmp(without, with) == 0)
            {
                ++failures;
                printf("FAIL: adding a %s special did not change the digest\n",
                       classes[c].name);
            }
        }
    }

    // Class-specific scalars, one per branch, so a writer that emitted the
    // right number of bytes from the wrong fields would still fail.
    {
        char base[DCS_DIGEST_LEN];
        char moved[DCS_DIGEST_LEN];
        size_t n = 0;
        size_t c;

        struct { const char *name; int *field; } scalars[7];

        BuildState();
        LinkCeiling();
        LinkFloor();
        LinkPlat();
        LinkFlash();
        LinkStrobe();
        LinkGlow();
        D_StateCanaryDigest(base, sizeof(base), &n);

        scalars[0].name = "ceiling topheight";   scalars[0].field = &test_ceiling.topheight;
        scalars[1].name = "door topcountdown";   scalars[1].field = &test_door.topcountdown;
        scalars[2].name = "floor floordestheight"; scalars[2].field = &test_floor.floordestheight;
        scalars[3].name = "plat wait";           scalars[3].field = &test_plat.wait;
        scalars[4].name = "flash maxtime";       scalars[4].field = &test_flash.maxtime;
        scalars[5].name = "strobe darktime";     scalars[5].field = &test_strobe.darktime;
        scalars[6].name = "glow direction";      scalars[6].field = &test_glow.direction;

        for (c = 0; c < 7; ++c)
        {
            size_t m = 0;

            *scalars[c].field += 7;
            D_StateCanaryDigest(moved, sizeof(moved), &m);

            ++checks;
            if (strcmp(base, moved) == 0)
            {
                ++failures;
                printf("FAIL: %s does not reach the digest\n", scalars[c].name);
            }

            *scalars[c].field -= 7;
        }

        /* back to the baseline, so the loop above really isolated each field */
        D_StateCanaryDigest(moved, sizeof(moved), &n);
        Check(strcmp(base, moved) == 0, "restoring every scalar restores the digest");
    }

    // The class set is written twice in the serializer: once to count the
    // specials and once to write them. Nothing above would notice the two
    // lists drifting apart, because a wrong count still changes the digest
    // consistently on both peers. So read the count field back directly.
    //
    // Specials are the last section and the count is the U32 that opens it,
    // so with no specials linked the whole section is that one field: it sits
    // at the end of the stream, and it stays there when specials are added.
    {
        byte buf[DCS_SCRATCH_LEN_TEST];
        size_t bare;
        size_t offset;

        BuildState();
        RemoveThinker(&test_door.thinker);
        bare = D_StateCanarySerialize(buf, sizeof(buf));
        Check(bare >= 4, "a state with no specials still writes the count");
        offset = bare - 4;
        Check(ReadU32(buf, offset) == 0, "and that count reads zero");

        BuildState();
        Check(ReadU32At(buf, sizeof(buf), offset) == 1,
              "the door-only fixture counts one special");

        BuildState();
        LinkCeiling();
        LinkFloor();
        LinkPlat();
        LinkFlash();
        LinkStrobe();
        LinkGlow();
        Check(ReadU32At(buf, sizeof(buf), offset) == 7,
              "all seven classes are counted, not just the ones written");
    }

    // --- empty world ------------------------------------------------------

    {
        char empty[DCS_DIGEST_LEN];
        size_t len = 0;

        BuildState();
        memset(playeringame, 0, sizeof(playeringame));
        numsectors = 0;
        thinkercap.next = &thinkercap;
        thinkercap.prev = &thinkercap;

        Check(D_StateCanaryDigest(empty, sizeof(empty), &len),
              "an empty world still digests");
        Check(len > 0, "an empty world has a nonzero header");
        Check(strlen(empty) == DCS_DIGEST_LEN - 1, "and a full-length digest");
    }

    // --- an active player with no body ------------------------------------

    // MutPlayerInGame reaches this branch, but only as a side effect. Named
    // here so the contract is explicit rather than incidental.
    {
        char bodied[DCS_DIGEST_LEN];
        char bodiless[DCS_DIGEST_LEN];
        size_t a = 0;
        size_t b = 0;

        BuildState();
        D_StateCanaryDigest(bodied, sizeof(bodied), &a);

        BuildState();
        players[0].mo = NULL;
        Check(D_StateCanaryDigest(bodiless, sizeof(bodiless), &b),
              "an in-game player with no body still digests");
        Check(strcmp(bodied, bodiless) != 0,
              "losing a body changes the digest");
        Check(b < a, "and writes fewer bytes, since no position is recorded");
    }

    // The capture mechanism itself has to be shown to work, or the oversize
    // assertions below would pass against an empty buffer for the wrong
    // reason: "emits no sha256= token" is trivially true of no output at all.
    // So capture a report that does succeed, and require the token to appear.
    {
        char buf[512];

        BuildState();
        Check(CaptureReport("exit", buf, sizeof(buf)),
              "a successful report is captured");
        Check(strstr(buf, "sha256=") != NULL,
              "and the capture really does observe the sha256 token");
        Check(strstr(buf, "bytes=") != NULL, "and the byte count");
        Check(strstr(buf, "state did not fit") == NULL,
              "and a fitting state is not reported as a refusal");
    }

    // --- a state too large for the scratch buffer -------------------------

    // The serializer refuses rather than truncating. A truncated digest would
    // be the worst possible outcome: a stable, comparable, wrong answer. So
    // the refusal must be total -- no digest, no byte count, and above all no
    // sha256= token on the wire that the verdict layer could parse.
    {
        // The scratch size is private to the serializer, so this is sized
        // from the documented 512 KiB contract rather than from the macro. If
        // that buffer ever grows, the refusal assertions below fail loudly
        // instead of quietly testing nothing.
        size_t huge_count = ((512 * 1024) / 8) + 4096;
        sector_t *huge = calloc(huge_count, sizeof(sector_t));
        char digest[DCS_DIGEST_LEN];
        size_t len = 12345;
        size_t fitted = 0;
        char fitted_digest[DCS_DIGEST_LEN];

        if (huge == NULL)
        {
            ++failures;
            printf("FAIL: could not allocate the oversize fixture\n");
        }
        else
        {
            size_t i;

            /* a digest from a state that does fit, to compare against */
            BuildState();
            D_StateCanaryDigest(fitted_digest, sizeof(fitted_digest), &fitted);

            BuildState();
            for (i = 0; i < huge_count; ++i)
            {
                huge[i].floorheight = (fixed_t) i;
                huge[i].lightlevel = (short)(i & 0xff);
            }
            numsectors = (int) huge_count;
            sectors = huge;

            Check(D_StateCanarySerialize(NULL, 0) == 0,
                  "serializing an oversize state into no buffer writes nothing");

            memset(digest, '@', sizeof(digest));
            Check(!D_StateCanaryDigest(digest, sizeof(digest), &len),
                  "an oversize state refuses to digest");
            Check(len == 12345, "and leaves the byte count untouched");

            for (i = 0; i < sizeof(digest); ++i)
            {
                if (digest[i] != '@') break;
            }
            Check(i == sizeof(digest),
                  "and does not write a single byte of the digest buffer");

            // The report is the only thing the verdict layer ever sees, so
            // assert on its actual output rather than on the return path.
            {
                char buf[512];

                Check(CaptureReport("exit", buf, sizeof(buf)),
                      "the oversize report is captured");
                Check(strstr(buf, "state did not fit, no digest") != NULL,
                      "the oversize report names the refusal");
                Check(strstr(buf, "sha256=") == NULL,
                      "and emits no sha256= token the verdict could parse");
                Check(strstr(buf, "bytes=") == NULL,
                      "and no byte count either");
            }

            /* the successful path is unaffected by the failed one */
            free(huge);
            BuildState();
            {
                char again[DCS_DIGEST_LEN];
                size_t n = 0;
                Check(D_StateCanaryDigest(again, sizeof(again), &n),
                      "a fitting state still digests after a refusal");
                Check(strcmp(again, fitted_digest) == 0 && n == fitted,
                      "and produces exactly the same result as before it");
            }
        }
    }

    printf("%d checks, %d failures\n", checks, failures);

    return failures == 0 ? 0 : 1;
}
