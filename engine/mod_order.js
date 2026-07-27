// Canonical mod ordering and content rules for the loader, and for any other
// host of this engine.
//
// The engine does not honour the order mods appear on the command line.
// W_ParseCommandLine processes every -merge input before every -file input
// ("Merged PWADs are loaded first, because they are supposed to be modified
// IWADs"), preserving order only within each group. So a display order of
// file, merge, file is not achievable, and showing it as though it were would
// imply a precedence the engine will not deliver.
//
// The contract here is therefore the smallest faithful one: declared order is
// preserved within a load kind, and the effective order is all merges then all
// files. Callers normalize to that, and the fingerprint peers compare is built
// from the effective order, never the display order.

export const LOAD_KINDS = ["merge", "file"];

// The pinned shareware baseline. This fork is shareware-IWAD-only by design,
// so the loader verifies the bytes rather than trusting a filename.
export const IWAD_PIN = {
  name: "doom1.wad",
  size: 4196020,
  sha256: "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771",
};

// The path the IWAD occupies in the Emscripten FS. A PWAD may not take it.
export const IWAD_PATH = "/doom1.wad";

// Guards state that is written after an await.
//
// Reading a chosen file is asynchronous, so two selections can be in flight
// at once and finish in either order. Without a guard a slow first pick can
// land after a second one, leaving the accepted bytes describing a different
// file from the one the chooser shows. Each selection takes a generation and
// discards its own result if it is no longer the current one.
export function createSelectionGuard() {
  let current = 0;

  return {
    begin() {
      return ++current;
    },
    isCurrent(generation) {
      return generation === current;
    },
    // Clearing the chooser must also strand anything still in flight.
    invalidate() {
      ++current;
    },
  };
}

// Effective load order: merges first in their declared relative order, then
// files in theirs. Stable, so equal-kind entries keep the order given.
export function canonicalOrder(entries) {
  const merges = entries.filter((e) => e.load === "merge");
  const files = entries.filter((e) => e.load === "file");
  return [...merges, ...files];
}

// True when the requested order already matches what the engine will do, so a
// caller can tell "accepted as given" from "normalized". Compares identity,
// not name, so duplicates are handled.
export function isCanonical(entries) {
  const ordered = canonicalOrder(entries);
  return entries.length === ordered.length &&
    entries.every((e, i) => e === ordered[i]);
}

// Whether a WAD carries a DEHACKED lump, read from its directory.
//
// This is not cosmetic detail: -dehlump is a single global switch that loads
// every DEHACKED lump from every loaded WAD. The engine offers no way to take
// one PWAD's patch and skip another's, so the honest per-entry value is
// "does this file contain one", combined with the global switch. A per-entry
// toggle would let two configurations differ in the UI and the fingerprint
// while producing identical argv and identical simulation.
export function hasDehackedLump(bytes) {
  const view = new DataView(bytes);
  if (view.byteLength < 12) return false;

  const magic = String.fromCharCode(
    view.getUint8(0), view.getUint8(1), view.getUint8(2), view.getUint8(3));
  if (magic !== "IWAD" && magic !== "PWAD") return false;

  const numLumps = view.getUint32(4, true);
  const dirOfs = view.getUint32(8, true);

  // A truncated or hostile directory must not drive a huge scan.
  if (numLumps > 65536) return false;
  if (dirOfs + numLumps * 16 > view.byteLength) return false;

  for (let i = 0; i < numLumps; i++) {
    const at = dirOfs + i * 16 + 8;
    let name = "";
    for (let c = 0; c < 8; c++) {
      const ch = view.getUint8(at + c);
      if (ch === 0) break;
      name += String.fromCharCode(ch);
    }
    if (name.toUpperCase() === "DEHACKED") return true;
  }

  return false;
}

// What the engine will actually do with this entry's embedded patch.
export function effectiveDehacked(entry, dehlump) {
  return !!dehlump && !!entry.hasDehacked;
}

// A basename the engine cannot take as an argument.
//
// Two engine behaviours make this a correctness rule rather than hygiene.
// W_ParseCommandLine ends each filename list at the first argument whose
// first byte is "-", so a PWAD named that way is mounted and fingerprinted
// but never loaded. M_FindResponseFile runs before WAD parsing and expands
// any argument starting with "@", so such a file would have its own bytes
// parsed as a command line. Either one breaks the promise that the
// fingerprint describes what is actually loaded, and a local file can
// legitimately be named either way.
//
// Path separators and control characters go in the same pass: the name
// becomes an FS path as well as an argument, and neither is safe to
// represent.
export function basenamePolicyError(name) {
  if (!name) return "the file has no name";
  if (name === "." || name === "..") return JSON.stringify(name) + " is not a filename";
  if (name.startsWith("-")) {
    return JSON.stringify(name) +
      " starts with \"-\", so the engine would read it as an option and never load it";
  }
  if (name.startsWith("@")) {
    return JSON.stringify(name) +
      " starts with \"@\", so the engine would expand it as a response file instead of loading it";
  }
  if (name.includes("/") || name.includes("\\")) {
    return JSON.stringify(name) + " contains a path separator";
  }
  for (const ch of name) {
    const code = ch.charCodeAt(0);
    if (code < 0x20 || code === 0x7f) {
      return JSON.stringify(name) + " contains control characters";
    }
  }
  return "";
}

// Rejects a set the loader cannot honour. Every entry is written to the FS by
// name, so a duplicate name would silently overwrite and a PWAD named
// doom1.wad would replace the IWAD. Either way the displayed tuple would
// describe bytes that are not the bytes loaded.
export function validatePwads(entries) {
  const seen = new Set();

  for (const e of entries) {
    const key = e.name.toLowerCase();

    const unusable = basenamePolicyError(e.name);
    if (unusable) {
      return { ok: false, reason: unusable };
    }

    if (key === IWAD_PIN.name) {
      return { ok: false, reason: `a PWAD may not be named ${IWAD_PIN.name}: it would overwrite the IWAD` };
    }
    if (seen.has(key)) {
      return { ok: false, reason: `duplicate filename ${e.name}: the second would overwrite the first` };
    }
    if (!key.endsWith(".wad")) {
      // -deh and -bex are never emitted, so accepting a standalone patch
      // would show it in the fingerprint while the engine ignored it.
      return { ok: false, reason: `${e.name} is not a .wad: standalone patch files are not supported` };
    }
    seen.add(key);
  }

  return { ok: true, reason: "" };
}

export function validateIwad(iwad) {
  if (!iwad) return { ok: false, reason: "no IWAD chosen" };
  if (iwad.size !== IWAD_PIN.size) {
    return { ok: false, reason: `IWAD is ${iwad.size} bytes, expected ${IWAD_PIN.size}` };
  }
  if (iwad.hash !== IWAD_PIN.sha256) {
    return { ok: false, reason: "IWAD does not match the pinned shareware DOOM1.WAD v1.9" };
  }
  return { ok: true, reason: "" };
}

// The argv fragment for a mod set. Emitted in effective order so that reading
// the command line and reading the load order give the same answer.
// The engine's auto-detect walks its IWAD table before reaching doom1.wad:
// doom2.wad, plutonia.wad, tnt.wad and doom.wad all outrank it, matched
// case-insensitively in the same directory the loader mounts uploads into. A
// PWAD carrying one of those names would boot as the IWAD while the page
// displayed the verified doom1.wad tuple. Naming the pin explicitly removes
// the auto-detect from the picture entirely; D_FindWADByName resolves an
// absolute path directly before it consults any search directory.
export function iwadArgv() {
  return ["-iwad", IWAD_PATH];
}

export function modArgv(entries, dehlump) {
  const ordered = canonicalOrder(entries);
  const merges = ordered.filter((e) => e.load === "merge").map((e) => e.name);
  const files = ordered.filter((e) => e.load === "file").map((e) => e.name);
  const argv = [];

  if (merges.length) argv.push("-merge", ...merges);
  if (files.length) argv.push("-file", ...files);

  // Only meaningful when something actually carries a patch; emitting it
  // otherwise would put a flag in the fingerprint that changes nothing.
  if (dehlump && ordered.some((e) => e.hasDehacked)) argv.push("-dehlump");

  return argv;
}

// The string peers compare before starting a game. It serializes the
// effective order and the whole tuple: name, content hash, load kind, and the
// embedded-DEHACKED behaviour that will actually take effect.
//
// Encoded with JSON rather than by joining fields with delimiters. A filename
// is attacker-influenced and may legitimately contain any character the
// basename policy permits, including ":" and "|", so delimiter concatenation
// is ambiguous: one entry whose name embeds the separators produces the same
// string as two ordinary entries. That is not hypothetical, it was
// reproduced. JSON quotes and escapes every field, so no name can forge a
// field boundary.
export function fingerprint(entries, dehlump) {
  return JSON.stringify(
    canonicalOrder(entries).map((e, i) => ({
      i,
      load: e.load,
      name: e.name,
      hash: e.hash,
      deh: effectiveDehacked(e, dehlump) ? 1 : 0,
    })));
}
