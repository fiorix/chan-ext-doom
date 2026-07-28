//! WebSocket binding: named rooms over `/ws/{room}`, sharing one
//! [`crate::runtime::Runtime`] with every other transport. Route 1 is
//! permanently owned by the Rust server in every room; the old in-band
//! reset marker is deleted, not disabled.

use std::io;
use std::num::NonZeroUsize;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::runtime::{Effects, Runtime, SharedState};
use crate::{MAX_PAYLOAD_LEN, PlayerId, RelayError, RoomName};

pub(crate) use crate::runtime::{RouteId, SERVER_ROUTE};

#[cfg(test)]
pub(crate) use crate::runtime::classify_malformed;
#[cfg(test)]
pub(crate) use crate::runtime::timer_loop;
#[cfg(test)]
pub(crate) use crate::server_role::MalformedClass;
#[cfg(test)]
pub(crate) use std::collections::HashMap;
#[cfg(test)]
pub(crate) use tokio::sync::Mutex;
#[cfg(test)]
pub(crate) use tokio::time::Instant;

const INBOUND_HEADER_LEN: usize = 8;
const OUTBOUND_HEADER_LEN: usize = 4;

/// Maximum accepted WebSocket binary-frame size.
pub const MAX_INBOUND_FRAME_LEN: usize = INBOUND_HEADER_LEN + MAX_PAYLOAD_LEN;

type ServerState = SharedState;

/// Serves named rooms at `/ws/{room}` until the listener shuts down.
pub async fn serve(listener: TcpListener, outbox_capacity: NonZeroUsize) -> io::Result<()> {
    crate::runtime::serve(listener, Vec::new(), outbox_capacity).await
}

/// The WebSocket router over the shared runtime state.
pub(crate) fn router(state: SharedState) -> Router {
    Router::new()
        .route("/ws/{room}", get(upgrade))
        .with_state(state)
}

impl Runtime {
    fn bind_source(
        &mut self,
        room_name: &RoomName,
        player_id: PlayerId,
        source: RouteId,
    ) -> Result<(), BindingError> {
        let room = self
            .rooms
            .get_mut(room_name)
            .ok_or(BindingError::UnknownPlayer)?;
        let connection = room
            .connections
            .get(&player_id)
            .ok_or(BindingError::UnknownPlayer)?;
        match connection.route {
            Some(route) if route != source => return Err(BindingError::SourceChanged),
            Some(_) => return Ok(()),
            None => {}
        }
        if room.routes.contains_key(&source) {
            return Err(BindingError::SourceInUse);
        }

        room.connections
            .get_mut(&player_id)
            .expect("the connection was checked above")
            .route = Some(source);
        room.routes.insert(source, player_id);
        Ok(())
    }

    fn recipient(&self, room_name: &RoomName, destination: RouteId) -> Option<PlayerId> {
        self.rooms.get(room_name)?.routes.get(&destination).copied()
    }

    fn handle_envelope(
        &mut self,
        room_name: &RoomName,
        player_id: PlayerId,
        envelope: WireEnvelope<'_>,
    ) -> Result<Effects, BindingError> {
        // Route 1 is permanently server-owned: a client source of 1 is
        // rejected before any route, packet, or third-party effect, and
        // only that connection closes. The exact old reset marker is
        // nothing more than this forbidden source.
        if envelope.from.get() == SERVER_ROUTE {
            return Err(BindingError::ServerRoute);
        }
        self.bind_source(room_name, player_id, envelope.from)?;
        let Some(destination) = envelope.to else {
            // Registration-only traffic never reaches the role.
            return Ok(Effects::default());
        };
        if destination.get() == SERVER_ROUTE {
            let now = self.now();
            let Self {
                rooms,
                udp_notifiers,
                ..
            } = self;
            let Some(room) = rooms.get_mut(room_name) else {
                return Ok(Effects::default());
            };
            let effect = room.server_payload(now, player_id, envelope.payload);
            return Ok(room.apply(effect, udp_notifiers));
        }

        let Some(recipient) = self.recipient(room_name, destination) else {
            return Ok(Effects::default());
        };
        let now = self.now();
        let Self {
            rooms,
            udp_notifiers,
            ..
        } = self;
        let room = rooms
            .get_mut(room_name)
            .ok_or(BindingError::UnknownPlayer)?;
        let (_, effect) =
            room.host
                .relay(now, player_id, recipient, envelope.from, envelope.payload)?;
        Ok(room.apply(effect, udp_notifiers))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BindingError {
    UnknownPlayer,
    ServerRoute,
    SourceChanged,
    SourceInUse,
    Relay(RelayError),
}

impl From<RelayError> for BindingError {
    fn from(error: RelayError) -> Self {
        Self::Relay(error)
    }
}

async fn upgrade(
    websocket: WebSocketUpgrade,
    Path(room_name): Path<String>,
    State(state): State<ServerState>,
) -> Result<Response, axum::http::StatusCode> {
    let room_name =
        RoomName::try_from(room_name.as_str()).map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    Ok(websocket
        .max_message_size(MAX_INBOUND_FRAME_LEN)
        .on_upgrade(move |socket| session(socket, state, room_name)))
}

async fn session(socket: WebSocket, state: ServerState, room_name: RoomName) {
    let waiter = Arc::new(Notify::new());
    let (player_id, join_effects) = {
        let mut runtime = state.0.lock().await;
        match runtime.join(room_name.clone(), Arc::clone(&waiter)) {
            Ok(joined) => joined,
            Err(_) => return,
        }
    };
    for waiter in join_effects.ws_waiters {
        waiter.notify_one();
    }

    let (sink, stream) = socket.split();
    let mut writer = tokio::spawn(write_loop(
        sink,
        state.clone(),
        room_name.clone(),
        player_id,
        Arc::clone(&waiter),
    ));
    let writer_finished;

    tokio::select! {
        () = read_loop(stream, state.clone(), room_name.clone(), player_id, Arc::clone(&waiter)) => {
            writer_finished = false;
        }
        _ = &mut writer => {
            writer_finished = true;
        }
    }

    let leave_effects = {
        let mut runtime = state.0.lock().await;
        runtime.leave(&room_name, player_id)
    };
    for waiter in leave_effects.ws_waiters {
        waiter.notify_one();
    }
    for waiter in leave_effects.udp_waiters {
        waiter.notify_one();
    }

    if !writer_finished {
        let _ = writer.await;
    }
}

async fn read_loop(
    mut stream: SplitStream<WebSocket>,
    state: ServerState,
    room_name: RoomName,
    player_id: PlayerId,
    waiter: Arc<Notify>,
) {
    while let Some(message) = stream.next().await {
        let Ok(message) = message else {
            return;
        };

        match message {
            Message::Binary(frame) => {
                let Ok(envelope) = decode_inbound(&frame) else {
                    return;
                };
                let effects = {
                    let mut runtime = state.0.lock().await;
                    match runtime.handle_envelope(&room_name, player_id, envelope) {
                        Ok(effects) => effects,
                        // A binding rejection closes only this connection.
                        Err(_) => return,
                    }
                };
                for waiter in effects.ws_waiters {
                    waiter.notify_one();
                }
                for waiter in effects.udp_waiters {
                    waiter.notify_one();
                }
            }
            Message::Ping(_) => waiter.notify_one(),
            Message::Pong(_) => {}
            Message::Close(_) | Message::Text(_) => return,
        }
    }
}

async fn write_loop(
    mut sink: SplitSink<WebSocket, Message>,
    state: ServerState,
    room_name: RoomName,
    player_id: PlayerId,
    waiter: Arc<Notify>,
) {
    loop {
        let (packet, connected) = {
            let mut runtime = state.0.lock().await;
            let Some(room) = runtime.rooms.get_mut(&room_name) else {
                return;
            };
            (
                room.host.pop_outbound(player_id),
                room.host.contains(player_id),
            )
        };

        if let Some(packet) = packet {
            if sink
                .send(Message::Binary(encode_outbound(packet).into()))
                .await
                .is_err()
            {
                return;
            }
            continue;
        }
        if !connected {
            // Every already-owed frame is delivered FIFO before the
            // close even though the peer is already gone: the captured
            // prefix with its own source routes, then the terminal.
            let pending = {
                let mut runtime = state.0.lock().await;
                runtime
                    .rooms
                    .get_mut(&room_name)
                    .and_then(|room| room.connections.get_mut(&player_id))
                    .map(|connection| std::mem::take(&mut connection.pending))
            };
            if let Some(mut pending) = pending {
                while let Some(frame) = pending.pop_front() {
                    let _ = sink.send(Message::Binary(frame.into())).await;
                }
            }
            let _ = sink.send(Message::Close(None)).await;
            return;
        }
        if sink.flush().await.is_err() {
            return;
        }
        waiter.notified().await;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WireEnvelope<'a> {
    to: Option<RouteId>,
    from: RouteId,
    payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecodeError {
    TooShort,
    TooLarge,
    ZeroSource,
}

fn decode_inbound(frame: &[u8]) -> Result<WireEnvelope<'_>, DecodeError> {
    if frame.len() > MAX_INBOUND_FRAME_LEN {
        return Err(DecodeError::TooLarge);
    }
    let header = frame
        .get(..INBOUND_HEADER_LEN)
        .ok_or(DecodeError::TooShort)?;
    let to = u32::from_le_bytes(
        header[..4]
            .try_into()
            .expect("the destination slice is four bytes"),
    );
    let from = u32::from_le_bytes(
        header[4..]
            .try_into()
            .expect("the source slice is four bytes"),
    );
    let from = RouteId::new(from).ok_or(DecodeError::ZeroSource)?;

    Ok(WireEnvelope {
        to: RouteId::new(to),
        from,
        payload: &frame[INBOUND_HEADER_LEN..],
    })
}

fn encode_outbound(packet: crate::room::OwedPacket<RouteId>) -> Vec<u8> {
    let mut frame = Vec::with_capacity(OUTBOUND_HEADER_LEN + packet.payload().len());
    frame.extend_from_slice(&packet.metadata().get().to_le_bytes());
    frame.extend_from_slice(packet.payload());
    frame
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use doom_proto::{
        ClientPacket, ConnectData, GameSettings, ServerPacket, Syn, TiccmdDiff, WireHeader,
    };
    use tokio::time::timeout;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    // --- reducer-level tests ---------------------------------------------

    fn test_runtime() -> Runtime {
        test_runtime_with_capacity(4)
    }

    fn test_runtime_with_capacity(capacity: usize) -> Runtime {
        Runtime {
            rooms: HashMap::new(),
            outbox_capacity: NonZeroUsize::new(capacity).expect("test capacity is nonzero"),
            started: Instant::now(),
            next_listener: 0,
            udp_notifiers: HashMap::new(),
        }
    }

    fn room() -> RoomName {
        RoomName::try_from("e1m1").expect("test room name is valid")
    }

    fn route(value: u32) -> RouteId {
        RouteId::new(value).expect("test route is nonzero")
    }

    fn join(runtime: &mut Runtime) -> PlayerId {
        runtime
            .join(room(), Arc::new(Notify::new()))
            .expect("join succeeds")
            .0
    }

    #[test]
    fn source_binding_rejects_changes_and_duplicate_ownership() {
        let mut runtime = test_runtime();
        let first = join(&mut runtime);
        let second = join(&mut runtime);

        assert_eq!(runtime.bind_source(&room(), first, route(11)), Ok(()));
        assert_eq!(
            runtime.bind_source(&room(), first, route(12)),
            Err(BindingError::SourceChanged)
        );
        assert_eq!(
            runtime.bind_source(&room(), second, route(11)),
            Err(BindingError::SourceInUse)
        );
    }

    #[test]
    fn server_route_source_is_rejected_before_any_effect() {
        let mut runtime = test_runtime();
        let offender_waiter = Arc::new(Notify::new());
        let offender = runtime
            .join(room(), Arc::clone(&offender_waiter))
            .expect("offender joins")
            .0;
        let bystander_waiter = Arc::new(Notify::new());
        let bystander = runtime
            .join(room(), Arc::clone(&bystander_waiter))
            .expect("bystander joins")
            .0;

        // The exact old reset marker, and a payload-carrying,
        // destination-carrying source of 1: the same forbidden source,
        // rejected before any route, packet, or third-party effect.
        for frame in [
            client_frame(0, SERVER_ROUTE, &[]),
            client_frame(7, SERVER_ROUTE, b"payload"),
        ] {
            let envelope = decode_inbound(&frame).expect("frame decodes");
            assert!(matches!(
                runtime.handle_envelope(&room(), offender, envelope),
                Err(BindingError::ServerRoute)
            ));
        }
        {
            let room = runtime.rooms.get(&room()).expect("room exists");
            assert!(room.routes.is_empty(), "no route was bound");
            assert!(room.connections[&offender].route.is_none());
            // Admission already happened: the member stays until normal
            // cleanup, with no effect on the bystander.
            assert!(room.host.contains(offender));
            assert!(room.host.contains(bystander));
        }

        // Normal cleanup removes the admitted offender with its one
        // external Leave; a pre-SYN member's removal is quiet for
        // everyone else (no abort, broadcast, or game-end effect).
        let effects = runtime.leave(&room(), offender);
        assert!(
            !effects.ws_waiters.is_empty(),
            "the offender's writer is woken"
        );
        assert!(
            effects
                .ws_waiters
                .iter()
                .all(|waiter| Arc::ptr_eq(waiter, &offender_waiter)),
            "the cleanup is quiet for the bystander"
        );
        let room = runtime.rooms.get(&room()).expect("room exists");
        assert!(!room.host.contains(offender));
        assert!(room.host.contains(bystander));
    }

    /// The pinned pre-3.0 magic as a literal: the production constant
    /// must classify exactly this value, not whatever it happens to hold.
    const PINNED_OLD_MAGIC: u32 = 0xccd9_74d4;

    #[test]
    fn malformed_classification_is_narrow() {
        // The pinned pre-3.0 magic, reliable and plain framings
        // (protocol header words are big-endian on the wire).
        let mut old_reliable = vec![0x80, 0x00, 0x00];
        old_reliable.extend_from_slice(&PINNED_OLD_MAGIC.to_be_bytes());
        assert_eq!(
            classify_malformed(&old_reliable),
            MalformedClass::Syn { old_magic: true }
        );
        let mut old_plain = vec![0x00, 0x00];
        old_plain.extend_from_slice(&PINNED_OLD_MAGIC.to_be_bytes());
        assert_eq!(
            classify_malformed(&old_plain),
            MalformedClass::Syn { old_magic: true }
        );
        // SYN-shaped with any other magic.
        let mut other_syn = vec![0x80, 0x00, 0x00, 1, 2, 3, 4];
        assert_eq!(
            classify_malformed(&other_syn),
            MalformedClass::Syn { old_magic: false }
        );
        other_syn.push(0xaa);
        // Truncated SYN shape: no magic to read.
        assert_eq!(
            classify_malformed(&[0x80, 0x00]),
            MalformedClass::Syn { old_magic: false }
        );
        // Non-SYN types and truncation are established-peer noise.
        assert_eq!(
            classify_malformed(&[0x00, 0x06, 1]),
            MalformedClass::Established
        );
        assert_eq!(
            classify_malformed(&[0x80, 0x63, 0]),
            MalformedClass::Established
        );
        assert_eq!(classify_malformed(&[0x00]), MalformedClass::Established);
        assert_eq!(classify_malformed(&[]), MalformedClass::Established);
    }

    #[test]
    fn inbound_envelope_uses_little_endian_addresses() {
        let frame = [2, 0, 0, 0, 1, 0, 0, 0, 0xaa, 0xbb];
        let envelope = decode_inbound(&frame).expect("valid frame decodes");

        assert_eq!(
            envelope,
            WireEnvelope {
                to: RouteId::new(2),
                from: route(1),
                payload: &[0xaa, 0xbb],
            }
        );
    }

    #[test]
    fn registration_envelope_has_no_destination() {
        let frame = [0, 0, 0, 0, 7, 0, 0, 0];
        let envelope = decode_inbound(&frame).expect("registration frame decodes");

        assert_eq!(
            envelope,
            WireEnvelope {
                to: None,
                from: route(7),
                payload: &[],
            }
        );
    }

    #[test]
    fn malformed_envelopes_are_rejected_before_payload_copy() {
        assert_eq!(
            decode_inbound(&[0; INBOUND_HEADER_LEN - 1]),
            Err(DecodeError::TooShort)
        );

        let mut zero_source = [0; INBOUND_HEADER_LEN];
        zero_source[0] = 1;
        assert_eq!(decode_inbound(&zero_source), Err(DecodeError::ZeroSource));

        let oversized = vec![0; MAX_INBOUND_FRAME_LEN + 1];
        assert_eq!(decode_inbound(&oversized), Err(DecodeError::TooLarge));
    }

    // --- loopback helpers --------------------------------------------------

    fn client_frame(to: u32, from: u32, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(INBOUND_HEADER_LEN + payload.len());
        frame.extend_from_slice(&to.to_le_bytes());
        frame.extend_from_slice(&from.to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn server_frame(from: u32, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(OUTBOUND_HEADER_LEN + payload.len());
        frame.extend_from_slice(&from.to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn fixture_bytes(session: &str, file: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(session)
            .join(file);
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    fn plain() -> WireHeader {
        WireHeader { reliable_seq: None }
    }

    fn reliable(seq: u8) -> WireHeader {
        WireHeader {
            reliable_seq: Some(seq),
        }
    }

    fn syn_packet(name: &str, mission: u8, mode: u8, lowres_turn: u8) -> Vec<u8> {
        ClientPacket::Syn(Syn {
            version: b"Chocolate Doom 3.1.1".to_vec(),
            protocols: vec![b"CHOCOLATE_DOOM_0".to_vec()],
            connect: ConnectData {
                gamemode: mode,
                gamemission: mission,
                lowres_turn,
                drone: 0,
                max_players: 4,
                is_freedoom: 0,
                wad_sha1: [7; 20],
                deh_sha1: [8; 20],
                player_class: 0,
            },
            player_name: name.as_bytes().to_vec(),
        })
        .encode(plain(), false)
        .expect("syn encodes")
    }

    fn settings_packet(seq: u8, deathmatch: u8) -> Vec<u8> {
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
        .encode(reliable(seq), false)
        .expect("gamestart encodes")
    }

    fn launch_packet(seq: u8) -> Vec<u8> {
        ClientPacket::Launch
            .encode(reliable(seq), false)
            .expect("launch encodes")
    }

    fn ack_packet(next_seq: u8) -> Vec<u8> {
        ClientPacket::ReliableAck { next_seq }
            .encode(plain(), false)
            .expect("ack encodes")
    }

    async fn spawn_server() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener binds");
        let address = listener.local_addr().expect("listener has an address");
        let server = tokio::spawn(async move {
            serve(
                listener,
                NonZeroUsize::new(64).expect("test capacity is nonzero"),
            )
            .await
        });
        (address, server)
    }

    async fn next_message<S>(
        socket: &mut S,
    ) -> Option<Result<ClientMessage, tokio_tungstenite::tungstenite::Error>>
    where
        S: StreamExt<Item = Result<ClientMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("server responds within five seconds")
    }

    async fn next_binary<S>(socket: &mut S) -> Vec<u8>
    where
        S: StreamExt<Item = Result<ClientMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        match next_message(socket).await {
            Some(Ok(ClientMessage::Binary(frame))) => frame.to_vec(),
            other => panic!("expected binary frame, got {other:?}"),
        }
    }

    /// Read one binary frame and decode it as a server packet with its
    /// route: the four-byte little-endian source, then the packet.
    async fn next_decoded<S>(socket: &mut S, lowres: bool) -> (u32, ServerPacket)
    where
        S: StreamExt<Item = Result<ClientMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        let frame = next_binary(socket).await;
        let from = u32::from_le_bytes(frame[..4].try_into().expect("route header"));
        let (_, packet) = ServerPacket::decode(&frame[OUTBOUND_HEADER_LEN..], lowres)
            .expect("server packet decodes");
        (from, packet)
    }

    async fn send<S>(socket: &mut S, to: u32, from: u32, payload: &[u8])
    where
        S: SinkExt<ClientMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    {
        socket
            .send(ClientMessage::Binary(
                client_frame(to, from, payload).into(),
            ))
            .await
            .expect("frame sends");
    }

    // --- loopback tests -------------------------------------------------

    #[tokio::test]
    async fn websocket_binding_relays_between_named_room_members() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/coop");
        let (mut first, _) = connect_async(&room_url)
            .await
            .expect("first client connects");
        let (mut second, _) = connect_async(&room_url)
            .await
            .expect("second client connects");

        send(&mut first, 0, 11, &[]).await;
        send(&mut second, 11, 22, b"ready").await;
        assert_eq!(next_binary(&mut first).await, server_frame(22, b"ready"));

        send(&mut first, 22, 11, b"packet").await;
        assert_eq!(next_binary(&mut second).await, server_frame(11, b"packet"));

        first.close(None).await.expect("first client closes");
        second.close(None).await.expect("second client closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn server_route_and_old_reset_marker_close_only_the_sender() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/reset");
        let (mut existing, _) = connect_async(&room_url)
            .await
            .expect("existing client connects");

        send(&mut existing, 22, 22, b"ready").await;
        assert_eq!(next_binary(&mut existing).await, server_frame(22, b"ready"));

        // The exact old reset marker is now merely a forbidden source.
        let (mut resetter, _) = connect_async(&room_url).await.expect("resetter connects");
        send(&mut resetter, 0, SERVER_ROUTE, &[]).await;
        assert!(matches!(
            next_message(&mut resetter).await,
            Some(Ok(ClientMessage::Close(_))) | None
        ));

        // Nobody else was disconnected or unbound.
        send(&mut existing, 22, 22, b"alive").await;
        assert_eq!(next_binary(&mut existing).await, server_frame(22, b"alive"));

        existing.close(None).await.expect("existing closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn rooms_have_isolated_roles_and_reconnect_starts_fresh() {
        let (address, server) = spawn_server().await;
        let alpha_url = format!("ws://{address}/ws/alpha");
        let beta_url = format!("ws://{address}/ws/beta");
        let (mut alpha, _) = connect_async(&alpha_url).await.expect("alpha connects");
        let (mut beta, _) = connect_async(&beta_url).await.expect("beta connects");

        send(&mut alpha, 1, 20, &syn_packet("Alice", 0, 0, 0)).await;
        send(&mut beta, 1, 20, &syn_packet("Bravo", 0, 0, 0)).await;
        for socket in [&mut alpha, &mut beta] {
            let (from, _) = next_decoded(socket, false).await;
            assert_eq!(from, SERVER_ROUTE);
            match next_decoded(socket, false).await {
                (from, ServerPacket::WaitingData(data)) => {
                    assert_eq!(from, SERVER_ROUTE);
                    assert_eq!(data.players.len(), 1, "roles are isolated per room");
                }
                other => panic!("expected waiting data, got {other:?}"),
            }
        }

        // The room empties and is dropped; a reconnect under a different
        // mission/mode pair is adopted, which a stale role would reject.
        alpha.close(None).await.expect("alpha closes");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (mut reconnected, _) = connect_async(&alpha_url).await.expect("reconnect connects");
        send(&mut reconnected, 1, 20, &syn_packet("Carol", 1, 2, 0)).await;
        let (from, accept) = next_decoded(&mut reconnected, false).await;
        assert_eq!(from, SERVER_ROUTE);
        assert!(
            matches!(accept, ServerPacket::SynAccept(_)),
            "a fresh role accepts the new mission, got {accept:?}"
        );

        reconnected.close(None).await.expect("reconnect closes");
        beta.close(None).await.expect("beta closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn syn_fixture_produces_accept_and_waiting_data_from_the_server_route() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/fixture");
        let (mut client, _) = connect_async(&room_url).await.expect("client connects");

        let fixture = fixture_bytes("handshake-keepalive", "000-c2s-client1-syn.bin");
        send(&mut client, 1, 20, &fixture).await;

        let (from, accept) = next_decoded(&mut client, false).await;
        assert_eq!(from, SERVER_ROUTE);
        match accept {
            ServerPacket::SynAccept(accept) => {
                assert_eq!(accept.protocol, b"CHOCOLATE_DOOM_0");
            }
            other => panic!("expected syn accept, got {other:?}"),
        }
        let (from, data) = next_decoded(&mut client, false).await;
        assert_eq!(from, SERVER_ROUTE);
        match data {
            ServerPacket::WaitingData(data) => {
                assert_eq!(data.players.len(), 1);
                assert_eq!(data.is_controller, 1);
                assert_eq!(data.consoleplayer, 0);
            }
            other => panic!("expected waiting data, got {other:?}"),
        }

        client.close(None).await.expect("client closes");
        server.abort();
        let _ = server.await;
    }

    /// Drive two loopback clients into the game with deathmatch 1.
    async fn gamestart_pair(
        address: std::net::SocketAddr,
        lowres: u8,
    ) -> (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) {
        let room_url = format!("ws://{address}/ws/game");
        let (mut alice, _) = connect_async(&room_url).await.expect("alice connects");
        let (mut bob, _) = connect_async(&room_url).await.expect("bob connects");
        send(&mut alice, 1, 20, &syn_packet("Alice", 0, 0, lowres)).await;
        send(&mut bob, 1, 21, &syn_packet("Bob", 0, 0, lowres)).await;
        send(&mut alice, 1, 20, &ack_packet(1)).await;
        send(&mut bob, 1, 21, &ack_packet(1)).await;
        send(&mut alice, 1, 20, &launch_packet(0)).await;
        send(&mut alice, 1, 20, &ack_packet(2)).await;
        send(&mut bob, 1, 21, &ack_packet(2)).await;
        send(&mut alice, 1, 20, &settings_packet(1, 1)).await;
        send(&mut bob, 1, 21, &settings_packet(0, 0)).await;
        (alice, bob)
    }

    #[tokio::test]
    async fn two_clients_reach_gamestart_with_per_recipient_consoleplayer() {
        let (address, server) = spawn_server().await;
        let (mut alice, mut bob) = gamestart_pair(address, 0).await;

        let mut starts = [None, None];
        for _ in 0..8 {
            for (index, socket) in [(0, &mut alice), (1, &mut bob)] {
                if starts[index].is_none()
                    && let (_, ServerPacket::GameStart(settings)) =
                        next_decoded(socket, false).await
                {
                    starts[index] = Some(settings);
                }
            }
            if starts.iter().all(Option::is_some) {
                break;
            }
        }
        let alice_start = starts[0].take().expect("alice got gamestart");
        let bob_start = starts[1].take().expect("bob got gamestart");
        assert_eq!(alice_start.consoleplayer, 0);
        assert_eq!(bob_start.consoleplayer, 1);
        assert_eq!(alice_start.deathmatch, 1);
        assert_eq!(bob_start.deathmatch, 1);

        alice.close(None).await.expect("alice closes");
        bob.close(None).await.expect("bob closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn lowres_room_round_trips_narrow_gamedata() {
        let (address, server) = spawn_server().await;
        let (mut alice, mut bob) = gamestart_pair(address, 1).await;

        // Wait for the authoritative GAMESTART on both connections:
        // only then is the room in game and narrow, and the upload
        // below is guaranteed to arrive after it.
        let mut started = [false, false];
        while !started.iter().all(|done| *done) {
            for (index, socket) in [(0, &mut alice), (1, &mut bob)] {
                if !started[index]
                    && let (_, ServerPacket::GameStart(_)) = next_decoded(socket, true).await
                {
                    started[index] = true;
                }
            }
        }

        // A narrow nonzero angleturn: one byte on the wire at the
        // negotiated width.
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
        .encode(plain(), true)
        .expect("narrow gamedata encodes");
        send(&mut alice, 1, 20, &upload).await;

        // The fan-out arrives through the timer pump.
        let frame = timeout(Duration::from_secs(2), async {
            loop {
                let frame = next_binary(&mut bob).await;
                if let Ok((_, ServerPacket::GameData(_))) =
                    ServerPacket::decode(&frame[OUTBOUND_HEADER_LEN..], true)
                {
                    break frame;
                }
            }
        })
        .await
        .expect("fan-out within two seconds");
        let (_, data) =
            ServerPacket::decode(&frame[OUTBOUND_HEADER_LEN..], true).expect("narrow decodes");
        let ServerPacket::GameData(data) = data else {
            unreachable!("checked above");
        };
        assert_eq!(data.tics[0].players[0].1.turn, Some(0x100));
        assert!(
            ServerPacket::decode(&frame[OUTBOUND_HEADER_LEN..], false).is_err(),
            "the fan-out cannot be read at the wrong width"
        );

        alice.close(None).await.expect("alice closes");
        bob.close(None).await.expect("bob closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn timer_drives_waiting_data_cadence_and_reliable_retry() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/timer");
        let (mut client, _) = connect_async(&room_url).await.expect("client connects");
        send(&mut client, 1, 20, &syn_packet("Alice", 0, 0, 0)).await;

        // No acknowledgement and no further input: the runtime timer
        // must drive the cadence and the reliable retry.
        let mut accepts = 0;
        let mut waiting = 0;
        let deadline = Instant::now() + Duration::from_millis(1_600);
        while Instant::now() < deadline && (accepts < 2 || waiting < 2) {
            let frame = timeout(Duration::from_millis(1_600), next_binary(&mut client))
                .await
                .expect("timer traffic arrives");
            match ServerPacket::decode(&frame[OUTBOUND_HEADER_LEN..], false)
                .expect("decodes")
                .1
            {
                ServerPacket::SynAccept(_) => accepts += 1,
                ServerPacket::WaitingData(_) => waiting += 1,
                ServerPacket::Keepalive => {}
                other => panic!("unexpected packet {other:?}"),
            }
        }
        assert_eq!(accepts, 2, "the unacknowledged head retried");
        assert_eq!(waiting, 2, "the cadence produced a second waiting data");

        client.close(None).await.expect("client closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test(start_paused = true)]
    async fn missed_timer_ticks_are_skipped_not_burst() {
        // A single-player room in game: every timer pass pumps one
        // fan-out, so fan-out count measures reducer passes. The local
        // capacity fits the drive-to-gamestart flow, whose reliable
        // chain peaks at six queued packets before the drain; the
        // shared default of four would evict the peer mid-setup.
        let mut runtime = test_runtime_with_capacity(8);
        let player = join(&mut runtime);
        for payload in [
            syn_packet("Alice", 0, 0, 0),
            launch_packet(0),
            ack_packet(1),
            ack_packet(2),
            settings_packet(1, 0),
        ] {
            let frame = client_frame(1, 20, &payload);
            let envelope = decode_inbound(&frame).expect("frame decodes");
            runtime
                .handle_envelope(&room(), player, envelope)
                .expect("envelope accepted");
        }
        {
            let room = runtime.rooms.get_mut(&room()).expect("room");
            assert!(room.host.contains(player), "the room is in game");
            while room.host.pop_outbound(player).is_some() {}
        }

        let state = SharedState(Arc::new(Mutex::new(runtime)));
        let timer = tokio::spawn(timer_loop(state.clone()));
        // The interval exists and has ticked once; drain that pass.
        tokio::task::yield_now().await;
        {
            let mut runtime = state.0.lock().await;
            let room = runtime.rooms.get_mut(&room()).expect("room");
            while room.host.pop_outbound(player).is_some() {}
        }
        // Ten seconds pass with the executor unable to service the
        // timer: two hundred missed 50 ms ticks.
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 0..15 {
            tokio::task::yield_now().await;
        }
        timer.abort();

        let mut runtime = state.0.lock().await;
        let room = runtime.rooms.get_mut(&room()).expect("room");
        let mut gamedata = 0;
        let mut resends = 0;
        let mut keepalives = 0;
        let mut retries = 0;
        while let Some(packet) = room.host.pop_outbound(player) {
            match ServerPacket::decode(packet.payload(), false)
                .expect("decodes")
                .1
            {
                ServerPacket::GameData(_) => gamedata += 1,
                ServerPacket::GameDataResend { .. } => resends += 1,
                ServerPacket::Keepalive => keepalives += 1,
                ServerPacket::GameStart(_) => retries += 1,
                other => panic!("unexpected packet {other:?}"),
            }
        }
        // Skip: exactly one collapsed pass — one pump emission plus one
        // deadlock replay. The deadlock request, the keepalive, and the
        // reliable retry exist only because the role clock saw the real
        // monotonic 10 s jump: a fake collapsed clock would produce
        // none of them.
        assert_eq!(gamedata, 2, "one collapsed pass, not a burst");
        assert_eq!(resends, 1, "the deadlock threshold saw the jump");
        assert_eq!(keepalives, 1, "the send-idle threshold saw the jump");
        assert_eq!(retries, 1, "the reliable retry saw the jump");
    }

    #[tokio::test]
    async fn cancelling_serve_stops_the_runtime_timer() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/cancel");
        let (mut client, _) = connect_async(&room_url).await.expect("client connects");
        send(&mut client, 1, 20, &syn_packet("Alice", 0, 0, 0)).await;
        let (_, accept) = next_decoded(&mut client, false).await;
        assert!(matches!(accept, ServerPacket::SynAccept(_)));
        let (_, data) = next_decoded(&mut client, false).await;
        assert!(matches!(data, ServerPacket::WaitingData(_)));

        // The reliable head is never acknowledged and the initial
        // traffic is drained. With the timer coupled to the serve
        // future, cancelling the server silences the retry and cadence
        // that a detached timer would keep driving.
        server.abort();
        let retry = timeout(Duration::from_millis(1_500), next_binary(&mut client)).await;
        assert!(
            retry.is_err(),
            "no timer-driven retry after cancellation: {retry:?}"
        );

        client.close(None).await.expect("client closes");
    }

    #[tokio::test]
    async fn transport_close_aborts_startup_for_the_survivor() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/abort");
        let (mut alice, _) = connect_async(&room_url).await.expect("alice connects");
        let (mut bob, _) = connect_async(&room_url).await.expect("bob connects");
        send(&mut alice, 1, 20, &syn_packet("Alice", 0, 0, 0)).await;
        send(&mut bob, 1, 21, &syn_packet("Bob", 0, 0, 0)).await;
        send(&mut alice, 1, 20, &ack_packet(1)).await;
        send(&mut bob, 1, 21, &ack_packet(1)).await;
        send(&mut alice, 1, 20, &launch_packet(0)).await;
        send(&mut alice, 1, 20, &ack_packet(2)).await;
        send(&mut bob, 1, 21, &ack_packet(2)).await;

        // Order the leave after bob's chain is drained: receiving the
        // LAUNCH proves his first ack landed, and the settle time lets
        // the second be processed; an undrained chain would suppress
        // the reliable abort message once bob is disconnecting.
        loop {
            if let (_, ServerPacket::Launch { .. }) = next_decoded(&mut bob, false).await {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        alice.close(None).await.expect("alice closes");

        // The abort console message reaches bob through the outbox.
        let aborted = timeout(Duration::from_secs(2), async {
            loop {
                match next_decoded(&mut bob, false).await {
                    (_, ServerPacket::ConsoleMessage { message })
                        if message.starts_with(b"Game startup aborted") =>
                    {
                        break true;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("abort broadcast within two seconds");
        assert!(aborted);

        bob.close(None).await.expect("bob closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn old_magic_terminal_then_close_and_established_reject_retains() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/old");

        // Pre-SYN old magic: the exact terminal REJECTED is the last
        // binary frame, then the connection closes.
        let (mut old, _) = connect_async(&room_url).await.expect("old client connects");
        let mut old_syn = vec![0x00, 0x00];
        old_syn.extend_from_slice(&PINNED_OLD_MAGIC.to_be_bytes());
        send(&mut old, 1, 20, &old_syn).await;
        let (from, rejected) = next_decoded(&mut old, false).await;
        assert_eq!(from, SERVER_ROUTE);
        match rejected {
            ServerPacket::Rejected { reason } => assert!(
                reason.starts_with(b"You are using an old client version that is not supported by this server. This server is running ")
            ),
            other => panic!("expected rejected, got {other:?}"),
        }
        assert!(matches!(
            next_message(&mut old).await,
            Some(Ok(ClientMessage::Close(_))) | None
        ));

        // Established old magic: a plain REJECTED, and the connection
        // is retained (the lobby cadence still reaches it).
        let (mut established, _) = connect_async(&room_url)
            .await
            .expect("established client connects");
        send(&mut established, 1, 21, &syn_packet("Alice", 0, 0, 0)).await;
        send(&mut established, 1, 21, &old_syn).await;
        let rejected = timeout(Duration::from_secs(2), async {
            loop {
                if let (_, ServerPacket::Rejected { .. }) =
                    next_decoded(&mut established, false).await
                {
                    break true;
                }
            }
        })
        .await;
        assert_eq!(rejected, Ok(true));
        let retained = timeout(Duration::from_secs(2), async {
            loop {
                if let (_, ServerPacket::WaitingData(_)) =
                    next_decoded(&mut established, false).await
                {
                    break true;
                }
            }
        })
        .await;
        assert_eq!(
            retained,
            Ok(true),
            "the retained peer still gets lobby data"
        );

        established.close(None).await.expect("established closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn malformed_wrong_direction_and_unknown_destination_are_dropped_alive() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/noise");
        let (mut client, _) = connect_async(&room_url).await.expect("client connects");
        send(&mut client, 1, 20, &syn_packet("Alice", 0, 0, 0)).await;

        // Undecodable garbage to the server route: dropped, not rejected.
        send(&mut client, 1, 20, &[0x63, 0x80, 0, 1, 2, 3]).await;
        // A payload shorter than the two-byte type word still reaches
        // the role as Malformed (never a binding error or rejection).
        send(&mut client, 1, 20, &[0x06]).await;
        // A server-family packet (wrong direction): unsupported c2s.
        let syn_accept = ServerPacket::SynAccept(doom_proto::SynAccept {
            version: b"x".to_vec(),
            protocol: b"y".to_vec(),
        })
        .encode(reliable(0), false)
        .expect("encodes");
        send(&mut client, 1, 20, &syn_accept).await;
        // An unknown destination: unroutable, no effect.
        send(&mut client, 77, 20, b"void").await;
        // A registration envelope: never reaches the role.
        send(&mut client, 0, 20, &[]).await;

        // The connection is alive: the lobby cadence still reaches it.
        let alive = timeout(Duration::from_secs(2), async {
            loop {
                if let (_, ServerPacket::WaitingData(_)) = next_decoded(&mut client, false).await {
                    break true;
                }
            }
        })
        .await;
        assert_eq!(alive, Ok(true));

        client.close(None).await.expect("client closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn oversized_frame_closes_only_the_offender() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/size");
        let (mut big, _) = connect_async(&room_url).await.expect("big client connects");
        let (mut other, _) = connect_async(&room_url)
            .await
            .expect("other client connects");

        send(&mut other, 33, 33, b"ready").await;
        assert_eq!(next_binary(&mut other).await, server_frame(33, b"ready"));

        let oversized = vec![0; MAX_INBOUND_FRAME_LEN + 1];
        big.send(ClientMessage::Binary(oversized.into()))
            .await
            .expect("oversized frame sends");
        assert!(matches!(
            next_message(&mut big).await,
            Some(Ok(ClientMessage::Close(_))) | Some(Err(_)) | None
        ));

        send(&mut other, 33, 33, b"alive").await;
        assert_eq!(next_binary(&mut other).await, server_frame(33, b"alive"));

        other.close(None).await.expect("other closes");
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn removal_prefix_reaches_the_connection_frames_fifo() {
        fn feed(runtime: &mut Runtime, target: PlayerId, payload: &[u8]) {
            let effect = {
                let now = runtime.now();
                let bind_room = runtime.rooms.get_mut(&room()).expect("room");
                bind_room.server_payload(now, target, payload)
            };
            let Runtime {
                rooms,
                udp_notifiers,
                ..
            } = runtime;
            rooms
                .get_mut(&room())
                .expect("room")
                .apply(effect, udp_notifiers);
        }

        let mut runtime = test_runtime();
        let (target, _) = runtime
            .join(room(), Arc::new(Notify::new()))
            .expect("target joins");
        let sender = join(&mut runtime);

        // Queue for the still pre-SYN target, in order: one opaque
        // relay from a non-server route, then two distinguishable host
        // QUERY_RESPONSE packets (a connected player arrives between
        // them).
        {
            let now = runtime.now();
            let bind_room = runtime.rooms.get_mut(&room()).expect("room");
            let (outcome, _) = bind_room
                .host
                .relay(now, sender, target, route(20), b"relay-through-route-20")
                .expect("relay queues");
            assert_eq!(outcome, crate::RelayOutcome::Queued(target));
        }
        let query = ClientPacket::Query.encode(plain(), false).expect("encodes");
        feed(&mut runtime, target, &query);
        let syn = syn_packet("Alice", 0, 0, 0);
        feed(&mut runtime, sender, &syn);
        feed(&mut runtime, target, &query);

        // The old-magic classification is a terminal REJECTED removal.
        let mut old_syn = vec![0x00, 0x00];
        old_syn.extend_from_slice(&PINNED_OLD_MAGIC.to_be_bytes());
        feed(&mut runtime, target, &old_syn);

        // The target is absent from membership, and its connection
        // carries the exact FIFO frames: the relay with its own route,
        // the two responses with the server route, then the terminal.
        let bind_room = runtime.rooms.get(&room()).expect("room");
        assert!(!bind_room.host.contains(target));
        let pending = &bind_room.connections[&target].pending;
        assert_eq!(pending.len(), 4);
        assert_eq!(pending[0], server_frame(20, b"relay-through-route-20"));
        for frame in pending.iter().skip(1) {
            assert_eq!(frame[..4], SERVER_ROUTE.to_le_bytes());
        }
        let (_, ServerPacket::QueryResponse(first)) =
            ServerPacket::decode(&pending[1][OUTBOUND_HEADER_LEN..], false).expect("decodes")
        else {
            panic!("query response")
        };
        let (_, ServerPacket::QueryResponse(second)) =
            ServerPacket::decode(&pending[2][OUTBOUND_HEADER_LEN..], false).expect("decodes")
        else {
            panic!("query response")
        };
        assert_eq!(first.num_players, 0);
        assert_eq!(second.num_players, 1);
        assert!(matches!(
            ServerPacket::decode(&pending[3][OUTBOUND_HEADER_LEN..], false)
                .expect("decodes")
                .1,
            ServerPacket::Rejected { .. }
        ));
    }

    #[tokio::test]
    async fn wire_order_is_owed_prefix_then_terminal_then_close() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/order");
        let (mut client, _) = connect_async(&room_url).await.expect("client connects");

        // Two pre-SYN member QUERYs queue two host responses; the
        // old-magic SYN then removes the client with a terminal
        // REJECTED.
        let query = ClientPacket::Query.encode(plain(), false).expect("encodes");
        send(&mut client, 1, 20, &query).await;
        send(&mut client, 1, 20, &query).await;
        let mut old_syn = vec![0x00, 0x00];
        old_syn.extend_from_slice(&PINNED_OLD_MAGIC.to_be_bytes());
        send(&mut client, 1, 20, &old_syn).await;

        // The wire shows exactly the two responses, then the REJECTED,
        // then the close, all from the server route.
        for _ in 0..2 {
            let (from, packet) = next_decoded(&mut client, false).await;
            assert_eq!(from, SERVER_ROUTE);
            assert!(matches!(packet, ServerPacket::QueryResponse(_)));
        }
        let (from, packet) = next_decoded(&mut client, false).await;
        assert_eq!(from, SERVER_ROUTE);
        assert!(matches!(packet, ServerPacket::Rejected { .. }));
        assert!(matches!(
            next_message(&mut client).await,
            Some(Ok(ClientMessage::Close(_))) | None
        ));
        server.abort();
        let _ = server.await;
    }
}
