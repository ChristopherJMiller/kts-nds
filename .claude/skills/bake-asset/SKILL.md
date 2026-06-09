---
name: bake-asset
description: Add or re-bake a NitroFS asset (3D model, sprite, audio). Use when the user wants to add a new .obj / .png sprite / .wav, or when an asset isn't showing up in the ROM and you need to diagnose the bake. Covers the three pipelines (obj2dl, png2sprite, wav2bank), where source/output go, and what `build.rs` and `just rom` expect.
---

# bake-asset

This project has three asset pipelines, all driven by `build.rs` and all
landing under `build/nitrofs/`, which `just rom` packs into the ROM with
`ndstool -d`. The crates under `crates/{obj2dl,png2sprite,wav2bank}` are
host-side libraries + CLIs — the `lib` is what `build.rs` uses; the `bin`
is for manual one-off bakes.

| Asset    | Source                   | Baker         | Output (`build/nitrofs/`)                         | Loaded by                                       |
| -------- | ------------------------ | ------------- | ------------------------------------------------- | ----------------------------------------------- |
| 3D model | `assets/*.obj`           | `obj2dl`      | `<name>.dl`                                       | `DsMesh::load("nitro:/<name>.dl")`              |
| Sprite   | `assets/sprites/**/*.png`| `png2sprite`  | `sprites/<rel>.sprite` + `OUT_DIR/sprites.rs`     | `bevy_nds_sprite` lazily on first `Sprite` spawn |
| Audio    | `audio/{music,sfx}/*.wav`| `wav2bank`    | `soundbank.bin` + `OUT_DIR/sounds.rs`             | maxmod via `Music`/`PlaySfx`                    |

All bakers require the BlocksDS toolchain (`grit`, `mmutil`), which is on
`$PATH` **only inside `nix develop`**. Outside the shell, bakers print a
`cargo:warning=` and skip — the ROM still builds but assets are missing.

## Adding a new asset

### 3D model (`.obj`)

1. Drop the file into `assets/<name>.obj`.
2. `nix develop --command just build` — `build.rs` invokes `obj2dl::build_dir`
   and writes `build/nitrofs/<name>.dl`. Look for
   `cargo:warning=asset compilation failed:` to catch bake errors.
3. Reference it in game code: `DsMesh::load("nitro:/<name>.dl")`. The
   loader flushes the dcache before DMA — that's already wired.

Geometry is centered (`opts.center = true`), matching how the demo teapot
spins about its middle. Local AABB is computed for `bevy_nds_3d_cull`.

To bake manually (debug an `.obj` outside cargo):

    nix develop --command cargo run -p obj2dl -- \
      --input assets/<name>.obj --output /tmp/<name>.dl --center

### Sprite (`.png`)

1. Drop the file into `assets/sprites/<name>.png` (subdirectories are
   allowed — `assets/sprites/ui/cursor.png` is fine). **16-color
   indexed-palette PNG** is what `grit` wants; otherwise the bake fails or
   colors map oddly. Square sizes only: **8×8, 16×16, 32×32, 64×64** — any
   other dimensions are silently dropped at runtime by `bevy_nds_sprite`.
2. `nix develop --command just build` — `build.rs` walks the source tree
   recursively, runs `png2sprite::build_dir`, and writes
   `build/nitrofs/sprites/<rel>.sprite` (magic `BSP1` + sizes + palette +
   gfx). It also emits `$OUT_DIR/sprites.rs`, a Rust constants module of
   NitroFS paths the game `include!`s.
3. Reference the constant in game code, mirroring `sounds`:

   ```rust
   mod sprites { include!(concat!(env!("OUT_DIR"), "/sprites.rs")); }
   commands.spawn(Sprite::new(sprites::CURSOR).at(16, 8));
   // subdir → nested module: sprites::ui::CURSOR
   ```

   `bevy_nds_sprite` lazy-loads each distinct `image` path the first time
   it sees a `Sprite` carrying it, claiming the next free 4bpp palette
   bank. Cap is 16 distinct images.

Missing `grit` warning means you're not in the Nix shell. `build.rs` still
writes a predicted `sprites.rs` from the PNG filenames so the game
compiles, but the binaries aren't in the ROM and `Sprite` entities simply
don't render.

### Audio (`.wav`)

`wav2bank` packs **every** `.wav` under `audio/music/` and `audio/sfx/`
into one `soundbank.bin` plus a generated `sounds.rs` of `SFX_*` /
`MOD_*` IDs. The game `include!`s the IDs.

1. Drop a 16-bit mono PCM wav into `audio/sfx/<name>.wav` (for one-shots)
   or `audio/music/<name>.wav` (for looping tracks).
2. `nix develop --command just build`. Music WAVs get a forward-loop `smpl`
   chunk injected before `mmutil` packs them — that's how maxmod knows to
   loop the track.
3. Reference the ID in game code: `commands.spawn(PlaySfx(SFX_<NAME>))` /
   `Music(MOD_<NAME>)`. The names are upper-snake-case of the filename.

Outside `nix develop`, `wav2bank::predict_ids` still emits a stub
`sounds.rs` so the game compiles — but `soundbank.bin` won't exist, and
the ROM will be silent.

To bake manually:

    nix develop --command cargo run -p wav2bank -- \
      --input audio --output /tmp/soundbank.bin --ids /tmp/sounds.rs

## Diagnosing "the asset isn't in the ROM"

1. **Did the baker even run?** Look at the build output for
   `cargo:warning=…not found` (grit / mmutil). If you see one, you're
   outside `nix develop`.
2. **Is the file in `build/nitrofs/`?** That's the staging area.
   `ls build/nitrofs/` after a build.
3. **Did `ndstool` pack it?** `just rom` only adds `-d build/nitrofs` if
   the directory exists — empty dir is a no-op. Run `just rom` (not just
   `just build`), it's the packing step.
4. **Is the loader looking at the right path?** Runtime path is always
   `nitro:/<file>` (forward slash, lowercase). Typos here fail silently.
5. **Cache?** `read_file` flushes the dcache automatically, but if you
   wrote a custom loader, check `bevy_nds_nitrofs::flush_dcache`.

## Re-bake after a source change

Cargo tracks the source dirs via `rerun-if-changed` on `assets/`,
`assets/sprites/`, and `audio/`. Editing a file there triggers re-bake on
the next `just build`. Editing a *baker crate* (e.g. `obj2dl/src/lib.rs`)
also triggers re-bake because cargo rebuilds the build-dep.

If a re-bake doesn't seem to happen, the cleanest fix is `rm -rf
build/nitrofs && just build`.

## Don't

- Don't hard-code sound IDs. `include!` the generated `sounds.rs`.
- Don't put bakers' temp files into `build/nitrofs/` — that's what
  `wav2bank-work` and `png2sprite-work` directories under `$OUT_DIR` are
  for. Anything in `build/nitrofs/` ends up in the ROM.
- Don't extend a baker for an unrelated format. Add a new host crate
  (`<thing>2<output>`) instead — same pattern as the three we have.
