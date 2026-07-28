# engine provenance and build recipe

`engine/` is the doom fork this repo builds on: a Crispy base with the Chocolate network layer restored on top. It is imported source, not vendored-and-forgotten. Every file taken from upstream is listed with the revision and blob it came from, and every deviation from upstream is listed with the reason.

## Upstream

| field | value |
|--------------|--------------------------------------------------------|
| base project | rojo2/wasm-doom |
| url | https://github.com/rojo2/wasm-doom |
| commit | 619e69715ec303e9f7d192a0ba2d4025cf485214 |
| ref | master |
| lineage | Crispy Doom, itself Chocolate Doom, itself id's release |
| license | GPL-2.0 (`../LICENSE`, `../COPYING.md`) |

The base import is the tracked tree at that commit and nothing else: 253 files, taken with `git archive` so no `.git` metadata and no build outputs can come along.

```sh
git clone https://github.com/rojo2/wasm-doom /tmp/wasm-doom-ref
git -C /tmp/wasm-doom-ref archive --format=tar \
    619e69715ec303e9f7d192a0ba2d4025cf485214 | tar -x -C engine/
```

### Network layer

rojo2 deleted every network implementation file. `src/net_defs.h` survived intact, and it is byte-identical to crispy-doom's, which is what makes the restoration mechanical rather than a rewrite: the contract the deleted files implemented is still in the tree.

Restored from **crispy-doom-5.6.2** (commit `d5df2fd3e59d5e1ca362f5879a77e5aea5c30b71`, 2019-09-12), the last Crispy release before rojo2's final commit. Its `src/net_defs.h` blob is `ab852c08eec9fda948b6a2664b5179035e4c2e27`, identical to the one already in this tree.

| file | upstream blob |
|-----------------------|------------------------------------------|
| src/aes_prng.c | 06f9ac4c0a4a4c1b0be1cbcc4dd8b0b31bfec3a3 |
| src/aes_prng.h | 79a3f8d75a1f6c0e63d20b31a4ba69a7d2ccbe20 |
| src/net_client.c | 3799b0e13e8d1cdef8ddbe57ceae903b450b40b8 |
| src/net_client.h | 37ba01984e4e2526bce50c66c2c038471dd1f436 |
| src/net_common.c | d3c40d23c0acc0c8de6c82f2c9030c99fc953128 |
| src/net_common.h | 15a7684aef8554a81db39fc115dec9aa9a336dae |
| src/net_gui.h | 4f4198b60878728386568a6991aafd76869d2389 |
| src/net_io.c | 02519afe21d730cc82e5d0f470314a63311b44eb |
| src/net_io.h | d61a1eb3d9f3c5527e891b119c80de5c449378fa |
| src/net_loop.c | 65140fa3bc9cbdc283d766727bb904fd87541291 |
| src/net_loop.h | 5a2e58ee1c9669ded5d97acfa720c3fe1d73242e |
| src/net_packet.c | 23bfda675e5f2d9e477b7e40da5e0e64b834ebf3 |
| src/net_packet.h | 6beb44b2aacd8d6e7c75d27e187ba7ea643e0e7c |
| src/net_petname.c | db6600db6a2ad15771feb4142ee4ae766b517a63 |
| src/net_petname.h | 292250744022ea0d8d96348c4f5046c01ac2a448 |
| src/net_query.c | 4f19a8b75ab25be4773c30c1b831795f39334576 |
| src/net_query.h | 6a92eae8c8bc57aa644c71e70cf4e059c54592a7 |
| src/net_sdl.c | c9b3e81f36a6c60ed9fa3edaf72df3df6cc7bae7 |
| src/net_sdl.h | c249de1c490cb99899505b19e0527ebde8c5ade4 |
| src/net_server.c | e0d752e0cf4aab573fa01f073af4b7c90855380f |
| src/net_structrw.c | 437bc71a5ab550f454fadc7166fef68ad4feabb3 |
| src/net_structrw.h | 68de20dd89aa1283759659151cbbef6214c0c2f9 |
| src/doom/d_net.c | 24adeb706a983e9888d2ee3914c5ad75e6de7a9b |
| src/d_loop.c | replaced by the 5.6.2 file, see deviations |

`aes_prng` is not optional decoration: `net_structrw` reads and writes the secure-demo PRNG seed, so it is part of the wire format.

Not taken: `net_dedicated.c` (this repo's dedicated server is the Rust one, not this engine) and `net_gui.c` (see deviations).

Reproduce any of them:

```sh
git clone https://github.com/fabiangreffrath/crispy-doom /tmp/crispy-ref
git -C /tmp/crispy-ref show crispy-doom-5.6.2:src/net_client.c
```

## License boundary

`engine/` is GPL-2.0 and stays that way. The Rust crates under `crates/` are Apache-2.0 and never link, embed, or statically include anything built from this directory: `doom.js` and `doom.wasm` are runtime data, fetched over HTTP by a browser or instantiated inside a wasmtime sandbox.

The separation is a runtime isolation boundary, not always an OS process boundary. A browser tab may run the engine in the same process as other content, and a wasmtime embedding runs it inside the host process. What holds in every case is that the GPL engine is loaded as data into a sandboxed instance with a defined interface, and no GPL object code is linked into an Apache-2.0 binary.

Redistributing a build of this engine means shipping the corresponding source: this directory, including every deviation below.

## Deviations from upstream

### Base tree

**`src/v_trans.h:38`, `enum` to `typedef enum`.** Required to link at all with any modern clang.

```diff
-enum
+typedef enum
 {
     CR_NONE,
```

Upstream's form declares a tentative global named `cr_t` in every translation unit that includes the header. Compilers defaulting to `-fno-common`, which clang has since clang 11, reject the link with `duplicate symbol: cr_t`. `cr_t` is not used as a type anywhere in the tree, so the typedef spelling changes nothing but the link.

**`src/doom/d_main.c`, shareware `-file` guard removed.** id's shareware build refused `-file` outright. This fork allows PWADs on the shareware IWAD, because mods are first-class runtime data here and the ordered PWAD set is what peers agree on before a net game. This is an intentional policy departure from id's historical shareware restriction, authorised for this fork. `gamemode` stays `shareware`; the registered-IWAD lump check is untouched and nothing pretends to own the registered episodes.

**`src/doom/d_main.c`, network glue returned to `d_net.c`.** rojo2 deleted `doom/d_net.c` and transplanted a cut-down copy of its contents into `d_main.c`: `netcmds`, `RunTic`, `doom_loop_interface`, `LoadGameSettings`, `SaveGameSettings`, and an inlined single-player stand-in for `D_CheckNetGame`. With the real `d_net.c` restored those collide, and more importantly the inlined stand-in meant the restored `D_CheckNetGame` would never run. `d_main.c` now declares `D_ConnectNetGame` / `D_CheckNetGame` and calls them where upstream does, and the transplanted copies are gone. `NET_Init()` was also restored: rojo2 had kept its log line but deleted the call.

**`src/doom/d_net.c`, one line dropped.** `player->mo->interp = false;` refers to a Crispy interpolation field this base predates. Same class as the `d_loop.c` removals below.

**`src/d_loop.c` replaced with the crispy-doom-5.6.2 file, minus what this base cannot compile.** rojo2 had gutted it: no net includes, `D_StartNetGame` hardcoded to one player, `GetLowTic` never waiting, `D_InitNetGame` always false, `D_QuitNetGame` empty, `PlayersInGame` always true. Restoring it is the point of the exercise. The 5.6.2 file needs three edits to build here, none of them network-related:

- `#include "crispy.h"`, `oldleveltime`, and the `return_early` uncapped-framerate shortcut are removed. This base has no `crispy.h`.
- `D_NonVanillaRecord` / `D_NonVanillaPlayback` take `char *feature`, not `const char *`. This tree's `d_loop.h` predates the const change.
- `net_sdl_module` becomes `NET_TRANSPORT_MODULE` (see below).

**`src/net_loop.c`, dropped packets are freed.** Both send paths hand `QueuePush` a `NET_PacketDup`, so the queue owns it, but upstream returns on a full ring without freeing, leaking one packet per overflow. This transport carries the in-WASM host talking to its own client, so it is on the browser network path and the leak is reachable there. `test/test_net_loop.c` pins the behaviour against a counting allocator.

**`src/net_query.c`, `src/net_server.c`.** `#include "net_sdl.h"` becomes `#include "net_transport.h"`, and `net_query.c`'s two `net_sdl_module` uses become `NET_TRANSPORT_MODULE`. Without this the browser build fails to link, because `net_sdl.c` is not compiled there.

**`src/i_timer.c`, `I_Sleep` yields under emscripten.** `SDL_Delay` busy-waits in a wasm build, which starves the browser event loop. That is fatal for the network code specifically, because Chocolate sleeps exactly when it is waiting for the network to progress: the connect loop, the wait for the host to launch, and the tic stall all call `I_Sleep`. With a busy wait the WebSocket callbacks cannot run, so a connect could only ever time out. This was observed, not theorised: before the change the client logged `Failed to connect to ws node 1: No response from server` and only then reported the socket opening. Yielding requires ASYNCIFY, which the recipe enables.

### New files

**`src/doomtype.h`, `boolean` given one fixed ABI.** Upstream selects builtin `bool` when `<stdbool.h>` has already been seen and a four-byte enum otherwise, so the type's width follows whatever a translation unit happened to include first. That is not survivable in this tree: `g_game.c` includes `<emscripten.h>`, and so `<stdbool.h>`, before the Doom headers, while `p_setup.c` reaches the Doom headers first. `player_t` carries `boolean` members (`cards[]`, `backpack`, `didsecret`), so the two objects disagreed about its layout: 304 bytes against 328, with `killcount` at offset 196 against 216. A write to `players[i]` used one stride against an allocation made with the other and overran into neighbouring globals, corrupting `gameepisode` and `deathmatch`.

It is `typedef int boolean` now, with `true`/`false` defined only when `<stdbool.h>` has not already supplied them. `int` keeps this lineage's historical four-byte layout and matches the enum branch's semantics exactly: neither normalises an assignment to 0 or 1, so no existing code changes behaviour.

This was core simulation memory corruption. It surfaced as a demo header recording the wrong episode, which is worth remembering: the visible symptom was several layers away from the fault, and the header only exposed it because the armed line names its fields.

`test/test_abi.c` links two objects that reach the headers in opposite orders and compares widths and offsets across `player_t`, `wbplayerstruct_t`, `wbstartstruct_t`, `vertex_t`, `anim_t`, `ceiling_t`, `floormove_t`, `deh_mapping_entry_t` and `net_full_ticcmd_t`. Those are the shared structs whose layout can actually move. `plat_t` and the ST/HU widget structs carry single `boolean` value fields, which measure identically under the old split because the difference is absorbed into padding, so equality checks on them would not bite; the assertion that `sizeof(boolean)` is 4 is what really guards them. `animdef_t` is not in scope at all: it lives inside `p_spec.c` rather than a header, it is `PACKED_STRUCT`, and its `istexture` is a `signed char`. The `boolean istexture` in `p_spec.h` belongs to `anim_t`, which is covered.

Two of those actually diverge under the old conditional, and the pattern is worth knowing: the damage comes from `boolean` *arrays*, where the width multiplies. `player_t` (which holds `cards[6]`) moved 304 to 328 bytes with `killcount` at offset 196 against 216, and `net_full_ticcmd_t` (which holds `playeringame[8]`) moved 176 to 200. A struct with a single `boolean` field mostly absorbs the difference into padding and looks fine, which is exactly why per-field reasoning is not a substitute for pinning the width. `net_full_ticcmd_t` is the one with wire consequences: it crosses `net_client`, `net_server` and `d_loop`, so a split there would have shifted the whole GAMEDATA fan-out. The formats themselves are safe regardless, because `p_saveg` and `net_structrw` read and write explicit widths, so no demo, savegame or packet ever carried the raw ABI.

Rebuild everything after touching this. A partial rebuild leaves some objects on the old layout and reproduces the fault in a new shape. The `emcc` recipe is safe by construction, since it compiles every source in one invocation; the CMake route keeps separate objects and needs a fresh build directory.

**`src/doom/d_main.c`, the registered main loop is held until startup finishes.** The loop is registered at the top of `D_DoomMain`, and ASYNCIFY makes that reachable long before the game is ready: every `I_Sleep` during the network connect and launch waits yields to the browser, which can dispatch an iteration. `D_DoomLoopIter` therefore returns immediately until `main_loop_started`, and that flag is now set at the *end* of `D_DoomLoop`'s initialisation rather than the beginning. WebSocket callbacks still run during those yields, which is the point of yielding; only gameplay tics are held back from partially initialised state.

**`src/doom/g_game.c`, the demo writer restored.** `G_WriteDemoTiccmd` was an empty stub, so `-record` produced a header and no ticcmds. Restored from crispy-doom-5.6.2 (`src/doom/g_game.c`, blob `efcba42943083ac684d71773ded164a1cd5f705d`), dropping only the `crispy->fliplevels` prologue, the same exclusion class as the `d_loop.c` restoration. The write, rewind, re-read round trip is kept deliberately: the re-read canonicalises the recording peer's own turn to the demo format's resolution, and without it that peer simulates a full-precision turn while every other peer simulates the quantised one, which desyncs a recorded netgame by construction.

Overflow keeps upstream's vanilla stop rather than porting `IncreaseDemoBuffer`. A verification run raises `-maxdemo` instead, which the loader does: a buffer that grows on one side only would truncate the other and read as a desync that never happened.

**`src/doom/d_main.c`, `G_BeginRecording` given a call site, and not upstream's.** The function existed with no caller at all. Upstream begins recording in `D_DoomLoop` immediately before the loop starts, but that site does not work here: this fork registers its emscripten main loop at the top of `D_DoomMain`, so tics can run before `D_DoomLoop` is reached. With `demo_p` still null at that point, `G_WriteDemoTiccmd`'s re-read walks uninitialised memory, finds a stray `0x80` DEMOMARKER and calls `G_CheckDemoStatus`, which clears `demorecording` again with no diagnostic. `G_WriteDemoTiccmd` now fails closed on those invariants, so that path is gone regardless of ordering.

Arming happens after the selected `G_InitNew`, held until then in an explicit pending name. The header records `gameskill`, `gameepisode` and `gamemap`, and `G_InitNew` is what assigns them; `D_CheckNetGame` settles only the `start*` values they derive from. Arming any earlier writes a header for map 0 of episode 0 at skill 0, which both peers write identically, so no digest comparison can catch it.

The header was wrong for a while after that move, reporting `skill=0 episode=0` with a nonsense `deathmatch`. That turned out not to be a demo problem at all: it was the `boolean` ABI split described above corrupting those globals. With the ABI fixed, a single-player run reports `version=109 skill=2 episode=1 map=1 deathmatch=0`, and two peers report identical headers differing only in `consoleplayer`.

**`-verifydemo` unbinds the demo-quit key for verification runs.** `'q'` is the inherited upstream default for `key_demo_quit` in both lineages, but this fork rebound `key_fire` from right-control to `'q'` for browser friendliness, and `G_WriteDemoTiccmd` polls the quit key on every recorded tic, so together they ended a recording on the player's first shot. The default is left at upstream's value and the loader passes `-verifydemo` alongside `-record`, which unbinds the key for that run only. This also stops a persistent configured binding from ending a canary, and leaves ordinary demo recording behaving exactly as upstream does.

**`src/d_democanary.c`, `src/d_democanary.h`.** The desync canary. SHA-256 over the 13-byte header with the `consoleplayer` byte zeroed, plus the ticcmd stream bounded at the exit anchor. Every peer records every in-game player's ticcmd in player order from the same fanned-out `netcmds`, so equal digests mean the peers saw the same inputs. Two differences are legitimate and must not trip it: byte 8 is each peer's own player index, and the recording keeps growing after the level ends while the two humans quit at different moments.

It proves input history, not simulation state. This fork writes no consistancy bytes, so a simulation bug fed identical inputs passes this check. SHA-256 is implemented in that file rather than reusing `sha1.c`, which is both a stronger digest for something whose job is to be believed and free of the SDL headers `sha1.c` pulls in.

**`src/net_transport.h`.** Selects the `net_module_t` that carries packets off the machine: WebSockets under `__EMSCRIPTEN__`, SDL_net UDP otherwise. Everything above it is transport-agnostic, so this is the only place the choice is made.

**`src/net_websockets.c`, `src/net_websockets.h`.** The browser transport, carrying the Chocolate protocol to a room router. Modelled on `net_sdl.c` and on cloudflare/doom-wasm's module at commit `65e0d3ae2ffa604155eebd96ed40da6567bd08f4`, whose wire format it preserves exactly. It is a reimplementation rather than a copy, because the reference carries defects that are not safe to inherit:

| reference behaviour | here |
|---|---|
| `NET_NewPacket(numBytes - 4)` with no length check; an under-4-byte frame underflows an unsigned length into a huge allocation and copy | the frame is parsed and rejected before any arithmetic |
| no upper bound on frame size | frames above `NET_WS_MAX_FRAME` are rejected |
| a full receive queue returns without freeing the packet, leaking one per overflow | the dropped packet is freed and the drop is counted |
| `FreeAddress` calls `free()` on entries in a static array | the address table owns its entries, as in `net_sdl.c`, and `FreeAddress` returns the slot |
| addresses are never reclaimed; the table only grows | freed slots are reused, and the table grows only when genuinely full |
| `ips[]` is zero-initialised, so a lookup for node 0 matches an unused slot | occupancy is explicit; node 0 is reserved and never allocated |
| node id is `rand() % 0xfffe` seeded from `time(NULL)`, so two clients starting in the same second collide and silently steal each other's traffic | drawn from `getentropy`, with reserved ids retried |
| `atoi` on the resolve address, so any non-numeric string silently becomes node 0 | parsed with `strtoul` and rejected if it is not a node id |
| blocks on `emscripten_sleep` waiting for the socket | connects asynchronously; the fix for the event-loop problem is in `I_Sleep`, where it also helps the launch wait and the tic stall |
| a send failure flips state without closing the handle, so a later send can build a second socket while the first one's callbacks still mutate the same globals | a failure closes and deletes the handle, drains the ring and is terminal for the module; callbacks carrying a discarded handle are ignored |
| a failed announce only prints, leaving a host that never claimed the room live and retrying forever | an announce failure takes the same terminal path, because a host the router never learned about is unreachable |
| `net_websockets.h` uses the `NET_SDL_H` include guard | its own guard |

The socket lifecycle is deliberately terminal rather than self-healing. An in-place reconnect would have to close the old handle, drain the ring without discarding live packets, and keep callbacks from the dead handle away from the replacement's state. That is a lot of hazard for no benefit here, because the host page's model is that a relaunch tears down the whole WASM instance: a session that loses its socket is over. Failures therefore close and delete the handle, drain the queue, and refuse further use, and events arriving from a discarded handle are ignored.

**`src/net_ws_frame.c`, `src/net_ws_frame.h`.** The envelope codec and bounded receive ring, split out of `net_websockets.c` so they carry no emscripten dependency and can be tested on the host. These are the parts that must reject malformed input and must drop rather than grow, so they are the parts worth testing off-target.

**`src/net_gui.c` replaced, not restored.** Upstream draws a lobby with the textscreen widget library. This tree has no textscreen library, only `i_txt.c`, which is SDL text-mode emulation for ENDOOM. The surfaces that embed this engine own their lobby anyway: the loader page in the browser, the host program in a native embedding. So `NET_WaitForLaunch` is headless here. It keeps the semantics that matter, on stdout rather than in widgets: the `-nodes N` auto-launch when the controller sees enough nodes, the WAD and dehacked SHA-1 mismatch warning, and the fatal error if the connection drops while waiting.

## Exit-state canary (DCS1)

The demo canary proves two peers saw the same inputs. It cannot prove they reached the same state: this fork writes no consistancy bytes, so a simulation that diverged on identical inputs passes it. `src/doom/d_statecanary.c` serializes the simulation itself at the same anchor and hashes that, and the two lines stay separately labelled so neither claim absorbs the other.

Both fire from the `ga_completed` branch of `G_Ticker`, before `G_DoCompleted`. `G_Ticker` handles gameaction at the top of the tic *after* the exit tic, so that point observes a fully advanced simulation rather than the partially advanced one visible inside `G_ExitLevel`, and it precedes `G_PlayerFinishLevel` stripping cards and powers. Every peer reaches it on the same gametic because the exit is a simulation event, and the secret exit routes through the same branch.

The stream is `DCS1`, explicit fixed-width little-endian, in four sections after the header: players in player order, sectors in sector-index order, live mobjs in thinker-list order, then active special thinkers. `P_AddThinker` appends and removal only unlinks, so thinker order is allocation order and is lockstep-deterministic.

No addresses reach the digest, which is the property that makes it comparable at all. A mobj's state becomes its `states[]` index, its player becomes the player index plus one, and `target`/`tracer` become indices into the enumerated live-mobj list. A null reference and a non-null reference that does not resolve to a live mobj get distinct reserved values, because those are different facts and collapsing them would hide the second. Special thinkers name their sector by index.

The header carries `gametic`, `leveltime`, episode, map, skill, deathmatch and `prndindex`. The RNG index is the subtle one and it is not optional: two peers can hold identical visible state and still diverge on the next roll if it differs. `rndindex` is presentation-side and stays out.

An absent player slot contributes its `playeringame` marker and nothing else. Its storage is stale rather than zero and its `mo` is not safe to dereference, so there is nothing there worth hashing.

Excluded on purpose, because two healthy peers legitimately differ in all of it: view and camera fields, `bob`, colormaps, `extralight`, damage and bonus counts, the message and attacker pointers, psprite animation, `consoleplayer` and `displayplayer`, demo and network bookkeeping, and the per-instance PRNG. Sector `oldspecial` and mobj `validcount` are bookkeeping rather than simulation and are out too.

Known residual, recorded rather than hidden: weapon psprite animation is excluded, so a divergence that lives only in mid-fire animation and never reaches `readyweapon`, `pendingweapon`, ammo or `refire` would not be caught. If one ever escapes that way, per-player psprite state index and tics become format v2.

The special-thinker set is the `p_saveg` one, and two live thinkers sit outside it. `T_MoveElevator` and `T_FireFlicker` run as ordinary thinkers but are not among the seven classes the serializer writes, so their *phase* is not in the stream. Their effects are: an elevator drives `T_MovePlane`, whose floor and ceiling heights are serialized, and a flicker writes `sector->lightlevel`, which is serialized too. A flicker additionally draws from `P_Random`, so a phase difference perturbs `prndindex`, which the header carries. The gap is therefore narrow rather than open: a divergence is missed only while it remains confined to the internal phase and has not yet produced a different height, light level or RNG draw. Closing it fully means writing elevator direction and destination and the flicker counters, which is format v2 work alongside psprites.

## Automatic verdict

Both canary lines are text on a console nobody reads during a two-window run, so the loader pages compare them for you and display `M1 CANARY MATCH`, `MISMATCH` or `PENDING`. The reducer is `canary_verdict.js`, shared verbatim between the pages and the tests, so the logic under test is the logic that ships.

A result is only ever built from a fully parsed line: `bytes=<n> sha256=<64 hex>`, anchored at both ends. The refusal notices carry no digest and parse to nothing, so a state that overflowed the serializer's scratch buffer stays `PENDING` and can never read as agreement. Two peers that both failed to produce a digest agree about nothing, and the display says so.

Comparison is gated on exit identity: episode, map and gametic must match before two reports are compared at all. Peers are routinely a moment apart, and comparing across that boundary would report a divergence that is really one window being ahead.

**Trust boundary.** Pages pair over a `BroadcastChannel`, which every page on the origin can read and write. Session ids are therefore public to any same-origin participant. A malicious same-origin page can read the live partner's session id, name it in `replaces`, displace the honest peer, reflect a later report back, and leave the victim displaying a stable `M1 CANARY MATCH` while the real peer stays locked out. The protocol authenticates nobody. A shared token would not change this, because it would be readable through the same origin; and code that can do any of the above can equally patch the reducer or write the verdict into the DOM directly. So state it plainly: the verdict is evidence only while every script and page served from that origin is trusted. It is a desync detector for a cooperating pair, not a security control.

The perimeter is therefore not just the origin's served files but **every live tab and window on that origin**, because any one of them can reach the channel. A page that is merely open counts. M1 acceptance controls the perimeter operationally rather than in code: close every other tab and window on the origin, and run with exactly the two prepared windows, both visible.

Forward boundary, recorded now so it is not rediscovered later: when this verdict eventually rides inside the chan SPA origin at M4, it stays what it is here, a lobby convenience and desync aid, not a security signal. The perimeter there is that whole origin and everything running in it, which is a far larger surface than two purpose-opened windows. If the verdict is ever made to gate a real action, its trust model and architecture have to be re-examined at that point rather than inherited from this document.

**Recovery and what counts as acceptance.** The in-page Relaunch button carries session replacement correctly and is the supported way to restart. A whole-page F5 is different, and it is not globally `PENDING`: the reloaded page loses its identity and shows `PENDING`, while the *surviving* page keeps its old `MATCH` against a session that no longer exists, indefinitely. Recovery is reloading or relaunching both windows. Adopting a partner on a timeout would paper over this and reopen third-page takeover, so it is deliberately absent.

Acceptance therefore requires **both live windows displaying MATCH simultaneously after the same shared exit**. One window's retained `MATCH` is never sufficient evidence, because it may be describing a peer that is already gone.

## Wire format

The room router is a dumb relay keyed on node ids, so the envelope names both endpoints. Node ids are u32 little-endian in both directions:

```
engine -> router:  [to u32][from u32][chocolate packet...]
router -> engine:  [from u32][chocolate packet...]
```

The room host is node id 1. Ids 0 and 1 are reserved; 0 because a frame addressed to it is the host claiming the room, which resets any session already there. This matches cloudflare/doom-workers `router/index.mjs` at commit `22d8665f75017c4e1971d7e93567237645916ba1`.

## Mod loading

Mods are runtime data, and the loaded set is part of what peers must agree on. The agreement tuple is `(name, sha256, load kind, effective embedded-DEHACKED behaviour)` per entry, plus the order. Filenames and hashes alone are not sufficient: an embedded `DEHACKED` lump changes simulation behaviour only when it is enabled, so two peers holding identical bytes still desync if one enables it and the other does not.

Load kinds are Chocolate's: `-file` for ordinary loading, `-merge` for the deutex-style merge of sprite, flat and patch namespaces.

`-dehlump` is a single global switch. The engine loads every `DEHACKED` lump from every loaded WAD or none at all; there is no per-file form. So the honest per-entry fact is whether a file *carries* such a lump, which `mod_order.js` reads from the WAD directory rather than asking the user, and the effective behaviour is that fact combined with the global switch. Exposing a per-entry toggle would let two configurations differ in the UI and in the fingerprint while producing an identical command line and an identical simulation.

Standalone `.deh` and `.bex` patches are rejected. Nothing emits `-deh`, so accepting them would put a file in the fingerprint that the engine never reads.

Every file is written into the Emscripten FS by name, so the loader rejects duplicate filenames and any PWAD named `doom1.wad`. Either would silently overwrite, leaving the displayed tuple describing bytes that are not the bytes loaded.

The IWAD is pinned, not merely named: 4196020 bytes and sha256 `1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771` (sha1 `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d`). This fork is shareware-only by design, so the loader verifies the bytes and refuses to launch otherwise.

Verifying the bytes is not sufficient on its own, because it does not settle which file the engine opens. `d_iwad.c`'s auto-detect walks its table in order, and `doom2.wad`, `plutonia.wad`, `tnt.wad` and `doom.wad` all precede `doom1.wad`, matched case-insensitively in the same directory uploads are mounted into. A PWAD carrying one of those names would boot as the IWAD while the page displayed the verified `doom1.wad` tuple. The launch argv therefore names the pin explicitly with `-iwad /doom1.wad`; `D_FindWADByName` resolves an absolute path before consulting any search directory, so auto-detect never runs.

The fingerprint is JSON rather than delimiter-joined fields. A filename may legitimately contain `:` and `|`, so concatenation was ambiguous: a single entry whose name embedded the separators produced exactly the string two ordinary entries produced. That collision was captured, not theorised, and the regression test carries the exact case.

**Effective order is not command-line order.** `W_ParseCommandLine` processes every `-merge` input before every `-file` input, regardless of how the groups appear on the command line, preserving order only within each group. Upstream says so directly: "Merged PWADs are loaded first, because they are supposed to be modified IWADs." So the canonical effective order is all merges in their declared relative order, then all files in theirs. `mod_order.js` implements that, the loader normalizes to it rather than displaying an order the engine will not honour, and the fingerprint peers compare is built from the effective order. A requested `file -> merge -> file` sequence is not achievable and is normalized, with the reason reported.

## Prerequisites

| tool | version used | source |
|------------|--------------|-------------------------------------------|
| emscripten | 6.0.3 | emsdk at `~/dev/emsdk` |
| node | 22.16.0 | bundled in emsdk, used by emcc internally |

Install the pinned version explicitly. `latest` is a moving alias and will not reproduce the hashes below.

```sh
git clone https://github.com/emscripten-core/emsdk ~/dev/emsdk
~/dev/emsdk/emsdk install 6.0.3 && ~/dev/emsdk/emsdk activate 6.0.3
```

## Browser build

Run from `engine/`. Outputs land in `engine/out/`, which is a build directory and is never committed.

```sh
source ~/dev/emsdk/emsdk_env.sh
mkdir -p out
emcc -Oz \
  '-Wno-#warnings' -Wno-macro-redefined -Wno-switch \
  -Igifenc -Iopl -Isdl_mixer -Isrc -Isrc/doom \
  $(find gifenc opl sdl_mixer src -name '*.c' -not -name 'net_sdl.c' | LC_ALL=C sort) \
  -s WASM=1 -s USE_SDL=2 -s USE_LIBPNG=1 \
  -s ALLOW_MEMORY_GROWTH=1 -s NO_EXIT_RUNTIME=1 \
  -s EXPORTED_RUNTIME_METHODS=FS,UTF8ToString \
  -s MODULARIZE=1 -s ASSERTIONS=0 \
  -s ASYNCIFY -s ASYNCIFY_STACK_SIZE=32768 \
  -lwebsocket.js \
  -o out/doom.js
```

Three parts of that line are load-bearing:

- `-not -name net_sdl.c` keeps the UDP transport out of the browser build. It needs SDL_net, which emscripten does not provide, and `net_transport.h` selects the WebSocket module here anyway. There is no native build to mirror this: see "Native build" below.
- `-lwebsocket.js` links emscripten's WebSocket implementation. Without it the transport's symbols are undefined at link time.
- `-s ASYNCIFY` lets `I_Sleep` yield to the browser event loop. Without it the network waits cannot make progress and every connect times out. It costs about 41% in wasm size (1187974 to 1689618 bytes), which is the price of the connect path working at all. `ASYNCIFY_ONLY` could narrow the instrumentation later; it has not been tuned.

`LC_ALL=C sort` is also load-bearing. Bare `find` emits readdir order, which varies by filesystem and by the order files were created, and emcc assigns wasm function indices and `EM_ASM` string addresses in command-line order. Without the sort the same source tree produces a different `doom.wasm` on every machine. With it, the build is bit-reproducible.

The flag set is otherwise upstream's `CMakeLists.txt` Release configuration, minus the two spellings emscripten has since removed (`EXTRA_EXPORTED_RUNTIME_METHODS`, `--no-heap-copy`). The single `emcc` invocation is used instead of upstream's CMake path because it needs no host `cmake` and pins the flags where they can be read.

### The CMake route

`CMakeLists.txt` builds the same thing and is kept in step with the recipe above. It restricts its globs to the four source roots so `test/` (whose translation units carry their own `main`) is never linked into the engine, sorts the source list for the same determinism reason, drops `src/net_sdl.c` to match what `net_transport.h` selects, and pins `C_STANDARD 99` so a toolchain defaulting to C23 cannot reject this tree's legacy `doomtype.h` enum member named `false`.

```sh
source ~/dev/emsdk/emsdk_env.sh
emcmake cmake -S engine -B build
cmake --build build
```

**It is an Emscripten-only entry point and refuses anything else.** A non-Emscripten toolchain fails at configure time with a deliberate diagnostic, before CMake compiles even its compiler-identification program and long before anything probes for SDL. That refusal is a statement about the source, not about the host: see "Native build" below.

Executed and compared. Configure and a full link complete with emsdk 6.0.3, and the result is deterministic: two fresh configure-and-build cycles produce an identical `doom.wasm`. Against the single-`emcc` reference from the same clean archive:

| artifact | reference | CMake |
| --- | --- | --- |
| `doom.js` | 188756 bytes | byte-identical |
| `doom.wasm` | 1689618 bytes | 1689704 bytes, +86 |

The `doom.js` files are the same bytes. The `doom.wasm` files are the same program with a different layout, and the difference is accounted for rather than assumed: the type, import, function, table, memory, global, export and element sections are identical in size, both modules declare 1896 functions and carry 1896 code bodies and 2160 data segments, the memory declarations are identical, and neither contains a string the other lacks, so no build path leaks in. The whole delta is `code` +83 and `data` +3, and it starts at a data-segment header whose `i32.const` offset differs by four. CMake compiles each translation unit to its own object and `wasm-ld` merges them at object granularity, while the recipe hands every source to one `emcc` invocation that packs the data more tightly; the shifted segment changes the magnitude of address constants embedded in the code, which changes their LEB128 encoding widths. The pinned hashes therefore stay the recipe's, which reproduces them exactly.

### Expected output

Reproduced from this tree with emscripten 6.0.3. These are the artifacts the recipe is expected to produce, not artifacts published anywhere.

| file | bytes | sha256 |
|-----------|---------|------------------------------------------------------------------|
| doom.js | 188756 | b375a1eadb9c8cb5c90c7e5231b360792e52e49c5c9d5ac1750692e1a2ae56a5 |
| doom.wasm | 1689618 | a3b8ca1ef3e7f0a4db88096d0cd02be1534ac28cfe68da44146a58b43f66aee4 |

Hashes are pinned to emscripten 6.0.3. A toolchain bump changes them; re-record rather than assume drift is a defect.

## Native build

**There is no native build of this fork, and the missing piece is the source, not the toolchain.**

An earlier version of this document said the source and transport selection were wired and only the toolchain was absent. That was wrong, and the correction matters because it changes what M2 owes. With SDL2 2.32.10 and SDL2_net 2.2.0 present, a native configure gets past both packages and then dies partway through the build:

```
engine/src/doom/d_main.c:81:10: fatal error: emscripten.h: No such file or directory
```

That is not an isolated include. Fifteen translation units include `<emscripten.h>` unconditionally, only `i_timer.c` guards it. There are twenty-one `EM_ASM` sites spread across video, audio, input, joystick, GIF capture, ENDOOM and the game layer, with nine in `i_webmusic.c` alone. `d_main.c` inverts the game loop through `emscripten_set_main_loop` and `emscripten_cancel_main_loop`, which is an architectural difference rather than a flag. Selecting `net_sdl.c` instead of `net_websockets.c` changes none of it.

So `CMakeLists.txt` refuses a non-Emscripten toolchain at configure time rather than advertising a native target it cannot deliver. The refusal is checked before `project()`, so it fires before any C is compiled and before SDL is probed at all: a host with every native dependency installed still gets the same diagnostic, because the dependency that is missing is in this tree.

`net_sdl.c` remains in the tree as restored, reviewed source. It is deliberately excluded from every build here and is **not presently buildable**. Native UDP interop, which is what makes native chocolate-doom clients able to share a room, is an M2 exit requirement, and reaching it means porting the browser-only seams above, not just flipping the transport selection.

## Running it

The build is `MODULARIZE=1`, so `doom.js` exports an async factory rather than populating a global `Module`. A host must call the factory with `{ canvas, arguments, preRun }` and place the WADs in the Emscripten FS from `preRun`. `Module["FS"]` is attached at factory-eval time and `run()` awaits run dependencies before `callMain`, so anything written there is on disk before `D_DoomMain` looks for it.

Two pages in this directory do that:

- `doom.html`, the contributor loader. Picks the IWAD and the ordered PWAD set with their hashes, load kinds and DEHACKED policy, chooses single player / host / join and the room URL, shows the resulting argv and the mod fingerprint, and launches.
- `doom-frame.html`, the engine host. The loader creates one per launch and destroys it to relaunch. Removing the iframe takes the instance, its main loop, its audio graph and its FS with it. Doom loads its WAD set once during `D_DoomMain` and has no supported path to unload one from a live session, so changing the mod set restarts the instance. That is a real restart, not a hot swap, and the loader does not claim otherwise.

Serve them over `http://localhost`. Both `crypto.subtle`, used for the content hashes, and ES module imports require a secure context, so `file://` will not work.

### IWAD

Shareware `DOOM1.WAD` v1.9: 4196020 bytes, sha1 `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d`. Source: `https://www.doomworld.com/3ddownloads/ports/shareware_doom_iwad.zip`. The idgames `doom19s.zip` is the 1995 installer and its WAD is DEICE-packed, so it is not usable directly.

## Tests

```sh
engine/test/run.sh
```

Builds and runs, with a plain host compiler and node, no emscripten and no WADs:

- `test/test_net_ws_frame.c` over `src/net_ws_frame.c`: envelope round-trip and byte order, rejection of short frames (the underflow case), rejection of oversize frames, the bare-envelope case, and the receive ring's FIFO order, overflow refusal, drop counting and freeing.
- `test/test_net_loop.c` over `src/net_loop.c`: the loopback ring's overflow path, against a counting allocator, so a packet the transport takes ownership of and refuses is proven freed rather than leaked.
- `test/test_mod_order.mjs` over `mod_order.js`: the canonical effective order including the `file -> merge -> file` trap, argv construction, the DEHACKED lump probe against synthesized WAD directories, the duplicate and reserved-name rules, the IWAD pin, and that the fingerprint changes when order, load kind or effective DEHACKED behaviour changes.
- `test/test_democanary.c` over `src/d_democanary.c`: two peers differing only in the consoleplayer byte and in how much they recorded after the anchor digest equal; every byte inside the anchor changes the digest; a short anchor, an empty demo and an undersized output buffer are refused; and the digest is pinned against an independent SHA-256 implementation.
- `test/test_statecanary.c` over `src/doom/d_statecanary.c`: a synthesized state of two players, three sectors, four mobjs with a target link and one door special. Its exact serialized length and SHA-256 are pinned as checked-in literals, computed outside the program over the serializer's bytes rather than by the digest function, because a baseline the code under test generates moves whenever the code moves and would let a field reorder update both sides and stay green. Beyond that: 27 simulation values each change the digest, 16 per-peer presentation values each leave it alone, a null reference and an unresolvable one are distinguished, an inactive player's stale storage and unsafe `mo` are never read, two peers differing only in presentation digest identically, the same state at different allocations digests identically, and the reported byte count is the real serialized length. Every special class is exercised separately, each mutating a scalar only that class writes, so a branch cannot pass by merely executing; the specials count field is read back from the stream, because the serializer walks its class list twice and nothing else would notice the two copies drifting apart. An empty world, an in-game player with no body, and a state too large for the scratch buffer are named cases: the oversize one asserts against the report's real output that a refusal emits neither a `sha256=` token nor a byte count, and leaves the caller's buffers untouched.
- `test/test_canary_verdict.mjs` and `test/test_verdict_system.mjs` over `canary_verdict.js`: line parsing, the arm and report state machine, and whole-conversation simulation of two and three pages, including both finish orders across several exits, late and duplicate reports, and convergence without a publish storm. The refused-digest and known-boundary cases are named there too: identical refusals never read as a match, and the forged same-origin replacement and the asymmetric one-window reload are pinned as the unauthenticated behaviour described under "Automatic verdict" rather than left to be rediscovered.
- An ownership check over the transport modules: `NET_RecvPacket` in `net_io.c` takes the single address reference that consumers release, so no transport module may reference in its own `RecvPacket`. Getting this wrong leaks one reference per received packet and the address table grows without bound, which nothing else would surface.

Each guard was checked by removing it and confirming the suite fails, so the tests are known to be capable of failing rather than merely green.

## Verification

The browser build is verified by running it. Serve `out/doom.js`, `out/doom.wasm`, a loader page and `doom1.wad` over HTTP, then load the page in headless Chrome with software GL (`--enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader`).

Single player is healthy when the console carries the `DOOM Shareware` banner, `W_Init` reports ` adding doom1.wad`, startup runs through `I_Init`, `R_Init`, `P_Init`, `S_Init`, `D_CheckNetGame`, `HU_Init`, `ST_Init` with no `I_Error`, and a screenshot after roughly 15 seconds shows E1M1 rendering behind a live status bar.

The network path is healthy when two instances, one launched with `-privateserver -wss <url> -nodes 2` and one with `-connect 1 -wss <url>`, both reach `D_CheckNetGame` reporting `player 1 of 2` and `player 2 of 2`. Running them in separate browser processes avoids overloading a single software-GL renderer.

**Start the host first and let it connect before joining.** Claiming a room is a reset: the frame the host sends to node 0 disconnects every other connection already in that room, in this engine's router and in `doomd` alike. A client that connects during the host's startup is therefore dropped, and since a closed socket is terminal here, it does not recover. Observed against `doomd`: the join reported `closed (clean=1 code=1005)` and then `Failed to connect to ws node 1`, purely because its socket opened before the host had claimed the room. With the host connected first, the same pair reaches `player 1 of 2` and `player 2 of 2`.
