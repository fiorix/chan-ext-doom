// Network arguments for the loader, and the guard that keeps this build a
// client.
//
// Route 1 is permanently owned by the Rust room server. No browser tab hosts a
// game: every tab is a Chocolate client that addresses the server as node 1 and
// draws its own route from the transport. There is no hosting mode to choose
// and no in-band room reset, because room lifetime is membership-driven on the
// server rather than claimed by a client.
//
// The node expectation goes to every client, not only the one that happens to
// be controller at launch. CheckAutoLaunch requires both is_controller, which
// the server assigns, and a non-zero expected_nodes, which comes solely from
// -nodes. A client that never received the threshold can therefore never launch
// even after the server promotes it, so withholding it from anyone strands
// startup on controller failover.

// The room host is always node 1: the router keys on node ids, and the server
// owns that one.
export const SERVER_ROUTE = "1";

// Flags that would make this build serve rather than join. Both spellings
// matter: -privateserver implies -server, so either one claims route 1.
export const SERVER_FLAGS = ["-server", "-privateserver"];

export const MODES = ["single", "multiplayer"];

// Node-count bounds, matching the control in the page.
export const MIN_NODES = 2;
export const MAX_NODES = 8;

export function isMultiplayer(mode) {
  return mode === "multiplayer";
}

// Whether the room and node controls apply. They are live for multiplayer and
// inert for single player, which is the whole of the mode's meaning now.
export function controlsEnabled(mode) {
  return isMultiplayer(mode);
}

export function validateRoom(room) {
  const value = (room || "").trim();
  if (!value) return { ok: false, reason: "no room URL" };
  return { ok: true, value };
}

export function validateNodes(nodes) {
  const value = Number(nodes);
  if (!Number.isInteger(value) || value < MIN_NODES || value > MAX_NODES) {
    return { ok: false, reason: `node count must be ${MIN_NODES} to ${MAX_NODES}` };
  }
  return { ok: true, value };
}

// The network arguments for one launch. Single player contributes none, so a
// single-player command line carries no network flags at all.
export function netArgv(mode, room, nodes) {
  if (!isMultiplayer(mode)) return [];

  const validRoom = validateRoom(room);
  const validNodes = validateNodes(nodes);
  if (!validRoom.ok || !validNodes.ok) {
    throw new Error(validRoom.ok ? validNodes.reason : validRoom.reason);
  }

  return ["-connect", SERVER_ROUTE, "-wss", validRoom.value,
          "-nodes", String(validNodes.value)];
}

// Fail closed on the final argv, whatever assembled it.
//
// This does not trust the builder above: it inspects what is actually about to
// be launched, so a future edit that reintroduces a server flag by any route is
// refused here rather than failing at runtime. The failure it prevents is local
// rather than shared: the tab would run a second server role against route 1,
// which the Rust server owns, and the server rejects a frame whose source is 1
// and closes that one connection, so the offending tab simply cannot join.
export function serverFlagIn(argv) {
  return (argv || []).find((arg) => SERVER_FLAGS.includes(arg)) || null;
}

export function guardClientOnly(argv) {
  const found = serverFlagIn(argv);
  if (!found) return { ok: true };
  return {
    ok: false,
    flag: found,
    reason: `${found} would run a server role on route 1, which the Rust room `
          + `server owns: this build is a client of that server, and a tab `
          + `claiming route 1 cannot join`,
  };
}

// The single launch decision, covering every network reason a launch must be
// refused. The page delegates to this from both the form refresh and the
// launch button, so the two can never disagree.
//
// The last check is the load-bearing one: it inspects the argv that is about
// to be launched and requires a multiplayer launch to actually carry its
// network arguments. A builder that swallows an error and returns a
// content-only argv would otherwise pass every upstream check, because a
// present room and an absent server flag are both individually fine, and the
// game would start with no connection at all.
export function networkBlockedReason(mode, room, nodes, argv) {
  if (isMultiplayer(mode)) {
    const validRoom = validateRoom(room);
    if (!validRoom.ok) return validRoom.reason;

    const validNodes = validateNodes(nodes);
    if (!validNodes.ok) return validNodes.reason;
  }

  const guard = guardClientOnly(argv);
  if (!guard.ok) return guard.reason;

  if (isMultiplayer(mode)) {
    for (const flag of ["-connect", "-wss", "-nodes"]) {
      if (!(argv || []).includes(flag)) {
        return `multiplayer launch is missing ${flag}`;
      }
    }
  }

  return "";
}
