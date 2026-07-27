//! WebSocket binding for the protocol-agnostic room core.

use std::collections::HashMap;
use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};

use crate::{
    MAX_PAYLOAD_LEN, OutboundPacket, PlayerId, Registry, RelayError, RelayOutcome, RoomName,
};

const INBOUND_HEADER_LEN: usize = 8;
const OUTBOUND_HEADER_LEN: usize = 4;
const RESET_ROUTE: u32 = 1;

/// Maximum accepted WebSocket binary-frame size.
pub const MAX_INBOUND_FRAME_LEN: usize = INBOUND_HEADER_LEN + MAX_PAYLOAD_LEN;

/// Serves named rooms at `/ws/{room}` until the listener shuts down.
pub async fn serve(listener: TcpListener, outbox_capacity: NonZeroUsize) -> io::Result<()> {
    let state = ServerState(Arc::new(Mutex::new(Runtime {
        registry: Registry::new(outbox_capacity),
        connections: HashMap::new(),
        routes: HashMap::new(),
    })));
    let router = Router::new()
        .route("/ws/{room}", get(upgrade))
        .with_state(state);

    axum::serve(listener, router).await
}

#[derive(Clone)]
struct ServerState(Arc<Mutex<Runtime>>);

struct Runtime {
    registry: Registry<RouteId>,
    connections: HashMap<PlayerId, Connection>,
    routes: HashMap<RoomName, HashMap<RouteId, PlayerId>>,
}

struct Connection {
    room_name: RoomName,
    route: Option<RouteId>,
    waiter: Arc<Notify>,
}

impl Runtime {
    fn join(
        &mut self,
        room_name: RoomName,
        waiter: Arc<Notify>,
    ) -> Result<PlayerId, crate::JoinError> {
        let player_id = self.registry.join(room_name.clone())?;
        self.connections.insert(
            player_id,
            Connection {
                room_name,
                route: None,
                waiter,
            },
        );
        Ok(player_id)
    }

    fn handle_envelope(
        &mut self,
        player_id: PlayerId,
        envelope: WireEnvelope<'_>,
    ) -> Result<FrameEffect, BindingError> {
        let mut effect = FrameEffect::default();
        if envelope.is_reset() {
            effect.waiters = self.reset_room(player_id);
        }

        self.bind_source(player_id, envelope.from)?;
        let Some(destination) = envelope.to else {
            return Ok(effect);
        };
        let Some(recipient) = self.recipient(player_id, destination) else {
            return Ok(effect);
        };

        match self
            .registry
            .relay(player_id, recipient, envelope.from, envelope.payload)?
        {
            RelayOutcome::Unroutable => {}
            RelayOutcome::Queued(recipient) => {
                if let Some(connection) = self.connections.get(&recipient) {
                    effect.waiters.push(Arc::clone(&connection.waiter));
                }
            }
            RelayOutcome::SlowConsumerDisconnected(recipient) => {
                if let Some(waiter) = self.disconnect(recipient) {
                    effect.waiters.push(waiter);
                }
                effect.sender_connected = recipient != player_id;
            }
        }

        Ok(effect)
    }

    fn bind_source(&mut self, player_id: PlayerId, source: RouteId) -> Result<(), BindingError> {
        let connection = self
            .connections
            .get(&player_id)
            .ok_or(BindingError::UnknownPlayer)?;
        match connection.route {
            Some(route) if route != source => return Err(BindingError::SourceChanged),
            Some(_) => return Ok(()),
            None => {}
        }
        let room_name = connection.room_name.clone();
        if self
            .routes
            .get(&room_name)
            .is_some_and(|routes| routes.contains_key(&source))
        {
            return Err(BindingError::SourceInUse);
        }

        self.connections
            .get_mut(&player_id)
            .expect("the connection was checked above")
            .route = Some(source);
        self.routes
            .entry(room_name)
            .or_default()
            .insert(source, player_id);
        Ok(())
    }

    fn recipient(&self, player_id: PlayerId, destination: RouteId) -> Option<PlayerId> {
        let room_name = &self.connections.get(&player_id)?.room_name;
        self.routes.get(room_name)?.get(&destination).copied()
    }

    fn reset_room(&mut self, player_id: PlayerId) -> Vec<Arc<Notify>> {
        let Some(room_name) = self
            .connections
            .get(&player_id)
            .map(|connection| connection.room_name.clone())
        else {
            return Vec::new();
        };
        let victims: Vec<_> = self
            .connections
            .iter()
            .filter_map(|(&candidate, connection)| {
                (candidate != player_id && connection.room_name == room_name).then_some(candidate)
            })
            .collect();
        let waiters = victims
            .into_iter()
            .filter_map(|victim| self.disconnect(victim))
            .collect();
        self.unbind(player_id);
        waiters
    }

    fn disconnect(&mut self, player_id: PlayerId) -> Option<Arc<Notify>> {
        self.registry.leave(player_id);
        let connection = self.connections.remove(&player_id)?;
        if let Some(route) = connection.route {
            self.remove_route(&connection.room_name, route);
        }
        Some(connection.waiter)
    }

    fn unbind(&mut self, player_id: PlayerId) {
        let Some((room_name, route)) =
            self.connections.get_mut(&player_id).and_then(|connection| {
                connection
                    .route
                    .take()
                    .map(|route| (connection.room_name.clone(), route))
            })
        else {
            return;
        };
        self.remove_route(&room_name, route);
    }

    fn remove_route(&mut self, room_name: &RoomName, route: RouteId) {
        let remove_room = self.routes.get_mut(room_name).is_some_and(|routes| {
            routes.remove(&route);
            routes.is_empty()
        });
        if remove_room {
            self.routes.remove(room_name);
        }
    }
}

struct FrameEffect {
    waiters: Vec<Arc<Notify>>,
    sender_connected: bool,
}

impl Default for FrameEffect {
    fn default() -> Self {
        Self {
            waiters: Vec::new(),
            sender_connected: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BindingError {
    UnknownPlayer,
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
    let player_id = {
        let mut runtime = state.0.lock().await;
        let Ok(player_id) = runtime.join(room_name, Arc::clone(&waiter)) else {
            return;
        };
        player_id
    };

    let (sink, stream) = socket.split();
    let mut writer = tokio::spawn(write_loop(
        sink,
        state.clone(),
        player_id,
        Arc::clone(&waiter),
    ));
    let writer_finished;

    tokio::select! {
        () = read_loop(stream, state.clone(), player_id, Arc::clone(&waiter)) => {
            writer_finished = false;
        }
        _ = &mut writer => {
            writer_finished = true;
        }
    }

    let removed_waiter = {
        let mut runtime = state.0.lock().await;
        runtime.disconnect(player_id)
    };
    removed_waiter.unwrap_or(waiter).notify_one();

    if !writer_finished {
        let _ = writer.await;
    }
}

async fn read_loop(
    mut stream: SplitStream<WebSocket>,
    state: ServerState,
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
                let effect = {
                    let mut runtime = state.0.lock().await;
                    let Ok(effect) = runtime.handle_envelope(player_id, envelope) else {
                        return;
                    };
                    effect
                };
                for waiter in effect.waiters {
                    waiter.notify_one();
                }
                if !effect.sender_connected {
                    return;
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
    player_id: PlayerId,
    waiter: Arc<Notify>,
) {
    loop {
        let (packet, connected) = {
            let mut runtime = state.0.lock().await;
            (
                runtime.registry.pop_outbound(player_id),
                runtime.registry.contains(player_id),
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
        } else if !connected {
            let _ = sink.send(Message::Close(None)).await;
            return;
        } else {
            if sink.flush().await.is_err() {
                return;
            }
            waiter.notified().await;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct RouteId(NonZeroU32);

impl RouteId {
    const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    const fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WireEnvelope<'a> {
    to: Option<RouteId>,
    from: RouteId,
    payload: &'a [u8],
}

impl WireEnvelope<'_> {
    fn is_reset(self) -> bool {
        self.to.is_none() && self.from.get() == RESET_ROUTE && self.payload.is_empty()
    }
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

fn encode_outbound(packet: OutboundPacket<RouteId>) -> Vec<u8> {
    let mut frame = Vec::with_capacity(OUTBOUND_HEADER_LEN + packet.payload().len());
    frame.extend_from_slice(&packet.metadata().get().to_le_bytes());
    frame.extend_from_slice(packet.payload());
    frame
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use tokio::time::timeout;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

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

        first
            .send(ClientMessage::Binary(client_frame(0, 11, &[]).into()))
            .await
            .expect("first route registers");
        second
            .send(ClientMessage::Binary(client_frame(11, 22, b"ready").into()))
            .await
            .expect("second route registers and sends");
        assert_eq!(next_binary(&mut first).await, server_frame(22, b"ready"));

        first
            .send(ClientMessage::Binary(
                client_frame(22, 11, b"packet").into(),
            ))
            .await
            .expect("first client sends");
        assert_eq!(next_binary(&mut second).await, server_frame(11, b"packet"));

        first.close(None).await.expect("first client closes");
        second.close(None).await.expect("second client closes");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn exact_server_registration_resets_the_room() {
        let (address, server) = spawn_server().await;
        let room_url = format!("ws://{address}/ws/reset");
        let (mut existing, _) = connect_async(&room_url)
            .await
            .expect("existing client connects");

        existing
            .send(ClientMessage::Binary(client_frame(22, 22, b"ready").into()))
            .await
            .expect("existing route registers");
        assert_eq!(next_binary(&mut existing).await, server_frame(22, b"ready"));

        let (mut resetter, _) = connect_async(&room_url).await.expect("resetter connects");
        resetter
            .send(ClientMessage::Binary(client_frame(0, 1, &[]).into()))
            .await
            .expect("reset frame sends");
        assert!(matches!(
            next_message(&mut existing).await,
            Some(Ok(ClientMessage::Close(_)))
        ));

        resetter
            .send(ClientMessage::Binary(client_frame(1, 1, b"alive").into()))
            .await
            .expect("resetter remains joined");
        assert_eq!(next_binary(&mut resetter).await, server_frame(1, b"alive"));

        resetter.close(None).await.expect("resetter closes");
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn source_binding_rejects_changes_and_duplicate_ownership() {
        let mut runtime = test_runtime();
        let waiter = Arc::new(Notify::new());
        let first = runtime
            .join(room(), Arc::clone(&waiter))
            .expect("first player joins");
        let second = runtime
            .join(room(), Arc::new(Notify::new()))
            .expect("second player joins");

        assert_eq!(runtime.bind_source(first, route(11)), Ok(()));
        assert_eq!(
            runtime.bind_source(first, route(12)),
            Err(BindingError::SourceChanged)
        );
        assert_eq!(
            runtime.bind_source(second, route(11)),
            Err(BindingError::SourceInUse)
        );
    }

    #[test]
    fn reset_marker_must_be_exactly_eight_bytes() {
        assert!(
            decode_inbound(&client_frame(0, 1, &[]))
                .expect("exact reset decodes")
                .is_reset()
        );
        assert!(
            !decode_inbound(&client_frame(0, 1, b"x"))
                .expect("payload frame decodes")
                .is_reset()
        );
        assert!(
            !decode_inbound(&client_frame(2, 1, &[]))
                .expect("addressed frame decodes")
                .is_reset()
        );
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

    fn test_runtime() -> Runtime {
        Runtime {
            registry: Registry::new(NonZeroUsize::new(4).expect("test capacity is nonzero")),
            connections: HashMap::new(),
            routes: HashMap::new(),
        }
    }

    fn room() -> RoomName {
        RoomName::try_from("e1m1").expect("test room name is valid")
    }

    fn route(value: u32) -> RouteId {
        RouteId::new(value).expect("test route is nonzero")
    }

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
                NonZeroUsize::new(4).expect("test capacity is nonzero"),
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
        timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("server responds within one second")
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
}
