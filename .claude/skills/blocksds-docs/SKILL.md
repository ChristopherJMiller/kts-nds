---
name: blocksds-docs
description: Look up authoritative BlocksDS / libnds reference material. Use when verifying an FFI signature, finding an idiomatic usage example for a hardware subsystem (3D, sprites, audio, NitroFS, IPC, …), or checking how the upstream SDK initializes a capability. Caches the upstream SDK at .claude/cache/blocksds-sdk/ (depth 1) so you can grep docs/content/ and examples/.
---

# blocksds-docs

This project's crates wrap libnds via the BlocksDS toolchain. The upstream
SDK is the source of truth for FFI signatures, init order, and idiomatic
usage. Reach for this skill before guessing — libnds is not in your training
data in any reliable way.

## Two sources, used for different things

1. **SDK repo** — clone on demand, gitignored.

       git clone --depth 1 https://codeberg.org/blocksds/sdk \
         .claude/cache/blocksds-sdk

   What's inside:
   - `docs/content/` — markdown reference (rendered at blocksds.skylyrac.net).
     Guides on the build system, ARM7/ARM9 split, NitroFS, DLDI, libraries.
   - `examples/` — working C programs grouped by subsystem
     (`graphics_3d/`, `audio/maxmod/`, `filesystem/nitrofs/`, `input/`,
     `ipc/`, `interrupts/`, …). Read these to see init order and idiomatic
     wiring — they're the cleanest specimens of "how libnds is meant to be
     used."
   - `sys/` — link specs, CRTs, default ARM7 cores. Useful only when
     debugging the linker stage.
   - Submodules (`libs/libnds`, `libs/maxmod`, …) are **not** fetched by
     the depth-1 clone — use source #2 for headers.

   If the cache already exists, use it as-is. Don't `git pull` unless the
   user explicitly asks; the SDK changes slowly and stale-by-a-week is fine.

2. **Installed headers via the Nix shell** — already on disk, no clone needed.

       nix develop --command bash -c 'echo $BLOCKSDS'
       # → .../blocksds/core
       ls $BLOCKSDS/libs/libnds/include/nds/

   This is where every `extern "C"` declaration in our crates should be
   cited from (CLAUDE.md: "comment citing the libnds header"). Headers
   include `nds/arm9/video.h`, `nds/arm9/console.h`, `nds/input.h`,
   `nds/arm9/sprite.h`, `nds/arm9/videoGL.h`, etc.

## Typical lookups

Verify an FFI signature before adding it to a crate's `ffi.rs`:

    grep -rn "swiWaitForVBlank\|videoSetMode" \
      "$BLOCKSDS/libs/libnds/include/nds/"

Find the canonical init dance for a subsystem:

    ls .claude/cache/blocksds-sdk/examples/graphics_3d/
    cat .claude/cache/blocksds-sdk/examples/graphics_3d/textured_quad/source/main.c

Look up a concept (build flow, DLDI, maxmod soundbank format) by topic:

    grep -rl "soundbank\|mmInitDefault" \
      .claude/cache/blocksds-sdk/docs/content/ \
      .claude/cache/blocksds-sdk/examples/audio/

## When to use vs. skip

Use this skill when:
- Adding a new `extern "C"` declaration (find the header, copy the
  signature, cite it in a comment).
- Building a new capability crate and you need the libnds init sequence.
- The user asks "how does BlocksDS do X" or "is there an example of Y."

Skip when:
- The signature is already declared in one of our crates' `ffi.rs` — trust
  what's there.
- You're working on pure host-side logic (encoders, parsers, culling math) —
  no libnds involved.
