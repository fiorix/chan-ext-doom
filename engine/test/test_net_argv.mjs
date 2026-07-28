// Contract tests for the loader's network arguments and the client-only guard.
//
// These pin behaviour rather than text: that single player emits no network
// flags at all, that every multiplayer launch carries the server route and the
// node expectation, and that the guard refuses either server spelling wherever
// it came from.

import { readFileSync } from "node:fs";

import {
  MAX_NODES,
  MIN_NODES,
  MODES,
  SERVER_FLAGS,
  SERVER_ROUTE,
  controlsEnabled,
  guardClientOnly,
  isMultiplayer,
  netArgv,
  networkBlockedReason,
  serverFlagIn,
  validateNodes,
  validateRoom,
} from "../net_argv.js";

let checks = 0;
let failures = 0;

function check(ok, what) {
  checks += 1;
  if (!ok) {
    failures += 1;
    console.log(`FAIL: ${what}`);
  }
}

const ROOM = "ws://127.0.0.1:8080/ws/e1m1";

// --- single player carries no network flags --------------------------------

{
  const argv = netArgv("single", ROOM, 2);
  check(argv.length === 0, "single player emits no network arguments");
  for (const flag of ["-connect", "-wss", "-nodes", ...SERVER_FLAGS]) {
    check(!argv.includes(flag), `single player emits no ${flag}`);
  }
}

// --- multiplayer emits exactly the client shape ----------------------------

{
  const argv = netArgv("multiplayer", ROOM, 3);
  check(
    JSON.stringify(argv) ===
      JSON.stringify(["-connect", "1", "-wss", ROOM, "-nodes", "3"]),
    `multiplayer argv is the client shape, got ${JSON.stringify(argv)}`,
  );
  check(argv[1] === SERVER_ROUTE, "the server is addressed as route 1");
  check(argv.includes("-nodes"), "every multiplayer client carries -nodes");
  for (const flag of SERVER_FLAGS) {
    check(!argv.includes(flag), `multiplayer argv never contains ${flag}`);
  }
}

// Every node count in range reaches the argv, so no client is left without the
// threshold a promoted controller needs.
for (let n = MIN_NODES; n <= MAX_NODES; n += 1) {
  const argv = netArgv("multiplayer", ROOM, n);
  check(argv[argv.indexOf("-nodes") + 1] === String(n),
        `node count ${n} reaches the argv`);
}

// --- the room and node controls ---------------------------------------------

check(MODES.length === 2, "there are exactly two modes");
check(MODES.includes("single") && MODES.includes("multiplayer"),
      "the two modes are single and multiplayer");
check(!MODES.includes("host") && !MODES.includes("join"),
      "no hosting or joining mode survives");
check(isMultiplayer("multiplayer") && !isMultiplayer("single"),
      "only multiplayer is a network mode");
check(controlsEnabled("multiplayer"), "room and nodes are live for multiplayer");
check(!controlsEnabled("single"), "room and nodes are inert for single player");

// --- validation bounds are preserved ----------------------------------------

check(!validateRoom("").ok, "an empty room URL is refused");
check(!validateRoom("   ").ok, "a whitespace room URL is refused");
check(validateRoom(` ${ROOM} `).value === ROOM, "the room URL is trimmed");
check(!validateNodes(MIN_NODES - 1).ok, `${MIN_NODES - 1} nodes is refused`);
check(!validateNodes(MAX_NODES + 1).ok, `${MAX_NODES + 1} nodes is refused`);
check(!validateNodes(2.5).ok, "a fractional node count is refused");
check(validateNodes(MIN_NODES).ok && validateNodes(MAX_NODES).ok,
      "the bounds themselves are accepted");

for (const [room, nodes, why] of [
  ["", 2, "no room"],
  [ROOM, 1, "too few nodes"],
  [ROOM, 9, "too many nodes"],
]) {
  let threw = false;
  try {
    netArgv("multiplayer", room, nodes);
  } catch {
    threw = true;
  }
  check(threw, `multiplayer refuses to build an argv with ${why}`);
}

// --- the guard, each spelling independently ---------------------------------

for (const flag of SERVER_FLAGS) {
  const verdict = guardClientOnly(["-iwad", "/doom1.wad", flag, "-wss", ROOM]);
  check(!verdict.ok, `the guard rejects ${flag}`);
  check(verdict.flag === flag, `the guard names ${flag}`);
  check(/route 1/.test(verdict.reason), `${flag} rejection explains route 1`);
  check(serverFlagIn(["-nosound", flag]) === flag, `${flag} is found anywhere in the argv`);
}

// Both at once, and in any position.
check(!guardClientOnly([...SERVER_FLAGS]).ok, "the guard rejects both spellings together");
check(!guardClientOnly(["-server"]).ok, "the guard rejects a lone -server");
check(!guardClientOnly([...netArgv("multiplayer", ROOM, 2), "-privateserver"]).ok,
      "a server flag appended after a valid client argv is still rejected");

// The guard passes what the loader actually builds, in both modes.
check(guardClientOnly(netArgv("multiplayer", ROOM, 2)).ok,
      "a real multiplayer argv passes the guard");
check(guardClientOnly(netArgv("single", ROOM, 2)).ok,
      "a real single-player argv passes the guard");
check(serverFlagIn(netArgv("multiplayer", ROOM, 8)) === null,
      "no server flag hides in a maximal multiplayer argv");

// --- the launch decision, over the argv the loader actually assembles -------

// This is the integration, not the helper. The loader's builder is
// non-throwing: it catches netArgv's rejection so a half-filled form can still
// render a content argv. That means netArgv refusing is not enough on its own,
// because the refusal is swallowed by design. The launch decision has to catch
// it again at the seam, or a multiplayer launch proceeds with no network
// arguments at all.
//
// buildArgv below is deliberately the page's shape, including the catch.
function buildArgv(mode, room, nodes) {
  const argv = ["-iwad", "/doom1.wad"];
  try {
    argv.push(...netArgv(mode, room, nodes));
  } catch {
    return argv;
  }
  return argv;
}

for (const [nodes, why] of [
  [MIN_NODES - 1, "below the minimum"],
  [MAX_NODES + 1, "above the maximum"],
  [0, "zero"],
  [-1, "negative"],
  [2.5, "fractional"],
  ["", "empty"],
  ["   ", "whitespace"],
  ["two", "non-numeric"],
  [null, "null"],
  [undefined, "absent"],
]) {
  const argv = buildArgv("multiplayer", ROOM, nodes);
  // The precondition for this test to mean anything: the builder really did
  // swallow the rejection and hand back a launchable-looking argv.
  check(!argv.includes("-connect"),
        `the builder drops the network argv when the node count is ${why}`);
  const reason = networkBlockedReason("multiplayer", ROOM, nodes, argv);
  check(reason !== "",
        `a multiplayer launch is refused when the node count is ${why}`);
  // Name the actual fault. Falling through to the generic missing-argument
  // reason would still refuse the launch, but it would tell the player the
  // build is broken rather than that the field is wrong.
  check(reason === `node count must be ${MIN_NODES} to ${MAX_NODES}`,
        `the ${why} node count is reported as a node count fault, got ${JSON.stringify(reason)}`);
}

// The same hole via the room, and via both at once.
for (const [room, nodes, why] of [
  ["", 2, "the room is empty"],
  ["   ", 2, "the room is whitespace"],
  ["", 99, "both the room and the node count are invalid"],
]) {
  const argv = buildArgv("multiplayer", room, nodes);
  check(networkBlockedReason("multiplayer", room, nodes, argv) === "no room URL",
        `a multiplayer launch is refused for the room when ${why}`);
}

// The symptom check stands on its own: an argv that lost its network arguments
// for any reason at all is refused, even when the form inputs are valid.
for (const flag of ["-connect", "-wss", "-nodes"]) {
  const argv = netArgv("multiplayer", ROOM, 4);
  const i = argv.indexOf(flag);
  const stripped = [...argv.slice(0, i), ...argv.slice(i + 2)];
  check(networkBlockedReason("multiplayer", ROOM, 4, stripped)
          === `multiplayer launch is missing ${flag}`,
        `a multiplayer launch missing ${flag} is refused by name`);
}

// And it does not block what it should not.
check(networkBlockedReason("multiplayer", ROOM, 4,
        buildArgv("multiplayer", ROOM, 4)) === "",
      "a fully valid multiplayer launch is allowed");
for (const nodes of [MIN_NODES, MAX_NODES]) {
  check(networkBlockedReason("multiplayer", ROOM, nodes,
          buildArgv("multiplayer", ROOM, nodes)) === "",
        `a multiplayer launch at ${nodes} nodes is allowed`);
}
check(networkBlockedReason("single", "", "", buildArgv("single", "", "")) === "",
      "single player is allowed with no room and no node count");
check(networkBlockedReason("single", "", "", ["-iwad", "/doom1.wad"]) === "",
      "single player is not asked for network arguments");

// The final-argv guard still runs from the same decision, both spellings.
for (const flag of SERVER_FLAGS) {
  const reason = networkBlockedReason(
    "multiplayer", ROOM, 4, [...netArgv("multiplayer", ROOM, 4), flag]);
  check(/route 1/.test(reason), `the launch decision refuses ${flag}`);
  check(networkBlockedReason("single", "", "", ["-iwad", "/doom1.wad", flag]) !== "",
        `the launch decision refuses ${flag} in single player too`);
}

// --- the page itself, so the module cannot pass while the loader diverges ---

// A structural read of the shipped page. This is deliberately not a DOM test:
// it keeps the suite portable, and the live control states are asserted
// separately against a real browser.
{
  const page = readFileSync(new URL("../doom.html", import.meta.url), "utf8");

  const radios = [...page.matchAll(/name="mode"\s+value="([a-z]+)"/g)].map((m) => m[1]);
  check(radios.length === 2, `the page offers two modes, got ${JSON.stringify(radios)}`);
  check(radios.includes("single"), "the page keeps single player");
  check(radios.includes("multiplayer"), "the page offers one multiplayer choice");
  check(!radios.includes("host") && !radios.includes("join"),
        "the page offers no hosting or joining choice");

  for (const flag of SERVER_FLAGS) {
    check(!page.includes(`"${flag}"`), `the page never pushes ${flag}`);
  }

  check(page.includes("netArgv("), "the page builds network argv through netArgv");

  // The seam that closes the integration hole: the page's launch decision is
  // handed the argv it is about to launch, and the launch button re-checks it
  // rather than trusting the form's rendered state.
  check(/networkBlockedReason\(\s*mode\(\),\s*el\("room"\)\.value,\s*el\("nodes"\)\.value,\s*buildArgv\(\)\)/
          .test(page),
        "the page decides launches with the argv it is about to launch");
  const launchBody = page.slice(page.indexOf("function launch()"));
  check(/^[\s\S]{0,500}?const blocked = blockedReason\(\);[\s\S]{0,200}?if \(blocked\) \{[\s\S]{0,200}?return;/
          .test(launchBody),
        "launch re-checks the blocking reason and returns before mounting bytes");
  check(/function blockedReason\(\)[\s\S]{0,600}?networkReason\(\)/.test(page),
        "the blocking reason includes the network decision");

  // Both controls follow the same predicate now: live for multiplayer, inert
  // for single player.
  const gates = [...page.matchAll(/el\("(room|nodes)"\)\.disabled = ([^;]+);/g)]
    .map((m) => [m[1], m[2].trim()]);
  check(gates.length === 2, `both controls are gated, got ${JSON.stringify(gates)}`);
  for (const [id, expr] of gates) {
    check(expr === 'mode() === "single"',
          `${id} is enabled for multiplayer and disabled for single player, got ${expr}`);
  }
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
