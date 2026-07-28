// System-level tests for the arm/session protocol.
//
// The classifier tests check one decision at a time. That is not enough here:
// the three-page storm was a property of how correct decisions compose across
// pages, and every individual classification in it was right. So this drives a
// message queue over the same step function the page runs and asserts what the
// system settles into, including that it settles at all.

import {
  receiveMessage,
  receiveCanaryLine,
  armMessage,
  publishOutbound,
  armAdopts,
  compareReports,
  isComplete,
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

const report = (inSha = H1, stSha = H2) => ({
  input: { kind: "input", bytes: 1200, sha256: inSha },
  state: {
    kind: "state", episode: 1, map: 1, gametic: 5000, bytes: 655, sha256: stSha,
  },
});

// A world of pages sharing one broadcast queue. Delivery order is the queue's
// order, which is what the browser gives us; the reordering case below drives
// a different order deliberately.
function makeWorld() {
  const pages = new Map();
  const queue = [];
  let delivered = 0;
  const actions = [];

  function broadcast(fromId, msg) {
    for (const id of pages.keys()) {
      if (id !== fromId) queue.push({ to: id, msg });
    }
  }

  return {
    pages,
    queue,
    actions,
    get delivered() { return delivered; },

    add(id, session) {
      pages.set(id, { session, replaces: null, partner: null,
                      myReport: null, peerReport: null });
      broadcast(id, armMessage(pages.get(id)));
    },

    // A relaunch keeps the old session id long enough to name it, so the
    // actual partner can tell this apart from an unrelated page arriving.
    relaunch(id, session) {
      const prev = pages.get(id);
      const next = { session, replaces: prev.session, partner: null,
                     myReport: null, peerReport: null };
      pages.set(id, next);
      broadcast(id, armMessage(next));
    },

    setReport(id, r) {
      const p = pages.get(id);
      p.myReport = r;
      for (const m of publishOutbound(p)) broadcast(id, m);
    },

    // Feeds one engine log line through the same reducer the page uses, so
    // the local half of the protocol is exercised rather than simulated.
    line(id, text) {
      const r = receiveCanaryLine(pages.get(id), text);
      pages.set(id, r.state);
      actions.push(`${id}:${r.action}`);
      for (const out of r.outbound) broadcast(id, out);
      return r.action;
    },

    // One whole exit, as the engine emits it: input line then state line.
    finishExit(id, tic, inSha = H1, stSha = H2) {
      this.line(id, `DEMO CANARY: exit bytes=1200 sha256=${inSha}`);
      this.line(id,
        `STATE CANARY: exit episode=1 map=1 gametic=${tic} bytes=655 sha256=${stSha}`);
    },

    send(fromId, msg) { broadcast(fromId, msg); },

    // Runs to quiescence, or gives up. Giving up is the failure this file
    // exists to catch.
    drain(limit = 500) {
      let steps = 0;
      while (queue.length && steps < limit) {
        const { to, msg } = queue.shift();
        steps++;
        delivered++;
        const r = receiveMessage(pages.get(to), msg);
        actions.push(`${to}:${r.action}`);
        pages.set(to, r.state);
        for (const out of r.outbound) broadcast(to, out);
      }
      return { drained: queue.length === 0, steps };
    },

    verdict(id) {
      const p = pages.get(id);
      return compareReports(p.myReport, p.peerReport).aggregate;
    },
    partner(id) { return pages.get(id).partner; },
    hasReport(id) { return isComplete(pages.get(id).myReport); },
    countActions(suffix) {
      return actions.filter((a) => a.endsWith(suffix)).length;
    },
  };
}

// --- 1. two pages converge --------------------------------------------------

{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  const { drained, steps } = w.drain();

  check(drained, "two pages: the queue drains");
  check(steps < 20, `two pages: it settles quickly (${steps} deliveries)`);
  check(w.partner("A") === "B1" && w.partner("B") === "A1",
        "two pages: each holds the other as partner");

  w.setReport("A", report());
  w.setReport("B", report());
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH && w.verdict("B") === VERDICT.MATCH,
        "two pages: agreeing reports reach MATCH on both");
}

// --- 2. one-sided relaunch replaces the named session on both sides ---------

{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();
  w.setReport("A", report());
  w.setReport("B", report());
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH, "one-sided: MATCH before the relaunch");

  w.relaunch("B", "B2");
  const { drained } = w.drain();

  check(drained, "one-sided: the queue drains after a relaunch");
  check(w.partner("A") === "B2", "one-sided: A adopts the replacement session");
  check(w.partner("B") === "A1", "one-sided: B re-partners with A");
  check(!w.hasReport("A"), "one-sided: A's stale report is cleared on A's side");
  check(w.verdict("A") === VERDICT.PENDING && w.verdict("B") === VERDICT.PENDING,
        "one-sided: the old MATCH is gone on both sides");

  w.setReport("A", report());
  w.setReport("B", report());
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH && w.verdict("B") === VERDICT.MATCH,
        "one-sided: the new pairing can reach MATCH");
}

// --- 3. simultaneous relaunch converges -------------------------------------

{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();

  w.relaunch("A", "A2");
  w.relaunch("B", "B2");
  const { drained, steps } = w.drain();

  check(drained, "simultaneous: the queue drains");
  check(steps < 40, `simultaneous: it settles (${steps} deliveries)`);
  check(w.partner("A") === "B2" && w.partner("B") === "A2",
        "simultaneous: both end on the other's new session");

  w.setReport("A", report());
  w.setReport("B", report());
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH, "simultaneous: the new pairing matches");
}

// --- 4. a third page cannot disturb an established pair ---------------------

// This is the case that used to storm forever.
{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();
  w.setReport("A", report());
  w.setReport("B", report());
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH, "third page: the pair matches first");

  w.add("C", "C1");
  const { drained, steps } = w.drain();

  check(drained, "third page: the queue drains instead of storming");
  check(steps < 30, `third page: it settles quickly (${steps} deliveries)`);
  check(w.partner("A") === "B1" && w.partner("B") === "A1",
        "third page: the pair keeps its partners");
  check(w.verdict("A") === VERDICT.MATCH && w.verdict("B") === VERDICT.MATCH,
        "third page: the established MATCH survives");
  check(w.hasReport("A") && w.hasReport("B"),
        "third page: neither page's report was cleared");
  check(w.countActions(":arm:unrelated") > 0,
        "third page: the pair names the arm as unrelated rather than silently dropping it");
  check(w.verdict("C") === VERDICT.PENDING,
        "third page: the newcomer stays pending, since nobody reciprocates");
}

// A third page must not be able to displace a partner by guessing either.
{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();
  w.setReport("A", report());
  w.setReport("B", report());
  w.drain();

  // C claims to replace B, which it is not.
  w.pages.set("C", { session: "C1", replaces: null, partner: null,
                     myReport: null, peerReport: null });
  w.send("C", { type: "arm", session: "C1", replaces: "B1" });
  w.drain();

  check(w.partner("A") === "C1",
        "a page naming our partner does take its place, which is the protocol's trust boundary");
  check(w.verdict("B") === VERDICT.MATCH,
        "B, which was not named, keeps its verdict");
}

// --- 5. duplicates, reordering and late unrelated arms ----------------------

{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();
  w.setReport("A", report());
  w.setReport("B", report());
  w.drain();
  const before = w.verdict("A");

  // The same arm again, several times.
  for (let i = 0; i < 5; i++) w.send("B", armMessage(w.pages.get("B")));
  const dup = w.drain();
  check(dup.drained, "duplicates: the queue drains");
  check(w.verdict("A") === before, "duplicates: the verdict does not roll back");
  check(w.countActions(":arm:already-partnered") >= 5,
        "duplicates: repeats are named as already-partnered");

  // A late arm from a session nobody is partnered with.
  w.send("A", { type: "arm", session: "ghost", replaces: null });
  w.drain();
  check(w.verdict("B") === before, "late unrelated arm: no rollback");
  check(w.partner("B") === "A1", "late unrelated arm: partner unchanged");

  // Reordered delivery: an arm queued behind a report.
  w.queue.push({ to: "A", msg: { type: "arm", session: "ghost2", replaces: null } });
  w.queue.unshift({ to: "A", msg: { type: "arm", session: "ghost3", replaces: null } });
  const re = w.drain();
  check(re.drained, "reordered: the queue drains");
  check(w.partner("A") === "B1", "reordered: partner still intact");
}

// --- 6. a replacement naming the wrong session is ignored -------------------

{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();

  // Names a session that is not A's partner.
  w.pages.set("D", { session: "D1", replaces: "someone-else", partner: null,
                     myReport: null, peerReport: null });
  w.send("D", { type: "arm", session: "D1", replaces: "someone-else" });
  const { drained } = w.drain();

  check(drained, "wrong replaces: the queue drains");
  check(w.partner("A") === "B1" && w.partner("B") === "A1",
        "wrong replaces: neither page changes partner");
  check(w.countActions(":arm:unrelated") > 0,
        "wrong replaces: it is named unrelated");
}

// --- 7. reports from replaced sessions stay rejected -------------------------

{
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();
  w.relaunch("B", "B2");
  w.drain();

  // B's old session tries to report to A.
  w.send("B", {
    type: "report", session: "B1", partner: "A1", ...report(),
  });
  w.drain();
  check(w.verdict("A") === VERDICT.PENDING,
        "a report from a replaced session is rejected");
  check(w.countActions(":report:rejected") > 0,
        "the rejection is named rather than silent");

  // And a report addressed to a session that has been replaced.
  w.relaunch("A", "A2");
  w.drain();
  w.send("B", {
    type: "report", session: w.pages.get("B").session, partner: "A1", ...report(),
  });
  w.drain();
  check(w.verdict("A") === VERDICT.PENDING,
        "a report addressed to a replaced session is rejected");
}

// --- convergence under many pages -------------------------------------------

// Not required, but the storm was a scaling property, so it is worth knowing
// the protocol does not merely survive three.
{
  const w = makeWorld();
  for (let i = 0; i < 8; i++) w.add("P" + i, "S" + i);
  const { drained, steps } = w.drain(5000);
  check(drained, `eight pages: the queue drains (${steps} deliveries)`);
  check(w.countActions(":arm:adopt") <= 8,
        "eight pages: adoptions are bounded by the page count");
}

// --- multi-exit: both finish orders must converge ---------------------------

// The defect this section exists for: a peer that finished an exit first had
// its report discarded when the local page started that same exit, and since
// receiving a report never triggers a reply, the second finisher waited for
// something already delivered.

function pairedWorld() {
  const w = makeWorld();
  w.add("A", "A1");
  w.add("B", "B1");
  w.drain();
  return w;
}

// 1. first exit, each finish order
for (const [first, second] of [["A", "B"], ["B", "A"]]) {
  const w = pairedWorld();
  w.finishExit(first, 1000);
  w.drain();
  check(w.verdict(first) === VERDICT.PENDING,
        `first exit, ${first} first: the first finisher waits`);

  w.finishExit(second, 1000);
  const { drained } = w.drain();

  check(drained, `first exit, ${first} first: the queue drains`);
  check(w.verdict("A") === VERDICT.MATCH && w.verdict("B") === VERDICT.MATCH,
        `first exit, ${first} first: both reach MATCH`);
}

// 2. second and third exits, both orders
for (const [first, second] of [["A", "B"], ["B", "A"]]) {
  const w = pairedWorld();

  for (const [n, tic] of [[1, 1000], [2, 2000], [3, 3000]]) {
    const inSha = String(n).repeat(64).slice(0, 64);
    const stSha = String((n + 4) % 10).repeat(64).slice(0, 64);

    w.finishExit(first, tic, inSha, stSha);
    w.drain();
    w.finishExit(second, tic, inSha, stSha);
    const { drained } = w.drain();

    check(drained, `exit ${n}, ${first} first: the queue drains`);
    check(w.verdict("A") === VERDICT.MATCH && w.verdict("B") === VERDICT.MATCH,
          `exit ${n}, ${first} first: both reach MATCH`);
  }
}

// 3. a peer report arriving before local input, between input and state, and
//    after local state
{
  // before local input
  let w = pairedWorld();
  w.finishExit("A", 1000);
  w.drain();
  w.finishExit("B", 1000);
  w.drain();
  check(w.verdict("B") === VERDICT.MATCH, "peer report before local input: MATCH");

  // between local input and local state
  w = pairedWorld();
  w.line("B", `DEMO CANARY: exit bytes=1200 sha256=${H1}`);
  w.finishExit("A", 1000);
  w.drain();
  check(w.verdict("B") === VERDICT.PENDING,
        "peer report mid-report: still pending until the local pair completes");
  w.line("B", `STATE CANARY: exit episode=1 map=1 gametic=1000 bytes=655 sha256=${H2}`);
  w.drain();
  check(w.verdict("B") === VERDICT.MATCH, "peer report mid-report: MATCH once complete");

  // after local state
  w = pairedWorld();
  w.finishExit("B", 1000);
  w.drain();
  w.finishExit("A", 1000);
  w.drain();
  check(w.verdict("B") === VERDICT.MATCH, "peer report after local state: MATCH");
}

// 4. peers temporarily at different exits: PENDING, never a false MISMATCH
{
  const w = pairedWorld();
  w.finishExit("A", 1000);
  w.drain();
  w.finishExit("B", 1000);
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH, "different exits: matched at the first exit");

  // A runs ahead to the next exit with entirely different digests.
  w.finishExit("A", 2000, "7".repeat(64), "8".repeat(64));
  w.drain();
  check(w.verdict("B") !== VERDICT.MISMATCH,
        "different exits: B does not report a divergence merely for being behind");
  check(w.verdict("A") === VERDICT.PENDING,
        "different exits: A waits rather than comparing across exits");

  // B catches up with the same digests: they agree.
  w.finishExit("B", 2000, "7".repeat(64), "8".repeat(64));
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH && w.verdict("B") === VERDICT.MATCH,
        "different exits: both match once B reaches the same exit");
}

// A real divergence at the same exit must still be reported.
{
  const w = pairedWorld();
  w.finishExit("A", 1000, H1, H2);
  w.drain();
  w.finishExit("B", 1000, H1, "9".repeat(64));
  w.drain();
  check(w.verdict("A") === VERDICT.MISMATCH && w.verdict("B") === VERDICT.MISMATCH,
        "a genuine divergence at the same exit is still MISMATCH on both");
}

// 5. duplicate and reordered peer reports
{
  const w = pairedWorld();
  w.finishExit("A", 1000);
  w.drain();
  w.finishExit("B", 1000);
  w.drain();
  const settled = w.verdict("B");

  // The same report again.
  const aState = w.pages.get("A");
  for (let i = 0; i < 3; i++) {
    w.send("A", { type: "report", session: aState.session, partner: aState.partner,
                  input: aState.myReport.input, state: aState.myReport.state });
  }
  w.drain();
  check(w.verdict("B") === settled, "duplicate peer reports do not change the verdict");

  // Both move to the next exit, then a stale report for the previous one
  // arrives late and must not displace the newer result.
  w.finishExit("A", 2000, "3".repeat(64), "4".repeat(64));
  w.drain();
  w.finishExit("B", 2000, "3".repeat(64), "4".repeat(64));
  w.drain();
  check(w.verdict("B") === VERDICT.MATCH, "second exit matches");

  w.send("A", { type: "report", session: aState.session, partner: aState.partner,
                input: { kind: "input", bytes: 1200, sha256: H1 },
                state: { kind: "state", episode: 1, map: 1, gametic: 1000,
                         bytes: 655, sha256: H2 } });
  w.drain();
  check(w.verdict("B") === VERDICT.MATCH,
        "a late report for an older exit does not displace the newer one");
  check(w.countActions(":report:stale") > 0, "the stale report is named as such");
}

// 6. no publish storm: a received report never causes another report
{
  const w = pairedWorld();
  const before = w.countActions(":report:accepted");
  w.finishExit("A", 1000);
  w.drain();
  w.finishExit("B", 1000);
  const { drained, steps } = w.drain();

  check(drained, "no storm: the queue drains");
  check(steps < 20, `no storm: it settles in ${steps} deliveries`);
  // Two complete reports, so each side accepts at most one.
  check(w.countActions(":report:accepted") - before <= 2,
        "no storm: each side accepts one report, and answers none");
}

// Bounded storage: many exits must not accumulate state.
{
  const w = pairedWorld();
  for (let n = 1; n <= 12; n++) {
    const sha = String(n % 10).repeat(64);
    w.finishExit("A", n * 1000, sha, sha);
    w.drain();
    w.finishExit("B", n * 1000, sha, sha);
    w.drain();
    check(w.verdict("A") === VERDICT.MATCH, `exit ${n}: still matching`);
  }
  const p = w.pages.get("B");
  check(Object.keys(p).length <= 6,
        "twelve exits later the page still holds a fixed set of fields");
  check(p.peerReport && p.peerReport.state.gametic === 12000,
        "and only the newest peer report");
}

// --- a refused digest can never become a MATCH -----------------------------

// When the exit state does not fit the serializer's scratch buffer the engine
// prints a refusal instead of a result. That line carries no digest, so there
// is nothing to compare; the danger would be treating "both sides said the
// same thing" as agreement. Silence is not agreement.

const REFUSAL = "STATE CANARY: exit: state did not fit, no digest";

{
  // Both peers refuse, with byte-identical text.
  const w = pairedWorld();
  w.line("A", `DEMO CANARY: exit bytes=1200 sha256=${H1}`);
  w.line("A", REFUSAL);
  w.drain();
  w.line("B", `DEMO CANARY: exit bytes=1200 sha256=${H1}`);
  w.line("B", REFUSAL);
  w.drain();

  check(w.verdict("A") === VERDICT.PENDING && w.verdict("B") === VERDICT.PENDING,
        "two identical refusals are not a MATCH");
  check(!w.hasReport("A") && !w.hasReport("B"),
        "a refusal never completes a report");
}

{
  // One peer refuses, the other produces a real result.
  const w = pairedWorld();
  w.finishExit("A", 1000);
  w.drain();
  w.line("B", `DEMO CANARY: exit bytes=1200 sha256=${H1}`);
  w.line("B", REFUSAL);
  const { drained } = w.drain();

  check(drained, "a one-sided refusal still settles");
  check(w.verdict("B") === VERDICT.PENDING,
        "the refusing page cannot match a complete peer");
  check(w.verdict("A") === VERDICT.PENDING,
        "and the complete page keeps waiting rather than matching a refusal");
}

{
  // The refusal must not be parsed into a partial state half either.
  const w = pairedWorld();
  check(w.line("A", REFUSAL) === "line:not-a-result",
        "the refusal is not a result line");

  // Truncated and malformed digests are equally inert.
  for (const bad of [
    "STATE CANARY: exit episode=1 map=1 gametic=10 bytes=655 sha256=",
    "STATE CANARY: exit episode=1 map=1 gametic=10 bytes=655 sha256=abc",
    `STATE CANARY: exit episode=1 map=1 gametic=10 sha256=${H2}`,
    `STATE CANARY: exit episode=1 map=1 gametic=10 bytes= sha256=${H2}`,
  ]) {
    check(w.line("A", bad) === "line:not-a-result",
          `an incomplete result is rejected: ${bad.slice(30, 70)}`);
  }
  check(w.verdict("A") === VERDICT.PENDING,
        "and none of them moved the verdict");
}

// --- known, deliberately unauthenticated boundaries ------------------------

// These two cases are not defects being fixed. They are the honest edges of a
// protocol that authenticates nobody, pinned here so they stay visible as
// known behavior instead of being rediscovered later as surprises, or worse,
// described as something the protocol protects against.

{
  // Forged replacement: any same-origin script can read a session id off the
  // channel, name the live partner in `replaces`, take its place, and then
  // reflect a report back. The victim displays a perfectly stable MATCH that
  // means nothing, and the real peer is locked out.
  const w = pairedWorld();
  w.finishExit("A", 1000);
  w.drain();
  w.finishExit("B", 1000);
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH, "the honest pair matches first");

  const bSession = w.pages.get("B").session;

  // A third page displaces B by naming B's session, which it can simply read
  // off the shared channel.
  w.add("X", "X1");
  w.send("X", { type: "arm", session: "X2", replaces: bSession });
  w.drain();
  check(w.partner("A") === "X2",
        "BOUNDARY: a same-origin page can displace the live partner");

  // Adopting a new partner clears both reports, so the reflection is not
  // instant: the victim first has to finish another exit and publish it.
  check(w.verdict("A") === VERDICT.PENDING,
        "the displaced pairing at least resets the victim to PENDING");

  w.finishExit("A", 2000, "5".repeat(64), "6".repeat(64));
  w.drain();
  const aState = w.pages.get("A");

  // Now the attacker echoes the victim's own report back at it.
  w.send("X", { type: "report", session: "X2", partner: aState.session,
                input: aState.myReport.input, state: aState.myReport.state });
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH,
        "BOUNDARY: a reflected report yields a stable but meaningless MATCH");

  // The displaced honest peer can no longer reach A.
  w.finishExit("B", 2000, "5".repeat(64), "6".repeat(64));
  w.drain();
  check(w.partner("A") === "X2",
        "BOUNDARY: the real peer stays locked out");
  // Documented in docs/provenance.md: the verdict is evidence only when every
  // script on the served origin is trusted. No in-protocol fix exists, since a
  // token would be readable through the same origin.
}

{
  // Asymmetric full reload: F5 on one window only. The reloaded page loses its
  // session and correctly shows PENDING. The surviving page is NOT reset --
  // it keeps its old MATCH against a session that no longer exists, and will
  // hold it indefinitely.
  const w = pairedWorld();
  w.finishExit("A", 1000);
  w.drain();
  w.finishExit("B", 1000);
  w.drain();
  check(w.verdict("A") === VERDICT.MATCH && w.verdict("B") === VERDICT.MATCH,
        "both windows match before the reload");

  const deadSession = w.pages.get("B").session;

  // B is reloaded from scratch: a brand new page, no memory, no `replaces`.
  w.pages.delete("B");
  w.add("B", "B-reloaded");
  w.drain();

  check(w.verdict("B") === VERDICT.PENDING,
        "the reloaded window is PENDING, having lost its identity");
  check(w.verdict("A") === VERDICT.MATCH,
        "BOUNDARY: the surviving window retains a MATCH for a dead session");
  check(w.pages.get("A").partner === deadSession,
        "BOUNDARY: the survivor still points at the session that no longer exists");
  check(w.partner("B") !== deadSession,
        "and the reloaded page did not recover the old pairing");

  // Hence the acceptance rule recorded in docs/provenance.md: a single
  // window showing MATCH proves nothing. Both live windows must show MATCH
  // simultaneously after the same shared exit. Recovery is reloading or
  // relaunching both windows, never adopting a partner on a timeout, which
  // would reopen third-page takeover.
  w.relaunch("A", "A-relaunched");
  w.drain();
  check(w.verdict("A") === VERDICT.PENDING,
        "relaunching the survivor clears the stale MATCH");
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
