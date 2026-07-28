//! Sans-I/O room and server-role state machines for DOOM multiplayer.

mod core;
pub mod room;
mod runtime;
pub mod server_role;
pub mod websocket;

pub use core::{
    DEFAULT_OUTBOX_CAPACITY, HostError, HostOutcome, JoinError, MAX_PAYLOAD_LEN, MAX_PLAYERS,
    MAX_ROOM_NAME_LEN, OutboundPacket, PlayerId, Registry, RelayError, RelayOutcome, RoomName,
    RoomNameError,
};
