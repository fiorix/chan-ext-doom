use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
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

/// A nonzero address advertised by an engine transport.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RouteId(NonZeroU32);

impl RouteId {
    /// Constructs an address, rejecting zero because it means "no destination".
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the integer representation.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// A transport-independent packet routing request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Envelope<'a> {
    to: Option<RouteId>,
    from: RouteId,
    payload: &'a [u8],
}

impl<'a> Envelope<'a> {
    /// Constructs an envelope. A missing destination registers the sender route.
    pub const fn new(to: Option<RouteId>, from: RouteId, payload: &'a [u8]) -> Self {
        Self { to, from, payload }
    }
}

/// A packet waiting for delivery to one player.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundPacket {
    from: RouteId,
    payload: Vec<u8>,
}

impl OutboundPacket {
    /// Returns the source address advertised to the recipient.
    pub const fn from(&self) -> RouteId {
        self.from
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

/// Why an envelope could not be relayed.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RelayError {
    /// The sender is no longer joined.
    #[error("player is not joined")]
    UnknownPlayer,
    /// The sender tried to change its advertised address.
    #[error("player changed its route address")]
    SourceChanged,
    /// Another connection already owns the advertised address in this room.
    #[error("route address is already in use")]
    SourceInUse,
    /// The opaque payload exceeded the memory-bound limit.
    #[error("payload is {len} bytes; maximum is {max}")]
    PayloadTooLarge {
        /// Observed payload size.
        len: usize,
        /// Maximum payload size.
        max: usize,
    },
}

/// Observable result of handling one valid envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayOutcome {
    /// No joined player owns the destination.
    Unroutable,
    /// The packet entered the recipient's FIFO outbox.
    Queued(PlayerId),
    /// The recipient's full outbox caused it to be removed from the room.
    SlowConsumerDisconnected(PlayerId),
}

/// A registry of named, protocol-agnostic relay rooms.
#[derive(Debug)]
pub struct Registry {
    rooms: HashMap<RoomName, Room>,
    memberships: HashMap<PlayerId, RoomName>,
    next_player_id: Option<NonZeroU64>,
    outbox_capacity: NonZeroUsize,
}

impl Registry {
    /// Constructs a registry with the given per-player packet limit.
    pub fn new(outbox_capacity: NonZeroUsize) -> Self {
        Self {
            rooms: HashMap::new(),
            memberships: HashMap::new(),
            next_player_id: NonZeroU64::new(1),
            outbox_capacity,
        }
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
        envelope: Envelope<'_>,
    ) -> Result<RelayOutcome, RelayError> {
        if envelope.payload.len() > MAX_PAYLOAD_LEN {
            return Err(RelayError::PayloadTooLarge {
                len: envelope.payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }

        let room_name = self
            .memberships
            .get(&sender)
            .cloned()
            .ok_or(RelayError::UnknownPlayer)?;
        let room = self
            .rooms
            .get_mut(&room_name)
            .expect("a membership always points to an existing room");
        let player = room
            .players
            .get_mut(&sender)
            .expect("a membership always points to an existing player");

        match player.route {
            Some(route) if route != envelope.from => return Err(RelayError::SourceChanged),
            Some(_) => {}
            None => {
                if room.routes.contains_key(&envelope.from) {
                    return Err(RelayError::SourceInUse);
                }
                player.route = Some(envelope.from);
                room.routes.insert(envelope.from, sender);
            }
        }

        let Some(destination) = envelope.to else {
            return Ok(RelayOutcome::Unroutable);
        };
        let Some(&recipient) = room.routes.get(&destination) else {
            return Ok(RelayOutcome::Unroutable);
        };
        let recipient_player = room
            .players
            .get_mut(&recipient)
            .expect("a route always points to an existing player");

        if recipient_player.outbox.len() >= self.outbox_capacity.get() {
            self.remove_player(&room_name, recipient);
            return Ok(RelayOutcome::SlowConsumerDisconnected(recipient));
        }

        recipient_player.outbox.push_back(OutboundPacket {
            from: envelope.from,
            payload: envelope.payload.to_vec(),
        });
        Ok(RelayOutcome::Queued(recipient))
    }

    /// Removes and returns the oldest pending packet for a player.
    pub fn pop_outbound(&mut self, player_id: PlayerId) -> Option<OutboundPacket> {
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
        let remove_room = if let Some(room) = self.rooms.get_mut(room_name) {
            if let Some(player) = room.players.remove(&player_id)
                && let Some(route) = player.route
            {
                room.routes.remove(&route);
            }
            room.players.is_empty()
        } else {
            false
        };

        if remove_room {
            self.rooms.remove(room_name);
        }
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(
            NonZeroUsize::new(DEFAULT_OUTBOX_CAPACITY).expect("default outbox capacity is nonzero"),
        )
    }
}

#[derive(Debug, Default)]
struct Room {
    players: HashMap<PlayerId, Player>,
    routes: HashMap<RouteId, PlayerId>,
}

#[derive(Debug, Default)]
struct Player {
    route: Option<RouteId>,
    outbox: VecDeque<OutboundPacket>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> RoomName {
        RoomName::try_from("e1m1").expect("test room name is valid")
    }

    fn route(value: u32) -> RouteId {
        RouteId::new(value).expect("test route is nonzero")
    }

    fn registry_with_capacity(capacity: usize) -> Registry {
        Registry::new(NonZeroUsize::new(capacity).expect("test capacity is nonzero"))
    }

    fn register(registry: &mut Registry, player: PlayerId, address: RouteId) {
        let outcome = registry
            .relay(player, Envelope::new(None, address, &[]))
            .expect("registration succeeds");
        assert_eq!(outcome, RelayOutcome::Unroutable);
    }

    #[test]
    fn join_and_leave_use_stable_player_ids() {
        let mut registry = Registry::default();
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
        let mut registry = Registry::default();
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
    fn relay_preserves_fifo_order() {
        let mut registry = Registry::default();
        let sender = registry.join(room()).expect("sender joins");
        let recipient = registry.join(room()).expect("recipient joins");
        let sender_route = route(11);
        let recipient_route = route(22);
        register(&mut registry, recipient, recipient_route);

        for payload in [b"first".as_slice(), b"second".as_slice()] {
            assert_eq!(
                registry
                    .relay(
                        sender,
                        Envelope::new(Some(recipient_route), sender_route, payload),
                    )
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
        assert_eq!(first.from(), sender_route);
        assert_eq!(first.payload(), b"first");
        assert_eq!(second.payload(), b"second");
        assert!(registry.pop_outbound(recipient).is_none());
    }

    #[test]
    fn relay_does_not_echo_unless_sender_is_addressed() {
        let mut registry = Registry::default();
        let sender = registry.join(room()).expect("sender joins");
        let recipient = registry.join(room()).expect("recipient joins");
        let sender_route = route(11);
        let recipient_route = route(22);
        register(&mut registry, sender, sender_route);
        register(&mut registry, recipient, recipient_route);

        registry
            .relay(
                sender,
                Envelope::new(Some(recipient_route), sender_route, b"peer"),
            )
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
            .relay(
                sender,
                Envelope::new(Some(sender_route), sender_route, b"self"),
            )
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
        let sender_route = route(11);
        let slow_route = route(22);
        register(&mut registry, slow, slow_route);

        for payload in [b"one".as_slice(), b"two".as_slice()] {
            assert_eq!(
                registry
                    .relay(
                        sender,
                        Envelope::new(Some(slow_route), sender_route, payload),
                    )
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
                .relay(
                    sender,
                    Envelope::new(Some(slow_route), sender_route, b"overflow"),
                )
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
    fn route_address_cannot_be_spoofed_or_reused() {
        let mut registry = Registry::default();
        let first = registry.join(room()).expect("first player joins");
        let second = registry.join(room()).expect("second player joins");
        register(&mut registry, first, route(11));

        assert_eq!(
            registry.relay(second, Envelope::new(None, route(11), &[])),
            Err(RelayError::SourceInUse)
        );
        assert_eq!(
            registry.relay(first, Envelope::new(None, route(12), &[])),
            Err(RelayError::SourceChanged)
        );
    }

    #[test]
    fn rooms_are_isolated() {
        let mut registry = Registry::default();
        let first = registry.join(room()).expect("first player joins");
        let other_room = RoomName::try_from("e1m2").expect("test room name is valid");
        let second = registry
            .join(other_room)
            .expect("second player joins another room");
        register(&mut registry, first, route(11));
        register(&mut registry, second, route(22));

        assert_eq!(
            registry
                .relay(first, Envelope::new(Some(route(22)), route(11), b"nope"))
                .expect("unknown destination is not an error"),
            RelayOutcome::Unroutable
        );
        assert!(registry.pop_outbound(second).is_none());
    }

    #[test]
    fn oversized_payload_is_rejected_without_binding_the_source() {
        let mut registry = Registry::default();
        let player = registry.join(room()).expect("player joins");
        let oversized = vec![0; MAX_PAYLOAD_LEN + 1];

        assert_eq!(
            registry.relay(player, Envelope::new(None, route(11), &oversized)),
            Err(RelayError::PayloadTooLarge {
                len: MAX_PAYLOAD_LEN + 1,
                max: MAX_PAYLOAD_LEN,
            })
        );
        assert_eq!(
            registry
                .relay(player, Envelope::new(None, route(12), &[]))
                .expect("rejected payload did not bind the first source"),
            RelayOutcome::Unroutable
        );
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
