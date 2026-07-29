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
    // indeterminate and a drone-first connection mismatches it. The
    // reason is byte-exact with the pinned server: the still-default
    // mode renders through D_GameModeString's default as "unknown",
    // and the client's concrete mode names itself (net_server.c:721).
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
    assert!(actions.iter().any(|a| is_reject_with(
        a,
        b"Game mismatch: server is doom (unknown), client is doom (shareware)"
    )));
}

#[test]
fn game_mismatch_reason_names_chex_and_hacx_correctly() {
    // pack_chex is mission 4 and pack_hacx is mission 5 (d_mode.h); a
    // swap in either mapping fails this test by name.
    for (mode, mission, server_side) in [(3, 4, "chex (retail)"), (2, 5, "hacx (commercial)")] {
        let mut h = Harness::new();
        let first = h.join("a");
        h.role.handle(
            T0,
            Input::Packet {
                player: first,
                header: WireHeader { reliable_seq: None },
                packet: ClientPacket::Syn(syn_value("First", mode, mission, 0)),
            },
        );
        let second = h.join("b");
        let actions = h.role.handle(
            T0,
            Input::Packet {
                player: second,
                header: WireHeader { reliable_seq: None },
                packet: ClientPacket::Syn(syn_value("Second", 0, 0, 0)),
            },
        );
        let expected =
            format!("Game mismatch: server is {server_side}, client is doom (shareware)");
        assert!(
            actions
                .iter()
                .any(|a| is_reject_with(a, expected.as_bytes())),
            "the mismatch reason names {server_side}"
        );
    }
}

#[test]
fn mission_and_mode_names_match_upstream_strings() {
    // D_GameMissionString / D_GameModeString (d_mode.c): the named
    // ordinals keep their names; every other mission renders "none"
    // and every other mode renders "unknown", so neither default can
    // silently return to the old rendering.
    let missions = [
        "doom", "doom2", "tnt", "plutonia", "chex", "hacx", "heretic", "hexen", "strife",
    ];
    for (ordinal, name) in missions.iter().enumerate() {
        assert_eq!(mission_name(ordinal as u8), *name);
    }
    for ordinal in [9u8, 10, 11, 255] {
        assert_eq!(mission_name(ordinal), "none", "mission {ordinal}");
    }
    let modes = ["shareware", "registered", "commercial", "retail"];
    for (ordinal, name) in modes.iter().enumerate() {
        assert_eq!(mode_name(ordinal as u8), *name);
    }
    for ordinal in [4u8, 5, 255] {
        assert_eq!(mode_name(ordinal), "unknown", "mode {ordinal}");
    }
}

#[test]
fn valid_mode_table_matches_chocolate_311_exactly() {
    // The pinned 13 valid mission/mode pairs from Chocolate Doom 3.1.1
    // D_ValidGameMode, exhaustive over the byte: the Crispy-lineage
    // NERVE/MASTER ordinals 9 and 10 must NOT be admitted (ordinal 9
    // means doom2f to the pinned 3.1.1 oracle; deferred per the lead
    // reconciliation of follow-up 20).
    let valid = [
        (0, 0),
        (0, 1),
        (0, 3),
        (1, 2),
        (2, 2),
        (3, 2),
        (4, 3),
        (5, 2),
        (6, 0),
        (6, 1),
        (6, 3),
        (7, 2),
        (8, 2),
    ];
    for mission in 0..=u8::MAX {
        for mode in 0..=u8::MAX {
            assert_eq!(
                valid_game_mode(mission, mode),
                valid.contains(&(mission, mode)),
                "pair ({mission}, {mode})"
            );
        }
    }
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

// followup-lead-server-24 addendum 2 / proto-38: the client-visible
// slot-reuse discriminator. "first" takes slot 0, "second" slot 1;
// "first" leaves with "second" present; "third" reuses slot 0. Slot
// order is [third, second] while PlayerId order is [second, third],
// permanently. One timer pass times both survivors out: pinned
// NET_SV_Run order expires the reused slot 0 first, so "second" is
// told about "third" and "third" hears nothing. A mutation restoring
// ascending-PlayerId traversal expires "second" first and sends
// "third" the 'second' message instead; it fails this test by name.
#[test]
fn slot_reuse_timeout_broadcast_follows_protocol_slot_order() {
    let mut h = Harness::new();
    let first = h.join("a");
    let second = h.join("b");
    h.syn(first, "first");
    h.syn(second, "second");

    // "first" leaves with "second" still present: slot 0 frees for
    // reuse and no abort or end-game fires (one player remains).
    h.role.handle(T0, Input::Leave { player: first });
    let third = h.join("c");
    h.syn(third, "third");

    // Both survivors acknowledge their accepts so the timeout broadcast
    // emits immediately, aligning both receive clocks on T0.
    h.ack(second, 1);
    h.ack(third, 1);

    // One pass at 30 s times out both Connected peers in a single
    // batch. The reused slot 0 expires first: its broadcast reaches the
    // still-connected "second"; the later one reaches no one.
    let actions = h.tick(30_001);

    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Disconnect { player, .. } if *player == third
    )));
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::Disconnect { player, .. } if *player == second
    )));
    let console: Vec<(PlayerId, &Vec<u8>)> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Send {
                player,
                packet: ServerPacket::ConsoleMessage { message },
                ..
            } => Some((*player, message)),
            _ => None,
        })
        .collect();
    assert_eq!(console.len(), 1, "exactly one timeout broadcast emits");
    assert_eq!(console[0].0, second);
    assert_eq!(
        console[0].1.as_slice(),
        b"Client 'third' timed out and disconnected"
    );
    // Nothing was sent to the peer that expired first.
    assert!(Harness::sends_to(&actions, third).is_empty());
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
    // The terminal packet is delivered as the FINAL packet before the
    // removal takes effect: nothing owed is lost behind the close.
    assert!(matches!(outbox.last(), Some(ServerPacket::Rejected { .. })));

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

#[test]
fn disconnecting_drone_is_not_counted_in_waiting_data() {
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

    // While the drone is connected it counts.
    let data = h.role.waiting_data(alice);
    assert_eq!(data.num_drones, 1);

    // The player leaves: the game ends and the drone starts draining.
    h.role.handle(T0, Input::Leave { player: alice });
    assert!(matches!(
        h.role.peers.get(&drone).expect("drone").conn,
        super::Conn::Disconnecting { .. }
    ));

    // A new player connects: its first lobby update must not count the
    // draining drone (NET_SV_NumDrones uses ClientConnected).
    let carol = h.join("c");
    h.syn(carol, "Carol");
    let data = h.role.waiting_data(carol);
    assert_eq!(data.num_drones, 0, "a disconnecting drone is not connected");
}

#[test]
fn presyn_member_is_timer_silent() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let ghost = h.join("g");
    h.syn(alice, "Alice");
    h.ack(alice, 1);
    // ghost is admitted but never sends SYN.

    // Past the one-second send-idle threshold: no keepalive or any other
    // action targets the pre-SYN member.
    let actions = h.tick(2000);
    assert!(
        Harness::sends_to(&actions, ghost).is_empty(),
        "a pre-SYN member must get no protocol traffic, got {actions:?}"
    );

    // Thirty seconds on, with the survivor's receive clock kept fresh: no
    // timeout, disconnect, GameEnded, or console broadcast from the
    // pre-SYN member, and in particular no empty-name broadcast to the
    // live survivor.
    for step in 1..=30 {
        h.role.handle(
            Milliseconds(T0.0 + 2000 + step * 1000),
            Input::Packet {
                player: alice,
                header: WireHeader { reliable_seq: None },
                packet: ClientPacket::Keepalive,
            },
        );
    }
    let actions = h.tick(32_000);
    assert!(
        Harness::sends_to(&actions, ghost).is_empty(),
        "a pre-SYN member must stay timer-silent at 30 s, got {actions:?}"
    );
    assert!(
        !actions.iter().any(|a| matches!(a, Action::GameEnded)),
        "a pre-SYN member must not end the game"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Disconnect { .. })),
        "a pre-SYN member must not be timed out"
    );
    assert!(
        !actions.iter().any(|a| matches!(a, Action::Send {
            packet: ServerPacket::ConsoleMessage { message },
            ..
        } if message.starts_with(b"Client ''"))),
        "no empty-name broadcast may reach the live survivor"
    );
    assert_eq!(h.role.peer_count(), 2);
}

#[test]
fn reliable_cap_is_literally_sixty_four() {
    // The deviation is deliberate and literal: 64 accepted, the 65th
    // removes. This must not drift with a reused constant.
    assert_eq!(RELIABLE_CAP, 64);

    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    h.role
        .peers
        .get_mut(&alice)
        .expect("peer")
        .reliable_outbox
        .clear();

    // Exactly sixty-four accepted enqueues, counted literally.
    for _ in 0..64 {
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
        64
    );
    assert_eq!(h.role.peer_count(), 1);

    // The 65th removes the peer without allocation or victim emission.
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
    assert!(
        Harness::sends_to(&actions, alice).is_empty(),
        "the 65th emits nothing to the removed peer"
    );
    assert_eq!(h.role.peer_count(), 0);
}

#[test]
fn rejection_on_empty_room_emits_no_game_ended() {
    let mut h = Harness::new();
    let stranger = h.join("s");
    let mut syn = syn_value("Stranger", 0, 0, 0);
    syn.protocols = vec![b"OBSOLETE".to_vec()];
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: stranger,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn),
        },
    );

    // Exactly the terminal rejection/removal; nothing else.
    assert_eq!(actions.len(), 1);
    let expected = format!(
        "Version mismatch: server version is: {}; client is: Chocolate Doom 3.1.1. No common compatible protocol could be negotiated.",
        String::from_utf8_lossy(super::SERVER_VERSION)
    );
    assert!(
        actions
            .iter()
            .any(|a| is_reject_with(a, expected.as_bytes()))
    );
    assert!(
        !actions.iter().any(|a| matches!(a, Action::GameEnded)),
        "a rejected stranger must not end the game"
    );
    assert_eq!(h.role.peer_count(), 0);
}

#[test]
fn rejection_leaves_presyn_bystander_untouched() {
    let mut h = Harness::new();
    let stranger = h.join("s");
    let bystander = h.join("b");
    let mut syn = syn_value("Stranger", 0, 0, 0);
    syn.protocols = vec![b"OBSOLETE".to_vec()];
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: stranger,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn),
        },
    );

    // The bystander is neither targeted nor removed, and no game-end or
    // broadcast path fires.
    assert!(
        Harness::sends_to(&actions, bystander).is_empty(),
        "the bystander must receive nothing, got {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Disconnect { player, .. } if *player == bystander)),
        "the bystander must not be removed"
    );
    assert!(!actions.iter().any(|a| matches!(a, Action::GameEnded)));
    assert_eq!(h.role.peer_count(), 1);
    assert_eq!(
        h.role.peers.get(&bystander).map(|peer| peer.syn),
        Some(false),
        "the bystander remains an admitted pre-SYN member"
    );
}

#[test]
fn rejection_terminal_stays_final_before_close_in_the_reducer() {
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

    let mut h = Harness::new();
    let stranger = h.join("s");
    let bystander = h.join("b");
    let mut syn = syn_value("Stranger", 0, 0, 0);
    syn.protocols = vec![b"OBSOLETE".to_vec()];
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: stranger,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn),
        },
    );

    let mut host = MockHost::default();
    host.apply(&actions);
    let outbox = host.outboxes.get(&stranger).expect("an outbox exists");
    assert!(
        matches!(outbox.last(), Some(ServerPacket::Rejected { .. })),
        "the terminal REJECTED is the final packet before the close"
    );
    assert!(host.closed.contains(&stranger));
    assert!(!host.closed.contains(&bystander));
}

#[test]
fn established_old_magic_rejects_without_removing() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.launch(alice);
    assert_eq!(h.role.state(), ServerState::WaitingStart);

    let actions = h.role.handle(
        T0,
        Input::Malformed {
            player: alice,
            class: MalformedClass::Syn { old_magic: true },
        },
    );

    // Exactly one plain non-terminal REJECTED to the controller, with the
    // full source-shaped reason.
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Send {
            player,
            header,
            packet: ServerPacket::Rejected { reason },
            ..
        } => {
            assert_eq!(*player, alice);
            assert_eq!(header.reliable_seq, None);
            let expected = format!(
                "You are using an old client version that is not supported by this server. This server is running {}.",
                String::from_utf8_lossy(super::SERVER_VERSION)
            );
            assert_eq!(reason.as_slice(), expected.as_bytes());
        }
        other => panic!("expected a plain REJECTED, got {other:?}"),
    }

    // Both peers, the controller, readiness, and the room state survive;
    // no Disconnect, console message, or GameEnded follows.
    assert_eq!(h.role.peer_count(), 2);
    assert_eq!(h.role.controller(), Some(alice));
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Disconnect { .. }))
    );
    assert!(!actions.iter().any(|a| matches!(a, Action::GameEnded)));
    assert!(!actions.iter().any(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::ConsoleMessage { .. },
            ..
        }
    )));

    // The game still starts afterwards.
    h.ack(alice, 1);
    h.ack(bob, 1);
    h.gamestart(alice, 1, 0);
    h.gamestart(bob, 0, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
}

#[test]
fn presyn_old_magic_uses_terminal_close_with_exact_reason() {
    let mut h = Harness::new();
    let stranger = h.join("s");
    let actions = h.role.handle(
        T0,
        Input::Malformed {
            player: stranger,
            class: MalformedClass::Syn { old_magic: true },
        },
    );

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Disconnect {
            player,
            terminal: Some(terminal),
            ..
        } => {
            assert_eq!(*player, stranger);
            match &terminal.1 {
                ServerPacket::Rejected { reason } => {
                    let expected = format!(
                        "You are using an old client version that is not supported by this server. This server is running {}.",
                        String::from_utf8_lossy(super::SERVER_VERSION)
                    );
                    assert_eq!(reason.as_slice(), expected.as_bytes());
                }
                other => panic!("expected a terminal REJECTED, got {other:?}"),
            }
        }
        other => panic!("expected a terminal Disconnect, got {other:?}"),
    }
    assert_eq!(h.role.peer_count(), 0);
    assert!(!actions.iter().any(|a| matches!(a, Action::GameEnded)));
}

#[test]
fn presyn_leave_lone_member_is_exact_and_quiet() {
    let mut h = Harness::new();
    let ghost = h.join("g");
    let actions: Vec<Action> = h.role.handle(T0, Input::Leave { player: ghost });

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Disconnect {
            player,
            reason,
            terminal,
        } => {
            assert_eq!(*player, ghost);
            assert_eq!(*reason, DisconnectReason::Remote);
            assert_eq!(*terminal, None);
        }
        other => panic!("expected exactly one remote Disconnect, got {other:?}"),
    }
    assert_eq!(h.role.peer_count(), 0);
}

#[test]
fn presyn_leave_beside_a_connected_player_changes_nothing_else() {
    let mut h = Harness::new();
    let alice = h.join("a");
    let ghost = h.join("g");
    h.syn(alice, "Alice");

    let actions: Vec<Action> = h.role.handle(T0, Input::Leave { player: ghost });
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        Action::Disconnect {
            player,
            reason,
            terminal,
        } => {
            assert_eq!(*player, ghost);
            assert_eq!(*reason, DisconnectReason::Remote);
            assert_eq!(*terminal, None);
        }
        other => panic!("expected exactly one remote Disconnect, got {other:?}"),
    }

    // The connected player's identity, controller role, and state are
    // unchanged, and no broadcast or GameEnded fired.
    assert_eq!(h.role.peer_count(), 1);
    assert_eq!(h.role.controller(), Some(alice));
    assert_eq!(h.role.peers.get(&alice).expect("alice").name, b"Alice");
    assert!(!actions.iter().any(|a| matches!(a, Action::GameEnded)));
    assert!(!actions.iter().any(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::ConsoleMessage { .. },
            ..
        }
    )));
}

// --- in-game tic windows -------------------------------------------------------

fn diff(forward: i8) -> doom_proto::TiccmdDiff {
    doom_proto::TiccmdDiff {
        forward: Some(forward),
        ..Default::default()
    }
}

fn upload(ack: u8, start: u8, tics: Vec<(i16, doom_proto::TiccmdDiff)>) -> ClientPacket {
    ClientPacket::GameData(doom_proto::GameDataClient {
        ack,
        start,
        tics: tics
            .into_iter()
            .map(|(latency, diff)| doom_proto::ClientTic { latency, diff })
            .collect(),
    })
}

fn gamedata_to(actions: &[Action], player: PlayerId) -> Vec<&doom_proto::GameDataServer> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Send {
                player: p,
                packet: ServerPacket::GameData(data),
                ..
            } if *p == player => Some(data),
            _ => None,
        })
        .collect()
}

fn resends_to(actions: &[Action], player: PlayerId) -> Vec<(u32, u8)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Send {
                player: p,
                packet: ServerPacket::GameDataResend { start, count },
                ..
            } if *p == player => Some((*start, *count)),
            _ => None,
        })
        .collect()
}

/// Drive a one-player room into InGame with extratics 1.
fn in_game_one() -> (Harness, PlayerId) {
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    h.launch(alice);
    h.ack(alice, 1);
    h.gamestart(alice, 1, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    (h, alice)
}

/// Drive a two-player room into InGame with extratics 1.
fn in_game_two() -> (Harness, PlayerId, PlayerId) {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.launch(alice);
    h.ack(alice, 1);
    h.ack(bob, 1);
    h.gamestart(alice, 1, 0);
    h.gamestart(bob, 0, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    (h, alice, bob)
}

#[test]
fn expand_boundaries_wrap_and_range() {
    // Middle of a byte: no wrap.
    assert_eq!(expand_tic(512, 10), 512 + 10);
    // 0x40 boundary: at it exactly, no backward wrap (strictly below).
    assert_eq!(expand_tic(0x140, 0xb1), 0x1b1);
    // Below it, a byte above 0xb0 wraps backward.
    assert_eq!(expand_tic(0x13f, 0xb1), 0xb1);
    // Just at 0x40: no backward wrap (strictly below required); the
    // byte is replaced as-is.
    assert_eq!(expand_tic(0x140, 0xb0), 0x1b0);
    // 0xb0 boundary: above it, a byte below 0x40 wraps forward.
    assert_eq!(expand_tic(0x1b1, 0x3f), 0x23f);
    // Exactly 0xb0: no forward wrap, the byte is replaced as-is.
    assert_eq!(expand_tic(0x1b0, 0x3f), 0x13f);
    // Backward wrap below zero stays out of range rather than aliasing.
    assert!(expand_tic(0x20, 0xf0) < 0);
    // Huge references stay exact in i64 with no accidental truncation.
    assert_eq!(expand_tic(u32::MAX - 0x7f, 0x3f), 0xffff_ff3f_i64);
}

#[test]
fn upload_stores_only_in_range_tics_and_raises_ack_monotonically() {
    let (mut h, alice) = in_game_one();

    // Three single-player pumps put tics 0..2 on the wire, so the
    // acknowledgement of exactly the send sequence below is the valid
    // ceiling boundary rather than an acknowledgement of unsent tics.
    h.tick(1);
    h.tick(2);
    h.tick(3);

    // Tics 0..2 in range, nothing missing behind them: no request.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(3, 0, vec![(10, diff(1)), (11, diff(2)), (12, diff(3))]),
        },
    );
    assert!(actions.is_empty(), "no resend needed behind present tics");
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert!(recv.entries[0][0].active);
        assert!(recv.entries[1][0].active);
        assert!(!recv.entries[3][0].active);
        assert_eq!(recv.entries[0][0].latency, 10);
    }
    assert_eq!(
        h.role
            .peers
            .get(&alice)
            .expect("peer")
            .game
            .as_ref()
            .expect("game")
            .acknowledged,
        3
    );

    // A mid-window upload legitimately reveals the missing prefix behind
    // it, exactly as pinned: one bounded request for it.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(3, 5, vec![(10, diff(4))]),
        },
    );
    let requests = resends_to(&actions, alice);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, 3);
    assert_eq!(requests[0].1, 2);

    // A lower ack does not move the acknowledgement back.
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(1, 7, vec![(5, diff(4))]),
        },
    );
    assert_eq!(
        h.role
            .peers
            .get(&alice)
            .expect("peer")
            .game
            .as_ref()
            .expect("game")
            .acknowledged,
        3
    );
}

#[test]
fn stale_and_future_uploads_are_dropped_without_aliasing() {
    let (mut h, alice) = in_game_one();
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 5, vec![(1, diff(9))]),
        },
    );

    // Re-uploading the same absolute tic overwrites nothing extra and
    // cannot alias a later slot (absolute identity preserved).
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 5, vec![(1, diff(42))]),
        },
    );
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert_eq!(recv.entries[5][0].diff.forward, Some(42));
        assert!(!recv.entries[6][0].active);
    }

    // A tic beyond the window end is dropped, and the discovered run it
    // reveals is bounded by the upstream clamp: the request covers only
    // in-window missing tics, never the out-of-range ones.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 130, vec![(1, diff(1))]),
        },
    );
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert!(!recv.entries[BACKUPTICS - 1][0].active);
    }
    let requests = resends_to(&actions, alice);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, 6);
    assert_eq!(
        requests[0].1, 121,
        "the run is bounded at the upstream clamp"
    );
}

#[test]
fn uploads_from_wrong_states_are_inert() {
    let (mut h, alice) = in_game_one();

    // A drone upload is rejected in game.
    let drone = h.join("d");
    h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::Syn(syn_value("Observer", 0, 0, 1)),
        },
    );
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(1, diff(1))]),
        },
    );
    assert!(actions.is_empty());
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert!(!recv.entries[0][0].active);
    }

    // An upload after the game ends is inert too.
    h.role.handle(T0, Input::Leave { player: alice });
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: drone,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(1, diff(1))]),
        },
    );
    assert!(actions.is_empty());
    assert!(h.role.recv.is_none(), "the window resets at game end");
}

#[test]
fn two_player_reciprocal_fanout_excludes_recipient_and_maxes_latency() {
    let (mut h, alice, bob) = in_game_two();

    // Alice uploads tic 0; alice's fan-out needs bob's tic first, but
    // bob's fan-out does not need bob's own.
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(30, diff(5))]),
        },
    );
    let actions = h.tick(1);
    assert!(
        gamedata_to(&actions, alice).is_empty(),
        "alice needs bob's tic first"
    );

    // Bob already receives alice's command at index 0: his own tic is
    // excluded from his requirements too.
    let to_bob = gamedata_to(&actions, bob);
    assert_eq!(to_bob.len(), 1);
    assert_eq!(to_bob[0].tics.len(), 1);
    assert_eq!(to_bob[0].tics[0].players.len(), 1);
    assert_eq!(to_bob[0].tics[0].players[0].0, 0);
    assert_eq!(to_bob[0].tics[0].players[0].1.forward, Some(5));

    h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(99, diff(7))]),
        },
    );

    // Alice then receives only bob's command, with bob's latency, at 1.
    let actions = h.tick(2);
    let to_alice = gamedata_to(&actions, alice);
    assert_eq!(to_alice.len(), 1);
    assert_eq!(to_alice[0].start, 0);
    assert_eq!(to_alice[0].tics.len(), 1);
    assert_eq!(to_alice[0].tics[0].latency, 99);
    assert_eq!(to_alice[0].tics[0].players.len(), 1);
    assert_eq!(to_alice[0].tics[0].players[0].0, 1);
    assert_eq!(to_alice[0].tics[0].players[0].1.forward, Some(7));
}

#[test]
fn single_player_empty_fanout_capped_at_ten_ahead() {
    let (mut h, alice) = in_game_one();
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(1, diff(1))]),
        },
    );

    // With no other player, the server fans out empty commands but stays
    // at most 10 tics ahead of the window.
    let mut total = 0;
    for step in 1..=12 {
        total += gamedata_to(&h.tick(step), alice).len();
    }
    assert_eq!(total, 11, "tics 0..=10 fan out, then the limit holds");

    // extratics replay: each emission covers sendseq-1 .. sendseq.
    let actions = h.tick(13);
    let last = gamedata_to(&actions, alice);
    assert!(last.is_empty());
}

#[test]
fn stall_guard_stops_pumping_past_forty_ahead() {
    let (mut h, alice, bob) = in_game_two();
    // Bob's tics 40 and 41 are present so completeness cannot mask the
    // stall guard at either send sequence.
    h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 40, vec![(1, diff(1)), (1, diff(2))]),
        },
    );
    // At exactly 40 ahead of the minimum acknowledgement, alice pumps.
    {
        let game = h
            .role
            .peers
            .get_mut(&alice)
            .expect("peer")
            .game
            .as_mut()
            .expect("game");
        game.sendseq = 40;
        // The forced send sequence skips the queue entries real pumps
        // would have written; the all-or-nothing span needs them whole.
        for seq in 39..=41u32 {
            game.sendqueue[(seq as usize) % BACKUPTICS] = Some(QueuedTic {
                seq,
                tic: doom_proto::FullTic {
                    latency: 0,
                    players: Vec::new(),
                },
            });
        }
    }
    assert_eq!(
        gamedata_to(&h.tick(1), alice).len(),
        1,
        "exactly 40 still pumps"
    );

    // One further ahead and the pinned stall guard stops the pump.
    {
        let game = h
            .role
            .peers
            .get_mut(&alice)
            .expect("peer")
            .game
            .as_mut()
            .expect("game");
        game.sendseq = 41;
    }
    assert!(gamedata_to(&h.tick(2), alice).is_empty(), "41 ahead stalls");
}

#[test]
fn advance_window_requires_min_ack_and_completeness() {
    let (mut h, alice, bob) = in_game_two();
    // Alice uploads tic 0; nothing has been sent to her yet, so her
    // upload cannot carry a valid non-zero acknowledgement.
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(1, diff(1))]),
        },
    );
    h.tick(1);
    assert_eq!(
        h.role.recv.as_ref().expect("window").start,
        0,
        "bob's tic is required"
    );

    // Bob's tic completes the first window slot. His first pump above
    // sent his tic 0, so the ack 1 his upload carries is exactly valid.
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert_eq!(recv.entries[0][0].diff.forward, Some(1));
    }
    h.role.handle(
        T0,
        Input::Packet {
            player: bob,
            header: WireHeader { reliable_seq: None },
            packet: upload(1, 0, vec![(1, diff(2))]),
        },
    );
    // Alice's first pump happens once bob's tic is in; only then can her
    // standalone ack 1 pass the send-sequence ceiling.
    h.tick(2);
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::GameDataAck { ack: 1 },
        },
    );
    h.tick(3);
    assert_eq!(h.role.recv.as_ref().expect("window").start, 1);

    // The completed tic is consumed off the bottom of the window; the
    // next slot now holds nothing until more data arrives.
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert!(!recv.entries[0][0].active);
        assert!(!recv.entries[0][1].active);
    }
}

#[test]
fn gap_triggers_one_request_then_strict_300ms_re_requests() {
    let (mut h, alice) = in_game_one();

    // Tics 0..2 arrive; tic 3 is skipped; tic 4 arrives and reveals 3.
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(1, diff(0)), (1, diff(1)), (1, diff(2))]),
        },
    );
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 4, vec![(1, diff(4))]),
        },
    );
    let requests = resends_to(&actions, alice);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, 3);
    assert_eq!(requests[0].1, 1);

    // No duplicate before the expiry.
    assert!(resends_to(&h.tick(300), alice).is_empty());
    // Strictly more than 300 ms: the run is re-requested once.
    let again = resends_to(&h.tick(301), alice);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].0, 3);
    assert_eq!(again[0].1, 1);
    // And not again immediately after.
    assert!(resends_to(&h.tick(601), alice).is_empty());
}

#[test]
fn resend_request_is_atomic_against_stale_and_spoofed() {
    let (mut h, alice) = in_game_one();
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(1, diff(1)), (1, diff(2)), (1, diff(3))]),
        },
    );
    // Pump so the queue holds tics 0..2.
    h.tick(1);
    h.tick(2);
    h.tick(3);

    // A valid request for 0..2 replays exactly those queued tics.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::GameDataResend { start: 0, count: 3 },
        },
    );
    let replay = gamedata_to(&actions, alice);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].tics.len(), 3);

    // A stale request (tic 9 not queued) is ignored entirely.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::GameDataResend { start: 1, count: 9 },
        },
    );
    assert!(gamedata_to(&actions, alice).is_empty());

    // Zero count is ignored, not answered with an empty packet.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::GameDataResend { start: 0, count: 0 },
        },
    );
    assert!(gamedata_to(&actions, alice).is_empty());

    // Arithmetic boundary: a count wrapping past u32 must not panic or
    // alias the queue.
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: ClientPacket::GameDataResend {
                start: u32::MAX - 1,
                count: 255,
            },
        },
    );
    assert!(gamedata_to(&actions, alice).is_empty());
}

#[test]
fn deadlock_requests_first_missing_plus_five_and_replays_queue() {
    let (mut h, alice, bob) = in_game_two();

    // Both players upload tic 0; both pump once (sendseq 1 each).
    for player in [alice, bob] {
        h.role.handle(
            T0,
            Input::Packet {
                player,
                header: WireHeader { reliable_seq: None },
                packet: upload(0, 0, vec![(1, diff(1))]),
            },
        );
    }
    h.tick(1);

    // Bob goes silent past the deadlock threshold; alice stays fresh.
    let actions = h.role.handle(
        Milliseconds(T0.0 + 1001),
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 1, vec![(1, diff(2))]),
        },
    );
    let mut all = actions;
    all.extend(h.tick(1001));

    // First missing tic (1) plus five.
    let requests = resends_to(&all, bob);
    assert!(
        requests.iter().any(|r| r.0 == 1 && r.1 == 6),
        "deadlock requests first missing plus five, got {requests:?}"
    );
    // And bob's exact unacknowledged queue (tic 0) is replayed.
    let replay = gamedata_to(&all, bob);
    assert!(
        replay
            .iter()
            .any(|g| g.tics.iter().any(|t| !t.players.is_empty())),
        "the unacknowledged queue replays with alice's command"
    );

    // The 300 ms resend cadence re-requests the stamped run while it
    // stays missing: nothing at 100 ms, one re-request at 301 ms.
    assert!(resends_to(&h.tick(1101), bob).is_empty());
    let cadence = h.tick(1302);
    assert!(
        resends_to(&cadence, bob)
            .iter()
            .any(|r| r.0 == 1 && r.1 == 6)
    );

    // And the >1000 ms deadlock recovery fires again on its own clock.
    let second = h.tick(2003);
    assert!(
        resends_to(&second, bob)
            .iter()
            .any(|r| r.0 == 1 && r.1 == 6)
    );
}

#[test]
fn window_resets_across_game_end_and_second_launch() {
    let (mut h, alice) = in_game_one();
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader { reliable_seq: None },
            packet: upload(0, 0, vec![(1, diff(9))]),
        },
    );

    // End the game: the old peer is gone, as pinned (every client is
    // disconnected at game end). A new game needs a new admission and
    // SYN, and nothing stale survives into it.
    h.role.handle(T0, Input::Leave { player: alice });
    assert!(h.role.recv.is_none());
    let alice = h.join("a2");
    h.syn(alice, "Alice");
    h.launch(alice);
    h.ack(alice, 1);
    h.gamestart(alice, 1, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert!(!recv.entries[0][0].active, "the old tic is gone");
        let game = h
            .role
            .peers
            .get(&alice)
            .expect("peer")
            .game
            .as_ref()
            .expect("game");
        assert_eq!(game.sendseq, 0);
        assert_eq!(game.acknowledged, 0);
    }
}

#[test]
fn transcript_single_player_gamedata_stable_fields() {
    // The committed single-player session: after GAMESTART the client
    // uploads from tic 0 and the server fans out empty commands.
    let mut h = Harness::new();
    let alice = h.join("a");
    let (_, syn) = ClientPacket::decode(
        &fixture_bytes("gamestart-gamedata", "000-c2s-client1-syn.bin"),
        false,
    )
    .expect("syn");
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
    .expect("launch");
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: launch_header,
            packet: launch,
        },
    );
    h.ack(alice, 1);
    let (start_header, start) = ClientPacket::decode(
        &fixture_bytes("gamestart-gamedata", "008-c2s-client1-gamestart.bin"),
        false,
    )
    .expect("gamestart");
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: start_header,
            packet: start,
        },
    );
    h.ack(alice, 2);
    assert_eq!(h.role.state(), ServerState::InGame);

    // Feed the committed first upload (one tic, zero diff) and pump: the
    // single-player fan-out is an empty command list, stable by packet
    // order and field shape.
    let (header, packet) = ClientPacket::decode(
        &fixture_bytes("gamestart-gamedata", "029-c2s-client1-gamedata.bin"),
        false,
    )
    .expect("gamedata decodes");
    let ClientPacket::GameData(data) = packet else {
        panic!("fixture 029 is GAMEDATA");
    };
    assert_eq!(data.tics.len(), 1);
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header,
            packet: ClientPacket::GameData(data),
        },
    );

    let actions = h.tick(1);
    let fanout = gamedata_to(&actions, alice);
    assert_eq!(fanout.len(), 1);
    assert_eq!(fanout[0].tics.len(), 1);
    assert!(
        fanout[0].tics[0].players.is_empty(),
        "single-player fan-out carries no other player's commands"
    );
    let _ = header;
}

// --- followup battery: frozen indices, acknowledgement contract, abuse
// bounds, drones, resend immutability, zero-count split, wire asymmetry

fn acknowledged(h: &Harness, player: PlayerId) -> u32 {
    h.role
        .peers
        .get(&player)
        .expect("peer")
        .game
        .as_ref()
        .expect("game")
        .acknowledged
}

fn active_count(h: &Harness) -> usize {
    h.role
        .recv
        .as_ref()
        .expect("window")
        .entries
        .iter()
        .flatten()
        .filter(|entry| entry.active)
        .count()
}

fn queued(latency: i16, seq: u32) -> QueuedTic {
    QueuedTic {
        seq,
        tic: doom_proto::FullTic {
            latency,
            players: Vec::new(),
        },
    }
}

fn game_mut(h: &mut Harness, player: PlayerId) -> &mut PeerGame {
    h.role
        .peers
        .get_mut(&player)
        .expect("peer")
        .game
        .as_mut()
        .expect("game")
}

fn send(h: &mut Harness, player: PlayerId, packet: ClientPacket) -> Vec<Action> {
    h.role.handle(
        T0,
        Input::Packet {
            player,
            header: WireHeader { reliable_seq: None },
            packet,
        },
    )
}

/// Drive a three-player room into InGame with extratics 1.
fn in_game_three() -> (Harness, PlayerId, PlayerId, PlayerId) {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    let carol = h.join("c");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.syn(carol, "Carol");
    h.launch(alice);
    h.ack(alice, 1);
    h.ack(bob, 1);
    h.ack(carol, 1);
    h.gamestart(alice, 1, 0);
    h.gamestart(bob, 0, 0);
    h.gamestart(carol, 0, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    (h, alice, bob, carol)
}

/// Drive a player-plus-drone room into InGame with extratics 1.
fn in_game_drone() -> (Harness, PlayerId, PlayerId) {
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
    h.launch(alice);
    h.ack(alice, 1);
    h.ack(drone, 1);
    h.gamestart(alice, 1, 0);
    h.gamestart(drone, 0, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    (h, alice, drone)
}

/// Drive a two-player room into InGame with controller extratics 127.
fn in_game_two_127() -> (Harness, PlayerId, PlayerId) {
    let mut h = Harness::new();
    let alice = h.join("a");
    let bob = h.join("b");
    h.syn(alice, "Alice");
    h.syn(bob, "Bob");
    h.launch(alice);
    h.ack(alice, 1);
    h.ack(bob, 1);
    let mut settings = settings_value(0);
    settings.extratics = 127;
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader {
                reliable_seq: Some(1),
            },
            packet: ClientPacket::GameStart(settings),
        },
    );
    h.gamestart(bob, 0, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    assert_eq!(
        h.role.settings.as_ref().expect("settings").extratics,
        127,
        "the controller settings are authoritative"
    );
    (h, alice, bob)
}

#[test]
fn acknowledgement_contract_ceiling_negative_and_independence() {
    let (mut h, alice) = in_game_one();
    // Three single-player pumps put tics 0..2 on the wire.
    h.tick(1);
    h.tick(2);
    h.tick(3);

    // Exactly the send sequence: valid.
    send(&mut h, alice, ClientPacket::GameDataAck { ack: 3 });
    assert_eq!(acknowledged(&h, alice), 3);

    // One beyond what was ever sent: invalid. The expansion itself is
    // non-negative (4), so this is blocked by the ceiling alone.
    send(&mut h, alice, ClientPacket::GameDataAck { ack: 4 });
    assert_eq!(acknowledged(&h, alice), 3);

    // 0xb0 expands to +176 against a zero window: again blocked only by
    // the ceiling, never by negativity (0xb0 does not wrap down).
    send(&mut h, alice, ClientPacket::GameDataAck { ack: 0xb0 });
    assert_eq!(acknowledged(&h, alice), 3);

    // 0xb1 expands to -79: invalid regardless of the ceiling.
    send(&mut h, alice, ClientPacket::GameDataAck { ack: 0xb1 });
    assert_eq!(acknowledged(&h, alice), 3);

    // A regression is ignored.
    send(&mut h, alice, ClientPacket::GameDataAck { ack: 2 });
    assert_eq!(acknowledged(&h, alice), 3);

    // An invalid acknowledgement never blocks valid in-window tics in
    // the same packet.
    send(&mut h, alice, upload(0xb1, 1, vec![(5, diff(4))]));
    assert!(h.role.recv.as_ref().expect("window").entries[1][0].active);
    assert_eq!(acknowledged(&h, alice), 3);
}

#[test]
fn negative_expanded_start_skips_insertion_and_gaps_but_ack_applies() {
    let (mut h, alice) = in_game_one();
    h.tick(1);
    h.tick(2);
    h.tick(3);

    // start 0xb1 expands to -79 against a zero window. The upload carries
    // 90 tics: if a negative start were inserted offset by offset,
    // offsets 79..89 would land inside the window.
    let tics: Vec<(i16, doom_proto::TiccmdDiff)> = (0..90i16).map(|i| (i, diff(1))).collect();
    let actions = send(&mut h, alice, upload(2, 0xb1, tics));
    assert!(
        actions.is_empty(),
        "no gap generation from a negative start"
    );
    assert_eq!(
        active_count(&h),
        0,
        "nothing inserted from a negative start"
    );
    assert_eq!(acknowledged(&h, alice), 2, "the valid ack still applied");
}

#[test]
fn extratics_127_accepted_128_rejected() {
    // 128 cannot be served atomically from a 128-entry queue: rejected
    // as malformed at GAMESTART, with no adoption and no readiness.
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    h.launch(alice);
    h.ack(alice, 1);
    let mut settings = settings_value(0);
    settings.extratics = 128;
    let actions = h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader {
                reliable_seq: Some(1),
            },
            packet: ClientPacket::GameStart(settings),
        },
    );
    // Only the reliable acknowledgement; no adoption, no readiness.
    assert!(!actions.is_empty());
    assert!(actions.iter().all(|a| matches!(
        a,
        Action::Send {
            packet: ServerPacket::ReliableAck { .. },
            ..
        }
    )));
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    assert!(h.role.settings.is_none());
    assert!(!h.role.peers.get(&alice).expect("peer").ready);

    // 127 spans at most the whole queue: accepted, and the game starts.
    let mut h = Harness::new();
    let alice = h.join("a");
    h.syn(alice, "Alice");
    h.launch(alice);
    h.ack(alice, 1);
    let mut settings = settings_value(0);
    settings.extratics = 127;
    h.role.handle(
        T0,
        Input::Packet {
            player: alice,
            header: WireHeader {
                reliable_seq: Some(1),
            },
            packet: ClientPacket::GameStart(settings),
        },
    );
    assert_eq!(h.role.state(), ServerState::InGame);
    assert_eq!(h.role.settings.as_ref().expect("settings").extratics, 127);
}

#[test]
fn extratics_127_pumps_a_whole_128_tic_span() {
    let (mut h, alice, bob) = in_game_two_127();
    // Force the boundary state: deep acknowledgements keep the stall
    // guard out of the way, and alice's queue holds 0..=126 whole.
    for player in [alice, bob] {
        game_mut(&mut h, player).acknowledged = 100;
    }
    {
        let game = game_mut(&mut h, alice);
        game.sendseq = 127;
        for seq in 0..=126u32 {
            game.sendqueue[seq as usize] = Some(queued(seq as i16, seq));
        }
    }
    // Bob's tic 127 completes the recipient requirement at that slot.
    send(&mut h, bob, upload(0, 127, vec![(7, diff(9))]));

    let actions = h.tick(1);
    let to_alice = gamedata_to(&actions, alice);
    assert_eq!(to_alice.len(), 1);
    assert_eq!(to_alice[0].start, 0, "exact start of the 128-tic span");
    assert_eq!(to_alice[0].tics.len(), 128, "exact count of the span");
    assert_eq!(to_alice[0].tics[0].latency, 0);
    assert_eq!(to_alice[0].tics[126].latency, 126);
    // The fresh tic carries bob's diff at his frozen index.
    assert_eq!(to_alice[0].tics[127].latency, 7);
    assert_eq!(to_alice[0].tics[127].players, vec![(1, diff(9))]);
}

#[test]
fn send_tics_is_all_or_nothing_across_an_interior_hole() {
    let (mut h, alice) = in_game_one();
    {
        let game = game_mut(&mut h, alice);
        for seq in 0..=9u32 {
            game.sendqueue[seq as usize] = Some(queued(seq as i16, seq));
        }
        game.sendqueue[5] = None;
    }
    // The hole is interior: skipping it would compress the span under
    // its old start and alias the tail onto the wrong absolute tics.
    assert!(h.role.send_tics(alice, 0, 9).is_empty());

    // Filled, the same span emits exactly its ten entries.
    game_mut(&mut h, alice).sendqueue[5] = Some(queued(5, 5));
    let actions = h.role.send_tics(alice, 0, 9);
    let to_alice = gamedata_to(&actions, alice);
    assert_eq!(to_alice.len(), 1);
    assert_eq!(to_alice[0].start, 0);
    assert_eq!(to_alice[0].tics.len(), 10);
}

#[test]
fn removal_keeps_frozen_indices_and_reciprocal_bits() {
    let (mut h, alice, bob, carol) = in_game_three();
    send(&mut h, alice, upload(0, 0, vec![(1, diff(1))]));
    send(&mut h, bob, upload(0, 0, vec![(2, diff(2))]));
    send(&mut h, carol, upload(0, 0, vec![(3, diff(3))]));
    let actions = h.tick(1);
    // Baseline: reciprocal fan-out at the frozen indices 0, 1, 2.
    assert_eq!(
        gamedata_to(&actions, bob)[0].tics[0].players,
        vec![(0, diff(1)), (2, diff(3))]
    );

    // Advance once so the completed tic is consumed off the window:
    // nothing stale remains in any column to satisfy a wrong
    // completeness set after the removal.
    send(&mut h, alice, ClientPacket::GameDataAck { ack: 1 });
    send(&mut h, bob, ClientPacket::GameDataAck { ack: 1 });
    send(&mut h, carol, ClientPacket::GameDataAck { ack: 1 });
    h.tick(2);
    assert_eq!(h.role.recv.as_ref().expect("window").start, 1);

    // Player 0 leaves mid-game; the game continues with holes preserved.
    h.role.handle(T0, Input::Leave { player: alice });
    assert_eq!(h.role.state(), ServerState::InGame);

    send(&mut h, bob, upload(0, 1, vec![(4, diff(8))]));
    send(&mut h, carol, upload(0, 1, vec![(5, diff(9))]));
    let actions = h.tick(3);
    // The reciprocal fan-out bits stay at the frozen indices 1 and 2:
    // carol was not renumbered into the hole.
    let to_bob = gamedata_to(&actions, bob);
    assert_eq!(to_bob[0].tics[1].players, vec![(2, diff(9))]);
    let to_carol = gamedata_to(&actions, carol);
    assert_eq!(to_carol[0].tics[1].players, vec![(1, diff(8))]);

    // Advancement resumes with only the survivors' columns required.
    send(&mut h, bob, ClientPacket::GameDataAck { ack: 2 });
    send(&mut h, carol, ClientPacket::GameDataAck { ack: 2 });
    h.tick(4);
    assert_eq!(h.role.recv.as_ref().expect("window").start, 2);
    assert!(
        !h.role.recv.as_ref().expect("window").entries[0][0].active,
        "the old column 0 holds nothing and is ignored"
    );
}

#[test]
fn drone_gets_full_fanout_holds_min_ack_and_never_gets_requests() {
    let (mut h, alice, drone) = in_game_drone();
    send(&mut h, alice, upload(0, 0, vec![(11, diff(5))]));
    let actions = h.tick(1);
    // The drone receives the full fan-out: every player is "other" to a
    // drone, so alice's own command is included at her frozen index.
    let to_drone = gamedata_to(&actions, drone);
    assert_eq!(to_drone.len(), 1);
    assert_eq!(to_drone[0].start, 0);
    assert_eq!(to_drone[0].tics.len(), 1);
    assert_eq!(to_drone[0].tics[0].latency, 11);
    assert_eq!(to_drone[0].tics[0].players, vec![(0, diff(5))]);
    // The player herself gets the recipient-excluding empty merge.
    assert!(gamedata_to(&actions, alice)[0].tics[0].players.is_empty());

    // A drone upload is rejected outright: drones hold no receive slot.
    let before = active_count(&h);
    let actions = send(&mut h, drone, upload(0, 0, vec![(1, diff(9))]));
    assert!(actions.is_empty());
    assert_eq!(active_count(&h), before);

    // Alice alone cannot lift the minimum acknowledgement: the drone
    // participates in it even though it never uploads.
    send(&mut h, alice, ClientPacket::GameDataAck { ack: 1 });
    h.tick(2);
    assert_eq!(h.role.recv.as_ref().expect("window").start, 0);

    // Over a second of silence: no resend and no deadlock replay is ever
    // addressed to the drone (alice gets hers as usual).
    let actions = h.tick(1_500);
    assert!(resends_to(&actions, drone).is_empty());
    assert!(gamedata_to(&actions, drone).is_empty());
    assert!(!resends_to(&actions, alice).is_empty());

    // The drone's standalone acknowledgement is accepted and finally
    // lets the window advance past the completed tic.
    send(&mut h, drone, ClientPacket::GameDataAck { ack: 1 });
    h.tick(1_501);
    assert_eq!(h.role.recv.as_ref().expect("window").start, 1);
}

#[test]
fn resend_replays_queued_tics_verbatim_not_the_live_window() {
    let (mut h, alice, bob) = in_game_two();
    // Three of alice's tics queue for bob across three pumps; capture
    // the new tail tic of each emission.
    let mut originals: Vec<doom_proto::FullTic> = Vec::new();
    for step in 0..3u64 {
        h.role.handle(
            T0,
            Input::Packet {
                player: alice,
                header: WireHeader { reliable_seq: None },
                packet: upload(0, step as u8, vec![(step as i16, diff(5 + step as i8))]),
            },
        );
        let actions = h.tick(step + 1);
        let to_bob = gamedata_to(&actions, bob);
        assert_eq!(to_bob.len(), 1);
        originals.push(to_bob[0].tics.last().expect("tail tic").clone());
    }

    // A conflicting duplicate then rewrites alice's live window slot:
    // the live window and bob's queue now disagree.
    send(&mut h, alice, upload(0, 0, vec![(50, diff(99))]));
    assert_eq!(
        h.role.recv.as_ref().expect("window").entries[0][0]
            .diff
            .forward,
        Some(99)
    );

    // The resend replays the queued tics verbatim, in original order, so
    // a client reconstructing cumulatively (base plus diffs) rebuilds
    // exactly the sequence the server first sent.
    let actions = send(
        &mut h,
        bob,
        ClientPacket::GameDataResend { start: 0, count: 3 },
    );
    let resent = gamedata_to(&actions, bob);
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0].start, 0);
    assert_eq!(resent[0].tics, originals);
}

#[test]
fn zero_count_gamedata_runs_ack_and_gaps_but_zero_count_resend_is_inert() {
    let (mut h, alice) = in_game_one();
    h.tick(1);
    h.tick(2);
    h.tick(3);

    // GAMEDATA with no tics: acknowledgement and gap logic still run.
    let actions = send(&mut h, alice, upload(2, 6, vec![]));
    assert_eq!(acknowledged(&h, alice), 2);
    assert_eq!(resends_to(&actions, alice), vec![(0, 6)]);
    assert_eq!(active_count(&h), 0, "no tics inserted");

    // RESEND with count zero is a quiet whole-request ignore, even
    // though tics 0..2 are legitimately queued.
    let actions = send(
        &mut h,
        alice,
        ClientPacket::GameDataResend { start: 0, count: 0 },
    );
    assert!(actions.is_empty());
}

#[test]
fn wire_shapes_keep_the_server_client_asymmetry_explicit() {
    // The server GAMEDATA has no acknowledgement field at all: the type
    // cannot carry one, and a roundtrip preserves exactly start + tics.
    let server = ServerPacket::GameData(doom_proto::GameDataServer {
        start: 5,
        tics: vec![doom_proto::FullTic {
            latency: 2,
            players: vec![(1, diff(4))],
        }],
    });
    let bytes = server
        .encode(WireHeader { reliable_seq: None }, false)
        .expect("encode");
    let (_, decoded) = ServerPacket::decode(&bytes, false).expect("decode");
    assert_eq!(decoded, server);

    // The client resend request carries a full u32 start, not the low
    // byte the tic-number fields use.
    let resend = ClientPacket::GameDataResend {
        start: 0x1234_5678,
        count: 3,
    };
    let bytes = resend
        .encode(WireHeader { reliable_seq: None }, false)
        .expect("encode");
    let (_, decoded) = ClientPacket::decode(&bytes, false).expect("decode");
    assert_eq!(decoded, resend);

    // lowres_turn is the codec width switch for the angleturn field.
    let data = ClientPacket::GameData(doom_proto::GameDataClient {
        ack: 3,
        start: 5,
        tics: vec![doom_proto::ClientTic {
            latency: 1,
            diff: doom_proto::TiccmdDiff {
                turn: Some(0x100),
                ..Default::default()
            },
        }],
    });
    let wide = data
        .encode(WireHeader { reliable_seq: None }, false)
        .expect("encode");
    let narrow = data
        .encode(WireHeader { reliable_seq: None }, true)
        .expect("encode");
    assert!(
        wide.len() > narrow.len(),
        "lowres_turn narrows the angleturn field"
    );
}

// --- FU19/FU20 battery: discriminating regressions for the three
// surviving guard mutants, the six-tic deadlock tail, and the
// cumulative reconstruction oracle

/// Client-style cumulative oracle: apply each tic's partial diff for one
/// player onto that player's running absolute base (forward, turn).
fn reconstruct(tics: &[doom_proto::FullTic], player: u8) -> Vec<(i8, i16)> {
    let mut base = (0i8, 0i16);
    tics.iter()
        .map(|tic| {
            let (_, diff) = tic
                .players
                .iter()
                .find(|(index, _)| *index == player)
                .expect("player diff present");
            if let Some(forward) = diff.forward {
                base.0 = forward;
            }
            if let Some(turn) = diff.turn {
                base.1 = turn;
            }
            base
        })
        .collect()
}

#[test]
fn completeness_alone_blocks_advancement_when_all_acks_are_ahead() {
    let (mut h, alice, bob) = in_game_two();
    // Alice's tic 0 is present; bob's is not. With the ratified ACK
    // ceiling the partner of a laggard can never acknowledge past the
    // stall through packets, so the only way to isolate the completeness
    // gate from the minimum acknowledgement is to set both ACKs ahead
    // directly (proto probe `im_completeness_gate_proof`).
    send(&mut h, alice, upload(0, 0, vec![(1, diff(1))]));
    game_mut(&mut h, alice).acknowledged = 1;
    game_mut(&mut h, bob).acknowledged = 1;
    h.tick(1);
    assert_eq!(
        h.role.recv.as_ref().expect("window").start,
        0,
        "completeness alone blocks advancement with min-ack satisfied"
    );

    // The exact missing tic arrives: advancement resumes.
    send(&mut h, bob, upload(0, 0, vec![(1, diff(2))]));
    h.tick(2);
    assert_eq!(h.role.recv.as_ref().expect("window").start, 1);
}

#[test]
fn resend_request_is_inert_with_a_wrong_absolute_identity_in_slot() {
    let (mut h, alice) = in_game_one();
    h.tick(1);
    // The requested modulo slot holds a QueuedTic, but its absolute
    // sequence belongs to a later tic: a stale request for tic 0 must
    // not alias it (proto probe `im_resend_aliased_slot_ignored`).
    game_mut(&mut h, alice).sendqueue[0] = Some(queued(0, 128));
    let actions = send(
        &mut h,
        alice,
        ClientPacket::GameDataResend { start: 0, count: 1 },
    );
    assert!(
        actions.is_empty(),
        "a wrong absolute identity in the right slot is inert"
    );

    // The same entry serves a request whose identity does match.
    let actions = send(
        &mut h,
        alice,
        ClientPacket::GameDataResend {
            start: 128,
            count: 1,
        },
    );
    let to_alice = gamedata_to(&actions, alice);
    assert_eq!(to_alice.len(), 1);
    assert_eq!(to_alice[0].start, 128);
    assert_eq!(to_alice[0].tics.len(), 1);
}

#[test]
fn deadlock_boundary_is_strictly_over_1000ms_with_full_six_tic_tail() {
    let (mut h, alice, bob) = in_game_two();
    // Alice uploads tics 0..=126 at T0: her only missing slot is 127,
    // never stamped, so the 300 ms path stays silent for it. Bob's
    // single tic lets alice pump exactly once, so her queue holds one
    // unacknowledged tic for the replay.
    send(&mut h, bob, upload(0, 0, vec![(9, diff(7))]));
    let tics: Vec<(i16, doom_proto::TiccmdDiff)> = (0..127i16).map(|i| (i, diff(1))).collect();
    send(&mut h, alice, upload(0, 0, tics));
    h.tick(1);
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert!(recv.entries[126][0].active);
        assert!(!recv.entries[127][0].active);
    }
    // Bob never uploads again, so alice never pumps: every resend and
    // gamedata addressed to alice below is deadlock-originated.

    // Exactly 1000 ms is not strictly over the threshold: quiet (proto
    // probe `im_deadlock_strict_boundary`).
    let actions = h.tick(1_000);
    assert!(
        resends_to(&actions, alice).is_empty(),
        "1000 ms is not over the deadlock threshold"
    );
    assert!(gamedata_to(&actions, alice).is_empty());

    // 1001 ms: the full six-tic wire request at the tail, plus the exact
    // unacknowledged queue replay; only in-window slots are stamped.
    let actions = h.tick(1_001);
    assert_eq!(
        resends_to(&actions, alice),
        vec![(127, 6)],
        "the wire request always covers six tics even past the tail"
    );
    let replay = gamedata_to(&actions, alice);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].tics[0].players, vec![(1, diff(7))]);
    {
        let recv = h.role.recv.as_ref().expect("window");
        assert_eq!(
            recv.entries[127][0].resend_time,
            Some(Milliseconds(T0.0 + 1_001)),
            "only the in-window slot is stamped"
        );
        assert_eq!(recv.entries[126][0].resend_time, None);
    }
}

#[test]
fn resend_preserves_cumulative_base_plus_diff_reconstruction() {
    let (mut h, alice, bob) = in_game_two();
    // Three partial diffs whose absolute meaning depends on the running
    // per-player base: forward set once, turn carried across a tic that
    // does not mention it, then forward changed alone.
    let diffs = [
        doom_proto::TiccmdDiff {
            forward: Some(50),
            turn: Some(0x100),
            ..Default::default()
        },
        doom_proto::TiccmdDiff {
            turn: Some(0x200),
            ..Default::default()
        },
        doom_proto::TiccmdDiff {
            forward: Some(-20),
            ..Default::default()
        },
    ];
    let mut oracle_base = (0i8, 0i16);
    let mut oracle = Vec::new();
    for d in &diffs {
        if let Some(forward) = d.forward {
            oracle_base.0 = forward;
        }
        if let Some(turn) = d.turn {
            oracle_base.1 = turn;
        }
        oracle.push(oracle_base);
    }
    assert_eq!(oracle, vec![(50, 0x100), (50, 0x200), (-20, 0x200)]);

    // Queue the three tics for bob across three pumps and reconstruct
    // the absolute commands from the live fan-out.
    let mut live = Vec::new();
    for (step, d) in diffs.iter().enumerate() {
        send(&mut h, alice, upload(0, step as u8, vec![(1, d.clone())]));
        let actions = h.tick(step as u64 + 1);
        live.push(
            gamedata_to(&actions, bob)[0]
                .tics
                .last()
                .expect("tail")
                .clone(),
        );
    }
    assert_eq!(reconstruct(&live, 0), oracle, "live fan-out reconstructs");

    // A conflicting duplicate poisons the live window; the verbatim
    // resend must still reconstruct the same absolute commands.
    send(
        &mut h,
        alice,
        upload(
            0,
            0,
            vec![(
                50,
                doom_proto::TiccmdDiff {
                    forward: Some(127),
                    ..Default::default()
                },
            )],
        ),
    );
    let actions = send(
        &mut h,
        bob,
        ClientPacket::GameDataResend { start: 0, count: 3 },
    );
    let resent = gamedata_to(&actions, bob)[0].tics.clone();
    assert_eq!(
        reconstruct(&resent, 0),
        oracle,
        "the resend reconstructs the same absolute commands"
    );
}
