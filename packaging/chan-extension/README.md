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

The verified runtime inputs are tracked under `runtime/`. For a local Unix install, `scripts/install-chan-extension.sh` builds the Rust executable, copies that runtime directory into the layout above, and writes the Chan declaration.

Copy `doomit.toml` to the active Chan home under `extensions/doomit.toml`, replace `command` with the extracted binary's absolute path, and restart Chan. Chan itself installs and ships no Doomit files.
