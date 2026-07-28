//! UDP binding: one Chocolate packet per datagram, byte-exact with no
//! envelope. Address identity is the `(listener, SocketAddr)` pair;
//! the mapping is binding-owned and removed in the same reduction as
//! the registry removal. UDP has no hangup signal, so silence is
//! handled only by the role's own timer through the shared reducer.

use std::net::SocketAddr;

use tokio::net::UdpSocket;

use doom_proto::{ClientPacket, WireHeader};

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

/// One listener task per configured `--udp ROOM=ADDR` bind: receives
/// datagrams for its pinned room, drives the shared runtime, and sends
/// every queued datagram FIFO as individual packets. Its lifetime is
/// the serve future's through the composition root's join.
pub(crate) async fn listener(socket: UdpSocket, room_name: RoomName, state: SharedState) {
    let (listener_id, notifier) = {
        let mut runtime = state.0.lock().await;
        runtime.register_listener()
    };
    let mut buf = vec![0u8; RECV_BUF_LEN];
    loop {
        tokio::select! {
            received = socket.recv_from(&mut buf) => {
                let Ok((len, from)) = received else {
                    break;
                };
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
        let datagrams = {
            let mut runtime = state.0.lock().await;
            runtime.drain_udp(listener_id, &room_name)
        };
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
        // both are width-independent).
        let lowres = rooms
            .get(room_name)
            .map(|room| room.host.lowres_turn())
            .unwrap_or(false);
        match ClientPacket::decode(payload, lowres) {
            Ok((header, packet @ ClientPacket::Syn(_))) => {
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
    /// then each of its mapped peers' outboxes FIFO. An outbound
    /// datagram over the 1500-byte ceiling is a distinct producer
    /// error: it is never truncated, split, silently dropped, or sent
    /// through the slow-consumer path, and it isolates exactly its
    /// intended recipient through the normal removal path while every
    /// other peer continues.
    pub(crate) fn drain_udp(
        &mut self,
        listener: ListenerId,
        room_name: &RoomName,
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        let now = self.now();
        let Self {
            rooms,
            udp_notifiers,
            ..
        } = self;
        let mut out = Vec::new();
        let mut oversized: Vec<(PlayerId, usize)> = Vec::new();
        {
            let Some(room) = rooms.get_mut(room_name) else {
                return out;
            };
            if let Some(pending) = room.pending_udp.get_mut(&listener) {
                while let Some((address, bytes)) = pending.pop_front() {
                    if bytes.len() > MAX_DATAGRAM_LEN {
                        eprintln!(
                            "doomd udp: producer error: {} bytes exceeds the 1500-byte datagram ceiling for {address}; discarded with its removed peer",
                            bytes.len()
                        );
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
                room.apply(effect, udp_notifiers);
            }
        }
        out
    }
}
