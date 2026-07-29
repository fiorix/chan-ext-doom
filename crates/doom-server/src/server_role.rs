//! Deterministic sans-I/O Chocolate server role: connection lifecycle,
//! lobby, and authoritative GAMESTART.
//!
//! The role consumes decoded [`ClientPacket`] values and a caller-supplied
//! monotonic clock, and returns explicit bounded actions. There are no
//! sockets, addresses, WebSocket envelopes, async types, tasks, sleeps,
//! wall-clock reads, or I/O in this module. Packet semantics live in
//! `doom-proto`; transport metadata (such as the lobby address label) is
//! carried as opaque bounded bytes, never interpreted.
//!
//! Peer identity is the registry-allocated [`PlayerId`]: the room host
//! admits membership first and only then introduces the peer here, so a
//! failed admission can never leave the role believing a peer exists.
//!
//! Behavior is traced from the pinned Chocolate Doom 3.1.1 server
//! (`net_server.c`, `net_common.c` at `410d9685`). Constants: 16 protocol
//! slots, 8 room connections (enforced by the registry), 4 Doom players
//! (from client capability), 30 s receive-silence timeout, 1 s keepalive,
//! 1 s reliable head retry, 1 s WAITING_DATA cadence, 5 initiated
//! DISCONNECT sends at 1 s intervals, 5 s disconnected-sleep, and a
//! per-peer reliable FIFO cap of 64, the one deliberate abuse-path
//! deviation from upstream's unbounded list.

use std::collections::{BTreeMap, VecDeque};

use doom_proto::{ClientPacket, GameSettings, ServerPacket, WireHeader};

use crate::PlayerId;

/// Monotonic milliseconds. The caller promises this never goes backwards;
/// tests drive it by hand.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Milliseconds(pub u64);

const MAX_NODES: usize = 16;
const NET_MAX_PLAYERS: usize = 8;
const MAX_NAME_LEN: usize = 30;
const MODE_INDETERMINED: u8 = 4;

/// In-game window size (`BACKUPTICS`).
const BACKUPTICS: usize = 128;
/// `NET_ExpandTicNum` wrap boundaries.
const EXPAND_LOW: i64 = 0x40;
const EXPAND_HIGH: i64 = 0xb0;
/// A client further than this ahead of the minimum acknowledgement is not
/// pumped (pinned 40-tic stall guard).
const STALL_TICS: i64 = 40;
/// With no other player's tic present, the server stays at most this far
/// ahead of the receive window (pinned single-player limit).
const SINGLE_PLAYER_AHEAD: i64 = 10;
/// Missing tics are re-requested only after strictly more than this.
const RESEND_AFTER_MS: u64 = 300;
/// No accepted in-range game data for this long triggers deadlock recovery.
const DEADLOCK_MS: u64 = 1_000;

/// Reliable FIFO cap per peer. The 65th enqueue removes the peer.
const RELIABLE_CAP: usize = 64;
/// Silence after which a connected peer is dropped (CONNECTION_TIMEOUT_LEN).
const TIMEOUT_MS: u64 = 30_000;
/// Send-idle period after which a bare keepalive goes out.
const KEEPALIVE_MS: u64 = 1_000;
/// Age of the reliable head after which it is retried in place.
const RELIABLE_RETRY_MS: u64 = 1_000;
/// Lobby update cadence while waiting for launch.
const WAITDATA_MS: u64 = 1_000;
/// Initiated DISCONNECT sends before forcing removal (MAX_RETRIES).
const DISCONNECT_SENDS: u8 = 5;
const DISCONNECT_RETRY_MS: u64 = 1_000;
/// How long a remotely disconnected identity lingers for ACK re-sends.
const SLEEP_MS: u64 = 5_000;

/// Server identity sent in the SYN accept and the query response.
const SERVER_VERSION: &[u8] = b"doomit doom-server (Chocolate 3.1.1 compatible)";
/// The one protocol name this implementation negotiates.
const PROTOCOL_NAME: &[u8] = b"CHOCOLATE_DOOM_0";
/// Query response description.
const SERVER_DESCRIPTION: &[u8] = b"doomit room server";

/// What the room host feeds the role.
#[derive(Clone, Debug)]
pub enum Input {
    /// A peer the room host has already admitted. `addr_label` is opaque
    /// transport metadata echoed back in lobby updates; it must encode as
    /// a bounded wire string (no NUL, at most 29 bytes), and the role
    /// validates that here rather than letting an unencodable action out.
    Join {
        player: PlayerId,
        addr_label: Vec<u8>,
    },
    /// The room host removed the peer (hangup or slow-consumer removal).
    Leave { player: PlayerId },
    /// A decoded client packet with its framing.
    Packet {
        player: PlayerId,
        header: WireHeader,
        packet: ClientPacket,
    },
    /// Bytes that did not decode. The caller classifies the BYTES (a
    /// SYN-shaped packet, possibly from a pre-3.0 client, versus anything
    /// else); the role then decides by PEER STATE what the pinned source
    /// does with them: a pre-SYN member is rejected and removed, an
    /// established peer is rejected but retained, and every other class
    /// is dropped by default.
    Malformed {
        player: PlayerId,
        class: MalformedClass,
    },
    /// Time advance. All timers run only from this input.
    Timer,
}

/// Malformed-byte classification supplied by the caller. This classifies
/// the bytes only; peer state responsibilities (retain versus remove)
/// are the role's, per pinned `NET_SV_ParseSYN`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MalformedClass {
    /// A SYN-shaped packet; `old_magic` marks the pre-3.0 magic value,
    /// the one malformed case with a source-backed REJECT.
    Syn { old_magic: bool },
    /// Anything else from an established peer.
    Established,
}

/// What the role wants the room host to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Queue a fully framed server packet for one peer. `lowres` is
    /// the codec context that was authoritative when the role produced
    /// the packet: the reducer encodes with exactly this width, and
    /// the tag travels with the bytes through the outbox and any
    /// removal capture, so a later settings reset can never rewrite
    /// the context of an already-produced packet.
    Send {
        player: PlayerId,
        header: WireHeader,
        lowres: bool,
        packet: ServerPacket,
    },
    /// Remove the peer from the room. `terminal` is the final packet the
    /// peer is owed (a REJECTED or an acknowledgement). This is the
    /// SendAndClose contract, atomic by construction: the terminal
    /// packet rides inside the removal action itself, so no ordering
    /// mistake can separate it from the close, and the binding must
    /// deliver it to the peer before closing, because the role's state
    /// for the peer is already gone and no later send will ever be
    /// produced for it. A binding-style reducer proves the delivery in
    /// `terminal_packets_survive_a_binding_style_reducer`.
    Disconnect {
        player: PlayerId,
        reason: DisconnectReason,
        terminal: Option<Box<(WireHeader, ServerPacket)>>,
    },
    /// The room returns to waiting-for-launch (startup aborted or no
    /// players remain). Initiated-disconnect actions for survivors
    /// precede it.
    GameEnded,
}

/// Why the role removed a peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisconnectReason {
    Timeout,
    ReliableOverflow,
    Remote,
    GameEnded,
    /// An input that could never encode (for example a label with NUL).
    MalformedInput,
}

/// Room-level state (`net_server_state_t`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerState {
    WaitingLaunch,
    WaitingStart,
    InGame,
}

/// The pinned connection lifecycle for one peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Conn {
    Connected,
    /// We initiated: five sends at 1 s intervals, then forced removal.
    /// The retry clock starts at the actual first send.
    Disconnecting {
        sends: u8,
        last: Milliseconds,
        reason: DisconnectReason,
    },
    /// The peer sent DISCONNECT: acknowledged, then the identity lingers
    /// so a lost ACK can be re-sent to a duplicate DISCONNECT.
    Sleeping {
        until: Milliseconds,
    },
}

/// One reliable outbox entry. Only the head is ever emitted: a new entry
/// behind an unacknowledged head waits.
#[derive(Clone, Debug, PartialEq)]
struct ReliableEntry {
    seq: u8,
    packet: ServerPacket,
    last_retry: Option<Milliseconds>,
}

/// Pinned `NET_ExpandTicNum`: expand the low byte of a tic number against
/// a reference (the receive window start), wrapping at the 0x40/0xb0
/// boundaries. Computed in i64 so nothing underflows; out-of-range
/// results are discarded by the window checks, never aliased.
fn expand_tic(relative: u32, low: u8) -> i64 {
    let high = (relative & !0xff) as i64;
    let low_part = (relative & 0xff) as i64;
    let byte = low as i64;
    let mut result = high | byte;
    if low_part < EXPAND_LOW && byte > EXPAND_HIGH {
        result -= 0x100;
    }
    if low_part > EXPAND_HIGH && byte < EXPAND_LOW {
        result += 0x100;
    }
    result
}

/// Pinned acknowledgement contract: an expanded acknowledgement applies
/// only when it is non-negative, strictly ahead of the stored value, and
/// no greater than the peer's send sequence (a client cannot acknowledge
/// tics the server never sent). Anything else is ignored, without
/// affecting anything else in the same packet.
fn apply_ack(game: &mut PeerGame, expanded: i64) {
    if expanded < 0 || expanded <= game.acknowledged as i64 || expanded > game.sendseq as i64 {
        return;
    }
    game.acknowledged = expanded as u32;
}

/// One receive-window slot for one player: a received tic, its latency,
/// and the last time a resend was requested for it.
#[derive(Clone, Debug, Default, PartialEq)]
struct RecvEntry {
    active: bool,
    latency: i16,
    diff: doom_proto::TiccmdDiff,
    resend_time: Option<Milliseconds>,
}

/// The shared bounded receive window. Slot `i` always means absolute tic
/// `start + i`; absolute identity is preserved by construction as the
/// window shifts, and every stored tic is verified in range before use.
#[derive(Clone, Debug)]
struct RecvWindow {
    start: u32,
    entries: Box<[[RecvEntry; NET_MAX_PLAYERS]; BACKUPTICS]>,
}

impl RecvWindow {
    fn new(start: u32) -> Self {
        RecvWindow {
            start,
            entries: Box::new(std::array::from_fn(|_| {
                std::array::from_fn(|_| RecvEntry::default())
            })),
        }
    }
}

/// One queued fan-out tic with its absolute identity, so a stale or
/// spoofed resend request cannot alias an overwritten slot.
#[derive(Clone, Debug, PartialEq)]
struct QueuedTic {
    seq: u32,
    tic: doom_proto::FullTic,
}

/// A connected peer's in-game send state.
#[derive(Clone, Debug)]
struct PeerGame {
    sendseq: u32,
    acknowledged: u32,
    /// Updated only when an upload stores at least one tic in range
    /// (pinned deadlock-clock semantics).
    last_gamedata: Milliseconds,
    sendqueue: Box<[Option<QueuedTic>; BACKUPTICS]>,
}

impl PeerGame {
    fn new(now: Milliseconds) -> Self {
        PeerGame {
            sendseq: 0,
            acknowledged: 0,
            last_gamedata: now,
            sendqueue: Box::new([const { None }; BACKUPTICS]),
        }
    }
}

/// Per-peer protocol state.
#[derive(Clone, Debug)]
struct Peer {
    /// A valid SYN has been accepted. Before that, the peer is a room
    /// member but not a Chocolate client: only the SYN/refusal and QUERY
    /// paths exist for it (pinned unknown-address handling).
    syn: bool,
    /// The reusable protocol slot, assigned at acceptance as the lowest
    /// free slot (upstream's first-inactive-slot rule). Slot order drives
    /// player numbering and the max_players reference; `None` pre-SYN.
    slot: Option<usize>,
    conn: Conn,
    name: Vec<u8>,
    addr_label: Vec<u8>,
    lowres_turn: bool,
    drone: bool,
    max_players: u8,
    is_freedoom: u8,
    wad_sha1: [u8; 20],
    deh_sha1: [u8; 20],
    player_class: u8,
    order: u64,
    ready: bool,
    reliable_send_seq: u8,
    reliable_recv_seq: u8,
    reliable_outbox: VecDeque<ReliableEntry>,
    last_recv: Milliseconds,
    last_send: Milliseconds,
    last_waitdata: Milliseconds,
    /// In-game state, initialized exactly once at game start and cleared
    /// on removal or game end so nothing stale reaches a later game.
    game: Option<PeerGame>,
    /// The frozen per-game player index, assigned once at game start and
    /// never renumbered while the game runs (pinned `sv_players[]`
    /// semantics). `None` for drones and outside a game.
    game_index: Option<usize>,
}

impl Peer {
    fn admitted(now: Milliseconds, order: u64, addr_label: Vec<u8>) -> Self {
        Peer {
            syn: false,
            slot: None,
            conn: Conn::Connected,
            name: Vec::new(),
            addr_label,
            lowres_turn: false,
            drone: false,
            max_players: NET_MAX_PLAYERS as u8,
            is_freedoom: 0,
            wad_sha1: [0; 20],
            deh_sha1: [0; 20],
            player_class: 0,
            order,
            ready: false,
            reliable_send_seq: 0,
            reliable_recv_seq: 0,
            reliable_outbox: VecDeque::new(),
            last_recv: now,
            last_send: now,
            last_waitdata: now,
            game: None,
            game_index: None,
        }
    }

    /// The pinned `ClientConnected`: protocol client that is not in the
    /// process of disconnecting.
    fn connected(&self) -> bool {
        self.syn && matches!(self.conn, Conn::Connected)
    }

    /// A connected non-drone player.
    fn is_player(&self) -> bool {
        self.connected() && !self.drone
    }
}

/// The deterministic server role. One instance per named room.
#[derive(Debug)]
pub struct ServerRole {
    state: ServerState,
    gamemode: Option<u8>,
    gamemission: Option<u8>,
    settings: Option<GameSettings>,
    peers: BTreeMap<PlayerId, Peer>,
    next_order: u64,
    /// The time of the input currently being handled.
    clock: Milliseconds,
    /// The shared receive window, present only in game.
    recv: Option<RecvWindow>,
}

impl Default for ServerRole {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerRole {
    /// An empty room waiting for launch.
    pub fn new() -> Self {
        Self {
            state: ServerState::WaitingLaunch,
            gamemode: None,
            gamemission: None,
            settings: None,
            peers: BTreeMap::new(),
            next_order: 0,
            clock: Milliseconds(0),
            recv: None,
        }
    }

    /// The room-level state.
    pub fn state(&self) -> ServerState {
        self.state
    }

    /// The room's authoritative `lowres_turn` codec context: wide until
    /// the controller's settings are adopted at GAMESTART, then their
    /// negotiated value. The binding decodes and encodes every GAMEDATA
    /// family packet with exactly this value; it must never infer width
    /// from payload length or a client-local flag.
    pub fn lowres_turn(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|settings| settings.lowres_turn != 0)
    }

    /// Number of admitted peers (players plus drones).
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Number of connected non-drone players.
    pub fn player_count(&self) -> usize {
        self.peers.values().filter(|peer| peer.is_player()).count()
    }

    /// The oldest connected non-drone peer, recomputed on demand (the
    /// upstream controller rule).
    pub fn controller(&self) -> Option<PlayerId> {
        self.peers
            .iter()
            .filter(|(_, peer)| peer.is_player())
            .min_by_key(|(_, peer)| peer.order)
            .map(|(player, _)| *player)
    }

    /// Handle one input against the caller's clock.
    pub fn handle(&mut self, now: Milliseconds, input: Input) -> Vec<Action> {
        self.clock = now;
        match input {
            Input::Join { player, addr_label } => self.on_join(player, addr_label, now),
            Input::Leave { player } => self.on_leave(player),
            Input::Packet {
                player,
                header,
                packet,
            } => self.on_packet(player, header, packet, now),
            Input::Malformed { player, class } => self.on_malformed(player, class),
            Input::Timer => self.on_timer(now),
        }
    }

    // --- admission and removal -------------------------------------------

    fn on_join(&mut self, player: PlayerId, addr_label: Vec<u8>, now: Milliseconds) -> Vec<Action> {
        // A duplicate admission must not replace live protocol state.
        if self.peers.contains_key(&player) {
            return Vec::new();
        }
        // Labels must encode as bounded wire strings; an unencodable
        // label is rejected here rather than surfacing inside an action.
        if addr_label.contains(&0) {
            return vec![Action::Disconnect {
                player,
                reason: DisconnectReason::MalformedInput,
                terminal: None,
            }];
        }
        let mut label = addr_label;
        label.truncate(MAX_NAME_LEN - 1);
        self.next_order += 1;
        self.peers
            .insert(player, Peer::admitted(now, self.next_order, label));
        Vec::new()
    }

    fn on_leave(&mut self, player: PlayerId) -> Vec<Action> {
        let Some(peer) = self.peers.get(&player) else {
            return Vec::new();
        };
        // A member that never completed SYN is not a protocol client: its
        // removal carries no abort or game-end effects.
        if !peer.syn {
            return self.remove_unconnected(player, DisconnectReason::Remote, None);
        }
        self.remove_connected(player, DisconnectReason::Remote, None, None)
    }

    /// Remove a member that never completed SYN. Pinned
    /// `NET_SV_SendReject` handling: the address never becomes an active
    /// client, so no abort, zero-player, broadcast, or game-end path can
    /// fire for it, and no other admitted member is touched.
    fn remove_unconnected(
        &mut self,
        player: PlayerId,
        reason: DisconnectReason,
        terminal: Option<Box<(WireHeader, ServerPacket)>>,
    ) -> Vec<Action> {
        self.peers.remove(&player);
        vec![Action::Disconnect {
            player,
            reason,
            terminal,
        }]
    }

    /// The single removal path for protocol-connected peers.
    /// `cause_message` is a console broadcast to the remaining connected
    /// peers (the timeout broadcast); `terminal` is the final packet owed
    /// to the removed peer itself.
    fn remove_connected(
        &mut self,
        player: PlayerId,
        reason: DisconnectReason,
        cause_message: Option<Vec<u8>>,
        terminal: Option<Box<(WireHeader, ServerPacket)>>,
    ) -> Vec<Action> {
        let Some(peer) = self.peers.remove(&player) else {
            return Vec::new();
        };

        let mut actions = vec![Action::Disconnect {
            player,
            reason,
            terminal,
        }];
        if let Some(message) = cause_message {
            self.broadcast_console(&mut actions, message);
        }

        if self.state == ServerState::WaitingStart && peer.is_player() {
            // A non-drone loss while waiting for GAMESTART aborts startup
            // (pinned behavior): the broadcast goes out first, then the
            // game ends and every survivor is disconnected.
            let mut message = b"Game startup aborted because player '".to_vec();
            message.extend_from_slice(&peer.name);
            message.extend_from_slice(b"' disconnected.");
            self.broadcast_console(&mut actions, message);
            actions.extend(self.end_game());
        } else if self.player_count() == 0 {
            // No players remain: the room ends and every survivor is
            // disconnected (pinned NET_SV_GameEnded, which fires at any
            // room state once no players are left).
            actions.extend(self.end_game());
        }

        actions
    }

    /// Pinned `NET_SV_GameEnded`: reset to waiting-for-launch and
    /// disconnect EVERY remaining client. Protocol-connected survivors
    /// get the initiated-disconnect lifecycle; members that never
    /// completed SYN are removed silently.
    fn end_game(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        self.state = ServerState::WaitingLaunch;
        self.gamemode = None;
        self.settings = None;
        self.recv = None;
        for peer in self.peers.values_mut() {
            peer.game = None;
            peer.game_index = None;
        }

        let survivors: Vec<PlayerId> = self.peers.keys().copied().collect();
        for survivor in survivors {
            let Some(peer) = self.peers.get(&survivor) else {
                continue;
            };
            match (peer.syn, peer.conn) {
                // Protocol-connected survivors get the initiated
                // disconnect lifecycle.
                (true, Conn::Connected) => {
                    self.start_disconnect(&mut actions, survivor, DisconnectReason::GameEnded);
                }
                // Members that never completed SYN are removed silently.
                (false, Conn::Connected) => {
                    self.peers.remove(&survivor);
                    actions.push(Action::Disconnect {
                        player: survivor,
                        reason: DisconnectReason::GameEnded,
                        terminal: None,
                    });
                }
                // Already disconnecting or asleep: the lifecycle in
                // progress is left alone (upstream's repeated GameEnded
                // calls never restart it either).
                _ => {}
            }
        }
        actions.push(Action::GameEnded);
        actions
    }

    // --- packet handling ---------------------------------------------------

    fn on_packet(
        &mut self,
        player: PlayerId,
        header: WireHeader,
        packet: ClientPacket,
        now: Milliseconds,
    ) -> Vec<Action> {
        let Some(peer) = self.peers.get_mut(&player) else {
            return Vec::new();
        };
        peer.last_recv = now;

        // Pinned unknown-address handling: before a valid SYN there is no
        // protocol client, so only the SYN/refusal path and QUERY exist.
        if !peer.syn {
            return match packet {
                ClientPacket::Syn(syn) => self.on_syn(player, syn),
                ClientPacket::Query => {
                    let packet = self.query_response();
                    vec![self.emit(player, plain(), packet)]
                }
                _ => Vec::new(),
            };
        }

        // A peer we are disconnecting hears only its own lifecycle: the
        // final ACK completes removal; everything else is dropped.
        let disconnecting = self
            .peers
            .get(&player)
            .is_some_and(|peer| matches!(peer.conn, Conn::Disconnecting { .. }));
        if disconnecting {
            return match packet {
                ClientPacket::DisconnectAck => {
                    self.remove_connected(player, DisconnectReason::Remote, None, None)
                }
                _ => Vec::new(),
            };
        }

        // A remotely disconnected peer is asleep: a duplicate DISCONNECT
        // is re-acknowledged (the first ACK may have been lost), nothing
        // else is processed.
        let sleeping = self
            .peers
            .get(&player)
            .is_some_and(|peer| matches!(peer.conn, Conn::Sleeping { .. }));
        if sleeping {
            return match packet {
                ClientPacket::Disconnect => {
                    vec![self.emit(player, plain(), ServerPacket::DisconnectAck)]
                }
                _ => Vec::new(),
            };
        }

        let mut actions = Vec::new();

        // Reliable receive rule (upstream): acknowledge with the current
        // next-expected sequence regardless of acceptance, then process
        // only an exact in-sequence packet.
        if let Some(seq) = header.reliable_seq {
            let (ack_value, in_sequence) = {
                let peer = self.peers.get_mut(&player).expect("peer checked above");
                let expected = peer.reliable_recv_seq;
                if seq == expected {
                    peer.reliable_recv_seq = expected.wrapping_add(1);
                    (peer.reliable_recv_seq, true)
                } else {
                    (expected, false)
                }
            };
            let packet = ServerPacket::ReliableAck {
                next_seq: ack_value,
            };
            actions.push(self.emit(player, plain(), packet));
            if !in_sequence {
                return actions;
            }
        }

        match packet {
            ClientPacket::Syn(_) => {
                // Duplicate SYN from an established peer: drop.
            }
            ClientPacket::Keepalive => {}
            ClientPacket::Launch => {
                if self.state == ServerState::WaitingLaunch && self.controller() == Some(player) {
                    self.state = ServerState::WaitingStart;
                    let num_players = self.player_count() as u8;
                    for target in self.connected_peers() {
                        self.enqueue_reliable(
                            &mut actions,
                            target,
                            ServerPacket::Launch { num_players },
                        );
                    }
                }
            }
            ClientPacket::GameStart(settings) => {
                if self.state == ServerState::WaitingStart {
                    actions.extend(self.on_gamestart(player, settings));
                }
            }
            ClientPacket::Disconnect => {
                actions.push(self.emit(player, plain(), ServerPacket::DisconnectAck));
                if let Some(peer) = self.peers.get_mut(&player) {
                    peer.conn = Conn::Sleeping {
                        until: Milliseconds(now.0 + SLEEP_MS),
                    };
                }
            }
            ClientPacket::DisconnectAck => {
                // An ACK we were not waiting for is ignored (upstream's
                // disconnect-ack only completes a local disconnect).
            }
            ClientPacket::ReliableAck { next_seq } => {
                actions.extend(self.on_reliable_ack(player, next_seq));
            }
            ClientPacket::Query => {
                let packet = self.query_response();
                actions.push(self.emit(player, plain(), packet));
            }
            ClientPacket::GameData(data) => {
                actions.extend(self.on_upload(player, data, now));
            }
            ClientPacket::GameDataAck { ack } => {
                self.on_gamedata_ack(player, ack);
            }
            ClientPacket::GameDataResend { start, count } => {
                actions.extend(self.on_resend_request(player, start, count));
            }
        }

        actions
    }

    fn on_syn(&mut self, player: PlayerId, syn: doom_proto::Syn) -> Vec<Action> {
        let mut actions = Vec::new();

        // Protocol negotiation: the only protocol this role speaks. The
        // rejection keeps the pinned shape, including the client-reported
        // version after validation, bounded for the wire.
        if !syn
            .protocols
            .iter()
            .any(|name| name.as_slice() == PROTOCOL_NAME)
        {
            let mut reason = b"Version mismatch: server version is: ".to_vec();
            reason.extend_from_slice(SERVER_VERSION);
            reason.extend_from_slice(b"; client is: ");
            reason.extend_from_slice(&syn.version);
            reason.extend_from_slice(b". No common compatible protocol could be negotiated.");
            reason.truncate(256);
            self.reject(&mut actions, player, reason);
            return actions;
        }

        // Connect data validity (upstream drops invalid data silently).
        if !valid_game_mode(syn.connect.gamemission, syn.connect.gamemode)
            || syn.connect.max_players as usize > NET_MAX_PLAYERS
        {
            return actions;
        }

        if self.state != ServerState::WaitingLaunch {
            self.reject(
                &mut actions,
                player,
                b"Server is not currently accepting connections".to_vec(),
            );
            return actions;
        }

        // Capacity: players against the game's cap, everyone against the
        // protocol slot ceiling.
        if (syn.connect.drone == 0 && self.player_count() >= self.max_players())
            || self.peers.len() >= MAX_NODES
        {
            self.reject(&mut actions, player, b"Server is full!".to_vec());
            return actions;
        }

        // The first non-drone client's mode/mission is adopted; later
        // clients must match it exactly. Upstream quirk: a drone arriving
        // before any player faces the still-indeterminate mode and is
        // rejected with "Game mismatch".
        if self.gamemode.is_none() && syn.connect.drone == 0 && self.player_count() == 0 {
            self.gamemode = Some(syn.connect.gamemode);
            self.gamemission = Some(syn.connect.gamemission);
        }
        let adopted = (
            self.gamemode.unwrap_or(MODE_INDETERMINED),
            self.gamemission.unwrap_or(0),
        );
        if adopted != (syn.connect.gamemode, syn.connect.gamemission) {
            let reason = format!(
                "Game mismatch: server is {} ({}), client is {} ({})",
                mission_name(adopted.1),
                mode_name(adopted.0),
                mission_name(syn.connect.gamemission),
                mode_name(syn.connect.gamemode),
            );
            self.reject(&mut actions, player, reason.into_bytes());
            return actions;
        }

        // Accept, taking the lowest free protocol slot (upstream's
        // first-inactive-slot rule; a slot frees for reuse on removal).
        let slot = self.free_slot();
        {
            let peer = self
                .peers
                .get_mut(&player)
                .expect("the peer was admitted by the room host");
            peer.syn = true;
            peer.slot = Some(slot);
            peer.name = {
                let mut name = syn.player_name;
                name.truncate(MAX_NAME_LEN - 1);
                name
            };
            peer.lowres_turn = syn.connect.lowres_turn != 0;
            peer.drone = syn.connect.drone != 0;
            peer.max_players = syn.connect.max_players;
            peer.is_freedoom = syn.connect.is_freedoom;
            peer.wad_sha1 = syn.connect.wad_sha1;
            peer.deh_sha1 = syn.connect.deh_sha1;
            peer.player_class = syn.connect.player_class;
        }

        self.enqueue_reliable(
            &mut actions,
            player,
            ServerPacket::SynAccept(doom_proto::SynAccept {
                version: SERVER_VERSION.to_vec(),
                protocol: PROTOCOL_NAME.to_vec(),
            }),
        );

        // Pinned first-update behavior: the new client's last_send_time
        // starts unset, so its first WAITING_DATA leaves in the same
        // server run as the accept, right after it.
        let data = self.waiting_data(player);
        actions.push(self.emit(player, plain(), ServerPacket::WaitingData(data)));
        if let Some(peer) = self.peers.get_mut(&player) {
            peer.last_waitdata = self.clock;
        }

        actions
    }

    fn on_gamestart(&mut self, player: PlayerId, settings: GameSettings) -> Vec<Action> {
        let mut actions = Vec::new();

        if self.controller() == Some(player) {
            let mode = self.gamemode.unwrap_or(MODE_INDETERMINED);
            let mission = self.gamemission.unwrap_or(0);
            if !valid_game_settings(mode, mission, &settings) {
                // Invalid settings: no adoption, no readiness (upstream).
                return actions;
            }
            self.settings = Some(settings);
        }

        if let Some(peer) = self.peers.get_mut(&player) {
            peer.ready = true;
        }

        let all_ready = !self.peers.is_empty()
            && self
                .peers
                .values()
                .filter(|peer| peer.connected())
                .all(|peer| peer.ready);
        if all_ready && self.peers.values().any(|peer| peer.connected()) {
            return self.start_game();
        }

        // Refresh ready peers with the current lobby state (upstream's
        // SendAllWaitingData on readiness).
        for target in self
            .peers
            .iter()
            .filter(|(_, peer)| peer.connected() && peer.ready)
            .map(|(player, _)| *player)
            .collect::<Vec<_>>()
        {
            let data = self.waiting_data(target);
            actions.push(self.emit(target, plain(), ServerPacket::WaitingData(data)));
        }
        actions
    }

    fn start_game(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(mut settings) = self.settings.clone() else {
            // Everyone is ready but the controller never sent valid
            // settings: stay waiting (upstream cannot start either).
            return actions;
        };

        // lowres_turn comes from the players (sv_players), never drones.
        // The server's stored settings take the computed value, as
        // upstream's StartGame writes it before broadcasting.
        settings.lowres_turn = self
            .peers
            .values()
            .filter(|peer| peer.is_player())
            .any(|peer| peer.lowres_turn) as u8;
        settings.player_classes = self
            .established_players()
            .iter()
            .map(|player| {
                self.peers
                    .get(player)
                    .expect("listed peer exists")
                    .player_class
            })
            .collect();
        self.settings = Some(settings.clone());

        for target in self.connected_peers() {
            let mut per_recipient = settings.clone();
            per_recipient.consoleplayer = self.player_index(target);
            self.enqueue_reliable(&mut actions, target, ServerPacket::GameStart(per_recipient));

            // Overflow is transactional: if the enqueue removed the
            // target or ended the room, the transition does not continue
            // and the state must not be overwritten back to InGame.
            if !self.peers.contains_key(&target) || self.state != ServerState::WaitingStart {
                return actions;
            }
        }

        // Every window, acknowledgement, send sequence, and game-data
        // clock initializes exactly once here, and every connected
        // non-drone player gets its frozen index for this game, in slot
        // order. Later removals clear participation only and must not
        // renumber survivors: holes are preserved.
        let now = self.clock;
        self.recv = Some(RecvWindow::new(0));
        let players = self.established_players();
        for (_, peer) in self.peers.iter_mut().filter(|(_, peer)| peer.connected()) {
            peer.game = Some(PeerGame::new(now));
            peer.game_index = None;
        }
        for (index, player) in players.iter().enumerate() {
            if let Some(peer) = self.peers.get_mut(player) {
                peer.game_index = Some(index);
            }
        }

        self.state = ServerState::InGame;
        actions
    }

    // --- in-game receive, acknowledgement, and retransmission ------------

    /// The frozen per-game index of a connected player, or `None` for a
    /// drone (drones never occupy a receive slot because they never
    /// upload). Assigned once at game start and never renumbered.
    fn recv_player_index(&self, player: PlayerId) -> Option<usize> {
        let peer = self.peers.get(&player)?;
        peer.connected().then_some(peer.game_index)?
    }

    /// Pinned `NET_SV_ParseGameData`: accept uploads only in game and
    /// only from connected non-drone players. Expand both low bytes
    /// against the receive window; apply the acknowledgement
    /// independently, so an invalid one never blocks in-window tics from
    /// the same packet; store in-range tics only when the expanded start
    /// is non-negative; and request any newly revealed missing run
    /// behind the fresh data without duplicating a live request.
    fn on_upload(
        &mut self,
        player: PlayerId,
        data: doom_proto::GameDataClient,
        now: Milliseconds,
    ) -> Vec<Action> {
        let mut actions = Vec::new();

        if self.state != ServerState::InGame {
            return actions;
        }
        let Some(index) = self.recv_player_index(player) else {
            return actions;
        };
        let Some(recv) = self.recv.as_mut() else {
            return actions;
        };
        let Some(peer) = self.peers.get(&player) else {
            return actions;
        };
        if !peer.is_player() {
            return actions;
        }

        let ack_expanded = expand_tic(recv.start, data.ack);
        let start_expanded = expand_tic(recv.start, data.start);

        // The acknowledgement is validated on its own and applied first,
        // so an invalid one never blocks the in-window tics below.
        if let Some(game) = self
            .peers
            .get_mut(&player)
            .and_then(|peer| peer.game.as_mut())
        {
            apply_ack(game, ack_expanded);
        }

        // A negative expanded start is invalid for both insertion and
        // gap generation (a positive start beyond the window still flows
        // through the clamped gap scan below).
        if start_expanded < 0 {
            return actions;
        }

        for (offset, tic) in data.tics.iter().enumerate() {
            let slot = start_expanded + offset as i64 - recv.start as i64;
            if !(0..BACKUPTICS as i64).contains(&slot) {
                continue;
            }
            let slot = slot as usize;
            recv.entries[slot][index].active = true;
            recv.entries[slot][index].diff = tic.diff.clone();
            recv.entries[slot][index].latency = tic.latency;
            // The deadlock clock resets only when at least one tic is
            // actually stored (pinned semantics).
            if let Some(game) = self
                .peers
                .get_mut(&player)
                .and_then(|peer| peer.game.as_mut())
            {
                game.last_gamedata = now;
            }
        }

        // Missing-run discovery behind the new data: scan down for the
        // first unreceived, not-yet-requested entry and request the run.
        // Upstream clamps the scan to BACKUPTICS - 1, so the top slot of
        // the window is never part of a discovered run.
        let resend_end = (start_expanded - recv.start as i64).min(BACKUPTICS as i64 - 1);
        if resend_end > 0 {
            let mut run_start = resend_end;
            let mut slot = resend_end - 1;
            while slot >= 0 {
                let entry = &recv.entries[slot as usize][index];
                if entry.active || entry.resend_time.is_some() {
                    break;
                }
                run_start = slot;
                slot -= 1;
            }
            if run_start < resend_end {
                let recv = self.recv.as_mut().expect("window checked above");
                for stamp in run_start..resend_end {
                    recv.entries[stamp as usize][index].resend_time = Some(now);
                }
                let start = recv.start + run_start as u32;
                actions.push(self.emit(
                    player,
                    plain(),
                    ServerPacket::GameDataResend {
                        start,
                        count: (resend_end - run_start) as u8,
                    },
                ));
            }
        }

        actions
    }

    /// Pinned `NET_SV_ParseGameDataACK`: the same independently
    /// validated acknowledgement update, from any connected client
    /// including drones (pinned: drones acknowledge).
    fn on_gamedata_ack(&mut self, player: PlayerId, ack: u8) {
        if self.state != ServerState::InGame {
            return;
        }
        let Some(recv) = &self.recv else {
            return;
        };
        let ack_expanded = expand_tic(recv.start, ack);
        let Some(peer) = self.peers.get_mut(&player) else {
            return;
        };
        if !peer.connected() {
            return;
        }
        if let Some(game) = peer.game.as_mut() {
            apply_ack(game, ack_expanded);
        }
    }

    /// Pinned `NET_SV_ParseResendRequest`, atomic: the whole request is
    /// honored only if every requested absolute tic still matches its
    /// queued slot exactly; otherwise it is ignored as stale or spoofed.
    /// A zero count asks for nothing and is ignored (upstream emits an
    /// empty packet there; nothing meaningful travels, so we drop it
    /// instead, the one deliberate divergence here).
    fn on_resend_request(&mut self, player: PlayerId, start: u32, count: u8) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.state != ServerState::InGame || count == 0 {
            return actions;
        }
        let Some(peer) = self.peers.get(&player) else {
            return actions;
        };
        if !peer.connected() {
            return actions;
        }
        let Some(game) = &peer.game else {
            return actions;
        };

        let end = start as u64 + count as u64 - 1;
        for tic in start as u64..=end {
            match &game.sendqueue[(tic as usize) % BACKUPTICS] {
                Some(queued) if queued.seq as u64 == tic => {}
                _ => return actions,
            }
        }

        actions.extend(self.send_tics(player, start, end));
        actions
    }

    /// Emit a fan-out span from a peer's send queue, all or nothing:
    /// every requested entry must still match its queued absolute
    /// identity, otherwise the whole send is inert. Skipping an interior
    /// entry would compress the span under its old start and alias the
    /// tail onto the wrong absolute tics.
    fn send_tics(&mut self, player: PlayerId, start: u32, end: u64) -> Vec<Action> {
        let mut tics: Vec<doom_proto::FullTic> = Vec::new();
        let Some(peer) = self.peers.get(&player) else {
            return Vec::new();
        };
        let Some(game) = &peer.game else {
            return Vec::new();
        };
        for tic in start as u64..=end {
            match &game.sendqueue[(tic as usize) % BACKUPTICS] {
                Some(queued) if queued.seq as u64 == tic => tics.push(queued.tic.clone()),
                _ => return Vec::new(),
            }
        }

        let packet = ServerPacket::GameData(doom_proto::GameDataServer {
            start: (start & 0xff) as u8,
            tics,
        });
        vec![self.emit(player, plain(), packet)]
    }

    fn on_malformed(&mut self, player: PlayerId, class: MalformedClass) -> Vec<Action> {
        match class {
            MalformedClass::Syn { old_magic: true } => {
                // Pinned `NET_SV_ParseSYN` old-magic arm: REJECTED goes
                // out either way, and what happens to the peer depends on
                // whether it is an active client yet.
                match self.peers.get(&player).map(|peer| peer.syn) {
                    // An established peer is retained with a plain,
                    // non-terminal rejection; controller, room state,
                    // readiness, and every other member are untouched.
                    Some(true) => {
                        vec![self.emit(
                            player,
                            plain(),
                            ServerPacket::Rejected {
                                reason: old_client_reason(),
                            },
                        )]
                    }
                    // A pre-SYN member is removed with the terminal
                    // close, through the never-connected path.
                    Some(false) => {
                        let mut actions = Vec::new();
                        self.reject(&mut actions, player, old_client_reason());
                        actions
                    }
                    // Nothing is held for an unknown member.
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    // --- timers --------------------------------------------------------------

    fn on_timer(&mut self, now: Milliseconds) -> Vec<Action> {
        let mut actions = Vec::new();

        // Pinned NET_SV_Run walks the reusable clients[] array: the
        // timer visits every SYN-accepted peer in protocol-slot order,
        // and the role's effects are order-sensitive within one batch.
        for player in self.timer_peers() {
            let (conn, idle_recv, idle_send, idle_waitdata, connected, name) = {
                let Some(peer) = self.peers.get(&player) else {
                    continue;
                };
                (
                    peer.conn,
                    now.0.saturating_sub(peer.last_recv.0),
                    now.0.saturating_sub(peer.last_send.0),
                    now.0.saturating_sub(peer.last_waitdata.0),
                    peer.connected(),
                    peer.name.clone(),
                )
            };

            match conn {
                Conn::Connected => {}
                Conn::Disconnecting {
                    sends,
                    last,
                    reason,
                } => {
                    let idle = now.0.saturating_sub(last.0);
                    if idle > DISCONNECT_RETRY_MS {
                        if sends < DISCONNECT_SENDS {
                            if let Some(peer) = self.peers.get_mut(&player) {
                                peer.conn = Conn::Disconnecting {
                                    sends: sends + 1,
                                    last: now,
                                    reason,
                                };
                            }
                            let packet = ServerPacket::Disconnect;
                            actions.push(self.emit(player, plain(), packet));
                        } else {
                            actions.extend(self.remove_connected(player, reason, None, None));
                        }
                    }
                    // No keepalive, reliable retry, WAITING_DATA, or lobby
                    // broadcast while disconnecting (pinned).
                    continue;
                }
                Conn::Sleeping { until } => {
                    if now >= until {
                        actions.extend(self.remove_connected(
                            player,
                            DisconnectReason::Remote,
                            None,
                            None,
                        ));
                    }
                    continue;
                }
            }

            // 30 s of receive silence drops the peer and broadcasts to the
            // rest (pinned timeout behavior; nothing is sent to the dead
            // peer itself).
            if idle_recv > TIMEOUT_MS {
                let mut message = b"Client '".to_vec();
                message.extend_from_slice(&name);
                message.extend_from_slice(b"' timed out and disconnected");
                actions.extend(self.remove_connected(
                    player,
                    DisconnectReason::Timeout,
                    Some(message),
                    None,
                ));
                continue;
            }

            // 1 s of send silence emits a bare keepalive.
            if idle_send > KEEPALIVE_MS {
                actions.push(self.emit(player, plain(), ServerPacket::Keepalive));
            }

            // Reliable head retry, in place; the queue never grows here.
            let retry = {
                let Some(peer) = self.peers.get_mut(&player) else {
                    continue;
                };
                match peer.reliable_outbox.front() {
                    Some(front) => match front.last_retry {
                        Some(last) if now.0.saturating_sub(last.0) > RELIABLE_RETRY_MS => {
                            Some((front.seq, front.packet.clone()))
                        }
                        _ => None,
                    },
                    None => None,
                }
            };
            if let Some((seq, packet)) = retry {
                if let Some(peer) = self.peers.get_mut(&player)
                    && let Some(front) = peer.reliable_outbox.front_mut()
                {
                    front.last_retry = Some(now);
                }
                actions.push(self.emit(
                    player,
                    WireHeader {
                        reliable_seq: Some(seq),
                    },
                    packet,
                ));
            }

            // Lobby cadence while waiting for launch.
            if connected && self.state == ServerState::WaitingLaunch && idle_waitdata > WAITDATA_MS
            {
                let data = self.waiting_data(player);
                actions.push(self.emit(player, plain(), ServerPacket::WaitingData(data)));
                if let Some(peer) = self.peers.get_mut(&player) {
                    peer.last_waitdata = now;
                }
            }

            // In-game per-client work (pinned RunClient in game).
            if connected && self.state == ServerState::InGame {
                actions.extend(self.pump(player));
                actions.extend(self.check_deadlock(player));
            }
        }

        // State-level in-game work (pinned NET_SV_Run): advance the
        // receive window, then re-request expired missing runs per player.
        if self.state == ServerState::InGame {
            self.advance_window();
            for player in self.established_players() {
                actions.extend(self.check_resends(player));
            }
        }

        actions
    }

    /// Pinned `NET_SV_LatestAcknowledged`: the minimum acknowledgement
    /// across connected clients, drones included.
    fn latest_acknowledged(&self) -> Option<i64> {
        self.peers
            .values()
            .filter(|peer| peer.connected())
            .filter_map(|peer| peer.game.as_ref().map(|game| game.acknowledged as i64))
            .min()
    }

    /// Pinned `NET_SV_AdvanceWindow`: advance only up to the minimum
    /// acknowledgement and only while the first tic is complete for
    /// every connected non-drone player. Disconnected or draining peers
    /// never hold advancement; connected drones hold the minimum
    /// acknowledgement but contribute no receive completeness columns.
    /// Absolute identity is preserved by construction while shifting.
    fn advance_window(&mut self) {
        let Some(min_ack) = self.latest_acknowledged() else {
            return;
        };
        let players = self.established_players();
        if players.is_empty() {
            return;
        }
        let indices: Vec<usize> = players
            .iter()
            .filter_map(|player| self.peers.get(player)?.game_index)
            .collect();
        let Some(recv) = self.recv.as_mut() else {
            return;
        };

        while (recv.start as i64) < min_ack {
            let complete = indices.iter().all(|index| recv.entries[0][*index].active);
            if !complete {
                break;
            }
            for slot in 0..BACKUPTICS - 1 {
                recv.entries[slot] = recv.entries[slot + 1].clone();
            }
            recv.entries[BACKUPTICS - 1] = Default::default();
            recv.start += 1;
        }
    }

    /// Pinned `NET_SV_PumpSendQueue`: pump each connected client
    /// independently. Skip when more than 40 tics ahead of the minimum
    /// acknowledgement; require the current tic from every other
    /// connected player; exclude the recipient's own command; merge
    /// active diffs in stable ascending order with the maximum latency;
    /// emit covering `sendseq - extratics .. sendseq` clamped at zero;
    /// then advance `sendseq`. The pinned single-player limit allows at
    /// most 10 tics ahead of the window. Drones receive full fan-out
    /// with no exclusion, since they hold no receive slot.
    fn pump(&mut self, player: PlayerId) -> Vec<Action> {
        let mut actions = Vec::new();

        let Some(min_ack) = self.latest_acknowledged() else {
            return actions;
        };
        let Some(recv) = &self.recv else {
            return actions;
        };
        let Some(peer) = self.peers.get(&player) else {
            return actions;
        };
        let Some(game) = &peer.game else {
            return actions;
        };

        if game.sendseq as i64 - min_ack > STALL_TICS {
            return actions;
        }

        let recv_index = game.sendseq as i64 - recv.start as i64;
        if !(0..BACKUPTICS as i64).contains(&recv_index) {
            return actions;
        }
        let recv_index = recv_index as usize;

        // Frozen per-game indices: removal never renumbers the columns.
        let players: Vec<(usize, PlayerId)> = self
            .established_players()
            .into_iter()
            .filter_map(|candidate| {
                self.peers
                    .get(&candidate)
                    .and_then(|peer| peer.game_index)
                    .map(|index| (index, candidate))
            })
            .collect();
        let recipient_index = players
            .iter()
            .find(|(_, candidate)| *candidate == player)
            .map(|(index, _)| *index);

        // The current tic must be present from every other connected
        // player (the recipient relies on its own command already).
        let mut others = 0usize;
        for (index, _) in &players {
            if Some(*index) == recipient_index {
                continue;
            }
            if !recv.entries[recv_index][*index].active {
                return actions;
            }
            others += 1;
        }

        if others == 0 && game.sendseq as i64 > recv.start as i64 + SINGLE_PLAYER_AHEAD {
            return actions;
        }

        // Merge: every active player's diff, ascending frozen index, max
        // latency. The playeringame bits and the ascending order are
        // frozen for the life of the game.
        let mut merged = doom_proto::FullTic {
            latency: 0,
            players: Vec::new(),
        };
        for (index, _) in &players {
            if Some(*index) == recipient_index {
                continue;
            }
            let entry = &recv.entries[recv_index][*index];
            if !entry.active {
                continue;
            }
            merged.latency = merged.latency.max(entry.latency);
            merged.players.push((*index as u8, entry.diff.clone()));
        }

        let sendseq = game.sendseq;
        let Some(game) = self
            .peers
            .get_mut(&player)
            .and_then(|peer| peer.game.as_mut())
        else {
            return actions;
        };
        game.sendqueue[(sendseq as usize) % BACKUPTICS] = Some(QueuedTic {
            seq: sendseq,
            tic: merged,
        });

        let settings = self.settings.clone().unwrap_or_else(|| GameSettings {
            ticdup: 1,
            extratics: 0,
            deathmatch: 0,
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
            player_classes: Vec::new(),
        });
        // extratics is validated below BACKUPTICS at game start, so the
        // whole span is always inside the send queue (and far below the
        // codec's 255-count bound).
        let span = settings.extratics as u32;
        let start = sendseq.saturating_sub(span);
        actions.extend(self.send_tics(player, start, sendseq as u64));

        if let Some(game) = self
            .peers
            .get_mut(&player)
            .and_then(|peer| peer.game.as_mut())
        {
            game.sendseq = game.sendseq.wrapping_add(1);
        }

        actions
    }

    /// Pinned `NET_SV_CheckResends`: contiguous runs of missing entries
    /// whose last request is strictly more than 300 ms old are
    /// re-requested and re-stamped. Requests never duplicate a live one.
    fn check_resends(&mut self, player: PlayerId) -> Vec<Action> {
        let mut actions = Vec::new();
        let now = self.clock;
        let Some(index) = self.recv_player_index(player) else {
            return actions;
        };
        // Collect expired contiguous runs first; emitting borrows again.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        {
            let Some(recv) = self.recv.as_mut() else {
                return actions;
            };
            let mut run_start: Option<usize> = None;
            for slot in 0..BACKUPTICS {
                let entry = &recv.entries[slot][index];
                let expired = !entry.active
                    && entry
                        .resend_time
                        .is_some_and(|time| now.0 > time.0 + RESEND_AFTER_MS);
                if expired {
                    if run_start.is_none() {
                        run_start = Some(slot);
                    }
                } else if let Some(start) = run_start.take() {
                    runs.push((start, slot - 1));
                }
            }
            if let Some(start) = run_start.take() {
                runs.push((start, BACKUPTICS - 1));
            }
        }
        for (start, end) in runs {
            actions.extend(self.emit_resend(player, index, start, end));
        }

        actions
    }

    /// Emit one bounded resend request and stamp only the in-range
    /// missing entries it covers.
    fn emit_resend(
        &mut self,
        player: PlayerId,
        index: usize,
        start_slot: usize,
        end_slot: usize,
    ) -> Vec<Action> {
        let now = self.clock;
        let Some(recv) = self.recv.as_mut() else {
            return Vec::new();
        };
        for slot in start_slot..=end_slot {
            recv.entries[slot][index].resend_time = Some(now);
        }
        let start = recv.start + start_slot as u32;
        vec![self.emit(
            player,
            plain(),
            ServerPacket::GameDataResend {
                start,
                count: (end_slot - start_slot + 1) as u8,
            },
        )]
    }

    /// Pinned deadlock request: the wire always asks for the first
    /// missing tic plus five (six tics), even when that interval extends
    /// past the window tail; only the local stamps are bounded.
    fn emit_deadlock_resend(
        &mut self,
        player: PlayerId,
        index: usize,
        first: usize,
    ) -> Vec<Action> {
        let now = self.clock;
        let Some(recv) = self.recv.as_mut() else {
            return Vec::new();
        };
        for slot in first..=(first + 5).min(BACKUPTICS - 1) {
            recv.entries[slot][index].resend_time = Some(now);
        }
        let start = recv.start + first as u32;
        vec![self.emit(
            player,
            plain(),
            ServerPacket::GameDataResend { start, count: 6 },
        )]
    }

    /// Pinned `NET_SV_CheckDeadlock`: strictly more than 1000 ms without
    /// accepted in-range game data from a connected non-drone player
    /// triggers a resend request for the first missing tic plus five and
    /// a replay of that client's exact unacknowledged send queue. The
    /// clock resets only on the triggering event, so recovery cannot
    /// amplify. Drones are excluded, as pinned.
    fn check_deadlock(&mut self, player: PlayerId) -> Vec<Action> {
        let mut actions = Vec::new();
        let now = self.clock;

        let Some(index) = self.recv_player_index(player) else {
            return actions;
        };
        let Some(recv) = &self.recv else {
            return actions;
        };
        let Some(peer) = self.peers.get(&player) else {
            return actions;
        };
        if !peer.is_player() {
            return actions;
        }
        let Some(game) = &peer.game else {
            return actions;
        };
        if now.0.saturating_sub(game.last_gamedata.0) <= DEADLOCK_MS {
            return actions;
        }

        // The first missing tic for this player, plus five: the pinned
        // wire request always covers six tics; only the local stamps are
        // bounded to the window.
        let missing = (0..BACKUPTICS).find(|slot| !recv.entries[*slot][index].active);
        let Some(first) = missing else {
            return actions;
        };
        actions.extend(self.emit_deadlock_resend(player, index, first));

        // Replay the client's exact unacknowledged queue.
        let (acknowledged, sendseq) = {
            let Some(game) = self.peers.get(&player).and_then(|peer| peer.game.as_ref()) else {
                return actions;
            };
            (game.acknowledged, game.sendseq)
        };
        if sendseq > acknowledged {
            actions.extend(self.send_tics(player, acknowledged, sendseq as u64 - 1));
        }

        if let Some(game) = self
            .peers
            .get_mut(&player)
            .and_then(|peer| peer.game.as_mut())
        {
            game.last_gamedata = now;
        }

        actions
    }

    // --- reliable mechanics --------------------------------------------------

    /// Queue a reliable packet. Only the head is ever emitted: an empty
    /// FIFO emits the new head in the same pump; an enqueue behind an
    /// unacknowledged head emits nothing. The 65th enqueue removes the
    /// peer without allocating or emitting to it.
    fn enqueue_reliable(
        &mut self,
        actions: &mut Vec<Action>,
        player: PlayerId,
        packet: ServerPacket,
    ) {
        let Some(peer) = self.peers.get_mut(&player) else {
            return;
        };

        if peer.reliable_outbox.len() >= RELIABLE_CAP {
            let name = peer.name.clone();
            let mut message = b"Client '".to_vec();
            message.extend_from_slice(&name);
            message.extend_from_slice(b"' disconnected");
            actions.extend(self.remove_connected(
                player,
                DisconnectReason::ReliableOverflow,
                Some(message),
                None,
            ));
            return;
        }

        let empty = peer.reliable_outbox.is_empty();
        let seq = peer.reliable_send_seq;
        peer.reliable_send_seq = peer.reliable_send_seq.wrapping_add(1);
        peer.reliable_outbox.push_back(ReliableEntry {
            seq,
            packet: packet.clone(),
            last_retry: None,
        });
        if empty {
            if let Some(front) = self
                .peers
                .get_mut(&player)
                .and_then(|peer| peer.reliable_outbox.front_mut())
            {
                front.last_retry = Some(self.clock);
            }
            actions.push(self.emit(
                player,
                WireHeader {
                    reliable_seq: Some(seq),
                },
                packet,
            ));
        }
    }

    /// Exact-head acknowledgement: pop the head and emit the next one
    /// immediately, with its retry clock starting now (pinned semantics).
    fn on_reliable_ack(&mut self, player: PlayerId, next_seq: u8) -> Vec<Action> {
        let mut actions = Vec::new();
        let Some(peer) = self.peers.get_mut(&player) else {
            return actions;
        };

        let Some(front) = peer.reliable_outbox.front() else {
            return actions;
        };
        if next_seq != front.seq.wrapping_add(1) {
            return actions;
        }
        peer.reliable_outbox.pop_front();

        let next = peer
            .reliable_outbox
            .front()
            .map(|front| (front.seq, front.packet.clone()));
        if let Some((seq, packet)) = next {
            if let Some(front) = peer.reliable_outbox.front_mut() {
                front.last_retry = Some(self.clock);
            }
            actions.push(self.emit(
                player,
                WireHeader {
                    reliable_seq: Some(seq),
                },
                packet,
            ));
        }
        actions
    }

    // --- helpers -----------------------------------------------------------

    /// Every actual send updates the peer's keepalive-send clock. All
    /// emission is centralized here so accounting cannot be forgotten,
    /// and every packet carries the codec width authoritative at this
    /// exact production point.
    fn emit(&mut self, player: PlayerId, header: WireHeader, packet: ServerPacket) -> Action {
        if let Some(peer) = self.peers.get_mut(&player) {
            peer.last_send = self.clock;
        }
        Action::Send {
            player,
            header,
            lowres: self.lowres_turn(),
            packet,
        }
    }

    /// The lowest protocol slot no SYN-accepted peer occupies.
    fn free_slot(&self) -> usize {
        (0..MAX_NODES)
            .find(|slot| !self.peers.values().any(|peer| peer.slot == Some(*slot)))
            .expect("SYN capacity is checked before acceptance")
    }

    /// Every SYN-accepted peer — Connected, Disconnecting, and Sleeping
    /// alike — in reusable protocol-slot order (pinned `NET_SV_Run`
    /// walking the `clients[]` array). Draining peers still carry timer
    /// work, so this is not `connected_peers()`; pre-SYN members hold
    /// no slot and stay timer-silent.
    fn timer_peers(&self) -> Vec<PlayerId> {
        let mut peers: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.syn)
            .map(|(player, peer)| (peer.slot.expect("SYN-accepted peers hold slots"), *player))
            .collect();
        peers.sort_unstable();
        peers.into_iter().map(|(_, player)| player).collect()
    }

    /// Every protocol-connected peer, players and drones, in slot order
    /// (upstream's slot-ordered assignments).
    fn connected_peers(&self) -> Vec<PlayerId> {
        let mut peers: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.connected())
            .map(|(player, peer)| (peer.slot.expect("connected peers hold slots"), *player))
            .collect();
        peers.sort_unstable();
        peers.into_iter().map(|(_, player)| player).collect()
    }

    /// Connected non-drone players in slot order.
    fn established_players(&self) -> Vec<PlayerId> {
        self.connected_peers()
            .into_iter()
            .filter(|player| self.peers.get(player).is_some_and(|peer| !peer.drone))
            .collect()
    }

    /// `NET_SV_MaxPlayers`: the value of the connected peer in the
    /// lowest occupied slot, which a disconnect frees for reuse.
    fn max_players(&self) -> usize {
        self.connected_peers()
            .first()
            .and_then(|player| self.peers.get(player))
            .map(|peer| peer.max_players as usize)
            .unwrap_or(NET_MAX_PLAYERS)
    }

    fn player_index(&self, player: PlayerId) -> i8 {
        self.established_players()
            .iter()
            .position(|candidate| *candidate == player)
            .map(|index| index as i8)
            .unwrap_or(-1)
    }

    fn waiting_data(&self, recipient: PlayerId) -> doom_proto::WaitData {
        let controller = self.controller();
        let checksums_peer = controller.or(Some(recipient));
        let (wad_sha1, deh_sha1, is_freedoom) = checksums_peer
            .and_then(|player| self.peers.get(&player))
            .map(|peer| (peer.wad_sha1, peer.deh_sha1, peer.is_freedoom))
            .unwrap_or(([0; 20], [0; 20], 0));

        doom_proto::WaitData {
            players: self
                .established_players()
                .iter()
                .map(|player| {
                    let peer = self.peers.get(player).expect("listed peer exists");
                    doom_proto::WaitPlayer {
                        name: peer.name.clone(),
                        addr: peer.addr_label.clone(),
                    }
                })
                .collect(),
            num_drones: self
                .peers
                .values()
                .filter(|peer| peer.connected() && peer.drone)
                .count() as u8,
            ready_players: self
                .peers
                .values()
                .filter(|peer| peer.is_player() && peer.ready)
                .count() as u8,
            max_players: self.max_players() as u8,
            is_controller: (controller == Some(recipient)) as u8,
            consoleplayer: self.player_index(recipient),
            wad_sha1,
            deh_sha1,
            is_freedoom,
        }
    }

    /// The stateless query answer for this room's current state. The
    /// binding answers a QUERY from an unmapped address with exactly
    /// this, without admitting a peer or allocating any identity.
    pub fn query_response(&self) -> ServerPacket {
        ServerPacket::QueryResponse(doom_proto::QueryData {
            version: SERVER_VERSION.to_vec(),
            server_state: match self.state {
                ServerState::WaitingLaunch => 0,
                ServerState::WaitingStart => 1,
                ServerState::InGame => 2,
            },
            num_players: self.player_count() as u8,
            max_players: self.max_players() as u8,
            gamemode: self.gamemode.unwrap_or(MODE_INDETERMINED),
            gamemission: self.gamemission.unwrap_or(0),
            description: SERVER_DESCRIPTION.to_vec(),
            protocols: Some(vec![PROTOCOL_NAME.to_vec()]),
        })
    }

    /// Reject a never-connected peer. Only the stranger is removed, with
    /// the terminal REJECTED riding the close atomically; no connected
    /// client abort, zero-player, broadcast, or game-end path runs, and
    /// no admitted bystander is affected (pinned `NET_SV_SendReject`).
    fn reject(&mut self, actions: &mut Vec<Action>, player: PlayerId, mut reason: Vec<u8>) {
        reason.truncate(256);
        let terminal = Box::new((
            WireHeader { reliable_seq: None },
            ServerPacket::Rejected { reason },
        ));
        actions.extend(self.remove_unconnected(player, DisconnectReason::Remote, Some(terminal)));
    }

    /// Begin an initiated DISCONNECT: five sends at one-second intervals
    /// whose clock starts at this actual first send, then a forced
    /// removal. The peer leaves every connected set immediately and gets
    /// no non-disconnect traffic while it drains.
    fn start_disconnect(
        &mut self,
        actions: &mut Vec<Action>,
        player: PlayerId,
        reason: DisconnectReason,
    ) {
        let packet = ServerPacket::Disconnect;
        actions.push(self.emit(player, plain(), packet));
        if let Some(peer) = self.peers.get_mut(&player) {
            peer.conn = Conn::Disconnecting {
                sends: 1,
                last: self.clock,
                reason,
            };
        }
    }

    fn broadcast_console(&mut self, actions: &mut Vec<Action>, message: Vec<u8>) {
        for target in self.connected_peers() {
            self.enqueue_reliable(
                actions,
                target,
                ServerPacket::ConsoleMessage {
                    message: message.clone(),
                },
            );
        }
    }
}

/// A plain (non-reliable) frame.
fn plain() -> WireHeader {
    WireHeader { reliable_seq: None }
}

/// The full source-shaped old-client reason, including the server
/// version sentence, bounded for the wire.
fn old_client_reason() -> Vec<u8> {
    let mut reason =
        b"You are using an old client version that is not supported by this server. This server is running "
            .to_vec();
    reason.extend_from_slice(SERVER_VERSION);
    reason.push(b'.');
    reason.truncate(256);
    reason
}

/// `D_ValidGameMode` (d_mode.c): valid mission/mode pairs.
fn valid_game_mode(mission: u8, mode: u8) -> bool {
    matches!(
        (mission, mode),
        (0, 0)
            | (0, 1)
            | (0, 3)
            | (1, 2)
            | (2, 2)
            | (3, 2)
            | (4, 3)
            | (5, 2)
            | (6, 0)
            | (6, 1)
            | (6, 3)
            | (7, 2)
            | (8, 2)
    )
}

/// `D_ValidEpisodeMap` (d_mode.c): per mission/mode, the max episode and
/// map, including the two Heretic exceptions (registered episode 4 is
/// E4M1 only; retail episode 6 is E6M1 through E6M3).
fn valid_episode_map(mission: u8, mode: u8, episode: u8, map: u8) -> bool {
    if mission == 6 {
        if mode == 3 && episode == 6 {
            return (1..=3).contains(&map);
        }
        if mode == 1 && episode == 4 {
            return map == 1;
        }
    }
    let Some((max_episode, max_map)) = (match (mission, mode) {
        (0, 0) => Some((1, 9)),
        (0, 1) => Some((3, 9)),
        (0, 3) => Some((4, 9)),
        (4, 3) => Some((1, 5)),
        (1, 2) | (2, 2) | (3, 2) | (5, 2) => Some((1, 32)),
        (6, 0) => Some((1, 9)),
        (6, 1) => Some((3, 9)),
        (6, 3) => Some((5, 9)),
        (7, 2) => Some((1, 60)),
        (8, 2) => Some((1, 34)),
        _ => None,
    }) else {
        return false;
    };
    episode >= 1 && episode <= max_episode && map >= 1 && map <= max_map
}

/// `D_ValidGameVersion` (d_mode.c): the doom family accepts its ten
/// versions; other missions accept only their own.
fn valid_game_version(mission: u8, version: u8) -> bool {
    match mission {
        0..=5 => version <= 10,
        6 => version == 11,
        7 => matches!(version, 12 | 13),
        8 => matches!(version, 14 | 15),
        _ => false,
    }
}

/// `NET_ValidGameSettings` (net_common.c).
fn valid_game_settings(mode: u8, mission: u8, settings: &GameSettings) -> bool {
    if settings.ticdup == 0 || settings.deathmatch > 2 {
        return false;
    }
    // Deliberate abuse-path deviation: upstream accepts any extratics
    // byte, but a span of BACKUPTICS or more cannot be replayed
    // atomically from the bounded send queue, so reject it here.
    if settings.extratics as usize >= BACKUPTICS {
        return false;
    }
    if !(-1..=4).contains(&settings.skill) {
        return false;
    }
    valid_game_version(mission, settings.gameversion)
        && valid_episode_map(mission, mode, settings.episode, settings.map)
}

fn mission_name(mission: u8) -> &'static str {
    match mission {
        0 => "doom",
        1 => "doom2",
        2 => "tnt",
        3 => "plutonia",
        4 => "hacx",
        5 => "chex",
        6 => "heretic",
        7 => "hexen",
        8 => "strife",
        _ => "unknown",
    }
}

fn mode_name(mode: u8) -> &'static str {
    match mode {
        0 => "shareware",
        1 => "registered",
        2 => "commercial",
        3 => "retail",
        _ => "indetermined",
    }
}

#[cfg(test)]
mod tests;
