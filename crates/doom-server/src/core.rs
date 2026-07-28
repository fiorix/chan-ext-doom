use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::str::FromStr;

use thiserror::Error;

/// Maximum number of connections in one room.
pub const MAX_PLAYERS: usize = 8;

/// Default number of packets retained for one connection.
pub const DEFAULT_OUTBOX_CAPACITY: usize = 64;

/// Maximum room-name length in bytes.
pub const MAX_ROOM_NAME_LEN: usize = 64;

/// Maximum opaque packet size accepted by the room core.
pub const MAX_PAYLOAD_LEN: usize = 65_527;

/// The exact largest legitimate host-originated batch one atomic
/// reduction can queue for a single recipient, in packet units,
/// composition-proved against the accepted protocol semantics: 64
/// contiguous expired resend-run requests in a 128-slot window, one
/// pump emission, one keepalive, and one head-only reliable-lane
/// emission. The deadlock request and replay cannot raise it, because
/// stamping from the first missing slot suppresses enough expired
/// runs in the same pass, and foreign timeout broadcasts share the
/// same reliable lane. The exact maximum retained outbox count is
/// `(capacity - 1) + MAX_HOST_BATCH` (63 + 67 = 130 with defaults),
/// because a batch-start snapshot at capacity removes on the next
/// send; `capacity + MAX_HOST_BATCH` is a safe loose upper bound.
pub(crate) const MAX_HOST_BATCH: usize = 67;

/// A validated room name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RoomName(String);

impl RoomName {
    /// Returns the room name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoomName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for RoomName {
    type Err = RoomNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl TryFrom<&str> for RoomName {
    type Error = RoomNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(RoomNameError::Empty);
        }
        if value.len() > MAX_ROOM_NAME_LEN {
            return Err(RoomNameError::TooLong {
                len: value.len(),
                max: MAX_ROOM_NAME_LEN,
            });
        }
        if let Some((index, _)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_'))
        {
            return Err(RoomNameError::InvalidCharacter { index });
        }

        Ok(Self(value.to_owned()))
    }
}

/// Why a room name was rejected.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RoomNameError {
    /// The room name was empty.
    #[error("room name must not be empty")]
    Empty,
    /// The room name exceeded the configured limit.
    #[error("room name is {len} bytes; maximum is {max}")]
    TooLong {
        /// Observed byte length.
        len: usize,
        /// Maximum byte length.
        max: usize,
    },
    /// The room name contained a byte outside the portable name alphabet.
    #[error("room name contains an invalid character at byte {index}")]
    InvalidCharacter {
        /// Byte offset of the invalid character.
        index: usize,
    },
}

/// A stable connection identifier allocated by the registry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlayerId(NonZeroU64);

impl PlayerId {
    /// Returns the integer representation.
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// A packet waiting for delivery to one player.
///
/// Metadata is supplied by the transport binding and remains opaque to the room core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundPacket<Metadata> {
    metadata: Metadata,
    payload: Vec<u8>,
}

impl<Metadata> OutboundPacket<Metadata> {
    /// Returns the transport-owned metadata.
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Returns the opaque packet bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// Why a connection could not join a room.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum JoinError {
    /// The room already has [`MAX_PLAYERS`] connections.
    #[error("room is full")]
    RoomFull,
    /// The registry exhausted its stable identifier space.
    #[error("player identifier space is exhausted")]
    PlayerIdsExhausted,
}

/// Why a packet could not be relayed.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RelayError {
    /// The sender is no longer joined.
    #[error("player is not joined")]
    UnknownPlayer,
    /// The opaque payload exceeded the memory-bound limit.
    #[error("payload is {len} bytes; maximum is {max}")]
    PayloadTooLarge {
        /// Observed payload size.
        len: usize,
        /// Maximum payload size.
        max: usize,
    },
}

/// Observable result of handling one valid relay request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayOutcome {
    /// The recipient is absent or belongs to another room.
    Unroutable,
    /// The packet entered the recipient's FIFO outbox.
    Queued(PlayerId),
    /// The recipient's full outbox caused it to be removed from the room.
    SlowConsumerDisconnected(PlayerId),
}

/// Observable result of queueing one host-originated packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostOutcome {
    /// The packet entered the recipient's FIFO outbox.
    Queued(PlayerId),
    /// The recipient's full outbox caused it to be removed from the room.
    SlowConsumerDisconnected(PlayerId),
}

/// Why a host-originated packet could not be queued.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum HostError {
    /// The recipient is no longer joined.
    #[error("player is not joined")]
    UnknownPlayer,
    /// The opaque payload exceeded the memory-bound limit.
    #[error("payload is {len} bytes; maximum is {max}")]
    PayloadTooLarge {
        /// Observed payload size.
        len: usize,
        /// Maximum payload size.
        max: usize,
    },
}

/// A registry of named, protocol-agnostic relay rooms.
#[derive(Debug)]
pub struct Registry<Metadata = ()> {
    rooms: HashMap<RoomName, Room<Metadata>>,
    memberships: HashMap<PlayerId, RoomName>,
    next_player_id: Option<NonZeroU64>,
    outbox_capacity: NonZeroUsize,
    /// Outbox occupancy at the start of the active host-originated
    /// batch, while one runs. Slow-consumer removal for host sends is
    /// measured against this snapshot: a single bounded producer batch
    /// is never counted as consumer backlog.
    host_batch_backlog: Option<Vec<(PlayerId, usize)>>,
}

impl<Metadata> Registry<Metadata> {
    /// Constructs a registry with the given per-player packet limit.
    pub fn new(outbox_capacity: NonZeroUsize) -> Self {
        Self {
            rooms: HashMap::new(),
            memberships: HashMap::new(),
            next_player_id: NonZeroU64::new(1),
            outbox_capacity,
            host_batch_backlog: None,
        }
    }

    /// Marks the start of one atomic host-originated batch: removals
    /// for host sends are measured against outbox occupancy now, so a
    /// valid producer batch cannot trip the slow-consumer bound. The
    /// consumer still proves backlog across the boundary: a member at
    /// capacity at this moment is removed by its next host send, and an
    /// overflow left undrained at batch end is measured by the next
    /// batch's snapshot.
    pub(crate) fn begin_host_batch(&mut self) {
        let backlog = self
            .rooms
            .values()
            .flat_map(|room| {
                room.players
                    .iter()
                    .map(|(player_id, player)| (*player_id, player.outbox.len()))
            })
            .collect();
        self.host_batch_backlog = Some(backlog);
    }

    /// Ends the active host-originated batch.
    pub(crate) fn end_host_batch(&mut self) {
        self.host_batch_backlog = None;
    }

    /// Joins a room, creating it when necessary.
    pub fn join(&mut self, room_name: RoomName) -> Result<PlayerId, JoinError> {
        if self.room_size(&room_name) >= MAX_PLAYERS {
            return Err(JoinError::RoomFull);
        }

        let player_id = self
            .next_player_id
            .map(PlayerId)
            .ok_or(JoinError::PlayerIdsExhausted)?;
        self.next_player_id = player_id.get().checked_add(1).and_then(NonZeroU64::new);

        self.rooms
            .entry(room_name.clone())
            .or_default()
            .players
            .insert(player_id, Player::default());
        self.memberships.insert(player_id, room_name);
        Ok(player_id)
    }

    /// Leaves a room and removes the empty room.
    pub fn leave(&mut self, player_id: PlayerId) -> bool {
        let Some(room_name) = self.memberships.get(&player_id).cloned() else {
            return false;
        };
        self.remove_player(&room_name, player_id);
        true
    }

    /// Returns whether a player is still joined.
    pub fn contains(&self, player_id: PlayerId) -> bool {
        self.memberships.contains_key(&player_id)
    }

    /// Returns the number of joined players in a room.
    pub fn room_size(&self, room_name: &RoomName) -> usize {
        self.rooms
            .get(room_name)
            .map_or(0, |room| room.players.len())
    }

    /// Routes one opaque payload and reports the explicit resulting effect.
    pub fn relay(
        &mut self,
        sender: PlayerId,
        recipient: PlayerId,
        metadata: Metadata,
        payload: &[u8],
    ) -> Result<RelayOutcome, RelayError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(RelayError::PayloadTooLarge {
                len: payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }

        {
            let sender_room = self
                .memberships
                .get(&sender)
                .ok_or(RelayError::UnknownPlayer)?;
            if self.memberships.get(&recipient) != Some(sender_room) {
                return Ok(RelayOutcome::Unroutable);
            }

            let recipient_player = self
                .rooms
                .get_mut(sender_room)
                .expect("a membership always points to an existing room")
                .players
                .get_mut(&recipient)
                .expect("a membership always points to an existing player");
            if recipient_player.outbox.len() < self.outbox_capacity.get() {
                recipient_player.outbox.push_back(OutboundPacket {
                    metadata,
                    payload: payload.to_vec(),
                });
                return Ok(RelayOutcome::Queued(recipient));
            }
        }

        let room_name = self
            .memberships
            .get(&sender)
            .cloned()
            .expect("the sender membership was checked above");
        self.remove_player(&room_name, recipient);
        Ok(RelayOutcome::SlowConsumerDisconnected(recipient))
    }

    /// Queues one host-originated packet for a player, reusing the relay
    /// payload limit, FIFO order, exact capacity, and the policy that
    /// the next packet onto a full outbox removes the recipient. No
    /// sender membership is required: the host is not a room member.
    pub fn queue_host(
        &mut self,
        recipient: PlayerId,
        metadata: Metadata,
        payload: &[u8],
    ) -> Result<HostOutcome, HostError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(HostError::PayloadTooLarge {
                len: payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }

        {
            let room_name = self
                .memberships
                .get(&recipient)
                .ok_or(HostError::UnknownPlayer)?;
            // Inside a host batch the removal decision was made by the
            // batch-start snapshot; the producer's own bounded batch
            // (`MAX_HOST_BATCH`) bounds growth here. Outside a batch,
            // fall back to the per-packet measurement.
            if let Some(backlog) = &self.host_batch_backlog {
                let backlogged = backlog
                    .iter()
                    .find(|(player_id, _)| *player_id == recipient)
                    .is_some_and(|(_, len)| *len >= self.outbox_capacity.get());
                if !backlogged {
                    let recipient_player = self
                        .rooms
                        .get_mut(room_name)
                        .expect("a membership always points to an existing room")
                        .players
                        .get_mut(&recipient)
                        .expect("a membership always points to an existing player");
                    recipient_player.outbox.push_back(OutboundPacket {
                        metadata,
                        payload: payload.to_vec(),
                    });
                    debug_assert!(
                        recipient_player.outbox.len() < self.outbox_capacity.get() + MAX_HOST_BATCH,
                        "the documented memory bound holds for a producer-bounded batch"
                    );
                    return Ok(HostOutcome::Queued(recipient));
                }
            } else {
                let recipient_player = self
                    .rooms
                    .get_mut(room_name)
                    .expect("a membership always points to an existing room")
                    .players
                    .get_mut(&recipient)
                    .expect("a membership always points to an existing player");
                if recipient_player.outbox.len() < self.outbox_capacity.get() {
                    recipient_player.outbox.push_back(OutboundPacket {
                        metadata,
                        payload: payload.to_vec(),
                    });
                    return Ok(HostOutcome::Queued(recipient));
                }
            }
        }

        let room_name = self
            .memberships
            .get(&recipient)
            .cloned()
            .expect("the recipient membership was checked above");
        self.remove_player(&room_name, recipient);
        Ok(HostOutcome::SlowConsumerDisconnected(recipient))
    }

    /// Removes and returns the oldest pending packet for a player.
    pub fn pop_outbound(&mut self, player_id: PlayerId) -> Option<OutboundPacket<Metadata>> {
        let room_name = self.memberships.get(&player_id)?;
        self.rooms
            .get_mut(room_name)?
            .players
            .get_mut(&player_id)?
            .outbox
            .pop_front()
    }

    fn remove_player(&mut self, room_name: &RoomName, player_id: PlayerId) {
        self.memberships.remove(&player_id);
        let remove_room = self.rooms.get_mut(room_name).is_some_and(|room| {
            room.players.remove(&player_id);
            room.players.is_empty()
        });

        if remove_room {
            self.rooms.remove(room_name);
        }
    }
}

impl<Metadata> Default for Registry<Metadata> {
    fn default() -> Self {
        Self::new(
            NonZeroUsize::new(DEFAULT_OUTBOX_CAPACITY).expect("default outbox capacity is nonzero"),
        )
    }
}

#[derive(Debug)]
struct Room<Metadata> {
    players: HashMap<PlayerId, Player<Metadata>>,
}

impl<Metadata> Default for Room<Metadata> {
    fn default() -> Self {
        Self {
            players: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct Player<Metadata> {
    outbox: VecDeque<OutboundPacket<Metadata>>,
}

impl<Metadata> Default for Player<Metadata> {
    fn default() -> Self {
        Self {
            outbox: VecDeque::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> RoomName {
        RoomName::try_from("e1m1").expect("test room name is valid")
    }

    fn registry_with_capacity(capacity: usize) -> Registry<u32> {
        Registry::new(NonZeroUsize::new(capacity).expect("test capacity is nonzero"))
    }

    #[test]
    fn join_and_leave_use_stable_player_ids() {
        let mut registry = Registry::<u32>::default();
        let first = registry.join(room()).expect("first player joins");
        let second = registry.join(room()).expect("second player joins");

        assert_eq!(registry.room_size(&room()), 2);
        assert!(registry.leave(first));
        assert!(!registry.contains(first));
        assert_eq!(registry.room_size(&room()), 1);

        let third = registry.join(room()).expect("replacement player joins");
        assert!(third > second);
        assert_ne!(first, third);
        assert!(!registry.leave(first));
    }

    #[test]
    fn room_capacity_is_eight() {
        let mut registry = Registry::<u32>::default();
        for _ in 0..MAX_PLAYERS {
            registry.join(room()).expect("room has a free slot");
        }

        assert_eq!(registry.join(room()), Err(JoinError::RoomFull));
        assert_eq!(registry.room_size(&room()), MAX_PLAYERS);
        assert!(
            registry
                .join(RoomName::try_from("e1m2").expect("test room name is valid"))
                .is_ok()
        );
    }

    #[test]
    fn relay_preserves_fifo_order_and_transport_metadata() {
        let mut registry = Registry::<u32>::default();
        let sender = registry.join(room()).expect("sender joins");
        let recipient = registry.join(room()).expect("recipient joins");

        for (metadata, payload) in [(11, b"first".as_slice()), (12, b"second".as_slice())] {
            assert_eq!(
                registry
                    .relay(sender, recipient, metadata, payload)
                    .expect("relay succeeds"),
                RelayOutcome::Queued(recipient)
            );
        }

        let first = registry
            .pop_outbound(recipient)
            .expect("first packet is queued");
        let second = registry
            .pop_outbound(recipient)
            .expect("second packet is queued");
        assert_eq!(*first.metadata(), 11);
        assert_eq!(first.payload(), b"first");
        assert_eq!(*second.metadata(), 12);
        assert_eq!(second.payload(), b"second");
        assert!(registry.pop_outbound(recipient).is_none());
    }

    #[test]
    fn relay_does_not_echo_unless_sender_is_addressed() {
        let mut registry = Registry::<u32>::default();
        let sender = registry.join(room()).expect("sender joins");
        let recipient = registry.join(room()).expect("recipient joins");

        registry
            .relay(sender, recipient, 11, b"peer")
            .expect("peer relay succeeds");
        assert!(registry.pop_outbound(sender).is_none());
        assert_eq!(
            registry
                .pop_outbound(recipient)
                .expect("recipient gets peer packet")
                .payload(),
            b"peer"
        );

        registry
            .relay(sender, sender, 11, b"self")
            .expect("self relay succeeds");
        assert_eq!(
            registry
                .pop_outbound(sender)
                .expect("addressed sender gets packet")
                .payload(),
            b"self"
        );
    }

    #[test]
    fn full_outbox_disconnects_slow_consumer_at_the_bound() {
        let mut registry = registry_with_capacity(2);
        let sender = registry.join(room()).expect("sender joins");
        let slow = registry.join(room()).expect("slow consumer joins");

        for payload in [b"one".as_slice(), b"two".as_slice()] {
            assert_eq!(
                registry
                    .relay(sender, slow, 11, payload)
                    .expect("relay fits in the bounded outbox"),
                RelayOutcome::Queued(slow)
            );
        }
        let pending_at_bound = registry
            .rooms
            .get(&room())
            .and_then(|room| room.players.get(&slow))
            .map(|player| player.outbox.len());
        assert_eq!(pending_at_bound, Some(2));

        assert_eq!(
            registry
                .relay(sender, slow, 11, b"overflow")
                .expect("overflow applies the disconnect policy"),
            RelayOutcome::SlowConsumerDisconnected(slow)
        );
        assert!(!registry.contains(slow));
        assert!(registry.pop_outbound(slow).is_none());
        assert_eq!(registry.room_size(&room()), 1);

        let replacement = registry.join(room()).expect("slot is released");
        assert!(replacement > slow);
    }

    #[test]
    fn rooms_are_isolated() {
        let mut registry = Registry::<u32>::default();
        let first = registry.join(room()).expect("first player joins");
        let other_room = RoomName::try_from("e1m2").expect("test room name is valid");
        let second = registry
            .join(other_room)
            .expect("second player joins another room");

        assert_eq!(
            registry
                .relay(first, second, 11, b"nope")
                .expect("unknown destination is not an error"),
            RelayOutcome::Unroutable
        );
        assert!(registry.pop_outbound(second).is_none());
    }

    #[test]
    fn oversized_payload_is_rejected_before_outbox_mutation() {
        let mut registry = Registry::<u32>::default();
        let sender = registry.join(room()).expect("sender joins");
        let recipient = registry.join(room()).expect("recipient joins");
        let oversized = vec![0; MAX_PAYLOAD_LEN + 1];

        assert_eq!(
            registry.relay(sender, recipient, 11, &oversized),
            Err(RelayError::PayloadTooLarge {
                len: MAX_PAYLOAD_LEN + 1,
                max: MAX_PAYLOAD_LEN,
            })
        );
        assert!(registry.pop_outbound(recipient).is_none());
    }

    #[test]
    fn host_queue_preserves_fifo_order_and_metadata_without_a_sender() {
        let mut registry = Registry::<u32>::default();
        let recipient = registry.join(room()).expect("recipient joins");

        for (metadata, payload) in [(1, b"first".as_slice()), (2, b"second".as_slice())] {
            assert_eq!(
                registry
                    .queue_host(recipient, metadata, payload)
                    .expect("host queue succeeds"),
                HostOutcome::Queued(recipient)
            );
        }

        let first = registry
            .pop_outbound(recipient)
            .expect("first packet is queued");
        let second = registry
            .pop_outbound(recipient)
            .expect("second packet is queued");
        assert_eq!(*first.metadata(), 1);
        assert_eq!(first.payload(), b"first");
        assert_eq!(*second.metadata(), 2);
        assert_eq!(second.payload(), b"second");
        assert!(registry.pop_outbound(recipient).is_none());
    }

    #[test]
    fn host_queue_disconnects_slow_consumer_at_the_exact_bound() {
        let mut registry = registry_with_capacity(2);
        let slow = registry.join(room()).expect("slow consumer joins");

        for payload in [b"one".as_slice(), b"two".as_slice()] {
            assert_eq!(
                registry
                    .queue_host(slow, 1, payload)
                    .expect("host queue fits in the bounded outbox"),
                HostOutcome::Queued(slow)
            );
        }
        assert_eq!(
            registry
                .queue_host(slow, 1, b"overflow")
                .expect("overflow applies the disconnect policy"),
            HostOutcome::SlowConsumerDisconnected(slow)
        );
        assert!(!registry.contains(slow));
        assert!(registry.pop_outbound(slow).is_none());
        assert_eq!(registry.room_size(&room()), 0);

        let replacement = registry.join(room()).expect("slot is released");
        assert!(replacement > slow);
    }

    #[test]
    fn host_queue_rejects_unknown_player_and_oversized_before_mutation() {
        let mut registry = Registry::<u32>::default();
        let recipient = registry.join(room()).expect("recipient joins");
        let gone = registry.join(room()).expect("second player joins");
        assert!(registry.leave(gone));

        assert_eq!(
            registry.queue_host(gone, 1, b"nope"),
            Err(HostError::UnknownPlayer)
        );
        let oversized = vec![0; MAX_PAYLOAD_LEN + 1];
        assert_eq!(
            registry.queue_host(recipient, 1, &oversized),
            Err(HostError::PayloadTooLarge {
                len: MAX_PAYLOAD_LEN + 1,
                max: MAX_PAYLOAD_LEN,
            })
        );
        assert!(registry.pop_outbound(recipient).is_none());
        assert!(registry.contains(recipient));
    }

    #[test]
    fn host_batch_queues_past_capacity_and_removes_only_across_the_boundary() {
        let mut registry = registry_with_capacity(64);
        let player = registry.join(room()).expect("player joins");

        // From an empty outbox, one valid batch may exceed the
        // configured capacity up to the producer's own measured bound:
        // the batch is producer-bounded, not consumer backlog.
        registry.begin_host_batch();
        // One beyond the producer invariant still queues: a batch
        // addition is never a slow-consumer result by itself (the
        // RoomHost reduction contract asserts the invariant loudly).
        for _ in 0..=MAX_HOST_BATCH {
            assert_eq!(
                registry
                    .queue_host(player, 1, b"burst")
                    .expect("batch queue succeeds"),
                HostOutcome::Queued(player)
            );
        }
        registry.end_host_batch();

        // Left undrained, the overflow is consumer backlog proven at
        // the next batch start: the next host send removes the member.
        registry.begin_host_batch();
        assert_eq!(
            registry
                .queue_host(player, 1, b"next")
                .expect("removal applies the policy"),
            HostOutcome::SlowConsumerDisconnected(player)
        );
        registry.end_host_batch();
        assert!(!registry.contains(player));
    }

    #[test]
    fn relay_overflow_still_removes_at_exactly_capacity_plus_one() {
        // Peer-to-peer relay never enters a host batch: the exact
        // per-packet 64/65 policy is unchanged by the batch semantics.
        let mut registry = registry_with_capacity(64);
        let sender = registry.join(room()).expect("sender joins");
        let recipient = registry.join(room()).expect("recipient joins");

        for _ in 0..64 {
            assert_eq!(
                registry
                    .relay(sender, recipient, 11, b"relay")
                    .expect("relay succeeds"),
                RelayOutcome::Queued(recipient)
            );
        }
        assert_eq!(
            registry
                .relay(sender, recipient, 11, b"overflow")
                .expect("the 65th applies the removal policy"),
            RelayOutcome::SlowConsumerDisconnected(recipient)
        );
        assert!(!registry.contains(recipient));
    }

    #[test]
    fn host_batch_boundary_is_the_pre_batch_occupancy() {
        let mut registry = registry_with_capacity(2);
        let player = registry.join(room()).expect("player joins");

        // One below capacity at batch start: the whole batch queues.
        registry
            .queue_host(player, 1, b"one")
            .expect("pre-fill queues");
        registry.begin_host_batch();
        for _ in 0..5 {
            assert_eq!(
                registry
                    .queue_host(player, 1, b"burst")
                    .expect("batch queue succeeds"),
                HostOutcome::Queued(player)
            );
        }
        registry.end_host_batch();

        // At capacity at batch start: the next host send removes.
        let drained: Vec<_> = (0..6)
            .map(|_| registry.pop_outbound(player).expect("six packets pending"))
            .collect();
        assert_eq!(drained.len(), 6);
        registry
            .queue_host(player, 1, b"one")
            .expect("pre-fill queues");
        registry
            .queue_host(player, 1, b"two")
            .expect("pre-fill queues");
        registry.begin_host_batch();
        assert_eq!(
            registry
                .queue_host(player, 1, b"overflow")
                .expect("removal applies the policy"),
            HostOutcome::SlowConsumerDisconnected(player)
        );
        registry.end_host_batch();
        assert!(!registry.contains(player));
    }

    #[test]
    fn room_names_use_a_portable_bounded_alphabet() {
        assert!(RoomName::try_from("coop_E1M1-2").is_ok());
        assert_eq!(RoomName::try_from(""), Err(RoomNameError::Empty));
        assert_eq!(
            RoomName::try_from("bad/name"),
            Err(RoomNameError::InvalidCharacter { index: 3 })
        );
        assert!(matches!(
            RoomName::try_from("x".repeat(MAX_ROOM_NAME_LEN + 1).as_str()),
            Err(RoomNameError::TooLong { .. })
        ));
    }
}
