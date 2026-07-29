//! CLI evidence for `doomd serve` (task-lead-server-15 item 9): the
//! repeatable `--udp ROOM=ADDR` flag, actual bound-port reporting,
//! spec/conflict rejection before serving, and no-UDP backward
//! compatibility, driven against the real binary. Every child is
//! bounded and reaped: rejection paths must exit before a deadline and
//! serving paths are killed and waited by a guard, even on panic.

use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::panic::catch_unwind;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn doomd(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_doomd"));
    command.args(args);
    command
}

fn spawn(args: &[&str]) -> Child {
    doomd(args)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("doomd spawns")
}

/// Kill + wait on drop: no serving child survives a failed assertion.
struct ChildGuard(Child);

impl ChildGuard {
    fn spawn(args: &[&str]) -> Self {
        Self(spawn(args))
    }
}

impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Collect up to `count` startup lines from the server's stderr with a
/// bounded wait; a reader thread keeps the pipe from filling.
fn startup_lines(child: &mut Child, count: usize) -> Vec<String> {
    let stderr = child.stderr.take().expect("stderr is piped");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut lines = Vec::new();
    while lines.len() < count && Instant::now() < deadline {
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => lines.push(line),
            Err(_) => break,
        }
    }
    lines
}

/// The port a startup line advertises: the `host:port` authority after
/// the scheme, before any path or room suffix.
fn advertised_port(line: &str, scheme: &str) -> u16 {
    let after = line.split(scheme).nth(1).expect("scheme prefix present");
    let authority = after.split_whitespace().next().expect("authority present");
    let authority = authority.split('/').next().expect("authority host");
    authority
        .rsplit(':')
        .next()
        .expect("port segment")
        .parse()
        .expect("numeric port")
}

/// Kernel-assigned probe sockets held simultaneously while their
/// addresses are recorded, so requested distinct addresses are
/// distinct by construction (two sockets bound at once can never share
/// a port). The caller drops the guard to release the ports
/// immediately before spawning the child; no fixed test port is ever
/// assumed free.
struct ProbeSockets(Vec<std::net::UdpSocket>);

impl ProbeSockets {
    fn bind(count: usize) -> Self {
        Self(
            (0..count)
                .map(|_| std::net::UdpSocket::bind("127.0.0.1:0").expect("bind a probe socket"))
                .collect(),
        )
    }

    fn addrs(&self) -> Vec<SocketAddr> {
        self.0
            .iter()
            .map(|socket| socket.local_addr().expect("probe address"))
            .collect()
    }
}

/// Bounded rejection: the command must exit on its own before the
/// deadline with the needle in its stderr. A wrongly accepted command
/// keeps serving; that fails fast — the child is killed and waited and
/// the panic names the args — instead of hanging the suite. Returns
/// the captured stderr for further assertions. The child rides in a
/// `ChildGuard` from spawn, so a panic at the poll, the stderr read,
/// or either assertion is still reaped; the explicit kill/wait in the
/// wrongly-accepted branch is safely idempotent under Drop (`kill`
/// errors are ignored and `wait` returns the cached status).
fn run_rejects(args: &[&str], needle: &str) -> String {
    let mut child = ChildGuard::spawn(args);
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait().expect("poll") {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                child.kill().expect("killed");
                let _ = child.wait();
                panic!("{args:?} was wrongly accepted and kept serving; killed instead of hanging");
            }
        }
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("stderr reads");
    assert!(
        !status.success(),
        "expected rejection of {args:?}: {stderr}"
    );
    assert!(
        stderr.contains(needle),
        "expected {needle:?} in the rejection output: {stderr}"
    );
    stderr
}

#[test]
fn udp_repeatable_and_port_zero_reports_actual_ports() {
    let mut child = ChildGuard::spawn(&[
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--udp",
        "arena=127.0.0.1:0",
        "--udp",
        "arena=127.0.0.1:0",
    ]);
    let lines = startup_lines(&mut child, 3);
    assert!(
        child.try_wait().expect("poll").is_none(),
        "still serving with two listeners on one room"
    );

    assert_eq!(
        lines.len(),
        3,
        "one ws and two udp startup lines: {lines:?}"
    );
    let ws = lines
        .iter()
        .find(|line| line.contains("ws://"))
        .expect("ws line");
    assert!(ws.contains("/ws/<room>"));
    assert_ne!(advertised_port(ws, "ws://"), 0, "the actual ws port");
    let udp: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("udp://"))
        .collect();
    assert_eq!(udp.len(), 2, "both listeners report");
    let first = advertised_port(udp[0], "udp://");
    let second = advertised_port(udp[1], "udp://");
    assert!(first != 0 && second != 0, "actual ports, not zero");
    assert_ne!(first, second, "each listener binds its own port");
    assert!(
        udp.iter().all(|line| line.contains("room arena")),
        "each line names its pinned room"
    );
}

#[test]
fn one_port_zero_udp_and_port_zero_ws_listen_serves() {
    // No concrete address exists on either side, so there is nothing
    // to conflict: the WS-conflict exemption in isolation.
    let mut child = ChildGuard::spawn(&[
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--udp",
        "arena=127.0.0.1:0",
    ]);
    let lines = startup_lines(&mut child, 2);
    assert!(child.try_wait().expect("poll").is_none(), "still serving");

    assert_eq!(lines.len(), 2, "one ws and one udp startup line: {lines:?}");
    let ws = lines
        .iter()
        .find(|line| line.contains("ws://"))
        .expect("ws line");
    let udp = lines
        .iter()
        .find(|line| line.contains("udp://"))
        .expect("udp line");
    assert_ne!(advertised_port(ws, "ws://"), 0, "the actual ws port");
    assert_ne!(advertised_port(udp, "udp://"), 0, "the actual udp port");
    assert!(udp.contains("room arena"), "the line names its pinned room");
}

#[test]
fn same_room_listeners_on_two_distinct_concrete_addresses_serve() {
    // Both probe sockets are bound at once: the two concrete addresses
    // are distinct by construction, then released immediately before
    // the child needs them.
    let probes = ProbeSockets::bind(2);
    let addrs = probes.addrs();
    let (first, second) = (addrs[0], addrs[1]);
    assert_ne!(first, second, "simultaneously bound probes differ");
    drop(probes);
    let first_spec = format!("arena={first}");
    let second_spec = format!("arena={second}");
    let mut child = ChildGuard::spawn(&[
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--udp",
        first_spec.as_str(),
        "--udp",
        second_spec.as_str(),
    ]);
    let lines = startup_lines(&mut child, 3);
    assert!(child.try_wait().expect("poll").is_none(), "still serving");

    assert_eq!(
        lines.len(),
        3,
        "one ws and two udp startup lines: {lines:?}"
    );
    let udp: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("udp://"))
        .collect();
    assert_eq!(udp.len(), 2, "both listeners report");
    let ports: Vec<u16> = udp
        .iter()
        .map(|line| advertised_port(line, "udp://"))
        .collect();
    assert!(
        ports.contains(&first.port()) && ports.contains(&second.port()),
        "the two acquired concrete ports report: {ports:?}"
    );
    assert!(
        udp.iter().all(|line| line.contains("room arena")),
        "each line names its pinned room"
    );
}

#[test]
fn malformed_udp_specs_are_rejected() {
    run_rejects(&["serve", "--udp", "arena"], "expected ROOM=ADDR");
    run_rejects(
        &["serve", "--udp", "BAD NAME=127.0.0.1:9000"],
        "invalid character",
    );
    run_rejects(&["serve", "--udp", "arena=nope"], "invalid UDP address");
    run_rejects(
        &["serve", "--udp", "=127.0.0.1:9000"],
        "room name must not be empty",
    );
    run_rejects(&["serve", "--udp", "arena="], "invalid UDP address");
}

#[test]
fn duplicate_and_conflicting_binds_are_rejected() {
    // Kernel-assigned addresses even though these reject before bind:
    // the suite carries no fixed-port assumptions.
    let duplicate = {
        let probes = ProbeSockets::bind(1);
        let addr = probes.addrs()[0];
        drop(probes);
        format!("arena={addr}")
    };
    run_rejects(
        &[
            "serve",
            "--udp",
            duplicate.as_str(),
            "--udp",
            duplicate.as_str(),
        ],
        "duplicate UDP bind address",
    );
    let conflict = {
        let probes = ProbeSockets::bind(1);
        let addr = probes.addrs()[0];
        drop(probes);
        addr.to_string()
    };
    let conflict_spec = format!("arena={conflict}");
    run_rejects(
        &[
            "serve",
            "--listen",
            conflict.as_str(),
            "--udp",
            conflict_spec.as_str(),
        ],
        "conflicts with the WebSocket listen",
    );
}

#[test]
fn websocket_only_serve_prints_line_and_stays_up() {
    let mut child = ChildGuard::spawn(&["serve", "--listen", "127.0.0.1:0"]);
    let lines = startup_lines(&mut child, 1);
    assert_eq!(lines.len(), 1, "exactly the websocket startup line");
    assert!(lines[0].contains("ws://"));
    assert_ne!(advertised_port(&lines[0], "ws://"), 0);
    assert!(
        child.try_wait().expect("poll").is_none(),
        "a WebSocket-only serve persists without any --udp listener"
    );
}

#[test]
fn udp_bind_failure_prints_no_websocket_startup_line() {
    // A foreign process holds the concrete port: the kernel rejects
    // what CLI validation cannot see, and no startup line may precede
    // the failure. The occupying socket is bound directly at port zero
    // and held continuously through the child's rejection.
    let occupied = std::net::UdpSocket::bind("127.0.0.1:0").expect("preoccupy the port");
    let spec = format!("arena={}", occupied.local_addr().expect("occupied address"));
    let stderr = run_rejects(
        &["serve", "--listen", "127.0.0.1:0", "--udp", spec.as_str()],
        "failed to bind UDP",
    );
    assert!(
        !stderr.contains("ws://"),
        "no websocket startup line precedes a failed bind: {stderr}"
    );
    drop(occupied);
}

#[test]
fn rejection_helper_fails_fast_on_a_wrongly_accepted_command() {
    // The harness proof: a command that wrongly keeps serving must be
    // killed and failed fast, never waited on forever.
    let started = Instant::now();
    let outcome = catch_unwind(|| {
        run_rejects(&["serve", "--listen", "127.0.0.1:0"], "never printed");
    });
    assert!(outcome.is_err(), "the wrongly accepted command fails");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the helper stayed bounded instead of hanging"
    );
}
