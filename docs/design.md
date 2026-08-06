# doomit design

This document is the living specification for doomit's component boundaries, protocol strategy, constraints, and planned extensions.

## Purpose

doomit provides a buildable DOOM engine fork with Chocolate Doom multiplayer restored and a Rust crate family that separates room transport, protocol handling, Chan integration, and native embedding. Browser and native engines act as Chocolate clients of the Rust server role. The crate boundaries also support embedding in Rust applications without making Chan a dependency of the protocol or server core.

## Design principles

- Preserve the Chocolate Doom wire protocol and engine behavior instead of creating a new game protocol.
- Keep the GPL-2.0 engine and Apache-2.0 Rust crates separated by a runtime data boundary.
- Keep room membership, packet routing, protocol state, and game simulation as distinct responsibilities.
- Size the room layer for Chocolate's `NET_MAXPLAYERS` value of eight while respecting the engine's four-player in-game limit.
- Treat the shareware Doom 1.9 IWAD as pinned runtime data and PWAD identity as part of multiplayer agreement.
- Make deterministic behavior observable through independent input-history and simulation-state digests.
- Keep Chan integration in an independently installed adapter over narrow host and library boundaries.

## Current system

The engine is a rojo2/wasm-doom Crispy-lineage base with the matching Chocolate network layer restored. Browser builds use `net_websockets.c` and the loader permits only the Chocolate client role. Native builds use SDL_net UDP. Both targets run the same game simulation and client protocol behavior.

`doom-server` provides a protocol-agnostic sans-I/O `Registry` for named rooms with a maximum of eight connections, bounded FIFO outboxes, payload limits, and disconnect-on-overflow behavior. A transport-neutral `RoomHost` combines each named room with one Rust `ServerRole`. The role owns handshake, lobby settings, personalized GAMESTART, ticcmd windows and fan-out, reliable sequencing, resend, keepalive, timeout, and disconnect behavior. It never simulates the game.

The WebSocket binding owns route envelopes and route identity. Route 1 is permanently server-owned, and the legacy destination-0 reset marker is inert. The UDP binding carries one Chocolate packet per datagram, keys peers by listener and remote socket address, and shares the same `RoomHost` as WebSocket peers. `doomd serve` exposes `/ws/{room}` and repeatable `--udp ROOM=ADDR` listeners.

The browser loader validates the pinned shareware IWAD, manages ordered PWADs, normalizes Chocolate's merge-before-file precedence, fingerprints the effective mod configuration, and replaces the engine iframe when configuration changes. It launches multiplayer tabs with the same client-only network arguments and compares exit digests between two cooperating same-origin pages.

`doom-extension` adapts the browser engine and `doom-server` to Chan extensions v1. It verifies the pinned runtime data before advertising an ephemeral loopback endpoint, isolates lobbies by Chan's private tenant scope, and keeps the engine inside the opaque extension iframe. Installation remains independent: Chan does not bundle or install Doomit.

The fixture set contains 113 curated native UDP datagrams from seven Chocolate Doom 3.1.1 sessions. `doom-proto` provides the directional byte-exact packet codec used by the server role and bindings, plus fixture membership, length, header, and hash checks. `doom-embed` remains a buildable crate scaffold without a wasmtime host.

## Repository shape

```text
fiorix/chan-ext-doom
├── engine/                         # GPL-2.0 client engine, browser loader, native target, tests
├── crates/
│   ├── doom-extension/             # independently installed Chan adapter and extension UI
│   ├── doom-proto/                 # byte-exact Chocolate packet codec and fixture checks
│   ├── doom-server/                # room core, Rust server role, WS/UDP bindings, doomd CLI
│   └── doom-embed/                 # native-host crate scaffold
├── fixtures/                       # captured Chocolate packet evidence
├── docs/
│   ├── protocol.md                 # observed, source-grounded, and integration wire inventory
│   ├── chan-extension.md           # Chan host, lobby, iframe, and distribution contract
│   ├── verification.md             # fixture capture and validation procedure
│   └── mods.md                     # selected PWAD contract and provenance
└── scripts/gate.sh                 # Rust formatting, lint, and test gate
```

No crate imports Chan libraries. `doom-extension` speaks the external extensions-v1 process, HTTP, WebSocket, and iframe contract.

## Component contracts

### Engine

The engine owns game simulation, Chocolate client behavior, browser or native input/audio/video, WAD loading, demo recording, and the exit-state serializer. Its WebSocket and SDL_net transports carry Chocolate packets without changing their bytes.

The browser loader refuses `-server` and `-privateserver`; route 1 belongs to the Rust server. Retained C server sources remain part of the imported engine lineage and are still referenced by client-path teardown, but they are not the browser multiplayer authority.

The engine emits two exit reports at the same `ga_completed` anchor before level teardown. The input report hashes the canonical recorded ticcmd stream. The state report serializes deterministic game state in the explicit `DCS1` format and hashes it. Presentation-only state and per-instance pointers are excluded. The loader displays an aggregate match only when both report types match for a reciprocally paired launch and the same exit identity.

### Room server

The `Registry` owns portable room names, membership, stable connection identifiers, bounded queues, FIFO relay, payload limits, and slow-consumer removal. It remains protocol-agnostic.

The `RoomHost` owns protocol-aware reduction around the `ServerRole`, packet-local encoding context, transport-neutral removal effects, and authoritative room settings. The `ServerRole` owns reusable protocol slots, slot-ordered timers, the player table, lobby settings, GAMESTART, cumulative ticcmd windows, reliable delivery, resend and deadlock recovery, keepalive, timeout, and disconnect state.

Transport adapters own external identity and I/O. WebSocket peers use bound route identifiers and server-owned route 1. UDP peers use listener-scoped socket identity, raw datagrams, stateless QUERY, a 1500-byte ceiling, bounded newest-suffix GAMEDATA adaptation, and two-phase take/send/finish cleanup. Both adapters feed the same room clock and role.

### Protocol crate

The protocol crate exposes explicit directional big-endian encoding and decoding for Chocolate packet fields, including packet headers, handshake messages, GAMESTART settings, ticcmd windows, reliable sequencing, resend, keepalive, query, rejection, and disconnect. The separate WebSocket route envelope uses little-endian route identifiers and is not part of the Chocolate packet format.

The codec uses fixed-width field operations rather than C layout, transmute, or copied GPL implementation code. Captured packets and the source-grounded inventory in `protocol.md` define the interoperability evidence.

### Native embedding

The native embedding boundary is designed as an optional wasmtime host for the engine WebAssembly artifact. Host callbacks provide framebuffer, audio, and input, while network imports map to a transport abstraction. Bot clients use the same boundary for scale and deterministic tests. Wasmtime remains isolated in this crate so consumers that only need the room server do not take the dependency.

### Chan extension

The independently installed adapter owns the extensions-v1 handshake, verified runtime-data service, tenant-scoped lobby, host message bridge, and nested engine lifecycle. Chan owns discovery, supervision, capability proxying, session context, commands, presentation, and the outer sandbox. The complete boundary is defined in [`chan-extension.md`](chan-extension.md).

## Protocol and transport strategy

Chocolate packets remain the game protocol. Browser engines wrap each packet in an asymmetric route envelope: client frames name a destination and source, while server frames name only the source. Route identifiers are little-endian u32 values and Chocolate packet fields remain big-endian.

Client browser frames target server route 1. A client may not bind route 1, and a frame claiming source 1 closes only that offender. The old destination-0 registration/reset marker does not reset the room. Room lifetime follows live membership plus already-owed removal traffic.

Native clients send raw Chocolate packets over UDP. Repeated listeners may share a named room while `(listener, remote SocketAddr)` remains the binding identity. The adapter accepts datagrams up to and including 1500 bytes and never parses a truncated oversize input. Input-reachable oversize GAMEDATA output keeps the newest complete suffix and advances the advertised start so the client requests the omitted prefix through ordinary resend.

Both transports reach one Rust role per room. The role is the protocol authority but not a simulation authority: clients simulate locally from the same settings and ticcmd stream.

## Compatibility boundary

The interoperability oracle is Chocolate Doom 3.1.1 at upstream commit `410d96855b5df5410ff591a90efeafa889119224`. The current product path uses the pinned shareware Doom 1.9 IWAD.

The imported engine has later Crispy-lineage mission ordinals that conflict with the pinned Chocolate table. NERVE and MASTER are not admitted by the Rust role because SYN-only acceptance would be incomplete without matching episode/map and version semantics. No claim is made for every Crispy-lineage mission.

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

### Embedding and scale validation

Implement the wasmtime host and bot client boundary, then exercise eight-room capacity, sustained lockstep, stalled-peer removal, and deterministic mod behavior without requiring eight human players.

## Verification model

Implemented checks cover fixture integrity, byte-exact packet round trips, malformed-input rejection, the sans-I/O server lifecycle, cumulative tic windows, reliable sequencing, shared WebSocket/UDP room behavior, route ownership, listener identity, bounded queues, CLI lifecycle, engine transport framing, loader configuration, deterministic canary serialization, and multi-page verdict convergence. Browser builds are pinned by exact artifact hashes under emsdk 6.0.3, and the native target is reproducible from the same tree.

Live integration proves that the pinned native engine reaches personalized GAMESTART and sustained UDP tic exchange against `doomd`. A mixed native UDP and browser WebSocket room reaches players 1/2 and 2/2 with authoritative deathmatch settings and sustained traffic. A separate native `-record` run proves authoritative low-resolution turn negotiation for both transports.

The end-to-end browser invariant is bilateral agreement at the same exit: both pages report matching input history and matching serialized simulation state. A missing digest, stale partner, unrelated page, malformed report, or one-sided match remains incomplete. The current client-only Rust-hosted path still lacks a completed bilateral E1M1 exit evidence run.

## Non-goals

- The server does not simulate DOOM game logic.
- This repository does not modify chan.
- The engine does not change gameplay beyond netcode restoration, deterministic verification, and mod management.
- The design does not include NAT traversal.
- The browser verdict does not authenticate same-origin pages and must not gate a security-sensitive action.

## References

- [`../roadmap/doom-multiplayer.md`](../roadmap/doom-multiplayer.md): superseded grounding analysis that produced this design.
- [`../roadmap/design-review-2026-07-27.md`](../roadmap/design-review-2026-07-27.md): accepted decision audit for this design.
- https://github.com/cloudflare/doom-wasm: browser multiplayer precedent.
- https://blog.cloudflare.com/doom-multiplayer-workers/: room-router precedent.
- https://github.com/rojo2/wasm-doom: engine base.
- https://www.moddb.com/games/doom/mods: mod catalog source.
