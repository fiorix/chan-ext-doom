// Chan's keyboard relay through Doom's two frames, executed as shipped. The
// nested game frame's relay block is read out of frame.html between its
// marker comments, and the outer page's message listener out of app.js, and
// each runs against an isolated host. A shell chord typed in the game frame
// travels frame -> outer page -> Chan; these tests hold each hop.
//
// The keydowns come from published layouts, the same facts Chan's own
// matchers are tested against (Chan's web/packages/web-shared/src/
// keyboardVectors.ts). The advertised chords are a subset of what Chan
// advertises to an extension on a Linux browser.
//
//   node --test scripts/tests/keyboard-relay.test.mjs

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const asset = (name) =>
  readFileSync(new URL(`../../crates/doom-extension/assets/${name}`, import.meta.url), "utf8");

const frameHtml = asset("frame.html");
const start = frameHtml.indexOf("// Chan's keyboard relay.");
const end = frameHtml.indexOf("// End of Chan's keyboard relay.");
assert.ok(start >= 0 && end > start, "the relay block is marked in frame.html");
const FRAME_RELAY = frameHtml.slice(start, end);

const appJs = asset("app.js");
const listenerStart = appJs.indexOf('window.addEventListener("message", (event) => {');
const listenerEnd = appJs.indexOf("\n});\n", listenerStart);
assert.ok(listenerStart >= 0 && listenerEnd > listenerStart, "app.js has its message listener");
const OUTER_LISTENER = appJs.slice(listenerStart, listenerEnd + "\n});".length);
const HOST_CONSTANTS = [...appJs.matchAll(/^const (HOST_[A-Z]+) = ("[^"]+");$/gm)];

const LINUX = "Mozilla/5.0 (X11; Linux x86_64)";
const NONCE = "frame-nonce-1";

const chord = (key, mods) => ({
  key,
  ctrlKey: false,
  altKey: false,
  metaKey: false,
  shiftKey: false,
  ...mods,
});
const ADVERTISED = [
  chord("K", { ctrlKey: true, altKey: true }), // command launcher
  chord("T", { ctrlKey: true, shiftKey: true }), // new terminal
  chord(".", { ctrlKey: true }), // Hybrid Nav
  chord("1", { ctrlKey: true, altKey: true }), // first tab
];

function keydown(init) {
  const { altGraph = false, ...fields } = init;
  return {
    key: "",
    code: "",
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    shiftKey: false,
    repeat: false,
    isComposing: false,
    defaultPrevented: false,
    propagationStopped: false,
    ...fields,
    getModifierState: (name) => name === "AltGraph" && altGraph,
    preventDefault() {
      this.defaultPrevented = true;
    },
    stopImmediatePropagation() {
      this.propagationStopped = true;
    },
  };
}

/// The nested game frame, loaded with its nonce.
function loadFrame() {
  const reported = [];
  const listeners = {};
  const outer = {};
  const frame = {
    parent: outer,
    addEventListener: (type, fn) => (listeners[type] ??= []).push(fn),
  };
  const report = (type, detail = {}) => reported.push({ type, nonce: NONCE, ...detail });
  new Function("window", "navigator", "nonce", "report", FRAME_RELAY)(
    frame,
    { userAgent: LINUX },
    NONCE,
    report,
  );
  return {
    reported,
    host(data, from = outer) {
      for (const fn of listeners.message ?? []) fn({ source: from, data });
    },
    advertise() {
      this.host({ type: "chan:extension-host-keymap:v2", keys: ADVERTISED, nonce: NONCE });
    },
    press(init) {
      const event = keydown(init);
      for (const fn of listeners.keydown ?? []) fn(event);
      return event;
    },
  };
}

const CTRL_SHIFT = { ctrlKey: true, shiftKey: true };

test("the game frame relays Colemak Ctrl+Shift+T and hides it from the engine", () => {
  const frame = loadFrame();
  frame.advertise();
  const event = frame.press({ key: "T", code: "KeyF", ...CTRL_SHIFT });
  assert.equal(event.defaultPrevented, true);
  assert.equal(event.propagationStopped, true);
  assert.deepEqual(frame.reported, [
    {
      type: "chan:extension-keydown:v2",
      nonce: NONCE,
      key: "T",
      code: "KeyF",
      ctrlKey: true,
      altKey: false,
      metaKey: false,
      shiftKey: true,
      repeat: false,
      isComposing: false,
      altGraph: false,
    },
  ]);
});

test("the G on KeyT stays game input", () => {
  const frame = loadFrame();
  frame.advertise();
  const event = frame.press({ key: "G", code: "KeyT", ...CTRL_SHIFT });
  assert.equal(event.propagationStopped, false);
  assert.deepEqual(frame.reported, []);
});

test("AZERTY . typed with Shift reaches the unshifted chord; US > does not", () => {
  const frame = loadFrame();
  frame.advertise();
  frame.press({ key: ".", code: "Comma", ...CTRL_SHIFT });
  frame.press({ key: ">", code: "Period", ...CTRL_SHIFT });
  assert.deepEqual(
    frame.reported.map((message) => message.code),
    ["Comma"],
  );
});

test("AZERTY & on Digit1 keeps the digit position", () => {
  const frame = loadFrame();
  frame.advertise();
  frame.press({ key: "&", code: "Digit1", ctrlKey: true, altKey: true });
  assert.equal(frame.reported.length, 1);
});

test("text entry is never relayed", () => {
  const frame = loadFrame();
  frame.advertise();
  const launcher = { key: "k", code: "KeyK", ctrlKey: true, altKey: true };
  frame.press({ ...launcher, isComposing: true });
  frame.press({ ...launcher, altGraph: true });
  frame.press({ key: "Dead", code: "BracketLeft", ctrlKey: true });
  assert.deepEqual(frame.reported, []);
});

test("ordinary game input keeps its handling", () => {
  const frame = loadFrame();
  frame.advertise();
  const space = frame.press({ key: " ", code: "Space" });
  const arrow = frame.press({ key: "ArrowUp", code: "ArrowUp" });
  const fire = frame.press({ key: "Control", code: "ControlLeft", ctrlKey: true });
  assert.equal(space.defaultPrevented, true);
  assert.equal(arrow.defaultPrevented, true);
  assert.equal(fire.defaultPrevented, false);
  assert.equal(space.propagationStopped || arrow.propagationStopped, false);
  assert.deepEqual(frame.reported, []);
});

test("a keymap with a stale nonce, from another window, or on v1 advertises nothing", () => {
  const frame = loadFrame();
  const keys = ADVERTISED;
  frame.host({ type: "chan:extension-host-keymap:v2", keys, nonce: "an-earlier-frame" });
  frame.host({ type: "chan:extension-host-keymap:v2", keys, nonce: NONCE }, {});
  frame.host({ type: "chan:extension-host-keymap:v1", keys, nonce: NONCE });
  frame.press({ key: "T", code: "KeyF", ...CTRL_SHIFT });
  assert.deepEqual(frame.reported, []);
});

/// The outer extension page's message listener with one live game frame.
function loadOuter() {
  const toChan = [];
  const toFrame = [];
  const listeners = {};
  const chan = { postMessage: (message) => toChan.push(message) };
  const liveFrame = { contentWindow: { postMessage: (message) => toFrame.push(message) } };
  const state = {
    context: {},
    hostKeys: [],
    frame: liveFrame,
    frameNonce: NONCE,
    viewActive: true,
    pendingLaunch: null,
  };
  const page = {
    parent: chan,
    addEventListener: (type, fn) => (listeners[type] ??= []).push(fn),
  };
  const names = HOST_CONSTANTS.map(([, name]) => name);
  const values = HOST_CONSTANTS.map(([, , literal]) => JSON.parse(literal));
  const noop = () => {};
  new Function(
    "window",
    "parent",
    "state",
    "sendHello",
    "render",
    "runCommand",
    "log",
    ...names,
    OUTER_LISTENER,
  )(page, chan, state, noop, noop, noop, noop, ...values);
  const deliver = (source, data) => {
    for (const fn of listeners.message ?? []) fn({ source, data });
  };
  return { toChan, toFrame, state, chan, liveFrame, deliver };
}

test("the outer page forwards Chan's keymap to the live frame under its nonce", () => {
  const outer = loadOuter();
  outer.deliver(outer.chan, { type: "chan:extension-host-keymap:v2", keys: ADVERTISED });
  assert.deepEqual(outer.state.hostKeys, ADVERTISED);
  assert.deepEqual(outer.toFrame, [
    { type: "chan:extension-host-keymap:v2", keys: ADVERTISED, nonce: NONCE },
  ]);
});

test("a relay from the live frame reaches Chan once, without the nonce", () => {
  const outer = loadOuter();
  const relayed = { type: "chan:extension-keydown:v2", key: "T", code: "KeyF", nonce: NONCE };
  outer.deliver(outer.liveFrame.contentWindow, relayed);
  assert.deepEqual(outer.toChan, [{ type: "chan:extension-keydown:v2", key: "T", code: "KeyF" }]);
});

test("a stale frame's relay is dropped, by source and by nonce", () => {
  const outer = loadOuter();
  const relayed = { type: "chan:extension-keydown:v2", key: "T", code: "KeyF" };
  const replacedFrame = { postMessage() {} };
  outer.deliver(replacedFrame, { ...relayed, nonce: NONCE });
  outer.deliver(outer.liveFrame.contentWindow, { ...relayed, nonce: "an-earlier-frame" });
  outer.deliver(outer.liveFrame.contentWindow, { ...relayed, type: "chan:extension-keydown:v1", nonce: NONCE });
  assert.deepEqual(outer.toChan, []);
});
