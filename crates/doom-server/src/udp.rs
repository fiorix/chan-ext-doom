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
        let (datagrams, effects) = {
            let mut runtime = state.0.lock().await;
            runtime.drain_udp(listener_id, &room_name)
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

    /// Drain every owed datagram for one listener of a room: its
    /// pending removal traffic first (terminal last per removed peer),
    /// then each of its mapped peers' outboxes FIFO. Capturing the
    /// pending batch for the send attempt clears this listener's
    /// removal tombstones, and a room left empty afterwards is dropped
    /// here, so pending traffic is part of room lifetime. An outbound
    /// datagram over the 1500-byte ceiling is first offered to the
    /// accepted `GAMEDATA` newest-suffix adaptation; only a packet that
    /// cannot be adapted is a distinct producer error, never truncated,
    /// split, silently dropped, or sent through the slow-consumer path,
    /// isolating exactly its intended recipient through the normal
    /// removal path while every other peer continues. The effects of
    /// that isolation (survivor wakeups, the isolated peer's own
    /// terminal capture) ride the returned `Effects` through the same
    /// post-lock delivery path as every other reduction.
    pub(crate) fn drain_udp(
        &mut self,
        listener: ListenerId,
        room_name: &RoomName,
    ) -> (Vec<(SocketAddr, Vec<u8>)>, Effects) {
        let now = self.now();
        let Self {
            rooms,
            udp_notifiers,
            ..
        } = self;
        let mut out = Vec::new();
        let mut effects = Effects::default();
        let mut oversized: Vec<(PlayerId, usize)> = Vec::new();
        {
            let Some(room) = rooms.get_mut(room_name) else {
                return (out, effects);
            };
            let lowres = room.host.lowres_turn();
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
            // This listener's pending batch is captured above for the
            // send attempt that follows the lock release, so its
            // removal tombstones clear and re-admission opens.
            room.udp_draining
                .retain(|(candidate, _)| *candidate != listener);
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
        }
        for (player, _) in oversized {
            if let Some(room) = rooms.get_mut(room_name) {
                let effect = room.host.leave(now, player);
                effects.merge(room.apply(effect, udp_notifiers));
            }
        }
        // Final empty-room cleanup: a room whose last batch this drain
        // attempted now drops, and the next valid SYN starts fresh.
        if rooms.get(room_name).is_some_and(BindRoom::is_empty) {
            rooms.remove(room_name);
        }
        (out, effects)
    }
}
