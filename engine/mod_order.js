// Canonical mod ordering for the loader and any other host of this engine.
//
// The engine does not honour the order mods appear on the command line.
// W_ParseCommandLine processes every -merge input before every -file input
// ("Merged PWADs are loaded first, because they are supposed to be modified
// IWADs"), preserving order only within each group. So a display order of
// file, merge, file is not achievable, and showing it as though it were
// would imply a precedence the engine will not deliver.
//
// The contract here is therefore the smallest faithful one: declared order is
// preserved within a load kind, and the effective order is all merges then
// all files. Callers normalize to that, and the fingerprint peers compare is
// built from the effective order, never the display order.

export const LOAD_KINDS = ["merge", "file"];

// Effective load order: merges first in their declared relative order, then
// files in theirs. Stable, so equal-kind entries keep the order given.
export function canonicalOrder(entries) {
  const merges = entries.filter((e) => e.load === "merge");
  const files = entries.filter((e) => e.load === "file");
  return [...merges, ...files];
}

// True when the requested order already matches what the engine will do, so
// a caller can tell the difference between "accepted as given" and
// "normalized". Compares identity, not name, so duplicates are handled.
export function isCanonical(entries) {
  const ordered = canonicalOrder(entries);
  return entries.length === ordered.length &&
    entries.every((e, i) => e === ordered[i]);
}

// The argv fragment for a mod set. Emitted in effective order so that reading
// the command line and reading the load order give the same answer.
//
// -dehlump is a single global switch in this engine, not a per-file one, so
// any entry opting in turns embedded DEHACKED lumps on for the whole set.
// That is why the policy is part of the agreement tuple: peers holding
// identical bytes still desync if one of them enables it and the other
// does not.
export function modArgv(entries) {
  const ordered = canonicalOrder(entries);
  const merges = ordered.filter((e) => e.load === "merge").map((e) => e.name);
  const files = ordered.filter((e) => e.load === "file").map((e) => e.name);
  const argv = [];

  if (merges.length) argv.push("-merge", ...merges);
  if (files.length) argv.push("-file", ...files);
  if (ordered.some((e) => e.deh)) argv.push("-dehlump");

  return argv;
}

// The string peers compare before starting a game. It serializes the
// effective order and the whole tuple: name, content hash, load kind, and
// embedded-DEHACKED policy. Filenames and hashes alone are not enough.
export function fingerprint(entries) {
  return canonicalOrder(entries)
    .map((e, i) => `${i}:${e.load}:${e.name}:${e.hash}:deh=${e.deh ? 1 : 0}`)
    .join("|");
}
