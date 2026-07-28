// Automatic MATCH/MISMATCH for the two exit canaries.
//
// The point is that no human compares hex strings. Two peers each print an
// input-history digest and a state digest at the shared exit anchor; each
// page publishes its own on a same-origin channel keyed by the room, picks up
// the other's, and renders a verdict.
//
// The comparison is here rather than in the page so it can be tested without
// two browsers. That matters more than usual: a verdict that reads MATCH when
// the peers disagree is worse than no verdict at all, because it is evidence
// someone will trust.
//
// Correlation uses the room plus the exit itself (episode, map, exit gametic)
// rather than a negotiated session id. The exit is a simulation event that
// fires on the same gametic on every peer, so it identifies the run without
// either side having to agree on anything first. See acceptsReport for what
// that does and does not guarantee across a relaunch.

export const VERDICT = {
  MATCH: "MATCH",
  MISMATCH: "MISMATCH",
  PENDING: "PENDING",
};

// Both canaries print at the same anchor, in a fixed shape:
//
//   DEMO CANARY: exit bytes=<n> sha256=<64 hex>
//   STATE CANARY: exit episode=<e> map=<m> gametic=<t> bytes=<n> sha256=<hex>
//
// Anything else, including the "armed" line and the short-recording notice,
// is not a result and must not be parsed into one.
export function parseCanaryLine(text) {
  if (typeof text !== "string") return null;

  const demo = text.match(
    /^DEMO CANARY: exit bytes=(\d+) sha256=([0-9a-f]{64})$/);
  if (demo) {
    return { kind: "input", bytes: Number(demo[1]), sha256: demo[2] };
  }

  const state = text.match(
    /^STATE CANARY: exit episode=(\d+) map=(\d+) gametic=(-?\d+) bytes=(\d+) sha256=([0-9a-f]{64})$/);
  if (state) {
    return {
      kind: "state",
      episode: Number(state[1]),
      map: Number(state[2]),
      gametic: Number(state[3]),
      bytes: Number(state[4]),
      sha256: state[5],
    };
  }

  return null;
}

// Trailing slashes and case in the scheme/host are not meaningful, but the
// room path is. Two peers that typed the same room differently must land on
// the same channel, and two different rooms must not.
export function normalizeRoomUrl(url) {
  if (typeof url !== "string" || url.trim() === "") return "";

  let out = url.trim();
  try {
    const parsed = new URL(out);
    parsed.hash = "";
    out = parsed.toString();
  } catch {
    // Not parseable as a URL; compare what was typed, minus surrounding
    // whitespace, rather than guessing at it.
  }

  return out.replace(/\/+$/, "");
}

export function channelName(roomUrl) {
  return "doomit-canary:" + normalizeRoomUrl(roomUrl);
}

// Identifies one exit. Peers that reached the same exit of the same map on
// the same gametic are comparing the same thing.
export function runKey(report) {
  if (!report || !report.state) return null;
  const s = report.state;
  return `${s.episode}:${s.map}:${s.gametic}`;
}

// A report is only complete once both canaries have been seen. Comparing on
// one of them would let a page announce MATCH while the other half was still
// unknown.
export function isComplete(report) {
  return !!(report && report.input && report.state);
}

function compareOne(mine, theirs) {
  if (!mine || !theirs) return VERDICT.PENDING;
  return (mine.sha256 === theirs.sha256 && mine.bytes === theirs.bytes)
    ? VERDICT.MATCH : VERDICT.MISMATCH;
}

// The aggregate is deliberately pessimistic: it is MATCH only when both
// components are MATCH, and MISMATCH as soon as either one is, even if the
// other has not arrived. A disagreement on one canary is a real result and
// should not wait on the other to be reported.
export function compareReports(mine, theirs) {
  const input = compareOne(mine && mine.input, theirs && theirs.input);
  const state = compareOne(mine && mine.state, theirs && theirs.state);

  let aggregate;
  if (input === VERDICT.MISMATCH || state === VERDICT.MISMATCH) {
    aggregate = VERDICT.MISMATCH;
  } else if (input === VERDICT.MATCH && state === VERDICT.MATCH) {
    aggregate = VERDICT.MATCH;
  } else {
    aggregate = VERDICT.PENDING;
  }

  return { input, state, aggregate };
}

// Whether a received report should be compared against ours.
//
// Rejects our own echo, incomplete reports, and reports from a different
// exit. A launch id is deliberately NOT part of this: it is generated per
// peer, so the other side's value carries no meaning here and comparing the
// two would reject every genuine report.
//
// Staleness after a relaunch is handled by the caller clearing what it has
// stored and taking a new peer id, so nothing received before the current
// launch survives into it. The residual is narrow and worth stating: two
// separate runs that reach the same map and exit on the same gametic produce
// the same key, and would be compared against each other. That is harmless,
// because identical keys mean identical simulated histories to that point,
// which is exactly what the digests measure.
export function acceptsReport(received, ctx) {
  if (!received || typeof received !== "object") return false;
  if (received.peer === undefined || received.peer === ctx.selfPeer) return false;
  if (!isComplete(received)) return false;
  return runKey(received) === ctx.runKey;
}
