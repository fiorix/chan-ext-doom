# doomit design

This document is the living specification for doomit's component boundaries, protocol strategy, constraints, and planned extensions.

## Purpose

doomit provides a buildable DOOM engine fork with Chocolate Doom multiplayer restored and a Rust crate family that separates room transport, protocol handling, and native embedding. The browser implementation is usable on its own; the crate boundaries also support a pure Rust server role, native Chocolate client interoperability, and embedding in Rust applications without making chan a dependency.

## Design principles

- Preserve the Chocolate Doom wire protocol and engine behavior instead of creating a new game protocol.
- Keep the GPL-2.0 engine and Apache-2.0 Rust crates separated by a runtime data boundary.
- Keep room membership, packet routing, protocol state, and game simulation as distinct responsibilities.
- Size the room layer for Chocolate's `NET_MAXPLAYERS` value of eight while respecting the engine's four-player in-game limit.
- Treat the shareware Doom 1.9 IWAD as pinned runtime data and PWAD identity as part of multiplayer agreement.
- Make deterministic behavior observable through independent input-history and simulation-state digests.
- Keep chan integration outside this repository and expose narrow library boundaries that a consumer can adapt.

## Current system

The engine is a rojo2/wasm-doom Crispy lineage base with the matching Chocolate network layer restored. Browser builds use `net_websockets.c`; one browser engine runs Chocolate's client and server roles, and other browser engines connect as clients.

`doom-server` provides a sans-I/O `Registry` for named rooms with a maximum of eight connections, bounded FIFO outboxes, payload limits, and disconnect-on-overflow behavior. Its WebSocket binding owns Cloudflare-compatible route envelopes and route identity. The `doomd serve` CLI exposes that binding at `/ws/{room}`. It relays opaque Chocolate packets and contains no Chocolate game-server state machine.

The browser loader validates the pinned shareware IWAD, manages ordered PWADs, normalizes Chocolate's merge-before-file precedence, fingerprints the effective mod configuration, and replaces the engine iframe when configuration changes. It can host or join a room, record a verification demo, and compare exit digests between two cooperating same-origin pages.

The fixture set contains 102 curated native UDP datagrams from five Chocolate Doom 3.1.1 sessions. `doom-proto` verifies fixture membership, lengths, headers, and hashes; it exposes no packet codec. `doom-embed` is a buildable crate scaffold without a wasmtime host.

The CMake and single-command engine build routes are browser-only and require Emscripten. `src/net_sdl.c` is retained source, but no native engine target or Rust UDP binding is buildable from this tree.

## Repository shape

```text
fiorix/doomit
├── engine/                         # GPL-2.0 engine fork, browser loader, builds, tests
├── crates/
│   ├── doom-proto/                 # fixture checks and protocol-codec boundary
│   ├── doom-server/                # room core, WebSocket binding, doomd CLI
│   └── doom-embed/                 # native-host crate scaffold
├── fixtures/                       # captured Chocolate packet evidence
├── docs/
│   ├── protocol.md                 # observed and source-grounded wire inventory
│   ├── verification.md             # fixture capture and validation procedure
│   └── mods.md                     # selected PWAD contract and provenance
└── scripts/gate.sh                 # Rust formatting, lint, and test gate
```

Nothing in `crates/` depends on chan.

## Component contracts

### Engine

The engine owns game simulation, Chocolate client and server behavior, browser input/audio/video, WAD loading, demo recording, and the exit-state serializer. The WebSocket transport is a `net_module_t` implementation and carries Chocolate packets without changing their bytes.

The engine emits two exit reports at the same `ga_completed` anchor before level teardown. The input report hashes the canonical recorded ticcmd stream. The state report serializes deterministic game state in the explicit `DCS1` format and hashes it. Presentation-only state and per-instance pointers are excluded. The loader displays an aggregate match only when both report types match for a reciprocally paired launch and the same exit identity.

### Room server

The room core owns portable room names, membership, stable connection identifiers, bounded queues, FIFO relay, payload limits, and slow-consumer removal. The WebSocket adapter owns route parsing, source binding, room reset semantics, and frame encoding. These layers remain protocol-agnostic.

The designed Chocolate server-role layer sits above packet codecs and below transport adapters. It owns handshake state, the player table, lobby settings, GAMESTART, ticcmd fan-out, resend windows, keepalive, timeout, and disconnect behavior. It never simulates the game.

### Protocol crate

The protocol crate's intended public surface is explicit big-endian encoding and decoding for Chocolate packet fields, including packet headers, handshake messages, GAMESTART settings, ticcmd windows, reliable sequencing, resend, keepalive, and disconnect. The separate WebSocket route envelope uses little-endian route identifiers and is not part of the Chocolate packet format.

The codec must use fixed-width field operations rather than C layout, transmute, or copied GPL implementation code. Captured packets and the source-grounded inventory in `protocol.md` define the interoperability evidence.

### Native embedding

The native embedding boundary is designed as an optional wasmtime host for the engine WebAssembly artifact. Host callbacks provide framebuffer, audio, and input, while network imports map to a transport abstraction. Bot clients use the same boundary for scale and deterministic tests. Wasmtime remains isolated in this crate so consumers that only need the room server do not take the dependency.

## Protocol and transport strategy

Chocolate packets remain the game protocol. Browser engines add an asymmetric route envelope around each packet:

- Engine to relay: destination route ID, source route ID, then the unchanged Chocolate packet.
- Relay to engine: source route ID, then the unchanged Chocolate packet.

The route IDs are little-endian u32 values. Chocolate packet fields remain big-endian. The current host browser runs Chocolate's server role; the relay only moves frames between route IDs.

The pure Rust server role is the designed replacement for the host browser's server role. Browser clients reach it through the room/WebSocket boundary, and native Chocolate clients reach it through a UDP binding. Both transports feed the same packet and server-state interfaces.

## Mod contract

The pinned IWAD is shareware Doom 1.9: 4,196,020 bytes, SHA-1 `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d`, and SHA-256 `1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771`.

Each PWAD identity contains its basename, SHA-256, load kind, and effective embedded-DEHACKED behavior. The multiplayer fingerprint contains the canonical effective order: all `merge` entries in declared relative order, then all `file` entries in declared relative order. Standalone DEH/BEX patches, unsafe basenames, duplicate names, and the reserved IWAD name are rejected.

Changing the set restarts the engine instance. The loader does not claim hot unloading. No third-party PWAD binary is committed; source and rights metadata live in the mod catalog and provenance snapshot.

## Licensing boundary

- `engine/` is GPL-2.0. Distributing a built engine artifact requires corresponding source and preserved upstream attribution.
- The Rust crates are Apache-2.0 and reimplement wire behavior from observed packets and public format evidence. They do not copy engine C code.
- The engine WebAssembly artifact is runtime data. Rust crates neither link it nor include it in their binaries.
- Browser iframes and a native WebAssembly instance are runtime isolation boundaries, not claims that every host uses a separate operating-system process.

## Planned components

### Rust protocol and server role

Implement the byte-exact codec and the Chocolate server-role state machine behind the existing room interfaces. Verify behavior against captured native sessions and source-grounded packet layouts.

### Native transport

Make the engine buildable without browser-only Emscripten seams, enable the retained SDL_net transport, and add a Rust UDP adapter so native Chocolate clients and browser clients can join the same server role.

### Embedding and scale validation

Implement the wasmtime host and bot client boundary, then exercise eight-room capacity, sustained lockstep, stalled-peer removal, and deterministic mod behavior without requiring eight human players.

### chan adapter

Expose the sans-I/O room and server-role core through a chan-owned route and lobby UI. The chan SPA origin treats the browser canary as a cooperative desync aid, not as a security signal.

## Verification model

Implemented checks cover the Rust room relay, WebSocket route binding, fixture integrity, transport framing and ownership, ABI consistency, loader configuration, deterministic canary serialization, and multi-page verdict behavior. The browser build is pinned by exact artifact hashes under emsdk 6.0.3.

Protocol and server-role implementation must add golden decode/encode parity, malformed-input rejection, reliable-sequence and resend coverage, and replay against native Chocolate sessions. Native transport must demonstrate a real Chocolate client join. Scale work must measure bounded memory while a peer stalls and exercise the full eight-connection room capacity.

The end-to-end browser invariant is bilateral agreement at the same exit: both pages report matching input history and matching serialized simulation state. A missing digest, stale partner, unrelated page, malformed report, or one-sided match remains incomplete.

## Non-goals

- The server does not simulate DOOM game logic.
- This repository does not modify chan.
- The engine does not change gameplay beyond netcode restoration, deterministic verification, and mod management.
- The design does not include NAT traversal.
- The browser verdict does not authenticate same-origin pages and must not gate a security-sensitive action.

## References

- `chan-doom:team/roadmap/v0.80.0/doom-multiplayer.md`: grounding and acceptance analysis.
- `chan:crates/chan-server/resources/doom/README.md`: bundle build and provenance model.
- https://github.com/cloudflare/doom-wasm: browser multiplayer precedent.
- https://blog.cloudflare.com/doom-multiplayer-workers/: room-router precedent.
- https://github.com/rojo2/wasm-doom: engine base.
- https://www.moddb.com/games/doom/mods: mod catalog source.
