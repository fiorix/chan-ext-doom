# doomit

DOOM, embeddable: a merged-lineage engine fork with its multiplayer netcode
revived, plus a Rust crate family for protocol, room-server, and native
embedding work.

The [design](docs/design.md) is the spec of record. This repository is at the
M1 transport spike: browsers run the restored engine and one browser remains
the Chocolate game host, while `doom-server` provides the bounded WebSocket
room relay. The pure-Rust Chocolate server role and UDP interop are M2;
`doom-embed` and bot clients are M3. They are goals, not current claims.

M1 acceptance requires two browser windows to complete E1M1 without a
recorded-command divergence and a selected-PWAD remove/restart smoke. The
engine and scripted two-node GAMESTART path are implemented; that interactive
acceptance check remains.

The host-selected PWAD validation set and its provenance are in
[docs/mods.md](docs/mods.md).

## Layout

- `engine/` — GPL-2.0 engine fork, browser loader, restored Chocolate netcode,
  WebSocket transport, native SDL_net transport source, and build/test recipes.
- `crates/doom-server/` — protocol-agnostic bounded rooms, Cloudflare-compatible
  WebSocket envelope relay, and the `doomd` CLI. It is not yet the Chocolate
  game-server state machine.
- `crates/doom-proto/` — current fixture-integrity checks; the full byte-exact
  codec lands with the M2 server role.
- `crates/doom-embed/` — buildable scaffold for the M3 wasmtime host.
- `fixtures/` — 102 curated datagrams from five real Chocolate Doom 3.1.1
  sessions, with capture/export tooling and provenance.
- `docs/` — accepted design, observed protocol inventory, mod catalog, and
  reproduction notes.

## Checks

```sh
./scripts/gate.sh
cd engine && npm test
```

The Rust gate runs formatting, warning-clean clippy, and tests. Engine build
and browser instructions, including the pinned emsdk version and reproducible
single-`emcc` artifact hashes, are in
[`engine/docs/provenance.md`](engine/docs/provenance.md).

## Licensing

The engine fork (`engine/`) is GPL-2.0, derived from the documented
DOOM/Chocolate/Crispy lineage. The Rust crates are Apache-2.0 and written from
scratch. Rust binaries do not link GPL engine object code; a browser or future
wasmtime host loads the separately licensed engine WASM as runtime data across
a defined sandbox interface.
