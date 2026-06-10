---
name: asset-audit
description: DS-budget-aware asset auditor for the bevy-ds platform / Kill the Serpent. Read-only diagnostic. Use to check source + baked assets against the DS hardware limits (per-frame geometry budget, the 16 sprite 4bpp palette-bank cap, sprite square-size constraints, VRAM banks, mmutil audio formats) and to find NitroFS orphans (baked but unreferenced) and missing references (referenced but unbaked). Run before a milestone, after adding assets, or when the ROM nears a limit.
---

# asset-audit

Read-only. Reports findings; writes nothing. The DS has hard, low ceilings, so
"asset management" here means *budget enforcement*, not just naming hygiene.

## When to use
- Before a milestone, after adding/rebaking assets (see `bake-asset`), or when a
  scene drops frames / the ROM nears a limit.

## The budgets

Authoritative hardware ceilings (per-frame polygon/vertex counts, VRAM bank
sizes) — **look these up via the `blocksds-docs` skill** rather than hardcoding;
they vary by render mode. Use them as the ceiling for the estimates below.

Project caps that ARE fixed (from CLAUDE.md / our crates):
- **Sprites:** ≤ **16 distinct `.sprite` images** (one per 4bpp palette bank,
  per `bevy_nds_sprite`). Square sizes **only**: 8×8 / 16×16 / 32×32 / 64×64.
- **3D models:** baked display lists (`.dl` via `obj2dl` / `include_obj!`). A
  model's poly/vertex count must fit the per-frame budget *together with every
  other model drawn that frame* — audit by worst-case concurrent space.
- **Backgrounds:** `bevy_nds_bg` / `png2bg` — tile vs bitmap mode, palette size.
- **Audio:** `wav2bank` / `mmutil` — WAV in, music WAVs need a forward-loop
  `smpl` chunk, SFX names become `SFX_*` ids.

## Steps

1. **Load standards.** Read CLAUDE.md (asset pipeline + conventions). Pull exact
   geometry/VRAM ceilings via `blocksds-docs` if any check is borderline.
2. **Inventory.** Glob source (`assets/**/*.obj`, `assets/sprites/**/*.png`,
   `assets/bg/**/*.png`, `audio/{music,sfx}/*.wav`) and baked
   (`build/nitrofs/**`). List both.
3. **Per-category budget check** → PASS / WARN / FAIL with the specific number:
   - Sprites: count distinct `.sprite` referenced → WARN at 13+, FAIL at >16.
     Verify each PNG yields a square 8/16/32/64 sprite.
   - 3D: estimate per-model poly/vertex from `.dl` size; flag the heaviest and any
     space whose concurrent models risk the per-frame ceiling.
   - BG / audio: format + mode compliance.
4. **Cross-reference.**
   - **Orphans:** baked files in `build/nitrofs/` with no reference in code
     (grep for the `nitro:/…` path or the generated `include!` constant).
   - **Missing:** `nitro:/…` paths / asset constants referenced in code with no
     corresponding baked file.
5. **Report.** A per-category table (PASS/WARN/FAIL + overage), the orphan and
   missing lists, and the single highest-risk item. No writes.

## Notes
- Counts from `.dl`/`.sprite` sizes are *estimates*; confirm exact poly counts
  from `obj2dl` output and palette usage from `grit` when a check is borderline.
- If the audit bounds coverage (e.g. only sampled one space), say so — don't
  imply the whole ROM passed.
