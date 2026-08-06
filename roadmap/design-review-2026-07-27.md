# DOOM net-revival: standalone repo + generalised Rust crate — design for review

Status: ACCEPTED 2026-07-27 by @@Alex review; finalized as `../docs/design.md`. This file is the review record (the host's inline @@Alex answers) kept as the decision audit trail. The protocol/transport analysis in `doom-multiplayer.md` still stands and is the grounding for the design.

## The ask

Revive DOOM multiplayer in a new, separate repo: a fork of the engine with the deleted netcode restored, plus a generalised Rust crate family that (a) lets any Rust program embed DOOM with the protocol wired, (b) is the multiplayer server (the server role is native Rust, not an in-WASM host tab), and (c) can later plug into chan as a library, with the SPA integration specced separately back in chan's v0.80.0 item.

## Verified ground (checked 2026-07-27, not assumed)

- The shipped `doom.wasm` (bundle copy at `dev/doom/dist/`) is built from rojo2/wasm-doom (Crispy Doom lineage, GPL-2.0) with ALL network code deleted; `d_loop.c` is gutted. Source: roadmap grounding + `crates/chan-server/resources/doom/README.md` (doom-overlay branch). The tree cannot emit or consume a single packet today.
- rojo2/wasm-doom: live, GPL-2.0, dormant since 2019 (fork of lazarv/wasm-doom).
- cloudflare/doom-wasm: live, GPL-2.0, pushed as recently as 2026-04. It is Chocolate Doom + restored netcode + one added file `src/net_websockets.c` (chocolate's net layer is a pluggable `net_module_t` vtable, UDP swapped for emscripten WebSockets). This is the proven precedent and the main de-risking anchor.
- chan is Apache-2.0 (`LICENSE`, root `Cargo.toml`). The doom engine lineage is GPL-2.0. This sets the licensing boundary below — it is the single hardest constraint on "plug into chan as a library."
- The chocolate dedicated server does NOT simulate the game. It is a relay: handshake, player/address table, GAMESTART broadcast, ticcmd window fan-out at 35 Hz, resend and timeout bookkeeping. That is the whole role Rust has to reproduce — no game logic.
- `cs open <file>` opens a file in the running chan window (verified via `cs --help`).

## Licensing boundary (drives the whole shape)

- `engine/` (the fork): GPL-2.0, public, corresponding source shipped. Same posture as today's `doom-v1` bundle (LICENSES.txt + build recipe).
- Rust crates: Apache-2.0, written from scratch. The wire protocol is reimplemented for interoperability (formats/constants are not creative expression); no C code is ported line-by-line. This is what keeps the crate linkable into Apache-2.0 chan-server.
- The engine WASM is always *data* to the Rust side: served/downloaded at runtime, never `include_bytes!`'d, never FFI-linked. The GPL boundary is a process/sandbox boundary (browser iframe, wasmtime instance), exactly like today's bundle model.

TODO: Confirm license posture: Apache-2.0 clean-room crates + GPL engine as runtime data (recommended; enables chan library-linking) vs GPL-2.0 everywhere (simpler, but chan integration becomes sidecar-process, not library).

@@Alex: Confirmed Apache-2.0 crates + GPL engine as runtime data

## Repo shape

```
fiorix/<repo>                       # name TODO
├── engine/                         # fork of the doom engine (GPL-2.0), revival lands here
│   └── docs/                       # build recipe + provenance (mirrors chan's bundle README)
├── crates/
│   ├── doom-proto/                 # wire codec: packets, ticcmd, GAMESTART — pure data, no I/O
│   ├── doom-server/                # rooms + server role; sans-IO core + tokio/axum WS binding + CLI
│   └── doom-embed/                 # (phase 3, optional) wasmtime host for the engine WASM
├── docs/
│   ├── protocol.md                 # the wire protocol, written down as an owned spec
│   └── verification.md             # capture-replay fixtures + how they were recorded
└── examples/                       # standalone server, two-client loopback demo
```

Nothing in `crates/` may depend on chan. chan is the first consumer, not a dependency.

## Crate shape validation

**`doom-proto`** — encode/decode for the chocolate wire format: packet header and `NET_PACKET_TYPE_*` family, connect/accept handshake, GAMESTART settings, full-ticcmd fan-out, keepalive/disconnect. Explicit little-endian reads/writes, no transmute, no C-layout structs. No I/O, no async, no allocation-heavy types. This is the piece that must be byte-exact, so it is the piece that is easiest to test: golden packets captured from a real chocolate-doom ↔ chocolate-server session, plus fuzzing. (~Small: the full packet zoo is a few dozen variants over a ~10-field header; a ticcmd is ~8 bytes.)

**`doom-server`** — the core deliverable. Two layers:

1. Room layer (phase 1): `Registry` of rooms keyed by name; join/leave; per-connection bounded outbox with a slow-consumer = disconnect policy (at 35 Hz lockstep you cannot buffer a stalled peer; this cap is designed in from day one because the roadmap flagged unbounded outboxes as the chan-side risk). Protocol-agnostic envelope relay — this is the Cloudflare Durable Object role, but as a real library.
2. Server-role state machine (phase 2): the native Rust NET_SV — handshake, player table, GAMESTART broadcast with agreed settings, ticcmd window fan-out (BACKUPTICS window, resend on sequence gaps, keepalive/timeout, disconnect). Still zero game simulation.

Shape discipline: the core is sans-IO — `handle(&mut self, from: PlayerId, bytes: &[u8], now: Instant) -> Vec<Action>` with `Action::{Send, Disconnect, ...}`. The tokio/axum WebSocket binding and the `doomd serve` CLI are thin wrappers. Payoff: deterministic tests with no runtime, and chan later drives the same core from its own route handler with its own auth in front (bearer check before upgrade), the same way `routes/scene.rs` drives its registry. Binary WebSocket frames carry packets (the 8-byte Cloudflare to/from envelope rides along verbatim in phase 1; re-evaluate once Rust owns the protocol).

**`doom-embed`** (phase 3, optional) — wasmtime host that runs the engine WASM natively: host provides framebuffer/audio callbacks and input injection, and the engine's net module calls host imports mapped onto a `Transport`. Two payoffs: (a) this is what makes the crate genuinely "embed DOOM in any Rust program" beyond the browser, and (b) headless bot clients — the only sane way to run the 4-player soak test without four humans. Kept as a separate crate so wasmtime's weight is opt-in (chan-server would never link it).

**`engine/`** — the fork. Revival patch set, per the roadmap grounding:

1. Restore `src/net_*.c`, `i_net.c`, `d_net.c` from the matching crispy/chocolate vintage.
2. Port `net_websockets.c` (the Cloudflare `net_module_t`).
3. Un-stub `d_loop.c` (`D_StartNetGame`, `GetLowTic`); keep the surviving `D_ReceiveTic`/`TryRunTics`/BACKUPTICS machinery as the injection point.
4. Keep the documented `v_trans.h` build fix; keep the existing emscripten build recipe, republished as a `doom-v2` bundle with sha-pinned provenance.

TODO: Engine fork base: rojo2/wasm-doom (crispy lineage, matches the shipped single-player engine, netcode restoration is on us) vs cloudflare/doom-wasm (restoration already done, but switches the engine lineage crispy→chocolate and re-validates the whole single-player build) vs fresh crispy-doom upstream + emscripten port (freshest, most work).

@@Alex: can we take the best of both and merge the lineage into something that others will be able to build upon? We have super powers with AI now, we can keep fidelity and validate funcionality first, then we can consider hardening later. One of the things I want native support from the get-go is managing and loading/unloading of mods, we should pick 2-3 from https://www.moddb.com/games/doom/mods and validate as we go.

TODO: Repo name and home (e.g. github.com/fiorix/...), and whether it is public from creation (GPL effectively requires it once the bundle ships). 

@@Alex: this will be fiorix/chan-ext-doom

## Protocol strategy

Options considered:

- **A. Keep the chocolate wire protocol; Rust reimplements the server role.** Engine diff stays minimal (restored client code + one transport file — the proven Cloudflare shape). The Rust server is the one real chunk of new code, and it is provable: replay recorded chocolate-server sessions against it byte-for-byte. Desktop interop stays possible.
- B. New owned protocol. Rejected for v1: re-solves resend/drift/lobby problems chocolate already solved, doubles the engine-side diff, loses interop. (The roadmap independently rejected the equivalent "pure JS lockstep" path.)
- C. Dumb envelope router with an in-WASM host tab forever. Rejected as the end state — the ask is that Rust is the server, and a host tab is a player with host advantage and migration pain. Retained as the phase-1 spike (see below).
- D. Server role = the engine's own net_server.c compiled as a headless WASM, driven by doom-embed inside the server process. Byte-exact parity with zero reimplementation, but it drags a C event loop in a wasmtime box into every server host (including chan), and makes the server behavior opaque to Rust tests. Fallback if option A's fidelity proves elusive.

Recommendation: **A, reached in two phases, with the phase-1 spike from C**. The room layer built for the spike is exactly the substrate the Rust server role runs on — no throwaway work.

Side benefit of A with a persistent Rust server: the roadmap's hardest open question ("host migration on leader handover") dissolves — the server outlives every player, and chan's window-session leader degrades to pure lobby authority (who presses start).

TODO: Confirm option A (chocolate wire, Rust server role) over D (wasm-embedded net_server) — recommended A; D stays documented as fallback.

@@Alex: Confirmed A

## Phasing

- **M0 — scaffold.** Repo, workspace, engine fork builds the current single-player WASM with the documented recipe (CI caches the emscripten toolchain). Exit: bit-identical-ish bundle vs the pinned recipe; `cargo test` green on empty crates.
- **M1 — transport spike.** Engine revival patch set (netcode + net_websockets.c + d_loop.c); doom-server room layer only; one browser tab runs the in-WASM host. Exit: two browser windows on the same machine play co-op start-to-exit without desync (demo-recorded end states identical).
- **M2 — Rust server role.** doom-proto codec + golden fixtures; server-role state machine behind the same room API; host tab removed. Exit: same two-window co-op against the pure Rust server; capture-replay parity vs real chocolate-server on recorded sessions.
- **M3 — hardening.** 4-player 10-minute soak (doom-embed bots if that phase lands, otherwise four real tabs); stalled-client outbox-cap test; `docs/protocol.md` complete; `doom-v2` bundle published with recipe + LICENSES.txt.
- **M4 — chan integration (separate spec, lives in chan's repo).** chan-server links doom-server as a library, mounts the room core on a new `/api/doom/ws` route modeled on `routes/scene.rs` minus disk persistence; lobby UI in the DoomOverlay driven by window-session roster; the engine iframe opens its own WebSocket to the route (the iframe isolation boundary survives — SPA↔iframe traffic is lobby postMessage only). This doc defines only the seam: the crate's sans-IO core is the integration surface.

## Verification (adversarial, per plan)

- Golden capture-replay: build upstream chocolate-doom + chocolate-server natively, record real sessions (handshake, GAMESTART, mid-game ticcmd streams, resend-under-loss, disconnect), replay byte-for-byte against doom-proto and later doom-server. The Rust server is wrong until proven otherwise against these fixtures.
- Codec fuzzing (proptest at minimum; cargo-fuzz target left in-tree).
- Desync canary: identical demo-recorded end state across clients over a full E1M1 clear, on both phase-1 and phase-2 servers.
- Slow-client test: stall one connection, assert the outbox cap trips and the peer is disconnected with bounded memory — not just "green test," measured RSS.
- 4-player soak, 10+ minutes, including over the chan gateway tunnel path at M4, not just loopback.

## Non-goals

- No server-side game simulation, ever. The server relays inputs; the engines simulate.
- No changes to chan's doc/scene sessions, `/ws` bus, or window-session registry.
- No game-logic changes in the engine beyond the netcode revival.
- No WS↔UDP relay to a native chocolate-server and no NAT traversal in v1.
- No crates.io publishing until the shape survives M2 (git-dep is enough for chan).

TODO: Scope call: is doom-embed (native wasmtime embedding + bot clients) in v1 scope, or deferred until after chan integration?

@@Alex: in scope for v1 yes

TODO: Desktop chocolate-doom interop (native clients joining Rust-hosted rooms over UDP): supported goal or explicit non-goal?

@@Alex: yes

TODO: Game modes for v1: co-op only, or deathmatch flags exposed at once?

@@Alex: co-op and deathmatch flags

TODO: Player ceiling: 4 (Doom MAXPLAYERS) or design the room layer for 8 (NET_MAXPLAYERS) from the start?

@@Alex: 8

TODO: IWAD strategy for the generalised crate: shareware-only (chan's pinned doom1.wad) or pluggable-IWAD/freedoom support?

@@Alex: shareware-only? so long we can use mods

TODO: Keep the M1 in-WASM-host spike, or skip straight to the Rust server role? (Recommended: keep — it de-risks the engine revival independently of the protocol port.)

@@Alex: keep

TODO: Where the M4 chan-integration spec lives and when: back in `team/roadmap/v0.80.0/doom-multiplayer.md` as a revision, or a new chan roadmap item?

@@Alex: new item after we complete this

## References

- `../chan-doom/team/roadmap/v0.80.0/doom-multiplayer.md` — grounding + acceptance checks this design inherits.
- `../chan-doom/team/roadmap/v0.80.0/doom-overlay-remote-protocol.md` — the sibling item (spectate/remote protocol); untouched by this design, revisits once multiplayer lands.
- `crates/chan-server/resources/doom/README.md` (doom-overlay branch) — bundle build recipe + provenance model to mirror.
- https://github.com/cloudflare/doom-wasm (GPL-2.0) + https://blog.cloudflare.com/doom-multiplayer-workers/ — the precedent.
- https://github.com/rojo2/wasm-doom (GPL-2.0) — current engine upstream.
