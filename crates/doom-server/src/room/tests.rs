use super::*;

use doom_proto::{ConnectData, GameSettings, ServerPacket, Syn, TiccmdDiff};

use crate::server_role::ServerState;

const T0: Milliseconds = Milliseconds(10_000);

fn host(capacity: usize) -> RoomHost<u32> {
    RoomHost::new(
        RoomName::try_from("e1m1").expect("valid room"),
        capacity.try_into().expect("nonzero capacity"),
        1,
    )
}

fn join(host: &mut RoomHost<u32>) -> PlayerId {
    let (player, _) = host
        .join(T0, |player| format!("ws:{}", player.get()).into_bytes())
        .expect("join succeeds");
    player
}

fn syn(name: &str) -> ClientPacket {
    ClientPacket::Syn(Syn {
        version: b"Chocolate Doom 3.1.1".to_vec(),
        protocols: vec![b"CHOCOLATE_DOOM_0".to_vec()],
        connect: ConnectData {
            gamemode: 0,
            gamemission: 0,
            lowres_turn: 0,
            drone: 0,
            max_players: 4,
            is_freedoom: 0,
            wad_sha1: [7; 20],
            deh_sha1: [8; 20],
            player_class: 0,
        },
        player_name: name.as_bytes().to_vec(),
    })
}

fn syn_lowres(name: &str) -> ClientPacket {
    let ClientPacket::Syn(mut syn) = syn(name) else {
        unreachable!("syn builds a syn");
    };
    syn.connect.lowres_turn = 1;
    ClientPacket::Syn(syn)
}

fn settings(deathmatch: u8, lowres_turn: u8) -> GameSettings {
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
        lowres_turn,
        new_sync: 1,
        timelimit: 0,
        loadgame: -1,
        random: 0,
        consoleplayer: 0,
        player_classes: vec![0],
    }
}

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

fn syn_drone_lowres(name: &str) -> ClientPacket {
    let ClientPacket::Syn(mut syn) = syn_lowres(name) else {
        unreachable!()
    };
    syn.connect.drone = 1;
    ClientPacket::Syn(syn)
}

fn diff_turn() -> doom_proto::TiccmdDiff {
    doom_proto::TiccmdDiff {
        turn: Some(0x100),
        ..Default::default()
    }
}

fn plain() -> WireHeader {
    WireHeader { reliable_seq: None }
}

fn reliable(seq: u8) -> WireHeader {
    WireHeader {
        reliable_seq: Some(seq),
    }
}

fn launch(host: &mut RoomHost<u32>, player: PlayerId) -> HostEffect<u32> {
    host.packet(T0, player, reliable(0), ClientPacket::Launch)
}

fn ack(host: &mut RoomHost<u32>, player: PlayerId, next_seq: u8) -> HostEffect<u32> {
    host.packet(T0, player, plain(), ClientPacket::ReliableAck { next_seq })
}

fn gamestart(
    host: &mut RoomHost<u32>,
    player: PlayerId,
    seq: u8,
    deathmatch: u8,
) -> HostEffect<u32> {
    host.packet(
        T0,
        player,
        reliable(seq),
        ClientPacket::GameStart(settings(deathmatch, 0)),
    )
}

fn pop(host: &mut RoomHost<u32>, player: PlayerId) -> (u32, ServerPacket) {
    let packet = host.pop_outbound(player).expect("queued packet");
    let (_, decoded) =
        ServerPacket::decode(packet.payload(), host.lowres_turn()).expect("host output decodes");
    (*packet.metadata(), decoded)
}

fn pop_waiting_data(host: &mut RoomHost<u32>, player: PlayerId) -> doom_proto::WaitData {
    while let Some(packet) = host.pop_outbound(player) {
        if let ServerPacket::WaitingData(data) = ServerPacket::decode(packet.payload(), false)
            .expect("decodes")
            .1
        {
            return data;
        }
    }
    panic!("no waiting data queued for {player:?}");
}

fn drain(host: &mut RoomHost<u32>, player: PlayerId) {
    while host.pop_outbound(player).is_some() {}
}

#[test]
fn relay_metadata_equal_to_server_metadata_stays_opaque() {
    let mut h = host(8);
    let alice = join(&mut h);
    let bob = join(&mut h);

    let (outcome, _) = h
        .relay(T0, alice, bob, 1, b"opaque relay")
        .expect("relay succeeds");
    assert_eq!(outcome, RelayOutcome::Queued(bob));
    h.packet(T0, bob, plain(), syn("Bob"));

    let relay = h.pop_outbound(bob).expect("relay remains first");
    assert_eq!(relay.payload(), b"opaque relay");
    let first_host = h.pop_outbound(bob).expect("SYN_ACCEPT follows");
    let second_host = h.pop_outbound(bob).expect("WAITING_DATA follows");
    assert_eq!(
        [relay.lowres(), first_host.lowres(), second_host.lowres()],
        [None, Some(false), Some(false)],
        "metadata equality must not shift host tags onto opaque relays"
    );
}

#[test]
fn join_then_syn_queues_accept_and_first_waiting_data_from_the_host() {
    let mut h = host(8);
    let alice = join(&mut h);

    let effect = h.packet(T0, alice, plain(), syn("Alice"));
    assert_eq!(effect.wakes, vec![alice]);
    assert!(effect.disconnects.is_empty());
    assert!(!effect.game_ended);

    let (metadata, accept) = pop(&mut h, alice);
    assert_eq!(metadata, 1, "host-originated sends carry the host route");
    assert!(matches!(accept, ServerPacket::SynAccept(_)));
    let (metadata, data) = pop(&mut h, alice);
    assert_eq!(metadata, 1);
    match data {
        ServerPacket::WaitingData(data) => {
            assert_eq!(data.players.len(), 1);
            assert_eq!(data.is_controller, 1);
            assert_eq!(data.consoleplayer, 0);
        }
        other => panic!("expected waiting data, got {other:?}"),
    }
    assert!(h.pop_outbound(alice).is_none());
}

#[test]
fn gamestart_reaches_everyone_with_per_recipient_consoleplayer() {
    let mut h = host(8);
    let alice = join(&mut h);
    let bob = join(&mut h);
    h.packet(T0, alice, plain(), syn("Alice"));
    h.packet(T0, bob, plain(), syn("Bob"));
    launch(&mut h, alice);
    // Drain both reliable chains so the GAMESTART broadcast can emit.
    for player in [alice, bob] {
        ack(&mut h, player, 1);
        ack(&mut h, player, 2);
    }
    drain(&mut h, alice);
    drain(&mut h, bob);

    gamestart(&mut h, alice, 1, 1);
    // The controller's GAMESTART only adopts settings and marks the
    // controller ready: a reliable ack plus a refreshed waiting data to
    // the ready peer, and no authoritative broadcast while bob is
    // unready.
    let mut acked = false;
    let mut refreshed = false;
    while let Some(packet) = h.pop_outbound(alice) {
        match ServerPacket::decode(packet.payload(), false)
            .expect("decodes")
            .1
        {
            ServerPacket::ReliableAck { .. } => acked = true,
            ServerPacket::WaitingData(data) => {
                refreshed = true;
                assert_eq!(data.ready_players, 1);
            }
            ServerPacket::GameStart(_) => {
                panic!("no authoritative broadcast while a peer is unready")
            }
            _ => {}
        }
    }
    assert!(acked, "the controller's reliable GAMESTART is acked");
    assert!(refreshed, "ready peers are refreshed");
    assert!(h.pop_outbound(bob).is_none(), "bob gets nothing yet");

    // The second peer's GAMESTART completes readiness: the personalized
    // authoritative GAMESTART goes to both.
    gamestart(&mut h, bob, 0, 0);
    assert_eq!(h.role.state(), ServerState::InGame);

    let mut starts = Vec::new();
    while let Some(packet) = h.pop_outbound(alice) {
        if let ServerPacket::GameStart(settings) = ServerPacket::decode(packet.payload(), false)
            .expect("decodes")
            .1
        {
            starts.push(settings);
        }
    }
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].consoleplayer, 0);
    assert_eq!(starts[0].deathmatch, 1);

    let mut starts = Vec::new();
    while let Some(packet) = h.pop_outbound(bob) {
        if let ServerPacket::GameStart(settings) = ServerPacket::decode(packet.payload(), false)
            .expect("decodes")
            .1
        {
            starts.push(settings);
        }
    }
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].consoleplayer, 1);
    assert_eq!(starts[0].deathmatch, 1);
}

#[test]
fn lowres_room_encodes_narrow_fanout_that_wrong_width_cannot_read() {
    let mut h = host(8);
    let alice = join(&mut h);
    let bob = join(&mut h);
    // A recording player negotiates the narrow width in its SYN
    // connect data; the start recomputes the settings value from the
    // players, never from the settings byte alone.
    h.packet(T0, alice, plain(), syn_lowres("Alice"));
    h.packet(T0, bob, plain(), syn_lowres("Bob"));
    launch(&mut h, alice);
    for player in [alice, bob] {
        ack(&mut h, player, 1);
        ack(&mut h, player, 2);
    }
    gamestart(&mut h, alice, 1, 0);
    gamestart(&mut h, bob, 0, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    assert!(h.lowres_turn(), "the negotiated width governs the codec");
    drain(&mut h, alice);
    drain(&mut h, bob);

    // A narrow nonzero angleturn uploads fine (the binding decoded it
    // with the room value before handing it over).
    h.packet(
        T0,
        alice,
        plain(),
        ClientPacket::GameData(doom_proto::GameDataClient {
            ack: 0,
            start: 0,
            tics: vec![doom_proto::ClientTic {
                latency: 1,
                diff: TiccmdDiff {
                    turn: Some(0x100),
                    ..Default::default()
                },
            }],
        }),
    );
    h.tick(Milliseconds(T0.0 + 1));

    let packet = h.pop_outbound(bob).expect("fan-out queued");
    let (_, decoded) =
        ServerPacket::decode(packet.payload(), true).expect("narrow fan-out decodes");
    match decoded {
        ServerPacket::GameData(data) => {
            assert_eq!(data.tics[0].players[0].1.turn, Some(0x100));
        }
        other => panic!("expected gamedata, got {other:?}"),
    }
    assert!(
        ServerPacket::decode(packet.payload(), false).is_err(),
        "the same bytes cannot be read at the wrong width"
    );
}

#[test]
fn host_origin_slow_consumer_feeds_leave_into_the_role_once() {
    let mut h = host(4);
    let bob = join(&mut h);
    let alice = join(&mut h);
    // Bob's outbox reaches the exact bound: his own SYN accept and first
    // waiting data, then two relayed packets from alice.
    h.packet(T0, bob, plain(), syn("Bob"));
    h.packet(T0, alice, plain(), syn("Alice"));
    drain(&mut h, alice);
    for payload in [b"a".as_slice(), b"b".as_slice()] {
        let (outcome, _) = h
            .relay(T0, alice, bob, 11, payload)
            .expect("relay succeeds");
        assert_eq!(outcome, RelayOutcome::Queued(bob));
    }

    // A zero-production pass first: bob merely has packets queued, and
    // the policy is next-packet-removes, never proactive eviction.
    let effect = h.tick(Milliseconds(T0.0 + 500));
    assert!(effect.disconnects.is_empty());
    assert!(h.contains(bob));

    // The next host-originated send to bob (the waiting-data cadence)
    // removes him, and the role is told in the same batch. Alice has
    // headroom and survives the cascade.
    let effect = h.tick(Milliseconds(T0.0 + 1_001));
    assert_eq!(effect.disconnects, vec![(bob, None)]);
    assert!(!h.contains(bob));
    assert!(h.contains(alice));
    assert!(!effect.game_ended, "lobby removal ends no game");

    // No ghost peer: alice's next lobby update lists only herself.
    drain(&mut h, alice);
    h.tick(Milliseconds(T0.0 + 2_002));
    assert_eq!(pop_waiting_data(&mut h, alice).players.len(), 1);
}

#[test]
fn terminal_rejection_rides_the_disconnect_effect_not_the_outbox() {
    let mut h = host(8);
    let player = join(&mut h);

    // Pre-SYN old magic: the terminal REJECTED is handed to the binding
    // with the removal, atomic, and nothing is queued behind it.
    let effect = h.malformed(T0, player, MalformedClass::Syn { old_magic: true });
    assert_eq!(effect.disconnects.len(), 1);
    let (disconnected, terminal) = &effect.disconnects[0];
    assert_eq!(*disconnected, player);
    let terminal = terminal.as_ref().expect("terminal frame owed");
    let (_, packet) = ServerPacket::decode(terminal, false).expect("terminal decodes");
    match packet {
        ServerPacket::Rejected { reason } => assert!(
            reason.starts_with(b"You are using an old client version that is not supported by this server. This server is running ")
        ),
        other => panic!("expected rejected, got {other:?}"),
    }
    assert!(h.pop_outbound(player).is_none());
    assert!(!h.contains(player));

    // Established old magic: plain REJECTED through the outbox, peer
    // retained, no disconnect effect.
    let alice = join(&mut h);
    h.packet(T0, alice, plain(), syn("Alice"));
    drain(&mut h, alice);
    let effect = h.malformed(T0, alice, MalformedClass::Syn { old_magic: true });
    assert!(effect.disconnects.is_empty());
    let (_, packet) = pop(&mut h, alice);
    assert!(matches!(packet, ServerPacket::Rejected { .. }));
    assert!(h.contains(alice));
}

#[test]
fn valid_timer_batch_never_counts_as_consumer_backlog() {
    let mut h = host(64);
    let alice = join(&mut h);
    h.packet(T0, alice, plain(), syn("Alice"));
    launch(&mut h, alice);
    ack(&mut h, alice, 1);
    ack(&mut h, alice, 2);
    gamestart(&mut h, alice, 1, 0);
    assert_eq!(h.role.state(), ServerState::InGame);
    // The authoritative GAMESTART stays the unacknowledged reliable
    // head; everything else is drained.
    drain(&mut h, alice);

    // Seed the 64 alternating missing slots 0,2,...,126 through valid
    // odd-slot uploads, draining every resulting request immediately:
    // every even slot ends up missing and stamped, just past expiry.
    for start in (1..=127u8).step_by(2) {
        let effect = h.packet(T0, alice, plain(), upload(0, start, vec![(1, diff(1))]));
        assert!(effect.disconnects.is_empty());
        drain(&mut h, alice);
    }
    assert!(
        h.pop_outbound(alice).is_none(),
        "outbox is empty before the tick"
    );

    // Refresh the deadlock clock at +1000 ms without a send: a
    // duplicate upload onto an active slot stores (resetting the
    // clock) and requests nothing, since the slot behind it is already
    // stamped. Then tick at +1001: keepalive, the GAMESTART retry, the
    // pump, and the 64 expired contiguous resend runs are one valid
    // producer batch.
    let effect = h.packet(
        Milliseconds(T0.0 + 1_000),
        alice,
        plain(),
        upload(0, 1, vec![(9, diff(9))]),
    );
    assert_eq!(effect, HostEffect::default());
    let effect = h.tick(Milliseconds(T0.0 + 1_001));

    assert!(effect.disconnects.is_empty(), "no valid peer is removed");
    assert!(h.contains(alice));

    let mut resends = 0;
    let mut retries = 0;
    let mut keepalives = 0;
    let mut gamedata = 0;
    while let Some(packet) = h.pop_outbound(alice) {
        match ServerPacket::decode(packet.payload(), false)
            .expect("decodes")
            .1
        {
            ServerPacket::GameDataResend { .. } => resends += 1,
            ServerPacket::GameStart(_) => retries += 1,
            ServerPacket::Keepalive => keepalives += 1,
            ServerPacket::GameData(_) => gamedata += 1,
            other => panic!("unexpected packet {other:?}"),
        }
    }
    assert_eq!(resends, 64, "every expired run re-requests");
    assert_eq!(retries, 1);
    assert_eq!(keepalives, 1);
    assert_eq!(gamedata, 1);
}

#[test]
fn cleanup_after_a_role_originated_removal_is_inert() {
    let mut h = host(8);
    let player = join(&mut h);

    // The role removes the pre-SYN member itself, the terminal riding
    // the removal; the registry member goes with the same batch.
    let effect = h.malformed(T0, player, MalformedClass::Syn { old_magic: true });
    assert_eq!(effect.disconnects.len(), 1);
    assert!(!h.contains(player));

    // The later transport cleanup is inert by contract: a default
    // effect, zero additional Leaves.
    assert_eq!(h.leave(T0, player), HostEffect::default());

    // The contract is structural, not the role's tolerance of unknown
    // leaves: with the registry member gone first (as a binding's own
    // cleanup ordering could leave behind), a guarded leave still
    // never reaches the role, so the live role peer is untouched.
    let live = join(&mut h);
    assert_eq!(h.role.peer_count(), 1);
    h.registry.leave(live);
    assert_eq!(h.leave(T0, live), HostEffect::default());
    assert_eq!(h.role.peer_count(), 1, "the role peer is untouched");
}

#[test]
fn transport_leave_mid_startup_aborts_with_game_ended_and_room_persists() {
    let mut h = host(8);
    let alice = join(&mut h);
    let bob = join(&mut h);
    h.packet(T0, alice, plain(), syn("Alice"));
    h.packet(T0, bob, plain(), syn("Bob"));
    launch(&mut h, alice);
    assert_eq!(h.role.state(), ServerState::WaitingStart);
    // Drain both reliable chains so the abort broadcast can emit.
    for player in [alice, bob] {
        ack(&mut h, player, 1);
        ack(&mut h, player, 2);
    }

    let effect = h.leave(T0, alice);
    assert!(effect.game_ended, "startup abort ends the game");
    assert_eq!(effect.disconnects, vec![(alice, None)]);
    assert!(!h.is_empty(), "the room persists while bob remains");

    // The abort broadcast reaches the survivor through the outbox.
    let mut saw_abort = false;
    while let Some(packet) = h.pop_outbound(bob) {
        if let ServerPacket::ConsoleMessage { message } =
            ServerPacket::decode(packet.payload(), false)
                .expect("decodes")
                .1
        {
            saw_abort |= message.starts_with(b"Game startup aborted because player 'Alice'");
        }
    }
    assert!(saw_abort, "abort console message reaches bob");
}

#[test]
fn timer_drives_waiting_data_cadence_and_reliable_retry() {
    let mut h = host(8);
    let alice = join(&mut h);
    h.packet(T0, alice, plain(), syn("Alice"));

    // Exactly one second is not over the cadence: quiet.
    let effect = h.tick(Milliseconds(T0.0 + 1_000));
    assert!(effect.wakes.is_empty());

    // Past it: the second waiting data and the retry of the
    // unacknowledged reliable SYN accept leave through the same outbox.
    let effect = h.tick(Milliseconds(T0.0 + 1_001));
    assert_eq!(effect.wakes, vec![alice]);
    let mut accepts = 0;
    let mut waiting = 0;
    let mut keepalives = 0;
    while let Some(packet) = h.pop_outbound(alice) {
        match ServerPacket::decode(packet.payload(), false)
            .expect("decodes")
            .1
        {
            ServerPacket::SynAccept(_) => accepts += 1,
            ServerPacket::WaitingData(_) => waiting += 1,
            ServerPacket::Keepalive => keepalives += 1,
            other => panic!("unexpected packet {other:?}"),
        }
    }
    assert_eq!(accepts, 2, "the unacknowledged head retries once");
    assert_eq!(waiting, 2, "the cadence produced a second waiting data");
    assert_eq!(keepalives, 1, "the send-idle keepalive fires");
}

#[test]
fn relay_slow_consumer_feeds_leave_into_the_role_once() {
    let mut h = host(4);
    let bob = join(&mut h);
    let alice = join(&mut h);
    h.packet(T0, bob, plain(), syn("Bob"));
    h.packet(T0, alice, plain(), syn("Alice"));
    drain(&mut h, alice);

    for payload in [b"a".as_slice(), b"b".as_slice()] {
        let (outcome, _) = h
            .relay(T0, alice, bob, 11, payload)
            .expect("relay succeeds");
        assert_eq!(outcome, RelayOutcome::Queued(bob));
    }

    // Relay metadata survived on the queued packets.
    let mut relayed = Vec::new();
    while let Some(packet) = h.pop_outbound(bob) {
        relayed.push((*packet.metadata(), packet.payload().to_vec()));
    }
    assert_eq!(relayed.len(), 4);
    assert_eq!(relayed[2], (11, b"a".to_vec()));
    assert_eq!(relayed[3], (11, b"b".to_vec()));

    // Refill to the bound; the next relay removes bob and tells the
    // role in the same batch.
    for payload in [
        b"c".as_slice(),
        b"d".as_slice(),
        b"e".as_slice(),
        b"f".as_slice(),
    ] {
        let (outcome, _) = h
            .relay(T0, alice, bob, 11, payload)
            .expect("relay succeeds");
        assert_eq!(outcome, RelayOutcome::Queued(bob));
    }
    let (outcome, effect) = h
        .relay(T0, alice, bob, 11, b"overflow")
        .expect("overflow applies the policy");
    assert_eq!(outcome, RelayOutcome::SlowConsumerDisconnected(bob));
    assert_eq!(effect.disconnects, vec![(bob, None)]);
    assert!(!h.contains(bob));

    // The role holds no ghost: alice's next lobby update lists only
    // herself.
    h.tick(Milliseconds(T0.0 + 1_001));
    assert_eq!(pop_waiting_data(&mut h, alice).players.len(), 1);
}

// The removal effect preserves the exact queued prefix, FIFO, with
// each packet's binding metadata and codec tag, then the terminal
// (proto followup-lead-server-23 addenda 3-5). A mutation that removes
// the player from the registry before the capture must fail this.
#[test]
fn removal_effect_carries_the_owed_prefix_with_metadata_and_tags() {
    let mut h = host(16);
    let alice = join(&mut h);
    let target = join(&mut h);

    // Queue, in order, for the still pre-SYN target: one opaque relay
    // from a non-server route, then two distinguishable host
    // QUERY_RESPONSE packets (the room gains a connected player
    // between them, so the reported count changes).
    let relay_payload = b"relay-through-route-20";
    let (outcome, _) = h
        .relay(T0, alice, target, 20, relay_payload)
        .expect("relay queues");
    assert_eq!(outcome, RelayOutcome::Queued(target));
    let first_query = h.packet(T0, target, plain(), ClientPacket::Query);
    assert!(first_query.wakes.contains(&target));
    h.packet(T0, alice, plain(), syn("Alice"));
    let second_query = h.packet(T0, target, plain(), ClientPacket::Query);
    assert!(second_query.wakes.contains(&target));

    // The old-magic classification is a terminal REJECTED removal for
    // the pre-SYN target.
    let effect = h.malformed(T0, target, MalformedClass::Syn { old_magic: true });

    // The target is already absent from membership, and the effect
    // still carries its exact owed prefix.
    assert!(!h.contains(target));
    let owed = effect
        .removal_owed
        .iter()
        .find(|(player, _)| *player == target)
        .map(|(_, packets)| packets)
        .expect("the owed prefix is captured");
    assert_eq!(owed.len(), 3);
    // The relay keeps its non-server route and no codec context.
    assert_eq!(owed[0].metadata(), &20);
    assert_eq!(owed[0].payload(), relay_payload);
    assert_eq!(owed[0].lowres(), None);
    // The two host responses keep the server route and their
    // production-time (pre-gamestart, wide) codec tag.
    assert_eq!(owed[1].metadata(), &1);
    assert_eq!(owed[1].lowres(), Some(false));
    assert_eq!(owed[2].metadata(), &1);
    assert_eq!(owed[2].lowres(), Some(false));
    let (_, ServerPacket::QueryResponse(first)) =
        ServerPacket::decode(owed[1].payload(), false).expect("first decodes")
    else {
        panic!("query response")
    };
    let (_, ServerPacket::QueryResponse(second)) =
        ServerPacket::decode(owed[2].payload(), false).expect("second decodes")
    else {
        panic!("query response")
    };
    assert_eq!(first.num_players, 0);
    assert_eq!(second.num_players, 1);
    assert_ne!(
        owed[1].payload(),
        owed[2].payload(),
        "the prefix packets are distinguishable"
    );
    // The terminal REJECTED rides the removal, after the prefix.
    let terminal = effect
        .disconnects
        .iter()
        .find(|(player, _)| *player == target)
        .and_then(|(_, terminal)| terminal.as_ref())
        .expect("the terminal rides the removal");
    assert!(matches!(
        ServerPacket::decode(terminal, false)
            .expect("terminal decodes")
            .1,
        ServerPacket::Rejected { .. }
    ));
    // Nothing further can be sent to the removed target.
    assert!(h.pop_outbound(target).is_none());
}

// Addenda 3/4 (followup-lead-server-23), reconstructed per addendum 2
// of followup-lead-server-24: one timer pass emits the drone's
// GAMEDATA while the room is lowres, then times out the room's only
// player, and end_game resets the live width to wide in the same
// action list. The GAMEDATA must still encode at its production width:
// the action carries the tag, and the reducer never re-reads the live
// role width. A mutation encoding with the live (post-reset) width
// must fail this test.
//
// The producer drone is admitted LAST, into the reused slot 0: it is
// the highest PlayerId but the first protocol slot, so the pinned
// slot-order timer visits it before the player and the fan-out is
// produced ahead of the reset. A mutation restoring ascending-PlayerId
// traversal visits the player first, ends the game, and the drone
// never pumps — failing this test by name, a two-order discriminator
// in its own right.
#[test]
fn gamedata_produced_before_a_same_batch_reset_encodes_at_production_width() {
    let mut h = host(16);
    // Slot-reuse setup: the first player takes slot 0 and leaves with
    // the second player present, so the drone — joined last, the
    // highest PlayerId — is handed the reused slot 0 at acceptance and
    // becomes the first protocol slot. Everyone negotiates lowres.
    let departing = join(&mut h);
    let player = join(&mut h);
    let drone = join(&mut h);
    h.packet(T0, departing, plain(), syn_lowres("Departing"));
    h.packet(T0, player, plain(), syn_lowres("Player"));
    // Slot 0 frees with a player still present: no abort, no end-game.
    h.leave(T0, departing);
    h.packet(T0, drone, plain(), syn_drone_lowres("Observer"));
    h.packet(T0, player, reliable(0), ClientPacket::Launch);
    h.packet(
        T0,
        player,
        reliable(1),
        ClientPacket::GameStart(settings(0, 1)),
    );
    h.packet(
        T0,
        drone,
        reliable(0),
        ClientPacket::GameStart(settings(0, 1)),
    );
    assert!(h.lowres_turn(), "the room adopted lowres");

    // Playable tics from the player, drained from the drone's outbox,
    // so the pump's sendqueue holds entries and the final tick's
    // current slot is present.

    for tic in 0..6u8 {
        h.packet(
            T0,
            player,
            plain(),
            upload(0, tic, vec![(tic as i16, diff_turn())]),
        );
        h.tick(Milliseconds(T0.0 + 30 * u64::from(tic)));
    }
    while h.pop_outbound(drone).is_some() {}
    // One more upload covers the slot the final tick's pump needs.
    h.packet(T0, player, plain(), upload(0, 6, vec![(6, diff_turn())]));
    // The drone stays fresh; the player goes silent. At 25 s the drone
    // pings; past 30 s the player has timed out but the drone has not.
    h.packet(
        Milliseconds(T0.0 + 25_000),
        drone,
        plain(),
        ClientPacket::Keepalive,
    );
    // One timer pass: the drone's pump emits first (still lowres),
    // then the player's timeout ends the game and initiates the
    // drone's disconnect, in the same reduction.
    let effect = h.tick(Milliseconds(T0.0 + 31_000));
    assert!(effect.game_ended, "the only player timed out");
    assert!(!h.lowres_turn(), "the role reset to wide");

    // The drone's final fan-out encodes at its production width even
    // though the role finished the batch wide.
    let mut gamedata = None;
    while let Some(packet) = h.pop_outbound(drone) {
        if matches!(
            ServerPacket::decode(packet.payload(), true),
            Ok((_, ServerPacket::GameData(_)))
        ) {
            gamedata = Some(packet);
        }
    }
    let gamedata = gamedata.expect("the drone had a fan-out queued");
    assert_eq!(
        gamedata.lowres(),
        Some(true),
        "the tag is the production width"
    );
    let (_, ServerPacket::GameData(data)) =
        ServerPacket::decode(gamedata.payload(), true).expect("decodes lowres")
    else {
        unreachable!()
    };
    assert!(
        data.tics
            .iter()
            .any(|tic| tic.players.iter().any(|(_, d)| d.turn == Some(0x100))),
        "the narrow turn survived"
    );
    assert!(
        ServerPacket::decode(gamedata.payload(), false).is_err(),
        "the bytes are not wide-encoded"
    );
}

// Engine-33 item 3 / addendum 6: the exact-capacity recursive-leave
// construction. One reduce batch overflows the last player at the
// outbox bound, her recursive Leave ends the game and resets the width
// MID-REDUCE, and the drone's GAMEDATA sits later in the same batch
// (deterministic lowest-PlayerId-first timer order). It must still
// encode at its production width. A mutation reading the live role
// width at encode time fails this test by name.
#[test]
fn recursive_leave_flips_width_mid_reduce_but_gamedata_keeps_production_width() {
    let mut h = host(4);
    // The player has the lower PlayerId, the drone second; the player
    // SYNs first (upstream rejects a first-SYN drone).
    let alice = join(&mut h);
    let drone = join(&mut h);
    h.packet(T0, alice, plain(), syn_lowres("Alice"));
    h.packet(T0, drone, plain(), syn_drone_lowres("Observer"));
    h.packet(T0, alice, reliable(0), ClientPacket::Launch);
    h.packet(
        T0,
        alice,
        reliable(1),
        ClientPacket::GameStart(settings(0, 1)),
    );
    h.packet(
        T0,
        drone,
        reliable(0),
        ClientPacket::GameStart(settings(0, 1)),
    );
    assert!(h.lowres_turn(), "the room adopted lowres");
    while h.pop_outbound(alice).is_some() {}

    // Fill alice's outbox to exactly the bound: she is still a member,
    // and the next host send removes her as a slow consumer.
    for _ in 0..4 {
        let (outcome, _) = h.relay(T0, drone, alice, 20, b"fill").expect("fill relays");
        assert_eq!(outcome, RelayOutcome::Queued(alice));
    }
    assert!(h.contains(alice));

    // The tic the drone's fan-out merges, then one timer pass: alice's
    // pump send overflows her, her recursive Leave ends the game and
    // resets the settings mid-batch, and the drone's GAMEDATA is
    // encoded afterwards in the same reduction.
    h.packet(T0, alice, plain(), upload(0, 0, vec![(11, diff_turn())]));
    let effect = h.tick(Milliseconds(T0.0 + 30));
    assert!(effect.game_ended, "the last player left");
    assert!(!h.contains(alice));
    assert!(!h.lowres_turn(), "the role reset to wide mid-reduce");

    // The drone's GAMEDATA was produced while the room was lowres and
    // is encoded after the flip: the production tag keeps it lowres.
    let mut gamedata = None;
    while let Some(packet) = h.pop_outbound(drone) {
        if matches!(
            ServerPacket::decode(packet.payload(), true),
            Ok((_, ServerPacket::GameData(_)))
        ) {
            gamedata = Some(packet);
        }
    }
    let gamedata = gamedata.expect("the drone had a fan-out queued");
    assert_eq!(
        gamedata.lowres(),
        Some(true),
        "the tag is the production width"
    );
    let (_, ServerPacket::GameData(data)) =
        ServerPacket::decode(gamedata.payload(), true).expect("decodes lowres")
    else {
        unreachable!()
    };
    assert!(
        data.tics
            .iter()
            .any(|tic| tic.players.iter().any(|(_, d)| d.turn == Some(0x100))),
        "the narrow turn survived"
    );
    assert!(
        ServerPacket::decode(gamedata.payload(), false).is_err(),
        "the bytes are not wide-encoded"
    );
}
