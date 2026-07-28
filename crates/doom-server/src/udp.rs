//! UDP binding: one Chocolate packet per datagram, byte-exact with no
//! envelope. Address identity is the `(listener, SocketAddr)` pair;
//! the mapping is binding-owned and removed in the same reduction as
//! the registry removal. UDP has no hangup signal, so silence is
//! handled only by the role's own timer through the shared reducer.

use std::net::SocketAddr;

use tokio::net::UdpSocket;

use doom_proto::{ClientPacket, GameDataServer, ServerPacket, WireHeader};

use crate::runtime::{
    BindRoom, Effects, ListenerId, Runtime, SharedState, UdpPeer, classify_malformed,
};
use crate::server_role::MalformedClass;
use crate::{PlayerId, RoomName};

/// The pinned native protocol ceiling: SDL_net clients receive into
/// 1500 bytes, so no datagram larger than 1500 is ever accepted or
/// emitted.
pub(crate) const MAX_DATAGRAM_LEN: usize = 1500;

/// The bounded receive buffer, larger than the protocol ceiling so an
/// oversized original datagram is always detected and rejected whole
/// rather than truncated into a valid-looking packet.
const RECV_BUF_LEN: usize = 2048;

/// The accepted outbound-size adaptation (the proto-35 addendum 4/5
/// and engine-31 FU17 oracle): an oversize `GAMEDATA` emission is
/// re-encoded as the largest complete NEWEST suffix of its tic span
/// that fits the ceiling, with the advertised start advanced by the
/// omitted count (u8 wrap-safe by the expansion straddle rule) and the
/// original newest tic last, the only tic whose latency the client
/// uses for clock sync. The pinned client then requests exactly the
/// omitted prefix through its ordinary `GAMEDATA_RESEND` path, whose
/// bounded answers repeat the same rule, so reconstruction is exact
/// and terminates. Anything else over the ceiling — a non-`GAMEDATA`
/// packet or a one-tic oversize, neither reachable from accepted input
/// — cannot be adapted and takes the isolated-recipient producer-error
/// path. The boundary is inclusive: exactly 1500 bytes is emitted
/// unadapted.
fn adapt_gamedata(bytes: &[u8], lowres: bool) -> Option<Vec<u8>> {
    let (header, packet) = ServerPacket::decode(bytes, lowres).ok()?;
    let ServerPacket::GameData(data) = packet else {
        return None;
    };
    let total = data.tics.len();
    // The full span is already known to exceed the ceiling, so only
    // proper suffixes are candidates; the first fit is the largest.
    for count in (1..total).rev() {
        let omitted = total - count;
        let candidate = ServerPacket::GameData(GameDataServer {
            start: data.start.wrapping_add(omitted as u8),
            tics: data.tics[omitted..].to_vec(),
        });
        let encoded = candidate
            .encode(header, lowres)
            .expect("a gamedata suffix always encodes");
        if encoded.len() <= MAX_DATAGRAM_LEN {
            return Some(encoded);
        }
    }
    None
}

/// One listener task per configured `--udp ROOM=ADDR` bind: receives
/// datagrams for its pinned room, drives the shared runtime, and sends
/// every queued datagram FIFO as individual packets. It is the only
/// writer for its socket, and its lifetime is the serve future's: a
/// socket failure resolves it, which ends the shared service.
pub(crate) async fn listener(
    socket: UdpSocket,
    room_name: RoomName,
    state: SharedState,
) -> std::io::Result<()> {
    let (listener_id, notifier) = {
        let mut runtime = state.0.lock().await;
        runtime.register_listener()
    };
    let mut buf = vec![0u8; RECV_BUF_LEN];
    loop {
        tokio::select! {
            received = socket.recv_from(&mut buf) => {
                let (len, from) = received?;
                let effects = {
                    let mut runtime = state.0.lock().await;
                    runtime.udp_datagram(listener_id, &room_name, from, &buf[..len])
                };
                for waiter in effects.ws_waiters {
                    waiter.notify_one();
                }
                for waiter in effects.udp_waiters {
                    waiter.notify_one();
                }
                for (address, bytes) in effects.direct_udp {
                    let _ = socket.send_to(&bytes, address).await;
                }
            }
            _ = notifier.notified() => {}
        }
        let (datagrams, effects, taken) = {
            let mut runtime = state.0.lock().await;
            runtime.take_udp(listener_id, &room_name)
        };
        for waiter in effects.ws_waiters {
            waiter.notify_one();
        }
        for waiter in effects.udp_waiters {
            waiter.notify_one();
        }
        for (address, bytes) in datagrams {
            // Send failures are isolated and bounded: the datagram is
            // dropped, role state is untouched, no queue is retained.
            let _ = socket.send_to(&bytes, address).await;
        }
        // Phase two after the actual send attempts: clear this batch's
        // tombstones and drop the room if nothing retains it.
        {
            let mut runtime = state.0.lock().await;
            runtime.finish_udp(listener_id, &room_name, taken);
        }
    }
}

impl Runtime {
    /// One inbound datagram for a pinned room on one listener. A
    /// mapped `(listener, address)` feeds the same server-payload path
    /// as any transport. Unknown source addresses are admitted only by
    /// a valid SYN (or the old-magic refusal path, which removes them
    /// in the same batch); QUERY is answered statelessly; everything
    /// else is the pinned silent behavior with no admission.
    pub(crate) fn udp_datagram(
        &mut self,
        listener: ListenerId,
        room_name: &RoomName,
        from: SocketAddr,
        payload: &[u8],
    ) -> Effects {
        let now = self.now();
        let Self {
            rooms,
            udp_notifiers,
            outbox_capacity,
            ..
        } = self;
        // Oversized original datagrams are rejected before decode: a
        // truncated packet must never masquerade as a valid one.
        if payload.len() > MAX_DATAGRAM_LEN {
            if let Some(room) = rooms.get_mut(room_name)
                && let Some(player) = room.udp_addresses.get(&(listener, from)).copied()
            {
                let effect = room
                    .host
                    .malformed(now, player, MalformedClass::Established);
                return room.apply(effect, udp_notifiers);
            }
            return Effects::default();
        }

        if let Some(room) = rooms.get_mut(room_name)
            && let Some(player) = room.udp_addresses.get(&(listener, from)).copied()
        {
            let effect = room.server_payload(now, player, payload);
            return room.apply(effect, udp_notifiers);
        }

        // Unknown address: decode with the room's authoritative width
        // (wide before GAMESTART; only SYN and QUERY matter here, and
        // both are width-independent). Admission is refused while this
        // (listener, address) has an unattempted removal batch or the
        // room is held only by undrained removal traffic; the client
        // retransmits and is admitted after the drain attempt.
        let lowres = rooms
            .get(room_name)
            .map(|room| room.host.lowres_turn())
            .unwrap_or(false);
        let admission_blocked = rooms.get(room_name).is_some_and(|room| {
            room.udp_draining.contains(&(listener, from)) || room.is_draining()
        });
        match ClientPacket::decode(payload, lowres) {
            Ok((header, packet @ ClientPacket::Syn(_))) => {
                if admission_blocked {
                    return Effects::default();
                }
                // A valid SYN is admitted registry-first and mapped
                // atomically, then fed through the normal packet path.
                let room = rooms
                    .entry(room_name.clone())
                    .or_insert_with(|| BindRoom::new(room_name, *outbox_capacity));
                let Ok((player, effect)) = room.host.join(now, |_| from.to_string().into_bytes())
                else {
                    // A full room refuses silently at the binding.
                    return Effects::default();
                };
                let mut effects = room.apply(effect, udp_notifiers);
                room.udp_players.insert(
                    player,
                    UdpPeer {
                        listener,
                        address: from,
                    },
                );
                room.udp_addresses.insert((listener, from), player);
                let effect = room.host.packet(now, player, header, packet);
                effects.merge(room.apply(effect, udp_notifiers));
                effects
            }
            Ok((_, ClientPacket::Query)) => {
                // Stateless: no PlayerId, mapping, member, or
                // room-retaining connection is allocated. An absent
                // room is described by a transient fresh role.
                let response = rooms
                    .get(room_name)
                    .map(|room| room.host.query_response())
                    .unwrap_or_else(|| crate::server_role::ServerRole::new().query_response());
                let bytes = response
                    .encode(WireHeader { reliable_seq: None }, lowres)
                    .expect("the query response always encodes");
                let mut effects = Effects::default();
                effects.direct_udp.push((from, bytes));
                effects
            }
            Ok(_) => {
                // The known-client rule: unknown non-SYN traffic is
                // dropped silently, never admitted implicitly.
                Effects::default()
            }
            Err(_) => match classify_malformed(payload) {
                MalformedClass::Syn { old_magic: true } => {
                    if admission_blocked {
                        return Effects::default();
                    }
                    // Admission, classification, terminal REJECTED, and
                    // removal with unmapping in one reduction.
                    let room = rooms
                        .entry(room_name.clone())
                        .or_insert_with(|| BindRoom::new(room_name, *outbox_capacity));
                    let Ok((player, effect)) =
                        room.host.join(now, |_| from.to_string().into_bytes())
                    else {
                        return Effects::default();
                    };
                    let mut effects = room.apply(effect, udp_notifiers);
                    room.udp_players.insert(
                        player,
                        UdpPeer {
                            listener,
                            address: from,
                        },
                    );
                    room.udp_addresses.insert((listener, from), player);
                    let effect =
                        room.host
                            .malformed(now, player, MalformedClass::Syn { old_magic: true });
                    effects.merge(room.apply(effect, udp_notifiers));
                    effects
                }
                // Wrong magic, non-SYN, truncated, wrong-direction:
                // the pinned silent behavior, no admission.
                _ => Effects::default(),
            },
        }
    }

    /// Take every owed datagram for one listener of a room — phase one
    /// of the send contract. The batch is the listener's pending
    /// removal traffic first (terminal last per removed peer), then
    /// each of its mapped peers' outboxes FIFO. The take only MOVES
    /// the batch: this listener's tombstones stay and the room stays
    /// draining while the batch is in flight, so no new session can
    /// start before the prior terminal actually leaves the socket.
    /// The listener sends outside the lock, then calls `finish_udp`
    /// (phase two) to clear exactly this batch's tombstones and drop
    /// the room if it is now empty. An outbound datagram over the
    /// 1500-byte ceiling is first offered to the accepted `GAMEDATA`
    /// newest-suffix adaptation; only a packet that cannot be adapted
    /// is a distinct producer error, never truncated, split, silently
    /// dropped, or sent through the slow-consumer path, isolating
    /// exactly its intended recipient through the normal removal path
    /// while every other peer continues. The effects of that isolation
    /// (survivor wakeups, the isolated peer's own terminal capture)
    /// ride the returned `Effects` through the same post-lock delivery
    /// path as every other reduction.
    pub(crate) fn take_udp(
        &mut self,
        listener: ListenerId,
        room_name: &RoomName,
    ) -> (Vec<(SocketAddr, Vec<u8>)>, Effects, TakenUdpBatch) {
        let now = self.now();
        let Self {
            rooms,
            udp_notifiers,
            ..
        } = self;
        let mut out = Vec::new();
        let mut effects = Effects::default();
        let mut taken = TakenUdpBatch::default();
        let mut oversized: Vec<(PlayerId, usize)> = Vec::new();
        {
            let Some(room) = rooms.get_mut(room_name) else {
                return (out, effects, taken);
            };
            debug_assert!(
                !room.udp_inflight.contains(&listener),
                "one in-flight batch per listener"
            );
            let lowres = room.host.lowres_turn();
            // The tombstones this batch answers for, snapshotted before
            // any new removal can add its own: finishing clears exactly
            // these, never state created after the take.
            taken.tombstones = room
                .udp_draining
                .iter()
                .filter(|(candidate, _)| *candidate == listener)
                .copied()
                .collect();
            if let Some(pending) = room.pending_udp.get_mut(&listener) {
                while let Some((address, bytes)) = pending.pop_front() {
                    if bytes.len() > MAX_DATAGRAM_LEN {
                        match adapt_gamedata(&bytes, lowres) {
                            Some(adapted) => out.push((address, adapted)),
                            None => eprintln!(
                                "doomd udp: producer error: {} bytes exceeds the 1500-byte datagram ceiling for {address}; discarded with its removed peer",
                                bytes.len()
                            ),
                        }
                        continue;
                    }
                    out.push((address, bytes));
                }
            }
            let players: Vec<(PlayerId, SocketAddr)> = room
                .udp_players
                .iter()
                .filter(|(_, peer)| peer.listener == listener)
                .map(|(player, peer)| (*player, peer.address))
                .collect();
            for (player, address) in players {
                while let Some(packet) = room.host.pop_outbound(player) {
                    let bytes = packet.payload();
                    if bytes.len() > MAX_DATAGRAM_LEN {
                        // Input-reachable oversize (a three-player
                        // extratics=127 GAMEDATA span) is adapted, never
                        // isolated; only the impossible non-GAMEDATA or
                        // one-tic case takes the producer-error path.
                        if let Some(adapted) = adapt_gamedata(bytes, lowres) {
                            out.push((address, adapted));
                            continue;
                        }
                        eprintln!(
                            "doomd udp: producer error: {} bytes exceeds the 1500-byte datagram ceiling for {address}; isolating the recipient",
                            bytes.len()
                        );
                        oversized.push((player, bytes.len()));
                        // The remainder of the isolated peer's queue is
                        // discarded, never sent.
                        while room.host.pop_outbound(player).is_some() {}
                        break;
                    }
                    out.push((address, bytes.to_vec()));
                }
            }
            if !out.is_empty() {
                room.udp_inflight.insert(listener);
            }
        }
        for (player, _) in oversized {
            if let Some(room) = rooms.get_mut(room_name) {
                let effect = room.host.leave(now, player);
                effects.merge(room.apply(effect, udp_notifiers));
            }
        }
        (out, effects, taken)
    }

    /// Phase two of the send contract, called after the listener
    /// attempted every datagram of the taken batch outside the lock:
    /// clear exactly that batch's tombstones (never pending state or
    /// tombstones created after the take) and drop the room if nothing
    /// retains it anymore.
    pub(crate) fn finish_udp(
        &mut self,
        listener: ListenerId,
        room_name: &RoomName,
        taken: TakenUdpBatch,
    ) {
        let Some(room) = self.rooms.get_mut(room_name) else {
            return;
        };
        room.udp_inflight.remove(&listener);
        for identity in taken.tombstones {
            room.udp_draining.remove(&identity);
        }
        if room.is_empty() {
            self.rooms.remove(room_name);
        }
    }
}

/// The tombstone snapshot of one taken listener batch: finishing that
/// batch clears exactly these identities, never state created after
/// the take. Binding-owned; the listener carries it across the send
/// attempts.
#[derive(Default)]
pub(crate) struct TakenUdpBatch {
    tombstones: Vec<(ListenerId, SocketAddr)>,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::runtime::{JoinRefusal, ListenerFuture, udp_supervisor};
    use doom_proto::{ConnectData, FullTic, GameSettings, Syn, TiccmdDiff};
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio::sync::Notify;
    use tokio::time::{Instant, timeout};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    // --- helpers ----------------------------------------------------------

    fn room() -> RoomName {
        RoomName::try_from("e1m1").expect("valid room")
    }

    fn runtime() -> Runtime {
        Runtime {
            rooms: std::collections::HashMap::new(),
            outbox_capacity: NonZeroUsize::new(16).expect("nonzero"),
            started: Instant::now(),
            next_listener: 0,
            udp_notifiers: std::collections::HashMap::new(),
        }
    }

    fn syn(name: &str, lowres: u8) -> ClientPacket {
        ClientPacket::Syn(Syn {
            version: b"Chocolate Doom 3.1.1".to_vec(),
            protocols: vec![b"CHOCOLATE_DOOM_0".to_vec()],
            connect: ConnectData {
                gamemode: 0,
                gamemission: 0,
                lowres_turn: lowres,
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

    fn syn_bytes(name: &str, mission: u8, mode: u8, lowres: u8) -> Vec<u8> {
        let ClientPacket::Syn(mut value) = syn(name, lowres) else {
            unreachable!()
        };
        value.connect.gamemission = mission;
        value.connect.gamemode = mode;
        ClientPacket::Syn(value)
            .encode(WireHeader { reliable_seq: None }, false)
            .expect("syn encodes")
    }

    fn settings_bytes(seq: u8, deathmatch: u8) -> Vec<u8> {
        ClientPacket::GameStart(GameSettings {
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
        })
        .encode(
            WireHeader {
                reliable_seq: Some(seq),
            },
            false,
        )
        .expect("gamestart encodes")
    }

    fn launch_bytes(seq: u8) -> Vec<u8> {
        ClientPacket::Launch
            .encode(
                WireHeader {
                    reliable_seq: Some(seq),
                },
                false,
            )
            .expect("launch encodes")
    }

    fn ack_bytes(next_seq: u8) -> Vec<u8> {
        ClientPacket::ReliableAck { next_seq }
            .encode(WireHeader { reliable_seq: None }, false)
            .expect("ack encodes")
    }

    fn fixture_syn() -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/handshake-keepalive/000-c2s-client1-syn.bin");
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    fn addr() -> SocketAddr {
        "127.0.0.1:23420".parse().expect("valid addr")
    }

    /// Take and immediately finish one listener batch, mirroring the
    /// listener loop when the sends happen between the two phases.
    fn drain(
        rt: &mut Runtime,
        listener: ListenerId,
        room: &RoomName,
    ) -> (Vec<(SocketAddr, Vec<u8>)>, Effects) {
        let (datagrams, effects, taken) = rt.take_udp(listener, room);
        rt.finish_udp(listener, room, taken);
        (datagrams, effects)
    }

    /// A fully populated vanilla diff: 8 bytes wide, 7 lowres, so the
    /// three-player construction lands exactly on the oracle figures.
    fn full_diff() -> TiccmdDiff {
        TiccmdDiff {
            forward: Some(1),
            side: Some(1),
            turn: Some(0x100),
            buttons: Some(1),
            consistancy: Some(1),
            chatchar: Some(1),
            ..Default::default()
        }
    }

    /// A server GAMEDATA packet of `tics` full tics carrying two full
    /// diffs each (the three-player recipient view): 19 bytes per tic
    /// wide, 17 lowres, plus the 4-byte header.
    fn gamedata(start: u8, tics: usize, lowres: bool) -> (Vec<u8>, GameDataServer) {
        let data = GameDataServer {
            start,
            tics: (0..tics)
                .map(|tic| FullTic {
                    latency: tic as i16,
                    players: vec![(0, full_diff()), (1, full_diff())],
                })
                .collect(),
        };
        let bytes = ServerPacket::GameData(data.clone())
            .encode(WireHeader { reliable_seq: None }, lowres)
            .expect("gamedata encodes");
        (bytes, data)
    }

    struct MixedServer {
        ws_addr: SocketAddr,
        server: tokio::task::JoinHandle<std::io::Result<()>>,
        udp: UdpSocket,
    }

    async fn spawn_mixed(room: &str) -> MixedServer {
        let room_name = RoomName::try_from(room).expect("valid room");
        let ws_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ws listener binds");
        let ws_addr = ws_listener.local_addr().expect("ws address");
        let udp_socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("udp listener binds");
        let udp_addr = udp_socket.local_addr().expect("udp address");
        let server = tokio::spawn(crate::runtime::serve(
            ws_listener,
            vec![(room_name, udp_socket)],
            NonZeroUsize::new(64).expect("nonzero"),
        ));
        let udp = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("client socket binds");
        udp.connect(udp_addr).await.expect("client connects");
        MixedServer {
            ws_addr,
            server,
            udp,
        }
    }

    /// A WebSocket peer whose incoming frames are drained continuously
    /// by a reader task, so a test never stalls the server writer with
    /// an unread socket. Frames arrive raw (route, payload).
    struct WsPeer {
        out: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        incoming: tokio::sync::mpsc::UnboundedReceiver<(u32, Vec<u8>)>,
    }

    async fn ws_connect(url: &str) -> WsPeer {
        let (socket, _) = connect_async(url).await.expect("ws connects");
        let (mut sink, mut stream) = socket.split();
        let (out, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let (in_tx, incoming) = tokio::sync::mpsc::unbounded_channel::<(u32, Vec<u8>)>();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    frame = out_rx.recv() => {
                        let Some(frame) = frame else { break; };
                        if sink.send(ClientMessage::Binary(frame.into())).await.is_err() {
                            break;
                        }
                    }
                    message = stream.next() => {
                        match message {
                            Some(Ok(ClientMessage::Binary(frame))) => {
                                let from = u32::from_le_bytes(frame[..4].try_into().expect("route"));
                                if in_tx.send((from, frame[4..].to_vec())).is_err() {
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }
                }
            }
        });
        WsPeer { out, incoming }
    }

    impl WsPeer {
        fn send(&self, to: u32, from: u32, payload: &[u8]) {
            self.out
                .send(ws_frame(to, from, payload))
                .expect("ws frame queues");
        }

        async fn try_next(&mut self, millis: u64) -> Option<(u32, Vec<u8>)> {
            timeout(Duration::from_millis(millis), self.incoming.recv())
                .await
                .ok()
                .flatten()
        }
    }

    async fn udp_recv(socket: &UdpSocket, millis: u64) -> Vec<u8> {
        udp_try_recv(socket, millis)
            .await
            .expect("datagram arrives")
    }

    async fn udp_try_recv(socket: &UdpSocket, millis: u64) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 2048];
        match timeout(Duration::from_millis(millis), socket.recv(&mut buf)).await {
            Ok(Ok(len)) => {
                buf.truncate(len);
                Some(buf)
            }
            _ => None,
        }
    }

    async fn udp_expect_silence(socket: &UdpSocket, millis: u64) {
        let mut buf = [0u8; 64];
        assert!(
            timeout(Duration::from_millis(millis), socket.recv(&mut buf))
                .await
                .is_err(),
            "expected silence, got a datagram"
        );
    }

    fn decode_server(bytes: &[u8], lowres: bool) -> ServerPacket {
        ServerPacket::decode(bytes, lowres)
            .expect("server packet decodes")
            .1
    }

    fn ws_frame(to: u32, from: u32, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(8 + payload.len());
        frame.extend_from_slice(&to.to_le_bytes());
        frame.extend_from_slice(&from.to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    // --- runtime-level tests ----------------------------------------------

    #[test]
    fn udp_unknown_traffic_is_silent_and_never_admitted() {
        let mut rt = runtime();
        let (listener, _notifier) = rt.register_listener();
        let from = addr();
        for payload in [
            // decodable non-SYN (keepalive), truncated bytes, an
            // arbitrary-magic SYN shape, wrong-direction bytes, and an
            // oversized original datagram
            ClientPacket::Keepalive
                .encode(WireHeader { reliable_seq: None }, false)
                .expect("encodes"),
            vec![0x06],
            [vec![0x00, 0x00], b"JUNK".to_vec()].concat(),
            ServerPacket::Keepalive
                .encode(WireHeader { reliable_seq: None }, false)
                .expect("encodes"),
            vec![0xaa; MAX_DATAGRAM_LEN + 1],
        ] {
            let effects = rt.udp_datagram(listener, &room(), from, &payload);
            assert!(effects.ws_waiters.is_empty());
            assert!(effects.udp_waiters.is_empty());
            assert!(effects.direct_udp.is_empty());
        }
        let bind_room = rt.rooms.get(&room());
        assert!(
            bind_room.is_none() || bind_room.expect("room").udp_addresses.is_empty(),
            "no mapping was created"
        );
        // A later valid SYN from the same address is admitted normally.
        let effects = rt.udp_datagram(listener, &room(), from, &fixture_syn());
        assert!(
            !effects.udp_waiters.is_empty(),
            "the admitted peer's listener is woken"
        );
        let bind_room = rt.rooms.get(&room()).expect("room exists");
        assert_eq!(bind_room.udp_addresses.len(), 1);
    }

    #[test]
    fn udp_disconnect_acks_then_unmaps_and_readmits_fresh() {
        let mut rt = runtime();
        let (listener, _notifier) = rt.register_listener();
        let from = addr();
        rt.udp_datagram(listener, &room(), from, &fixture_syn());
        let first = rt.rooms[&room()].udp_addresses[&(listener, from)];
        // Drain the accept/waiting so the acknowledgement below is
        // unambiguous.
        drain(&mut rt, listener, &room());

        // The remote disconnect is acknowledged through the ordinary
        // outbox path.
        rt.udp_datagram(
            listener,
            &room(),
            from,
            &ClientPacket::Disconnect
                .encode(WireHeader { reliable_seq: None }, false)
                .expect("encodes"),
        );
        let (datagrams, _) = drain(&mut rt, listener, &room());
        assert!(
            datagrams.iter().any(|(a, bytes)| {
                *a == from && matches!(decode_server(bytes, false), ServerPacket::DisconnectAck)
            }),
            "the disconnect is acknowledged"
        );

        // After the sleep the mapping is removed exactly once; whatever
        // removal traffic was captured is attempted before re-admission.
        rt.started = rt
            .started
            .checked_sub(Duration::from_secs(6))
            .expect("backdate the clock past the sleep");
        rt.tick();
        assert!(
            rt.rooms
                .get(&room())
                .is_none_or(|bind_room| bind_room.udp_addresses.is_empty())
        );
        drain(&mut rt, listener, &room());
        if let Some(bind_room) = rt.rooms.get(&room()) {
            assert!(!bind_room.host.contains(first));
        }

        // A fresh SYN from the same address is a brand-new admission
        // with no stale state: exactly one member, a fresh accept owed.
        let effects = rt.udp_datagram(listener, &room(), from, &fixture_syn());
        assert!(!effects.udp_waiters.is_empty());
        let bind_room = rt.rooms.get(&room()).expect("room readmits");
        let second = bind_room.udp_addresses[&(listener, from)];
        assert!(bind_room.host.contains(second));
        let ServerPacket::QueryResponse(query) = bind_room.host.query_response() else {
            panic!("query response")
        };
        assert_eq!(query.num_players, 1);
    }

    // --- H1: listener-scoped identity --------------------------------------

    #[test]
    fn listener_scoped_identity_shares_one_room_without_aliasing() {
        let mut rt = runtime();
        let (l1, _n1) = rt.register_listener();
        let (l2, _n2) = rt.register_listener();
        let from = addr();

        // (a) The same remote address SYNs via both listeners of one
        // room: two independent admissions with distinct PlayerIds.
        rt.udp_datagram(l1, &room(), from, &fixture_syn());
        rt.udp_datagram(l2, &room(), from, &fixture_syn());
        let bind_room = rt.rooms.get(&room()).expect("room");
        let p1 = bind_room.udp_addresses[&(l1, from)];
        let p2 = bind_room.udp_addresses[&(l2, from)];
        assert_ne!(p1, p2, "the two listeners never alias one PlayerId");
        assert!(bind_room.host.contains(p1));
        assert!(bind_room.host.contains(p2));
        // (e) One shared role: the authoritative query response reports
        // both peers of the single room.
        let ServerPacket::QueryResponse(query) = bind_room.host.query_response() else {
            panic!("query response")
        };
        assert_eq!(query.num_players, 2);

        // (b) Bytes via l2 from an address admitted only on l1 are
        // unknown-address traffic there, never attributed to the l1
        // peer and never admitted implicitly.
        let other: SocketAddr = "127.0.0.1:29999".parse().expect("valid addr");
        let keepalive = ClientPacket::Keepalive
            .encode(WireHeader { reliable_seq: None }, false)
            .expect("encodes");
        let effects = rt.udp_datagram(l2, &room(), other, &keepalive);
        assert!(effects.ws_waiters.is_empty());
        assert!(effects.udp_waiters.is_empty());
        assert!(effects.direct_udp.is_empty());
        assert!(!rt.rooms[&room()].udp_addresses.contains_key(&(l2, other)));

        // (d) Removal of (l1, from) leaves (l2, from) fully intact, and
        // draining the removed peer's batch disturbs nothing else.
        rt.leave(&room(), p1);
        drain(&mut rt, l1, &room());
        let bind_room = rt.rooms.get(&room()).expect("room stays alive");
        assert!(!bind_room.udp_addresses.contains_key(&(l1, from)));
        assert!(!bind_room.host.contains(p1));
        assert!(bind_room.udp_addresses.contains_key(&(l2, from)));
        assert!(bind_room.host.contains(p2));
        let ServerPacket::QueryResponse(query) = bind_room.host.query_response() else {
            panic!("query response")
        };
        assert_eq!(query.num_players, 1);
    }

    // --- H2: serve composition ---------------------------------------------

    #[tokio::test]
    async fn udp_supervisor_pends_with_no_listeners() {
        assert!(
            timeout(Duration::from_millis(200), udp_supervisor(Vec::new()))
                .await
                .is_err(),
            "an empty listener list never resolves"
        );
    }

    #[tokio::test]
    async fn udp_supervisor_first_completion_ends_the_service() {
        let done: ListenerFuture = Box::pin(async { Ok(()) });
        let never: ListenerFuture = Box::pin(async {
            futures_util::future::pending::<()>().await;
            Ok(())
        });
        let result = timeout(Duration::from_secs(1), udp_supervisor(vec![done, never])).await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "the first listener completion resolves the supervisor"
        );
    }

    #[tokio::test]
    async fn udp_supervisor_first_error_propagates() {
        let failed: ListenerFuture =
            Box::pin(async { Err(std::io::Error::other("listener socket died")) });
        let never: ListenerFuture = Box::pin(async {
            futures_util::future::pending::<()>().await;
            Ok(())
        });
        let result = timeout(Duration::from_secs(1), udp_supervisor(vec![never, failed])).await;
        match result {
            Ok(Err(error)) => assert_eq!(error.to_string(), "listener socket died"),
            other => panic!("expected the listener error to propagate, got {other:?}"),
        }
    }

    // --- H3: outbound-size adaptation --------------------------------------

    #[test]
    fn adapt_gamedata_wide_suffix_exact_boundary() {
        // The oracle construction: 128 tics with two full diffs per tic
        // is 4 + 128 x 19 = 2436 bytes wide.
        let (bytes, original) = gamedata(200, 128, false);
        assert_eq!(bytes.len(), 2436, "the three-player 128-tic span");
        let adapted = adapt_gamedata(&bytes, false).expect("gamedata adapts");
        assert_eq!(adapted.len(), 1486, "78 wide tics is the largest fit");
        let (_, ServerPacket::GameData(adapted)) =
            ServerPacket::decode(&adapted, false).expect("the suffix decodes")
        else {
            unreachable!()
        };
        assert_eq!(adapted.tics.len(), 78);
        assert_eq!(adapted.start, 200u8.wrapping_add(50));
        assert_eq!(adapted.tics.as_slice(), &original.tics[50..]);
        assert_eq!(adapted.tics.last(), original.tics.last());
        // The 78-tic packet itself fits exactly and is never adapted.
        let (fits, _) = gamedata(200, 78, false);
        assert_eq!(fits.len(), 1486);
    }

    #[test]
    fn adapt_gamedata_lowres_suffix_exact_boundary() {
        // Lowres: 4 + 128 x 17 = 2180; the 88-tic suffix lands exactly
        // on 1500, which the inclusive bound must accept.
        let (bytes, original) = gamedata(200, 128, true);
        assert_eq!(bytes.len(), 2180, "the three-player lowres span");
        let adapted = adapt_gamedata(&bytes, true).expect("gamedata adapts");
        assert_eq!(adapted.len(), 1500, "88 lowres tics is exactly 1500");
        let (_, ServerPacket::GameData(adapted)) =
            ServerPacket::decode(&adapted, true).expect("the suffix decodes")
        else {
            unreachable!()
        };
        assert_eq!(adapted.tics.len(), 88);
        assert_eq!(adapted.start, 200u8.wrapping_add(40));
        assert_eq!(adapted.tics.as_slice(), &original.tics[40..]);
        assert_eq!(adapted.tics.last(), original.tics.last());
        // The 88-tic packet is exactly 1500; the 89-tic adapts to 88.
        let (fits, _) = gamedata(200, 88, true);
        assert_eq!(fits.len(), 1500);
        let (over, _) = gamedata(200, 89, true);
        assert_eq!(over.len(), 1517);
        let adapted = adapt_gamedata(&over, true).expect("gamedata adapts");
        assert_eq!(adapted.len(), 1500);
        // Wrap safety: 250 + 40 crosses the u8 boundary to 34 and the
        // absolute sequence still moves forward (engine-31 FU17 §4b).
        let (wrapped, _) = gamedata(250, 128, true);
        let adapted = adapt_gamedata(&wrapped, true).expect("gamedata adapts");
        let (_, ServerPacket::GameData(adapted)) =
            ServerPacket::decode(&adapted, true).expect("the suffix decodes")
        else {
            unreachable!()
        };
        assert_eq!(adapted.start, 34);
    }

    #[test]
    fn adapt_gamedata_rejects_non_gamedata() {
        assert!(adapt_gamedata(&vec![0xaa; MAX_DATAGRAM_LEN + 1], false).is_none());
        let keepalive = ServerPacket::Keepalive
            .encode(WireHeader { reliable_seq: None }, false)
            .expect("encodes");
        assert!(adapt_gamedata(&keepalive, false).is_none());
    }

    #[test]
    fn drain_udp_passes_boundary_packets_unmodified() {
        let mut rt = runtime();
        let (listener, _notifier) = rt.register_listener();
        let from = addr();
        rt.udp_datagram(listener, &room(), from, &fixture_syn());
        let player = rt.rooms[&room()].udp_addresses[&(listener, from)];
        let now = rt.now();
        let bind_room = rt.rooms.get_mut(&room()).expect("room");
        let (other, _) = bind_room
            .host
            .join(now, |_| b"ws:relay".to_vec())
            .expect("member joins");
        let route = crate::runtime::RouteId::new(11).expect("nonzero");
        // The exact boundary packets: 78 wide tics (1486) and 88 lowres
        // tics (exactly 1500) both pass the drain byte-identical.
        let (wide, _) = gamedata(7, 78, false);
        let (lowres, _) = gamedata(7, 88, true);
        assert_eq!(wide.len(), 1486);
        assert_eq!(lowres.len(), 1500);
        bind_room
            .host
            .relay(now, other, player, route, &wide)
            .expect("fits the room-core bound");
        bind_room
            .host
            .relay(now, other, player, route, &lowres)
            .expect("fits the room-core bound");
        let (datagrams, _) = drain(&mut rt, listener, &room());
        let relayed: Vec<&Vec<u8>> = datagrams
            .iter()
            .map(|(_, bytes)| bytes)
            .filter(|bytes| {
                matches!(
                    ServerPacket::decode(bytes, false),
                    Ok((_, ServerPacket::GameData(_)))
                ) || matches!(
                    ServerPacket::decode(bytes, true),
                    Ok((_, ServerPacket::GameData(_)))
                )
            })
            .collect();
        assert_eq!(relayed.len(), 2);
        assert!(relayed.contains(&&wide));
        assert!(relayed.contains(&&lowres));
    }

    #[test]
    fn drain_adapts_oversize_gamedata_and_retains_the_peer() {
        let mut rt = runtime();
        let (listener, _notifier) = rt.register_listener();
        let from = addr();
        rt.udp_datagram(listener, &room(), from, &fixture_syn());
        let player = rt.rooms[&room()].udp_addresses[&(listener, from)];

        // The input-reachable worst case: a 128-tic wide span at 2436
        // bytes, injected into the UDP peer's outbox.
        let (bytes, original) = gamedata(250, 128, false);
        assert_eq!(bytes.len(), 2436);
        let now = rt.now();
        let bind_room = rt.rooms.get_mut(&room()).expect("room");
        let (other, _) = bind_room
            .host
            .join(now, |_| b"ws:relay".to_vec())
            .expect("member joins");
        let route = crate::runtime::RouteId::new(11).expect("nonzero");
        bind_room
            .host
            .relay(now, other, player, route, &bytes)
            .expect("fits the room-core bound");

        let (datagrams, effects) = drain(&mut rt, listener, &room());
        let adapted_bytes = datagrams
            .iter()
            .map(|(_, bytes)| bytes)
            .find(|bytes| {
                matches!(
                    ServerPacket::decode(bytes, false),
                    Ok((_, ServerPacket::GameData(_)))
                )
            })
            .expect("the adapted gamedata is emitted");
        assert!(adapted_bytes.len() <= MAX_DATAGRAM_LEN);
        assert_eq!(adapted_bytes.len(), 1486);
        let (_, ServerPacket::GameData(adapted)) =
            ServerPacket::decode(adapted_bytes, false).expect("the suffix decodes")
        else {
            unreachable!()
        };
        // Start advanced by the omitted 50 across the u8 wrap (250 ->
        // 44), the original newest tic last, content exact.
        assert_eq!(adapted.tics.len(), 78);
        assert_eq!(adapted.start, 44);
        assert_eq!(adapted.tics.as_slice(), &original.tics[50..]);
        assert_eq!(adapted.tics.last(), original.tics.last());
        // The peer is retained, never isolated: mapping and membership
        // survive and the drain produced no removal effects.
        assert!(
            rt.rooms[&room()]
                .udp_addresses
                .contains_key(&(listener, from))
        );
        assert!(rt.rooms[&room()].host.contains(player));
        assert!(effects.ws_waiters.is_empty());
        assert!(effects.udp_waiters.is_empty());
    }

    // --- H4: pending traffic in room lifetime ------------------------------

    /// The draining-room refusals that must hold from the removal
    /// until the listener FINISHES the batch's send attempts: no
    /// re-admission of the removed address, no new session in the
    /// room, WebSocket joins refused.
    fn assert_draining_refusals(rt: &mut Runtime, listener: ListenerId, from: SocketAddr) {
        let effects = rt.udp_datagram(listener, &room(), from, &fixture_syn());
        assert!(effects.ws_waiters.is_empty());
        assert!(effects.udp_waiters.is_empty());
        assert!(effects.direct_udp.is_empty());
        let other: SocketAddr = "127.0.0.1:29999".parse().expect("valid addr");
        let effects = rt.udp_datagram(listener, &room(), other, &fixture_syn());
        assert!(effects.udp_waiters.is_empty());
        assert!(matches!(
            rt.join(room(), Arc::new(Notify::new())),
            Err(JoinRefusal::RoomDraining)
        ));
        assert!(rt.rooms[&room()].udp_addresses.is_empty());
    }

    #[test]
    fn pending_removal_traffic_holds_room_until_listener_attempt() {
        let mut rt = runtime();
        let (listener, _notifier) = rt.register_listener();
        let from = addr();
        rt.udp_datagram(listener, &room(), from, &fixture_syn());

        // 30 s of silence: the role's own timeout removes the peer in a
        // tick reduction, which captures the owed accept/waiting into
        // the bounded pending batch. The mapping is gone but the room
        // is retained against further sweeps.
        rt.started = rt
            .started
            .checked_sub(Duration::from_secs(31))
            .expect("backdate the clock past the receive timeout");
        rt.tick();
        let bind_room = rt
            .rooms
            .get(&room())
            .expect("pending traffic retains the room");
        assert!(bind_room.udp_addresses.is_empty());
        assert!(!bind_room.pending_udp[&listener].is_empty());
        assert!(bind_room.udp_draining.contains(&(listener, from)));

        // Before the take, the draining-room refusals hold.
        assert_draining_refusals(&mut rt, listener, from);

        // The take moves the batch out but attempts nothing yet: the
        // tombstone and the room's draining state SURVIVE the take, so
        // a WebSocket join racing the send window is still refused (the
        // addendum-1 seam) and no sweep can drop the room.
        let (datagrams, effects, taken) = rt.take_udp(listener, &room());
        assert!(datagrams.len() >= 2, "accept and waiting are owed");
        assert!(matches!(
            decode_server(&datagrams[0].1, false),
            ServerPacket::SynAccept(_)
        ));
        assert!(effects.ws_waiters.is_empty());
        let bind_room = rt
            .rooms
            .get(&room())
            .expect("the in-flight batch retains the room");
        assert!(bind_room.udp_draining.contains(&(listener, from)));
        assert!(bind_room.udp_inflight.contains(&listener));
        assert_draining_refusals(&mut rt, listener, from);
        rt.tick();
        assert!(rt.rooms.contains_key(&room()));

        // The finish (after the actual send attempts) clears exactly
        // this batch's tombstone and drops the emptied room.
        rt.finish_udp(listener, &room(), taken);
        assert!(
            !rt.rooms.contains_key(&room()),
            "the finished empty room drops"
        );

        // The next valid SYN starts a fresh host.
        rt.udp_datagram(listener, &room(), from, &fixture_syn());
        let bind_room = rt.rooms.get(&room()).expect("fresh room");
        assert_eq!(bind_room.udp_addresses.len(), 1);
        let ServerPacket::QueryResponse(query) = bind_room.host.query_response() else {
            panic!("query response")
        };
        assert_eq!(query.num_players, 1);
    }

    #[test]
    fn finish_clears_only_the_taken_batch_generation() {
        let mut rt = runtime();
        let (listener, _notifier) = rt.register_listener();
        let first = addr();
        let second: SocketAddr = "127.0.0.1:29999".parse().expect("valid addr");
        rt.udp_datagram(listener, &room(), first, &fixture_syn());

        // The second peer joins the silence 20 s later.
        rt.started = rt
            .started
            .checked_sub(Duration::from_secs(20))
            .expect("backdate the clock");
        rt.udp_datagram(listener, &room(), second, &fixture_syn());
        let surviving = rt.rooms[&room()].udp_addresses[&(listener, second)];

        // 35 s after the first admission (15 after the second): only
        // the first peer times out.
        rt.started = rt
            .started
            .checked_sub(Duration::from_secs(15))
            .expect("backdate past the first peer's timeout");
        rt.tick();
        let bind_room = rt.rooms.get(&room()).expect("room");
        assert!(bind_room.udp_draining.contains(&(listener, first)));
        assert!(!bind_room.udp_draining.contains(&(listener, second)));

        // Take the first peer's batch; while it is in flight, queue a
        // relay into the survivor's outbox and let the survivor cross
        // its own timeout, producing a LATER pending generation on the
        // same listener.
        let (datagrams, _, taken) = rt.take_udp(listener, &room());
        assert!(!datagrams.is_empty());
        let now = rt.now();
        let bind_room = rt.rooms.get_mut(&room()).expect("room");
        let (sender, _) = bind_room
            .host
            .join(now, |_| b"ws:relay".to_vec())
            .expect("member joins");
        let route = crate::runtime::RouteId::new(11).expect("nonzero");
        bind_room
            .host
            .relay(now, sender, surviving, route, b"later")
            .expect("relays");
        rt.started = rt
            .started
            .checked_sub(Duration::from_secs(20))
            .expect("backdate past the second peer's timeout");
        rt.tick();
        let bind_room = rt.rooms.get(&room()).expect("room retained");
        assert!(!bind_room.host.contains(surviving));
        assert!(bind_room.udp_draining.contains(&(listener, second)));
        assert!(!bind_room.pending_udp[&listener].is_empty());

        // Finishing the older batch clears only its own tombstone: the
        // later pending batch and its tombstone survive untouched.
        rt.finish_udp(listener, &room(), taken);
        let bind_room = rt
            .rooms
            .get(&room())
            .expect("the later generation retains the room");
        assert!(!bind_room.udp_draining.contains(&(listener, first)));
        assert!(bind_room.udp_draining.contains(&(listener, second)));
        assert!(!bind_room.pending_udp[&listener].is_empty());

        // The transportless relay member leaves quietly, the next take
        // delivers only the later generation's owed batch, and
        // finishing it drops the emptied room.
        rt.leave(&room(), sender);
        let (datagrams, _, taken) = rt.take_udp(listener, &room());
        assert!(!datagrams.is_empty());
        assert!(datagrams.iter().all(|(address, _)| *address == second));
        rt.finish_udp(listener, &room(), taken);
        assert!(!rt.rooms.contains_key(&room()));
    }

    // --- H5: isolation effects delivered -----------------------------------

    #[test]
    fn oversize_isolation_delivers_survivor_and_listener_effects() {
        let mut rt = runtime();
        let (listener, udp_notifier) = rt.register_listener();
        let from = addr();
        rt.udp_datagram(listener, &room(), from, &fixture_syn());
        let player = rt.rooms[&room()].udp_addresses[&(listener, from)];
        let ws_waiter = Arc::new(Notify::new());
        rt.join(room(), Arc::clone(&ws_waiter)).expect("ws joins");

        // A third member relays one ordinary and one undecodable
        // oversized payload into the UDP peer's outbox.
        let now = rt.now();
        let bind_room = rt.rooms.get_mut(&room()).expect("room");
        let (other, _) = bind_room
            .host
            .join(now, |_| b"ws:relay".to_vec())
            .expect("member joins");
        let route = crate::runtime::RouteId::new(11).expect("nonzero");
        bind_room
            .host
            .relay(now, other, player, route, b"small")
            .expect("small relays");
        let big = vec![0xaa; MAX_DATAGRAM_LEN + 1];
        bind_room
            .host
            .relay(now, other, player, route, &big)
            .expect("fits the room-core bound");

        let (datagrams, effects) = drain(&mut rt, listener, &room());
        assert!(
            datagrams
                .iter()
                .all(|(_, bytes)| bytes.len() <= MAX_DATAGRAM_LEN),
            "no oversized datagram is ever emitted"
        );
        assert!(
            datagrams.iter().any(|(_, bytes)| bytes == b"small"),
            "ordinary output ahead of the oversize is sent"
        );
        // The oversized recipient is isolated; the role may cascade an
        // established departure into an abort that empties the room.
        if let Some(bind_room) = rt.rooms.get(&room()) {
            assert!(!bind_room.udp_addresses.contains_key(&(listener, from)));
            assert!(!bind_room.host.contains(player));
        }
        // Every effect of the isolation rides the returned Effects: the
        // WebSocket survivor is woken with its refreshed lobby state,
        // and the isolating listener is woken so newly captured removal
        // traffic is attempted instead of sleeping forever.
        assert!(
            effects
                .ws_waiters
                .iter()
                .any(|waiter| Arc::ptr_eq(waiter, &ws_waiter)),
            "the survivor wakeup is delivered"
        );
        assert!(
            effects
                .udp_waiters
                .iter()
                .any(|waiter| Arc::ptr_eq(waiter, &udp_notifier)),
            "the isolating listener is woken for the removal traffic"
        );
    }

    // --- loopback tests ----------------------------------------------------

    #[tokio::test]
    async fn two_listeners_one_remote_socket_distinct_admissions() {
        let room_name = RoomName::try_from("arena").expect("valid room");
        let ws_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ws listener binds");
        let socket1 = UdpSocket::bind("127.0.0.1:0").await.expect("l1 binds");
        let socket2 = UdpSocket::bind("127.0.0.1:0").await.expect("l2 binds");
        let l1 = socket1.local_addr().expect("l1 address");
        let l2 = socket2.local_addr().expect("l2 address");
        let server = tokio::spawn(crate::runtime::serve(
            ws_listener,
            vec![(room_name.clone(), socket1), (room_name, socket2)],
            NonZeroUsize::new(64).expect("nonzero"),
        ));
        let client = UdpSocket::bind("127.0.0.1:0").await.expect("client binds");

        // One remote socket SYNs to both listeners of the same room.
        client.send_to(&fixture_syn(), l1).await.expect("syn to l1");
        client.send_to(&fixture_syn(), l2).await.expect("syn to l2");

        // Collect datagrams by reply source port until both admissions
        // and the shared two-player lobby are observed on each socket.
        let mut accepts = std::collections::HashSet::new();
        let mut lobbies = std::collections::HashMap::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && (accepts.len() < 2 || lobbies.len() < 2) {
            let mut buf = vec![0u8; 2048];
            let Ok(Ok((len, source))) =
                timeout(Duration::from_millis(300), client.recv_from(&mut buf)).await
            else {
                continue;
            };
            match decode_server(&buf[..len], false) {
                ServerPacket::SynAccept(_) => {
                    accepts.insert(source);
                }
                ServerPacket::WaitingData(data) if data.players.len() == 2 => {
                    lobbies.insert(source, data);
                }
                _ => {}
            }
        }
        assert!(
            accepts.contains(&l1) && accepts.contains(&l2),
            "each listener answered its own SYN from its own socket"
        );
        let w1 = lobbies.get(&l1).expect("l1 two-player lobby");
        let w2 = lobbies.get(&l2).expect("l2 two-player lobby");
        // Distinct PlayerIds proven at the socket layer: the per-
        // recipient consoleplayer differs while the one shared role
        // reports the same two players through both sockets.
        assert_eq!(w1.consoleplayer, 0);
        assert_eq!(w2.consoleplayer, 1);
        assert_eq!(w1.players.len(), 2);
        assert_eq!(w2.players.len(), 2);

        // A stateless QUERY describes the same shared room and is
        // answered from the addressed socket only.
        client
            .send_to(&[0x00, 0x0d], l2)
            .await
            .expect("query to l2");
        let deadline = Instant::now() + Duration::from_secs(2);
        let (source, query) = loop {
            assert!(Instant::now() < deadline, "no query response arrived");
            let mut buf = vec![0u8; 2048];
            let Ok(Ok((len, source))) =
                timeout(Duration::from_millis(300), client.recv_from(&mut buf)).await
            else {
                continue;
            };
            if let ServerPacket::QueryResponse(query) = decode_server(&buf[..len], false) {
                break (source, query);
            }
        };
        assert_eq!(source, l2, "the query answer leaves from l2 only");
        assert_eq!(query.num_players, 2);

        server.abort();
    }

    #[tokio::test]
    async fn websocket_only_serve_persists_without_udp() {
        let ws_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ws listener binds");
        let ws_addr = ws_listener.local_addr().expect("ws address");
        let server = tokio::spawn(crate::runtime::serve(
            ws_listener,
            Vec::new(),
            NonZeroUsize::new(64).expect("nonzero"),
        ));
        let mut alice = ws_connect(&format!("ws://{ws_addr}/ws/solo")).await;
        alice.send(1, 20, &syn_bytes("Alice", 0, 0, 0));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut accepted = false;
        while !accepted && Instant::now() < deadline {
            if let Some((_, payload)) = alice.try_next(300).await {
                accepted = matches!(decode_server(&payload, false), ServerPacket::SynAccept(_));
            }
        }
        assert!(accepted, "the websocket handshake completes");
        assert!(!server.is_finished(), "a WebSocket-only serve persists");
        server.abort();
    }

    #[tokio::test]
    async fn udp_syn_fixture_produces_bare_accept_waiting_data_and_one_mapping() {
        let server = spawn_mixed("fixture").await;
        server.udp.send(&fixture_syn()).await.expect("syn sends");

        let accept = udp_recv(&server.udp, 1_000).await;
        assert_eq!(
            u16::from_be_bytes([accept[0], accept[1]]),
            0x8000,
            "bare datagram with the big-endian reliable SYN accept word"
        );
        assert!(matches!(
            decode_server(&accept, false),
            ServerPacket::SynAccept(_)
        ));
        let data = udp_recv(&server.udp, 1_000).await;
        match decode_server(&data, false) {
            ServerPacket::WaitingData(data) => {
                assert_eq!(data.players.len(), 1);
                assert_eq!(data.is_controller, 1);
            }
            other => panic!("expected waiting data, got {other:?}"),
        }

        // A duplicate SYN neither duplicates membership nor repeats the
        // accept; the member persists. Retried accepts and keepalives
        // may interleave with the cadence's next waiting data.
        server.udp.send(&fixture_syn()).await.expect("dup sends");
        udp_expect_silence(&server.udp, 400).await;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            assert!(Instant::now() < deadline, "no further waiting data");
            match decode_server(&udp_recv(&server.udp, 1_000).await, false) {
                ServerPacket::WaitingData(data) => {
                    assert_eq!(data.players.len(), 1);
                    break;
                }
                ServerPacket::Keepalive | ServerPacket::SynAccept(_) => continue,
                other => panic!("expected waiting data, got {other:?}"),
            }
        }

        server.server.abort();
    }

    #[tokio::test]
    async fn udp_old_magic_terminal_then_fresh_readmission() {
        let server = spawn_mixed("old").await;
        let mut old_syn = vec![0x00, 0x00];
        old_syn.extend_from_slice(&0xccd9_74d4u32.to_be_bytes());
        server.udp.send(&old_syn).await.expect("old magic sends");

        let rejected = udp_recv(&server.udp, 1_000).await;
        match decode_server(&rejected, false) {
            ServerPacket::Rejected { reason } => assert!(
                reason.starts_with(b"You are using an old client version that is not supported by this server. This server is running ")
            ),
            other => panic!("expected rejected, got {other:?}"),
        }
        udp_expect_silence(&server.udp, 400).await;

        // The mapping is gone once the terminal left the socket: the
        // same address is admitted fresh.
        server.udp.send(&fixture_syn()).await.expect("syn sends");
        let accept = udp_recv(&server.udp, 1_000).await;
        assert!(matches!(
            decode_server(&accept, false),
            ServerPacket::SynAccept(_)
        ));

        server.server.abort();
    }

    #[tokio::test]
    async fn udp_query_is_stateless() {
        let server = spawn_mixed("query").await;
        for _ in 0..2 {
            server.udp.send(&[0x00, 0x0d]).await.expect("query sends");
            let response = udp_recv(&server.udp, 1_000).await;
            match decode_server(&response, false) {
                ServerPacket::QueryResponse(data) => {
                    assert_eq!(data.server_state, 0);
                    assert_eq!(data.num_players, 0);
                }
                other => panic!("expected query response, got {other:?}"),
            }
        }

        // Nothing accumulated: a later SYN sees a one-player room.
        server.udp.send(&fixture_syn()).await.expect("syn sends");
        udp_recv(&server.udp, 1_000).await;
        let data = udp_recv(&server.udp, 1_000).await;
        match decode_server(&data, false) {
            ServerPacket::WaitingData(data) => assert_eq!(data.players.len(), 1),
            other => panic!("expected waiting data, got {other:?}"),
        }

        server.server.abort();
    }

    #[tokio::test]
    async fn mixed_room_shared_lobby_and_personalized_gamestart() {
        let server = spawn_mixed("mixed").await;
        // The UDP peer is the oldest: its SYN is admitted before the
        // WebSocket connection (and its join) exists.
        server
            .udp
            .send(&fixture_syn())
            .await
            .expect("udp syn sends");
        let accept = udp_recv(&server.udp, 1_000).await;
        assert!(matches!(
            decode_server(&accept, false),
            ServerPacket::SynAccept(_)
        ));
        let room_url = format!("ws://{}/ws/mixed", server.ws_addr);
        let mut alice = ws_connect(&room_url).await;

        alice.send(1, 20, &syn_bytes("Alice", 0, 0, 0));
        alice.send(1, 20, &ack_bytes(1));
        server.udp.send(&ack_bytes(1)).await.expect("udp ack sends");

        // Both transports converge on the shared two-player lobby: the
        // UDP peer is oldest (controller, consoleplayer 0), the
        // WebSocket peer second.
        let mut udp_wait = None;
        let mut ws_wait = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && (udp_wait.is_none() || ws_wait.is_none()) {
            if udp_wait.is_none()
                && let Some(bytes) = udp_try_recv(&server.udp, 200).await
                && let ServerPacket::WaitingData(data) = decode_server(&bytes, false)
                && data.players.len() == 2
            {
                udp_wait = Some(data);
            }
            if ws_wait.is_none()
                && let Some((_, payload)) = alice.try_next(200).await
                && let ServerPacket::WaitingData(data) = decode_server(&payload, false)
                && data.players.len() == 2
            {
                ws_wait = Some(data);
            }
        }
        let udp_wait = udp_wait.expect("udp two-player lobby");
        let ws_wait = ws_wait.expect("ws two-player lobby");
        assert_eq!(udp_wait.is_controller, 1);
        assert_eq!(udp_wait.consoleplayer, 0);
        assert_eq!(ws_wait.is_controller, 0);
        assert_eq!(ws_wait.consoleplayer, 1);

        // The controller (the UDP peer) launches; both drive GAMESTART,
        // deathmatch 1 from the controller only.
        server.udp.send(&launch_bytes(0)).await.expect("udp launch");
        server.udp.send(&ack_bytes(2)).await.expect("udp ack2");
        alice.send(1, 20, &ack_bytes(2));
        server
            .udp
            .send(&settings_bytes(1, 1))
            .await
            .expect("udp gamestart");
        alice.send(1, 20, &settings_bytes(0, 0));

        // Personalized authoritative GAMESTART on both transports:
        // consoleplayer 0 for the UDP peer, 1 for the WebSocket peer,
        // deathmatch 1 preserved end to end.
        let mut udp_start = None;
        let mut ws_start = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && (udp_start.is_none() || ws_start.is_none()) {
            if udp_start.is_none()
                && let Some(bytes) = udp_try_recv(&server.udp, 200).await
                && let ServerPacket::GameStart(settings) = decode_server(&bytes, false)
            {
                udp_start = Some(settings);
            }
            if ws_start.is_none()
                && let Some((_, payload)) = alice.try_next(200).await
                && let ServerPacket::GameStart(settings) = decode_server(&payload, false)
            {
                ws_start = Some(settings);
            }
        }
        let udp_start = udp_start.expect("udp gamestart");
        let ws_start = ws_start.expect("ws gamestart");
        assert_eq!(udp_start.consoleplayer, 0);
        assert_eq!(udp_start.deathmatch, 1);
        assert_eq!(ws_start.consoleplayer, 1);
        assert_eq!(ws_start.deathmatch, 1);

        server.server.abort();
    }

    #[tokio::test]
    async fn mixed_narrow_turn_crosses_transports_at_the_room_width() {
        let server = spawn_mixed("narrow").await;
        // The UDP peer is the oldest (and therefore the controller that
        // launches): its SYN is admitted before the WebSocket join.
        server
            .udp
            .send(&syn_bytes("Udp", 0, 0, 1))
            .await
            .expect("udp syn sends");
        let accept = udp_recv(&server.udp, 1_000).await;
        assert!(matches!(
            decode_server(&accept, false),
            ServerPacket::SynAccept(_)
        ));
        let room_url = format!("ws://{}/ws/narrow", server.ws_addr);
        let mut alice = ws_connect(&room_url).await;

        alice.send(1, 20, &syn_bytes("Alice", 0, 0, 1));
        server.udp.send(&ack_bytes(1)).await.expect("udp ack");
        alice.send(1, 20, &ack_bytes(1));

        // Both transports converge on the shared two-player lobby
        // before the controller is allowed to launch.
        let mut udp_wait = false;
        let mut ws_wait = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !(udp_wait && ws_wait) {
            if !udp_wait
                && let Some(bytes) = udp_try_recv(&server.udp, 200).await
                && let ServerPacket::WaitingData(data) = decode_server(&bytes, true)
                && data.players.len() == 2
            {
                udp_wait = true;
            }
            if !ws_wait
                && let Some((_, payload)) = alice.try_next(200).await
                && let ServerPacket::WaitingData(data) = decode_server(&payload, true)
                && data.players.len() == 2
            {
                ws_wait = true;
            }
        }
        assert!(udp_wait && ws_wait, "both transports saw the lobby");

        server.udp.send(&launch_bytes(0)).await.expect("udp launch");
        server.udp.send(&ack_bytes(2)).await.expect("udp ack2");
        alice.send(1, 20, &ack_bytes(2));
        server
            .udp
            .send(&settings_bytes(1, 0))
            .await
            .expect("udp gamestart");
        alice.send(1, 20, &settings_bytes(0, 0));

        // Wait for the authoritative GAMESTART on both transports.
        let mut udp_started = false;
        let mut ws_started = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !(udp_started && ws_started) {
            if !udp_started
                && let Some(bytes) = udp_try_recv(&server.udp, 200).await
                && let ServerPacket::GameStart(_) = decode_server(&bytes, true)
            {
                udp_started = true;
            }
            if !ws_started
                && let Some((_, payload)) = alice.try_next(200).await
                && let ServerPacket::GameStart(_) = decode_server(&payload, true)
            {
                ws_started = true;
            }
        }
        assert!(
            udp_started && ws_started,
            "both transports reached gamestart"
        );

        // The UDP peer uploads a narrow nonzero angleturn.
        let upload = ClientPacket::GameData(doom_proto::GameDataClient {
            ack: 0,
            start: 0,
            tics: vec![doom_proto::ClientTic {
                latency: 1,
                diff: TiccmdDiff {
                    turn: Some(0x100),
                    ..Default::default()
                },
            }],
        })
        .encode(WireHeader { reliable_seq: None }, true)
        .expect("narrow upload encodes");
        server.udp.send(&upload).await.expect("upload sends");

        // The WebSocket peer receives the fan-out decodable only at the
        // room width, with the same value at the UDP peer's frozen
        // index, among the pump's ordinary game data.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = None;
        while found.is_none() && Instant::now() < deadline {
            if let Some((_, payload)) = alice.try_next(300).await
                && let Ok((_, ServerPacket::GameData(data))) = ServerPacket::decode(&payload, true)
                && data.tics.iter().any(|tic| {
                    tic.players
                        .iter()
                        .any(|(index, diff)| *index == 0 && diff.turn == Some(0x100))
                })
            {
                found = Some((payload, data));
            }
        }
        let (frame, data) = found.expect("the narrow turn crossed to the ws peer");
        assert_eq!(data.tics[0].players[0].0, 0);
        assert_eq!(data.tics[0].players[0].1.turn, Some(0x100));
        assert!(
            ServerPacket::decode(&frame, false).is_err(),
            "the fan-out cannot be read at the wrong width"
        );

        server.server.abort();
    }

    #[tokio::test]
    async fn udp_timer_traffic_shared_reducer_and_no_post_shutdown_work() {
        let server = spawn_mixed("timer").await;
        server.udp.send(&fixture_syn()).await.expect("syn sends");

        // No acknowledgement and no input: the shared timer drives the
        // cadence and retry, exactly once per period (no duplicate
        // timer).
        let mut accepts = 0;
        let mut waiting = 0;
        let deadline = Instant::now() + Duration::from_millis(1_600);
        while Instant::now() < deadline && (accepts < 2 || waiting < 2) {
            match decode_server(&udp_recv(&server.udp, 1_600).await, false) {
                ServerPacket::SynAccept(_) => accepts += 1,
                ServerPacket::WaitingData(_) => waiting += 1,
                ServerPacket::Keepalive => {}
                other => panic!("unexpected packet {other:?}"),
            }
        }
        assert_eq!(accepts, 2, "the unacknowledged head retried once");
        assert_eq!(waiting, 2, "the cadence produced a second waiting data");

        server.server.abort();
        udp_expect_silence(&server.udp, 1_500).await;
    }
}
