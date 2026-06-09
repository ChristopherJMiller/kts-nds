---
name: maxmod-docs
description: Look up authoritative maxmod reference material (ARM7 audio mixer used by bevy_nds_audio). Use when verifying a maxmod FFI signature, checking soundbank format details, looking up an mmEffect / mmFrame / mmInitDefault behavior, or understanding the IPC/FIFO protocol between ARM9 and ARM7. Caches the upstream maxmod repo at .claude/cache/maxmod/ (depth 1).
---

# maxmod-docs

`bevy_nds_audio` wraps **maxmod**, the ARM7-side mixer/tracker that BlocksDS
links via `-lmm9` on ARM9 and the `arm7_maxmod.elf` core on ARM7. Maxmod
has its own quirks (loop-point handling, MAS soundbank format, the FIFO
command protocol) that are not in the libnds docs and not in the BlocksDS
guides — they're in the maxmod repo itself.

## Set up the cache

The repo isn't checked in. Clone on demand:

    git clone --depth 1 https://codeberg.org/blocksds/maxmod \
      .claude/cache/maxmod

`.claude/cache/` is gitignored. If the clone already exists, use it as-is —
don't `git pull` unless the user asks.

## Where to look

Inside `.claude/cache/maxmod/`:

- `include/maxmod9.h` — the ARM9 header. This is the authoritative source
  for every FFI signature `bevy_nds_audio` declares. Cite it in comments
  next to each `extern "C"` block.
- `include/mm_types.h` — the shared type definitions (`mm_word`,
  `mm_sfxhand`, `mm_sound_effect`, etc.).
- `source_arm9/` and `source_arm7/` — the implementation. Useful when a
  behavior is surprising: the comments here explain *why* a function exists
  and what state it touches. The FIFO/IPC command IDs and packet shapes
  live here too.
- `docs/` — high-level reference (soundbank format, MAS file layout,
  how mmutil packs `.wav` / `.mod` / `.it` / `.s3m` into a single bank).

## When to use vs. skip

Use this skill when:
- Adding or auditing an `extern "C"` block in `bevy_nds_audio`.
- The user asks about `Music`, `PlaySfx`, sound priorities, or why a sound
  isn't looping (loop-point handling is the classic gotcha — `wav2bank`
  injects a forward-loop `smpl` chunk into music WAVs for exactly this).
- Debugging a soundbank issue: bank size, ID layout, mmutil flags.
- Touching the IPC seam (e.g. wondering why an effect is delayed by one
  frame — it's the FIFO).

Skip when:
- You only need libnds-level audio primitives (volume registers, channel
  enables) — those are in libnds. Reach for `blocksds-docs` instead.
- Working on the soundbank *baker* (`wav2bank`) without changing what gets
  baked — that crate is host-side and well-isolated.

## Companion: examples in the BlocksDS SDK

The BlocksDS SDK's `examples/audio/maxmod/` (see the `blocksds-docs` skill)
shows the canonical init order: `mmInitDefault(path)` →
`mmLoad(MOD_ID)` → `mmStart(MOD_ID, MM_PLAY_LOOP)` →
`mmEffect(SFX_ID)`. When in doubt about wiring order, read those examples
first; reach into the maxmod repo for the *why*.
