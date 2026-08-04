# Changelog

This file records notable development history and design decisions. Reference documentation describes only the repository's current behavior and contracts.

## 2026-08-04

### Chan extension

- Added lobby and console collapse toggles to the extension UI so the running game can take the whole tab, and returned focus to the engine frame after each toggle.
- Added native GitHub release archives for Linux x86_64 and arm64, Windows x86_64, and macOS arm64, plus a checksum-verifying installer that writes the extension into Chan's local discovery path.
- Added the pinned browser engine outputs, unmodified shareware IWAD, redistribution notice, and a local Chan installer so a checkout contains the complete extension runtime.
- Corrected the executable allowlist to the byte-identical result of two fresh builds with the documented emsdk 6.0.3 release and added a regression test over the tracked sidecars.

## 2026-08-03

### Chan extension

- Re-pinned the emsdk 6.0.3 browser artifacts after later engine changes made the previous allowlist reject reproducible builds, and tied the executable pins to the provenance record with a regression test.
- Verified discovery, capability proxying, tenant-scoped WebSockets, command launch, session context, engine rendering, presentation, and process shutdown against Chan v0.83.0.
- Fixed the empty-state copy remaining visible over the running game.

## 2026-07-29

### Rust protocol and server role

- Implemented the directional byte-exact Chocolate packet codec and expanded the committed native corpus to 113 packets from seven sessions, including two distinct REJECTED causes.
- Implemented the sans-I/O Rust Chocolate server role with reusable protocol slots, lobby authority, personalized GAMESTART, cumulative ticcmd windows, reliable delivery, resend and deadlock recovery, slot-ordered timers, and source-faithful timeout and disconnect lifecycles.
- Corrected protocol fidelity at the rejection boundary, including old-magic state handling, quiet pre-SYN removal, exact mission and mode names, and the pinned Chocolate Doom 3.1.1 admission table.

### Shared WebSocket and UDP hosting

- Replaced the browser-hosted Chocolate server role with one Rust `RoomHost` and `ServerRole` per named room, shared by WebSocket and UDP peers.
- Made WebSocket route 1 permanently server-owned, retired the legacy destination-0 room reset, coupled timer lifetime to service lifetime, and bounded each atomic host reduction with an exact producer invariant.
- Added listener-scoped UDP identity, stateless QUERY, exact 1500-byte datagram handling, newest-suffix adaptation for oversized GAMEDATA, packet-local codec width, terminal-last removal, and two-phase take/send/finish cleanup.
- Added repeatable `doomd serve --udp ROOM=ADDR` bindings and a bounded CLI process harness covering ephemeral ports, conflicts, bind failures, and child cleanup.

### Client targets and interoperability

- Added a reproducible native SDL_net engine target while preserving the pinned browser build, and made the browser loader client-only with fail-closed rejection of local server-role arguments.
- Verified the pinned native engine through personalized GAMESTART and sustained UDP tic exchange against `doomd`, including deathmatch settings, acknowledgements, and resend traffic.
- Verified a mixed native UDP and browser WebSocket room with players 1/2 and 2/2, authoritative deathmatch settings, sustained traffic, and a separate authoritative low-resolution turn run triggered by native recording.

## 2026-07-28

### Documentation

- Recast the repository-authored reference documentation around current behavior and explicit designed boundaries, added architecture and sequence diagrams, and moved development history into this changelog.
- Added contributor conventions for present-tense references, one-logical-line Markdown, Mermaid diagrams, and punctuation without em dashes.

### Browser multiplayer acceptance

- Verified two independent browser engines reaching GAMESTART through the Rust WebSocket room relay.
- Completed a two-window shareware E1M1 run with matching input-history and simulation-state digests in both windows.
- Verified the selected three-PWAD canonical order, effective DEHACKED behavior, visible mod effects, and removal followed by a clean engine relaunch.

### Deterministic verification

- Added a canonical demo-input digest and the explicit `DCS1` exit-state serializer at the level-completion anchor.
- Added automatic component and aggregate verdicts correlated by reciprocal per-launch sessions and exit identity.
- Hardened multi-page convergence, relaunch behavior, malformed-report rejection, message boundaries, and concurrent state-report capture.
- Documented the cooperative same-origin trust boundary and the exactly-two-visible-windows operating perimeter.

### Browser build contract

- Made CMake explicitly browser-only because the engine source depends on Emscripten APIs and main-loop inversion.
- Pinned GNU C99 in the single-command and CMake routes and recorded deterministic emsdk 6.0.3 artifact hashes.
- Replaced caller-controlled platform detection with an actual Emscripten compiler probe, then made that probe immune to command-line and cache preseeding.
- Added the Apache-2.0 license text for the Rust crates and adopted one-logical-line formatting for repository-authored Markdown.

## 2026-07-27

### Initial design decisions

- Chose an Apache-2.0 Rust crate family beside a GPL-2.0 engine fork, with the engine artifact kept as separately licensed runtime data.
- Chose the rojo2/wasm-doom Crispy lineage as the engine base and restored the matching Chocolate Doom network layer.
- Kept the Chocolate Doom wire protocol for interoperability and assigned the eventual native game-server role to Rust.
- Set native Chocolate client interoperability over UDP, co-op and deathmatch settings, an eight-connection room layer, and optional native WebAssembly embedding as design requirements.
- Pinned the shareware Doom 1.9 IWAD and allowed compatible PWADs without pretending the game mode is registered Doom.
- Kept chan integration outside the repository behind a sans-I/O library boundary.

### Repository foundation

- Created the Rust workspace with `doom-proto`, `doom-server`, and `doom-embed`.
- Added the bounded protocol-agnostic room registry, WebSocket route binding, and `doomd serve` CLI.
- Imported rojo2/wasm-doom at `619e69715ec303e9f7d192a0ba2d4025cf485214` and restored the matching Chocolate netcode, browser WebSocket transport, multiplayer loop behavior, and a mod-aware loader.
- Replaced the browser build's busy-waiting sleep with an ASYNCIFY yield so WebSocket callbacks can progress during connect, lobby, and tic waits.
- Restored demo ticcmd writing and armed recording only after the effective game settings exist.
- Fixed a translation-unit-dependent `boolean` ABI split that changed `player_t` and `net_full_ticcmd_t` layouts and corrupted adjacent simulation globals.
- Added deterministic browser and CMake build recipes with upstream provenance.

### Protocol evidence

- Added a standard-library capture/export rig and 102 curated datagrams from five Chocolate Doom 3.1.1 loopback sessions at upstream commit `410d96855b5df5410ff591a90efeafa889119224`.
- Added fixture integrity tests and an observed/source-grounded Chocolate packet inventory.
- Corrected Chocolate packet byte order to big-endian and documented the separate little-endian WebSocket route envelope.
- Recorded the 30-second silent timeout, disconnect and acknowledgement paths, and the same-tick console-message pre-emption behavior evidenced by the captures.
- Pinned the asymmetric WebSocket envelope against cloudflare/doom-wasm `65e0d3ae2ffa604155eebd96ed40da6567bd08f4` and doom-workers `22d8665f75017c4e1971d7e93567237645916ba1`.
- Hardened capture export names and destinations, corrected disconnect provenance, and separated raw capture-time timing notes from claims reproducible through committed fixtures.

### Mod catalog and loader

- Selected PSX Doom, Doom But Slightly More Spooky, and CoTeCiO's Sound Effect Pack as resource-only validation inputs compatible with the pinned shareware IWAD.
- Preserved source-page rights metadata and exact archive/PWAD hashes without committing third-party WAD binaries.
- Added pinned-IWAD validation, canonical merge-before-file ordering, JSON configuration fingerprints, embedded-DEHACKED policy, safe filename handling, and iframe replacement for configuration changes.
