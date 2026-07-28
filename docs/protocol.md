# Chocolate Doom wire protocol

This document is the owned inventory of the Chocolate Doom network protocol as implemented and exercised in this repository. Every statement is labeled by its evidence basis:

- **[observed]**: seen in the committed captures under `fixtures/` (index in `fixtures/manifest.json`; reproduction path in `docs/verification.md`).
- **[reference]**: read from the pinned upstream C source (chocolate-doom `410d96855b5df5410ff591a90efeafa889119224`, tag `chocolate-doom-3.1.1`). Used for interoperability only; no C code is ported into `crates/`.
- **[unknown]**: not present in committed evidence and not verified.
- **capture-time note**: seen in raw capture logs that are not committed (scratch `packets.jsonl` with `t_ms`). The committed fixtures retain packet order, direction, and bytes, but no timestamps, so rates and intervals quoted this way are not independently reproducible from the committed evidence alone.

Capture source of truth: `fixtures/` and `fixtures/rig/capture.py`. All captures are native UDP loopback, chocolate-doom 3.1.1 client against chocolate-server 3.1.1, shareware doom1.wad (sha1 `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d`, which is never committed).

## Current flow through the room relay

Two browser windows each run an engine instance (WASM) launched by its loader page. The engines speak the Chocolate net protocol inside the WebSocket envelope of section 6 through `doomd`, a protocol-agnostic WebSocket room relay that forwards binary frames by node id and neither simulates the game nor computes any verdict. At the level exit each engine computes its input-history and exit-state digests, and the two loader pages compare those digests over a same-origin `BroadcastChannel`.

```mermaid
sequenceDiagram
    participant HA as host page (doom.html)
    participant HE as host engine (WASM)
    participant R as doomd room relay
    participant JE as join engine (WASM)
    participant JP as join page (doom.html)

    HA->>HE: launch (IWAD, room URL, recording on)
    JP->>JE: launch (same set)
    HE->>R: connect; claim room: [to=0][from=1]
    JE->>R: connect: [to=1][from=N]
    Note over HE,JE: Chocolate packets inside the WS envelope
    JE->>R: SYN
    R->>HE: forward
    HE->>R: SYN accept, WAITING_DATA
    R->>JE: forward
    HE->>R: LAUNCH, GAMESTART
    R->>JE: forward
    loop 35 Hz lockstep
        JE->>R: GAMEDATA (ticcmds)
        R->>HE: fan-out
        HE->>R: GAMEDATA (fan-out)
        R->>JE: forward
    end
    Note over HE,JE: both engines reach the exit anchor (ga_completed)
    HE->>HA: DEMO CANARY and STATE CANARY lines
    JE->>JP: DEMO CANARY and STATE CANARY lines
    HA->>JP: BroadcastChannel: arm/session, then reports
    JP->>HA: BroadcastChannel: reports
    Note over HA,JP: pages compare digests and render the verdict
```

## 1. Transport and framing

- UDP datagrams, one packet per datagram. The server listens on port **2342** by default (`DEFAULT_PORT`). 2342 is also the client's default destination, but it **is** configurable client-side: `-port <n>` changes the default and `-connect host:port` targets an explicit port (`net_sdl.c`). The captures use the default. `[observed, reference]`
- There is **no per-packet length, magic, or checksum** in the connected phase; the datagram boundaries are the framing. `[observed]`
- Every packet starts with a **u16 big-endian type**. Bit 15 (`0x8000`) of that word is the **reliable flag**, not part of the type. `[observed]`
- **All multi-byte integers are big-endian.** `[observed]` in the SYN magic (`56 ab e1 8c` = 1454104972) and every header; `[reference]` `NET_WriteInt16/32` write MSB first. Any codec written against this document must use big-endian reads and writes; an earlier design note in this repository said little-endian, and the wire evidence above is what settled it.
- **Strings** are NUL-terminated, no length prefix. `[observed]`
- **SHA-1 digests** travel as 20 raw bytes. `[observed]` The `wad_sha1sum` is chocolate's composite `W_Checksum` of the loaded WAD set, **not** the plain sha1 of the IWAD file. `[reference]`
- **Reliable packets** carry a **u8 sequence number** immediately after the type word. The sequence is per-connection, per-direction, starts at 0, and increments for each reliable packet sent. `[observed]`: committed fixtures cover seqs 0 to 2 (c2s `LAUNCH` seq `00`, c2s `GAMESTART` seq `01`, s2c SYN accept seq `00`, s2c `LAUNCH` seq `01`). The mod-256 wrap itself is `[reference]` (`& 0xff` in the pinned source); it is not covered by committed evidence.
- The receiver answers every reliable packet with `RELIABLE_ACK` whose u8 payload is the **next expected** sequence (`received + 1`). `[observed]` for the seqs above; the mod-256 arithmetic is `[reference]`.
- An unacknowledged reliable packet is resent after 1 s. `[reference]` No resend of a reliable packet was needed on lossless loopback, so this is `[unknown]` on the wire.
- **Keepalive**: a side that has sent nothing for 1 s emits a bare `KEEPALIVE`. Interval is `[reference]` (`KEEPALIVE_PERIOD 1`); the ~1 Hz rate in the lobby is a capture-time note. Bare keepalives in both directions are in the committed fixtures. `[observed]`
- **Timeout**: after 30 s of receive-silence (`[reference]` `CONNECTION_TIMEOUT_LEN 30`) a connection is marked disconnected with reason "timeout", silently, with **no** `DISCONNECT` packet to the dead peer. `[observed]` in the committed fixtures only as ordering (traffic to the killed client stops before the drone's DISCONNECT); the ~34 s interval is a capture-time note.

## 2. Connection lifecycle

Observed end-to-end in `gamestart-gamedata`:

1. `SYN` (c2s), then `SYN` accept (s2c, reliable), then `WAITING_DATA` (s2c), then `RELIABLE_ACK` (c2s).
2. Lobby: the server re-sends `WAITING_DATA` on a 1 s cadence (section 7); both sides keepalive.
3. The controller client sends `LAUNCH` (c2s, reliable); the server broadcasts `LAUNCH` (s2c, reliable, u8 player count).
4. The controller sends `GAMESTART` (c2s, reliable, settings); the server marks it ready and, when all nodes are ready, broadcasts `GAMESTART` (s2c, reliable, authoritative settings).
5. In game: `GAMEDATA` both ways, `GAMEDATA_ACK` (c2s), `GAMEDATA_RESEND` both ways on gaps.
6. Teardown paths `[observed]`:
- Client process killed: nothing on the wire; the server drops it after the receive timeout (30 s `[reference]`; ~34 s in the raw log is a capture-time note) and broadcasts a `CONSOLE_MESSAGE` to the remaining clients.
- Last player gone: the server sends `DISCONNECT` to remaining clients (drones), which reply `DISCONNECT_ACK`.

Signal behavior: chocolate installs **no** signal handlers and sets `SDL_HINT_NO_SIGNAL_HANDLERS=1`; SIGTERM and SIGINT kill both binaries silently. `[observed, reference]` Consequence for captures: a clean client-initiated `DISCONNECT` is only reachable through the in-game UI, so headless captures observe the server-initiated path instead.

## 3. Packet inventory

Type is the low 15 bits of the header word. `[observed]` marks sessions where the type appears in committed fixtures. "Protocol dir" is the direction the packet is defined for (from the pinned source); where the captures cover only one direction, the observed direction is noted in the status column.

| type | name | protocol dir | reliable | status |
|---:|---|---|---|---|
| 0 | SYN | c2s request, s2c accept | s2c only | [observed] handshake-keepalive, gamestart-gamedata, drone-disconnect |
| 1 | ACK | (deprecated) | (unused in 3.x) | `[reference]` only |
| 2 | REJECTED | s2c | no | `[unknown]` (no mismatch scenario captured; layout in section 4) |
| 3 | KEEPALIVE | both | no | [observed] all sessions, both directions |
| 4 | WAITING_DATA | s2c | no | [observed] all lobby sessions |
| 5 | GAMESTART | both | yes | [observed] gamestart-gamedata, both directions |
| 6 | GAMEDATA | both | no | [observed] gamestart-gamedata, both directions |
| 7 | GAMEDATA_ACK | c2s | no | [observed] gamestart-gamedata |
| 8 | DISCONNECT | both (either end) | no | [observed] drone-disconnect, s2c only |
| 9 | DISCONNECT_ACK | both (reply) | no | [observed] drone-disconnect, c2s only |
| 10 | RELIABLE_ACK | both | no | [observed] all sessions, both directions |
| 11 | GAMEDATA_RESEND | both | no | [observed] gamestart-gamedata, both directions (organic) |
| 12 | CONSOLE_MESSAGE | s2c | yes | [observed] console-message |
| 13 | QUERY | c2s (any sender) | no | [observed] query |
| 14 | QUERY_RESPONSE | s2c | no | [observed] query |
| 15 | LAUNCH | both | yes | [observed] gamestart-gamedata, both directions |
| 16 | NAT_HOLE_PUNCH | both | no | `[unknown]` (NAT traversal is out of loopback scope) |

The pre-3.0 protocol (magic `3436803284`, per-packet magic+seq header) is rejected by 3.1.1 servers and out of scope. `[reference]`

## 4. Packet layouts

Offsets are decimal bytes from packet start. `u8/u16/u32` are big-endian; `s8/s16` two's complement; `str` is NUL-terminated; `sha1` is 20 raw bytes; `…` marks volatile content (see section 5).

### SYN (0) c2s, connect request

Observed `handshake-keepalive/000-c2s-client1-syn.bin` (107 B):

```
 0  u16  type = 0
 2  u32  magic = 0x56abe18c (1454104972)
 6  str  client version      "Chocolate Doom 3.1.1"
27  u8   num_protocols = 1
28  str  protocol[0]         "CHOCOLATE_DOOM_0"
45  u8   gamemode = 0 (shareware)
46  u8   gamemission = 0 (doom)
47  u8   lowres_turn = 0
48  u8   drone = 0
49  u8   max_players = 4 (engine MAXPLAYERS, not the net layer's 8)
50  u8   is_freedoom = 0
51  sha1 wad_sha1sum = 485fd232c51d1f9cc85815b7f717dbefee77211c …
71  sha1 deh_sha1sum = 4fbed9ba4ae5ebea957aa149b51a119aaf03160d …
91  u8   player_class = 104 … (uninitialized in doom; see section 5)
92  str  player name         "Ecstatic Ettin" …
```

### SYN (0) s2c, accept (reliable)

Observed `handshake-keepalive/001-s2c-client1-syn.bin` (41 B):

```
 0  u16  0x8000 (SYN | reliable)
 2  u8   rel_seq = 0
 3  str  server version      "Chocolate Doom 3.1.1"
24  str  negotiated protocol "CHOCOLATE_DOOM_0"
```

A rejection instead sends REJECTED (2): `u16 type` then `str reason`. `[reference]` only; no rejection scenario is captured.

### WAITING_DATA (4) s2c, lobby state, re-sent every 1 s

Observed `handshake-keepalive/002-s2c-client1-waiting_data.bin` (80 B):

```
 0  u16  type = 4
 2  u8   num_players = 1
 3  u8   num_drones = 0
 4  u8   ready_players = 0
 5  u8   max_players = 4
 6  u8   is_controller = 1
 7  s8   consoleplayer = 0
 8  str  player[0].name = "Ecstatic Ettin" …
23  str  player[0].addr = "127.0.0.1:40485" …
39  sha1 wad_sha1sum …   59  sha1 deh_sha1sum …   79  u8 is_freedoom = 0
```

Per-player name and address pairs repeat `num_players` times. The 3-node lobby sample (115 B) is in `console-message/269-…-waiting_data.bin`; the post-timeout 1-player sample (82 B) is `console-message/279-…`.

### LAUNCH (15)

- c2s, reliable, empty body: `80 0f 00` (seq 0). `[observed]` `gamestart-gamedata/004-…`.
- s2c, reliable, `u8 num_players`: `80 0f 01 01`. `[observed]` `gamestart-gamedata/006-…`. Only the controller may launch. `[reference]`

### GAMESTART (5), reliable, game settings

Observed `gamestart-gamedata/008-c2s-client1-gamestart.bin` (24 B); the s2c broadcast (`011-…`) is byte-identical except rel_seq (`02`):

```
 0  u16  0x8005 (GAMESTART | reliable)
 2  u8   rel_seq
 3  u8   ticdup = 1          4  u8  extratics = 1
 5  u8   deathmatch = 0      6  u8  nomonsters = 0
 7  u8   fast_monsters = 0   8  u8  respawn_monsters = 0
 9  u8   episode = 1        10  u8  map = 1
11  u8   skill = 2 (medium) 12  u8  gameversion = 5 (exe_doom_1_9)
13  u8   lowres_turn = 0    14  u8  new_sync = 1
15  u32  timelimit = 0
19  s8   loadgame = -1     20  u8  random = 64 … (strife-only; see section 5)
21  u8   num_players = 1   22  s8  consoleplayer = 0
23  u8   player_classes[0] = 120 … (uninitialized in doom; see section 5)
```

The client proposes; the server validates and broadcasts the authoritative copy with `num_players`, `consoleplayer` (per recipient), and `player_classes` filled in. `[reference]`

### GAMEDATA (6) c2s, client ticcmds

Observed `gamestart-gamedata/029-c2s-client1-gamedata.bin` (8 B: `00 06 0b 00 01 00 00 00`, one tic, zero diff):

```
 0  u16  type = 6
 2  u8   ack = recvwindow_start low byte (piggybacked GAMEDATA_ACK)
 3  u8   start tic low byte
 4  u8   num tics
 5  …    per tic: s16 latency, then ticcmd diff (below)
```

### GAMEDATA (6) s2c, server fan-out

Observed `gamestart-gamedata/013-s2c-client1-gamedata.bin` (10 B):

```
 0  u16  type = 6
 2  u8   start tic low byte
 3  u8   num tics
 4  …    per tic: s16 latency, u8 playeringame bitmask (bit i = player i),
         then ticcmd diff per active player
```

### ticcmd diff (inside GAMEDATA) `[reference]` + partial `[observed]`

u8 bitmask selecting which fields follow (1=forward, 2=side, 4=turn, 8=buttons, 16=consistancy, 32=chatchar, 64=raven lookfly/arti, 128=strife). Fields are u8/s8 in that order, except turn which is s16 (s8 times 256 when lowres_turn). The zero diff `00` (no change vs previous tic) dominates the idle captures; richer diffs are `[unknown]` in committed fixtures.

### GAMEDATA_ACK (7) c2s

`u16 type`, `u8 ack = recvwindow_start low byte`. `[observed]` `gamestart-gamedata/028-…`.

### GAMEDATA_RESEND (11)

`u16 type`, `u32 start tic`, `u8 num tics`. `[observed]` s2c `gamestart-gamedata/024-…` (`00 0b 00000000 06`, requesting tics 0 to 5) and c2s `027-…`. Emitted by both sides when the peer's window stalls; seen organically. Capture-time note: the headless client stalled ~1.2 s before its first GAMEDATA, which triggered the s2c resend.

### KEEPALIVE (3) / DISCONNECT (8) / DISCONNECT_ACK (9)

Bare `u16 type`, no body. `[observed]` (DISCONNECT s2c `drone-disconnect/178-…`, DISCONNECT_ACK c2s `…/179-…`). A local, orderly disconnect sends DISCONNECT retried at 1 s up to 5 times until DISCONNECT_ACK. `[reference]`

### RELIABLE_ACK (10)

`u16 type`, `u8 next_expected_seq`. `[observed]` both directions (for example `gamestart-gamedata/003-…` = `00 0a 01`).

### CONSOLE_MESSAGE (12) s2c, reliable

`u16 0x800c`, `u8 rel_seq`, `str message`. `[observed]` `console-message/275-…` and `276-…` (56 B): "Client 'Aggressive Demon' timed out and disconnected", broadcast to every still-connected client. A client that is itself being force-disconnected in the same tick never sees it: the disconnect pre-empts the reliable queue. `[observed]` (drone-disconnect contains no CONSOLE_MESSAGE for exactly this reason).

### QUERY (13) / QUERY_RESPONSE (14)

`chocolate-doom -query 127.0.0.1` sends a bare `u16 13`; the response is `[observed]` `query/001-…` (61 B):

```
 0  u16  type = 14
 2  str  version            "Chocolate Doom 3.1.1"
23  u8   server_state = 0 (waiting for launch)
24  u8   num_players = 0
25  u8   max_players = 8 (NET_MAXPLAYERS before any client caps it)
26  u8   gamemode = 4 (indetermined; no client has connected)
27  u8   gamemission = 0 (doom)
28  str  description        "Unnamed server"
44  u8   num_protocols = 1
45  str  protocol[0]        "CHOCOLATE_DOOM_0"
```

## 5. Volatile fields: do not assert on these

- **player name**: when the config has none, the client invents a random pet name ("Ecstatic Ettin", "Baby Demon", …). SYN and WAITING_DATA lengths vary with it. `[observed]`
- **player_class (SYN) and player_classes (GAMESTART)**: doom never initializes them (hexen-only code); observed values `104` and `120` are stack garbage. The server echoes them. `[observed, reference]`
- **random (GAMESTART)**: strife-only; not set by doom. `[reference]`
- **rel_seq, ack bytes, tic numbers**: depend on session timing.
- **addr strings in WAITING_DATA**: ephemeral ports.
- **wad/deh sha1**: stable for a fixed IWAD and deh set, but it is chocolate's composite checksum, not a file hash. `[reference]`
- **latency (s16) in GAMEDATA**: measured ping.

The fixture tests therefore assert structure (header, length, hash of the committed bytes), never re-generated equality: re-running the rig produces equivalent sessions with different volatile bytes.

## 6. WebSocket transport envelope (browser path)

This envelope is **not** part of the Chocolate packet format: it is a transport wrapper added and removed around the packets of section 4, and the wrapped bytes are unchanged. It is not observable in these native UDP captures, so this section is `[reference]` at source level, verified against both pinned upstream sources (engine module and router). On-the-wire verification against a live router is `[unknown]`: current coverage is source-level only.

Pins for this evidence:

- `cloudflare/doom-wasm` `main` @ `65e0d3ae2ffa604155eebd96ed40da6567bd08f4`, `src/net_websockets.c`
- `cloudflare/doom-workers` `main` @ `22d8665f75017c4e1971d7e93567237645916ba1`, `router/index.mjs`

Frame shapes (each binary WebSocket message):

- **Engine to router**: `[to: u32 LE][from: u32 LE][Chocolate packet…]`, minimum 8 bytes. The router reads `to = u32(data[0..4])` and `from = u32(data[4..8])` and routes by the `to` id.
- **Router to engine**: `[from: u32 LE][Chocolate packet…]`, minimum 4 bytes. The router forwards `data.slice(4)`, so it strips only the `to` field. The engine's receive callback strips that `from` prefix (`numBytes - 4`, payload at offset 4) before handing the Chocolate packet to the net layer. Hence the asymmetry: 8-byte header c2s, 4-byte header s2c.
- The u32 ids are host-order values on little-endian infrastructure (engine: `memcpy` on wasm32; router: `Uint32Array` over the frame), so they are **little-endian**, unlike the big-endian packet payload they wrap. Ids are per-instance (`instanceUID`), assigned by the hosting side.
- **Registration/reset**: an in-WASM server role announces itself with an 8-byte frame `to = 0`, `from = instanceUID` and an empty payload (sent by the engine's server-init). The pinned router special-cases `from == 1 && to == 0` as a server restart: it closes every session and clears its client table. Instance id 1 is the server role by that router's convention.

## 7. Timing and reliability parameters

| parameter | value | status |
|---|---|---|
| keepalive period | 1 s of send-idle | value [reference] (`KEEPALIVE_PERIOD 1`); presence both directions [observed]; ~1 Hz rate is a capture-time note |
| connection timeout | 30 s of receive-silence | value [reference] (`CONNECTION_TIMEOUT_LEN 30`); stop-then-drop ordering [observed]; ~33 to 34 s interval is a capture-time note |
| lobby WAITING_DATA | every 1 s | value [reference] (server's 1000 ms resend); one-per-block ordering [observed]; ~1 Hz rate is a capture-time note |
| reliable resend | after 1 s unacked | [reference] |
| reliable seq | u8, per direction | [observed] seqs 0 to 2 in committed fixtures; mod-256 wrap [reference] |
| DISCONNECT retries | 1 s, 5 times | [reference] |
| receive window | BACKUPTICS = 128 tics | [reference] |
| NET_MAXPLAYERS | 8 (net layer); doom engine caps at 4 on the wire (`max_players = 4` in SYN) | [observed, reference] |

## 8. Current boundaries

- REJECTED is not in any committed capture: producing it needs a version, mission, or capacity mismatch, and none of those is producible with a single shareware IWAD and unmodified binaries. Its layout in section 4 is `[reference]` only.
- Clean client-initiated DISCONNECT (menu quit) is not captured: it requires UI input, which the headless rig cannot produce. The same applies to NAT_HOLE_PUNCH, which needs real NAT.
- Rich ticcmd diffs (movement and buttons), ticdup above 1, extratics, and deathmatch flags in GAMESTART are not covered: every committed fixture is an idle single-player co-op run.
- Multi-player GAMESTART (`num_players > 1`, per-client `consoleplayer`) is not covered: committed fixtures carry single-player GAMESTART only.
- RESEND under real loss and reliable resend on the wire are not observed: the loopback path is lossless and the rig has no loss-injecting scenario.
- The WebSocket envelope of section 6 is verified at source level only; it has not been observed on a live router.
