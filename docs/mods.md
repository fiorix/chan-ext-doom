# Mod validation catalog

doomit validates native PWAD management against the three resource-only mods selected by the host on 2026-07-27. The pinned IWAD is shareware Doom 1.9: 4,196,020 bytes, SHA-1 `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d`.

Selection is a technical validation decision, not a blanket conclusion about third-party rights. Source metadata and archive permissions are recorded below. No PWAD binary is committed to this repository.

## Effective order and multiplayer identity

Chocolate processes every `-merge` input before every `-file` input, regardless of the option groups' command-line positions. doomit exposes that effective precedence instead of implying arbitrary cross-kind ordering:

1. `DoomBSMS.wad` — `merge`, embedded DEHACKED disabled.
2. `PSXDoom.wad` — `file`, embedded DEHACKED enabled for the determinism canary.
3. `COSOUNDS.wad` — `file`, embedded DEHACKED disabled.

The lobby fingerprint uses this canonical effective order and records, per entry, `(name, SHA-256, load kind, embedded-DEHACKED policy)`. Relative order within a load-kind group is significant.

## Selected files

### PSX Doom for Vanilla Doom v1.3

- Source: https://www.moddb.com/games/doom/addons/psx-doom-for-vanilla-doom
- Use the base `PSXDoom.wad`, not `PSXDoomEnhanced.wad`.
- Source metadata: Public Domain; uploader Kippykip; upstream credits are listed on the source page and in the archive README.
- Archive: `PSXDOOM.4.ZIP`, 2,456,495 bytes, MD5 `144efacda29d5b14d9de41c502e3155c`.
- PWAD: 1,111,759 bytes, SHA-256 `089e5771d4aa2073b2e9a25a5f8ac0416e941697c2abab2ddca3d61b19680aba`.
- Contents: no maps; replaces HUD, sounds, font, and related graphics; embeds a `DEHACKED` lump that changes weapon timing.
- Load policy: `file`; embedded DEHACKED is explicit and synced, equivalent to opting into `-dehlump`.
- Classification: simulation-affecting when embedded DEHACKED is enabled.

The source page's Public Domain label has no SPDX identifier. Preserve the source URL, archive README, and upstream credits in any download-on-demand provenance. The archive README contains no license text; the page-side declaration is captured in [`provenance/moddb-public-domain-2026-07-27.md`](provenance/moddb-public-domain-2026-07-27.md). Reassess redistribution separately from technical validation.

### Doom But Slightly More Spooky v1.5

- Source: https://www.moddb.com/games/doom/addons/doom-but-slightly-more-spooky
- Archive: `DoomBSMS_v1.5.zip`, 7,651,003 bytes, MD5 `7383c8d1adb737e5ee068fd61b5ff0db`.
- PWAD: `DoomBSMS.wad`, 15,235,104 bytes, SHA-256 `f9eee42df28425eda810ffa20a5cc278595d83ce8389aca5d9aa609f2ac26023`.
- Contents: no maps or DEHACKED; replaces palette, textures, sprites, and sounds.
- Load policy: `merge`, preserving Chocolate sprite/flat namespace semantics.
- Classification: cosmetic.

ModDB supplies only the family label “Creative Commons”; neither the page nor archive identifies a CC variant. The archive README instead grants custom permission to distribute the file unmodified with the README and lists the resource credits. Record the actual grant as custom/no SPDX identifier, preserve the README, and do not invent a CC identifier.

### CoTeCiO's Sound Effect Pack for Doom/Doom II v1.0

- Source: https://www.moddb.com/games/doom-ii/addons/cotecios-sound-effect-pack-for-doomdoom-ii
- Source metadata: Public Domain; the author/uploader credits themself for everything and says the sounds were created with their own voice.
- Archive: `COSOUNDS.zip`, 994,737 bytes, MD5 `2e11c984baeb73980cae1de941346136`.
- PWAD: `COSOUNDS.wad`, 1,424,476 bytes, SHA-256 `0e4cb93bc4d2251091c2f70d9f6e04d282ec49a63993d16b8f6be4a786d50ad4`.
- Contents: exactly 107 `DS*` sound lumps; no maps, namespace markers, or DEHACKED.
- Load policy: `file`.
- Classification: cosmetic.

The Public Domain declaration is source metadata rather than an SPDX license identifier, and the zip contains only the WAD with no license text. Preserve the source URL and author declaration in provenance; the dated offline record is [`provenance/moddb-public-domain-2026-07-27.md`](provenance/moddb-public-domain-2026-07-27.md).

## Compatibility audit

Each selected PWAD contains no map lumps, so it cannot introduce map texture, flat, thing, or player-start dependencies absent from the pinned shareware IWAD.

Three apparent map candidates were rejected after parsing their map resource references against the exact IWAD:

- Return to Phobos v2.1 uses registered-Doom textures and flats absent from shareware.
- E1M1 Recreated in Memory uses registered-Doom resources and has only one player start.
- Freedoom Phase 1 Maps for Ultimate Doom targets Ultimate Doom and uses flats absent from both its PWAD resources and the shareware IWAD.

A map authored specifically against shareware resources remains a later catalog follow-up.

## Verification status

The three selected PWADs were each run separately against the pinned IWAD in a clean build of engine commit `2704d45`, with only the host-approved removal of the historical fatal shareware `-file` guard:

- `PSXDoom.wad` loaded through `-file`; a second run with `-dehlump` reported one embedded DEHACKED lump.
- `DoomBSMS.wad` loaded through `-merge`.
- `COSOUNDS.wad` loaded through `-file`.

Each 12-second headless-browser run reached `D_CheckNetGame`, `HU_Init`, and `ST_Init` on a live 320×200 canvas without `I_Error`, a JavaScript exception, or module failure.

Still required before a milestone is green:

- the combined canonical-order overlap test;
- native load/unload through the public mod-management API;
- the host-run interactive multiplayer load/unload smoke;
- deterministic end-state comparison with PSX embedded DEHACKED enabled.
