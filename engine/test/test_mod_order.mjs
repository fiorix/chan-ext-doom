// Host tests for the mod ordering and content rules.
//
// The traps these guard: the engine loads every -merge input before every
// -file input, so a requested file -> merge -> file sequence is not
// achievable; -dehlump is global, so a per-entry toggle would let two
// configurations differ in the fingerprint while behaving identically; and
// every file is written to the FS by name, so a duplicate or a PWAD named
// doom1.wad would make the displayed tuple describe bytes that are not the
// bytes loaded.

import {
  canonicalOrder,
  isCanonical,
  modArgv,
  fingerprint,
  hasDehackedLump,
  effectiveDehacked,
  validatePwads,
  validateIwad,
  basenamePolicyError,
  createSelectionGuard,
  iwadArgv,
  IWAD_PIN,
  IWAD_PATH,
} from "../mod_order.js";

let checks = 0;
let failures = 0;

function check(ok, what) {
  checks++;
  if (!ok) {
    failures++;
    console.log("FAIL: " + what);
  }
}

const entry = (name, load, hasDehacked = false, hash = "h" + name) =>
  ({ name, load, hasDehacked, hash });

// The fingerprint's encoding is an implementation detail, but a few checks
// need to look inside it. Parsing defensively means a regression to an
// ambiguous encoding shows up as a failed check rather than an exception
// that aborts the rest of the suite.
function parseFingerprint(fp) {
  try {
    const parsed = JSON.parse(fp);
    return Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

// The host-selected validation catalog, with its real declared kinds.
const psx = entry("PSXDoom.wad", "file", true,
  "089e5771d4aa2073b2e9a25a5f8ac0416e941697c2abab2ddca3d61b19680aba");
const spooky = entry("DoomBSMS.wad", "merge", false,
  "f9eee42df28425eda810ffa20a5cc278595d83ce8389aca5d9aa609f2ac26023");
const cotecio = entry("COSOUNDS.wad", "file", false,
  "0e4cb93bc4d2251091c2f70d9f6e04d282ec49a63993d16b8f6be4a786d50ad4");

// --- effective order -------------------------------------------------------

// The trap case: an apparent file -> merge -> file request. The merge must
// come out first; the two files must keep their relative order.
{
  const requested = [psx, spooky, cotecio];
  const ordered = canonicalOrder(requested);

  check(ordered.map((e) => e.name).join(",") ===
        "DoomBSMS.wad,PSXDoom.wad,COSOUNDS.wad",
        "file -> merge -> file normalizes to merge, then the files in order");
  check(!isCanonical(requested),
        "the requested order is reported as not canonical, so a UI can say so");
  check(isCanonical(ordered), "the normalized order is canonical");
  check(ordered[0] === spooky, "the merge entry is effectively first");
  check(ordered.indexOf(psx) < ordered.indexOf(cotecio),
        "declared file-relative order survives normalization");
}

{
  const a = entry("a.wad", "merge");
  const b = entry("b.wad", "merge");
  const c = entry("c.wad", "file");
  const d = entry("d.wad", "file");

  check(canonicalOrder([b, d, a, c]).map((e) => e.name).join(",") ===
        "b.wad,a.wad,d.wad,c.wad",
        "relative order inside each kind is stable");
}

// --- argv ------------------------------------------------------------------

{
  const argv = modArgv([psx, spooky, cotecio], true);
  check(argv.join(" ") ===
        "-merge DoomBSMS.wad -file PSXDoom.wad COSOUNDS.wad -dehlump",
        "argv lists merges first, then files, then the global dehlump switch");
  check(argv.indexOf("-merge") < argv.indexOf("-file"),
        "-merge precedes -file on the command line");

  check(!modArgv([psx, spooky, cotecio], false).includes("-dehlump"),
        "no dehlump when the global switch is off");

  // The switch is pointless if nothing carries a patch, and emitting it
  // anyway would put a flag in the fingerprint that changes nothing.
  check(!modArgv([spooky, cotecio], true).includes("-dehlump"),
        "no dehlump when no entry carries a DEHACKED lump");
}

// --- embedded DEHACKED is global, not per-entry ----------------------------

{
  check(effectiveDehacked(psx, true), "a patch-carrying entry is live when the switch is on");
  check(!effectiveDehacked(psx, false), "the same entry is inert when the switch is off");
  check(!effectiveDehacked(cotecio, true),
        "an entry with no DEHACKED lump is inert even when the switch is on");
}

// A minimal WAD directory, so the DEHACKED probe is tested against bytes
// rather than a flag someone set by hand.
function buildWad(lumpNames) {
  const dirOfs = 12;
  const buf = new ArrayBuffer(dirOfs + lumpNames.length * 16);
  const view = new DataView(buf);
  for (let i = 0; i < 4; i++) view.setUint8(i, "PWAD".charCodeAt(i));
  view.setUint32(4, lumpNames.length, true);
  view.setUint32(8, dirOfs, true);
  lumpNames.forEach((name, i) => {
    const at = dirOfs + i * 16;
    view.setUint32(at, 0, true);
    view.setUint32(at + 4, 0, true);
    for (let c = 0; c < 8; c++) {
      view.setUint8(at + 8 + c, c < name.length ? name.charCodeAt(c) : 0);
    }
  });
  return buf;
}

{
  check(hasDehackedLump(buildWad(["DEHACKED", "MAP01"])),
        "a DEHACKED lump is found in the directory");
  check(!hasDehackedLump(buildWad(["MAP01", "THINGS"])),
        "a WAD without one is reported as not carrying a patch");
  check(!hasDehackedLump(new ArrayBuffer(4)),
        "a runt file is rejected rather than read past its end");
  check(!hasDehackedLump(buildWad([]).slice(0, 8)),
        "a truncated header is rejected");

  // A directory offset past the end must not be walked.
  const bad = buildWad(["MAP01"]);
  new DataView(bad).setUint32(8, 0xfffff0, true);
  check(!hasDehackedLump(bad), "an out-of-range directory offset is rejected");
}

// --- content rules ---------------------------------------------------------

{
  check(validatePwads([psx, spooky, cotecio]).ok, "the selected catalog is accepted");

  const dup = validatePwads([psx, entry("PSXDoom.wad", "file")]);
  check(!dup.ok, "a duplicate filename is rejected");
  check(dup.reason.includes("overwrite"), "the duplicate rejection says why");

  const reserved = validatePwads([entry("doom1.wad", "file")]);
  check(!reserved.ok, "a PWAD named doom1.wad is rejected");

  const cased = validatePwads([entry("DOOM1.WAD", "file")]);
  check(!cased.ok, "the reserved name is rejected regardless of case");

  const patch = validatePwads([entry("patch.deh", "file")]);
  check(!patch.ok, "a standalone .deh is rejected, since -deh is never emitted");
}

{
  check(validateIwad({ size: IWAD_PIN.size, hash: IWAD_PIN.sha256 }).ok,
        "the pinned shareware IWAD is accepted");
  check(!validateIwad({ size: 123, hash: IWAD_PIN.sha256 }).ok,
        "a wrong size is rejected");
  check(!validateIwad({ size: IWAD_PIN.size, hash: "deadbeef" }).ok,
        "wrong bytes at the right size are rejected");
  check(!validateIwad(null).ok, "no IWAD is rejected");
  check(!validateIwad({ size: IWAD_PIN.size, hash: "unavailable" }).ok,
        "an unavailable hash is not a pass");

  // The sha-1 is shown to the operator but must never gate the launch.
  check(IWAD_PIN.sha1 === "5b2e249b9c5133ec987b3ea77596381dc0d6bc1d",
        "the displayed sha-1 is the shareware value");
  check(!validateIwad({ size: IWAD_PIN.size, hash: IWAD_PIN.sha1 }).ok,
        "the sha-1 is not accepted in place of the enforced sha-256");
}

// --- fingerprint -----------------------------------------------------------

{
  const fp1 = fingerprint([psx, spooky, cotecio], true);
  const fp2 = fingerprint([spooky, psx, cotecio], true);

  check(fp1 === fp2,
        "two peers who typed the set in different orders agree, because the " +
        "fingerprint is canonical");
  const parsed1 = parseFingerprint(fp1);
  check(parsed1 !== null && parsed1[0].name === "DoomBSMS.wad" &&
        parsed1[0].load === "merge",
        "fingerprint starts with the effectively-first entry");
  check(fp1.includes("089e5771"), "fingerprint carries content hashes");
  check(parsed1 !== null && parsed1.some((e) => e.deh === 1),
        "fingerprint carries the effective dehacked policy");

  check(fingerprint([spooky, psx, cotecio], true) !==
        fingerprint([spooky, psx, cotecio], false),
        "flipping the global dehacked switch changes the fingerprint");

  check(fingerprint([spooky, psx, cotecio], true) !==
        fingerprint([spooky, cotecio, psx], true),
        "file-relative order changes the fingerprint");

  check(fingerprint([spooky, psx, cotecio], true) !==
        fingerprint([spooky, psx, { ...cotecio, load: "merge" }], true),
        "changing a load kind changes the fingerprint");

  // The switch is global, so a set with no patch-carrying entry must
  // fingerprint identically whether it is on or off: anything else would
  // claim a difference the engine does not make.
  check(fingerprint([spooky, cotecio], true) ===
        fingerprint([spooky, cotecio], false),
        "the switch does not change the fingerprint when no entry carries a patch");
}


// --- argv-hostile filenames ------------------------------------------------

// W_ParseCommandLine ends each filename list at the first argument starting
// with "-", and M_FindResponseFile expands anything starting with "@" before
// WAD parsing even happens. Both would be fingerprinted and mounted while
// behaving nothing like a loaded PWAD.
{
  check(basenamePolicyError("-nomonsters.wad") !== "",
        "a name starting with - is refused");
  check(basenamePolicyError("-nomonsters.wad").includes("option"),
        "the - rejection explains the engine would read it as an option");

  check(basenamePolicyError("@argv.wad") !== "",
        "a name starting with @ is refused");
  check(basenamePolicyError("@argv.wad").includes("response file"),
        "the @ rejection explains the response-file expansion");

  check(basenamePolicyError("sub/dir.wad") !== "", "a path separator is refused");
  check(basenamePolicyError("back\\slash.wad") !== "",
        "a backslash is refused");
  check(basenamePolicyError("..") !== "", "a dot-dot name is refused");
  check(basenamePolicyError("") !== "", "an empty name is refused");
  check(basenamePolicyError("bell" + String.fromCharCode(7) + ".wad") !== "",
        "a control character is refused");

  // The selected catalog must keep working.
  for (const name of ["PSXDoom.wad", "DoomBSMS.wad", "COSOUNDS.wad"]) {
    check(basenamePolicyError(name) === "", name + " remains valid");
  }

  // And the whole-set validator must enforce it, not just the helper.
  check(!validatePwads([entry("-evil.wad", "file")]).ok,
        "validatePwads refuses an option-shaped name");
  check(!validatePwads([entry("@evil.wad", "file")]).ok,
        "validatePwads refuses a response-file-shaped name");
}

// --- stale asynchronous selections -----------------------------------------

// Forced ordering, not timing: selection A is made, then B, then A completes.
// A must not be able to write its result after B superseded it.
{
  const guard = createSelectionGuard();

  const a = guard.begin();
  const b = guard.begin();

  check(!guard.isCurrent(a), "a superseded selection is not current");
  check(guard.isCurrent(b), "the latest selection is current");

  // Clearing the chooser must strand everything still in flight.
  const c = guard.begin();
  guard.invalidate();
  check(!guard.isCurrent(c), "clearing invalidates an in-flight selection");

  // A fresh selection after that is current again.
  const d = guard.begin();
  check(guard.isCurrent(d), "a new selection after a clear is current");
}


// --- the pinned IWAD is selected explicitly --------------------------------

// The engine's auto-detect table puts doom2.wad, plutonia.wad, tnt.wad and
// doom.wad ahead of doom1.wad, in the same directory uploads are mounted
// into. Reserving the doom1.wad name is not enough on its own: a PWAD called
// doom2.wad would boot as the IWAD while the page showed the verified
// doom1.wad tuple.
{
  check(iwadArgv().join(" ") === "-iwad " + IWAD_PATH,
        "the mounted pin is named explicitly");

  const higherPriority = ["doom2.wad", "plutonia.wad", "tnt.wad", "doom.wad"];

  for (const name of higherPriority) {
    // These are legal uploads: the policy only reserves the IWAD's own path.
    check(validatePwads([entry(name, "file")]).ok,
          name + " is accepted as an ordinary PWAD");

    const argv = iwadArgv().concat(modArgv([entry(name, "file")], false));
    check(argv[0] === "-iwad" && argv[1] === IWAD_PATH,
          "with " + name + " uploaded, the IWAD is still selected explicitly");
    check(argv.indexOf("-iwad") < argv.indexOf("-file"),
          "the explicit IWAD precedes the PWAD list for " + name);
  }
}

// --- the fingerprint cannot be forged by a filename ------------------------

// @@server's captured collision, reproduced against the old delimiter
// encoding. A filename may legitimately contain ":" and "|", so one entry
// whose name embeds them produced exactly the string two ordinary entries
// produced.
{
  const h1 = "a".repeat(64);
  const h2 = "b".repeat(64);

  const one = [{
    name: "X.wad:" + h1 + ":deh=0|1:file:Y.wad",
    hash: h2,
    load: "file",
    hasDehacked: false,
  }];
  const two = [
    { name: "X.wad", hash: h1, load: "file", hasDehacked: false },
    { name: "Y.wad", hash: h2, load: "file", hasDehacked: false },
  ];

  check(validatePwads(one).ok, "the crafted single-entry set is a valid set");
  check(validatePwads(two).ok, "the ordinary two-entry set is a valid set");
  check(fingerprint(one, false) !== fingerprint(two, false),
        "a name embedding the old delimiters no longer collides with two entries");

  // The general property, not just the captured instance.
  const sneaky = [
    { name: '","hash":"' + h1 + '.wad', hash: h2, load: "file", hasDehacked: false },
  ];
  check(validatePwads(sneaky).ok, "a name with quote characters is a valid set");
  check(fingerprint(sneaky, false) !== fingerprint(
          [{ name: "a.wad", hash: h1, load: "file", hasDehacked: false },
           { name: "b.wad", hash: h2, load: "file", hasDehacked: false }], false),
        "a name cannot forge a field boundary in the encoding");

  // Equal sets must still compare equal, or the encoding is useless.
  check(fingerprint(two, false) === fingerprint(
          [{ name: "X.wad", hash: h1, load: "file", hasDehacked: false },
           { name: "Y.wad", hash: h2, load: "file", hasDehacked: false }], false),
        "identical sets still fingerprint identically");
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
