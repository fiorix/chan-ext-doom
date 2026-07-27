//! WebSocket binding for the protocol-agnostic room core.

use std::collections::HashMap;
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
use tokio::sync::{Mutex, Notify};

use crate::{
    Envelope, MAX_PAYLOAD_LEN, OutboundPacket, PlayerId, Registry, RelayOutcome, RoomName, RouteId,
};

const INBOUND_HEADER_LEN: usize = 8;
const OUTBOUND_HEADER_LEN: usize = 4;

/// Maximum accepted WebSocket binary-frame size.
pub const MAX_INBOUND_FRAME_LEN: usize = INBOUND_HEADER_LEN + MAX_PAYLOAD_LEN;

/// Serves named rooms at `/ws/{room}` until the listener shuts down.
pub async fn serve(listener: TcpListener, outbox_capacity: NonZeroUsize) -> io::Result<()> {
    let state = ServerState(Arc::new(Mutex::new(Runtime {
        registry: Registry::new(outbox_capacity),
        waiters: HashMap::new(),
    })));
    let router = Router::new()
        .route("/ws/{room}", get(upgrade))
        .with_state(state);

    axum::serve(listener, router).await
}

#[derive(Clone)]
struct ServerState(Arc<Mutex<Runtime>>);

struct Runtime {
    registry: Registry,
    waiters: HashMap<PlayerId, Arc<Notify>>,
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
        let Ok(player_id) = runtime.registry.join(room_name) else {
            return;
        };
        runtime.waiters.insert(player_id, Arc::clone(&waiter));
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
        () = read_loop(stream, state.clone(), player_id) => {
            writer_finished = false;
        }
        _ = &mut writer => {
            writer_finished = true;
        }
    }

    let removed_waiter = {
        let mut runtime = state.0.lock().await;
        runtime.registry.leave(player_id);
        runtime.waiters.remove(&player_id)
    };
    if let Some(waiter) = removed_waiter {
        waiter.notify_one();
    }

    if !writer_finished {
        let _ = writer.await;
    }
}

async fn read_loop(mut stream: SplitStream<WebSocket>, state: ServerState, player_id: PlayerId) {
    while let Some(message) = stream.next().await {
        let Ok(message) = message else {
            return;
        };

        match message {
            Message::Binary(frame) => {
                let Ok(envelope) = decode_inbound(&frame) else {
                    return;
                };
                let (waiter, sender_connected) = {
                    let mut runtime = state.0.lock().await;
                    let Ok(outcome) = runtime.registry.relay(player_id, envelope) else {
                        return;
                    };
                    match outcome {
                        RelayOutcome::Unroutable => (None, true),
                        RelayOutcome::Queued(recipient) => {
                            (runtime.waiters.get(&recipient).cloned(), true)
                        }
                        RelayOutcome::SlowConsumerDisconnected(recipient) => (
                            runtime.waiters.get(&recipient).cloned(),
                            recipient != player_id,
                        ),
                    }
                };
                if let Some(waiter) = waiter {
                    waiter.notify_one();
                }
                if !sender_connected {
                    return;
                }
            }
            Message::Ping(_) => {
                let waiter = {
                    let runtime = state.0.lock().await;
                    runtime.waiters.get(&player_id).cloned()
                };
                if let Some(waiter) = waiter {
                    waiter.notify_one();
                }
            }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecodeError {
    TooShort,
    TooLarge,
    ZeroSource,
}

fn decode_inbound(frame: &[u8]) -> Result<Envelope<'_>, DecodeError> {
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

    Ok(Envelope::new(
        RouteId::new(to),
        from,
        &frame[INBOUND_HEADER_LEN..],
    ))
}

fn encode_outbound(packet: OutboundPacket) -> Vec<u8> {
    let mut frame = Vec::with_capacity(OUTBOUND_HEADER_LEN + packet.payload().len());
    frame.extend_from_slice(&packet.from().get().to_le_bytes());
    frame.extend_from_slice(packet.payload());
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;

    #[tokio::test]
    async fn websocket_binding_relays_between_named_room_members() {
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
        assert_eq!(next_binary(&mut first).await, server_frame(22, b"ready"),);

        first
            .send(ClientMessage::Binary(
                client_frame(22, 11, b"packet").into(),
            ))
            .await
            .expect("first client sends");
        assert_eq!(next_binary(&mut second).await, server_frame(11, b"packet"),);

        first.close(None).await.expect("first client closes");
        second.close(None).await.expect("second client closes");
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn inbound_envelope_uses_little_endian_addresses() {
        let frame = [2, 0, 0, 0, 1, 0, 0, 0, 0xaa, 0xbb];
        let envelope = decode_inbound(&frame).expect("valid frame decodes");

        assert_eq!(
            envelope,
            Envelope::new(RouteId::new(2), route(1), &[0xaa, 0xbb])
        );
    }

    #[test]
    fn registration_envelope_has_no_destination() {
        let frame = [0, 0, 0, 0, 7, 0, 0, 0];
        let envelope = decode_inbound(&frame).expect("registration frame decodes");

        assert_eq!(envelope, Envelope::new(None, route(7), &[]));
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

    async fn next_binary<S>(socket: &mut S) -> Vec<u8>
    where
        S: StreamExt<Item = Result<ClientMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        match socket.next().await {
            Some(Ok(ClientMessage::Binary(frame))) => frame.to_vec(),
            other => panic!("expected binary frame, got {other:?}"),
        }
    }
}
