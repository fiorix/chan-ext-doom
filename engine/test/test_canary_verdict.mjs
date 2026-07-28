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
  isComplete,
  compareReports,
  acceptsReport,
  classifyArm,
  isValidReport,
  isValidArm,
  addCanaryLine,
  labelFor,
  LABEL,
  ARM,
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

// --- completeness -----------------------------------------------------------

// Reports now carry a reciprocal session pair: who sent it, and which launch
// it is addressed to.
const mkReport = (session, partner, inSha, stSha, over = {}) => ({
  type: "report",
  session,
  partner,
  input: { kind: "input", bytes: 1200, sha256: inSha },
  state: {
    kind: "state", episode: 1, map: 1, gametic: 5000, bytes: 655, sha256: stSha,
  },
  ...over,
});

{
  check(!isComplete(null), "a null report is incomplete");
  check(!isComplete({ input: { sha256: H1 } }), "input alone is incomplete");
  check(!isComplete({ state: { sha256: H1 } }), "state alone is incomplete");
  check(isComplete(mkReport("a", "b", H1, H2)), "both halves make it complete");
}

// --- the verdict ------------------------------------------------------------

{
  const mine = mkReport("me", "you", H1, H2);

  let v = compareReports(mine, mkReport("you", "me", H1, H2));
  check(v.input === VERDICT.MATCH && v.state === VERDICT.MATCH &&
        v.aggregate === VERDICT.MATCH, "identical reports aggregate to MATCH");

  v = compareReports(mine, mkReport("you", "me", "c".repeat(64), H2));
  check(v.input === VERDICT.MISMATCH && v.state === VERDICT.MATCH &&
        v.aggregate === VERDICT.MISMATCH,
        "an input divergence alone makes the aggregate MISMATCH");

  // Same inputs, different simulation: the case DCS1 exists for.
  v = compareReports(mine, mkReport("you", "me", H1, "d".repeat(64)));
  check(v.input === VERDICT.MATCH && v.state === VERDICT.MISMATCH &&
        v.aggregate === VERDICT.MISMATCH,
        "a state divergence alone makes the aggregate MISMATCH");

  const shortInput = mkReport("you", "me", H1, H2);
  shortInput.input.bytes = 1199;
  check(compareReports(mine, shortInput).input === VERDICT.MISMATCH,
        "an equal digest with a different byte count is a mismatch");

  check(compareReports(mine, null).aggregate === VERDICT.PENDING,
        "no peer report yet is PENDING");
  check(compareReports(null, mkReport("you", "me", H1, H2)).aggregate === VERDICT.PENDING,
        "no local report yet is PENDING");

  v = compareReports(mine, { input: mine.input });
  check(v.input === VERDICT.MATCH && v.state === VERDICT.PENDING &&
        v.aggregate === VERDICT.PENDING, "half a peer report cannot reach MATCH");

  v = compareReports(mine, { input: { bytes: 1200, sha256: "e".repeat(64) } });
  check(v.aggregate === VERDICT.MISMATCH,
        "a disagreement in one half reports immediately");
}

// --- the literal UI contract -----------------------------------------------

{
  const agree = compareReports(mkReport("me", "you", H1, H2),
                               mkReport("you", "me", H1, H2));
  check(agree.labels.input === "INPUT MATCH", "input match renders INPUT MATCH");
  check(agree.labels.state === "STATE MATCH", "state match renders STATE MATCH");
  check(agree.labels.aggregate === "M1 CANARY MATCH",
        "both matching renders M1 CANARY MATCH");

  const diverged = compareReports(mkReport("me", "you", H1, H2),
                                  mkReport("you", "me", H1, "d".repeat(64)));
  check(diverged.labels.input === "INPUT MATCH", "input still renders INPUT MATCH");
  check(diverged.labels.state === "STATE MISMATCH",
        "state divergence renders STATE MISMATCH");
  check(diverged.labels.aggregate === "M1 CANARY MISMATCH",
        "a divergence renders M1 CANARY MISMATCH, never M1 CANARY MATCH");

  const waiting = compareReports(mkReport("me", "you", H1, H2), null);
  check(waiting.labels.input === "INPUT PENDING", "pending input renders INPUT PENDING");
  check(waiting.labels.state === "STATE PENDING", "pending state renders STATE PENDING");
  check(waiting.labels.aggregate === "M1 CANARY PENDING",
        "pending aggregate renders M1 CANARY PENDING");

  check(labelFor("aggregate", VERDICT.MATCH) === "M1 CANARY MATCH",
        "labelFor exposes the aggregate contract");
  check(labelFor("nonsense", VERDICT.MATCH) === null,
        "labelFor rejects an unknown component");

  // The aggregate MATCH string must be reachable only through both matching.
  for (const [mine, theirs] of [
    [mkReport("me", "you", H1, H2), mkReport("you", "me", H1, "d".repeat(64))],
    [mkReport("me", "you", H1, H2), mkReport("you", "me", "c".repeat(64), H2)],
    [mkReport("me", "you", H1, H2), null],
  ]) {
    check(compareReports(mine, theirs).labels.aggregate !== LABEL.aggregate.MATCH,
          "M1 CANARY MATCH is not rendered unless both components match");
  }
}

// --- report validation ------------------------------------------------------

// Anything on a same-origin channel reaches here. Being on it proves nothing.
{
  check(isValidReport(mkReport("a", "b", H1, H2)), "a well-formed report validates");

  const bad = [
    [null, "null"],
    ["string", "a string"],
    [{ ...mkReport("a", "b", H1, H2), type: "arm" }, "the wrong type tag"],
    [mkReport("", "b", H1, H2), "an empty session id"],
    [mkReport("a", "", H1, H2), "an empty partner id"],
    [mkReport("a".repeat(200), "b", H1, H2), "an oversized session id"],
    [mkReport("a", "b", "Z".repeat(64), H2), "a non-hex digest"],
    [mkReport("a", "b", H1.toUpperCase(), H2), "an uppercase digest"],
    [mkReport("a", "b", H1.slice(0, 63), H2), "a short digest"],
  ];
  for (const [r, why] of bad) {
    check(!isValidReport(r), `a report with ${why} is rejected`);
  }

  const numeric = [
    ["input", "bytes", -1, "a negative byte count"],
    ["input", "bytes", 1.5, "a fractional byte count"],
    ["input", "bytes", Infinity, "an infinite byte count"],
    ["input", "bytes", NaN, "a NaN byte count"],
    ["input", "bytes", Number.MAX_SAFE_INTEGER + 2, "an unsafe byte count"],
    ["input", "bytes", "1200", "a string byte count"],
    ["state", "episode", -1, "a negative episode"],
    ["state", "map", Infinity, "an infinite map"],
    ["state", "gametic", 1.5, "a fractional gametic"],
    ["state", "gametic", NaN, "a NaN gametic"],
  ];
  for (const [section, field, value, why] of numeric) {
    const r = mkReport("a", "b", H1, H2);
    r[section][field] = value;
    check(!isValidReport(r), `a report with ${why} is rejected`);
  }

  // A negative gametic is legal; only non-integers are not.
  const negTic = mkReport("a", "b", H1, H2);
  negTic.state.gametic = -1;
  check(isValidReport(negTic), "a negative gametic is allowed");

  const missing = mkReport("a", "b", H1, H2);
  delete missing.state;
  check(!isValidReport(missing), "a half report is rejected");

  const wrongKind = mkReport("a", "b", H1, H2);
  wrongKind.input.kind = "state";
  check(!isValidReport(wrongKind), "a mislabelled section is rejected");
}

// --- arm handling -----------------------------------------------------------

{
  const ctx = { session: "me", partner: null };

  check(classifyArm({ type: "arm", session: "you" }, ctx) === ARM.ADOPT,
        "a new peer's arm is adopted");
  check(classifyArm({ type: "arm", session: "me" }, ctx) === ARM.IGNORE_SELF,
        "our own arm is ignored");
  check(classifyArm({ type: "arm", session: "you" },
                    { session: "me", partner: "you" }) === ARM.IGNORE_KNOWN,
        "a repeat from our current partner is ignored, so arms cannot ping-pong");
  check(classifyArm({ type: "arm", session: "other" },
                    { session: "me", partner: "you" }) === ARM.ADOPT,
        "a different peer's arm replaces the partner");

  for (const junk of [null, "arm", { type: "report", session: "x" },
                      { type: "arm" }, { type: "arm", session: 5 },
                      { type: "arm", session: "" }]) {
    check(classifyArm(junk, ctx) === ARM.IGNORE_INVALID,
          `a malformed arm is ignored: ${JSON.stringify(junk)}`);
  }

  check(isValidArm({ type: "arm", session: "x" }), "a well-formed arm validates");
  check(!isValidArm({ type: "arm", session: "x".repeat(200) }),
        "an oversized arm session is rejected");
}

// --- the stale-correlation scenario this protocol exists for -----------------

// Window A holds a complete report from an old launch. Window B relaunches,
// takes a new session, and reaches the same map exit on the same gametic.
// Under exit-tuple correlation both pages accepted each other and could show
// MATCH for a pairing that never existed.
{
  const A_OLD = "A-launch-1";
  const B_OLD = "B-launch-1";
  const B_NEW = "B-launch-2";

  // A is still armed with B's OLD session and holds a report addressed to it.
  const aCtx = { session: A_OLD, partner: B_OLD };

  // B's new run publishes, addressed to whoever B is now armed with. B has
  // cleared its partner on relaunch, so it cannot even address A yet.
  const bNewUnarmed = mkReport(B_NEW, "", H1, H2);
  check(!acceptsReport(bNewUnarmed, aCtx),
        "a fresh run that has not armed yet cannot be accepted");

  // Even once B re-arms and addresses A, A is armed with B's OLD session, so
  // A must reject until A adopts the new arm.
  const bNewAddressed = mkReport(B_NEW, A_OLD, H1, H2);
  check(!acceptsReport(bNewAddressed, aCtx),
        "a fresh run from a peer we are not armed with is rejected");

  // And the reverse: B's new launch must not accept A's stale report, which
  // is addressed to B's previous session.
  const aStale = mkReport(A_OLD, B_OLD, H1, H2);
  check(!acceptsReport(aStale, { session: B_NEW, partner: A_OLD }),
        "a report addressed to our previous launch is rejected");

  // Only when both sides hold each other's current session does it count.
  check(acceptsReport(mkReport(B_NEW, A_OLD, H1, H2),
                      { session: A_OLD, partner: B_NEW }),
        "a reciprocally armed report is accepted");

  // Same exit tuple, different runs: no longer sufficient on its own.
  check(!acceptsReport(mkReport("someone-else", A_OLD, H1, H2),
                       { session: A_OLD, partner: B_NEW }),
        "matching the exit tuple is not enough without the right session");
}

// One-sided and both-sided relaunch, and partner replacement.
{
  const A1 = "A1", B1 = "B1", B2 = "B2", A2 = "A2";

  // One-sided: B relaunches. A adopts B2, which must clear A's partner state.
  check(classifyArm({ type: "arm", session: B2 },
                    { session: A1, partner: B1 }) === ARM.ADOPT,
        "a partner's relaunch is adopted as a new pairing");
  check(!acceptsReport(mkReport(B1, A1, H1, H2), { session: A1, partner: B2 }),
        "after adopting the new pairing, the old partner's report is rejected");

  // Both-sided: neither old session can reach the other.
  check(!acceptsReport(mkReport(B1, A1, H1, H2), { session: A2, partner: B2 }),
        "after both relaunch, an old report is rejected");
  check(acceptsReport(mkReport(B2, A2, H1, H2), { session: A2, partner: B2 }),
        "after both relaunch, the new pairing is accepted");

  // A third page joining replaces the partner rather than being ignored.
  check(classifyArm({ type: "arm", session: "C1" },
                    { session: A1, partner: B1 }) === ARM.ADOPT,
        "a different partner replaces the current one");
  check(!acceptsReport(mkReport(B1, A1, H1, H2), { session: A1, partner: "C1" }),
        "the replaced partner's report is no longer accepted");
}

// An unarmed page compares nothing at all.
{
  check(!acceptsReport(mkReport("them", "us", H1, H2),
                       { session: "us", partner: null }),
        "a page with no partner accepts nothing");
  check(!acceptsReport(mkReport("them", "us", H1, H2),
                       { session: null, partner: "them" }),
        "a page with no session of its own accepts nothing");
  check(!acceptsReport(mkReport("us", "us", H1, H2),
                       { session: "us", partner: "us" }),
        "our own echo is never accepted");
}

// --- multi-exit hybrids -----------------------------------------------------

// One launch can reach more than one exit. A later exit's input digest must
// never be published alongside the previous exit's state digest.
{
  const in1 = parseCanaryLine(inputLine(1200, H1));
  const st1 = parseCanaryLine(stateLine(1, 1, 5000, 655, H2));
  const in2 = parseCanaryLine(inputLine(2400, "9".repeat(64)));
  const st2 = parseCanaryLine(stateLine(1, 2, 9000, 700, "8".repeat(64)));

  let r = addCanaryLine(null, in1);
  check(!isComplete(r), "one line does not complete a report");
  r = addCanaryLine(r, st1);
  check(isComplete(r) && r.state.gametic === 5000, "both lines complete it");

  // The next exit's input starts over rather than joining the old state.
  const r2 = addCanaryLine(r, in2);
  check(!isComplete(r2), "a new input after a complete report starts a fresh one");
  check(r2.input.bytes === 2400 && !r2.state,
        "the fresh report carries the new input and no stale state");

  const r3 = addCanaryLine(r2, st2);
  check(isComplete(r3) && r3.state.gametic === 9000 && r3.input.bytes === 2400,
        "the second exit reports its own pair");
  check(r3.state.sha256 !== st1.sha256, "the second report does not reuse the first state");

  check(addCanaryLine(r, null) === r, "an unparsed line changes nothing");
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
