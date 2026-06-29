# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

This repo (`kts-nds`) holds **two things that evolve together**:

- **Kill the Serpent** — the game (`kts`, the root crate): a 3D cyber-dystopian
  DS capture game. *It currently boots a teapot/map tech demo* (`src/main.rs`)
  that exercises the platform while Milestone 1 de-risks the core feel; the game
  proper grows from there per the design docs below.
- **`bevy_nds`** — the companion library (the `crates/` workspace) that runs
  Bevy's `no_std` ECS on the DS. It is **actively developed here as a reusable
  engine**, and the game's needs drive which platform crates it grows. Keep it
  game-agnostic: game-specific names/paths belong in `kts`, not the library
  (e.g. the save dir `fat:/kts/` is set by the game, overriding the library's
  generic `fat:/bevy_nds/` default).

## Game design (Kill the Serpent)

The game's design is governed by documents, not ad-hoc decisions:

- **`docs/design/PILLARS.md`** is the north star — the three pillars (*the pen is
  the power* · *pressure is the puzzle* · *few tools, many combos*), the holistic
  (anti-segmentation) principle, and the prototype-first discipline.
- The **GitHub issues** on this repo are the authoritative design record.
  [#17](https://github.com/ChristopherJMiller/kts-nds/issues/17) is the hub:
  the `## Locked` control model + the repo-wide `## Open questions` register +
  the tracking index.

Two project skills keep this honest — **use them**:

- **`design-guard`** — run *before* building any design-bearing feature. It loads
  the pillars + the relevant issues, checks pillar/holistic alignment, and stops
  to surface any blocking Open question before code is written.
- **`design-sync`** — run *after* any design decision (in chat or while building).
  It writes the decision back to the owning issue (`## Locked` + dated rationale),
  updates #17, and keeps unresolved items flagged Open. `design-sync --audit`
  sweeps for decisions discussed but never recorded.

**Never assert an Open question as settled.** A decision doesn't count until it's
written to its issue.

## Project skills

Prefer these over doing the work by hand — they encode the conventions below:

- **Design governance** — `design-guard` (run *before* building a design-bearing
  feature), `design-sync` (run *after* any design decision), `design-space`
  (author a new "space" in the level graph per [#27](https://github.com/ChristopherJMiller/kts-nds/issues/27)).
- **New capability** — `add-capability` scaffolds a new `bevy_nds_<capability>`
  crate the right way (Cargo.toml, lib.rs skeleton, workspace members, opt-level,
  cfg gates, optional `DsPlugins` re-export). Use it instead of hand-rolling the
  "Adding a capability" steps below.
- **Assets** — `bake-asset` (add / re-bake a NitroFS model, sprite, or sound),
  `asset-audit` (check source + baked assets against the DS hardware budgets and
  find orphans / missing references).
- **Run & feel** — `preview-rom` (build + headless screenshot of both LCDs +
  frame-time stats), `playtest-log` (capture structured feel observations,
  especially the Milestone-1 spikes, tied to the open-questions register).
- **Reference lookups** — `blocksds-docs` (BlocksDS / libnds FFI + examples),
  `maxmod-docs` (the ARM7 audio mixer behind `bevy_nds_audio`).

## Environment

All builds need the BlocksDS toolchain, which is provided by the Nix dev shell.
`build.rs` reads `$BLOCKSDS` (set by the shell) to inject link flags, so **every
command must run inside `nix develop`** — outside it, linking fails with a
`BLOCKSDS is not set` warning. `mmutil` (used by `wav2bank` to bake audio) is
also only on `$PATH` inside the shell; without it the soundbank is skipped and
the ROM boots silent.

## Commands

The `Justfile` is the entry point (`just --list` for everything):

- `just build` / `just build-release` — compile the ARM9 ELF.
- `just check` — `cargo check` (fast feedback; no ROM).
- `just test [filter]` — host-side unit tests (see "Testing" below).
- `just fmt` — `cargo fmt`. `clippy` is installed but not wired to a task; run
  `cargo clippy` manually.
- `just rom [profile]` — package the ELF into `kts.nds` with `ndstool`,
  bundling the maxmod ARM7 core (so audio works) and `build/nitrofs/`.
- `just run [profile]` — build + package + launch melonDS.
- `just preview [profile]` — build + package + headless desmume screenshot to
  `preview.png` (CI-friendly; override with `OUT=`, `WAIT=`, `DISP=`).

`profile` defaults to `debug`; pass `release` for the smaller/faster ROM.

### Testing

The DS target is `no_std` with no test harness, so unit tests run **on the host
triple**. Plain `cargo test` does *not* work — use `just test` (or
`just test <filter>` for a single test). The recipe runs two `cargo test`
invocations split by dependency shape:

1. `bevy_nds_3d_obj`, `obj2dl`, `bevy_nds_3d_macros` — pure host crates, plain
   `--target $host`.
2. The platform subcrates (`bevy_nds_diagnostics`, `bevy_nds_time`,
   `bevy_nds_input`, `bevy_nds_gesture`, `bevy_nds_text`, `bevy_nds_bg`), `bevy_nds_3d_cull`,
   `bevy_nds_math`, `bevy_nds_cothread`, `wav2bank`, `bevy_nds_audio` — pull in code compiled against `core`, so they
   need `std` from source (`unstable.build-std=["std","panic_unwind","proc_macro"]`)
   and `panic = "unwind"` to avoid a duplicate-`core` lang-item clash and match
   the test harness. The first run is slow (builds `std`); later runs are fast.

Crates are `#![cfg_attr(not(test), no_std)]`. Bare-metal items (allocator,
panic handler, `critical-section`) live in `bevy_nds_runtime` and are gated on
`cfg(target_vendor = "nintendo")`, so they're inert when other crates are tested
on the host. **Never call FFI from tests** — split testable logic out of the FFI
call (e.g. `Grid::diff_runs` vs `Grid::flush`) and test the pure half. New
hardware code should follow that pattern.

## Architecture

The workspace follows a **"one capability, one crate"** pattern. Every DS
subsystem is its own additive crate; the **`bevy_nds`** umbrella re-exports
them and bundles the platform layer as `DsPlugins`. Games can depend on the
umbrella for the full platform, or on individual subcrates to opt out of what
they don't need (e.g. drop `bevy_nds_text` for a sprite-only game).

**Platform subcrates** (re-exported by `bevy_nds`):

- **`crates/bevy_nds`** — umbrella. Re-exports + `DsPlugins` plugin group.
- **`crates/bevy_nds_runtime`** — bare-metal items (allocator, panic, critical-section)
  + the vblank-driven `run()` loop. Items are gated on `cfg(target_vendor = "nintendo")`
  so they don't clash with `std` during host tests.
- **`crates/bevy_nds_video`** — `DsScreen`, `Consoles`, `VideoPlugin` (text
  consoles on both LCDs).
- **`crates/bevy_nds_input`** — buttons + touch as Bevy's `ButtonInput<DsButton>`
  and `Touches`.
- **`crates/bevy_nds_gesture`** — tap/long-press/swipe/drag from the touch stream.
- **`crates/bevy_nds_time`** — drives Bevy's `Time` off the hardware bus-clock timer.
- **`crates/bevy_nds_diagnostics`** — smoothed `Fps` resource.
- **`crates/bevy_nds_text`** — tile-console text renderer (`Glyph`/`DsText`/`TilePos`).
- **`crates/bevy_nds_sprite`** — 2D hardware sprites (OAM) on the sub engine.
  Lazy-loads `.sprite` assets from NitroFS on first sight of a `Sprite`
  carrying their path, up to 16 distinct images (one per 4bpp palette
  bank). Pure parser + size-code logic host-tested.
- **`crates/png2sprite`** — host CLI/library that wraps BlocksDS's `grit`
  to bake `assets/sprites/**/*.png` (recursive) into `.sprite` NitroFS
  assets under `nitro:/sprites/`. Also emits `OUT_DIR/sprites.rs` — a
  Rust constants module of paths the game `include!`s (mirrors
  `wav2bank`'s `sounds.rs`).
- **`crates/bevy_nds_nitrofs`** — mounts the ROM filesystem and exposes
  `read_file` / `flush_dcache`. Shared by 3D, audio, and any future asset loader.
- **`crates/bevy_nds_math`** — 20.12 fixed-point (`Fx32`, `FxVec2`, `FxVec3`)
  and safe wrappers around the DS hardware divide/sqrt coprocessor
  (`<nds/arm9/math.h>`, MMIO at `0x0400_0280` / `0x0400_02B0`), with software
  fallbacks for host tests. The no-FPU analogue of `portable-atomic`'s no-CAS
  story: used on per-frame math hot paths to avoid software-emulated `f32`.
- **`crates/bevy_nds_cothread`** — libnds cooperative threads
  (`<nds/cothread.h>`) wrapped as a `Tasks` resource + `Task<T>` handle. The
  runtime's vblank wait yields to spawned tasks (`cothread_yield_irq` in place
  of `swiWaitForVBlank`), so blocking work (NitroFS reads, saves, WiFi) can
  run off the per-frame critical path. Tasks are spawned **detached**: the
  libnds scheduler reaps them on completion, sidestepping a use-after-free
  against the scheduler's saved `next_ctx` that you get if you call
  `cothread_delete` from another running cothread.
- **`crates/bevy_nds_rtc`** — `WallClock` resource (broken-down
  year/month/day + h/m/s + weekday plus Unix `unix_secs`) sourced from the DS
  real-time clock via newlib `<time.h>` (single `time(NULL)` FFI call).
  Sibling to `bevy_nds_time` — that drives the monotonic `Time` from the
  hardware timer; this is the orthogonal wall-clock axis for save timestamps,
  day/night, RNG seeding. Civil decomposition (Howard Hinnant's algorithm) is
  pure Rust and host-tested.
- **`crates/bevy_nds_save`** — writable-filesystem persistence. Mounts FAT
  (DLDI flashcart, DSi SD) once via `fatInitDefault()` and exposes a
  slot-keyed `SaveStorage` resource. Both blocking (`read`/`write`) and
  cothread-async (`read_async`/`write_async` → `Task<T>`) flavours over the
  same newlib stdio backend (`fopen`/`fread`/`fwrite`). `StorageStatus`
  reports availability (FAT can fail to mount on unsupported flashcarts);
  callers degrade gracefully. Pure slot-path joining + name validation
  (rejects `..`, `/`, NUL) is host-tested.

**Capability crates** (additive, depended on directly by games when used):

- **`crates/bevy_nds_3d`** — hardware 3D backend (`Transform3d`, `DsMesh`,
  `Camera3d`, frustum culling, NitroFS model loading).
- **`crates/bevy_nds_3d_obj`** — host-side OBJ → display-list encoder, the
  single source of truth for geometry packing.
- **`crates/bevy_nds_3d_macros`** — `include_obj!` proc-macro that bakes a
  display list into the ARM9 binary at compile time.
- **`crates/bevy_nds_3d_cull`** — pure, host-testable view-frustum math.
- **`crates/obj2dl`** — host CLI + library, used by `build.rs` to bake
  `assets/*.obj` into `build/nitrofs/*.dl`.
- **`crates/bevy_nds_audio`** — maxmod (ARM7) audio backend: declarative
  `Music` resource, `PlaySfx` events.
- **`crates/wav2bank`** — host CLI + library wrapping BlocksDS `mmutil` to bake
  `audio/{music,sfx}/*.wav` into `soundbank.bin` plus a Rust module of
  `SFX_*` IDs the game `include!`s. Also injects a forward-loop `smpl` chunk
  into music WAVs so maxmod loops them.
- **`crates/bevy_nds_bg`** — 2D background layers (BG). Exposes a
  `Backgrounds` resource with `set_tile` / `set_bitmap` / `set_tile_scroll`
  setters keyed on `(DsScreen, BgKind)`. Tile BGs land on layer BG1 (4bpp,
  32×32 tiles = one screen-fill), bitmap BGs on extended layer BG3 (16bpp,
  256×256 direct-color). Lazy-loads `.bg` / `.bbg` blobs from NitroFS. Tile
  palette goes in bank 1 so the text console's bank-0 font keeps rendering.
  **Bitmap requires video mode 5**: it only works on the engine that
  *isn't* hosting 3D (the 3D plugin's Startup write to `REG_DISPCNT` forces
  mode 0 on the main engine, breaking bitmap there). Pure asset parser
  host-tested.
- **`crates/png2bg`** — host CLI/library wrapping BlocksDS's `grit` to bake
  `assets/backgrounds/tiled/**/*.png` into `.bg` and
  `assets/backgrounds/bitmap/**/*.png` into `.bbg`. Emits
  `$OUT_DIR/backgrounds.rs` (constants module of NitroFS paths,
  `backgrounds::tiled::*` / `backgrounds::bitmap::*`).
- **`crates/bevy_nds_scene`** — *game-agnostic* space/scene loader (issue #27).
  Loads a baked `.scene` blob from NitroFS and spawns each authored instance as
  a rendered entity (mesh + `Transform3d` + `DsMaterial`) tagged with a
  `SceneInstance { role }` — an **opaque** role string the game maps onto its
  own components. Also exposes a `LoadedScene` resource (camera mode, exits) and
  a `LoadSpace` event. No new FFI: it composes `bevy_nds_nitrofs` (bytes) +
  `bevy_nds_3d` (meshes). Pure `asset::parse` host-tested. Depended on directly
  (not in `DsPlugins`).
- **`crates/scene2bin`** — host CLI + library: bakes a **level** directory
  (`assets/levels/<name>/` = a `level.ron` manifest of the zone-graph layout +
  one `<zone>.ron` content file per zone) into `build/nitrofs/levels/<name>/
  *.scene` (+ a nested `levels.rs` constants module the game `include!`s, e.g.
  `levels::facility::ATRIUM`). Resolves reusable **prefabs** (`assets/prefabs/
  *.ron`, instance templates) into flat instances host-side at bake — the
  `.scene` blob never learns what a prefab is. Parse + validate (referenced
  meshes/prefabs) + derive connections from zone abutment + encode; the
  authoritative writer for the `.scene` format (`bevy_nds_scene` is the reader —
  keep the two in sync). Also `to_{level,zone,prefab}_ron` for the editor. RON is
  **host-only**; it never reaches the DS. *A level is the authoring/distribution
  unit; a zone is the runtime streaming unit — only the current zone is
  resident.*
- **`tools/scene-editor`** — standalone desktop GUI (eframe/egui) for authoring
  a whole **level**: one shared top-down canvas drawing every zone at its global
  `place` (drag whole zones / instances / waypoints) + a side panel for the
  manifest (name/entry/zone list), per-zone camera/bounds, and a prefab-`Use`
  picker, reading/writing the same RON via `scene2bin`. Split into modules
  (`app`/`canvas`/`panel`/`widgets`). **Detached from this workspace** (its own
  `[workspace]` + `.cargo/config.toml` re-target the host with a full-`std`
  build-std, since the repo root forces `build-std=[core,alloc]`). Run with
  `just edit [level-dir]`. `preview-rom` stays the DS-faithful check; the editor
  is for fast spatial layout.
- **`kts`** (root, `src/main.rs`) — *Kill the Serpent*, the game. A *pure-Bevy
  consumer*: only components and systems, **no FFI / allocator / panic handler**.

New game logic belongs in the root crate; new hardware capability gets its own
crate (see "Adding a capability" below).

### Adding a capability

When you reach for a new DS subsystem (sprites, save data, Wi-Fi, RTC, …),
**add a new crate, don't expand an existing one**. The `add-capability` skill
scaffolds all of this; the pattern it follows:

1. `crates/bevy_nds_<capability>/` with its own `Cargo.toml` and `src/lib.rs`.
2. Hand-written FFI lives in `src/ffi.rs` (or inline in `lib.rs` if small),
   with header citations. Don't extend another crate's FFI for unrelated work.
3. If it loads from the ROM filesystem, depend on `bevy_nds_nitrofs` and order
   PreStartup work `.after(bevy_nds_nitrofs::init_nitrofs)`.
4. Split pure logic from FFI and host-test the pure half; new code under
   `target_vendor = "nintendo"` cfg if it must compile differently on the host.
5. Add the crate to the workspace `members` + the `[profile.dev.package.*]`
   opt-level overrides if it's on the per-frame hot path.
6. If it belongs in `DsPlugins` (i.e. universally useful), re-export it from
   `bevy_nds` and add to the plugin group + `prelude`. Otherwise consumers
   depend on the crate directly.

**Don't consolidate** into an existing crate "for now" — every consolidation
adds duplicate FFI declarations across crates and obscures the dep graph. If a
capability outgrows itself, splitting *out* later is more work than just
starting in its own crate.

### DS hardware → ECS mapping

| DS hardware              | Exposed as                                              | Subcrate / plugin                                   |
| ------------------------ | ------------------------------------------------------- | --------------------------------------------------- |
| Top / bottom LCDs        | `DsScreen` component + `Consoles` resource              | `bevy_nds_video::VideoPlugin`                       |
| Buttons (`REG_KEYINPUT`) | `ButtonInput<DsButton>` resource                        | `bevy_nds_input::InputPlugin`                       |
| Touch screen             | `Touches` resource + `TouchInput` events                | `bevy_nds_input::InputPlugin`                       |
| Touch gestures           | `Gestures` resource + `GestureEvent` events             | `bevy_nds_gesture::GesturePlugin`                   |
| ROM filesystem (NitroFS) | `NitroFs` resource + `read_file` / `flush_dcache`       | `bevy_nds_nitrofs::NitroFsPlugin`                   |
| 3D touch picking         | `TouchPick` resource (mesh entity under the pen)        | `bevy_nds_3d::Ds3dPlugin`                           |
| Vertical-blank @ 60 Hz   | `set_runner` loop + hardware `Time` resource            | `bevy_nds_runtime::run` + `bevy_nds_time::TimePlugin` |
| —                        | smoothed `Fps` resource                                 | `bevy_nds_diagnostics::DiagnosticsPlugin`           |
| Tiled text background    | `Glyph` / `DsText` + `TilePos`, extracted each frame    | `bevy_nds_text::TextRenderPlugin`                   |
| 2D sprites (OAM)         | `Sprite` component (x, y in pixels)                     | `bevy_nds_sprite::SpritePlugin`                     |
| 3D geometry engine       | `Transform3d` + `DsMesh` + `Camera3d` resource          | `bevy_nds_3d::Ds3dPlugin`                           |
| ARM7 sound (maxmod)      | `Music` resource (looping) + `PlaySfx` events           | `bevy_nds_audio::AudioPlugin`                       |
| Math coprocessor (div/sqrt) | `Fx32` + `FxVec2`/`FxVec3`; `hw::div_*` / `hw::sqrt_*` | `bevy_nds_math`                                  |
| Cooperative threads (`cothread`) | `Tasks` resource + `Task<T>` handle (`spawn` / `poll`) | `bevy_nds_cothread::CothreadPlugin`           |
| Real-time clock          | `WallClock` resource (year/month/day + h/m/s + unix_secs) | `bevy_nds_rtc::RtcPlugin`                     |
| Writable FAT/SD storage  | `SaveStorage` resource (blocking + async slot I/O) + `StorageStatus` | `bevy_nds_save::SavePlugin`              |
| 2D background layers (BG) | `Backgrounds` resource (`set_tile` / `set_bitmap` / `set_tile_scroll`) | `bevy_nds_bg::BackgroundPlugin` |

`DsPlugins` (in `bevy_nds`) bundles the platform-layer plugins;
`bevy_nds::run(app)` (re-export from `bevy_nds_runtime`) installs the runner
(`swiWaitForVBlank` → `app.update()`).

### Rendering model

`bevy_nds_text` mirrors desktop Bevy's "extract entities to the GPU" shape, but
the "GPU" is the DS text console and the draw call is libnds `printf`. It is
**double-buffered and diffed at the grid level**: a static `front` buffer mirrors
the live tilemap, a `back` buffer is composed fresh each frame, and only
*differing* cells are written to hardware. Never call `consoleClear()` per frame
— that reintroduces flicker. Grid is fixed at 32×24 tiles (libnds default font).
`bevy_text` is intentionally *not* used (cosmic-text is too heavy for the DS).

### Asset pipeline

A model is always *bytes at a ROM address*. `bevy_nds_3d_obj` encodes an OBJ
into a libnds display list with all fixed-point/normal packing done host-side.
Two delivery paths produce **byte-identical** geometry:

- **Baked into the binary** via `include_obj!("model.obj")` — embeds a
  `&'static` display list in the ARM9 binary.
- **Loaded from NitroFS** via `build.rs` → `obj2dl` → `build/nitrofs/*.dl`,
  packed by `just rom` (`ndstool -d`) and read at runtime by
  `DsMesh::load("nitro:/model.dl")` (cache-flushes before the DMA).

Meshes carry a local AABB used by `bevy_nds_3d_cull` for view-frustum culling.

### Sprite pipeline

`bevy_nds_sprite` drives the sub engine's OAM. Sprite tile data + a 16-entry
palette live in a baked `.sprite` blob: `build.rs` walks
`assets/sprites/**/*.png` (recursive), calls `png2sprite` (wraps BlocksDS's
`grit`) and writes `build/nitrofs/sprites/<rel>.sprite`, which `just rom`
packs into the ROM. It also emits `$OUT_DIR/sprites.rs` — a Rust constants
module of NitroFS paths, with subdirectories rendered as nested modules
(e.g. `sprites::CURSOR`, `sprites::ui::CURSOR`) — that the game `include!`s.

Sprites are referenced by passing a constant to `Sprite { image, x, y }`.
The plugin's `SpriteAssets` resource lazy-loads each distinct `image` path
the first time it is observed, claiming the next free 4bpp palette bank
(cap = 16). Failed loads silently leave the sprite hidden. Supported square
sizes only: 8×8, 16×16, 32×32, 64×64. The on-disk format (magic `"BSP1"` +
sizes + palette + gfx) is defined once in `png2sprite::encode` and parsed
by `bevy_nds_sprite::asset::parse` — keep the two in sync.

### Audio pipeline

Sound is mixed on the **ARM7** by maxmod, driven from the ARM9 over FIFO/IPC.
The ROM must embed the maxmod ARM7 core (`just rom` selects `arm7_maxmod.elf`)
and link `-lmm9`. `build.rs` runs `wav2bank` to produce
`build/nitrofs/soundbank.bin` plus `$OUT_DIR/sounds.rs` (the `SFX_*` IDs the
game `include!`s, so no hard-coded indices). maxmod loads the bank from
`nitro:/` at runtime.

## Conventions

- **`no_std` everywhere.** Both binary and library crates are `#![no_std]`;
  `src/main.rs` is also `#![no_main]` with
  `#[unsafe(no_mangle)] extern "C" fn main`. Use `core` / `alloc`
  (`extern crate alloc;`), never `std`.
- **FFI is hand-written and lives in the crate that uses it** — no bindgen, no
  shared `bevy_nds::ffi`. Each subcrate declares the minimal libnds surface it
  needs (inline in `lib.rs` for small surfaces, `src/ffi.rs` for the heavier
  3D/audio cases), with a comment citing the libnds header (e.g. `<nds/input.h>`).
  Symbols resolve at the demo's final link via `build.rs`. Duplicate `extern "C"`
  declarations across crates are fine as long as signatures match.
- **Raw pointers in resources** (e.g. `ConsoleHandle`) get manual
  `unsafe impl Send + Sync` justified by "the DS is single-core". Keep that
  SAFETY comment.
- **Plugins, not free functions.** Each capability is a Bevy `Plugin`; the
  game groups its own systems in a `GamePlugin`. Re-export public plugins/types
  from `lib.rs` and add game-facing items to `bevy_nds::prelude`.
- **Schedule usage:** hardware init in `PreStartup` (e.g. `init_screens`), game
  setup in `Startup`, per-frame logic in `Update`.
- **Avoid per-frame heap churn.** Reuse buffers / `String` capacity (e.g.
  `update_hud` calls `text.0.clear()` then `write!`) rather than allocating
  each frame — RAM is ~4 MB and the ARM9 is 33 MHz.

## Build internals (rarely touched, but load-bearing)

- **Target.** Custom Tier-3 spec `armv5te-nintendo-ds.json` (ARM946E-S, no std,
  `panic = "abort"`, soft-float). `.cargo/config.toml` enables `-Z build-std`
  (core/alloc from source) and sets `--cfg portable_atomic_no_outline_atomics`.
- **Linking.** `build.rs` injects `-specs=$BLOCKSDS/sys/crts/ds_arm9.specs`,
  plus `-lnds9 -lmm9 -lc -lgcc` inside `--start-group/--end-group` (libgcc
  provides the atomic-barrier helpers the BlocksDS specs alias).
- **Atomics.** No hardware CAS on ARM946E-S; `portable-atomic` (pulled in by
  Bevy via the `critical-section` feature) sits on the interrupt-toggling
  `critical-section` impl in `bevy_nds_runtime`. **Keep the `critical-section`
  feature on every Bevy crate.**
- **Profiles.** Both profiles use `panic = "abort"`. Dev optimises *all
  dependencies* (`[profile.dev.package."*"] opt-level = 3`) and each of our
  engine subcrates explicitly (the `bevy_nds*` family) so the debug ROM still
  hits 60 fps on the 33 MHz ARM9. The *game* crate (`kts`) is at
  `opt-level = 1` — every additional capability plugin (sprite, audio, bg, …)
  adds another batch of monomorphized Bevy ECS code, and at `opt-level = 0`
  the debug binary grew past the 3.5 MiB EWRAM ceiling. opt-level=1 brings it
  back under without noticeably slowing rebuilds. When adding a new subcrate,
  append it to the per-package list in the root `Cargo.toml`.
