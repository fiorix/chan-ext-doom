// Host tests for the loader's postMessage boundary.
//
// @@server's review accepted the boundary itself but recorded that the only
// coverage was uncommitted scratch drivers. These are the committed cases.
// They matter more than ordinary input validation: the page derives a canary
// verdict from this traffic, so anything that can inject a log line can
// forge evidence that two peers agreed.

import { classifyMessage, GATE } from "../message_gate.js";

let checks = 0;
let failures = 0;

function check(ok, what) {
  checks++;
  if (!ok) {
    failures++;
    console.log("FAIL: " + what);
  }
}

function expect(msg, ctx, want, what) {
  const got = classifyMessage(msg, ctx);
  checks++;
  if (got !== want) {
    failures++;
    console.log(`FAIL: ${what}: expected ${want}, got ${got}`);
  }
}

// Stand-ins for window objects. Identity is all the gate compares.
const FRAME = { name: "current frame" };
const OLD_FRAME = { name: "torn-down frame" };
const SIBLING = { name: "sibling frame" };
const OPENER = { name: "opener" };

const ORIGIN = "http://127.0.0.1:8731";

const base = {
  expectedOrigin: ORIGIN,
  origin: ORIGIN,
  source: FRAME,
  frameWindow: FRAME,
  currentRun: 7,
  hasPending: false,
};

const ready = { type: "doom-frame-ready" };
const logLine = (run) => ({ type: "doom-log", run, stream: "out", text: "hi" });

// --- 1. origin, source and current-run all required ------------------------

expect(logLine(7), { ...base }, GATE.ACCEPT_LOG,
       "a log from the current frame, origin and run is accepted");

expect(logLine(7), { ...base, origin: "https://evil.example" },
       GATE.FOREIGN_ORIGIN, "a foreign origin is rejected");

expect(logLine(7), { ...base, origin: "http://127.0.0.1:9999" },
       GATE.FOREIGN_ORIGIN, "a different port on the same host is a foreign origin");

expect(logLine(7), { ...base, source: SIBLING }, GATE.FOREIGN_SOURCE,
       "a message from a sibling frame is rejected");

expect(logLine(6), { ...base }, GATE.STALE_RUN,
       "a log tagged with an older run is rejected");

expect(null, { ...base }, GATE.NO_DATA, "a message with no data is rejected");
expect("a string", { ...base }, GATE.NO_DATA, "a non-object payload is rejected");
expect({ type: "something-else" }, { ...base }, GATE.UNKNOWN_TYPE,
       "an unrecognised type is rejected");

// --- 2. ready is acted on once --------------------------------------------

expect(ready, { ...base, hasPending: true }, GATE.SEND_LAUNCH,
       "the first ready hands over the pending launch");

expect(ready, { ...base, hasPending: false }, GATE.DUPLICATE_READY,
       "a second ready with nothing pending does not send again");

// --- 3. relaunch, removed frame, late message ------------------------------

// After the iframe is removed the page holds no frame window. Everything
// from the old frame must be ignored, and for the right reason.
expect(logLine(7), { ...base, frameWindow: null, source: OLD_FRAME },
       GATE.NO_FRAME, "a log arriving after the frame was removed is rejected");

expect(ready, { ...base, frameWindow: null, source: OLD_FRAME, hasPending: true },
       GATE.NO_FRAME, "a ready arriving after the frame was removed is rejected");

// Relaunched: a new frame exists, and the old one is still talking.
expect(logLine(8), { ...base, currentRun: 8, source: OLD_FRAME },
       GATE.FOREIGN_SOURCE,
       "the previous frame cannot talk to the run that replaced it");

// Even with the right source, a payload from the previous run is stale.
expect(logLine(7), { ...base, currentRun: 8 }, GATE.STALE_RUN,
       "a log from the previous run is rejected after relaunch");

// The new run's own traffic is accepted, so the gate is not simply closed.
expect(logLine(8), { ...base, currentRun: 8 }, GATE.ACCEPT_LOG,
       "the new run's log is accepted after relaunch");

// --- 4. wrong run, exhaustively -------------------------------------------

for (const run of [0, 1, 6, 8, 99, -1, undefined, null, "7"]) {
  expect(logLine(run), { ...base }, GATE.STALE_RUN,
         `a log tagged ${JSON.stringify(run)} is rejected when the run is 7`);
}

// --- 5. sibling and foreign injection --------------------------------------

for (const [source, name] of [[SIBLING, "sibling"], [OPENER, "opener"],
                              [OLD_FRAME, "old frame"], [null, "null source"]]) {
  expect(logLine(7), { ...base, source }, GATE.FOREIGN_SOURCE,
         `a log injected by the ${name} is rejected`);
  expect(ready, { ...base, source, hasPending: true }, GATE.FOREIGN_SOURCE,
         `a ready injected by the ${name} is rejected`);
}

// The combination that matters most: correct origin, correct run, wrong
// window. Same-origin is not the same as same-frame, and this is the case a
// naive origin-only check would let through.
expect(logLine(7), { ...base, source: SIBLING }, GATE.FOREIGN_SOURCE,
       "same origin and same run are not enough without the right frame");

// --- 6. duplicate launch ---------------------------------------------------

// Two readys, one pending launch: exactly one send.
{
  let pending = true;
  const results = [];

  for (let i = 0; i < 3; i++) {
    const verdict = classifyMessage(ready, { ...base, hasPending: pending });
    results.push(verdict);
    if (verdict === GATE.SEND_LAUNCH) pending = false;
  }

  check(results.filter((r) => r === GATE.SEND_LAUNCH).length === 1,
        "three readys with one pending launch produce exactly one send");
  check(results[1] === GATE.DUPLICATE_READY && results[2] === GATE.DUPLICATE_READY,
        "the later readys are named as duplicates");
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
