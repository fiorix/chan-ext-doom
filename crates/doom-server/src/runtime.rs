//! Shared binding runtime: one room map, one monotonic clock, one
//! timer, and one effect-application path for every transport. A room
//! name has exactly one `RoomHost`, one `ServerRole`, one
//! membership/outbox registry, and one timer regardless of transport
//! mix; transport identity and address maps live here at the binding,
//! never in the sans-I/O role.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

use doom_proto::RELIABLE_BIT;

use crate::room::{HostEffect, RoomHost};
use crate::server_role::{MalformedClass, Milliseconds};
use crate::{JoinError, PlayerId, RoomName};

/// Route 1 is permanently owned by the Rust server in every room.
pub(crate) const SERVER_ROUTE: u32 = 1;
/// The pre-3.0 SYN magic: the one malformed case with a source-backed
/// rejection.
pub(crate) const OLD_SYN_MAGIC: u32 = 3_436_803_284;
/// The runtime timer period driving every live room role.
pub(crate) const TIMER_PERIOD: std::time::Duration = std::time::Duration::from_millis(50);

/// The shared runtime state handle.
#[derive(Clone)]
pub(crate) struct SharedState(pub(crate) Arc<Mutex<Runtime>>);

/// One configured UDP listener's future: it resolves only when its
/// socket fails or closes, which must end the shared service.
type ListenerFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>>;

/// The composition root: one shared runtime, one bounded timer, the
/// WebSocket router, and every configured UDP listener, with no future
/// detached, so cancelling or dropping the serve future cancels all of
/// them together.
pub async fn serve(
    listener: TcpListener,
    udp_listeners: Vec<(RoomName, tokio::net::UdpSocket)>,
    outbox_capacity: NonZeroUsize,
) -> std::io::Result<()> {
    let state = SharedState(Arc::new(Mutex::new(Runtime {
        rooms: HashMap::new(),
        outbox_capacity,
        started: Instant::now(),
        next_listener: 0,
        udp_notifiers: HashMap::new(),
    })));
    let router = crate::websocket::router(state.clone());
    let server = axum::serve(listener, router).into_future();
    let timer = timer_loop(state.clone());
    let udp = udp_supervisor(
        udp_listeners
            .into_iter()
            .map(|(room_name, socket)| {
                Box::pin(crate::udp::listener(socket, room_name, state.clone())) as ListenerFuture
            })
            .collect(),
    );
    tokio::select! {
        result = server => result,
        () = timer => Ok(()),
        result = udp => result,
    }
}

/// The UDP branch of the composition root. An empty listener list
/// pends forever so a WebSocket-only serve persists; with any listener
/// configured, the FIRST listener completion or error resolves the
/// branch and ends the shared service, cancelling the rest with it.
pub(crate) async fn udp_supervisor(listeners: Vec<ListenerFuture>) -> std::io::Result<()> {
    if listeners.is_empty() {
        futures_util::future::pending::<()>().await;
        unreachable!("the empty-listener pending branch never resolves");
    }
    let (result, _, _) = futures_util::future::select_all(listeners).await;
    result
}

pub(crate) struct Runtime {
    pub(crate) rooms: HashMap<RoomName, BindRoom>,
    pub(crate) outbox_capacity: NonZeroUsize,
    pub(crate) started: Instant,
    /// The next opaque UDP listener identity to issue.
    pub(crate) next_listener: u64,
    /// UDP listener wake handles by opaque listener identity: two
    /// listeners pinned to one room stay distinct, and the table lives
    /// outside the room map so a configured listener never pins an
    /// otherwise empty room host.
    pub(crate) udp_notifiers: HashMap<ListenerId, Arc<Notify>>,
}

/// One admitted UDP peer: the listener it arrived on plus its remote
/// address. Reply provenance, wakeups, and lifecycle are scoped to
/// that pair, so the same remote socket reaching two listeners of one
/// room is two distinct peers sharing the one `RoomHost`.
pub(crate) struct UdpPeer {
    pub(crate) listener: ListenerId,
    pub(crate) address: SocketAddr,
}

/// One named room across all transports: the shared host plus the
/// per-transport connection and identity maps.
pub(crate) struct BindRoom {
    pub(crate) host: RoomHost<RouteId>,
    pub(crate) connections: HashMap<PlayerId, Connection>,
    pub(crate) routes: HashMap<RouteId, PlayerId>,
    /// UDP address identity, binding-owned and listener-scoped:
    /// removed in the same reduction as the registry removal, without
    /// consulting the role.
    pub(crate) udp_players: HashMap<PlayerId, UdpPeer>,
    pub(crate) udp_addresses: HashMap<(ListenerId, SocketAddr), PlayerId>,
    /// Datagrams captured at removal time (the removed peer's remaining
    /// outbox and its terminal, terminal last), per listener so each
    /// socket sends exactly its own peers' traffic. Bounded per removal
    /// by the removed peer's own bounded outbox; drained by the
    /// listener that owns it.
    pub(crate) pending_udp: HashMap<ListenerId, VecDeque<(SocketAddr, Vec<u8>)>>,
}

pub(crate) struct Connection {
    pub(crate) route: Option<RouteId>,
    pub(crate) waiter: Arc<Notify>,
    /// The final binary frame owed to this peer before its close.
    pub(crate) terminal: Option<Vec<u8>>,
}

/// The work a reduction batch produced, per transport, to perform
/// after the mutex is released.
#[derive(Default)]
pub(crate) struct Effects {
    /// WebSocket writers to wake.
    pub(crate) ws_waiters: Vec<Arc<Notify>>,
    /// UDP listener notifiers to wake so each listener drains and sends.
    pub(crate) udp_waiters: Vec<Arc<Notify>>,
    /// Datagrams to send immediately, outside any room state (the
    /// stateless QUERY reply).
    pub(crate) direct_udp: Vec<(SocketAddr, Vec<u8>)>,
}

impl Effects {
    pub(crate) fn merge(&mut self, other: Effects) {
        self.ws_waiters.extend(other.ws_waiters);
        self.udp_waiters.extend(other.udp_waiters);
        self.direct_udp.extend(other.direct_udp);
    }
}

impl BindRoom {
    pub(crate) fn new(room_name: &RoomName, outbox_capacity: NonZeroUsize) -> Self {
        Self {
            host: RoomHost::new(
                room_name.clone(),
                outbox_capacity,
                RouteId::new(SERVER_ROUTE).expect("the server route is nonzero"),
            ),
            connections: HashMap::new(),
            routes: HashMap::new(),
            udp_players: HashMap::new(),
            udp_addresses: HashMap::new(),
            pending_udp: HashMap::new(),
        }
    }

    /// One payload addressed to the room's server: decode only as a
    /// client packet with the room's authoritative width; undecodable
    /// bytes are classified narrowly and the role decides by peer
    /// state. Shared by every transport's server-destined input.
    pub(crate) fn server_payload(
        &mut self,
        now: Milliseconds,
        player: PlayerId,
        payload: &[u8],
    ) -> HostEffect {
        match doom_proto::ClientPacket::decode(payload, self.host.lowres_turn()) {
            Ok((header, packet)) => self.host.packet(now, player, header, packet),
            Err(_) => self
                .host
                .malformed(now, player, classify_malformed(payload)),
        }
    }

    /// Whether the room holds nothing on any transport (the runtime
    /// then drops it, so unbounded room names cannot retain hosts).
    pub(crate) fn is_empty(&self) -> bool {
        self.host.is_empty() && self.connections.is_empty() && self.udp_players.is_empty()
    }

    /// Fold one reducer batch into transport state; the returned work
    /// is performed only after the mutex is released. WebSocket
    /// terminal packets ride their connection as the final frame; UDP
    /// removals capture every already-owed datagram first and the
    /// terminal last into the bounded pending queue, drop the address
    /// mapping in the same batch, and wake the listener to send.
    pub(crate) fn apply(
        &mut self,
        effect: HostEffect,
        udp_notifiers: &HashMap<ListenerId, Arc<Notify>>,
    ) -> Effects {
        let mut effects = Effects::default();
        let notify_udp = |effects: &mut Effects, listener: ListenerId| {
            if let Some(notifier) = udp_notifiers.get(&listener)
                && !effects.udp_waiters.iter().any(|w| Arc::ptr_eq(w, notifier))
            {
                effects.udp_waiters.push(Arc::clone(notifier));
            }
        };
        for player in effect.wakes {
            if let Some(connection) = self.connections.get(&player) {
                effects.ws_waiters.push(Arc::clone(&connection.waiter));
            }
            if let Some(peer) = self.udp_players.get(&player) {
                notify_udp(&mut effects, peer.listener);
            }
        }
        for (player, terminal) in effect.disconnects {
            if let Some(connection) = self.connections.get_mut(&player) {
                if let Some(terminal) = terminal {
                    let mut frame = Vec::with_capacity(4 + terminal.len());
                    frame.extend_from_slice(&SERVER_ROUTE.to_le_bytes());
                    frame.extend_from_slice(&terminal);
                    connection.terminal = Some(frame);
                }
                effects.ws_waiters.push(Arc::clone(&connection.waiter));
            } else if let Some(peer) = self.udp_players.remove(&player) {
                self.udp_addresses.remove(&(peer.listener, peer.address));
                let pending = self.pending_udp.entry(peer.listener).or_default();
                while let Some(packet) = self.host.pop_outbound(player) {
                    pending.push_back((peer.address, packet.payload().to_vec()));
                }
                if let Some(terminal) = terminal {
                    pending.push_back((peer.address, terminal));
                }
                notify_udp(&mut effects, peer.listener);
            }
        }
        effects
    }
}

impl Runtime {
    pub(crate) fn now(&self) -> Milliseconds {
        Milliseconds(self.started.elapsed().as_millis() as u64)
    }

    /// Issue the opaque identity and wake handle for one configured
    /// UDP listener. The handle lives outside the room map, so a
    /// listener never retains an otherwise empty room host.
    pub(crate) fn register_listener(&mut self) -> (ListenerId, Arc<Notify>) {
        let listener = ListenerId(self.next_listener);
        self.next_listener += 1;
        let notifier = Arc::new(Notify::new());
        self.udp_notifiers.insert(listener, Arc::clone(&notifier));
        (listener, notifier)
    }

    pub(crate) fn join(
        &mut self,
        room_name: RoomName,
        waiter: Arc<Notify>,
    ) -> Result<(PlayerId, Effects), JoinError> {
        let now = self.now();
        let Self {
            rooms,
            udp_notifiers,
            outbox_capacity,
            ..
        } = self;
        let room = rooms
            .entry(room_name.clone())
            .or_insert_with(|| BindRoom::new(&room_name, *outbox_capacity));
        let (player, effect) = room
            .host
            .join(now, |player| format!("ws:{}", player.get()).into_bytes())?;
        room.connections.insert(
            player,
            Connection {
                route: None,
                waiter,
                terminal: None,
            },
        );
        Ok((player, room.apply(effect, udp_notifiers)))
    }

    /// Transport hangup or binding-initiated removal: the room host's
    /// own initiator contract decides between exactly one `Leave` and
    /// an inert cleanup; this method only removes the transport state
    /// around it, and the room is dropped once it holds neither members
    /// nor connections on any transport.
    pub(crate) fn leave(&mut self, room_name: &RoomName, player_id: PlayerId) -> Effects {
        let mut effects = Effects::default();
        let now = self.now();
        let Self {
            rooms,
            udp_notifiers,
            ..
        } = self;
        let Some(room) = rooms.get_mut(room_name) else {
            return effects;
        };
        let effect = room.host.leave(now, player_id);
        effects.merge(room.apply(effect, udp_notifiers));
        if let Some(connection) = room.connections.remove(&player_id) {
            if let Some(route) = connection.route {
                room.routes.remove(&route);
            }
            effects.ws_waiters.push(connection.waiter);
        }
        if room.is_empty() {
            rooms.remove(room_name);
        }
        effects
    }

    /// One bounded timer drives every live room role with monotonic
    /// elapsed milliseconds, through the same reducer and notifier path
    /// as packet actions.
    pub(crate) fn tick(&mut self) -> Effects {
        let now = self.now();
        let mut effects = Effects::default();
        let mut empty = Vec::new();
        let Self {
            rooms,
            udp_notifiers,
            ..
        } = self;
        for (room_name, room) in rooms.iter_mut() {
            let effect = room.host.tick(now);
            effects.merge(room.apply(effect, udp_notifiers));
            if room.is_empty() {
                empty.push(room_name.clone());
            }
        }
        for room_name in empty {
            rooms.remove(&room_name);
        }
        effects
    }
}

/// One bounded runtime timer for every room and transport, coupled to
/// the serve future that drives it: missed ticks are skipped, while the
/// role clock still sees the real jump through monotonic elapsed
/// milliseconds.
pub(crate) async fn timer_loop(state: SharedState) {
    let mut interval = tokio::time::interval(TIMER_PERIOD);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let effects = {
            let mut runtime = state.0.lock().await;
            runtime.tick()
        };
        for waiter in effects.ws_waiters {
            waiter.notify_one();
        }
        for waiter in effects.udp_waiters {
            waiter.notify_one();
        }
    }
}

/// A route identifier inside a room's WebSocket envelope.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RouteId(NonZeroU32);

impl RouteId {
    pub(crate) const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Opaque identity of one configured UDP listener, issued at serve
/// startup. UDP address identity is the `(ListenerId, SocketAddr)`
/// pair, never the remote address alone.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ListenerId(u64);

/// Classify undecodable server-addressed bytes narrowly: a SYN-shaped
/// type with the pinned pre-3.0 magic is the one case with a
/// source-backed rejection; other SYN shapes are plain malformed SYNs;
/// anything else is established-peer noise. Truncation and unsupported
/// types never panic and never produce a generic rejection.
pub(crate) fn classify_malformed(payload: &[u8]) -> MalformedClass {
    let Some(type_bytes) = payload.get(..2) else {
        return MalformedClass::Established;
    };
    // Protocol header words are big-endian on the wire.
    let word = u16::from_be_bytes(type_bytes.try_into().expect("two bytes"));
    if word & !RELIABLE_BIT != 0 {
        return MalformedClass::Established;
    }
    // SYN-shaped: the magic follows the type word and the reliable
    // sequence byte when present.
    let offset = if word & RELIABLE_BIT != 0 { 3 } else { 2 };
    let magic = payload
        .get(offset..offset + 4)
        .map(|bytes| u32::from_be_bytes(bytes.try_into().expect("four bytes")));
    match magic {
        Some(OLD_SYN_MAGIC) => MalformedClass::Syn { old_magic: true },
        _ => MalformedClass::Syn { old_magic: false },
    }
}
