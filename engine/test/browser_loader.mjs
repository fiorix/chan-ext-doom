// Loader assertions against the real page in a real browser.
//
// test/run.sh is deliberately host-only: a plain compiler and node, no
// emscripten, no SDL, no WADs, no browser. This check needs Chrome and an
// IWAD, so it is not wired into that suite and is run on its own.
//
// What it adds over test/test_net_argv.mjs is the part a module test cannot
// reach: the page's own rendered argv, the live disabled state of the room and
// node controls, and whether the Launch button is actually refused. The module
// test pins the contract, this one pins that the page implements it.
//
// The IWAD is required rather than optional. blockedReason() checks the IWAD
// before it checks anything about the network, so every refusal assertion
// below would pass on the IWAD alone and prove nothing about the network. The
// run therefore accepts an IWAD first and asserts a launchable page before it
// asserts any refusal.
//
// Usage: node test/browser_loader.mjs <path-to-doom1.wad> [path-to-chrome]
//        IWAD=... CHROME=... node test/browser_loader.mjs

import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, extname, resolve } from "node:path";

const IWAD = process.argv[2] || process.env.IWAD;
const CHROME = process.argv[3] || process.env.CHROME || "chrome";

if (!IWAD) {
  console.error("usage: node test/browser_loader.mjs <path-to-doom1.wad> [chrome]");
  console.error("an IWAD is required: without one the page refuses to launch for");
  console.error("that reason alone, and every network assertion here is vacuous.");
  process.exit(2);
}

const ROOT = new URL("..", import.meta.url).pathname;
const ROOM = "ws://127.0.0.1:8080/ws/e1m1";

let checks = 0;
let failures = 0;

function check(ok, what) {
  checks += 1;
  if (!ok) {
    failures += 1;
    console.log(`FAIL: ${what}`);
  }
}

// --- a static server for the loader, because ES modules need an origin ------

const TYPES = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript" };

const server = createServer(async (req, res) => {
  const path = req.url.split("?")[0];
  try {
    const body = await readFile(join(ROOT, path === "/" ? "/doom.html" : path));
    res.writeHead(200, { "content-type": TYPES[extname(path)] || "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404);
    res.end("not found");
  }
});

await new Promise((resolve_) => server.listen(0, "127.0.0.1", resolve_));
const origin = `http://127.0.0.1:${server.address().port}`;

// --- drive Chrome over the devtools protocol, with no package dependency ----

// --no-sandbox because this host restricts unprivileged user namespaces, so
// the zygote sandbox aborts at startup. The page under test is local and is
// served from a loopback origin, and no remote content is loaded.
const profile = await mkdtemp(join(tmpdir(), "doomit-loader-"));
const chrome = spawn(CHROME, [
  "--headless=new",
  "--no-sandbox",
  "--disable-gpu",
  "--no-first-run",
  "--no-default-browser-check",
  "--remote-debugging-port=0",
  `--user-data-dir=${profile}`,
  "about:blank",
  // detached so Chrome leads its own process group. Killing the parent alone
  // leaves the zygote and one renderer per tab running, which outlive the run.
], { stdio: ["ignore", "ignore", "pipe"], detached: true });

let socket = null;

// Everything acquired above is released here, on success and on failure alike,
// so a failed assertion does not leave a browser and a listening socket behind.
async function cleanup() {
  try { socket?.close(); } catch { /* already gone */ }

  // Wait for Chrome to actually exit before removing the profile. It writes
  // there until the moment it dies, so removing it from under a live process
  // fails with ENOTEMPTY and turns a passing run into an error.
  if (chrome.exitCode === null) {
    const exited = new Promise((done) => chrome.once("exit", done));
    killTree("SIGTERM");
    await Promise.race([exited, new Promise((r) => setTimeout(r, 5000))]);
    if (chrome.exitCode === null) killTree("SIGKILL");
  }

  await new Promise((done) => server.close(done));
  await rm(profile, { recursive: true, force: true });
}

// Negative pid signals the group, so the zygote and the renderers go too.
function killTree(signal) {
  try { process.kill(-chrome.pid, signal); } catch { /* already gone */ }
}

process.on("exit", () => killTree("SIGKILL"));

async function devtoolsPort() {
  for (let i = 0; i < 200; i += 1) {
    try {
      const [port] = (await readFile(join(profile, "DevToolsActivePort"), "utf8")).split("\n");
      if (port) return port.trim();
    } catch { /* not written yet */ }
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error("Chrome never published a devtools port");
}

let nextId = 0;
const pending = new Map();

function command(method, params = {}) {
  const id = (nextId += 1);
  return new Promise((resolve_, reject) => {
    pending.set(id, (message) => {
      if (message.error) return reject(new Error(`${method}: ${message.error.message}`));
      resolve_(message.result);
    });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

// Evaluate in the page, surfacing a page-side throw here rather than letting it
// return a silent undefined.
async function evaluate(expression) {
  const { result, exceptionDetails } = await command("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (exceptionDetails) {
    throw new Error(exceptionDetails.exception?.description?.split("\n")[0] || "page threw");
  }
  return result.value;
}

try {
  const port = await devtoolsPort();
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const target = targets.find((t) => t.type === "page");
  if (!target) throw new Error("Chrome opened no page target");

  socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve_, reject) => {
    socket.addEventListener("open", resolve_, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    const settle = pending.get(message.id);
    if (settle) {
      pending.delete(message.id);
      settle(message);
    }
  });

  // Navigate over the protocol rather than on the command line: a URL passed
  // to Chrome leaves the devtools target on its initial empty document, and
  // every query below would read an empty page and pass vacuously.
  await command("Page.enable");
  await command("Page.navigate", { url: `${origin}/doom.html` });
  for (let i = 0; i < 100; i += 1) {
    if (await evaluate(`document.readyState === "complete" && !!document.getElementById("argv")`)) break;
    await new Promise((r) => setTimeout(r, 50));
  }
  check(await evaluate(`!!document.getElementById("argv")`), "the loader page loaded");

  // --- accept the IWAD through the page's own file chooser ------------------

  const { root } = await command("DOM.getDocument");
  const { nodeId } = await command("DOM.querySelector", {
    nodeId: root.nodeId,
    selector: "#iwad",
  });
  await command("DOM.setFileInputFiles", { nodeId, files: [resolve(IWAD)] });

  // The page hashes and validates the file asynchronously.
  for (let i = 0; i < 200; i += 1) {
    if (await evaluate(`document.getElementById("iwad-info").className !== "empty"`)) break;
    await new Promise((r) => setTimeout(r, 50));
  }
  const iwadInfo = await evaluate(`document.getElementById("iwad-info").textContent`);
  check(await evaluate(`document.getElementById("iwad-info").className !== "empty"`),
        `the page accepted the IWAD, reported ${JSON.stringify(iwadInfo)}`);

  // --- helpers over the DOM the user actually sees --------------------------

  async function set(id, value) {
    await evaluate(`(() => {
      const node = document.getElementById(${JSON.stringify(id)});
      node.value = ${JSON.stringify(String(value))};
      node.dispatchEvent(new Event("input", { bubbles: true }));
      node.dispatchEvent(new Event("change", { bubbles: true }));
      return true;
    })()`);
  }

  async function setMode(mode) {
    await evaluate(`(() => {
      const radio = document.querySelector('input[name="mode"][value="' + ${JSON.stringify(mode)} + '"]');
      if (!radio) throw new Error("no such mode: " + ${JSON.stringify(mode)});
      radio.checked = true;
      radio.dispatchEvent(new Event("change", { bubbles: true }));
      return true;
    })()`);
  }

  const view = () => evaluate(`(() => ({
    argv: document.getElementById("argv").textContent,
    blocked: document.getElementById("blocked").textContent,
    launchDisabled: document.getElementById("launch").disabled,
    roomDisabled: document.getElementById("room").disabled,
    nodesDisabled: document.getElementById("nodes").disabled,
    modes: [...document.querySelectorAll('input[name="mode"]')].map((r) => r.value),
  }))()`);

  // --- the modes the page offers -------------------------------------------

  const modes = (await view()).modes;
  check(modes.length === 2, `the page offers two modes, got ${JSON.stringify(modes)}`);
  check(modes.includes("single") && modes.includes("multiplayer"),
        "the two modes are single player and multiplayer");
  check(!modes.includes("host") && !modes.includes("join"),
        "the live page offers no hosting or joining choice");

  // --- single player: controls inert, no network arguments, launchable ------

  await setMode("single");
  const single = await view();
  check(single.roomDisabled, "the room control is disabled for single player");
  check(single.nodesDisabled, "the node control is disabled for single player");
  for (const flag of ["-connect", "-wss", "-nodes", "-server", "-privateserver"]) {
    check(!single.argv.includes(flag), `the single-player argv has no ${flag}`);
  }
  check(!single.launchDisabled,
        `single player is launchable with an IWAD, blocked by ${JSON.stringify(single.blocked)}`);

  // --- multiplayer: controls live, exact client argv, launchable -----------

  await setMode("multiplayer");
  const enabled = await view();
  check(!enabled.roomDisabled, "the room control is enabled for multiplayer");
  check(!enabled.nodesDisabled, "the node control is enabled for multiplayer");

  await set("room", ROOM);
  await set("nodes", 3);
  const multi = await view();
  check(multi.argv.includes(`-connect 1 -wss ${ROOM} -nodes 3`),
        `the page renders the client argv, got ${JSON.stringify(multi.argv)}`);
  for (const flag of ["-server", "-privateserver"]) {
    check(!multi.argv.includes(flag), `the multiplayer argv has no ${flag}`);
  }

  // The positive control the refusals below depend on. If this is blocked, the
  // refusal assertions prove nothing, so it is asserted rather than assumed.
  check(!multi.launchDisabled,
        `a complete multiplayer form is launchable, blocked by ${JSON.stringify(multi.blocked)}`);
  check(multi.blocked === "", "a complete multiplayer form states no reason");

  for (const n of [2, 4, 8]) {
    await set("nodes", n);
    const at = await view();
    check(at.argv.includes(`-nodes ${n}`), `the page renders -nodes ${n}`);
    check(!at.launchDisabled, `${n} nodes is launchable`);
  }

  // --- the integration hole, in the browser --------------------------------

  // An out-of-range node count must refuse the launch. The builder drops the
  // network arguments when it cannot build them, so without a blocking reason
  // the page would launch a multiplayer game with no connection at all.
  //
  // Each case is bracketed by a valid setting that is asserted launchable, so
  // a refusal here is attributable to the node count and not to page state
  // left over from an earlier step.
  for (const bad of [1, 9, 0, ""]) {
    await set("nodes", bad);
    const refused = await view();
    check(refused.launchDisabled,
          `the Launch button is disabled when the node count is ${JSON.stringify(bad)}`);
    check(/node count/.test(refused.blocked),
          `the page blames the node count for ${JSON.stringify(bad)}, said ${JSON.stringify(refused.blocked)}`);
    check(!refused.argv.includes("-connect"),
          `no half-formed connect survives a node count of ${JSON.stringify(bad)}`);

    await set("nodes", 2);
    check(!(await view()).launchDisabled,
          `the page is launchable again after ${JSON.stringify(bad)}`);
  }

  // The same for an empty room.
  await set("room", "");
  const noRoom = await view();
  check(noRoom.launchDisabled, "the Launch button is disabled with no room URL");
  check(/room/.test(noRoom.blocked),
        `the page blames the room, said ${JSON.stringify(noRoom.blocked)}`);
  check(!noRoom.argv.includes("-connect"), "no half-formed connect survives an empty room");

  await set("room", ROOM);
  check(!(await view()).launchDisabled, "the page is launchable again with the room restored");
} finally {
  await cleanup();
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
