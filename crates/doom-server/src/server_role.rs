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
//! DISCONNECT retries, and a per-peer reliable FIFO cap of 64, the one
//! deliberate abuse-path deviation from upstream's unbounded list.

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
/// Silence after which a peer is dropped (CONNECTION_TIMEOUT_LEN).
const TIMEOUT_MS: u64 = 30_000;
/// Send-idle period after which a bare keepalive goes out.
const KEEPALIVE_MS: u64 = 1_000;
/// Age of the reliable head after which it is retried.
const RELIABLE_RETRY_MS: u64 = 1_000;
/// Lobby update cadence while waiting for launch.
const WAITDATA_MS: u64 = 1_000;
/// Initiated DISCONNECT sends before forcing removal (MAX_RETRIES).
const DISCONNECT_SENDS: u8 = 5;
const DISCONNECT_RETRY_MS: u64 = 1_000;

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
    /// transport metadata (at most 29 bytes) echoed back in lobby updates;
    /// the role never interprets it.
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
    /// Remove the peer from the room. The role has already cleared
    /// everything it holds for the peer.
    Disconnect {
        player: PlayerId,
        reason: DisconnectReason,
    },
    /// The room returns to waiting-for-launch (startup aborted or no
    /// players remain). Drone cleanup actions precede it.
    GameEnded,
}

/// Why the role removed a peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisconnectReason {
    Timeout,
    ReliableOverflow,
    Remote,
    GameEnded,
}

/// Room-level state (`net_server_state_t`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerState {
    WaitingLaunch,
    WaitingStart,
    InGame,
}

/// One reliable outbox entry. The head is emitted once, then retried in
/// place; retries never add entries.
#[derive(Clone, Debug, PartialEq)]
struct ReliableEntry {
    seq: u8,
    packet: ServerPacket,
    last_retry: Option<Milliseconds>,
}

/// Initiated-disconnect progress (`NET_CONN_STATE_DISCONNECTING`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DisconnectProgress {
    sends: u8,
    last: Milliseconds,
    reason: DisconnectReason,
}

/// Per-peer protocol state.
#[derive(Clone, Debug)]
struct Peer {
    established: bool,
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
    disconnecting: Option<DisconnectProgress>,
}

impl Peer {
    fn admitted(now: Milliseconds, order: u64, addr_label: Vec<u8>) -> Self {
        Peer {
            established: false,
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
            disconnecting: None,
        }
    }

    /// A SYN-accepted non-drone.
    fn is_player(&self) -> bool {
        self.established && !self.drone
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
    /// The time of the input currently being handled. Reliable first
    /// transmissions are emitted within the same pump, as upstream's
    /// same-iteration connection run does.
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

    /// Number of SYN-accepted non-drone players.
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
        self.clock = now;
        match input {
            Input::Join { player, addr_label } => self.on_join(player, addr_label, now),
            Input::Leave { player } => self.remove_peer(player, DisconnectReason::Remote, None),
            Input::Packet {
                player,
                header,
                packet,
            } => self.on_packet(player, header, packet, now),
            Input::Malformed { player, class } => self.on_malformed(player, class),
            Input::Timer => self.on_timer(now),
        }
    }

    fn on_join(&mut self, player: PlayerId, addr_label: Vec<u8>, now: Milliseconds) -> Vec<Action> {
        let mut label = addr_label;
        label.truncate(MAX_NAME_LEN - 1);
        self.next_order += 1;
        self.peers
            .insert(player, Peer::admitted(now, self.next_order, label));
        Vec::new()
    }

    /// The single removal path. `cause_message`, when present, is the
    /// console text broadcast to the remaining established peers before
    /// the abort rules run (upstream's timeout broadcast).
    fn remove_peer(
        &mut self,
        player: PlayerId,
        reason: DisconnectReason,
        cause_message: Option<Vec<u8>>,
    ) -> Vec<Action> {
        let Some(peer) = self.peers.remove(&player) else {
            return Vec::new();
        };

        let mut actions = vec![Action::Disconnect { player, reason }];
        if let Some(message) = cause_message {
            self.broadcast_console(&mut actions, message);
        }

        if self.state == ServerState::WaitingStart && peer.is_player() {
            // A non-drone loss while waiting for GAMESTART aborts startup
            // (pinned behavior): broadcast the abort, then end the game.
            let mut message = b"Game startup aborted because player '".to_vec();
            message.extend_from_slice(&peer.name);
            message.extend_from_slice(b"' disconnected.");
            self.broadcast_console(&mut actions, message);
            actions.extend(self.end_game());
        } else if self.player_count() == 0 {
            // No players remain: the room ends and leftover drones are
            // cleaned up (pinned behavior).
            actions.extend(self.end_game());
        }

        actions
    }

    fn end_game(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        self.state = ServerState::WaitingLaunch;
        self.gamemode = None;
        self.settings = None;

        let drones: Vec<PlayerId> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.established && peer.drone)
            .map(|(player, _)| *player)
            .collect();
        for drone in drones {
            self.start_disconnect(&mut actions, drone, DisconnectReason::GameEnded);
        }
        actions.push(Action::GameEnded);
        actions
    }

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
            actions.push(self.send_plain(
                player,
                ServerPacket::ReliableAck {
                    next_seq: ack_value,
                },
            ));
            if !in_sequence {
                return actions;
            }
        }

        match packet {
            ClientPacket::Syn(syn) => {
                if self.peers.get(&player).is_some_and(|peer| peer.established) {
                    // Duplicate SYN from an established peer: drop.
                    return actions;
                }
                actions.extend(self.on_syn(player, syn));
            }
            ClientPacket::Keepalive => {}
            ClientPacket::Launch => {
                if self.state == ServerState::WaitingLaunch && self.controller() == Some(player) {
                    self.state = ServerState::WaitingStart;
                    let num_players = self.player_count() as u8;
                    for target in self.established_peers() {
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
                actions.push(self.send_plain(player, ServerPacket::DisconnectAck));
                actions.extend(self.remove_peer(player, DisconnectReason::Remote, None));
            }
            ClientPacket::DisconnectAck => {
                if self
                    .peers
                    .get(&player)
                    .is_some_and(|peer| peer.disconnecting.is_some())
                {
                    actions.extend(self.remove_peer(player, DisconnectReason::Remote, None));
                }
            }
            ClientPacket::ReliableAck { next_seq } => {
                if let Some(peer) = self.peers.get_mut(&player)
                    && let Some(front) = peer.reliable_outbox.front()
                    && next_seq == front.seq.wrapping_add(1)
                {
                    peer.reliable_outbox.pop_front();
                }
            }
            ClientPacket::Query => {
                actions.push(self.send_plain(player, self.query_response()));
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
            peer.established = true;
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

        let mut out = Vec::new();
        self.enqueue_reliable(
            &mut out,
            player,
            ServerPacket::SynAccept(doom_proto::SynAccept {
                version: SERVER_VERSION.to_vec(),
                protocol: PROTOCOL_NAME.to_vec(),
            }),
        );
        actions.extend(out);
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

        if !self.peers.is_empty() && self.peers.values().all(|peer| peer.ready) {
            return self.start_game();
        }

        // Refresh ready peers with the current lobby state (upstream's
        // SendAllWaitingData on readiness).
        for target in self
            .peers
            .iter()
            .filter(|(_, peer)| peer.ready)
            .map(|(player, _)| *player)
            .collect::<Vec<_>>()
        {
            let data = self.waiting_data(target);
            actions.push(self.send_plain(target, ServerPacket::WaitingData(data)));
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

        settings.lowres_turn = self.peers.values().any(|peer| peer.lowres_turn) as u8;
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

        for target in self.established_peers() {
            let mut per_recipient = settings.clone();
            per_recipient.consoleplayer = self.player_index(target);
            self.enqueue_reliable(&mut actions, target, ServerPacket::GameStart(per_recipient));
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

    fn on_timer(&mut self, now: Milliseconds) -> Vec<Action> {
        let mut actions = Vec::new();

        for player in self.peers.keys().copied().collect::<Vec<_>>() {
            let Some(peer) = self.peers.get(&player) else {
                continue;
            };
            let idle_recv = now.0.saturating_sub(peer.last_recv.0);
            let idle_send = now.0.saturating_sub(peer.last_send.0);
            let idle_waitdata = now.0.saturating_sub(peer.last_waitdata.0);
            let established = peer.established;
            let name = peer.name.clone();

            // 30 s of receive silence drops the peer and broadcasts to the
            // rest (pinned timeout behavior; nothing is sent to the dead
            // peer itself).
            if idle_recv > TIMEOUT_MS {
                let mut message = b"Client '".to_vec();
                message.extend_from_slice(&name);
                message.extend_from_slice(b"' timed out and disconnected");
                actions.extend(self.remove_peer(player, DisconnectReason::Timeout, Some(message)));
                continue;
            }

            // 1 s of send silence emits a bare keepalive.
            if idle_send > KEEPALIVE_MS {
                actions.push(self.send_plain(player, ServerPacket::Keepalive));
                if let Some(peer) = self.peers.get_mut(&player) {
                    peer.last_send = now;
                }
            }

            // Reliable head: emit once, then retry in place.
            let retry = {
                let Some(peer) = self.peers.get_mut(&player) else {
                    continue;
                };
                match peer.reliable_outbox.front_mut() {
                    Some(front) => match front.last_retry {
                        Some(last) if now.0.saturating_sub(last.0) > RELIABLE_RETRY_MS => {
                            front.last_retry = Some(now);
                            Some((front.seq, front.packet.clone()))
                        }
                        _ => None,
                    },
                    None => None,
                }
            };
            if let Some((seq, packet)) = retry {
                actions.push(Action::Send {
                    player,
                    header: WireHeader {
                        reliable_seq: Some(seq),
                    },
                    packet,
                });
            }

            // Initiated DISCONNECT: five sends, then a forced removal.
            enum DisconnectStep {
                Send,
                Remove(DisconnectReason),
                Wait,
            }
            let step = {
                let Some(peer) = self.peers.get_mut(&player) else {
                    continue;
                };
                match peer.disconnecting {
                    Some(progress)
                        if progress.sends < DISCONNECT_SENDS
                            && now.0.saturating_sub(progress.last.0) > DISCONNECT_RETRY_MS =>
                    {
                        peer.disconnecting = Some(DisconnectProgress {
                            sends: progress.sends + 1,
                            last: now,
                            reason: progress.reason,
                        });
                        DisconnectStep::Send
                    }
                    Some(progress)
                        if progress.sends >= DISCONNECT_SENDS
                            && now.0.saturating_sub(progress.last.0) > DISCONNECT_RETRY_MS =>
                    {
                        DisconnectStep::Remove(progress.reason)
                    }
                    _ => DisconnectStep::Wait,
                }
            };
            match step {
                DisconnectStep::Send => {
                    actions.push(self.send_plain(player, ServerPacket::Disconnect));
                }
                DisconnectStep::Remove(reason) => {
                    actions.extend(self.remove_peer(player, reason, None));
                }
                DisconnectStep::Wait => {}
            }

            // Lobby cadence while waiting for launch.
            if established
                && self.state == ServerState::WaitingLaunch
                && idle_waitdata > WAITDATA_MS
            {
                let data = self.waiting_data(player);
                actions.push(self.send_plain(player, ServerPacket::WaitingData(data)));
                if let Some(peer) = self.peers.get_mut(&player) {
                    peer.last_waitdata = now;
                }
            }
        }

        actions
    }

    // --- helpers -----------------------------------------------------------

    /// Every SYN-accepted peer, players and drones, in admit order.
    fn established_peers(&self) -> Vec<PlayerId> {
        let mut peers: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.established)
            .map(|(player, peer)| (peer.order, *player))
            .collect();
        peers.sort_unstable();
        peers.into_iter().map(|(_, player)| player).collect()
    }

    /// SYN-accepted non-drone players in admit order.
    fn established_players(&self) -> Vec<PlayerId> {
        self.established_peers()
            .into_iter()
            .filter(|player| self.peers.get(player).is_some_and(|peer| !peer.drone))
            .collect()
    }

    fn max_players(&self) -> usize {
        self.established_peers()
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
                .filter(|peer| peer.established && peer.drone)
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
        actions.push(self.send_plain(player, ServerPacket::Rejected { reason }));
        actions.extend(self.remove_peer(player, DisconnectReason::Remote, None));
    }

    /// Begin an initiated DISCONNECT (drones on game end): five sends,
    /// then a forced removal. The first send goes out immediately, as
    /// upstream's first connection run does.
    fn start_disconnect(
        &mut self,
        actions: &mut Vec<Action>,
        player: PlayerId,
        reason: DisconnectReason,
    ) {
        actions.push(self.send_plain(player, ServerPacket::Disconnect));
        if let Some(peer) = self.peers.get_mut(&player) {
            peer.disconnecting = Some(DisconnectProgress {
                sends: 1,
                last: peer.last_send,
                reason,
            });
        }
    }

    fn send_plain(&self, player: PlayerId, packet: ServerPacket) -> Action {
        Action::Send {
            player,
            header: WireHeader { reliable_seq: None },
            packet,
        }
    }

    /// Queue a reliable packet and emit its first transmission in the
    /// same pump, as upstream's same-iteration connection run does. The
    /// retry clock starts at `now`; a later timer retries the head in
    /// place. The 65th enqueue removes the peer instead of allocating.
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
            // The 65th enqueue removes the peer immediately: no
            // allocation, no explanatory enqueue to the already
            // unacknowledging peer.
            let name = peer.name.clone();
            let mut message = b"Client '".to_vec();
            message.extend_from_slice(&name);
            message.extend_from_slice(b"' disconnected");
            actions.extend(self.remove_peer(
                player,
                DisconnectReason::ReliableOverflow,
                Some(message),
            ));
            return;
        }

        let seq = peer.reliable_send_seq;
        peer.reliable_send_seq = peer.reliable_send_seq.wrapping_add(1);
        peer.reliable_outbox.push_back(ReliableEntry {
            seq,
            packet: packet.clone(),
            last_retry: Some(self.clock),
        });
        peer.last_send = self.clock;
        actions.push(Action::Send {
            player,
            header: WireHeader {
                reliable_seq: Some(seq),
            },
            packet,
        });
    }

    fn broadcast_console(&mut self, actions: &mut Vec<Action>, message: Vec<u8>) {
        for target in self.established_peers() {
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

/// `D_ValidEpisodeMap` (d_mode.c): per mission/mode, the max episode and map.
fn valid_episode_map(mission: u8, mode: u8, episode: u8, map: u8) -> bool {
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
