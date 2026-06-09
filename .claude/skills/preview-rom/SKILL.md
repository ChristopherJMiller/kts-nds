---
name: preview-rom
description: Build the ROM and capture a headless screenshot of both LCDs via desmume + Xvfb. Use to visually verify a change (UI tweak, sprite, 3D scene, text layout) without launching the melonDS GUI — works in CI, on remote shells, and inside Nix. Wraps `just preview` with sensible defaults and a quick interpretation pass.
---

# preview-rom

The DS has *two* LCDs stacked vertically (top: 256×192, bottom: 256×192).
"Does this look right?" almost always means looking at both. `just preview`
already does the heavy lifting (Xvfb + headless desmume + `import`); this
skill is the wrapper that picks knobs and looks at the result.

## Run

Inside the Nix dev shell — `just preview` chains the ROM build, so a stale
build is fine, it'll rebuild.

    nix develop --command just preview            # debug, preview.png, 10 s wait
    nix develop --command just preview release    # release profile

Knobs (env vars passed through `just`):

- `OUT=foo.png` — output path. Default `preview.png`.
- `WAIT=12` — seconds to let the ROM run before screenshotting. Increase
  if the change shows up after a state transition (intro, gesture trigger,
  audio cue). The Xvfb timeout is `WAIT + 20` and the emulator timeout is
  `WAIT + 6`, so high values are safe.
- `DISP=:99` — Xvfb display number. Bump if `:99` is already in use
  (rare; harmless to leave alone).

Example — capture release build after 15 s, write to `out.png`:

    nix develop --command bash -c \
      'OUT=out.png WAIT=15 just preview release'

## Inspect

The captured PNG is 256×384 (top LCD stacked over bottom). Use Read on
the file — it's a real image and the tool will show it.

Both LCDs should be visible. If one screen is black, that's *probably*
correct (some demos only use one), but verify against the change you made:
e.g. `bevy_nds_sprite` draws to the **sub engine (bottom LCD)**, so a sprite
change you can't see on top is fine.

## Quick interpretation

- **Completely black both screens** → the ROM panicked or didn't boot.
  Check `/tmp/bevy-ds-emu.log` for desmume's exit reason and
  `/tmp/bevy-ds-xvfb.log` for the X server.
- **Text garbled or in wrong cells** → the diff-renderer in `bevy_nds_text`
  is mis-rendering. Check `front`/`back` grid state in your edits.
- **Sprite invisible** → `.sprite` blob didn't bake (look for the
  `cargo:warning=grit not found` line in the build output) or the sprite
  is off-screen — the fallback is a 16×16 magenta cursor in the
  top-left of the bottom screen.
- **3D blank but text fine** → display-list didn't load. `nitro:/*.dl`
  likely didn't get packed by `ndstool` (check `build/nitrofs/`).

## When to use vs. skip

Use when:
- You changed *anything* visual (text, sprite, 3D, console layout).
- The user says "verify," "screenshot," "does it look right," or asks
  whether a UI change worked.
- Type checks and tests pass but you want eyes on it before reporting done.

Skip when:
- Change is host-side only (encoder, parser, math) — `just test` covers it.
- User explicitly wants the interactive emulator (`just run`).

## Tell the user what you saw

Don't just say "the preview ran." Describe what's on each screen, and call
out anything unexpected. If the screenshot doesn't match the change you
intended, **say so** rather than claim success — the change might be
ordered wrong, gated by a state that needs longer `WAIT=`, or rendered to
the wrong engine.
