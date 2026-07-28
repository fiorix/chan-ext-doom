// Automatic verdict over the two exit canaries.
//
// No human compares hex strings. Two peers each print an input-history digest
// and a state digest at the shared exit anchor; each page publishes its own on
// a same-origin channel keyed by the room, picks up the other's, and renders a
// literal outcome.
//
// The comparison lives here rather than in the page so it can be tested
// without two browsers. That matters more than usual: a verdict reading MATCH
// while the peers disagree is worse than no verdict, because it is evidence
// someone acts on.
//
// Correlation is a reciprocal per-launch session, not the exit tuple. An
// earlier version keyed on episode/map/gametic and claimed identical keys
// implied identical histories. That is false, and it opened a real hole: two
// different runs can reach the same map exit on the same tic, so a page
// holding a stale report would compare it against a peer's fresh run and
// report a verdict for a pairing that never existed. Both pages must now have
// armed with each other's current launch before any report is comparable, and
// either side relaunching breaks the pairing on both sides.

export const VERDICT = {
  MATCH: "MATCH",
  MISMATCH: "MISMATCH",
  PENDING: "PENDING",
};

// The literal strings the page renders. Defined here so the contract is one
// definition rather than markup that drifts away from the tests.
export const LABEL = {
  input: {
    MATCH: "INPUT MATCH",
    MISMATCH: "INPUT MISMATCH",
    PENDING: "INPUT PENDING",
  },
  state: {
    MATCH: "STATE MATCH",
    MISMATCH: "STATE MISMATCH",
    PENDING: "STATE PENDING",
  },
  aggregate: {
    MATCH: "M1 CANARY MATCH",
    MISMATCH: "M1 CANARY MISMATCH",
    PENDING: "M1 CANARY PENDING",
  },
};

export function labelFor(component, verdict) {
  const set = LABEL[component];
  return (set && set[verdict]) || null;
}

// Both canaries print at the same anchor, in a fixed shape:
//
//   DEMO CANARY: exit bytes=<n> sha256=<64 hex>
//   STATE CANARY: exit episode=<e> map=<m> gametic=<t> bytes=<n> sha256=<hex>
//
// Anything else, including the armed line and either "no digest" notice, is
// not a result and must not be parsed into one.
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

export function normalizeRoomUrl(url) {
  if (typeof url !== "string" || url.trim() === "") return "";

  let out = url.trim();
  try {
    const parsed = new URL(out);
    parsed.hash = "";
    out = parsed.toString();
  } catch {
    // Not parseable as a URL; compare what was typed rather than guessing.
  }

  return out.replace(/\/+$/, "");
}

export function channelName(roomUrl) {
  return "doomit-canary:" + normalizeRoomUrl(roomUrl);
}

export function isComplete(report) {
  return !!(report && report.input && report.state);
}

//
// Validation of anything arriving on the channel.
//
// A structured-clone object from any same-origin page reaches this channel.
// Being on it is not evidence of anything, so every field is checked before
// it can influence a verdict.
//

function isSessionId(v) {
  return typeof v === "string" && v.length > 0 && v.length <= 128;
}

function isDigest(v) {
  return typeof v === "string" && /^[0-9a-f]{64}$/.test(v);
}

function isCount(v) {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
}

function isTic(v) {
  return typeof v === "number" && Number.isSafeInteger(v);
}

export function isValidReport(r) {
  if (!r || typeof r !== "object") return false;
  if (r.type !== "report") return false;
  if (!isSessionId(r.session) || !isSessionId(r.partner)) return false;

  const i = r.input;
  const s = r.state;
  if (!i || typeof i !== "object" || i.kind !== "input") return false;
  if (!isCount(i.bytes) || !isDigest(i.sha256)) return false;

  if (!s || typeof s !== "object" || s.kind !== "state") return false;
  if (!isCount(s.bytes) || !isDigest(s.sha256)) return false;
  if (!isCount(s.episode) || !isCount(s.map) || !isTic(s.gametic)) return false;

  return true;
}

export function isValidArm(a) {
  return !!a && typeof a === "object" && a.type === "arm" &&
    isSessionId(a.session);
}

//
// Reciprocal correlation.
//
// A report counts only when it was sent by the partner we are currently armed
// with AND was addressed to our current session. One-sided knowledge is not
// enough: that is exactly what let a stale page answer a fresh run.
//

export function acceptsReport(received, ctx) {
  if (!isValidReport(received)) return false;
  if (!isSessionId(ctx.session) || !isSessionId(ctx.partner)) return false;
  if (received.session === ctx.session) return false;   // our own echo
  if (received.session !== ctx.partner) return false;   // not our partner
  if (received.partner !== ctx.session) return false;   // not addressed to us
  return true;
}

// How a page should react to an arm. Returning an explicit action keeps the
// page from re-deriving the rules and lets the tests name them.
export const ARM = {
  IGNORE_INVALID: "arm:invalid",
  IGNORE_SELF: "arm:self",
  IGNORE_KNOWN: "arm:already-partnered",
  ADOPT: "arm:adopt",
};

export function classifyArm(received, ctx) {
  if (!isValidArm(received)) return ARM.IGNORE_INVALID;
  if (received.session === ctx.session) return ARM.IGNORE_SELF;
  // A repeat from the partner we already hold must not re-clear state or
  // trigger another reply, or two pages ping-pong forever.
  if (received.session === ctx.partner) return ARM.IGNORE_KNOWN;
  return ARM.ADOPT;
}

function compareOne(mine, theirs) {
  if (!mine || !theirs) return VERDICT.PENDING;
  return (mine.sha256 === theirs.sha256 && mine.bytes === theirs.bytes)
    ? VERDICT.MATCH : VERDICT.MISMATCH;
}

// Pessimistic by design: MATCH only when both components match, MISMATCH as
// soon as either does even if the other is still unknown, PENDING otherwise.
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

  return {
    input,
    state,
    aggregate,
    labels: {
      input: LABEL.input[input],
      state: LABEL.state[state],
      aggregate: LABEL.aggregate[aggregate],
    },
  };
}

// One engine launch can reach more than one exit. Pairing a later exit's
// input digest with an earlier exit's state digest would publish a report
// describing no single moment, so a complete report is closed and the next
// input line starts a fresh one.
export function addCanaryLine(report, parsed) {
  if (!parsed) return report;
  if (isComplete(report)) {
    return parsed.kind === "input" ? { input: parsed } : { ...report };
  }
  return { ...(report || {}), [parsed.kind]: parsed };
}
