// Which postMessage traffic the loader is allowed to act on.
//
// This is a pure decision so it can be tested without a browser. It was
// inline in the page, which meant the only coverage was scratch drivers that
// never got committed, and the rules are about to matter a great deal more:
// once the page derives an automatic MATCH/MISMATCH verdict from canary
// lines, anything that can inject a log line can forge acceptance evidence.
//
// Three independent facts have to line up before a message counts:
//
//   - it came from this origin;
//   - it came from the iframe this page currently owns, not a sibling, not
//     the opener, and not a frame that has already been torn down;
//   - for anything carrying run-scoped data, it belongs to the run in
//     progress rather than one that has been replaced.
//
// Everything else is ignored with a named reason, so a rejection can be
// asserted for the right cause instead of merely observed as silence.

export const GATE = {
  NO_DATA: "ignore:no-data",
  FOREIGN_ORIGIN: "ignore:foreign-origin",
  FOREIGN_SOURCE: "ignore:foreign-source",
  NO_FRAME: "ignore:no-current-frame",
  UNKNOWN_TYPE: "ignore:unknown-type",
  SEND_LAUNCH: "send-launch",
  DUPLICATE_READY: "ignore:duplicate-ready",
  ACCEPT_LOG: "accept-log",
  STALE_RUN: "ignore:stale-run",
};

// ctx:
//   expectedOrigin   this document's origin
//   origin           the event's origin
//   source           the event's source window
//   frameWindow      the current iframe's contentWindow, or null once torn down
//   currentRun       the run id in progress
//   hasPending       whether a launch is waiting to be handed to the frame
export function classifyMessage(msg, ctx) {
  if (!msg || typeof msg !== "object") return GATE.NO_DATA;

  if (ctx.origin !== ctx.expectedOrigin) return GATE.FOREIGN_ORIGIN;

  // Checked before the source comparison, because a torn-down frame leaves
  // frameWindow null and every source would otherwise compare unequal and
  // report the wrong reason.
  if (!ctx.frameWindow) return GATE.NO_FRAME;

  if (ctx.source !== ctx.frameWindow) return GATE.FOREIGN_SOURCE;

  if (msg.type === "doom-frame-ready") {
    // The launch payload is consumed once. A frame that announces itself
    // twice, or a replayed ready from a frame being replaced, must not cause
    // a second send: the frame's own guard would reject it and the run would
    // silently keep the older configuration.
    return ctx.hasPending ? GATE.SEND_LAUNCH : GATE.DUPLICATE_READY;
  }

  if (msg.type === "doom-log") {
    return msg.run === ctx.currentRun ? GATE.ACCEPT_LOG : GATE.STALE_RUN;
  }

  return GATE.UNKNOWN_TYPE;
}
