# Chan extension archive

Each platform archive uses one layout:

```text
doomit/
  doomit-extension[.exe]
  doomit.toml
  share/doomit/doom.js
  share/doomit/doom.wasm
  share/doomit/doom1.wad
  licenses/LICENSE-APACHE
  licenses/engine-GPL-2.0.txt
  licenses/doom-shareware.txt
  source/doom-engine-source.tar.gz
```

`doomit-extension` resolves runtime data at `share/doomit` relative to itself and refuses to print its Chan handshake unless all three pinned hashes match. The archive builder must use the exact engine artifacts recorded in `engine/docs/provenance.md`, the unmodified shareware Doom 1.9 IWAD, the original IWAD distribution notice, and a corresponding-source archive for the GPL engine build.

Tagged releases publish `doomit-linux-x86_64.tar.gz`, `doomit-linux-aarch64.tar.gz`, `doomit-windows-x86_64.zip`, and `doomit-macos-aarch64.tar.gz`. Each binary is compiled on a native GitHub-hosted runner; the Linux binaries target musl. `SHA256SUMS` covers all four archives, and the release also carries the root `install.sh`.

The verified runtime inputs are tracked under `runtime/`. `scripts/package-chan-extension.py` assembles release archives, while `scripts/install-chan-extension.sh` builds and installs the Unix executable directly from a checkout.

The release installer detects the supported operating system and architecture, verifies the archive checksum, installs its complete contents under `~/.local/lib/doomit`, writes an absolute command path to `~/.chan/extensions/doomit.toml`, and leaves Chan restart to the operator. Chan itself installs and ships no Doomit files.
