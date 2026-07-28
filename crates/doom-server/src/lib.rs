//! Sans-I/O room and server-role state machines for DOOM multiplayer.

mod core;
pub mod server_role;
pub mod websocket;

pub use core::{
    DEFAULT_OUTBOX_CAPACITY, JoinError, MAX_PAYLOAD_LEN, MAX_PLAYERS, MAX_ROOM_NAME_LEN,
    OutboundPacket, PlayerId, Registry, RelayError, RelayOutcome, RoomName, RoomNameError,
};
