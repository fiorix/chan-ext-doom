# engine provenance and build recipe

`engine/` is the doom fork this repo builds on. It is imported source, not vendored-and-forgotten: every deviation from upstream is listed below, and the build recipe reproduces the shipped artifacts byte for byte.

## Upstream

| field | value |
|-------------|--------------------------------------------------------|
| project | rojo2/wasm-doom |
| url | https://github.com/rojo2/wasm-doom |
| commit | 619e69715ec303e9f7d192a0ba2d4025cf485214 |
| ref | master |
| lineage | Crispy Doom, itself Chocolate Doom, itself id's release |
| license | GPL-2.0 (`../LICENSE`, `../COPYING.md`) |

The import is the tracked tree at that commit and nothing else: 253 files, taken with `git archive` so no `.git` metadata and no build outputs can come along.

Reproduce the import:

```sh
git clone https://github.com/rojo2/wasm-doom /tmp/wasm-doom-ref
git -C /tmp/wasm-doom-ref archive --format=tar \
    619e69715ec303e9f7d192a0ba2d4025cf485214 | tar -x -C engine/
```

## License boundary

`engine/` is GPL-2.0 and stays that way. The Rust crates under `crates/` are Apache-2.0 and never link, embed, or statically include anything built from this directory: `doom.js` and `doom.wasm` are runtime data, served over HTTP to a browser or instantiated in a wasmtime sandbox. The boundary is a process boundary, so the two licenses never meet inside one binary.

Redistributing a build of this engine means shipping the corresponding source, which is this directory plus the deviation below.

## Deviations from upstream

One line, required to link at all with any modern clang.

`src/v_trans.h:38`, `enum` changed to `typedef enum`:

```diff
-enum
+typedef enum
 {
     CR_NONE,
```

Upstream's form declares a tentative global variable named `cr_t` in every translation unit that includes the header. Compilers defaulting to `-fno-common`, which clang has done since clang 11, then reject the link:

```
wasm-ld: error: duplicate symbol: cr_t
>>> defined in v_trans.o
>>> defined in r_data.o
```

`cr_t` is not used as a type anywhere in the tree, so the typedef spelling changes nothing but the link.

## Prerequisites

| tool | version used | source |
|------------|--------------|-------------------------------------------|
| emscripten | 6.0.3 | emsdk at `~/dev/emsdk` |
| node | 22.16.0 | bundled in emsdk, used by emcc internally |

```sh
git clone https://github.com/emscripten-core/emsdk ~/dev/emsdk
~/dev/emsdk/emsdk install latest && ~/dev/emsdk/emsdk activate latest
```

## Single-player build

Run from `engine/`. Outputs land in `engine/out/`, which is a build directory and is never committed.

```sh
source ~/dev/emsdk/emsdk_env.sh
mkdir -p out
emcc -Oz \
  '-Wno-#warnings' -Wno-macro-redefined -Wno-switch \
  -Igifenc -Iopl -Isdl_mixer -Isrc -Isrc/doom \
  $(find gifenc opl sdl_mixer src -name '*.c' | LC_ALL=C sort) \
  -s WASM=1 -s USE_SDL=2 -s USE_LIBPNG=1 \
  -s ALLOW_MEMORY_GROWTH=1 -s NO_EXIT_RUNTIME=1 \
  -s EXPORTED_RUNTIME_METHODS=FS,UTF8ToString \
  -s MODULARIZE=1 -s ASSERTIONS=0 \
  -o out/doom.js
```

The flag set is upstream's `CMakeLists.txt` Release configuration, minus the two spellings emscripten has since removed (`EXTRA_EXPORTED_RUNTIME_METHODS`, `--no-heap-copy`). The single `emcc` invocation is used instead of upstream's CMake path because it needs no host `cmake` and pins the flags where they can be read.

`LC_ALL=C sort` is load-bearing. Bare `find` emits readdir order, which varies by filesystem and by the order files were created, and emcc assigns wasm function indices and `EM_ASM` string addresses in command-line order. Without the sort the same source tree produces a different `doom.wasm` on every machine (observed: 1153188 vs 1155963 vs 1154218 bytes, all functionally identical). With it, the build is bit-reproducible.

### Expected output

Built from this tree with emscripten 6.0.3:

| file | bytes | sha256 |
|-----------|---------|------------------------------------------------------------------|
| doom.js | 178125 | 0e6c39721f2db3d5b1a2b69909ae9a044b61bfa2ab78027b04fbf9e34ffd5924 |
| doom.wasm | 1154218 | c0368d2f80d6c3d9185cf80896b6aebddbf1f8ef500f8e34760ee2659cf0276e |

These hashes are pinned to emscripten 6.0.3. A toolchain bump changes them; re-record rather than assume drift is a defect.

## Running it

The build is `MODULARIZE=1`, so `doom.js` exports an async factory rather than populating a global `Module`. Upstream's `doom.html` predates that and expects the global plus a `--preload-file` IWAD, so it does not drive this build as-is. A loader page must:

1. call the factory with `{ canvas, preRun }`,
2. in `preRun`, place the IWAD in the Emscripten FS: `M.FS.createPreloadedFile("/", "doom1.wad", "doom1.wad", true, false)`.

`Module["FS"]` is attached at factory-eval time and `run()` awaits run dependencies before `callMain`, so an IWAD preloaded that way is on disk before `D_DoomMain` looks for it.

### IWAD

Shareware `DOOM1.WAD` v1.9: 4196020 bytes, sha1 `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d`. Source: `https://www.doomworld.com/3ddownloads/ports/shareware_doom_iwad.zip`. The idgames `doom19s.zip` is the 1995 installer and its WAD is DEICE-packed, so it is not usable directly.

## Verification

The build is verified by running it, not just by linking it. Serve `out/doom.js`, `out/doom.wasm`, `doom1.wad`, and a loader page over HTTP, then load the page in headless Chrome with software GL (`--enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader`).

A good build reaches the attract demo: the console carries the `DOOM Shareware` banner, `W_Init` reports ` adding doom1.wad`, startup runs through `I_Init`, `R_Init`, `P_Init`, `S_Init`, `D_CheckNetGame`, `HU_Init`, `ST_Init` with no `I_Error`, and a screenshot after roughly 15 seconds shows E1M1 rendering behind a live status bar.
