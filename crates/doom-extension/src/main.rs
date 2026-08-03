use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use clap::Parser;
use doom_server::{RoomName, Server, ServerState};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::Instant;

const SCOPE_HEADER: &str = "x-chan-extension-scope";
const ROOM_GRACE: Duration = Duration::from_secs(30);
const SNAPSHOT_PERIOD: Duration = Duration::from_millis(250);
const IWAD_FINGERPRINT: &str =
    "doom1:1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771";

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_CSS: &str = include_str!("../assets/app.css");
const APP_JS: &str = include_str!("../assets/app.js");
const FRAME_HTML: &str = include_str!("../assets/frame.html");

#[derive(Debug, Parser)]
#[command(name = "doomit-extension", about = "Doomit extension adapter for Chan")]
struct Cli {
    /// IPv4 loopback address used by Chan's private extension proxy.
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: SocketAddr,

    /// Directory containing doom.js, doom.wasm, and doom1.wad.
    #[arg(long)]
    assets: Option<PathBuf>,

    /// Maximum queued packets per Doom connection.
    #[arg(long, default_value = "64")]
    outbox_capacity: NonZeroUsize,
}

#[derive(Clone)]
struct AppState {
    token: Arc<str>,
    assets: RuntimeAssets,
    doom: Server,
    lobbies: Arc<Mutex<HashMap<String, Lobby>>>,
}

#[derive(Clone)]
struct RuntimeAssets {
    doom_js: Arc<Vec<u8>>,
    doom_wasm: Arc<Vec<u8>>,
    doom_wad: Arc<Vec<u8>>,
}

struct AssetPin {
    name: &'static str,
    bytes: usize,
    sha256: &'static str,
}

const ASSET_PINS: [AssetPin; 3] = [
    AssetPin {
        name: "doom.js",
        bytes: 188_773,
        sha256: "570ab64917c90d173d5c31e859b511cbf758497b482a87fdc0a3d093416f804d",
    },
    AssetPin {
        name: "doom.wasm",
        bytes: 1_690_113,
        sha256: "11464889f0ef793562c97336aaa1657e89a07b9f8b53bd360982fe824d346e4b",
    },
    AssetPin {
        name: "doom1.wad",
        bytes: 4_196_020,
        sha256: "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771",
    },
];

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum ClaimRole {
    Player,
    Spectator,
}

#[derive(Clone, Debug)]
struct Claim {
    participant_id: String,
    name: String,
    protocol_name: String,
    role: ClaimRole,
    order: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum GameMode {
    Coop,
    Deathmatch,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct LobbySettings {
    mode: GameMode,
    target: u8,
}

struct Lobby {
    controls: usize,
    control_identities: HashMap<String, usize>,
    claim_expires_at: HashMap<String, Instant>,
    expires_at: Option<Instant>,
    claims: HashMap<String, Claim>,
    owner_id: Option<String>,
    next_order: u64,
    settings: LobbySettings,
}

impl Default for Lobby {
    fn default() -> Self {
        Self {
            controls: 0,
            control_identities: HashMap::new(),
            claim_expires_at: HashMap::new(),
            expires_at: None,
            claims: HashMap::new(),
            owner_id: None,
            next_order: 0,
            settings: LobbySettings {
                mode: GameMode::Coop,
                target: 2,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct AuthQuery {
    t: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ClientMessage {
    Hello {
        self_id: String,
        name: String,
    },
    Claim {
        request_id: String,
        role: ClaimRole,
        name: String,
    },
    Settings {
        request_id: String,
        mode: GameMode,
        target: u8,
    },
    Leave {
        request_id: String,
    },
}

#[derive(Serialize)]
struct ActionResult<'a> {
    r#type: &'static str,
    request_id: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<LobbySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_name: Option<String>,
}

#[derive(Serialize)]
struct ClaimView {
    participant_id: String,
    name: String,
    role: ClaimRole,
    controller: bool,
    connected: bool,
    ready: bool,
}

#[derive(Serialize)]
struct Snapshot {
    r#type: &'static str,
    online: bool,
    phase: &'static str,
    owner_id: Option<String>,
    controller: Option<String>,
    players: usize,
    spectators: usize,
    settings: LobbySettings,
    fingerprint: String,
    claims: Vec<ClaimView>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if !cli.listen.ip().is_loopback() || !cli.listen.is_ipv4() {
        bail!("--listen must use IPv4 loopback");
    }
    let assets_dir = match cli.assets {
        Some(path) => path,
        None => default_assets_dir()?,
    };
    let assets = load_assets(&assets_dir)?;
    let listener = TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("binding {}", cli.listen))?;
    let address = listener.local_addr().context("reading bound address")?;
    let token = random_token();
    let state = Arc::new(AppState {
        token: Arc::from(token.as_str()),
        assets,
        doom: Server::new(cli.outbox_capacity),
        lobbies: Arc::new(Mutex::new(HashMap::new())),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/frame.html", get(frame_html))
        .route("/doom.js", get(doom_js))
        .route("/doom.wasm", get(doom_wasm))
        .route("/doom1.wad", get(doom_wad))
        .route("/control", get(control))
        .route("/game", get(game))
        .with_state(state.clone());

    let handshake = serde_json::json!({
        "url": format!("http://{address}/"),
        "token": token,
        "singleton": true,
        "commands": [
            {"id": "play-solo", "title": "Play Solo", "keywords": ["doom", "single player"]},
            {"id": "join-session", "title": "Join Doom Session", "keywords": ["doom", "multiplayer"]},
            {"id": "spectate", "title": "Spectate Doom Session", "keywords": ["doom", "observer"]},
            {"id": "leave-game", "title": "Leave Doom Game", "keywords": ["doom", "disconnect"]},
            {"id": "toggle-presentation", "title": "Toggle Doom Presentation", "keywords": ["doom", "maximize", "restore"]}
        ]
    });
    println!("CHAN_EXTENSION_V1={handshake}");

    let http = axum::serve(listener, app).into_future();
    let doom_timer = state.doom.run();
    let reaper = reap_lobbies(state.clone());
    tokio::select! {
        result = http => result.context("serving extension")?,
        () = doom_timer => unreachable!("the Doom timer runs until cancellation"),
        () = reaper => unreachable!("the lobby reaper runs until cancellation"),
    }
    Ok(())
}

fn default_assets_dir() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("resolving extension executable")?;
    let parent = executable
        .parent()
        .context("extension executable has no parent directory")?;
    Ok(parent.join("share").join("doomit"))
}

fn load_assets(directory: &Path) -> Result<RuntimeAssets> {
    let mut loaded = HashMap::new();
    for pin in ASSET_PINS {
        let path = directory.join(pin.name);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        if bytes.len() != pin.bytes {
            bail!(
                "{} is {} bytes, expected {}",
                path.display(),
                bytes.len(),
                pin.bytes
            );
        }
        let actual = hex(&Sha256::digest(&bytes));
        if actual != pin.sha256 {
            bail!(
                "{} sha256 is {actual}, expected {}",
                path.display(),
                pin.sha256
            );
        }
        loaded.insert(pin.name, Arc::new(bytes));
    }
    Ok(RuntimeAssets {
        doom_js: loaded
            .remove("doom.js")
            .expect("the pin list contains doom.js"),
        doom_wasm: loaded
            .remove("doom.wasm")
            .expect("the pin list contains doom.wasm"),
        doom_wad: loaded
            .remove("doom1.wad")
            .expect("the pin list contains doom1.wad"),
    })
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn authorized(state: &AppState, auth: &AuthQuery) -> bool {
    auth.t.as_bytes() == state.token.as_bytes()
}

fn scope(headers: &HeaderMap) -> Result<String, StatusCode> {
    let value = headers
        .get(SCOPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    if value.is_empty() || value.len() > 256 {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(value.to_string())
}

fn room_name(scope: &str) -> RoomName {
    let digest = hex(&Sha256::digest(scope.as_bytes()));
    RoomName::try_from(format!("chan-{}", &digest[..32]).as_str())
        .expect("a truncated hex digest is a valid room name")
}

fn static_response(content_type: &'static str, body: impl Into<Body>) -> Response {
    let mut response = Response::new(body.into());
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

fn check_auth(state: &AppState, auth: &AuthQuery) -> Result<(), StatusCode> {
    authorized(state, auth)
        .then_some(())
        .ok_or(StatusCode::UNAUTHORIZED)
}

async fn index(
    State(state): State<Arc<AppState>>,
    Query(auth): Query<AuthQuery>,
) -> Result<Response, StatusCode> {
    check_auth(&state, &auth)?;
    Ok(static_response("text/html; charset=utf-8", INDEX_HTML))
}

macro_rules! embedded_handler {
    ($name:ident, $content_type:literal, $body:expr) => {
        async fn $name(
            State(state): State<Arc<AppState>>,
            Query(auth): Query<AuthQuery>,
        ) -> Result<Response, StatusCode> {
            check_auth(&state, &auth)?;
            Ok(static_response($content_type, $body))
        }
    };
}

embedded_handler!(app_css, "text/css; charset=utf-8", APP_CSS);
embedded_handler!(app_js, "text/javascript; charset=utf-8", APP_JS);
embedded_handler!(frame_html, "text/html; charset=utf-8", FRAME_HTML);

async fn doom_js(
    State(state): State<Arc<AppState>>,
    Query(auth): Query<AuthQuery>,
) -> Result<Response, StatusCode> {
    check_auth(&state, &auth)?;
    Ok(static_response(
        "text/javascript",
        (*state.assets.doom_js).clone(),
    ))
}

async fn doom_wasm(
    State(state): State<Arc<AppState>>,
    Query(auth): Query<AuthQuery>,
) -> Result<Response, StatusCode> {
    check_auth(&state, &auth)?;
    Ok(static_response(
        "application/wasm",
        (*state.assets.doom_wasm).clone(),
    ))
}

async fn doom_wad(
    State(state): State<Arc<AppState>>,
    Query(auth): Query<AuthQuery>,
) -> Result<Response, StatusCode> {
    check_auth(&state, &auth)?;
    Ok(static_response(
        "application/octet-stream",
        (*state.assets.doom_wad).clone(),
    ))
}

async fn control(
    websocket: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(auth): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_auth(&state, &auth)?;
    let scope = scope(&headers)?;
    Ok(websocket
        .max_message_size(16 * 1024)
        .on_upgrade(move |socket| control_session(socket, state, scope)))
}

async fn game(
    websocket: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(auth): Query<AuthQuery>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_auth(&state, &auth)?;
    let scope = scope(&headers)?;
    Ok(state.doom.websocket(websocket, room_name(&scope)))
}

async fn control_session(socket: WebSocket, state: Arc<AppState>, scope: String) {
    {
        let mut lobbies = state.lobbies.lock().await;
        let lobby = lobbies.entry(scope.clone()).or_default();
        lobby.controls += 1;
        lobby.expires_at = None;
    }
    let mut identity: Option<(String, String)> = None;
    let (mut sink, mut stream) = socket.split();
    let mut interval = tokio::time::interval(SNAPSHOT_PERIOD);
    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(Ok(Message::Text(text))) = incoming else { break };
                let message = match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(message) => message,
                    Err(_) => continue,
                };
                if let ClientMessage::Hello { self_id, name } = &message {
                    if valid_identity(self_id, name) {
                        set_control_identity(
                            &state,
                            &scope,
                            &mut identity,
                            self_id.clone(),
                            clean_name(name),
                        )
                        .await;
                    }
                    continue;
                }
                let response = apply_action(&state, &scope, &mut identity, message).await;
                if sink.send(Message::Text(response.into())).await.is_err() { break; }
            }
            _ = interval.tick() => {
                let snapshot = snapshot(&state, &scope).await;
                let Ok(snapshot) = serde_json::to_string(&snapshot) else { break };
                if sink.send(Message::Text(snapshot.into())).await.is_err() { break; }
            }
        }
    }
    let mut lobbies = state.lobbies.lock().await;
    if let Some(lobby) = lobbies.get_mut(&scope) {
        if let Some((self_id, _)) = identity {
            release_control_identity(lobby, &self_id, Instant::now());
        }
        lobby.controls = lobby.controls.saturating_sub(1);
        if lobby.controls == 0 {
            lobby.expires_at = Some(Instant::now() + ROOM_GRACE);
        }
    }
}

fn valid_identity(id: &str, name: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains(['\r', '\n', '\0'])
        && !name.contains(['\r', '\n', '\0'])
}

fn clean_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "Doom player".to_string()
    } else {
        name.chars().take(30).collect()
    }
}

async fn set_control_identity(
    state: &AppState,
    scope: &str,
    identity: &mut Option<(String, String)>,
    self_id: String,
    name: String,
) {
    let previous_id = identity.as_ref().map(|(id, _)| id.clone());
    if previous_id.as_deref() != Some(self_id.as_str()) {
        let mut lobbies = state.lobbies.lock().await;
        let lobby = lobbies.entry(scope.to_string()).or_default();
        if let Some(previous_id) = previous_id {
            release_control_identity(lobby, &previous_id, Instant::now());
        }
        *lobby.control_identities.entry(self_id.clone()).or_default() += 1;
        lobby.claim_expires_at.remove(&self_id);
    }
    *identity = Some((self_id, name));
}

fn release_control_identity(lobby: &mut Lobby, self_id: &str, now: Instant) {
    let Some(count) = lobby.control_identities.get_mut(self_id) else {
        return;
    };
    if *count > 1 {
        *count -= 1;
        return;
    }
    lobby.control_identities.remove(self_id);
    if lobby.claims.contains_key(self_id) {
        lobby
            .claim_expires_at
            .insert(self_id.to_string(), now + ROOM_GRACE);
    }
}

async fn apply_action(
    state: &AppState,
    scope: &str,
    identity: &mut Option<(String, String)>,
    message: ClientMessage,
) -> String {
    let request_id = match &message {
        ClientMessage::Hello { .. } => return String::new(),
        ClientMessage::Claim { request_id, .. }
        | ClientMessage::Settings { request_id, .. }
        | ClientMessage::Leave { request_id } => request_id,
    };
    let result = apply_action_inner(state, scope, identity, &message).await;
    let (owner, settings, protocol_name) = if result.is_ok() {
        let self_id = identity.as_ref().map(|(self_id, _)| self_id.as_str());
        let lobbies = state.lobbies.lock().await;
        lobbies.get(scope).map_or((None, None, None), |lobby| {
            (
                Some(lobby.owner_id.as_deref() == self_id),
                Some(lobby.settings),
                self_id.and_then(|self_id| {
                    lobby
                        .claims
                        .get(self_id)
                        .map(|claim| claim.protocol_name.clone())
                }),
            )
        })
    } else {
        (None, None, None)
    };
    serde_json::to_string(&ActionResult {
        r#type: "action-result",
        request_id,
        ok: result.is_ok(),
        message: result.err(),
        owner,
        settings,
        protocol_name,
    })
    .expect("action result is serializable")
}

async fn apply_action_inner(
    state: &AppState,
    scope: &str,
    identity: &mut Option<(String, String)>,
    message: &ClientMessage,
) -> Result<(), String> {
    let Some((self_id, hello_name)) = identity.clone() else {
        return Err("Chan session identity is not ready".to_string());
    };
    let protocol = state.doom.room_snapshot(&room_name(scope)).await;
    if protocol
        .as_ref()
        .is_some_and(|snapshot| snapshot.state == ServerState::InGame)
        && matches!(message, ClientMessage::Claim { .. })
    {
        return Err("The match has already started".to_string());
    }
    let mut lobbies = state.lobbies.lock().await;
    let lobby = lobbies.entry(scope.to_string()).or_default();
    match message {
        ClientMessage::Claim { role, name, .. } => {
            if *role == ClaimRole::Spectator
                && !lobby
                    .claims
                    .values()
                    .any(|claim| claim.role == ClaimRole::Player && claim.participant_id != self_id)
            {
                return Err("A player must join before spectators".to_string());
            }
            let existing = lobby
                .claims
                .get(&self_id)
                .map(|claim| (claim.order, claim.protocol_name.clone()));
            let (order, protocol_name) = existing.unwrap_or_else(|| {
                lobby.next_order = lobby.next_order.saturating_add(1);
                (lobby.next_order, new_protocol_name())
            });
            let name = clean_name(if name.trim().is_empty() {
                &hello_name
            } else {
                name
            });
            lobby.claims.insert(
                self_id.clone(),
                Claim {
                    participant_id: self_id.clone(),
                    name,
                    protocol_name,
                    role: *role,
                    order,
                },
            );
            lobby.claim_expires_at.remove(&self_id);
            elect_owner(lobby);
        }
        ClientMessage::Settings { mode, target, .. } => {
            let protocol_controller = protocol.as_ref().and_then(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .find(|peer| peer.controller)
                    .map(|peer| peer.name.as_str())
            });
            let controls_lobby = protocol_controller.map_or_else(
                || lobby.owner_id.as_deref() == Some(self_id.as_str()),
                |controller| {
                    lobby
                        .claims
                        .get(&self_id)
                        .is_some_and(|claim| claim.protocol_name == controller)
                },
            );
            if !controls_lobby {
                return Err("Only the Doom controller can change lobby settings".to_string());
            }
            if !(2..=4).contains(target) {
                return Err("Player target must be 2 to 4".to_string());
            }
            lobby.settings = LobbySettings {
                mode: *mode,
                target: *target,
            };
        }
        ClientMessage::Leave { .. } => {
            lobby.claims.remove(&self_id);
            lobby.claim_expires_at.remove(&self_id);
            elect_owner(lobby);
        }
        ClientMessage::Hello { .. } => {}
    }
    Ok(())
}

fn new_protocol_name() -> String {
    let mut bytes = [0_u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("chan-{}", hex(&bytes))
}

fn elect_owner(lobby: &mut Lobby) {
    if lobby.owner_id.as_ref().is_some_and(|owner| {
        lobby
            .claims
            .get(owner)
            .is_some_and(|claim| claim.role == ClaimRole::Player)
    }) {
        return;
    }
    lobby.owner_id = lobby
        .claims
        .values()
        .filter(|claim| claim.role == ClaimRole::Player)
        .min_by_key(|claim| claim.order)
        .map(|claim| claim.participant_id.clone());
}

async fn snapshot(state: &AppState, scope: &str) -> Snapshot {
    let protocol = state.doom.room_snapshot(&room_name(scope)).await;
    let (provisional_owner_id, settings, mut claims) = {
        let lobbies = state.lobbies.lock().await;
        let lobby = lobbies.get(scope);
        let mut claims = lobby
            .map(|lobby| lobby.claims.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        claims.sort_by_key(|claim| claim.order);
        (
            lobby.and_then(|lobby| lobby.owner_id.clone()),
            lobby.map_or(
                LobbySettings {
                    mode: GameMode::Coop,
                    target: 2,
                },
                |lobby| lobby.settings,
            ),
            claims,
        )
    };
    let protocol_controller = protocol.as_ref().and_then(|snapshot| {
        snapshot
            .peers
            .iter()
            .find(|peer| peer.controller)
            .map(|peer| peer.name.clone())
    });
    let owner_id = protocol_controller
        .as_ref()
        .and_then(|controller| {
            claims
                .iter()
                .find(|claim| &claim.protocol_name == controller)
                .map(|claim| claim.participant_id.clone())
        })
        .or(provisional_owner_id);
    let controller = owner_id
        .as_ref()
        .and_then(|owner| {
            claims
                .iter()
                .find(|claim| &claim.participant_id == owner)
                .map(|claim| claim.name.clone())
        })
        .or(protocol_controller.clone());
    let claim_views = claims
        .drain(..)
        .map(|claim| {
            let peer = protocol.as_ref().and_then(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .find(|peer| peer.name == claim.protocol_name && peer.connected)
            });
            ClaimView {
                controller: protocol_controller
                    .as_ref()
                    .map_or(owner_id.as_deref() == Some(&claim.participant_id), |name| {
                        name == &claim.protocol_name
                    }),
                connected: peer.is_some(),
                ready: peer.is_some_and(|peer| peer.ready),
                participant_id: claim.participant_id,
                name: claim.name,
                role: claim.role,
            }
        })
        .collect();
    let phase = match protocol.as_ref().map(|snapshot| snapshot.state) {
        Some(ServerState::WaitingLaunch) => "waiting-launch",
        Some(ServerState::WaitingStart) => "waiting-start",
        Some(ServerState::InGame) => "in-game",
        None => "idle",
    };
    let players = protocol.as_ref().map_or(0, |snapshot| {
        snapshot
            .peers
            .iter()
            .filter(|peer| peer.connected && !peer.drone)
            .count()
    });
    let spectators = protocol.as_ref().map_or(0, |snapshot| {
        snapshot
            .peers
            .iter()
            .filter(|peer| peer.connected && peer.drone)
            .count()
    });
    let fingerprint = protocol
        .as_ref()
        .and_then(|snapshot| snapshot.wad_sha1)
        .map_or_else(
            || IWAD_FINGERPRINT.to_string(),
            |sha1| format!("wad-sha1:{}", hex(&sha1)),
        );
    Snapshot {
        r#type: "snapshot",
        online: true,
        phase,
        owner_id,
        controller,
        players,
        spectators,
        settings,
        fingerprint,
        claims: claim_views,
    }
}

async fn reap_lobbies(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        let expired_scopes = {
            let mut lobbies = state.lobbies.lock().await;
            expire_lobbies(&mut lobbies, Instant::now())
        };
        for scope in expired_scopes {
            state.doom.close_room(&room_name(&scope)).await;
        }
    }
}

fn expire_lobbies(lobbies: &mut HashMap<String, Lobby>, now: Instant) -> Vec<String> {
    for lobby in lobbies.values_mut() {
        let expired_claims = lobby
            .claim_expires_at
            .iter()
            .filter(|(_, expires)| **expires <= now)
            .map(|(participant_id, _)| participant_id.clone())
            .collect::<Vec<_>>();
        for participant_id in expired_claims {
            lobby.claim_expires_at.remove(&participant_id);
            lobby.claims.remove(&participant_id);
        }
        elect_owner(lobby);
    }

    let expired_scopes = lobbies
        .iter()
        .filter(|(_, lobby)| {
            lobby.controls == 0 && lobby.expires_at.is_some_and(|expires| expires <= now)
        })
        .map(|(scope, _)| scope.clone())
        .collect::<Vec<_>>();
    for scope in &expired_scopes {
        lobbies.remove(scope);
    }
    expired_scopes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState {
            token: Arc::from("test"),
            assets: RuntimeAssets {
                doom_js: Arc::new(Vec::new()),
                doom_wasm: Arc::new(Vec::new()),
                doom_wad: Arc::new(Vec::new()),
            },
            doom: Server::new(NonZeroUsize::new(4).expect("nonzero")),
            lobbies: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[test]
    fn scope_maps_to_a_private_portable_room_name() {
        let first = room_name("tenant one");
        let second = room_name("tenant two");
        assert_ne!(first, second);
        assert_eq!(first.as_str().len(), 37);
        assert!(!first.as_str().contains("tenant"));
    }

    #[test]
    fn identity_is_bounded_and_control_character_free() {
        assert!(valid_identity("window-1", "Alice"));
        assert!(!valid_identity("", "Alice"));
        assert!(!valid_identity("bad\nwindow", "Alice"));
        assert!(!valid_identity("window-1", "bad\nname"));
    }

    #[test]
    fn nested_engine_stays_inside_the_opaque_extension_sandbox() {
        assert!(APP_JS.contains("frame.srcdoc = frameHtml"));
        assert!(APP_JS.contains("frame.name = nonce"));
        assert!(FRAME_HTML.contains("const nonce = window.name"));
        assert!(FRAME_HTML.contains("<base href=\"./\">"));
        assert!(!APP_JS.contains("frame.src = `frame.html"));
        assert!(APP_CSS.contains("#empty[hidden] { display: none; }"));
    }

    #[test]
    fn runtime_asset_pins_match_engine_provenance() {
        let provenance = include_str!("../../../engine/docs/provenance.md");
        for pin in &ASSET_PINS {
            let bytes = pin.bytes.to_string();
            let documented = provenance.lines().any(|line| {
                line.to_ascii_lowercase().contains(pin.name)
                    && line.contains(&bytes)
                    && line.contains(pin.sha256)
            });
            assert!(
                documented,
                "engine provenance is missing the {} runtime pin",
                pin.name
            );
        }
    }

    #[tokio::test]
    async fn first_player_controls_settings_and_lobbies_are_scope_isolated() {
        let state = state();
        let mut alice = Some(("alice-window".to_string(), "Alice".to_string()));
        let claim = ClientMessage::Claim {
            request_id: "one".to_string(),
            role: ClaimRole::Player,
            name: "Alice".to_string(),
        };
        apply_action_inner(&state, "scope-a", &mut alice, &claim)
            .await
            .expect("first player claim");
        let settings = ClientMessage::Settings {
            request_id: "two".to_string(),
            mode: GameMode::Deathmatch,
            target: 4,
        };
        apply_action_inner(&state, "scope-a", &mut alice, &settings)
            .await
            .expect("owner changes settings");

        let mut bob = Some(("bob-window".to_string(), "Bob".to_string()));
        apply_action_inner(&state, "scope-a", &mut bob, &claim)
            .await
            .expect("second player joins");
        assert!(
            apply_action_inner(&state, "scope-a", &mut bob, &settings)
                .await
                .is_err()
        );

        let lobbies = state.lobbies.lock().await;
        assert_eq!(lobbies["scope-a"].owner_id.as_deref(), Some("alice-window"));
        assert_eq!(lobbies["scope-a"].settings.target, 4);
        assert_ne!(
            lobbies["scope-a"].claims["alice-window"].protocol_name,
            lobbies["scope-a"].claims["bob-window"].protocol_name,
            "duplicate display names still get distinct protocol identities"
        );
        assert!(!lobbies.contains_key("scope-b"));
    }

    #[test]
    fn disconnected_claims_and_empty_rooms_expire_at_the_grace_bound() {
        let now = Instant::now();
        let mut lobby = Lobby {
            controls: 1,
            owner_id: Some("alice".to_string()),
            ..Lobby::default()
        };
        lobby.claims.insert(
            "alice".to_string(),
            Claim {
                participant_id: "alice".to_string(),
                name: "Player".to_string(),
                protocol_name: "chan-alice".to_string(),
                role: ClaimRole::Player,
                order: 1,
            },
        );
        lobby.claims.insert(
            "bob".to_string(),
            Claim {
                participant_id: "bob".to_string(),
                name: "Player".to_string(),
                protocol_name: "chan-bob".to_string(),
                role: ClaimRole::Player,
                order: 2,
            },
        );
        lobby.claim_expires_at.insert("alice".to_string(), now);
        let mut lobbies = HashMap::from([("scope".to_string(), lobby)]);

        assert!(expire_lobbies(&mut lobbies, now).is_empty());
        assert!(!lobbies["scope"].claims.contains_key("alice"));
        assert_eq!(lobbies["scope"].owner_id.as_deref(), Some("bob"));

        let lobby = lobbies.get_mut("scope").expect("lobby remains");
        lobby.controls = 0;
        lobby.expires_at = Some(now);
        assert_eq!(expire_lobbies(&mut lobbies, now), vec!["scope"]);
        assert!(lobbies.is_empty());
    }

    #[tokio::test]
    async fn spectator_requires_a_player_claim() {
        let state = state();
        let mut observer = Some(("observer-window".to_string(), "Observer".to_string()));
        let claim = ClientMessage::Claim {
            request_id: "one".to_string(),
            role: ClaimRole::Spectator,
            name: "Observer".to_string(),
        };
        let error = apply_action_inner(&state, "scope", &mut observer, &claim)
            .await
            .expect_err("drone-first is refused");
        assert!(error.contains("player must join"));
    }
}
