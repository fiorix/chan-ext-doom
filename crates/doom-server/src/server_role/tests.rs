//! Deterministic tests for the server-role state module. All clocks are
//! driven by hand; no timers, threads, or sockets exist here.

use doom_proto::{ClientPacket, ConnectData, GameSettings, Syn, WireHeader};

use crate::PlayerId;
use crate::core::{Registry, RoomName};

use super::*;

const T0: Milliseconds = Milliseconds(10_000);

fn player_id(registry: &mut Registry, room: RoomName) -> PlayerId {
    registry.join(room).expect("test admission succeeds")
}

struct Harness {
    role: ServerRole,
    registry: Registry,
    room: RoomName,
}

impl Harness {
    fn new() -> Self {
        Harness {
            role: ServerRole::new(),
            registry: Registry::default(),
            room: RoomName::try_from("e1m1").expect("valid room"),
        }
    }

    fn join(&mut self, addr: &str) -> PlayerId {
        let player = player_id(&mut self.registry, self.room.clone());
        self.role.handle(
            T0,
            Input::Join {
                player,
                addr_label: addr.as_bytes().to_vec(),
            },
        );
        player
    }

    fn syn(&mut self, player: PlayerId, name: &str) -> Vec<Action> {
        self.role.handle(
            T0,
            Input::Packet {
                player,
                header: WireHeader { reliable_seq: None },
                packet: ClientPacket::Syn(syn_value(name, 0, 0, 0)),
            },
        )
    }

    fn launch(&mut self, player: PlayerId) -> Vec<Action> {
        self.launch_seq(player, 0)
    }

    fn launch_seq(&mut self, player: PlayerId, seq: u8) -> Vec<Action> {
        self.role.handle(
            T0,
            Input::Packet {
                player,
                header: WireHeader {
                    reliable_seq: Some(seq),
                },
                packet: ClientPacket::Launch,
            },
        )
    }

    fn gamestart(&mut self, player: PlayerId, seq: u8, deathmatch: u8) -> Vec<Action> {
        self.role.handle(
            T0,
            Input::Packet {
                player,
                header: WireHeader {
                    reliable_seq: Some(seq),
                },
                packet: ClientPacket::GameStart(settings_value(deathmatch)),
            },
        )
    }

    fn tick(&mut self, ms: u64) -> Vec<Action> {
        self.role.handle(Milliseconds(T0.0 + ms), Input::Timer)
    }

    /// Acknowledge the current head: the exact-head ACK pops it and emits
    /// the next head immediately (pinned semantics).
    fn ack(&mut self, player: PlayerId, next_seq: u8) -> Vec<Action> {
        self.role.handle(
            T0,
            Input::Packet {
                player,
                header: WireHeader { reliable_seq: None },
                packet: ClientPacket::ReliableAck { next_seq },
            },
        )
    }

    fn sends_to(actions: &[Action], player: PlayerId) -> Vec<&Action> {
        actions
            .iter()
            .filter(|action| match action {
                Action::Send { player: p, .. } => *p == player,
                _ => false,
            })
            .collect()
    }

    fn reliable_sends_to(actions: &[Action], player: PlayerId) -> Vec<&Action> {
        actions
            .iter()
            .filter(|action| match action {
                Action::Send {
                    player: p, header, ..
                } => *p == player && header.reliable_seq.is_some(),
                _ => false,
            })
            .collect()
    }
}

fn syn_value(name: &str, gamemode: u8, gamemission: u8, drone: u8) -> Syn {
    Syn {
        version: b"Chocolate Doom 3.1.1".to_vec(),
        protocols: vec![b"CHOCOLATE_DOOM_0".to_vec()],
        connect: ConnectData {
            gamemode,
            gamemission,
            lowres_turn: 0,
            drone,
            max_players: 4,
            is_freedoom: 0,
            wad_sha1: [7; 20],
            deh_sha1: [8; 20],
            player_class: 0,
        },
        player_name: name.as_bytes().to_vec(),
    }
}

fn settings_value(deathmatch: u8) -> GameSettings {
    GameSettings {
        ticdup: 1,
        extratics: 1,
        deathmatch,
        nomonsters: 0,
        fast_monsters: 0,
        respawn_monsters: 0,
        episode: 1,
        map: 1,
        skill: 2,
        gameversion: 5,
        lowres_turn: 0,
        new_sync: 1,
        timelimit: 0,
        loadgame: -1,
        random: 0,
        consoleplayer: 0,
        player_classes: vec![0],
    }
}

fn is_syn_accept(action: &Action) -> bool {
    matches!(
        action,
        Action::Send {
            packet: ServerPacket::SynAccept(_),
            ..
        }
    )
}

fn is_reject_with(action: &Action, text: &[u8]) -> bool {
    match action {
        Action::Disconnect {
            terminal: Some(terminal),
            ..
        } => matches!(&terminal.1, ServerPacket::Rejected { reason } if reason.as_slice() == text),
        _ => false,
    }
}

// --- lobby and SYN ---------------------------------------------------------

#[test]
fn one_player_accept_waitdata_and_cadence() {
    let mut h = Harness::new();
    let alice = h.join("127.0.0.1:5001");
    let actions = h.syn(alice, "Alice");

    let accepts: Vec<_> = Harness::reliable_sends_to(&actions, alice);
    assert_eq!(accepts.len(), 1);
    match accepts[0] {
        Action::Send {
            header,
            packet: ServerPacket::SynAccept(accept),
            ..
        } => {
            assert_eq!(header.reliable_seq, Some(0));
            assert_eq!(accept.protocol, b"CHOCOLATE_DOOM_0");
        }
        other => panic!("expected SynAccept, got {other:?}"),
    }
    assert_eq!(h.role.controller(), Some(alice));

    // Pinned first-update behavior: the first personalized WAITING_DATA
    // leaves in the same pump as the accept, right after it.
    let first_updates: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(
                a,
                Action::Send {
                    packet: ServerPacket::WaitingData(_),
                    ..
                }
            )
        })
        .collect();
    assert_eq!(first_updates.len(), 1);

    // No further lobby update before the cadence elapses.
    assert!(h.tick(1000).is_empty());
    let actions = h.tick(1001);
    let updates: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(a, Action::Send {
            packet: ServerPacket::WaitingData(_),
            player,
            ..
        } if *player == alice)
        })
        .collect();
    assert_eq!(updates.len(), 1);
    match updates[0] {
        Action::Send {
            packet: ServerPacket::WaitingData(data),
            ..
        } => {
            assert_eq!(data.is_controller, 1);
            assert_eq!(data.consoleplayer, 0);
            assert_eq!(data.max_players, 4);
            assert_eq!(data.players.len(), 1);
            assert_eq!(data.players[0].name, b"Alice");
            assert_eq!(data.players[0].addr, b"127.0.0.1:5001");
            assert_eq!(data.wad_sha1, [7; 20]);
            assert_eq!(data.deh_sha1, [8; 20]);
        }
        other => panic!("expected WaitingData, got {other:?}"),
    }
}

#[test]
fn two_players_personalized_waitdata() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");

    let actions = h.tick(1001);
    let to_bob: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(a, Action::Send {
            packet: ServerPacket::WaitingData(_),
            player,
            ..
        } if *player == bob)
        })
        .collect();
    assert_eq!(to_bob.len(), 1);
    match &to_bob[0] {
        Action::Send {
            packet: ServerPacket::WaitingData(data),
            ..
        } => {
            assert_eq!(data.is_controller, 0);
            assert_eq!(data.consoleplayer, 1);
            assert_eq!(data.players.len(), 2);
        }
        other => panic!("expected WaitingData, got {other:?}"),
    }
    let to_alice: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(a, Action::Send {
            packet: ServerPacket::WaitingData(_),
            player,
            ..
        } if *player == alice)
        })
        .collect();
    assert_eq!(to_alice.len(), 1);
    match &to_alice[0] {
        Action::Send {
            packet: ServerPacket::WaitingData(data),
            ..
        } => assert_eq!(data.is_controller, 1),
        other => panic!("expected WaitingData, got {other:?}"),
    }
}

#[test]
fn duplicate_syn_is_dropped() {
    let mut h = Harness::new();
    let alice = h.join("a");
    assert_eq!(
        Harness::reliable_sends_to(&h.syn(alice, "Alice"), alice).len(),
        1
    );
    // Same SYN again: nothing emitted, still one accept total.
    assert!(h.syn(alice, "Alice").is_empty());
    assert_eq!(h.role.player_count(), 1);
}

#[test]
fn no_common_protocol_rejected() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let mut syn = syn_value("Alice", 0, 0, 0);
    syn.protocols = vec![b"SOME_OTHER_PROTOCOL".to_vec()];
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn),
        },
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Disconnect {
            terminal: Some(_),
            ..
        }
    )));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Disconnect { .. }))
    );
    assert_eq!(h.role.peer_count(), 0);
}

#[test]
fn old_magic_gets_source_backed_reject() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let actions = h.role.handle(
        T0,
        Input::Malformed {
            player: alice,
            class: MalformedClass::Syn { old_magic: true },
        },
    );
    assert!(actions.iter().any(|a| matches!(a, Action::Disconnect {
        terminal: Some(terminal),
        ..
    } if matches!(&terminal.1, ServerPacket::Rejected { reason }
        if reason.starts_with(b"You are using an old client version")))));
    // Other malformed classes drop silently.
    let bob = h.join("b");
    assert!(
        h.role
            .handle(
                T0,
                Input::Malformed {
                    player: bob,
                    class: MalformedClass::Syn { old_magic: false },
                },
            )
            .is_empty()
    );
}

#[test]
fn game_mismatch_rejected_with_fixture_text() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");

    let bob = h.join("b");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("RigProbe", 2, 1, 0)),
        },
    );
    assert!(actions.iter().any(|a| is_reject_with(
        a,
        b"Game mismatch: server is doom (shareware), client is doom2 (commercial)"
    )));
}

#[test]
fn server_full_players_rejected_but_drones_fit() {
    let mut h = Harness::new();
    let players: Vec<_> = (0..4).map(|_| h.join("p")).collect();
    for (index, player) in players.iter().enumerate() {
        h.role.handle(
            T0,
            Input::Packet {
                player: *player,
                header: WireHeader { reliable_seq: None },
                packet: ClientPacket::Syn(syn_value(["A", "B", "C", "D"][index], 0, 0, 0)),
            },
        );
    }
    assert_eq!(h.role.player_count(), 4);

    // Two drones fit beside the full player set.
    for _ in 0..2 {
        let drone = h.join("d");
        let actions = h.role.handle(
            T0,
            Input::Packet {
                player: drone,
                header: WireHeader { reliable_seq: None },
                packet: ClientPacket::Syn(syn_value("Observer", 0, 0, 1)),
            },
        );
        assert!(actions.iter().any(is_syn_accept));
    }

    // A fifth non-drone is full.
    let fifth = h.join("f");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: fifth,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("E", 0, 0, 0)),
        },
    );
    assert!(
        actions
            .iter()
            .any(|a| is_reject_with(a, b"Server is full!"))
    );
}

#[test]
fn invalid_connect_data_is_dropped_silently() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let mut syn = syn_value("Alice", 0, 0, 0);
    syn.connect.max_players = 9;
    assert!(
        h.role
            .handle(
                T0,
                Input::Packet {
                    player: alice,
                    header: WireHeader { reliable_seq: None },
                    packet: ClientPacket::Syn(syn),
                },
            )
            .is_empty()
    );

    let bob = h.join("b");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("Bob", 9, 9, 0)),
        },
    );
    assert!(actions.is_empty());
}

#[test]
fn drone_first_is_rejected_like_upstream() {
    // Upstream quirk: with no non-drone player adopted yet, the mode is
    // indeterminate and a drone-first connection mismatches it.
    let mut h = Harness::new();
    let drone = h.join("d");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("Observer", 0, 0, 1)),
        },
    );
    assert!(actions.iter().any(|a| matches!(a, Action::Disconnect {
        terminal: Some(terminal),
        ..
    } if matches!(&terminal.1, ServerPacket::Rejected { reason }
        if reason.starts_with(b"Game mismatch:")))));
}

// --- LAUNCH and GAMESTART ----------------------------------------------------

#[test]
fn launch_only_from_controller_and_broadcast() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");

    // Bob is not the controller: his reliable LAUNCH is acknowledged by
    // the reliable receive rule (upstream) but never processed.
    let ignored = h.launch(bob);
    assert!(ignored.iter().all(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::ReliableAck { .. },
            ..
        }
    )));
    assert_eq!(h.role.state(), ServerState::WaitingLaunch);

    let actions = h.launch(alice);
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    // Head-only delivery: the broadcast queues behind each client's
    // unacked SYN accept, so nothing is emitted to anyone yet.
    assert!(Harness::reliable_sends_to(&actions, alice).is_empty());
    assert!(Harness::reliable_sends_to(&actions, bob).is_empty());
    assert!(actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::ReliableAck { next_seq: 1 },
        player,
        ..
    } if *player == alice)));

    // The exact-head ACK pops the accept and emits LAUNCH immediately,
    // as the committed fixtures chain (accept seq 0, LAUNCH seq 1).
    let emitted = h.ack(alice, 1);
    assert_eq!(Harness::reliable_sends_to(&emitted, alice).len(), 1);
    match Harness::reliable_sends_to(&emitted, alice)[0] {
        Action::Send {
            header,
            packet: ServerPacket::Launch { num_players },
            ..
        } => {
            assert_eq!(header.reliable_seq, Some(1));
            assert_eq!(*num_players, 2);
        }
        other => panic!("expected Launch after ACK, got {other:?}"),
    }
    let emitted = h.ack(bob, 1);
    assert_eq!(Harness::reliable_sends_to(&emitted, bob).len(), 1);

    // A second LAUNCH (her next in-sequence value) is acknowledged but
    // ignored now that the state moved on: no broadcast, no transition.
    let actions = h.launch_seq(alice, 1);
    assert!(actions.iter().all(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::ReliableAck { .. },
            ..
        }
    )));
    assert_eq!(h.role.state(), ServerState::WaitingStart);
}

#[test]
fn controller_gamestart_is_authoritative_and_deathmatch_reaches_everyone() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.launch(alice);

    h.ack(alice, 1);
    h.ack(bob, 1);
    let _refresh = h.gamestart(alice, 1, 1);
    // Controller is the only ready one so far; no start yet, but a lobby
    // refresh reaches the ready peer.
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    assert!(h.role.settings.is_some());

    let _ready = h.gamestart(bob, 0, 1);
    assert_eq!(h.role.state(), ServerState::InGame);
    // The broadcast queues behind the LAUNCH head; the next ACKs release
    // it, one GAMESTART per recipient.
    let first = h.ack(alice, 2);
    let second = h.ack(bob, 2);
    let actions: Vec<_> = first.into_iter().chain(second).collect();
    let gamestarts: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(
                a,
                Action::Send {
                    packet: ServerPacket::GameStart(_),
                    ..
                }
            )
        })
        .collect();
    assert_eq!(gamestarts.len(), 2);
    for action in &gamestarts {
        match action {
            Action::Send {
                packet: ServerPacket::GameStart(settings),
                ..
            } => {
                assert_eq!(
                    settings.deathmatch, 1,
                    "deathmatch must reach every GAMESTART action"
                );
                assert_eq!(settings.ticdup, 1);
                assert_eq!(settings.skill, 2);
                assert_eq!(settings.gameversion, 5);
                assert_eq!(settings.player_classes.len(), 2);
            }
            other => panic!("expected GameStart, got {other:?}"),
        }
    }
    // consoleplayer is personalized per recipient.
    let to_alice: Vec<_> = gamestarts
        .iter()
        .filter(|a| matches!(a, Action::Send { player, .. } if *player == alice))
        .collect();
    match &to_alice[0] {
        Action::Send {
            packet: ServerPacket::GameStart(settings),
            ..
        } => assert_eq!(settings.consoleplayer, 0),
        _ => unreachable!(),
    }
    let to_bob: Vec<_> = gamestarts
        .iter()
        .filter(|a| matches!(a, Action::Send { player, .. } if *player == bob))
        .collect();
    match &to_bob[0] {
        Action::Send {
            packet: ServerPacket::GameStart(settings),
            ..
        } => assert_eq!(settings.consoleplayer, 1),
        _ => unreachable!(),
    }
}

#[test]
fn non_controller_gamestart_marks_ready_without_replacing_settings() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.launch(alice);

    // Bob (not the controller) sends different settings first.
    h.ack(alice, 1);
    h.ack(bob, 1);
    let mut other = settings_value(0);
    other.map = 2;
    h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader {
                reliable_seq: Some(0),
            },
            packet: ClientPacket::GameStart(other),
        },
    );
    assert!(
        h.role.settings.is_none(),
        "non-controller settings must not be adopted"
    );

    // Controller's settings then win.
    h.gamestart(alice, 1, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    let settings = h
        .role
        .settings
        .clone()
        .expect("controller settings adopted");
    assert_eq!(settings.map, 1);
}

#[test]
fn invalid_settings_are_not_adopted_and_not_ready() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    h.launch(alice);

    let mut bad = settings_value(0);
    bad.ticdup = 0;
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader {
                reliable_seq: Some(1),
            },
            packet: ClientPacket::GameStart(bad),
        },
    );
    // Only the reliable acknowledgement; no adoption, no readiness.
    assert!(actions.iter().all(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::ReliableAck { .. },
            ..
        }
    )));
    assert!(h.role.settings.is_none());
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    assert!(!h.role.peers.get(&alice).expect("peer").ready);
}

#[test]
fn controller_handoff_to_next_oldest() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    assert_eq!(h.role.controller(), Some(alice));

    h.role.handle(T0, Input::Leave { player: alice });
    assert_eq!(h.role.controller(), Some(bob));

    // The new controller can launch.
    let actions = h.launch(bob);
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    assert!(!actions.is_empty());
}

#[test]
fn disconnect_during_start_aborts_and_cleans_drones() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    let drone = h.join("d");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("Observer", 0, 0, 1)),
        },
    );
    h.launch(alice);
    assert_eq!(h.role.state(), ServerState::WaitingStart);

    // Drain both reliable chains so the abort broadcast can emit.
    h.ack(bob, 1);
    h.ack(bob, 2);
    h.ack(drone, 1);
    h.ack(drone, 2);

    let actions: Vec<Action> = h.role.handle(T0, Input::Leave { player: alice });
    assert!(actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::ConsoleMessage { message },
        ..
    } if message.starts_with(b"Game startup aborted because player 'Alice'"))));
    assert!(actions.iter().any(|a| matches!(a, Action::GameEnded)));
    assert_eq!(h.role.state(), ServerState::WaitingLaunch);
    // Pinned NET_SV_GameEnded: every survivor, player and drone alike,
    // starts its initiated disconnect.
    assert!(actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::Disconnect,
        player,
        ..
    } if *player == drone)));
    assert!(actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::Disconnect,
        player,
        ..
    } if *player == bob)));
}

#[test]
fn remote_disconnect_sleeps_five_seconds_with_duplicate_reack() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Disconnect,
        },
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::DisconnectAck,
            ..
        }
    )));
    // The identity lingers: no removal yet, in case the ACK was lost.
    assert_eq!(h.role.peer_count(), 1);

    // A duplicate DISCONNECT is re-acknowledged.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Disconnect,
        },
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::DisconnectAck,
            ..
        }
    )));
    assert_eq!(h.role.peer_count(), 1);

    // The identity is removed only after the sleep.
    assert!(h.tick(4999).is_empty());
    let actions = h.tick(5000);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Disconnect { .. }))
    );
    assert_eq!(h.role.peer_count(), 0);
}

// --- reliable mechanics -----------------------------------------------------

#[test]
fn reliable_first_send_is_immediate_then_retries_same_seq() {
    let mut h = Harness::new();
    let alice = h.join("a");

    // The first transmission lands in the same pump as the enqueue, as
    // upstream's same-iteration connection run does.
    let actions = h.syn(alice, "Alice");
    assert_eq!(Harness::reliable_sends_to(&actions, alice).len(), 1);
    match Harness::reliable_sends_to(&actions, alice)[0] {
        Action::Send { header, .. } => assert_eq!(header.reliable_seq, Some(0)),
        other => panic!("expected reliable send, got {other:?}"),
    }

    // No retry before the retry interval; one retry after, same sequence,
    // and the outbox still holds exactly one entry.
    assert_eq!(Harness::reliable_sends_to(&h.tick(1000), alice).len(), 0);
    let retried = h.tick(1001);
    assert_eq!(Harness::reliable_sends_to(&retried, alice).len(), 1);
    match Harness::reliable_sends_to(&retried, alice)[0] {
        Action::Send { header, .. } => assert_eq!(header.reliable_seq, Some(0)),
        other => panic!("expected reliable retry, got {other:?}"),
    }
    let peer = h.role.peers.get(&alice).expect("peer exists");
    assert_eq!(peer.reliable_outbox.len(), 1);
}

#[test]
fn reliable_ack_unlinks_only_the_exact_head() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    h.tick(1);

    // Wrong ack value: head stays.
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::ReliableAck { next_seq: 2 },
        },
    );
    assert_eq!(
        h.role
            .peers
            .get(&alice)
            .expect("peer")
            .reliable_outbox
            .len(),
        1
    );

    // Exact head ack: head unlinks.
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::ReliableAck { next_seq: 1 },
        },
    );
    assert_eq!(
        h.role
            .peers
            .get(&alice)
            .expect("peer")
            .reliable_outbox
            .len(),
        0
    );
}

#[test]
fn reliable_sequence_wraps_at_255() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");

    let peer = h.role.peers.get_mut(&alice).expect("peer");
    peer.reliable_send_seq = 255;
    peer.reliable_outbox.clear();

    let mut actions = Vec::new();
    h.role.enqueue_reliable(
        &mut actions,
        alice,
        ServerPacket::ConsoleMessage {
            message: b"wrap".to_vec(),
        },
    );
    let peer = h.role.peers.get(&alice).expect("peer");
    assert_eq!(peer.reliable_send_seq, 0);
    assert_eq!(
        peer.reliable_outbox.back().map(|entry| entry.seq),
        Some(255)
    );
}

#[test]
fn reliable_cap_64_accepted_65_removes_without_allocating() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");

    // The SYN accept already occupies one slot; clear it for a clean fill.
    h.role
        .peers
        .get_mut(&alice)
        .expect("peer")
        .reliable_outbox
        .clear();

    // Fill to the cap through the internal path (in-module test access).
    for _ in 0..RELIABLE_CAP {
        let mut sink = Vec::new();
        h.role.enqueue_reliable(
            &mut sink,
            alice,
            ServerPacket::ConsoleMessage {
                message: b"fill".to_vec(),
            },
        );
    }
    assert_eq!(
        h.role
            .peers
            .get(&alice)
            .expect("peer")
            .reliable_outbox
            .len(),
        RELIABLE_CAP
    );

    // Bob acknowledges his accept so the overflow broadcast can emit.
    h.ack(bob, 1);

    // The 65th: the peer is removed, no entry is added, and the effect is
    // a bounded broadcast to the remaining peer.
    let mut actions = Vec::new();
    h.role.enqueue_reliable(
        &mut actions,
        alice,
        ServerPacket::ConsoleMessage {
            message: b"one too many".to_vec(),
        },
    );
    assert!(actions.iter().any(|a| matches!(a, Action::Disconnect {
        player,
        reason: DisconnectReason::ReliableOverflow,
        ..
    } if *player == alice)));
    assert_eq!(h.role.peer_count(), 1);
    assert!(actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::ConsoleMessage { .. },
        player,
        ..
    } if *player == bob)));
}

#[test]
fn incoming_reliable_out_of_sequence_is_acked_but_dropped() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.launch(alice);

    // Bob's GAMESTART with seq 2 while 1 is expected: ack the expectation,
    // do not process.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader {
                reliable_seq: Some(2),
            },
            packet: ClientPacket::GameStart(settings_value(0)),
        },
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::ReliableAck { next_seq: 0 },
            ..
        }
    )));
    assert_eq!(h.role.state(), ServerState::WaitingStart);
}

// --- timers ------------------------------------------------------------------

#[test]
fn timeout_removes_and_broadcasts_only_to_survivors() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");

    // Bob acknowledges his accept so the timeout broadcast can emit.
    h.ack(bob, 1);

    // Alice goes silent for 30 s; Bob is still heard from.
    let actions = h.role.handle(
        Milliseconds(T0.0 + 30_001),
        Input::Packet {
            player: bob,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Keepalive,
        },
    );
    let mut all = actions;
    all.extend(h.tick(30_001));

    assert!(
        all.iter()
            .any(|a| matches!(a, Action::Disconnect { player, .. } if *player == alice))
    );
    let console: Vec<_> = all
        .iter()
        .filter(|a| {
            matches!(a, Action::Send {
            packet: ServerPacket::ConsoleMessage { message },
            ..
        } if message.starts_with(b"Client 'Alice' timed out"))
        })
        .collect();
    assert_eq!(console.len(), 1);
    match &console[0] {
        Action::Send { player, .. } => assert_eq!(*player, bob),
        other => panic!("expected console message, got {other:?}"),
    }
    // Nothing was sent to the dead peer.
    assert!(Harness::sends_to(&all, alice).is_empty());
}

#[test]
fn keepalive_after_one_second_send_idle() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");

    assert!(h.tick(1000).is_empty());
    let actions = h.tick(1001);
    assert!(actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::Keepalive,
        player,
        ..
    } if *player == alice)));
}

#[test]
fn initiated_disconnect_sends_five_times_then_removes() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let drone = h.join("d");
    h.syn(alice, "Alice");
    h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("Observer", 0, 0, 1)),
        },
    );

    // End the game with a drone attached.
    h.role.handle(T0, Input::Leave { player: alice });
    let mut disconnect_sends = 1; // the immediate first send in end_game
    for step in 1..10 {
        let actions = h.tick(1001 * step);
        disconnect_sends += actions
            .iter()
            .filter(|a| {
                matches!(a, Action::Send {
                packet: ServerPacket::Disconnect,
                player,
                ..
            } if *player == drone)
            })
            .count();
        if h.role.peer_count() == 0 {
            break;
        }
    }
    assert_eq!(disconnect_sends, 5, "exactly five DISCONNECT sends");
    assert_eq!(
        h.role.peer_count(),
        0,
        "the drone is removed after the fifth"
    );
}

#[test]
fn no_waitdata_cadence_once_in_game() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    h.launch(alice);
    h.gamestart(alice, 1, 0);
    assert_eq!(h.role.state(), ServerState::InGame);

    let actions = h.tick(2000);
    assert!(!actions.iter().any(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::WaitingData(_),
            ..
        }
    )));
}

// --- query -------------------------------------------------------------------

#[test]
fn query_response_reflects_room_state() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");

    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Query,
        },
    );
    match &actions[0] {
        Action::Send {
            packet: ServerPacket::QueryResponse(data),
            ..
        } => {
            assert_eq!(data.server_state, 0);
            assert_eq!(data.num_players, 2);
            assert_eq!(data.max_players, 4);
            assert_eq!(data.gamemode, 0);
            assert_eq!(data.gamemission, 0);
            assert_eq!(
                data.protocols.as_ref().expect("protocol list"),
                &vec![b"CHOCOLATE_DOOM_0".to_vec()]
            );
        }
        other => panic!("expected QueryResponse, got {other:?}"),
    }
}

// --- committed transcript replay (invariants, never volatile bytes) ----------

fn fixture_bytes(session: &str, file: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(session)
        .join(file);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn fixture_syn_drives_accept_shape() {
    let bytes = fixture_bytes("handshake-keepalive", "000-c2s-client1-syn.bin");
    let (header, packet) = ClientPacket::decode(&bytes, false).expect("fixture SYN decodes");

    let mut h = Harness::new();
    let alice = h.join("a");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header,
            packet,
        },
    );
    assert_eq!(Harness::reliable_sends_to(&actions, alice).len(), 1);
    match Harness::reliable_sends_to(&actions, alice)[0] {
        Action::Send {
            header,
            packet: ServerPacket::SynAccept(accept),
            ..
        } => {
            assert_eq!(header.reliable_seq, Some(0));
            assert!(!accept.version.is_empty());
            assert_eq!(accept.protocol, b"CHOCOLATE_DOOM_0");
        }
        other => panic!("expected SynAccept, got {other:?}"),
    }

    // The committed lobby update shape matches by invariant.
    let actions = h.tick(1001);
    let updates: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(a, Action::Send {
            packet: ServerPacket::WaitingData(_),
            player,
            ..
        } if *player == alice)
        })
        .collect();
    assert_eq!(updates.len(), 1);
    match updates[0] {
        Action::Send {
            packet: ServerPacket::WaitingData(data),
            ..
        } => {
            assert_eq!(data.is_controller, 1);
            assert_eq!(data.consoleplayer, 0);
            assert_eq!(data.max_players, 4);
            assert_eq!(data.players.len(), 1);
        }
        other => panic!("expected WaitingData, got {other:?}"),
    }
}

#[test]
fn fixture_launch_and_gamestart_replay() {
    let mut h = Harness::new();
    let alice = h.join("a");

    let (_, syn) = ClientPacket::decode(
        &fixture_bytes("gamestart-gamedata", "000-c2s-client1-syn.bin"),
        false,
    )
    .expect("fixture SYN decodes");
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: syn,
        },
    );

    let (launch_header, launch) = ClientPacket::decode(
        &fixture_bytes("gamestart-gamedata", "004-c2s-client1-launch.bin"),
        false,
    )
    .expect("fixture LAUNCH decodes");
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: launch_header,
            packet: launch,
        },
    );
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    // Head-only delivery: the broadcast queues behind the accept; the
    // exact-head ACK releases it (accept seq 0, LAUNCH seq 1).
    let emitted = h.ack(alice, 1);
    assert_eq!(Harness::reliable_sends_to(&emitted, alice).len(), 1);
    match Harness::reliable_sends_to(&emitted, alice)[0] {
        Action::Send {
            packet: ServerPacket::Launch { num_players },
            ..
        } => assert_eq!(*num_players, 1),
        other => panic!("expected Launch, got {other:?}"),
    }

    let (start_header, start) = ClientPacket::decode(
        &fixture_bytes("gamestart-gamedata", "008-c2s-client1-gamestart.bin"),
        false,
    )
    .expect("fixture GAMESTART decodes");
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: start_header,
            packet: start,
        },
    );
    assert_eq!(h.role.state(), ServerState::InGame);

    // The GAMESTART broadcast rides the chain: first the LAUNCH head,
    // then the GAMESTART behind it.
    h.ack(alice, 1);
    let actions = h.ack(alice, 2);
    let gamestarts: Vec<_> = actions
        .iter()
        .filter(|a| {
            matches!(
                a,
                Action::Send {
                    packet: ServerPacket::GameStart(_),
                    ..
                }
            )
        })
        .collect();
    assert_eq!(gamestarts.len(), 1);
    match &gamestarts[0] {
        Action::Send {
            packet: ServerPacket::GameStart(settings),
            ..
        } => {
            // The fixture's authoritative settings, by invariant.
            assert_eq!(settings.ticdup, 1);
            assert_eq!(settings.extratics, 1);
            assert_eq!(settings.deathmatch, 0);
            assert_eq!(settings.episode, 1);
            assert_eq!(settings.map, 1);
            assert_eq!(settings.skill, 2);
            assert_eq!(settings.gameversion, 5);
            assert_eq!(settings.new_sync, 1);
            assert_eq!(settings.consoleplayer, 0);
            assert_eq!(settings.player_classes.len(), 1);
        }
        other => panic!("expected GameStart, got {other:?}"),
    }
}

// --- followup-8 correction regressions ----------------------------------------

#[test]
fn head_only_second_enqueue_waits_and_ack_emits_next() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");

    // Behind the unacked accept, a broadcast queues silently.
    let mut actions = Vec::new();
    h.role.enqueue_reliable(
        &mut actions,
        alice,
        ServerPacket::ConsoleMessage {
            message: b"second".to_vec(),
        },
    );
    assert!(Harness::reliable_sends_to(&actions, alice).is_empty());

    // The exact-head ACK pops the accept and emits the queued message
    // immediately, with its retry clock starting now.
    let emitted = h.ack(alice, 1);
    assert_eq!(Harness::reliable_sends_to(&emitted, alice).len(), 1);
    match Harness::reliable_sends_to(&emitted, alice)[0] {
        Action::Send { header, .. } => assert_eq!(header.reliable_seq, Some(1)),
        other => panic!("expected next head after ACK, got {other:?}"),
    }

    // Losing the first send converges by head retry: the message comes
    // back with the same sequence after the retry interval, and the
    // queue never grows.
    assert!(Harness::reliable_sends_to(&h.tick(1000), alice).is_empty());
    let retried = h.tick(1001);
    assert_eq!(Harness::reliable_sends_to(&retried, alice).len(), 1);
    match Harness::reliable_sends_to(&retried, alice)[0] {
        Action::Send { header, .. } => assert_eq!(header.reliable_seq, Some(1)),
        other => panic!("expected head retry, got {other:?}"),
    }
    assert_eq!(
        h.role
            .peers
            .get(&alice)
            .expect("peer")
            .reliable_outbox
            .len(),
        1
    );
}

#[test]
fn gamestart_overflow_cannot_leave_ingame_with_zero_peers() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");

    // Fill alice's FIFO to the cap through the internal path.
    h.role
        .peers
        .get_mut(&alice)
        .expect("peer")
        .reliable_outbox
        .clear();
    for _ in 0..RELIABLE_CAP {
        let mut sink = Vec::new();
        h.role.enqueue_reliable(
            &mut sink,
            alice,
            ServerPacket::ConsoleMessage {
                message: b"fill".to_vec(),
            },
        );
    }

    // Drive to the start: the GAMESTART enqueue overflows, removes the
    // only peer, and the transition must not complete.
    h.launch(alice);
    h.gamestart(alice, 1, 0);
    assert_eq!(h.role.state(), ServerState::WaitingLaunch);
    assert_eq!(h.role.peer_count(), 0);
    assert_ne!(h.role.state(), ServerState::InGame);
}

#[test]
fn presyn_member_neither_blocks_start_nor_marks_ready() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    // bob never sends SYN.

    // bob's LAUNCH and GAMESTART are dropped as unknown-address traffic.
    assert!(h.launch(bob).is_empty());
    let actions = h.gamestart(bob, 0, 0);
    assert!(actions.is_empty());
    assert!(!h.role.peers.get(&bob).expect("peer").ready);
    assert_eq!(h.role.controller(), Some(alice));

    // Alice can launch and start alone: bob does not block the game.
    h.launch(alice);
    h.ack(alice, 1);
    h.gamestart(alice, 1, 0);
    assert_eq!(h.role.state(), ServerState::InGame);

    // bob's pre-SYN leave carries no abort or game-end effects.
    let actions: Vec<Action> = h.role.handle(T0, Input::Leave { player: bob });
    assert!(!actions.iter().any(|a| matches!(a, Action::GameEnded)));
    assert_eq!(h.role.state(), ServerState::InGame);
}

#[test]
fn no_non_disconnect_traffic_while_disconnecting() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let drone = h.join("d");
    h.syn(alice, "Alice");
    h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("Observer", 0, 0, 1)),
        },
    );

    // End the game: the drone is disconnecting. Nothing but DISCONNECT
    // retries may reach it afterwards.
    h.role.handle(T0, Input::Leave { player: alice });
    for step in 1..8 {
        let actions = h.tick(1001 * step);
        for action in &actions {
            if let Action::Send { packet, player, .. } = action
                && *player == drone
            {
                assert!(
                    matches!(packet, ServerPacket::Disconnect),
                    "only DISCONNECT may reach a disconnecting peer, got {packet:?}"
                );
            }
        }
        if h.role.peer_count() == 0 {
            break;
        }
    }

    // Its own packets do not restart protocol effects either.
    assert!(h.launch(drone).is_empty());
}

#[test]
fn heretic_episode_exceptions_and_boundaries() {
    assert!(valid_episode_map(6, 1, 4, 1), "registered heretic E4M1");
    assert!(!valid_episode_map(6, 1, 4, 2), "E4M2 is not valid");
    assert!(valid_episode_map(6, 3, 6, 1), "retail heretic E6M1");
    assert!(valid_episode_map(6, 3, 6, 2), "E6M2");
    assert!(valid_episode_map(6, 3, 6, 3), "E6M3");
    assert!(!valid_episode_map(6, 3, 6, 4), "E6M4 is not valid");
    // The exceptions do not leak into doom.
    assert!(!valid_episode_map(0, 1, 4, 1), "doom registered has no E4");
}

#[test]
fn drone_lowres_does_not_force_player_settings() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let drone = h.join("d");
    h.syn(alice, "Alice");
    // The drone records lowres; no player does.
    let mut drone_syn = syn_value("Observer", 0, 0, 1);
    drone_syn.connect.lowres_turn = 1;
    h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(drone_syn),
        },
    );

    h.launch(alice);
    h.ack(alice, 1);
    h.ack(drone, 1);
    h.ack(drone, 2);
    h.gamestart(alice, 1, 0);
    let settings = h.role.settings.clone().expect("settings adopted");
    assert_eq!(
        settings.lowres_turn, 0,
        "a drone's lowres must not force settings"
    );

    // A lowres player does force it.
    let mut h = Harness::new();
    let alice = h.join("a");
    let mut syn = syn_value("Alice", 0, 0, 0);
    syn.connect.lowres_turn = 1;
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn),
        },
    );
    h.launch(alice);
    h.ack(alice, 1);
    h.gamestart(alice, 1, 0);
    let settings = h.role.settings.clone().expect("settings adopted");
    assert_eq!(settings.lowres_turn, 1);
}

#[test]
fn every_plain_send_resets_the_keepalive_idle() {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");

    // Acknowledge the accept first so no retry is pending.
    h.ack(alice, 1);

    // A query at +900 ms sends a plain response, resetting send-idle.
    let actions = h.role.handle(
        Milliseconds(T0.0 + 900),
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Query,
        },
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::QueryResponse(_),
            ..
        }
    )));

    // At +1001 ms no keepalive may fire: the last send was 101 ms ago.
    let actions = h.tick(1001);
    assert!(!actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::Keepalive,
        player,
        ..
    } if *player == alice)));

    // The lobby cadence also counts as a send, so the next keepalive
    // lands a full idle second after the latest actual send.
    let actions = h.tick(2002);
    assert!(actions.iter().any(|a| matches!(a, Action::Send {
        packet: ServerPacket::Keepalive,
        player,
        ..
    } if *player == alice)));
}

#[test]
fn invalid_label_cannot_produce_unencodable_action() {
    let mut h = Harness::new();
    let player = player_id(&mut h.registry, h.room.clone());
    let actions = h.role.handle(
        T0,
        Input::Join {
            player,
            addr_label: b"bad\0label".to_vec(),
        },
    );
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Disconnect {
            reason: DisconnectReason::MalformedInput,
            ..
        }
    )));
    assert_eq!(h.role.peer_count(), 0);

    // And nothing about that peer can ever be emitted later.
    assert!(h.tick(2000).is_empty());

    // A long-but-NUL-free label is bounded, not rejected.
    let player = player_id(&mut h.registry, h.room.clone());
    assert!(
        h.role
            .handle(
                T0,
                Input::Join {
                    player,
                    addr_label: vec![b'x'; 100],
                },
            )
            .is_empty()
    );
    assert_eq!(h.role.peer_count(), 1);
}

#[test]
fn terminal_packets_survive_a_binding_style_reducer() {
    // A reducer that applies actions in order: sends enqueue to the
    // peer's outbox, a Disconnect delivers the terminal packet and only
    // then closes.
    #[derive(Default)]
    struct MockHost {
        outboxes: std::collections::BTreeMap<PlayerId, Vec<ServerPacket>>,
        closed: Vec<PlayerId>,
    }
    impl MockHost {
        fn apply(&mut self, actions: &[Action]) {
            for action in actions {
                match action {
                    Action::Send { player, packet, .. } => {
                        assert!(
                            !self.closed.contains(player),
                            "send to a closed peer: the role must not produce one"
                        );
                        self.outboxes
                            .entry(*player)
                            .or_default()
                            .push(packet.clone());
                    }
                    Action::Disconnect {
                        player, terminal, ..
                    } => {
                        if let Some(terminal) = terminal {
                            self.outboxes
                                .entry(*player)
                                .or_default()
                                .push(terminal.1.clone());
                        }
                        self.closed.push(*player);
                    }
                    Action::GameEnded => {}
                }
            }
        }
    }

    // Rejection: the REJECTED packet must be in the outbox at close time.
    let mut host = MockHost::default();
    let mut h = Harness::new();
    let alice = h.join("a");
    let mut syn = syn_value("Alice", 0, 0, 0);
    syn.protocols = vec![b"OTHER".to_vec()];
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn),
        },
    );
    host.apply(&actions);
    let outbox = host.outboxes.get(&alice).expect("an outbox exists");
    assert!(
        outbox
            .iter()
            .any(|p| matches!(p, ServerPacket::Rejected { .. }))
    );
    assert!(host.closed.contains(&alice));

    // Sleep-expiry removal carries no terminal but also cannot drop a
    // pending acknowledgement: the ACK went out as a plain send while the
    // identity was alive, before any close.
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Disconnect,
        },
    );
    host = MockHost::default();
    host.apply(&actions);
    let outbox = host.outboxes.get(&alice).expect("an outbox exists");
    assert!(
        outbox
            .iter()
            .any(|p| matches!(p, ServerPacket::DisconnectAck))
    );
}

// --- followup-9 pinned-source corrections ------------------------------------

fn syn_full(name: &str, gamemode: u8, gamemission: u8, drone: u8, max_players: u8) -> Syn {
    let mut syn = syn_value(name, gamemode, gamemission, drone);
    syn.connect.max_players = max_players;
    syn
}

#[test]
fn accepted_non_doom_mission_mode_pairs() {
    // Every accepted pair from the pinned table, plus the doom family.
    let accepted = [
        (0, 0),
        (0, 1),
        (0, 3),
        (1, 2),
        (2, 2),
        (3, 2),
        (4, 3), // pack_chex retail
        (5, 2), // pack_hacx commercial
        (6, 0),
        (6, 1),
        (6, 3),
        (7, 2),
        (8, 2),
    ];
    for (mission, mode) in accepted {
        assert!(
            valid_game_mode(mission, mode),
            "({mission}, {mode}) must be accepted"
        );
    }

    // The transposed Chex/Hacx shapes must both be rejected.
    assert!(!valid_game_mode(4, 2), "chex is not commercial");
    assert!(!valid_game_mode(5, 3), "hacx is not retail");
}

#[test]
fn genuine_chex_and_hacx_syns_are_accepted() {
    // pack_chex (mission 4, retail): previously silently dropped by the
    // transposed table.
    let mut h = Harness::new();
    let chex = h.join("c");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: chex,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_full("ChexPlayer", 3, 4, 0, 4)),
        },
    );
    assert!(
        actions.iter().any(is_syn_accept),
        "a genuine Chex SYN must be accepted, got {actions:?}"
    );

    // pack_hacx (mission 5, commercial).
    let mut h = Harness::new();
    let hacx = h.join("x");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: hacx,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_full("HacxPlayer", 2, 5, 0, 4)),
        },
    );
    assert!(
        actions.iter().any(is_syn_accept),
        "a genuine Hacx SYN must be accepted, got {actions:?}"
    );

    // The transposed shapes still take the source-compatible silent
    // invalid path (dropped, no accept, no reject).
    let mut h = Harness::new();
    let wrong = h.join("w");
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: wrong,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_full("Wrong", 2, 4, 0, 4)),
        },
    );
    assert!(actions.is_empty());
}

#[test]
fn episode_map_upper_boundaries() {
    // Chex: retail, E1 only, maps through 5.
    assert!(valid_episode_map(4, 3, 1, 5));
    assert!(!valid_episode_map(4, 3, 1, 6));
    assert!(!valid_episode_map(4, 3, 2, 1));
    // Hacx: commercial, E1 only, maps through 32.
    assert!(valid_episode_map(5, 2, 1, 32));
    assert!(!valid_episode_map(5, 2, 1, 33));
    assert!(!valid_episode_map(5, 2, 2, 1));
    // Heretic shareware: E1, maps through 9.
    assert!(valid_episode_map(6, 0, 1, 9));
    assert!(!valid_episode_map(6, 0, 1, 10));
    assert!(!valid_episode_map(6, 0, 2, 1));
    // Heretic registered: E1-E3 plus the E4M1 exception only.
    assert!(valid_episode_map(6, 1, 3, 9));
    assert!(valid_episode_map(6, 1, 4, 1));
    assert!(!valid_episode_map(6, 1, 4, 2));
    assert!(!valid_episode_map(6, 1, 3, 10));
    // Heretic retail: E1-E5 plus E6M1 through E6M3.
    assert!(valid_episode_map(6, 3, 5, 9));
    assert!(valid_episode_map(6, 3, 6, 1));
    assert!(valid_episode_map(6, 3, 6, 3));
    assert!(!valid_episode_map(6, 3, 6, 4));
    assert!(!valid_episode_map(6, 3, 5, 10));
    // Hexen: map 60 boundary.
    assert!(valid_episode_map(7, 2, 1, 60));
    assert!(!valid_episode_map(7, 2, 1, 61));
    // Strife: map 34 boundary.
    assert!(valid_episode_map(8, 2, 1, 34));
    assert!(!valid_episode_map(8, 2, 1, 35));
}

#[test]
fn slot_reuse_moves_the_max_players_reference() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_full("Alice", 0, 0, 0, 4)),
        },
    );
    h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_full("Bob", 0, 0, 0, 8)),
        },
    );
    assert_eq!(h.role.max_players(), 4, "the lowest slot's value wins");

    // Alice disconnects; carol takes her freed slot, not a new one.
    h.role.handle(T0, Input::Leave { player: alice });
    let carol = h.join("c");
    h.role.handle(
        T0,
        Input::Packet {
            player: carol,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_full("Carol", 0, 0, 0, 8)),
        },
    );
    assert_eq!(
        h.role.peers.get(&carol).expect("carol").slot,
        Some(0),
        "the freed slot is reused, not a fresh one"
    );
    assert_eq!(
        h.role.peers.get(&bob).expect("bob").slot,
        Some(1),
        "existing slots are not renumbered"
    );
    assert_eq!(
        h.role.max_players(),
        8,
        "the reused slot drives the reference"
    );

    // Player numbering follows slot order, not admission order.
    assert_eq!(h.role.player_index(bob), 1);
    assert_eq!(h.role.player_index(carol), 0);
}

#[test]
fn version_mismatch_rejection_uses_the_pinned_text() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let mut syn = syn_value("Alice", 0, 0, 0);
    syn.version = b"Chocolate Doom 2.3.0".to_vec();
    syn.protocols = vec![b"OBSOLETE".to_vec()];
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn),
        },
    );
    let expected = format!(
        "Version mismatch: server version is: {}; client is: Chocolate Doom 2.3.0. No common compatible protocol could be negotiated.",
        String::from_utf8_lossy(super::SERVER_VERSION)
    );
    assert!(
        actions
            .iter()
            .any(|a| is_reject_with(a, expected.as_bytes()))
    );
}

#[test]
fn all_ready_excludes_disconnecting_peers() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.launch(alice);

    // Alice readies alone, then leaves: the abort ends the game and bob
    // (never ready) starts disconnecting.
    h.ack(alice, 1);
    h.ack(bob, 1);
    h.gamestart(alice, 1, 0);
    h.role.handle(T0, Input::Leave { player: alice });
    assert!(matches!(
        h.role.peers.get(&bob).expect("bob").conn,
        super::Conn::Disconnecting { .. }
    ));

    // A new player connects, launches, and readies. Bob is draining and
    // must not count in the readiness requirement at all.
    let carol = h.join("c");
    h.syn(carol, "Carol");
    h.launch(carol);
    h.ack(carol, 1);
    h.gamestart(carol, 1, 0);
    assert_eq!(
        h.role.state(),
        ServerState::InGame,
        "a draining peer must not block or count toward the start"
    );
}
