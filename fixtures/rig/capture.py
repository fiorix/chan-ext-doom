#!/usr/bin/env python3
"""Loopback packet-capture rig for the Chocolate Doom wire protocol.

Spins up a real ``chocolate-server`` and ``chocolate-doom`` client on
127.0.0.1 with a small UDP relay in between. The rig relies on
chocolate's default UDP port (2342, ``DEFAULT_PORT``); the relay binds
that port and forwards to the server on a scratch port. (Clients can
override the default with ``-port`` or a ``host:port`` connect address;
the rig simply does not need it.) Every datagram is logged in arrival
order with direction and payload.

Subcommands:

  run      capture one session into a scratch directory
  export   curate a captured session into the fixtures tree and
           regenerate fixtures/manifest.json
  craft    build a documented probe packet (e.g. a SYN with chosen
           gamemode/gamemission) for injection with ``run --inject``

Python 3 standard library only; no third-party dependencies.

Golden-fixture hygiene: raw captures (packets.jsonl, process logs) stay
in scratch. Only the exported .bin packet payloads, a per-session
session.json, and the derived manifest.json are meant to be committed.
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import selectors
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

# Packet type names, mirroring net_packet_type_t in Chocolate Doom's
# net_defs.h (reference only; the numeric values are what the wire shows).
PACKET_TYPES = {
    0: "SYN",
    1: "ACK",  # deprecated
    2: "REJECTED",
    3: "KEEPALIVE",
    4: "WAITING_DATA",
    5: "GAMESTART",
    6: "GAMEDATA",
    7: "GAMEDATA_ACK",
    8: "DISCONNECT",
    9: "DISCONNECT_ACK",
    10: "RELIABLE_ACK",
    11: "GAMEDATA_RESEND",
    12: "CONSOLE_MESSAGE",
    13: "QUERY",
    14: "QUERY_RESPONSE",
    15: "LAUNCH",
    16: "NAT_HOLE_PUNCH",
}

RELIABLE_BIT = 0x8000

# Chocolate Doom's hard-coded default UDP port (client side cannot
# override it), and the scratch port the test server listens on.
DEFAULT_PORT = 2342
SERVER_PORT = 2343

LOOPBACK = "127.0.0.1"

# Pinned capture source. Keep in sync with docs/verification.md.
SOURCE = {
    "project": "chocolate-doom",
    "url": "https://github.com/chocolate-doom/chocolate-doom.git",
    "tag": "chocolate-doom-3.1.1",
    "commit": "410d96855b5df5410ff591a90efeafa889119224",
}

TOOLCHAIN = {
    "platform": "linux x86_64 (Ubuntu 26.04)",
    "sdl2": "2.32.10 (built from source, scratch prefix)",
    "sdl2_net": "2.2.0 (built from source, scratch prefix)",
    "configure_flags": "--disable-sdl2mixer --without-libsamplerate --disable-doc",
}

SHAREWARE_IWAD_SHA1 = "5b2e249b9c5133ec987b3ea77596381dc0d6bc1d"

# Session names become directory names under the fixtures root. Keep them
# on a narrow portable alphabet so a crafted or accidental name can never
# escape the fixtures tree (no separators, no dots, no leading dash).
SESSION_NAME_RE = re.compile(r"[a-z0-9][a-z0-9-]*")


def check_session_name(name):
    """Return the name if valid, else raise ValueError."""
    if not SESSION_NAME_RE.fullmatch(name):
        raise ValueError(
            f"invalid session name {name!r}: must match [a-z0-9][a-z0-9-]*"
        )
    return name


def session_name_arg(name):
    """argparse type for --name: reject bad names before anything runs."""
    try:
        return check_session_name(name)
    except ValueError as e:
        raise argparse.ArgumentTypeError(str(e)) from e


def export_destination(fixtures_dir, name):
    """Resolve the per-session export dir and prove it stays inside the
    fixtures root. Raises ValueError otherwise; nothing is mutated."""
    check_session_name(name)
    root = os.path.realpath(fixtures_dir)
    dest = os.path.realpath(os.path.join(root, name))
    if os.path.dirname(dest) != root:
        raise ValueError(
            f"export destination {dest!r} escapes fixtures root {root!r}"
        )
    return dest


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def parse_header(payload):
    """Decode the 2-byte big-endian packet header every packet starts with.

    Returns (type, reliable). This is the whole per-packet framing; there
    is no per-packet magic or length field in the connected phase.
    """
    if len(payload) < 2:
        return None, False
    v = (payload[0] << 8) | payload[1]
    return v & ~RELIABLE_BIT, bool(v & RELIABLE_BIT)


class Relay:
    """Bidirectional UDP relay that logs every datagram in order.

    Handles multiple clients: each new source address on the well-known
    port gets its own upstream socket, so the server sees each client as
    a distinct address. Packets are tagged with the peer name of the
    client involved (client1, client2, ... in join order).
    """

    def __init__(self, server_port):
        self.down = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.down.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.down.bind((LOOPBACK, DEFAULT_PORT))
        self.server_addr = (LOOPBACK, server_port)
        self.upstreams = {}  # client addr -> upstream socket
        self.peers = {}  # client addr -> peer name
        self.packets = []
        self.t0 = time.monotonic()
        self.sel = selectors.DefaultSelector()
        self.sel.register(self.down, selectors.EVENT_READ, "c2s")

    def poll(self, timeout):
        """Pump pending datagrams for up to `timeout` seconds."""
        for key, _ in self.sel.select(timeout):
            if key.data == "c2s":
                payload, addr = self.down.recvfrom(65535)
                up = self.upstreams.get(addr)
                if up is None:
                    up = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                    up.connect(self.server_addr)
                    self.upstreams[addr] = up
                    self.peers[addr] = f"client{len(self.upstreams)}"
                    self.sel.register(up, selectors.EVENT_READ, addr)
                up.send(payload)
                direction = "c2s"
            else:
                addr = key.data
                payload = self.upstreams[addr].recv(65535)
                self.down.sendto(payload, addr)
                direction = "s2c"
            self.packets.append(
                {
                    "i": len(self.packets),
                    "t_ms": round((time.monotonic() - self.t0) * 1000),
                    "dir": direction,
                    "peer": self.peers[addr],
                    "len": len(payload),
                    "sha256": hashlib.sha256(payload).hexdigest(),
                    "hex": payload.hex(),
                }
            )

    def close(self):
        self.sel.close()
        self.down.close()
        for up in self.upstreams.values():
            up.close()


def terminate(proc, name, grace=3.0):
    """SIGTERM, then SIGKILL after a grace period. Returns exit code."""
    if proc.poll() is not None:
        return proc.returncode
    proc.terminate()
    try:
        return proc.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        print(f"rig: {name} ignored SIGTERM, sending SIGKILL", file=sys.stderr)
        proc.kill()
        return proc.wait()


def cmd_run(args):
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)

    server_log = open(os.path.join(out, "server.log"), "w")
    server_argv = [
        args.server_bin,
        "-port",
        str(SERVER_PORT),
        "-privateserver",  # no master-server registration; loopback only
        "-netlog",
        os.path.join(out, "server.netlog"),
    ]

    # Client specs: name, extra args, start delay, signal, signal time.
    specs = [
        {
            "name": "client1",
            "extra": args.client_extra,
            "delay": 0.0,
            "signal": args.signal,
            "signal_at": args.sigterm_at,
        }
    ]
    if args.client2_extra is not None:
        specs.append(
            {
                "name": "client2",
                "extra": args.client2_extra,
                "delay": args.client2_delay,
                "signal": args.client2_signal,
                "signal_at": args.client2_signal_at,
            }
        )
    if args.client3_extra is not None:
        specs.append(
            {
                "name": "client3",
                "extra": args.client3_extra,
                "delay": args.client3_delay,
                "signal": "none",
                "signal_at": 1e9,
            }
        )

    env_base = dict(os.environ)
    clients = []
    for spec in specs:
        work = tempfile.mkdtemp(prefix=f"doom-rig-{spec['name']}-")
        env = dict(env_base)
        env["HOME"] = work
        env["SDL_VIDEODRIVER"] = "dummy"
        env["SDL_AUDIODRIVER"] = "dummy"
        argv = [
            args.client_bin,
            "-iwad",
            os.path.abspath(args.iwad),
            "-connect",
            LOOPBACK,
            "-config",
            os.path.join(work, "default.cfg"),
            "-extraconfig",
            os.path.join(work, "chocolate-doom.cfg"),
            "-savedir",
            work,
            "-netlog",
            os.path.join(out, f"{spec['name']}.netlog"),
        ] + shlex.split(spec["extra"])
        log = open(os.path.join(out, f"{spec['name']}.log"), "w")
        clients.append({**spec, "argv": argv, "env": env, "log": log, "proc": None})

    # Scheduled crafted-packet injections: (at_s, path, note). Each is sent
    # from its own scratch socket so the relay and server treat it as a new
    # client. Provenance for every injected datagram lands in session.json.
    injects = []
    for spec in args.inject:
        at_s, path, note = spec.split(":", 2)
        with open(path, "rb") as f:
            payload = f.read()
        injects.append(
            {
                "at_s": float(at_s),
                "file": os.path.abspath(path),
                "note": note,
                "payload": payload,
                "sha256": hashlib.sha256(payload).hexdigest(),
                "sock": None,
                "peer": None,
            }
        )

    t_start = time.monotonic()
    server = subprocess.Popen(
        server_argv, stdout=server_log, stderr=subprocess.STDOUT, env=env_base
    )
    time.sleep(0.5)  # let the server bind before the relay forwards
    relay = Relay(SERVER_PORT)

    deadline = t_start + args.duration
    try:
        while True:
            now = time.monotonic()
            elapsed = now - t_start
            for c in clients:
                if c["proc"] is None and elapsed >= c["delay"]:
                    c["proc"] = subprocess.Popen(
                        c["argv"], stdout=c["log"], stderr=subprocess.STDOUT, env=c["env"]
                    )
                if (
                    c["proc"] is not None
                    and not c.get("signaled")
                    and c["signal"] != "none"
                    and elapsed >= c["signal_at"]
                ):
                    c["signaled"] = True
                    if c["proc"].poll() is None:
                        sig = signal.SIGTERM if c["signal"] == "term" else signal.SIGKILL
                        c["proc"].send_signal(sig)
            for inj in injects:
                if inj["sock"] is None and elapsed >= inj["at_s"]:
                    inj["sock"] = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                    inj["sock"].sendto(inj["payload"], (LOOPBACK, DEFAULT_PORT))
                    # The relay assigns the peer name in join order.
                    inj["peer"] = f"client{len(clients) + injects.index(inj) + 1}"
            if now >= deadline:
                break
            relay.poll(min(0.25, max(0.0, deadline - now)))
    finally:
        relay.close()
        for inj in injects:
            if inj["sock"] is not None:
                inj["sock"].close()
        for c in clients:
            if c["proc"] is None:  # never started (short capture)
                c["exit_code"] = None
            else:
                c["exit_code"] = terminate(c["proc"], c["name"])
            c["log"].close()
            shutil.rmtree(c["env"]["HOME"], ignore_errors=True)
        server_rc = terminate(server, "server")
        server_log.close()

    with open(os.path.join(out, "packets.jsonl"), "w") as f:
        for p in relay.packets:
            f.write(json.dumps(p) + "\n")

    session = {
        "name": args.name,
        "description": args.description,
        "captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "source": SOURCE,
        "toolchain": TOOLCHAIN,
        "iwad": {
            "file": os.path.basename(args.iwad),
            "sha1": SHAREWARE_IWAD_SHA1,
            "note": "shareware doom1.wad; used at capture time, never committed",
        },
        "server_argv": server_argv,
        "clients": [
            {
                "name": c["name"],
                "argv": c["argv"],
                "env": {"SDL_VIDEODRIVER": "dummy", "SDL_AUDIODRIVER": "dummy"},
                "start_delay_s": c["delay"],
                "signal": c["signal"],
                "signal_at_s": c["signal_at"],
                "exit_code": c["exit_code"],
            }
            for c in clients
        ],
        "duration_s": args.duration,
        "packet_count": len(relay.packets),
        "server_exit_code": server_rc,
        "injects": [
            {
                "at_s": inj["at_s"],
                "file": inj["file"],
                "sha256": inj["sha256"],
                "peer": inj["peer"],
                "note": inj["note"],
            }
            for inj in injects
        ],
    }
    with open(os.path.join(out, "session.json"), "w") as f:
        json.dump(session, f, indent=2)
        f.write("\n")

    print(f"rig: captured {len(relay.packets)} datagrams into {out}")
    for c in clients:
        print(f"rig: {c['name']} rc={c['exit_code']}")
    print(f"rig: server rc={server_rc}")
    return 0


def packet_file_name(i, direction, peer, type_name):
    return f"{i:03d}-{direction}-{peer}-{type_name.lower()}.bin"


def cmd_export(args):
    session_dir = os.path.abspath(args.session)
    fixtures_dir = os.path.abspath(args.fixtures_dir)
    with open(os.path.join(session_dir, "session.json")) as f:
        session = json.load(f)
    packets = []
    with open(os.path.join(session_dir, "packets.jsonl")) as f:
        for line in f:
            packets.append(json.loads(line))

    start, end = 0, len(packets)
    if args.range:
        selected = []
        for part in args.range.split(","):
            a, b = (int(x) for x in part.split(":"))
            selected.extend(packets[a:b])
    else:
        selected = packets
    if not selected:
        print("rig: empty selection, nothing to export", file=sys.stderr)
        return 1

    name = session["name"]
    try:
        dest = export_destination(fixtures_dir, name)
    except ValueError as e:
        print(f"rig: refusing to export: {e}", file=sys.stderr)
        return 1
    if os.path.isdir(dest):
        shutil.rmtree(dest)
    os.makedirs(dest)

    entries = []
    for p in selected:
        payload = bytes.fromhex(p["hex"])
        ptype, reliable = parse_header(payload)
        type_name = PACKET_TYPES.get(ptype, f"UNKNOWN-{ptype}")
        peer = p.get("peer", "client1")
        fname = packet_file_name(p["i"], p["dir"], peer, type_name)
        with open(os.path.join(dest, fname), "wb") as f:
            f.write(payload)
        entries.append(
            {
                "i": p["i"],
                "dir": p["dir"],
                "peer": peer,
                "type": ptype,
                "type_name": type_name,
                "reliable": reliable,
                "file": f"{name}/{fname}",
                "length": len(payload),
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
        )

    # The per-session metadata travels with the fixtures so the manifest
    # can always be regenerated from the tree alone.
    session_out = dict(session)
    session_out["exported_range"] = args.range or "all"
    with open(os.path.join(dest, "session.json"), "w") as f:
        json.dump(session_out, f, indent=2)
        f.write("\n")

    write_manifest(fixtures_dir)
    print(f"rig: exported {len(entries)} packets to {dest}")
    return 0


def load_tree_sessions(fixtures_dir):
    """All sessions curated into the fixtures tree, with packet listings."""
    sessions = []
    for name in sorted(os.listdir(fixtures_dir)):
        sdir = os.path.join(fixtures_dir, name)
        sjson = os.path.join(sdir, "session.json")
        if not os.path.isdir(sdir) or not os.path.exists(sjson):
            continue
        with open(sjson) as f:
            session = json.load(f)
        entries = []
        for fname in sorted(os.listdir(sdir)):
            if not fname.endswith(".bin"):
                continue
            i_s, direction, peer, _type_part = fname[:-4].split("-", 3)
            path = os.path.join(sdir, fname)
            with open(path, "rb") as f:
                payload = f.read()
            ptype, reliable = parse_header(payload)
            entries.append(
                {
                    "i": int(i_s),
                    "dir": direction,
                    "peer": peer,
                    "type": ptype,
                    "type_name": PACKET_TYPES.get(ptype, f"UNKNOWN-{ptype}"),
                    "reliable": reliable,
                    "file": f"{name}/{fname}",
                    "length": len(payload),
                    "sha256": hashlib.sha256(payload).hexdigest(),
                }
            )
        sessions.append((session, entries))
    return sessions


def write_manifest(fixtures_dir):
    """Regenerate fixtures/manifest.json from the curated tree.

    Packet entries are emitted one JSON object per line on purpose: the
    doom-proto fixture test parses this file with std-only string scans
    (no serde dependency), so the line-oriented shape is part of the
    contract and is covered by that test.
    """
    sessions = load_tree_sessions(fixtures_dir)
    lines = ["{"]
    lines.append('  "manifest_version": 1,')
    lines.append('  "generated_by": "fixtures/rig/capture.py export",')
    lines.append(
        '  "note": "packet entries are one JSON object per line by design '
        '(std-only parse in doom-proto tests)",'
    )
    lines.append(f'  "source": {json.dumps(SOURCE)},')
    lines.append(f'  "toolchain": {json.dumps(TOOLCHAIN)},')
    lines.append(
        '  "iwad": '
        + json.dumps(
            {
                "file": "doom1.wad",
                "sha1": SHAREWARE_IWAD_SHA1,
                "note": "shareware IWAD used at capture time; never committed",
            }
        )
        + ","
    )
    lines.append('  "sessions": [')
    for si, (session, entries) in enumerate(sessions):
        lines.append("    {")
        lines.append(f'      "name": {json.dumps(session["name"])},')
        lines.append(f'      "description": {json.dumps(session["description"])},')
        lines.append(f'      "captured_at": {json.dumps(session["captured_at"])},')
        lines.append(f'      "server_argv": {json.dumps(session["server_argv"])},')
        lines.append(f'      "clients": {json.dumps(session.get("clients", []))},')
        lines.append('      "packets": [')
        for ei, e in enumerate(entries):
            comma = "," if ei + 1 < len(entries) else ""
            lines.append(f"        {json.dumps(e)}{comma}")
        lines.append("      ]")
        lines.append("    }" + ("," if si + 1 < len(sessions) else ""))
    lines.append("  ]")
    lines.append("}")
    path = os.path.join(fixtures_dir, "manifest.json")
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")
    # Sanity: the file we just wrote must be valid JSON.
    with open(path) as f:
        json.load(f)


def cmd_craft(args):
    """Build a documented probe packet (currently: a SYN with chosen
    gamemode/gamemission). The layout is exactly the c2s SYN documented
    in docs/protocol.md section 4, so the server's parser walks it like
    a real client's packet and only the chosen fields differ."""
    wad_sha1 = bytes(20)
    deh_sha1 = bytes(20)
    if args.checksums_from:
        with open(args.checksums_from, "rb") as f:
            ref = f.read()
        # Documented SYN offsets: connect data starts at 45, wad sha1 at
        # 51, deh sha1 at 71 (see docs/protocol.md section 4).
        wad_sha1 = ref[51:71]
        deh_sha1 = ref[71:91]
        if len(wad_sha1) != 20 or len(deh_sha1) != 20:
            print("rig: --checksums-from file is not a c2s SYN", file=sys.stderr)
            return 1

    def u16be(v):
        return bytes([(v >> 8) & 0xFF, v & 0xFF])

    def u32be(v):
        return bytes(
            [(v >> 24) & 0xFF, (v >> 16) & 0xFF, (v >> 8) & 0xFF, v & 0xFF]
        )

    def z(s):
        return s.encode("ascii") + b"\x00"

    pkt = b"".join(
        [
            u16be(0),
            u32be(1454104972),  # NET_MAGIC_NUMBER
            z("Chocolate Doom 3.1.1"),
            b"\x01",
            z("CHOCOLATE_DOOM_0"),
            bytes(
                [
                    args.gamemode & 0xFF,
                    args.gamemission & 0xFF,
                    0,  # lowres_turn
                    0,  # drone
                    4,  # max_players
                    0,  # is_freedoom
                ]
            ),
            wad_sha1,
            deh_sha1,
            b"\x00",  # player_class
            z(args.player_name),
        ]
    )
    with open(args.out, "wb") as f:
        f.write(pkt)
    print(f"rig: crafted SYN ({len(pkt)} bytes) -> {args.out}")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    pr = sub.add_parser("run", help="capture one session into scratch")
    pr.add_argument(
        "--name",
        required=True,
        type=session_name_arg,
        help="session name / fixture dir ([a-z0-9][a-z0-9-]*)",
    )
    pr.add_argument("--description", default="", help="one-line session summary")
    pr.add_argument("--server-bin", required=True)
    pr.add_argument("--client-bin", required=True)
    pr.add_argument("--iwad", required=True)
    pr.add_argument("--out", required=True, help="scratch output dir")
    pr.add_argument(
        "--client-extra",
        default="",
        help="extra client1 args, e.g. '-nodes 1' to autostart a game",
    )
    pr.add_argument(
        "--client2-extra",
        default=None,
        help="add a second client with these extra args, e.g. '-drone'",
    )
    pr.add_argument("--client2-delay", type=float, default=1.5)
    pr.add_argument(
        "--client2-signal", choices=["term", "kill", "none"], default="none"
    )
    pr.add_argument("--client2-signal-at", type=float, default=1e9)
    pr.add_argument(
        "--client3-extra",
        default=None,
        help="add a third client with these extra args (never signaled)",
    )
    pr.add_argument("--client3-delay", type=float, default=3.0)
    pr.add_argument("--duration", type=float, default=9.0)
    pr.add_argument(
        "--signal",
        choices=["term", "kill", "none"],
        default="term",
        help="signal sent to the client at --sigterm-at (chocolate installs no "
        "signal handlers: SIGTERM kills silently, same as SIGKILL)",
    )
    pr.add_argument(
        "--sigterm-at",
        type=float,
        default=6.0,
        help="seconds after start to signal the client",
    )
    pr.add_argument(
        "--inject",
        action="append",
        default=[],
        metavar="AT:FILE:NOTE",
        help="send FILE's bytes as a crafted datagram AT seconds in "
        "(repeatable); NOTE goes into session.json provenance",
    )
    pr.set_defaults(func=cmd_run)

    pe = sub.add_parser("export", help="curate a capture into the fixtures tree")
    pe.add_argument("--session", required=True, help="scratch session dir")
    pe.add_argument("--fixtures-dir", required=True)
    pe.add_argument(
        "--range",
        default=None,
        metavar="START:END[,START:END...]",
        help="packet index range(s) to export, comma-separated "
        "(default: all). Indices come from the capture's packets.jsonl "
        "and drift between runs — inspect before exporting.",
    )
    pe.set_defaults(func=cmd_export)

    pc = sub.add_parser(
        "craft", help="build a documented probe packet for --inject"
    )
    pc.add_argument("--kind", choices=["syn"], required=True)
    pc.add_argument("--gamemode", type=int, default=0)
    pc.add_argument("--gamemission", type=int, default=0)
    pc.add_argument("--player-name", default="RigProbe")
    pc.add_argument(
        "--checksums-from",
        default=None,
        help="a real c2s SYN fixture to copy the wad/deh sha1 fields from, "
        "so only the chosen fields differ from a real client packet",
    )
    pc.add_argument("--out", required=True)
    pc.set_defaults(func=cmd_craft)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
