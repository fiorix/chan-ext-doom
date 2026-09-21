const HOST_READY = "chan:extension-ready:v1";
const HOST_COMMAND = "chan:extension-command:v1";
const HOST_RESULT = "chan:extension-command-result:v1";
const HOST_KEYMAP = "chan:extension-host-keymap:v2";
const HOST_KEYDOWN = "chan:extension-keydown:v2";
const HOST_SESSION = "chan:extension-session-context:v1";
const HOST_VIEW = "chan:extension-view-state:v1";
const HOST_PRESENT = "chan:extension-presentation:v1";

const el = (id) => document.getElementById(id);
let frameDocument = null;
function loadFrameDocument() {
  frameDocument ??= fetch("frame.html").then(async (response) => {
    if (!response.ok) throw new Error(`engine frame load failed: HTTP ${response.status}`);
    return response.text();
  });
  return frameDocument;
}
const state = {
  context: { self_id: null, participants: [] },
  snapshot: null,
  control: null,
  controlRetry: 250,
  pendingActions: new Map(),
  frame: null,
  frameNonce: null,
  pendingLaunch: null,
  gameRole: null,
  viewActive: true,
  hostKeys: [],
  confirmResolve: null,
  hostReady: false,
};

function log(line) {
  const output = el("log");
  output.textContent += `${line}\n`;
  output.scrollTop = output.scrollHeight;
}

function websocketUrl(path) {
  const url = new URL(path, window.location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

function selfParticipant() {
  return state.context.participants.find((p) => p.id === state.context.self_id) ?? null;
}

function sendHello() {
  if (state.control?.readyState !== WebSocket.OPEN || !state.context.self_id) return;
  state.control.send(JSON.stringify({
    type: "hello",
    self_id: state.context.self_id,
    name: selfParticipant()?.name || "Doom player",
  }));
}

function connectControl() {
  const socket = new WebSocket(websocketUrl("control"));
  state.control = socket;
  socket.addEventListener("open", () => {
    state.controlRetry = 250;
    el("online").textContent = "online";
    el("online").classList.remove("offline");
    sendHello();
    if (!state.hostReady) {
      state.hostReady = true;
      parent.postMessage({ type: HOST_READY }, "*");
    }
  });
  socket.addEventListener("message", (event) => {
    let message;
    try { message = JSON.parse(event.data); } catch { return; }
    if (message.type === "snapshot") {
      state.snapshot = message;
      render();
      return;
    }
    if (message.type === "action-result" && typeof message.request_id === "string") {
      const pending = state.pendingActions.get(message.request_id);
      if (!pending) return;
      state.pendingActions.delete(message.request_id);
      if (message.ok) pending.resolve(message);
      else pending.reject(new Error(message.message || "action refused"));
    }
  });
  socket.addEventListener("close", () => {
    if (state.control === socket) state.control = null;
    el("online").textContent = "offline";
    el("online").classList.add("offline");
    render();
    const delay = state.controlRetry;
    state.controlRetry = Math.min(5000, delay * 2);
    setTimeout(connectControl, delay);
  });
}

function requestAction(action) {
  if (state.control?.readyState !== WebSocket.OPEN) {
    return Promise.reject(new Error("Doom server is offline"));
  }
  const request_id = crypto.randomUUID?.() ?? `${Date.now()}-${Math.random()}`;
  return new Promise((resolve, reject) => {
    state.pendingActions.set(request_id, { resolve, reject });
    state.control.send(JSON.stringify({ ...action, request_id }));
    setTimeout(() => {
      const pending = state.pendingActions.get(request_id);
      if (!pending) return;
      state.pendingActions.delete(request_id);
      reject(new Error("Doom server action timed out"));
    }, 5000);
  });
}

function roleFor(participantId) {
  return state.snapshot?.claims?.find((claim) => claim.participant_id === participantId) ?? null;
}

function render() {
  const snapshot = state.snapshot;
  el("phase").textContent = snapshot?.phase ?? "idle";
  el("controller").textContent = snapshot?.controller ?? "none";
  el("player-count").textContent = String(snapshot?.players ?? 0);
  el("spectator-count").textContent = String(snapshot?.spectators ?? 0);
  el("fingerprint").textContent = snapshot?.fingerprint ?? "shareware 1.9";
  if (snapshot?.settings && snapshot.owner_id) {
    el("mode").value = snapshot.settings.mode;
    el("target").value = String(snapshot.settings.target);
  }

  const mine = roleFor(state.context.self_id);
  const owner = snapshot?.owner_id === state.context.self_id;
  const inGame = snapshot?.phase === "in-game";
  const canConfigure = owner || !snapshot?.owner_id;
  el("mode").disabled = !canConfigure || inGame;
  el("target").disabled = !canConfigure || inGame;
  el("join").disabled = !state.context.self_id || mine?.role === "player" || inGame;
  el("spectate").disabled =
    !state.context.self_id ||
    mine?.role === "spectator" ||
    inGame ||
    (snapshot?.claims?.filter((claim) => claim.role === "player").length ?? 0) === 0;
  el("leave").disabled = !state.gameRole && !mine;

  const roster = el("roster");
  roster.replaceChildren();
  for (const participant of state.context.participants) {
    const claim = roleFor(participant.id);
    const item = document.createElement("li");
    const label = document.createElement("span");
    label.textContent = participant.name || participant.id;
    const status = document.createElement("small");
    const tags = [participant.role, participant.status];
    if (claim) {
      tags.push(claim.role + (claim.controller ? " (controller)" : ""));
      tags.push(claim.connected ? (claim.ready ? "ready" : "connected") : "claimed");
    }
    status.textContent = tags.join(" / ");
    item.append(label, status);
    roster.appendChild(item);
  }
  if (!state.context.participants.length) {
    const item = document.createElement("li");
    item.className = "muted";
    item.textContent = "Waiting for Chan session context";
    roster.appendChild(item);
  }
}

function confirmAction(copy) {
  el("confirm-copy").textContent = copy;
  el("confirm").hidden = false;
  el("accept-confirm").focus();
  return new Promise((resolve) => { state.confirmResolve = resolve; });
}

function finishConfirm(value) {
  el("confirm").hidden = true;
  state.confirmResolve?.(value);
  state.confirmResolve = null;
}

async function stopGame({ release = true, confirm = true } = {}) {
  if ((state.frame || roleFor(state.context.self_id)) && confirm) {
    const accepted = await confirmAction("This disconnects the current Doom client and releases its lobby role.");
    if (!accepted) return false;
  }
  state.frame?.remove();
  state.frame = null;
  state.frameNonce = null;
  state.pendingLaunch = null;
  state.gameRole = null;
  el("empty").hidden = false;
  if (release && roleFor(state.context.self_id)) {
    try { await requestAction({ type: "leave" }); } catch (error) { log(error.message); }
  }
  render();
  return true;
}

async function launch(role) {
  if (state.frame && state.gameRole === role) return;
  if (state.frame && !(await stopGame({ confirm: true }))) return;

  const participant = selfParticipant();
  let authoritative = state.snapshot?.settings;
  let protocolName = (participant?.name || "Doom player").replace(/[\r\n"\\]/g, " ").slice(0, 30);
  if (role !== "solo") {
    const claimed = await requestAction({
      type: "claim",
      role,
      name: participant?.name || "Doom player",
    });
    if (typeof claimed.protocol_name !== "string") {
      throw new Error("Doom server did not issue a launch ticket");
    }
    protocolName = claimed.protocol_name;
    authoritative = claimed.settings;
    if (role === "player" && claimed.owner) {
      const configured = await requestAction({
        type: "settings",
        mode: el("mode").value,
        target: Number(el("target").value),
      });
      authoritative = configured.settings;
    }
  }

  const wad = await fetch("doom1.wad").then((response) => {
    if (!response.ok) throw new Error(`IWAD load failed: HTTP ${response.status}`);
    return response.arrayBuffer();
  });
  const frameHtml = await loadFrameDocument();
  const nonce = crypto.randomUUID?.() ?? `${Date.now()}-${Math.random()}`;
  const config = new TextEncoder().encode(`player_name "${protocolName}"\n`).buffer;
  const argv = ["-iwad", "/doom1.wad", "-config", "/chan.cfg"];
  if (role !== "solo") {
    const mode = authoritative?.mode ?? "coop";
    const target = authoritative?.target ?? 2;
    if (mode === "deathmatch") argv.push("-deathmatch");
    if (role === "spectator") argv.push("-drone");
    argv.push("-connect", "1", "-wss", websocketUrl("game"), "-nodes", String(target));
  }

  const frame = document.createElement("iframe");
  frame.title = "Doom engine";
  // Chan deliberately gives the outer extension an opaque origin. A network
  // navigation here would therefore fail the proxy's `frame-ancestors
  // 'self'` policy. srcdoc keeps the nested engine under that opaque sandbox
  // without weakening Chan's response policy.
  frame.name = nonce;
  frame.srcdoc = frameHtml;
  state.frame = frame;
  state.frameNonce = nonce;
  state.gameRole = role;
  state.pendingLaunch = {
    type: "doom-launch",
    nonce,
    argv,
    files: [
      { path: "/doom1.wad", bytes: wad },
      { path: "/chan.cfg", bytes: config },
    ],
  };
  el("stage").insertBefore(frame, el("log"));
  el("empty").hidden = true;
  log(`launching ${role}: ${argv.join(" ")}`);
  render();
}

async function runCommand(message) {
  let ok = true;
  let result = "";
  try {
    if (message.id === "play-solo") await launch("solo");
    else if (message.id === "join-session") await launch("player");
    else if (message.id === "spectate") await launch("spectator");
    else if (message.id === "leave-game") {
      if (!(await stopGame({ confirm: true }))) result = "Leave cancelled";
    } else if (message.id === "toggle-presentation") {
      parent.postMessage({ type: HOST_PRESENT, action: "toggle" }, "*");
    } else {
      throw new Error(`Unknown Doomit command: ${message.id}`);
    }
  } catch (error) {
    ok = false;
    result = error.message || String(error);
    log(result);
  }
  parent.postMessage({ type: HOST_RESULT, request_id: message.request_id, ok, message: result || undefined }, "*");
}

window.addEventListener("message", (event) => {
  if (event.source === window.parent) {
    const message = event.data;
    if (!message || typeof message !== "object") return;
    if (message.type === HOST_SESSION) {
      state.context = {
        self_id: typeof message.self_id === "string" ? message.self_id : null,
        participants: Array.isArray(message.participants) ? message.participants : [],
      };
      sendHello();
      render();
    } else if (message.type === HOST_KEYMAP) {
      state.hostKeys = Array.isArray(message.keys) ? message.keys : [];
      state.frame?.contentWindow?.postMessage({ ...message, nonce: state.frameNonce }, "*");
    } else if (message.type === HOST_VIEW) {
      state.viewActive = message.active === true;
      state.frame?.contentWindow?.postMessage({ type: HOST_VIEW, nonce: state.frameNonce, active: state.viewActive }, "*");
    } else if (message.type === HOST_COMMAND && typeof message.id === "string") {
      void runCommand(message);
    }
    return;
  }

  if (event.source !== state.frame?.contentWindow) return;
  const message = event.data;
  if (!message || message.nonce !== state.frameNonce) return;
  if (message.type === "doom-frame-ready" && state.pendingLaunch) {
    const launch = state.pendingLaunch;
    state.pendingLaunch = null;
    state.frame.contentWindow.postMessage({ type: HOST_KEYMAP, keys: state.hostKeys, nonce: state.frameNonce }, "*");
    state.frame.contentWindow.postMessage({ type: HOST_VIEW, active: state.viewActive, nonce: state.frameNonce }, "*");
    state.frame.contentWindow.postMessage(launch, "*", launch.files.map((file) => file.bytes));
  } else if (message.type === "doom-log") {
    log(message.text);
  } else if (message.type === "doom-started") {
    log("engine started");
  } else if (message.type === HOST_KEYDOWN) {
    const { nonce: _nonce, ...hostMessage } = message;
    parent.postMessage(hostMessage, "*");
  }
});

function updateSettings() {
  if (state.snapshot?.owner_id !== state.context.self_id) return;
  void requestAction({
    type: "settings",
    mode: el("mode").value,
    target: Number(el("target").value),
  }).catch((error) => log(error.message));
}

function toggleOverlay(toggle, className, label, glyphs) {
  const collapsed = document.body.classList.toggle(className);
  toggle.textContent = glyphs[collapsed ? 0 : 1];
  toggle.title = `${collapsed ? "Show" : "Hide"} ${label}`;
  toggle.setAttribute("aria-label", toggle.title);
  toggle.setAttribute("aria-expanded", String(!collapsed));
  // Doom only sees keys while the engine frame holds focus, and clicking a
  // toggle took it away.
  state.frame?.contentWindow?.focus();
}

el("mode").addEventListener("change", updateSettings);
el("target").addEventListener("change", updateSettings);
el("solo").addEventListener("click", () => void launch("solo").catch((error) => log(error.message)));
el("join").addEventListener("click", () => void launch("player").catch((error) => log(error.message)));
el("spectate").addEventListener("click", () => void launch("spectator").catch((error) => log(error.message)));
el("leave").addEventListener("click", () => void stopGame({ confirm: true }));
el("toggle-lobby").addEventListener("click", () =>
  toggleOverlay(el("toggle-lobby"), "lobby-collapsed", "lobby", ["❯", "❮"]));
el("toggle-log").addEventListener("click", () =>
  toggleOverlay(el("toggle-log"), "log-collapsed", "console", ["▴", "▾"]));
el("cancel-confirm").addEventListener("click", () => finishConfirm(false));
el("accept-confirm").addEventListener("click", () => finishConfirm(true));

connectControl();
render();
