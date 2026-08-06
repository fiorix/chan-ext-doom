# Verifying and reproducing the protocol fixtures

How to independently check the committed golden packets in `fixtures/` and how to re-create them from scratch. Everything here ran on Ubuntu 26.04 x86_64; adapt package names elsewhere.

## 1. What is committed (and what is not)

Committed:

- `fixtures/<session>/NNN-dir-peer-type.bin`: raw UDP payloads, one file per datagram, in observed order (NNN is the capture index).
- `fixtures/<session>/session.json`: full provenance for each session, meaning the pinned commit, exact server and client argv, client environment, signal timings, and exit codes.
- `fixtures/manifest.json`: the machine-readable index with source commit, capture commands, and per-packet direction, order, type, reliable flag, length, and sha256. Regenerated from the tree by the rig; packet entries are one JSON object per line by design so the doom-proto test can parse with the standard library only.
- `fixtures/rig/capture.py`: the capture rig (Python 3 standard library only).
- `docs/protocol.md`: the inventory these fixtures ground.

The committed corpus is exactly **113 packets in seven sessions**: `console-message`, `drone-disconnect`, `gamestart-gamedata`, `handshake-keepalive`, `query`, `rejected-game-mismatch`, `rejected-in-game`.

Never committed: the IWAD, built binaries, upstream clones, raw `packets.jsonl` and process logs, pcaps. Those live in scratch only.

## 2. Independent validation of the committed fixtures

No build required, only coreutils and python3:

```sh
cd chan-ext-doom
# (a) every .bin: length and sha256 must match the manifest
python3 - <<'EOF'
import hashlib, json
m = json.load(open("fixtures/manifest.json"))
n = 0
for s in m["sessions"]:
    for p in s["packets"]:
        data = open("fixtures/" + p["file"], "rb").read()
        assert len(data) == p["length"], p["file"]
        assert hashlib.sha256(data).hexdigest() == p["sha256"], p["file"]
        hdr = (data[0] << 8) | data[1]
        want = p["type"] | (0x8000 if p["reliable"] else 0)
        assert hdr == want, (p["file"], hex(hdr))
        n += 1
print(f"{n} packets verified")
EOF
# (b) manifest is valid JSON (packet lines are single-line objects)
python3 -c "import json; json.load(open('fixtures/manifest.json'))"
# (c) the Rust checks (header/length/hash + no orphan bins):
cargo test -p doom-proto
```

Eyeball any packet with `xxd fixtures/handshake-keepalive/000-c2s-client1-syn.bin` and compare against `docs/protocol.md` section 4.

## 3. Reproducing the captures from scratch

### 3.1 Prerequisites

- Build tools: `gcc make autoconf automake pkg-config curl python3` (cmake and libtool are not needed).
- `libpng` dev files (chocolate-doom screenshots); check with `pkg-config --exists libpng`.
- SDL2 and SDL2_net are not assumed to exist on the host: the recipe builds both into a scratch prefix, with no sudo and no system changes.
- The shareware IWAD `doom1.wad`, sha1 `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d`. It is data you must supply; verify with `sha1sum doom1.wad`.

### 3.2 Pin and clone the capture source

```sh
# resolve the pin (recorded in fixtures/manifest.json -> source.commit)
git ls-remote --tags --refs https://github.com/chocolate-doom/chocolate-doom.git 'chocolate-doom-*'
git clone https://github.com/chocolate-doom/chocolate-doom.git ~/dev/chocolate-doom-ref
cd ~/dev/chocolate-doom-ref
git checkout 410d96855b5df5410ff591a90efeafa889119224   # tag chocolate-doom-3.1.1
```

### 3.3 Build SDL2 + SDL2_net into a scratch prefix

```sh
mkdir -p /tmp/doom-capture/{deps-src,deps,build,captures}
cd /tmp/doom-capture/deps-src
curl -fsSLO https://github.com/libsdl-org/SDL/releases/download/release-2.32.10/SDL2-2.32.10.tar.gz
curl -fsSLO https://github.com/libsdl-org/SDL_net/releases/download/release-2.2.0/SDL2_net-2.2.0.tar.gz
sha256sum *.tar.gz
# expect:
# 5f5993c530f084535c65a6879e9b26ad441169b3e25d789d83287040a9ca5165  SDL2-2.32.10.tar.gz
# 4e4a891988316271974ff4e9585ed1ef729a123d22c08bd473129179dc857feb  SDL2_net-2.2.0.tar.gz
tar xzf SDL2-2.32.10.tar.gz && tar xzf SDL2_net-2.2.0.tar.gz
mkdir build-sdl2 && cd build-sdl2
../SDL2-2.32.10/configure --prefix=/tmp/doom-capture/deps && make -j"$(nproc)" && make install
cd .. && mkdir build-sdl2net && cd build-sdl2net
export PATH=/tmp/doom-capture/deps/bin:$PATH
export PKG_CONFIG_PATH=/tmp/doom-capture/deps/lib/pkgconfig
../SDL2_net-2.2.0/configure --prefix=/tmp/doom-capture/deps && make -j"$(nproc)" && make install
```

### 3.4 Build chocolate-doom + chocolate-server

```sh
cd ~/dev/chocolate-doom-ref && autoreconf -f -i     # autogen.sh only works in-tree
mkdir -p /tmp/doom-capture/build/choc && cd /tmp/doom-capture/build/choc
export PATH=/tmp/doom-capture/deps/bin:$PATH
export PKG_CONFIG_PATH=/tmp/doom-capture/deps/lib/pkgconfig
~/dev/chocolate-doom-ref/configure \
  --prefix=/tmp/doom-capture/choc-install \
  --disable-sdl2mixer --without-libsamplerate --disable-doc \
  LDFLAGS=-Wl,-rpath,/tmp/doom-capture/deps/lib
make -j"$(nproc)"
ls -l src/chocolate-doom src/chocolate-server
```

Notes:

- The sound stack is disabled (`--disable-sdl2mixer`; the flag is NOT `--disable-sound`), which is why SDL2_mixer is never needed.
- `autoreconf` must run in the source dir; configure and make run out-of-tree.
- The rpath LDFLAGS makes the binaries find the scratch SDL2 without `LD_LIBRARY_PATH`.

### 3.5 Run the captures

The rig binds UDP 127.0.0.1:2342 and forwards to `chocolate-server -port 2343 -privateserver`; run scenarios sequentially. 2342 is chocolate's `DEFAULT_PORT`, a rig choice rather than a client limitation: clients can override with `-port` or a `host:port` connect address, and the rig does not need either. Raw output lands in `/tmp/doom-capture/captures/<name>/` (`packets.jsonl`, `session.json`, per-client logs, and chocolate's own `-netlog` dumps for cross-checking).

```sh
cd chan-ext-doom
S=/tmp/doom-capture/build/choc/src/chocolate-server
C=/tmp/doom-capture/build/choc/src/chocolate-doom
W=/path/to/doom1.wad   # sha1 must match section 3.1
O=/tmp/doom-capture/captures

python3 fixtures/rig/capture.py run --name handshake-keepalive \
  --description "lobby handshake + idle keepalives" \
  --server-bin $S --client-bin $C --iwad $W --out $O/handshake-keepalive \
  --duration 9 --sigterm-at 6

python3 fixtures/rig/capture.py run --name gamestart-gamedata \
  --description "autostart via -nodes 1: launch, gamestart, gamedata" \
  --server-bin $S --client-bin $C --iwad $W --out $O/gamestart-gamedata \
  "--client-extra=-nodes 1" --duration 12 --sigterm-at 10

python3 fixtures/rig/capture.py run --name drone-disconnect \
  --description "player killed; server disconnects the drone after 30s timeout" \
  --server-bin $S --client-bin $C --iwad $W --out $O/drone-disconnect \
  --signal kill --sigterm-at 5 "--client2-extra=-drone" --duration 44

python3 fixtures/rig/capture.py run --name console-message \
  --description "timeout broadcast to remaining clients" \
  --server-bin $S --client-bin $C --iwad $W --out $O/console-message \
  --signal kill --sigterm-at 5 "--client2-extra=-drone" "--client3-extra=" \
  --client3-delay 3 --duration 42

python3 fixtures/rig/capture.py run --name query \
  --description "server status query" \
  --server-bin $S --client-bin $C --iwad $W --out $O/query \
  "--client-extra=-query 127.0.0.1" --signal none --duration 6

# REJECTED evidence, two causes. Cause 1 needs no crafted input: a real
# client joining while the server is in game is refused.
python3 fixtures/rig/capture.py run --name rejected-in-game \
  --description "client2 joins while the server is in game; REJECTED 'not currently accepting connections'" \
  --server-bin $S --client-bin $C --iwad $W --out $O/rejected-in-game \
  "--client-extra=-nodes 1" "--client2-extra=" --client2-delay 6 --duration 12 --sigterm-at 10

# Cause 2 is a mismatched game mode/mission, which no two shareware
# clients can produce. The rig crafts a documented SYN: it copies the
# real packet's WAD and DEH checksums, changes only the two
# rejection-relevant fields (gamemode=2 commercial, gamemission=1
# doom2), and supplies a deterministic player class and name that are
# craft values, not the real client's (its class is uninitialized and
# its name random). The upstream mode/mission check, not player
# identity, produces the response.
python3 fixtures/rig/capture.py craft --kind syn --gamemode 2 --gamemission 1 \
  --checksums-from fixtures/handshake-keepalive/000-c2s-client1-syn.bin \
  --out /tmp/doom-capture/crafted-syn.bin
python3 fixtures/rig/capture.py run --name rejected-game-mismatch \
  --description "crafted doom2/commercial SYN against a doom/shareware lobby; REJECTED 'Game mismatch'" \
  --server-bin $S --client-bin $C --iwad $W --out $O/rejected-game-mismatch \
  --signal none --duration 8 \
  --inject "3:/tmp/doom-capture/crafted-syn.bin:crafted SYN per docs/protocol.md section 4: wad/deh sha1 copied from the real client SYN (handshake-keepalive/000); the two rejection-relevant fields changed deliberately (gamemode=2, gamemission=1); player_class=0 and name 'RigProbe' are deterministic craft choices, not the real client's values (its player_class is uninitialized and its name a random pet name)"
```

Expected packet counts (timing-dependent, plus or minus a few): handshake-keepalive about 25, gamestart-gamedata about 550, drone-disconnect about 180, console-message about 315, query exactly 2, rejected-in-game about 560, rejected-game-mismatch about 27.

### 3.6 Curate fixtures and regenerate the manifest

The `--range` indices below select windows from **the committed capture**; packet indices drift between runs (packet counts are timing-dependent, section 3.5). After a fresh capture, inspect its `packets.jsonl` and adjust the late-session windows before exporting:

```sh
# one line per datagram: index, time, direction, peer, type
python3 - <<'EOF'
import json
T={0:"SYN",3:"KEEPALIVE",4:"WAITING_DATA",5:"GAMESTART",6:"GAMEDATA",
   7:"GAMEDATA_ACK",8:"DISCONNECT",9:"DISCONNECT_ACK",10:"RELIABLE_ACK",
   11:"GAMEDATA_RESEND",12:"CONSOLE_MESSAGE",13:"QUERY",
   14:"QUERY_RESPONSE",15:"LAUNCH"}
for line in open("/tmp/doom-capture/captures/drone-disconnect/packets.jsonl"):
    p = json.loads(line)
    b = bytes.fromhex(p["hex"])
    t = ((b[0] << 8) | b[1]) & 0x7FFF
    print(p["i"], p["t_ms"], p["dir"], p["peer"], T.get(t, t))
EOF
```

```sh
python3 fixtures/rig/capture.py export --session $O/handshake-keepalive --fixtures-dir fixtures
python3 fixtures/rig/capture.py export --session $O/gamestart-gamedata --fixtures-dir fixtures --range 0:45
python3 fixtures/rig/capture.py export --session $O/drone-disconnect  --fixtures-dir fixtures --range 0:12,174:180
python3 fixtures/rig/capture.py export --session $O/console-message   --fixtures-dir fixtures --range 269:281
python3 fixtures/rig/capture.py export --session $O/query             --fixtures-dir fixtures
python3 fixtures/rig/capture.py export --session $O/rejected-in-game  --fixtures-dir fixtures --range 8:12,272:274
python3 fixtures/rig/capture.py export --session $O/rejected-game-mismatch --fixtures-dir fixtures --range 0:3,10:12
```

`--range` accepts comma-separated `START:END` windows. For `drone-disconnect`, pick the two handshakes plus the last keepalives through the DISCONNECT_ACK; for `console-message`, the window around the CONSOLE_MESSAGE broadcast; for `rejected-in-game`, the client1 GAMESTART block for state provenance plus the client2 SYN and REJECTED; for `rejected-game-mismatch`, the lobby handshake plus the injected SYN and REJECTED. Then re-run the checks from section 2.

## 4. Determinism expectations

Re-running the rig does **not** reproduce the committed bytes exactly: player names are random pet names, `player_class` and `player_classes` are uninitialized in doom, ephemeral ports differ, and sequence and timing fields drift (see `docs/protocol.md` section 5). What is stable and worth diffing between runs: packet type sequences per direction, the layout of every packet, lengths of fixed-layout types, and the timing constants (1 s keepalive, 1 s WAITING_DATA, 30 s timeout). The golden fixtures are one observed instance; the tests assert structure and hashes of the committed bytes, not regeneration.

## 5. Troubleshooting

- `configure: error: Package requirements (SDL2_mixer ...)`: you used `--disable-sound`; the correct flag is `--disable-sdl2mixer`.
- `autoreconf: error: 'configure.ac' is required`: you ran `autogen.sh` or `autoreconf` outside the source tree.
- Rig exits immediately with a bind error: UDP port 2342 is taken (`ss -lunp | grep 2342`); stop the other server or rig.
- Client dies instantly: check `<out>/client1.log`; usually a missing IWAD (path or sha1) or missing `SDL_VIDEODRIVER=dummy` (the rig sets it).
- `Failed to get I/O port permissions for 0x388` in the client log is harmless (OPL MIDI); captures are unaffected.

## 6. Current repository checks

Each repository suite provides distinct evidence beyond the fixture validation of section 2:

- `cargo test -p doom-proto --locked`: codec round-trip of the packet layouts in `docs/protocol.md` section 4, plus malformed-input guards.
- `cargo test -p doom-server --locked`: the sans-I/O server role (protocol lifecycle, tic windows) and the shared WebSocket/UDP binding and CLI surface of `doomd`.
- `./scripts/gate.sh`: the workspace gate, `cargo fmt --check` plus `cargo clippy --all-targets -- -D warnings` plus `cargo test`.
- `npm test` from `engine/`: the engine loader and page canary suites.

None of these captures packets: they are code-level evidence and complement, never substitute for, the committed fixtures.

## 7. Live native interoperability (integration evidence, not fixtures)

This recipe reproduces the accepted native UDP interoperability shape against `doomd`. It is live integration evidence, kept deliberately separate from the fixture sections above: nothing it produces is a committed fixture, and scratch traffic counts (for example strace logs) must never be cited as golden packet evidence.

Prerequisites: the native engine artifact, built as recorded in `engine/docs/provenance.md`, and the pinned shareware IWAD of section 3.1.

```sh
cd chan-ext-doom
cargo build --locked -p doom-server --bin doomd
./target/debug/doomd serve --listen 127.0.0.1:0 --udp arena=127.0.0.1:0
# read the advertised UDP endpoint from the startup lines
/path/to/native-doom -iwad /path/to/doom1.wad -nosound -nomusic \
  -connect 127.0.0.1:<advertised-udp-port> -nodes 1 -deathmatch
```

Expected: the client console shows the handshake (`NET_CL_ParseSYN`, `D_InitNetGame: Connected`; a program-name version note about the server string is expected and is wording only), then `NET_WaitForLaunch: starting with 1 nodes`, `startskill 2 deathmatch: 1`, and the personalized GAMESTART line `player 1 of 1 (1 nodes)`. The running game is itself sustained bidirectional GAMEDATA: the Chocolate lockstep advances only while tic data flows in both directions. For packet-level confirmation, `strace -f -e trace=sendto,recvfrom` on the client shows GAMEDATA both ways; those logs stay in scratch. A negative control (`-connect` to an unbound port) fails with `D_InitNetGame: Failed to connect`, which proves the positive run talked to `doomd`.
