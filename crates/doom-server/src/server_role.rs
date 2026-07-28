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
    /// Bytes that did not decode. The classification distinguishes a
    /// malformed SYN from an established peer's packet; `old_magic` marks
    /// the one malformed case with a source-backed REJECT (a pre-3.0
    /// client). Everything else is dropped by default.
    Malformed {
        player: PlayerId,
        class: MalformedClass,
    },
    /// Time advance. All timers run only from this input.
    Timer,
}

/// Malformed-byte classification supplied by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MalformedClass {
    Syn { old_magic: bool },
    Established,
}

/// What the role wants the room host to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Queue a fully framed server packet for one peer.
    Send {
        player: PlayerId,
        header: WireHeader,
        packet: ServerPacket,
    },
    /// Remove the peer from the room. `terminal` is the final packet the
    /// peer is owed (a REJECTED or an acknowledgement): the binding must
    /// deliver it before closing, because the role's state for the peer
    /// is already gone and no later send will ever be produced for it.
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

/// Per-peer protocol state.
#[derive(Clone, Debug)]
struct Peer {
    /// A valid SYN has been accepted. Before that, the peer is a room
    /// member but not a Chocolate client: only the SYN/refusal and QUERY
    /// paths exist for it (pinned unknown-address handling).
    syn: bool,
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
}

impl Peer {
    fn admitted(now: Milliseconds, order: u64, addr_label: Vec<u8>) -> Self {
        Peer {
            syn: false,
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
        }
    }

    /// The room-level state.
    pub fn state(&self) -> ServerState {
        self.state
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
            self.peers.remove(&player);
            return vec![Action::Disconnect {
                player,
                reason: DisconnectReason::Remote,
                terminal: None,
            }];
        }
        self.remove_connected(player, DisconnectReason::Remote, None, None)
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
            ClientPacket::GameData(_)
            | ClientPacket::GameDataAck { .. }
            | ClientPacket::GameDataResend { .. } => {
                // Tic windows are the next slice; accepted and ignored here.
            }
        }

        actions
    }

    fn on_syn(&mut self, player: PlayerId, syn: doom_proto::Syn) -> Vec<Action> {
        let mut actions = Vec::new();

        // Protocol negotiation: the only protocol this role speaks.
        if !syn
            .protocols
            .iter()
            .any(|name| name.as_slice() == PROTOCOL_NAME)
        {
            let mut reason = b"Version mismatch: server is: ".to_vec();
            reason.extend_from_slice(SERVER_VERSION);
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

        // Accept.
        {
            let peer = self
                .peers
                .get_mut(&player)
                .expect("the peer was admitted by the room host");
            peer.syn = true;
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
                .filter(|peer| peer.syn)
                .all(|peer| peer.ready);
        if all_ready && self.peers.values().any(|peer| peer.syn) {
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

        self.state = ServerState::InGame;
        actions
    }

    fn on_malformed(&mut self, player: PlayerId, class: MalformedClass) -> Vec<Action> {
        match class {
            MalformedClass::Syn { old_magic: true } => {
                let mut actions = Vec::new();
                self.reject(
                    &mut actions,
                    player,
                    b"You are using an old client version that is not supported by this server."
                        .to_vec(),
                );
                actions
            }
            _ => Vec::new(),
        }
    }

    // --- timers --------------------------------------------------------------

    fn on_timer(&mut self, now: Milliseconds) -> Vec<Action> {
        let mut actions = Vec::new();

        for player in self.peers.keys().copied().collect::<Vec<_>>() {
            let (syn, conn, idle_recv, idle_send, idle_waitdata, connected, name) = {
                let Some(peer) = self.peers.get(&player) else {
                    continue;
                };
                (
                    peer.syn,
                    peer.conn,
                    now.0.saturating_sub(peer.last_recv.0),
                    now.0.saturating_sub(peer.last_send.0),
                    now.0.saturating_sub(peer.last_waitdata.0),
                    peer.connected(),
                    peer.name.clone(),
                )
            };

            // Members that never completed SYN are binding-level: no
            // protocol timers exist for them.
            if !syn {
                continue;
            }

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
    /// emission is centralized here so accounting cannot be forgotten.
    fn emit(&mut self, player: PlayerId, header: WireHeader, packet: ServerPacket) -> Action {
        if let Some(peer) = self.peers.get_mut(&player) {
            peer.last_send = self.clock;
        }
        Action::Send {
            player,
            header,
            packet,
        }
    }

    /// Every protocol-connected peer, players and drones, in admit order.
    fn connected_peers(&self) -> Vec<PlayerId> {
        let mut peers: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.connected())
            .map(|(player, peer)| (peer.order, *player))
            .collect();
        peers.sort_unstable();
        peers.into_iter().map(|(_, player)| player).collect()
    }

    /// Connected non-drone players in admit order.
    fn established_players(&self) -> Vec<PlayerId> {
        self.connected_peers()
            .into_iter()
            .filter(|player| self.peers.get(player).is_some_and(|peer| !peer.drone))
            .collect()
    }

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
                .filter(|peer| peer.syn && peer.drone)
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

    fn query_response(&self) -> ServerPacket {
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

    fn reject(&mut self, actions: &mut Vec<Action>, player: PlayerId, mut reason: Vec<u8>) {
        reason.truncate(256);
        let terminal = Box::new((
            WireHeader { reliable_seq: None },
            ServerPacket::Rejected { reason },
        ));
        actions.extend(self.remove_connected(
            player,
            DisconnectReason::Remote,
            None,
            Some(terminal),
        ));
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
            | (4, 2)
            | (5, 3)
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
        (1, 2) | (2, 2) | (3, 2) | (4, 2) => Some((1, 32)),
        (5, 3) => Some((1, 5)),
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
