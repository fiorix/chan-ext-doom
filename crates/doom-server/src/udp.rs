//! UDP binding: one Chocolate packet per datagram, byte-exact with no
//! envelope. Address identity is the full `SocketAddr`; the mapping is
//! binding-owned and removed in the same reduction as the registry
//! removal. UDP has no hangup signal, so silence is handled only by
//! the role's own timer through the shared reducer.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::Notify;

use doom_proto::{ClientPacket, WireHeader};

use crate::runtime::{Effects, Runtime, SharedState, classify_malformed};
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
    let notifier = Arc::new(Notify::new());
    {
        let mut runtime = state.0.lock().await;
        runtime
            .udp_notifiers
            .insert(room_name.clone(), Arc::clone(&notifier));
    }
    let mut buf = vec![0u8; RECV_BUF_LEN];
    loop {
        tokio::select! {
            received = socket.recv_from(&mut buf) => {
                let Ok((len, from)) = received else {
                    break;
                };
                let effects = {
                    let mut runtime = state.0.lock().await;
                    runtime.udp_datagram(&room_name, from, &buf[..len])
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
            runtime.drain_udp(&room_name)
        };
        for (address, bytes) in datagrams {
            // Send failures are isolated and bounded: the datagram is
            // dropped, role state is untouched, no queue is retained.
            let _ = socket.send_to(&bytes, address).await;
        }
    }
}

impl Runtime {
    /// One inbound datagram for a pinned room. A mapped address feeds
    /// the same server-payload path as any transport. Unknown source
    /// addresses are admitted only by a valid SYN (or the old-magic
    /// refusal path, which removes them in the same batch); QUERY is
    /// answered statelessly; everything else is the pinned silent
    /// behavior with no admission.
    pub(crate) fn udp_datagram(
        &mut self,
        room_name: &RoomName,
        from: SocketAddr,
        payload: &[u8],
    ) -> Effects {
        let now = self.now();
        // Oversized original datagrams are rejected before decode: a
        // truncated packet must never masquerade as a valid one.
        if payload.len() > MAX_DATAGRAM_LEN {
            let notifier = self.udp_notifiers.get(room_name).cloned();
            if let Some(room) = self.rooms.get_mut(room_name)
                && let Some(player) = room.udp_addresses.get(&from).copied()
            {
                let effect = room
                    .host
                    .malformed(now, player, MalformedClass::Established);
                return room.apply(effect, notifier.as_ref());
            }
            return Effects::default();
        }

        if let Some(room) = self.rooms.get_mut(room_name)
            && let Some(player) = room.udp_addresses.get(&from).copied()
        {
            let notifier = self.udp_notifiers.get(room_name).cloned();
            let effect = room.server_payload(now, player, payload);
            return room.apply(effect, notifier.as_ref());
        }

        // Unknown address: decode with the room's authoritative width
        // (wide before GAMESTART; only SYN and QUERY matter here, and
        // both are width-independent).
        let lowres = self
            .rooms
            .get(room_name)
            .map(|room| room.host.lowres_turn())
            .unwrap_or(false);
        match ClientPacket::decode(payload, lowres) {
            Ok((header, packet @ ClientPacket::Syn(_))) => {
                // A valid SYN is admitted registry-first and mapped
                // atomically, then fed through the normal packet path.
                let notifier = self.udp_notifiers.get(room_name).cloned();
                let room = self.room_or_insert(room_name);
                let Ok((player, effect)) = room.host.join(now, |_| from.to_string().into_bytes())
                else {
                    // A full room refuses silently at the binding.
                    return Effects::default();
                };
                let mut effects = room.apply(effect, notifier.as_ref());
                room.udp_players.insert(player, from);
                room.udp_addresses.insert(from, player);
                let effect = room.host.packet(now, player, header, packet);
                effects.merge(room.apply(effect, notifier.as_ref()));
                effects
            }
            Ok((_, ClientPacket::Query)) => {
                // Stateless: no PlayerId, mapping, member, or
                // room-retaining connection is allocated. An absent
                // room is described by a transient fresh role.
                let response = self
                    .rooms
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
                    let notifier = self.udp_notifiers.get(room_name).cloned();
                    let room = self.room_or_insert(room_name);
                    let Ok((player, effect)) =
                        room.host.join(now, |_| from.to_string().into_bytes())
                    else {
                        return Effects::default();
                    };
                    let mut effects = room.apply(effect, notifier.as_ref());
                    room.udp_players.insert(player, from);
                    room.udp_addresses.insert(from, player);
                    let effect =
                        room.host
                            .malformed(now, player, MalformedClass::Syn { old_magic: true });
                    effects.merge(room.apply(effect, notifier.as_ref()));
                    effects
                }
                // Wrong magic, non-SYN, truncated, wrong-direction:
                // the pinned silent behavior, no admission.
                _ => Effects::default(),
            },
        }
    }

    /// Drain every owed datagram for a room: pending removal traffic
    /// first (terminal last per removed peer), then each mapped peer's
    /// outbox FIFO. An outbound datagram over the 1500-byte ceiling is
    /// a distinct producer error: it is never truncated, split,
    /// silently dropped, or sent through the slow-consumer path, and it
    /// isolates exactly its intended recipient through the normal
    /// removal path while every other peer continues.
    pub(crate) fn drain_udp(&mut self, room_name: &RoomName) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        let mut oversized: Vec<(PlayerId, usize)> = Vec::new();
        {
            let Some(room) = self.rooms.get_mut(room_name) else {
                return out;
            };
            while let Some((address, bytes)) = room.pending_udp.pop_front() {
                if bytes.len() > MAX_DATAGRAM_LEN {
                    eprintln!(
                        "doomd udp: producer error: {} bytes exceeds the 1500-byte datagram ceiling for {address}; discarded with its removed peer",
                        bytes.len()
                    );
                    continue;
                }
                out.push((address, bytes));
            }
            let players: Vec<(PlayerId, SocketAddr)> = room
                .udp_players
                .iter()
                .map(|(player, address)| (*player, *address))
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
            let now = self.now();
            let notifier = self.udp_notifiers.get(room_name).cloned();
            if let Some(room) = self.rooms.get_mut(room_name) {
                let effect = room.host.leave(now, player);
                room.apply(effect, notifier.as_ref());
            }
        }
        out
    }
}
