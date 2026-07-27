# doomit — DOOM net-revival: standalone repo + generalised Rust crate

Status: ACCEPTED 2026-07-27 by @@Alex review; all open questions from the review draft are resolved in "Decisions" below. This is the spec of record for the doomit team. The reviewed draft (with inline answers, the audit trail) lives at `chan:dev/doom/multiplayer-crate-design.md`; the grounding roadmap item is `chan-doom:team/roadmap/v0.80.0/doom-multiplayer.md`, whose protocol/transport analysis still stands.

## The ask

Revive DOOM multiplayer in a standalone repo: a fork of the engine with the deleted netcode restored, plus a generalised Rust crate family that (a) lets any Rust program embed DOOM with the protocol wired, (b) is the multiplayer server (the server role is native Rust, not an in-WASM host tab), and (c) can later plug into chan as a library, with the SPA integration specced separately as a new chan roadmap item after this repo's work completes.

## Decisions (from @@Alex review, 2026-07-27)

1. License posture: Apache-2.0 crates + GPL-2.0 engine kept as runtime data. Confirmed.
2. Engine fork base: merged lineage — take the best of rojo2/wasm-doom (crispy engine, matches the shipped single-player build) and cloudflare/doom-wasm (restored chocolate netcode, `net_websockets.c`) and merge them into one clean tree that others can build upon. Fidelity and validated functionality first; hardening later. Native mod management (load/unload of PWADs) is a from-the-start requirement, validated against 2-3 mods picked from https://www.moddb.com/games/doom/mods.
3. Repo: `fiorix/doomit`, code at `/home/fiorix/dev/github.com/fiorix/doomit`.
4. Protocol: option A confirmed — keep the chocolate wire protocol; Rust reimplements the server role. Option D (wasm-embedded net_server) stays documented as the fallback.
5. doom-embed (wasmtime native embedding + bot clients): IN scope for v1.
6. Desktop interop: YES — native chocolate-doom clients joining Rust-hosted rooms over UDP is a supported goal.
7. Game modes: co-op AND deathmatch flags from the start.
8. Player ceiling: design the room layer for 8 (NET_MAXPLAYERS). Note Doom's in-game cap is 4 (MAXPLAYERS); the net layer and rooms size for 8, the engine enforces its own cap at game start.
9. IWAD: shareware-only (pinned doom1.wad sha1 5b2e249b9c5133ec987b3ea77596381dc0d6bc1d), provided mods (PWADs on top) work.
10. Keep the M1 in-WASM-host spike before the Rust server role.
11. chan integration spec: a NEW chan roadmap item, written after this repo's work completes.

## Verified ground (checked 2026-07-27, not assumed)

- The shipped chan `doom.wasm` is built from rojo2/wasm-doom (Crispy Doom lineage, GPL-2.0) with ALL network code deleted; `d_loop.c` is gutted (`D_StartNetGame` forces one player, `GetLowTic` never waits). The surviving `D_ReceiveTic`/`TryRunTics`/BACKUPTICS machinery is the injection point.
- rojo2/wasm-doom: live, GPL-2.0, dormant since 2019 (fork of lazarv/wasm-doom).
- cloudflare/doom-wasm: live, GPL-2.0, pushed as recently as 2026-04. Chocolate Doom + restored netcode + one added transport file `src/net_websockets.c` (chocolate's net layer is a pluggable `net_module_t` vtable). The proven precedent and main de-risking anchor.
- chan is Apache-2.0; the doom engine lineage is GPL-2.0. This sets the licensing boundary — the hardest constraint on "plug into chan as a library."
- The chocolate dedicated server does NOT simulate the game. It is a relay: handshake, player/address table, GAMESTART broadcast, ticcmd window fan-out at 35 Hz, resend and timeout bookkeeping. That is the whole role Rust reproduces — no game logic.
- emsdk is installed on this host at `~/dev/emsdk` (the chan bundle recipe uses it).

## Licensing boundary (drives the whole shape)

- `engine/` (the fork): GPL-2.0, public on publication, corresponding source shipped. Same posture as chan's `doom-v1` bundle (LICENSES.txt + build recipe).
- Rust crates: Apache-2.0, written from scratch. The wire protocol is reimplemented for interoperability (formats/constants are not creative expression); no C code is ported line-by-line. This keeps the crates linkable into Apache-2.0 chan-server.
- The engine WASM is always *data* to the Rust side: served/downloaded at runtime, never `include_bytes!`'d, never FFI-linked. The GPL boundary is a process/sandbox boundary (browser iframe, wasmtime instance).

## Repo shape

```
fiorix/doomit
├── engine/                         # merged-lineage doom fork (GPL-2.0); the revival lands here
│   └── docs/                       # build recipe + provenance (mirrors chan's bundle README)
├── crates/
│   ├── doom-proto/                 # wire codec: packets, ticcmd, GAMESTART — pure data, no I/O
│   ├── doom-server/                # rooms + server role; sans-IO core + WS/UDP bindings + CLI
│   └── doom-embed/                 # wasmtime host for the engine WASM (native clients, bots)
├── fixtures/                       # golden packet captures from real chocolate sessions
├── docs/
│   ├── protocol.md                 # the wire protocol, written down as an owned spec
│   └── verification.md             # capture-replay fixtures + how they were recorded
└── examples/                       # standalone server, two-client loopback demo
```

Nothing in `crates/` may depend on chan. chan is the first consumer, not a dependency.

## Crate shape

**`doom-proto`** — encode/decode for the chocolate wire format: packet header and `NET_PACKET_TYPE_*` family, connect/accept handshake, GAMESTART settings, full-ticcmd fan-out, keepalive/disconnect. Explicit little-endian reads/writes, no transmute, no C-layout structs. No I/O, no async. This is the piece that must be byte-exact, so it is the piece that is easiest to test: golden packets captured from real chocolate-doom ↔ chocolate-server sessions, plus fuzzing. (Small: the packet zoo is a few dozen variants; a ticcmd is ~8 bytes.)

**`doom-server`** — the core deliverable. Two layers:

1. Room layer (phase 1): `Registry` of rooms keyed by name; join/leave; per-connection bounded outbox with slow-consumer = disconnect (at 35 Hz lockstep you cannot buffer a stalled peer). Rooms size for NET_MAXPLAYERS = 8. Protocol-agnostic envelope relay — the Cloudflare Durable Object role, as a real library.
2. Server-role state machine (phase 2): the native Rust NET_SV — handshake, player table, GAMESTART broadcast with agreed settings (co-op and deathmatch flags), ticcmd window fan-out (BACKUPTICS window, resend on sequence gaps, keepalive/timeout, disconnect). Zero game simulation.

Shape discipline: the core is sans-IO — `handle(&mut self, from: PlayerId, bytes: &[u8], now: Instant) -> Vec<Action>` with `Action::{Send, Disconnect, ...}`. Transports are thin bindings: WebSocket (browsers; binary frames carrying packets with the 8-byte to/from envelope) AND UDP (native chocolate-doom clients — desktop interop is a supported goal, so the datagram envelope semantics stay first-class). A `doomd serve` CLI wraps both. Payoff of sans-IO: deterministic tests with no runtime, and chan later drives the same core from its own route handler with its own auth in front.

**`doom-embed`** (in v1 scope) — wasmtime host that runs the engine WASM natively: host provides framebuffer/audio callbacks and input injection, and the engine's net module calls host imports mapped onto a `Transport`. Payoffs: (a) "embed DOOM in any Rust program" beyond the browser, (b) headless bot clients — the only sane way to run the 8-player soak without eight humans. Separate crate so wasmtime's weight is opt-in (chan-server never links it).

**`engine/`** — the merged-lineage fork. Base: the crispy-doom tree that wasm-doom ships (continuity with the proven single-player build), with the cloudflare/doom-wasm netcode restoration merged in (chocolate lineage, adapted to the crispy vintage). Keep the tree clean, documented, and upstream-attributed so others can build on it. Revival patch set:

1. Restore `src/net_*.c`, `i_net.c`, `d_net.c` from the matching crispy/chocolate vintage, using cloudflare/doom-wasm as the reference implementation.
2. Port `net_websockets.c` (the Cloudflare `net_module_t`) for the browser build; keep UDP via SDL_net or an equivalent for native builds.
3. Un-stub `d_loop.c` (`D_StartNetGame`, `GetLowTic`).
4. Keep the documented `v_trans.h` build fix and the existing emscripten build recipe; republish as a `doom-v2` bundle with sha-pinned provenance.

**Mod support (from the start, native)** — the engine and its hosts manage PWADs as first-class runtime data alongside the pinned shareware IWAD: enumerate, load, and unload mods without rebuilding or hand-editing command lines. The browser loader page and doom-embed's host API both expose it. Validated as we go against 2-3 mods picked from moddb (must be compatible with the shareware IWAD; shortlist raised as a host survey before adoption). Mod files ship under the same download-on-demand, provenance-recorded model as the IWAD.

## Protocol strategy

- **A (chosen). Keep the chocolate wire protocol; Rust reimplements the server role.** Engine diff stays minimal (restored client code + transport files — the proven Cloudflare shape). The Rust server is the one real chunk of new code, and it is provable: replay recorded chocolate-server sessions against it byte-for-byte. Desktop interop (decision 6) requires this.
- B. New owned protocol — rejected: re-solves resend/drift/lobby, doubles the engine diff, loses interop.
- C. Dumb envelope router + in-WASM host tab forever — rejected as the end state (Rust is the server); retained as the M1 spike.
- D. wasm-embedded net_server.c as the server role — documented fallback if A's fidelity proves elusive.

Side benefit of A with a persistent Rust server: host migration dissolves — the server outlives every player, and a chan window-session leader (later) degrades to pure lobby authority.

## Phasing

- **M0 — scaffold.** Workspace, three crates, local gate script (fmt, clippy `-D warnings`, test), engine/ imported with upstream commit pinned in `engine/docs/provenance.md`, single-player WASM builds with the documented recipe. Exit: clean gate; bundle parity vs the pinned recipe.
- **M1 — transport spike.** Engine revival patch set; doom-server room layer; one browser tab runs the in-WASM host. Exit: two browser windows play co-op start-to-exit without desync (demo-recorded end states identical), with one mod load/unload smoke.
- **M2 — Rust server role.** doom-proto codec + golden fixtures; server-role state machine behind the same room API; host tab removed; UDP binding lands so a native chocolate-doom client can join. Exit: same two-window co-op against the pure Rust server; capture-replay parity vs real chocolate-server; one native-client-over-UDP join; deathmatch flags exercised once.
- **M3 — hardening + doom-embed.** doom-embed host + headless bot clients; 8-player 10-minute soak (bots padding to 8); stalled-client outbox-cap test; mod validation against the 2-3 picked mods; `docs/protocol.md` complete; `doom-v2` bundle published locally with recipe + LICENSES.txt.
- **M4 — chan integration (separate spec, new chan roadmap item, after this work).** chan-server links doom-server as a library on a new `/api/doom/ws` route modeled on `routes/scene.rs` minus disk persistence; lobby UI in the DoomOverlay; the engine iframe opens its own WebSocket (iframe isolation survives; SPA↔iframe is lobby postMessage only). The seam this repo owes: the sans-IO core.

## Verification (adversarial)

- Golden capture-replay: build upstream chocolate-doom + chocolate-server natively, record real sessions (handshake, GAMESTART, mid-game ticcmd streams, resend-under-loss, disconnect), replay byte-for-byte against doom-proto and later doom-server. The Rust server is wrong until proven otherwise against these fixtures.
- Codec fuzzing (proptest at minimum; cargo-fuzz target in-tree).
- Desync canary: identical demo-recorded end state across clients over a full E1M1 clear, on both M1 and M2 servers.
- Slow-client test: stall one connection, assert the outbox cap trips and the peer is disconnected with bounded memory — measured RSS, not just a green test.
- 8-player soak, 10+ minutes, bots padding to capacity.
- Mod smokes: load/unload the picked PWADs mid-session at each milestone from M1 on.

## Non-goals

- No server-side game simulation, ever. The server relays inputs; the engines simulate.
- No changes to chan in this repo. chan integration is M4, specced elsewhere.
- No game-logic changes in the engine beyond the netcode revival and mod management.
- No WS↔UDP relay to a native chocolate-server and no NAT traversal in v1 (UDP interop is LAN/loopback scope).
- No publishing (crates.io, GitHub push, releases) until the host says so.

## References

- `chan-doom:team/roadmap/v0.80.0/doom-multiplayer.md` — grounding + acceptance checks this design inherits.
- `chan:crates/chan-server/resources/doom/README.md` (doom-overlay branch) — bundle build recipe + provenance model to mirror.
- https://github.com/cloudflare/doom-wasm (GPL-2.0) + https://blog.cloudflare.com/doom-multiplayer-workers/ — the precedent.
- https://github.com/rojo2/wasm-doom (GPL-2.0) — engine base.
- https://www.moddb.com/games/doom/mods — mod shortlist source.
