# doomit

DOOM, embeddable: a merged-lineage doom engine fork with its multiplayer netcode revived, and a Rust crate family that wires the protocol — `doom-proto` (wire codec), `doom-server` (rooms + the multiplayer server role, WebSocket and UDP), and `doom-embed` (run the engine WASM inside any Rust program via wasmtime).

The spec of record is [docs/design.md](docs/design.md).

## Layout

- `engine/` — the doom engine fork (merged rojo2/wasm-doom + cloudflare/doom-wasm lineage), GPL-2.0.
- `crates/` — the Rust crates, Apache-2.0. No chan dependencies; chan is the first consumer, not a dependency.
- `fixtures/` — golden packet captures from real chocolate-doom sessions.
- `docs/` — design, protocol spec, verification notes.
- `examples/` — standalone server and loopback demos.

## Licensing

The engine fork (`engine/`) is GPL-2.0 (derivative of crispy/chocolate doom). The Rust crates are Apache-2.0, written from scratch; the engine WASM is consumed strictly as runtime data, never linked or embedded, so the two licenses never mix in one binary.
