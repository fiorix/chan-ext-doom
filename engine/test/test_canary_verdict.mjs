// Host tests for the automatic MATCH/MISMATCH verdict.
//
// A verdict that reads MATCH when the peers disagree is worse than no verdict
// at all, because it is evidence someone will act on. So the cases that get
// the most attention here are the ones where something is missing, stale, or
// only half-known: those must read PENDING or MISMATCH, never MATCH.

import {
  parseCanaryLine,
  normalizeRoomUrl,
  channelName,
  runKey,
  isComplete,
  compareReports,
  acceptsReport,
  VERDICT,
} from "../canary_verdict.js";

let checks = 0;
let failures = 0;

function check(ok, what) {
  checks++;
  if (!ok) {
    failures++;
    console.log("FAIL: " + what);
  }
}

const H1 = "a".repeat(64);
const H2 = "b".repeat(64);

const inputLine = (bytes, sha) => `DEMO CANARY: exit bytes=${bytes} sha256=${sha}`;
const stateLine = (e, m, t, bytes, sha) =>
  `STATE CANARY: exit episode=${e} map=${m} gametic=${t} bytes=${bytes} sha256=${sha}`;

// --- parsing ---------------------------------------------------------------

{
  const d = parseCanaryLine(inputLine(1200, H1));
  check(d !== null && d.kind === "input" && d.bytes === 1200 && d.sha256 === H1,
        "an input canary line parses");

  const s = parseCanaryLine(stateLine(1, 1, 5000, 655, H2));
  check(s !== null && s.kind === "state" && s.episode === 1 && s.map === 1 &&
        s.gametic === 5000 && s.bytes === 655 && s.sha256 === H2,
        "a state canary line parses");

  // Lines that are not results must never become results. The armed line in
  // particular appears on every recorded run.
  const notResults = [
    "DEMO CANARY: armed version=109 skill=2 episode=1 map=1 deathmatch=0",
    "DEMO CANARY: verification mode, demo-quit key unbound",
    "DEMO CANARY: exit: recording too short to compare",
    "STATE CANARY: exit: state did not fit, no digest",
    "W_Init: Init WADfiles.",
    "",
  ];
  for (const line of notResults) {
    check(parseCanaryLine(line) === null,
          `a non-result line is not parsed as a result: ${JSON.stringify(line.slice(0, 40))}`);
  }

  // Malformed digests must not slip through as results.
  check(parseCanaryLine(inputLine(10, "z".repeat(64))) === null,
        "a non-hex digest is rejected");
  check(parseCanaryLine(inputLine(10, "a".repeat(63))) === null,
        "a short digest is rejected");
  check(parseCanaryLine(inputLine(10, "a".repeat(65))) === null,
        "a long digest is rejected");
  check(parseCanaryLine(null) === null, "a null line is rejected");
  check(parseCanaryLine(12345) === null, "a non-string line is rejected");

  // Trailing junk must not be accepted, or a crafted log line could carry a
  // payload past the digest.
  check(parseCanaryLine(inputLine(10, H1) + " extra") === null,
        "trailing content after a result line is rejected");
}

// --- room normalization ----------------------------------------------------

{
  const a = "ws://127.0.0.1:8080/ws/room";
  check(normalizeRoomUrl(a + "/") === normalizeRoomUrl(a),
        "a trailing slash does not change the room");
  check(normalizeRoomUrl("  " + a + "  ") === normalizeRoomUrl(a),
        "surrounding whitespace does not change the room");
  check(normalizeRoomUrl(a + "#frag") === normalizeRoomUrl(a),
        "a fragment does not change the room");

  // Different rooms must not collide, or two unrelated games would compare.
  check(normalizeRoomUrl(a) !== normalizeRoomUrl("ws://127.0.0.1:8080/ws/other"),
        "different room paths stay distinct");
  check(normalizeRoomUrl(a) !== normalizeRoomUrl("ws://127.0.0.1:9090/ws/room"),
        "different ports stay distinct");
  check(channelName(a) === channelName(a + "/"),
        "the channel name follows the normalized room");
  check(channelName(a) !== channelName("ws://127.0.0.1:8080/ws/other"),
        "different rooms get different channels");
}

// --- completeness and correlation -----------------------------------------

const mkReport = (peer, e, m, t, inSha, stSha) => ({
  peer,
  input: { kind: "input", bytes: 1200, sha256: inSha },
  state: { kind: "state", episode: e, map: m, gametic: t, bytes: 655, sha256: stSha },
});

{
  check(!isComplete(null), "a null report is incomplete");
  check(!isComplete({ input: { sha256: H1 } }), "input alone is incomplete");
  check(!isComplete({ state: { sha256: H1 } }), "state alone is incomplete");
  check(isComplete(mkReport("p", 1, 1, 5000, H1, H2)), "both halves make it complete");

  check(runKey(mkReport("p", 1, 1, 5000, H1, H2)) === "1:1:5000", "the run key is episode:map:gametic");
  check(runKey({}) === null, "a report with no state has no run key");
}

// --- the verdict -----------------------------------------------------------

{
  const mine = mkReport("me", 1, 1, 5000, H1, H2);

  // Agreement.
  let v = compareReports(mine, mkReport("you", 1, 1, 5000, H1, H2));
  check(v.input === VERDICT.MATCH && v.state === VERDICT.MATCH &&
        v.aggregate === VERDICT.MATCH, "identical reports aggregate to MATCH");

  // Input diverged, state agreed: the aggregate must not read MATCH.
  v = compareReports(mine, mkReport("you", 1, 1, 5000, "c".repeat(64), H2));
  check(v.input === VERDICT.MISMATCH && v.state === VERDICT.MATCH &&
        v.aggregate === VERDICT.MISMATCH,
        "an input divergence alone makes the aggregate MISMATCH");

  // State diverged, input agreed. This is the case DCS1 exists for: same
  // inputs, different simulation.
  v = compareReports(mine, mkReport("you", 1, 1, 5000, H1, "d".repeat(64)));
  check(v.input === VERDICT.MATCH && v.state === VERDICT.MISMATCH &&
        v.aggregate === VERDICT.MISMATCH,
        "a state divergence alone makes the aggregate MISMATCH");

  // Same digest, different length is still a divergence.
  const shortInput = mkReport("you", 1, 1, 5000, H1, H2);
  shortInput.input.bytes = 1199;
  v = compareReports(mine, shortInput);
  check(v.input === VERDICT.MISMATCH,
        "an equal digest with a different byte count is a mismatch");

  // Nothing from the peer yet: PENDING, never MATCH.
  v = compareReports(mine, null);
  check(v.aggregate === VERDICT.PENDING, "no peer report yet is PENDING");
  v = compareReports(mine, { peer: "you", input: mine.input });
  check(v.input === VERDICT.MATCH && v.state === VERDICT.PENDING &&
        v.aggregate === VERDICT.PENDING,
        "half a peer report cannot reach MATCH");

  // And nothing of our own yet.
  v = compareReports(null, mkReport("you", 1, 1, 5000, H1, H2));
  check(v.aggregate === VERDICT.PENDING, "no local report yet is PENDING");

  // A half-report that already disagrees is a real result and should not
  // wait for the other half.
  v = compareReports(mine, { peer: "you", input: { bytes: 1200, sha256: "e".repeat(64) } });
  check(v.aggregate === VERDICT.MISMATCH,
        "a disagreement in one half reports immediately");
}

// --- which reports are compared at all -------------------------------------

{
  const ctx = { selfPeer: "me", runKey: "1:1:5000" };

  check(acceptsReport(mkReport("you", 1, 1, 5000, H1, H2), ctx),
        "a complete report from another peer at the same exit is accepted");

  check(!acceptsReport(mkReport("me", 1, 1, 5000, H1, H2), ctx),
        "our own echo is not compared against ourselves");

  check(!acceptsReport(mkReport("you", 1, 1, 4999, H1, H2), ctx),
        "a report from a different exit gametic is not compared");
  check(!acceptsReport(mkReport("you", 1, 2, 5000, H1, H2), ctx),
        "a report from a different map is not compared");
  check(!acceptsReport(mkReport("you", 2, 1, 5000, H1, H2), ctx),
        "a report from a different episode is not compared");

  const half = mkReport("you", 1, 1, 5000, H1, H2);
  delete half.state;
  check(!acceptsReport(half, ctx), "an incomplete report is not compared");

  const anon = mkReport(undefined, 1, 1, 5000, H1, H2);
  check(!acceptsReport(anon, ctx), "a report with no peer id is not compared");

  check(!acceptsReport(null, ctx), "a null report is not compared");
  check(!acceptsReport("string", ctx), "a non-object report is not compared");
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
