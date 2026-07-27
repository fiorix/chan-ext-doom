// Host tests for the canonical mod ordering contract.
//
// The trap these guard: the engine loads every -merge input before every
// -file input, so a requested file -> merge -> file sequence is not
// achievable. These prove the normalized order is what the engine will
// actually do, and that the fingerprint peers compare is built from that
// order rather than from display order.

import {
  canonicalOrder,
  isCanonical,
  modArgv,
  fingerprint,
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

const entry = (name, load, deh = false, hash = "h" + name) =>
  ({ name, load, deh, hash });

// The host-selected validation catalog, with its real declared kinds.
const psx = entry("PSXDoom.wad", "file", true,
  "089e5771d4aa2073b2e9a25a5f8ac0416e941697c2abab2ddca3d61b19680aba");
const spooky = entry("DoomBSMS.wad", "merge", false,
  "f9eee42df28425eda810ffa20a5cc278595d83ce8389aca5d9aa609f2ac26023");
const cotecio = entry("COSOUNDS.wad", "file", false,
  "0e4cb93bc4d2251091c2f70d9f6e04d282ec49a63993d16b8f6be4a786d50ad4");

// The trap case named in the contract: an apparent file -> merge -> file
// request. The merge must come out first; the two files must keep their
// relative order.
{
  const requested = [psx, spooky, cotecio];
  const ordered = canonicalOrder(requested);

  check(ordered.map((e) => e.name).join(",") ===
        "DoomBSMS.wad,PSXDoom.wad,COSOUNDS.wad",
        "file -> merge -> file normalizes to merge, then the files in order");
  check(!isCanonical(requested),
        "the requested order is reported as not canonical, so a UI can say so");
  check(isCanonical(ordered), "the normalized order is canonical");

  // Display order must not be able to imply a precedence the engine ignores.
  check(ordered[0] === spooky, "the merge entry is effectively first");
  check(ordered.indexOf(psx) < ordered.indexOf(cotecio),
        "declared file-relative order survives normalization");
}

// argv reads in effective order, so the command line and the load order
// agree.
{
  const argv = modArgv([psx, spooky, cotecio]);
  check(argv.join(" ") ===
        "-merge DoomBSMS.wad -file PSXDoom.wad COSOUNDS.wad -dehlump",
        "argv lists merges first, then files, then the global dehlump switch");

  check(argv.indexOf("-merge") < argv.indexOf("-file"),
        "-merge precedes -file on the command line");
}

// -dehlump is global: it appears when any entry opts in, and not otherwise.
{
  check(!modArgv([spooky, cotecio]).includes("-dehlump"),
        "no dehlump when nothing opts in");
  check(modArgv([psx]).includes("-dehlump"), "dehlump when an entry opts in");
}

// Ordering within one kind is preserved exactly.
{
  const a = entry("a.wad", "merge");
  const b = entry("b.wad", "merge");
  const c = entry("c.wad", "file");
  const d = entry("d.wad", "file");

  check(canonicalOrder([b, d, a, c]).map((e) => e.name).join(",") ===
        "b.wad,a.wad,d.wad,c.wad",
        "relative order inside each kind is stable");
}

// The fingerprint is built from effective order and carries the whole tuple.
{
  const fp1 = fingerprint([psx, spooky, cotecio]);
  const fp2 = fingerprint([spooky, psx, cotecio]);

  check(fp1 === fp2,
        "two peers who typed the set in different orders agree, because the " +
        "fingerprint is canonical");

  check(fp1.startsWith("0:merge:DoomBSMS.wad:"),
        "fingerprint starts with the effectively-first entry");
  check(fp1.includes("089e5771"), "fingerprint carries content hashes");
  check(fp1.includes(":deh=1"), "fingerprint carries the dehacked policy");

  // The policy really is load-bearing: same bytes, same order, different
  // dehacked opt-in must not compare equal.
  const psxNoDeh = { ...psx, deh: false };
  check(fingerprint([spooky, psx, cotecio]) !==
        fingerprint([spooky, psxNoDeh, cotecio]),
        "flipping the dehacked policy changes the fingerprint");

  // Same set, different declared order within a kind, is a real difference.
  check(fingerprint([spooky, psx, cotecio]) !==
        fingerprint([spooky, cotecio, psx]),
        "file-relative order changes the fingerprint");

  // A load-kind change is a real difference too.
  check(fingerprint([spooky, psx, cotecio]) !==
        fingerprint([spooky, psx, { ...cotecio, load: "merge" }]),
        "changing a load kind changes the fingerprint");
}

console.log(`${checks} checks, ${failures} failures`);
process.exit(failures === 0 ? 0 : 1);
