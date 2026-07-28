//! One transport-neutral room host: exactly one room membership/outbox
//! set, one authoritative `ServerRole`, and the binding identity the
//! host stamps on server-originated sends. No sockets, no async, and
//! no transport envelope knowledge: bindings feed decoded inputs and
//! apply the returned effects, so no WebSocket-specific protocol state
//! exists here that a later UDP binding would duplicate.

use std::num::NonZeroUsize;

use doom_proto::{ClientPacket, WireHeader};

use crate::server_role::{Action, Input, MalformedClass, Milliseconds, ServerRole};
use crate::{
    HostError, HostOutcome, JoinError, OutboundPacket, PlayerId, Registry, RelayError,
    RelayOutcome, RoomName,
};

/// What the binding must apply after one consistent reduction batch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostEffect {
    /// Peers whose writers should wake: new host-originated outbound
    /// packets are queued for them.
    pub wakes: Vec<PlayerId>,
    /// Peers to close, each with the optional terminal server frame
    /// (encoded) owed as the final binary message before the close.
    pub disconnects: Vec<(PlayerId, Option<Vec<u8>>)>,
    /// The role returned the room to waiting-for-launch during this
    /// batch; the room itself persists and must not be duplicated.
    pub game_ended: bool,
}

impl HostEffect {
    fn merge(&mut self, other: HostEffect) {
        self.wakes.extend(other.wakes);
        self.disconnects.extend(other.disconnects);
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
}

impl<Metadata: Clone> RoomHost<Metadata> {
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

    /// Removes and returns the oldest pending packet for a player.
    pub fn pop_outbound(&mut self, player: PlayerId) -> Option<OutboundPacket<Metadata>> {
        self.registry.pop_outbound(player)
    }

    /// The room's authoritative `lowres_turn` codec context.
    pub fn lowres_turn(&self) -> bool {
        self.role.lowres_turn()
    }

    /// Admission: bounded registry membership first, then exactly one
    /// `Input::Join`; a failed admission never reaches the role. The
    /// label is volatile lobby metadata computed from the new id.
    pub fn join(
        &mut self,
        now: Milliseconds,
        addr_label: impl FnOnce(PlayerId) -> Vec<u8>,
    ) -> Result<(PlayerId, HostEffect), JoinError> {
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
    pub fn leave(&mut self, now: Milliseconds, player: PlayerId) -> HostEffect {
        let actions = self.role.handle(now, Input::Leave { player });
        let effect = self.reduce(now, Vec::new(), actions);
        self.registry.leave(player);
        effect
    }

    /// One decoded client packet addressed to the server.
    pub fn packet(
        &mut self,
        now: Milliseconds,
        player: PlayerId,
        header: WireHeader,
        packet: ClientPacket,
    ) -> HostEffect {
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
    ) -> HostEffect {
        let actions = self.role.handle(now, Input::Malformed { player, class });
        self.reduce(now, Vec::new(), actions)
    }

    /// The runtime timer: all cadence, retry, resend, and in-game work.
    pub fn tick(&mut self, now: Milliseconds) -> HostEffect {
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
    ) -> Result<(RelayOutcome, HostEffect), RelayError> {
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
    ) -> HostEffect {
        let mut effect = HostEffect::default();
        let mut removals = removals;
        let mut pending = actions;
        while !pending.is_empty() {
            let mut next = Vec::new();
            for action in pending.drain(..) {
                match action {
                    Action::Send {
                        player,
                        header,
                        packet,
                    } => {
                        let bytes = packet
                            .encode(header, self.role.lowres_turn())
                            .expect("role outputs always encode");
                        match self
                            .registry
                            .queue_host(player, self.server_metadata.clone(), &bytes)
                        {
                            Ok(HostOutcome::Queued(_)) => {
                                if !effect.wakes.contains(&player) {
                                    effect.wakes.push(player);
                                }
                            }
                            Ok(HostOutcome::SlowConsumerDisconnected(_)) => {
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
            if !effect
                .disconnects
                .iter()
                .any(|(disconnected, _)| *disconnected == player)
            {
                effect.disconnects.push((player, None));
            }
        }
        effect
    }
}

#[cfg(test)]
mod tests;
