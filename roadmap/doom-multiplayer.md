# DOOM multiplayer over chan's collaboration plane

Status: SUPERSEDED 2026-07-27 by `../docs/design.md` (the accepted spec after host review). Moved from chan's `team/roadmap/v0.80.0/` to fiorix/chan-ext-doom; doom work is tracked in this repo, not in chan's roadmap. Kept as the grounding analysis that produced the design (verified 2026-07-26 against `origin/doom-overlay` tip d776cbcb and a clone of rojo2/wasm-doom @ 619e697). Do not start from the shipped `doom.wasm`: it contains zero network code (see below).

## What

Multiplayer DOOM (co-op first, deathmatch rides the same path) between participants of a chan workspace, riding chan's real-time collaboration transport the same way collaborative text editing and Excalidraw scenes do: one WebSocket route per room, server fan-out, presence/leadership from the window session.

## What is already known (grounding, verified 2026-07-26)

Engine side (rojo2/wasm-doom, served by `crates/chan-server/src/routes/doom.rs`):

- The tree is Crispy Doom (a Chocolate Doom fork), so its native protocol is chocolate-doom's client-server UDP lockstep: `net_packet_t` / `NET_PACKET_TYPE_*` framing, `net_full_ticcmd_t` input fan-out at 35 Hz, separate `chocolate-server` process, max 4 players in Doom (8 at the net layer, `NET_MAXPLAYERS` in the orphaned `src/net_defs.h`). Vanilla's P2P `doomcom_t` protocol does not exist in this lineage.
- ALL network code was deleted from the tree: no `net_client.c` / `net_server.c` / `net_sdl.c` / `d_net.c` / `i_net.c`, no SDL_net. `src/d_loop.c` is gutted: `D_StartNetGame()` forces `num_players = 1`, `GetLowTic()` returns `maketic` (never waits for peers). `D_ReceiveTic()` and the `TryRunTics()` / `BACKUPTICS` lockstep machinery survive intact as the injection point.
- Conclusion: "use Doom's network protocol as-is" is impossible; the shipped WASM cannot emit or consume a single packet, and browsers have no UDP regardless (emscripten sockets emulate TCP-over-WebSocket only).
- The proven precedent is Cloudflare's doom-wasm (May 2021, GPL-2.0): they restored chocolate netcode and added ONE file, `src/net_websockets.c` — chocolate's net layer is a pluggable `net_module_t` vtable, so UDP was swapped for `<emscripten/websocket.h>` with an 8-byte to/from envelope per packet. One browser tab runs the in-WASM server (`NET_SV_*` logic); a dumb WebSocket envelope router forwards packets between players. Livedemo supported 4 players. (cloudflare/doom-wasm + blog.cloudflare.com/doom-multiplayer-workers/)

chan side (collaboration plane, verified against chan-server sources):

- `cs session` is NOT a document-collab session: `cs` is the CLI alias for the chan binary (`crates/chan-server/src/routes/cs_link.rs:6`); `cs session` drives the leader/followers WINDOW session (presence + leadership, `crates/chan-library/src/session_presence.rs`, joined via `/ws?w=<window_id>`). Collab sessions proper are `DocSession` (text, OT-lite update log) and `SceneSession` (Excalidraw, LWW element merge), each a WebSocket route keyed by file path.
- The reusable pattern is `crates/chan-server/src/routes/scene.rs` + `scene_sessions/`: registry, per-attach unbounded mpsc outbox, fan-out enqueued under the state lock = strict per-socket FIFO, lossless, ordered. Exactly what lockstep input relay wants.
- Roster + leadership come free from the window session: `session_roster` frames label participants (same as collab cursors), and the single leader slot (with `handover`/`takeover`) maps to the game host.
- What is missing: there is no client-originated generic broadcast (unknown frame types on `/ws` are dropped, `routes/ws.rs:296-320`), and the doc/scene registries couple fan-out to disk persistence (flusher/CAS/recovery), which a game must NOT inherit.

## Design (the wiring)

1. New route `GET /api/doom/ws?room=&w=`, modeled directly on `routes/scene.rs`: a `GameRegistry` / `GameSession` keyed by room name, per-attach outboxes, under-lock fan-out. No disk flusher, no CAS, no recovery records.
2. The chan server is a protocol-agnostic envelope router (the Cloudflare Durable Object role): it forwards opaque envelopes addressed by player id and never parses Doom protocol. Payloads are tiny (a `ticcmd_t` is ~8 bytes; 35 Hz x 4 players is trivial bandwidth), so base64-in-JSON or a small JSON struct is fine.
3. The window-session leader is the host tab running the in-WASM `NET_SV_*` server; leader handover = host migration. `session_roster` frames drive the lobby UI in the DoomOverlay.
4. Engine side: rebuild `doom.wasm` from a public chan fork of wasm-doom with (a) netcode restored from the matching crispy/chocolate vintage, (b) `net_websockets.c` ported, (c) `d_loop.c` un-stubbed. Republish as a `doom-v2` release bundle; the download flow in `routes/doom.rs` is already idempotent and URL-pinned, so the swap is one line per file in `DOOM_FILES`.
5. Lobby/start agreement (skill/episode/map/consoleplayer assignment, chocolate's GAMESTART semantics) rides the same room channel before the host starts emitting ticks.

## Constraints (verified; design around these)

- JSON text frames only on all collab sockets; binary gets a protocol error + close 1008 (`routes/doc.rs:260-270`).
- Per-socket FIFO is ordered and reliable (TCP + under-lock enqueue). Head-of-line blocking is the only latency risk; chocolate's `ticdup` / `extratics` knobs exist for exactly this.
- Outboxes are unbounded: fine at editor rates, needs a cap/policy at input-flood rates (a stalled game tab would otherwise grow memory without bound).
- No per-participant ACL: bearer token = full access; anyone in the tenant can join any room.
- Determinism requirements: identical IWAD (guaranteed, chan ships one pinned `doom1.wad`), identical WASM build on all players (guaranteed by the pinned release bundle), agreed game settings at start. RNG is the vanilla 256-entry table, so demo-grade sync holds.
- GPL-2.0: the modified engine fork must stay public with corresponding source (serving the .wasm is conveyance). The existing bundle model (GitHub release + build recipe in `crates/chan-server/resources/doom/README.md`) already accommodates this.

## Non-goals / boundaries

- No WS<->UDP relay to a native chocolate-server, and no interop with desktop chocolate-doom players (strictly more moving parts for no chan-internal benefit; revisit only if desktop interop becomes a hard requirement).
- No changes to doc/scene sessions, the `/ws` bus, or the window-session registry; the game gets its own route.
- No server-side game logic: chan-server routes envelopes, it never simulates.
- The alternative "pure JS lockstep" path (un-stub `d_loop.c`, export ticcmds via EM_ASM, inject via `D_ReceiveTic()`, ~100-200 lines of C) was considered and rejected as the primary path: it reimplements resend/drift/lobby logic that chocolate already solved, and browser `emscripten_set_main_loop` timing makes stall handling delicate. Acceptable only as a 2-player spike to de-risk the transport before committing to the netcode restoration.

## Rough size

Server route + registry (scene.rs minus disk): small, a focused day. Lobby UI in DoomOverlay + roster wiring: small-to-moderate. Engine fork (netcode restoration + net_websockets.c port + rebuild + doom-v2 bundle): the one moderate-to-large chunk, and the only real risk; the Cloudflare repo de-risks it substantially.

## Acceptance checks

- Two browser windows attached to the same chan server play a co-op game start-to-exit without desync (same IWAD/build guaranteed by the bundle; sync verified by identical demo-recorded end state or in-game consistency over a full E1M1 clear).
- Four concurrent players (doom's MAXPLAYERS) hold a stable game for 10+ minutes over the gateway tunnel path, not just loopback.
- Leader handover mid-game migrates the host (or cleanly ends the room with a surfaced error, if migration proves out of scope at spec time).
- A stalled/slow client cannot grow server memory without bound (outbox cap enforced + covered by a test).
- `doom-v2` bundle published with the same sha-pinned, recipe-documented provenance as `doom-v1`; GPL corresponding-source link shipped in LICENSES.txt.

## Open (decide at spec time, not now)

- Room naming/discovery UX (per-workspace single room vs named rooms; who can create).
- Whether host migration on handover is real (snapshot + resume) or a clean room reset.
- Co-op only for v1 of the item, or deathmatch flags exposed at once.
- Where the lobby lives: inside DoomOverlay vs the iframe loader page.
