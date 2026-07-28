// System-level tests for the arm/session protocol.
//
// The classifier tests check one decision at a time. That is not enough here:
// the three-page storm was a property of how correct decisions compose across
// pages, and every individual classification in it was right. So this drives a
// message queue over the same step function the page runs and asserts what the
// system settles into, including that it settles at all.

import {
  receiveMessage,
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

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
