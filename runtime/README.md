# Doomit runtime data

These files are tracked sidecar data for the Chan extension. They are served to the browser at runtime and are not linked into or embedded in the Apache-2.0 Rust binary.

| file | bytes | sha256 | terms |
| --- | ---: | --- | --- |
| `doom.js` | 188773 | `692d05e8eb96d913cd3f3f66e10fd66cdaca2b947318463166847ae3cdebe84b` | GPL-2.0 |
| `doom.wasm` | 1690108 | `814929d027480cf74c6734d3ece5b42c62c46d3d21a35fea15d79e16ee1dc009` | GPL-2.0 |
| `doom1.wad` | 4196020 | `1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771` | Doom shareware |

`doom.js` and `doom.wasm` are generated from the corresponding source under [`../engine/`](../engine/) using the recipe and pinned toolchain in [`../engine/docs/provenance.md`](../engine/docs/provenance.md). Their license is [`../engine/COPYING.md`](../engine/COPYING.md).

`doom1.wad` is the unmodified Doom 1.9 shareware IWAD extracted from `https://www.doomworld.com/3ddownloads/ports/shareware_doom_iwad.zip`. The source archive has SHA-256 `845f4f3a449343b068a4e178f9cb018cb1f5b7d5ef09db292864ed554f612276`. Id Software retains its copyright. [`doom-shareware-license.txt`](doom-shareware-license.txt) preserves the limited-use license and John Carmack's redistribution clarification archived with Debian's `doom-wad-shareware` package.
