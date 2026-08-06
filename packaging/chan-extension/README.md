# Chan extension archive

Each platform archive uses one layout:

```text
chan-ext-doom/
  chan-ext-doom[.exe]
  chan-ext-doom.toml
  share/chan-ext-doom/doom.js
  share/chan-ext-doom/doom.wasm
  share/chan-ext-doom/doom1.wad
  licenses/LICENSE-APACHE
  licenses/engine-GPL-2.0.txt
  licenses/doom-shareware.txt
  source/doom-engine-source.tar.gz
```

`chan-ext-doom` resolves runtime data at `share/chan-ext-doom` relative to itself and refuses to print its Chan handshake unless all three pinned hashes match. The archive builder must use the exact engine artifacts recorded in `engine/docs/provenance.md`, the unmodified shareware Doom 1.9 IWAD, the original IWAD distribution notice, and a corresponding-source archive for the GPL engine build.

Tagged releases publish `chan-ext-doom-linux-x86_64.tar.gz`, `chan-ext-doom-linux-aarch64.tar.gz`, `chan-ext-doom-windows-x86_64.zip`, and `chan-ext-doom-macos-aarch64.tar.gz`. Each binary is compiled on a native GitHub-hosted runner; the Linux binaries target musl. `SHA256SUMS` covers all four archives, and the release also carries the root `install.sh`.

The verified runtime inputs are tracked under `runtime/`. `scripts/package-chan-extension.py` assembles release archives, while `scripts/install-chan-extension.sh` builds and installs the Unix executable directly from a checkout.

The release installer detects the supported operating system and architecture, verifies the archive checksum, installs its complete contents under `~/.local/lib/chan-ext-doom`, writes an absolute command path to `~/.chan/extensions/chan-ext-doom.toml`, and leaves Chan restart to the operator. Chan itself installs and ships no Doomit files.
