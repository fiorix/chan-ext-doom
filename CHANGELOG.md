# Changelog

This file records notable development history and design decisions. Reference documentation describes only the repository's current behavior and contracts.

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
