//! One transport-neutral room host: exactly one room membership/outbox
//! set, one authoritative `ServerRole`, and the binding identity the
//! host stamps on server-originated sends. No sockets, no async, and
//! no transport envelope knowledge: bindings feed decoded inputs and
//! apply the returned effects, so no WebSocket-specific protocol state
//! exists here that a later UDP binding would duplicate.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;

use doom_proto::{ClientPacket, WireHeader};

use crate::core::MAX_HOST_BATCH;
use crate::server_role::{Action, Input, MalformedClass, Milliseconds, ServerRole};
use crate::{
    HostError, HostOutcome, JoinError, OutboundPacket, PlayerId, Registry, RelayError,
    RelayOutcome, RoomName,
};

/// One outbound packet owed to a peer: the opaque payload, the binding
/// metadata stamped at queue time (the server route for host sends,
/// the sender's route for relays), and for host-produced packets the
/// codec context authoritative at production time. Relay packets carry
/// no codec context: their payload is opaque and must never be
/// decoded for width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwedPacket<Metadata> {
    metadata: Metadata,
    payload: Vec<u8>,
    lowres: Option<bool>,
}

impl<Metadata> OwedPacket<Metadata> {
    /// Returns the transport-owned metadata.
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Returns the opaque packet bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The production-time codec width for host-produced packets;
    /// `None` for opaque relay payloads.
    pub const fn lowres(&self) -> Option<bool> {
        self.lowres
    }

    /// Consumes the packet and returns its payload bytes.
    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }
}

/// What the binding must apply after one consistent reduction batch.
#[derive(Clone, Debug, PartialEq)]
pub struct HostEffect<Metadata> {
    /// Peers whose writers should wake: new host-originated outbound
    /// packets are queued for them.
    pub wakes: Vec<PlayerId>,
    /// Peers to close, each with the optional terminal server frame
    /// (encoded) owed as the final binary message before the close.
    pub disconnects: Vec<(PlayerId, Option<Vec<u8>>)>,
    /// Packets still owed to each removed peer at reduction time (its
    /// remaining outbox, FIFO, with each packet's binding metadata and
    /// codec tag), captured before the registry dropped it: the
    /// binding delivers them first and the terminal last.
    pub removal_owed: Vec<(PlayerId, Vec<OwedPacket<Metadata>>)>,
    /// The role returned the room to waiting-for-launch during this
    /// batch; the room itself persists and must not be duplicated.
    pub game_ended: bool,
}

impl<Metadata> Default for HostEffect<Metadata> {
    fn default() -> Self {
        Self {
            wakes: Vec::new(),
            disconnects: Vec::new(),
            removal_owed: Vec::new(),
            game_ended: false,
        }
    }
}

impl<Metadata> HostEffect<Metadata> {
    fn merge(&mut self, other: HostEffect<Metadata>) {
        self.wakes.extend(other.wakes);
        self.disconnects.extend(other.disconnects);
        self.removal_owed.extend(other.removal_owed);
        self.game_ended |= other.game_ended;
    }
}

/// One named room: its membership/outbox registry, its protocol role,
/// and the host's own send identity, reduced together under one lock
/// by the binding.
#[derive(Debug)]
pub struct RoomHost<Metadata> {
    room_name: RoomName,
    registry: Registry<Metadata>,
    role: ServerRole,
    server_metadata: Metadata,
    /// Per-player FIFO of the codec widths the host stamped on each
    /// host-produced packet it queued, aligned with the registry
    /// outbox: relay packets interleave freely (they are opaque) and
    /// every removal drops the player's queue wholesale. This is what
    /// lets a drained or captured packet be decoded later with the
    /// width that was authoritative when it was produced, never the
    /// live room width.
    widths: HashMap<PlayerId, VecDeque<bool>>,
}

impl<Metadata: Clone + PartialEq> RoomHost<Metadata> {
    /// Creates the host for one room with the given outbox bound and
    /// the metadata stamped on every host-originated send.
    pub fn new(
        room_name: RoomName,
        outbox_capacity: NonZeroUsize,
        server_metadata: Metadata,
    ) -> Self {
        Self {
            registry: Registry::new(outbox_capacity),
            role: ServerRole::new(),
            room_name,
            server_metadata,
            widths: HashMap::new(),
        }
    }

    /// Whether the room holds no member (the binding then drops it).
    pub fn is_empty(&self) -> bool {
        self.registry.room_size(&self.room_name) == 0
    }

    /// Whether a player is still joined.
    pub fn contains(&self, player: PlayerId) -> bool {
        self.registry.contains(player)
    }

    /// Removes and returns the oldest pending packet for a player,
    /// tagged with its binding metadata and, for host-produced
    /// packets, its production-time codec width.
    pub fn pop_outbound(&mut self, player: PlayerId) -> Option<OwedPacket<Metadata>> {
        let packet = self.registry.pop_outbound(player)?;
        Some(self.tag(player, packet))
    }

    /// Pair one registry packet with its codec tag: host-produced
    /// packets (stamped with the server metadata) pop their width in
    /// outbox order; relay packets stay opaque with no context.
    fn tag(&mut self, player: PlayerId, packet: OutboundPacket<Metadata>) -> OwedPacket<Metadata> {
        let lowres = if *packet.metadata() == self.server_metadata {
            let lowres = self.widths.get_mut(&player).and_then(VecDeque::pop_front);
            debug_assert!(lowres.is_some(), "host packet missing its codec tag");
            lowres
        } else {
            None
        };
        OwedPacket {
            metadata: packet.metadata().clone(),
            payload: packet.payload().to_vec(),
            lowres,
        }
    }

    /// The room's authoritative `lowres_turn` codec context.
    pub fn lowres_turn(&self) -> bool {
        self.role.lowres_turn()
    }

    /// The stateless query answer for the room's current state; no
    /// peer, identity, or retention is created for the querier.
    pub fn query_response(&self) -> doom_proto::ServerPacket {
        self.role.query_response()
    }

    /// Admission: bounded registry membership first, then exactly one
    /// `Input::Join`; a failed admission never reaches the role. The
    /// label is volatile lobby metadata computed from the new id.
    pub fn join(
        &mut self,
        now: Milliseconds,
        addr_label: impl FnOnce(PlayerId) -> Vec<u8>,
    ) -> Result<(PlayerId, HostEffect<Metadata>), JoinError> {
        let player = self.registry.join(self.room_name.clone())?;
        let actions = self.role.handle(
            now,
            Input::Join {
                player,
                addr_label: addr_label(player),
            },
        );
        let effect = self.reduce(now, Vec::new(), actions);
        Ok((player, effect))
    }

    /// Transport hangup, binding rejection after admission, or any
    /// other binding-initiated removal: exactly one `Input::Leave`,
    /// reduced until quiescent, so no protocol peer survives the batch.
    /// When the role has already removed the peer itself (a
    /// role-originated `Action::Disconnect`, whose registry removal
    /// rode its action batch), the later transport cleanup is inert by
    /// construction: zero `Input::Leave`, the initiator contract every
    /// binding relies on.
    pub fn leave(&mut self, now: Milliseconds, player: PlayerId) -> HostEffect<Metadata> {
        if !self.registry.contains(player) {
            return HostEffect::default();
        }
        let actions = self.role.handle(now, Input::Leave { player });
        let effect = self.reduce(now, Vec::new(), actions);
        self.registry.leave(player);
        self.widths.remove(&player);
        effect
    }

    /// One decoded client packet addressed to the server.
    pub fn packet(
        &mut self,
        now: Milliseconds,
        player: PlayerId,
        header: WireHeader,
        packet: ClientPacket,
    ) -> HostEffect<Metadata> {
        let actions = self.role.handle(
            now,
            Input::Packet {
                player,
                header,
                packet,
            },
        );
        self.reduce(now, Vec::new(), actions)
    }

    /// Bytes that did not decode, classified by the binding.
    pub fn malformed(
        &mut self,
        now: Milliseconds,
        player: PlayerId,
        class: MalformedClass,
    ) -> HostEffect<Metadata> {
        let actions = self.role.handle(now, Input::Malformed { player, class });
        self.reduce(now, Vec::new(), actions)
    }

    /// The runtime timer: all cadence, retry, resend, and in-game work.
    pub fn tick(&mut self, now: Milliseconds) -> HostEffect<Metadata> {
        let actions = self.role.handle(now, Input::Timer);
        self.reduce(now, Vec::new(), actions)
    }

    /// Peer-to-peer relay through the same bounded outboxes; a slow
    /// consumer is removed and its single `Input::Leave` is fed into
    /// the role in the same batch.
    pub fn relay(
        &mut self,
        now: Milliseconds,
        sender: PlayerId,
        recipient: PlayerId,
        metadata: Metadata,
        payload: &[u8],
    ) -> Result<(RelayOutcome, HostEffect<Metadata>), RelayError> {
        let outcome = self.registry.relay(sender, recipient, metadata, payload)?;
        let mut effect = HostEffect::default();
        match outcome {
            RelayOutcome::Unroutable => {}
            RelayOutcome::Queued(player) => {
                if !effect.wakes.contains(&player) {
                    effect.wakes.push(player);
                }
            }
            RelayOutcome::SlowConsumerDisconnected(player) => {
                self.widths.remove(&player);
                let actions = self.role.handle(now, Input::Leave { player });
                effect.merge(self.reduce(now, vec![player], actions));
            }
        }
        Ok((outcome, effect))
    }

    /// Apply one action batch, then keep reducing: every host-origin
    /// send that removes a slow consumer feeds exactly one
    /// `Input::Leave` back into the role until no new actions appear.
    /// Role-originated removals never feed a second leave, each peer is
    /// woken at most once, and every registry-level removal (the seed
    /// plus any slow consumer) is reported exactly once: normally by
    /// the role's own `Action::Disconnect`, or by a plain entry when
    /// the role had no peer to confirm.
    fn reduce(
        &mut self,
        now: Milliseconds,
        removals: Vec<PlayerId>,
        actions: Vec<Action>,
    ) -> HostEffect<Metadata> {
        // One reduction is one atomic producer batch: consumer backlog
        // is measured at this moment, never per packet inside the
        // batch, so a valid burst (for example 64 contiguous expired
        // resend runs plus the pump/retry/keepalive of one timer pass)
        // cannot disconnect a draining peer.
        self.registry.begin_host_batch();
        let mut effect = HostEffect::default();
        let mut removals = removals;
        let mut pending = actions;
        // The producer invariant, enforced distinctly from the
        // slow-consumer policy: one reduction may queue at most
        // MAX_HOST_BATCH host-originated sends for one recipient,
        // across the initial batch and every recursively induced
        // Leave batch. Exceeding it is a producer bug, never a slow
        // peer, so it fails loudly instead of disconnecting anyone.
        let mut produced: Vec<(PlayerId, usize)> = Vec::new();
        while !pending.is_empty() {
            let mut next = Vec::new();
            for action in pending.drain(..) {
                match action {
                    Action::Send {
                        player,
                        header,
                        packet,
                    } => {
                        // The codec context is stamped at production
                        // time: the bytes and their width tag travel
                        // together through the outbox and any removal
                        // capture, never re-read from the live role.
                        let lowres = self.role.lowres_turn();
                        let bytes = packet
                            .encode(header, lowres)
                            .expect("role outputs always encode");
                        {
                            let position = produced
                                .iter()
                                .position(|(candidate, _)| *candidate == player);
                            let index = match position {
                                Some(index) => index,
                                None => {
                                    produced.push((player, 0));
                                    produced.len() - 1
                                }
                            };
                            produced[index].1 += 1;
                            assert!(
                                produced[index].1 <= MAX_HOST_BATCH,
                                "one reduction produced more than {MAX_HOST_BATCH} host sends for {player:?}"
                            );
                        }
                        match self
                            .registry
                            .queue_host(player, self.server_metadata.clone(), &bytes)
                        {
                            Ok(HostOutcome::Queued(_)) => {
                                self.widths.entry(player).or_default().push_back(lowres);
                                if !effect.wakes.contains(&player) {
                                    effect.wakes.push(player);
                                }
                            }
                            Ok(HostOutcome::SlowConsumerDisconnected(_)) => {
                                self.widths.remove(&player);
                                removals.push(player);
                                next.extend(self.role.handle(now, Input::Leave { player }));
                            }
                            Err(HostError::UnknownPlayer) => {}
                            Err(HostError::PayloadTooLarge { .. }) => {
                                unreachable!("server packets are bounded")
                            }
                        }
                    }
                    Action::Disconnect {
                        player, terminal, ..
                    } => {
                        // Capture every already-owed packet with its
                        // metadata and codec tag before the registry
                        // drops the outbox: the binding delivers the
                        // exact queued prefix first and the terminal
                        // last. Registry-level (slow-consumer) removals
                        // never take this path and never flush a
                        // backlog.
                        let mut owed = Vec::new();
                        while let Some(packet) = self.registry.pop_outbound(player) {
                            owed.push(self.tag(player, packet));
                        }
                        self.widths.remove(&player);
                        if !owed.is_empty()
                            && !effect
                                .removal_owed
                                .iter()
                                .any(|(candidate, _)| *candidate == player)
                        {
                            effect.removal_owed.push((player, owed));
                        }
                        self.registry.leave(player);
                        let terminal = terminal.map(|boxed| {
                            let (header, packet) = *boxed;
                            packet
                                .encode(header, self.role.lowres_turn())
                                .expect("terminal packets always encode")
                        });
                        if !effect
                            .disconnects
                            .iter()
                            .any(|(disconnected, _)| *disconnected == player)
                        {
                            effect.disconnects.push((player, terminal));
                        }
                    }
                    Action::GameEnded => effect.game_ended = true,
                }
            }
            pending = next;
        }
        for player in removals {
            self.widths.remove(&player);
            if !effect
                .disconnects
                .iter()
                .any(|(disconnected, _)| *disconnected == player)
            {
                effect.disconnects.push((player, None));
            }
        }
        self.registry.end_host_batch();
        effect
    }
}

#[cfg(test)]
mod tests;
