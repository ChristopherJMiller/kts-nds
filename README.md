# bevy-ds

[Bevy](https://bevyengine.org/)'s ECS running on a Nintendo DS, built into an
`.nds` ROM that boots in an emulator or on hardware. The build runs entirely
inside a Nix dev shell.

The workspace follows a "one capability, one crate" pattern: every DS subsystem
(video, input, gestures, text, audio, 3D, …) is its own additive crate. Games
depend on the **`bevy_nds`** umbrella, which re-exports everything and bundles
the platform layer as a single `DsPlugins` group; they can also depend on
individual subcrates directly and opt out of whatever they don't need (e.g.
drop `bevy_nds_text` for a sprite-only game).

- **`bevy_nds`** (`crates/bevy_nds`) — umbrella crate. Re-exports the platform
  subcrates and bundles them as `DsPlugins`.
- **`bevy_nds_runtime`** (`crates/bevy_nds_runtime`) — bare-metal items
  (`#[global_allocator]`, `#[panic_handler]`, `critical-section`) plus the
  vblank-driven `run()` loop. Must appear in the final binary exactly once.
- **`bevy_nds_video`** (`crates/bevy_nds_video`) — `DsScreen`, `Consoles`, and
  the `VideoPlugin` that brings up a text console on both LCDs.
- **`bevy_nds_input`** (`crates/bevy_nds_input`) — buttons + touch surfaced
  through Bevy's standard `ButtonInput` / `Touches`.
- **`bevy_nds_gesture`** (`crates/bevy_nds_gesture`) — pure tap/swipe/drag
  recognition over the touch stream.
- **`bevy_nds_time`** (`crates/bevy_nds_time`) — drives Bevy's `Time` resource
  off the DS bus-clock hardware timer.
- **`bevy_nds_diagnostics`** (`crates/bevy_nds_diagnostics`) — smoothed `Fps`
  resource.
- **`bevy_nds_text`** (`crates/bevy_nds_text`) — tile-console text renderer,
  expressed as an ECS extraction step (diffed, flicker-free).
- **`bevy_nds_sprite`** (`crates/bevy_nds_sprite`) — 2D hardware sprites
  (OAM): up to 128 movable image objects on the sub engine, drawn over the
  text/tile background. Asset is `grit`-baked from a PNG into a NitroFS
  `.sprite` blob; the crate also carries a small embedded fallback so it
  works without the asset pipeline.
- **`png2sprite`** (`crates/png2sprite`) — host CLI/lib that wraps BlocksDS's
  `grit` to bake `assets/sprites/*.png` into `.sprite` NitroFS assets.
- **`bevy_nds_nitrofs`** (`crates/bevy_nds_nitrofs`) — mounts the ROM filesystem
  in `PreStartup` and provides `read_file` / `flush_dcache`. Shared by `bevy_nds_3d`,
  `bevy_nds_audio`, and any future asset-loading subsystem.
- **`bevy_nds_3d`** (`crates/bevy_nds_3d`) — hardware 3D backend: `Transform3d`,
  `DsMesh`, `Camera3d`, view-frustum culling, model loading (baked or from
  NitroFS at runtime).
- **`bevy_nds_3d_obj`** (`crates/bevy_nds_3d_obj`) — host-side Wavefront OBJ →
  display-list encoder. Single source of truth for the geometry packing.
- **`bevy_nds_3d_macros`** (`crates/bevy_nds_3d_macros`) — `include_obj!` proc-macro.
- **`bevy_nds_3d_cull`** (`crates/bevy_nds_3d_cull`) — pure, host-testable
  view-frustum culling math.
- **`bevy_nds_audio`** (`crates/bevy_nds_audio`) — maxmod-backed music + SFX.
- **`bevy_nds_math`** (`crates/bevy_nds_math`) — 20.12 fixed-point (`Fx32` /
  `FxVec2` / `FxVec3`) and safe wrappers around the DS hardware divide/sqrt
  coprocessor. Pure, host-testable; replaces software `f32` in the per-frame
  math hot paths (the no-FPU ARM946E-S analogue of `portable-atomic`'s no-CAS
  story).
- **`bevy_nds_cothread`** (`crates/bevy_nds_cothread`) — libnds cooperative
  threads as a Bevy-friendly `Tasks` resource + `Task<T>` handle. The runtime's
  vblank wait yields to spawned tasks, so blocking work (NitroFS reads, saves,
  WiFi) can run off the per-frame critical path without dropping below 60 fps.
- **`obj2dl`** (`crates/obj2dl`) — host CLI/lib that bakes OBJ models into `.dl`
  NitroFS assets; used by the demo's `build.rs`.
- **`wav2bank`** (`crates/wav2bank`) — host CLI/lib that wraps `mmutil` to bake
  WAVs into `soundbank.bin`.
- **`perfread`** (`crates/perfread`) — host CLI that pulls
  `bevy_nds_diagnostics::PERF_BLOB` (a frame-time ring in main RAM) out of a
  running emulator over desmume's gdbstub and prints `min/avg/p50/p95` frame
  time. `just preview` invokes it next to the screenshot so each preview run
  reports both *what* the demo looked like and *how* it performed.
- **`bevy-ds`** (the root crate) — the demo. Plain Bevy components and systems,
  with no FFI, allocator or panic handler.

<p align="center">
  <img src="docs/demo.png" alt="The map + HUD on the top screen with the hardware-lit teapots on the bottom" width="320">
</p>

The demo renders a hardware-lit Utah teapot on one screen and a text HUD on the
other, with a smaller second teapot spinning beside it (two independent model
matrices composed on the CPU each frame). The D-pad moves the player's teapot,
ABXY rotate it, and moving it off the edge sends it to the other screen. The
models are loaded at runtime from the ROM filesystem (NitroFS), falling back to a
copy baked into the binary if the filesystem is unavailable — the HUD shows which
path was taken.

The screen-crossing follows from the hardware layout: the 3D core is attached to
the main 2D engine, and the `POWER_SWAP_LCDS` bit selects which physical LCD that
engine drives. The sub engine drives the other one. The teapot and the HUD are
therefore always on opposite screens, and a `Display3d` resource controls which
is which. Crossing the edge toggles the bit, swapping both at once.

## How it works

The full `bevy` crate depends on `wgpu` and `winit` and won't run on the DS.
Bevy's core has been `no_std`-capable since 0.16, so `bevy_nds` uses those pieces
and supplies the platform layer itself. DS hardware is mapped onto ordinary Bevy
concepts so game code doesn't deal with it directly:

| DS hardware              | Exposed as                                                            | Subcrate / plugin                                |
| ------------------------ | --------------------------------------------------------------------- | ------------------------------------------------ |
| Top / bottom LCDs        | `DsScreen::{Top,Bottom}` component + `Consoles` resource              | `bevy_nds_video::VideoPlugin`                    |
| Buttons (`REG_KEYINPUT`) | the standard `ButtonInput<DsButton>` resource                         | `bevy_nds_input::InputPlugin`                    |
| Touch screen (`touchRead`) | the standard `Touches` resource + `TouchInput` events               | `bevy_nds_input::InputPlugin`                    |
| Touch gestures (derived) | `Gestures` resource + `GestureEvent` events                           | `bevy_nds_gesture::GesturePlugin`                |
| ROM filesystem (NitroFS) | `NitroFs` resource + `read_file` / `flush_dcache`                     | `bevy_nds_nitrofs::NitroFsPlugin`                |
| 3D touch picking         | `TouchPick` resource (mesh entity under the pen, via position test)   | `bevy_nds_3d::Ds3dPlugin`                        |
| Vertical-blank @ ~60 Hz  | a `set_runner` frame loop + a real `Time` resource (hardware timer)   | `bevy_nds_runtime::run` + `bevy_nds_time::TimePlugin` |
| —                        | a smoothed `Fps` resource for diagnostics                             | `bevy_nds_diagnostics::DiagnosticsPlugin`        |
| Tiled text background    | `Glyph` / `DsText` + `TilePos`, drawn by an extraction system         | `bevy_nds_text::TextRenderPlugin`                |
| 2D hardware sprites (OAM) | `Sprite` component (x, y in pixels)                                  | `bevy_nds_sprite::SpritePlugin`                  |
| 3D geometry engine       | `Transform3d` + `DsMesh` + a `Camera3d` resource                      | `bevy_nds_3d::Ds3dPlugin`                        |
| ARM7 sound (maxmod)      | `Music` resource (looping) + `PlaySfx` events                         | `bevy_nds_audio::AudioPlugin`                    |
| Math coprocessor (divide/sqrt) | `Fx32` + `FxVec2`/`FxVec3` (20.12 fixed-point), `hw::div_*`/`hw::sqrt_*` | `bevy_nds_math`                          |
| Cooperative threads (`cothread`) | `Tasks` resource + `Task<T>` handle (`spawn` / `poll`)              | `bevy_nds_cothread::CothreadPlugin`              |

`DsPlugins` bundles all of it, and `bevy_nds::run(app)` installs the runner that
owns the frame loop (`cothread_yield_irq(IRQ_VBLANK)` → `app.update()`; with no
tasks spawned this reduces to a plain vblank wait).

### Rendering model

Desktop Bevy extracts entities to the GPU every frame. `bevy_nds` keeps that
shape, but the "GPU" is the DS text console (a tiled background) and the draw
call is a libnds `printf`. A drawable is any entity with a `TilePos` and a
`DsScreen`, plus either a `Glyph` (one character) or a `DsText` (a string).

The renderer is double-buffered at the grid level to avoid flicker. Each screen
keeps a `front` buffer mirroring the live tilemap and a `back` buffer composed
from scratch each frame. The render system stamps every drawable into `back`,
then writes only the changed cells to the hardware and copies them into `front`.
The screen is never blanked, and most frames touch only a few tiles. This avoids
both the flash of a full `consoleClear()` and per-frame allocation.

`bevy_text` (cosmic-text font rasterisation) is too heavy for the DS, so it's
dropped and replaced with this small `no_std` text layer on the tile engine.

This is the same approach
[`bevy_mod_gba`](https://github.com/bushrat011899/bevy_mod_gba) takes for the
Game Boy Advance.

### Bare-metal runtime

`bevy_nds_runtime` provides the pieces a bare-metal Rust program needs:

- a `#[global_allocator]` on top of newlib's heap (set up by the BlocksDS crt0),
- a `#[panic_handler]`, and
- a `critical-section` impl that toggles the DS interrupt-enable register, which
  is what Bevy's atomics (`portable-atomic`) sit on.

These items must appear in the final binary exactly once; depending on the
`bevy_nds` umbrella pulls them in transitively. They're `cfg`-gated to the DS
target (`target_vendor = "nintendo"`) so the subcrates can still link against
`std` for host unit tests.

## Prerequisites

- [Nix](https://nixos.org/) with flakes enabled.

The dev shell provides the Rust nightly toolchain, the BlocksDS SDK, `ndstool`,
the melonDS and desmume emulators, and the preview tooling.

BlocksDS comes in as a proper Nix derivation (no `buildFHSEnv`) via
[`pgattic/blocksds-nix`](https://github.com/pgattic/blocksds-nix), which patches
the official toolchain into the Nix store and exports `$BLOCKSDS` /
`$WONDERFUL_TOOLCHAIN`.

## Quick start

```sh
nix develop          # enter the dev shell (first run builds/fetches the toolchain)

just build           # compile the ARM9 ELF (debug)
just rom             # package it into bevy-ds.nds with ndstool
just run             # build + package + launch melonDS
just preview         # build + package + headless desmume screenshot -> preview.png
```

For the smaller, faster build, append `release`, e.g. `just run release`.

### Tasks

| Command                  | Description                                                |
| ------------------------ | ---------------------------------------------------------- |
| `just build`             | Compile the ARM9 ELF (debug).                              |
| `just build-release`     | Compile the ARM9 ELF (release).                            |
| `just rom [profile]`     | Package an ELF into `bevy-ds.nds` (`ndstool`).             |
| `just run [profile]`     | Build, package, and run in **melonDS** (interactive).      |
| `just preview [profile]` | Build, package, boot in **desmume** headlessly, save `preview.png` and print frame-time stats (`samples=… min=… avg=… p95=… fps_avg=…`) read from the ROM's `PERF_BLOB` via the gdbstub. Override with `OUT=`, `WAIT=`, `DISP=`, `GDBPORT=`. |
| `just snap [profile]`    | Like `preview`, but with a short default `WAIT` for grabbing the first stable frame (README banners, changelog snaps). Accepts fractional seconds. |
| `just check`             | `cargo check`.                                             |
| `just test [filter]`     | Run the `bevy_nds` host-side unit tests (builds for the host triple). |
| `just fmt`               | `cargo fmt`.                                               |
| `just clean`             | Remove build artifacts and the ROM.                        |

### Testing

The hardware-independent logic has unit tests, one batch per subcrate: the
render diffing (`bevy_nds_text`), the timer-tick→nanoseconds conversion
(`bevy_nds_time`), the FPS smoothing (`bevy_nds_diagnostics`), the button-mask
mapping and touch-state diff (`bevy_nds_input`), the gesture state machine
(`bevy_nds_gesture`), the OBJ→display-list packing (`bevy_nds_3d_obj`), the
view-frustum culling math (`bevy_nds_3d_cull`), the WAV loop-injection and
soundbank-ID parsing (`wav2bank`), and the audio volume/panning quantisation
(`bevy_nds_audio`). They run on the host, not the DS:

```sh
just test          # run all host unit tests
just test render   # run only tests whose name matches "render"
```

The crates are `no_std` only when not under `cfg(test)`, so the test build links
the host `std` and the standard test harness. `just test` compiles for the host
triple and overrides the project's `build-std`/panic settings for that run (see
the `Justfile`). Dependency-free crates test under a plain host target, while
crates that pull in `core`-compiled dependencies need a `std`-from-source build
to avoid a duplicate `core` lang-item clash, so the recipe runs two `cargo test`
invocations. The first run builds `std` and is slow; later runs are fast.
Hardware calls are kept out of the tested functions, so no DS or emulator is
required.

## Project layout

```
flake.nix                       dev shell: Rust nightly + BlocksDS + emulators + preview tools
rust-toolchain.toml             pins nightly + rust-src (for build-std)
armv5te-nintendo-ds.json        custom Tier-3 target spec (ARM946E-S, no_std)
.cargo/config.toml              build-std + target selection
build.rs                        injects libnds/specs/libgcc link args from $BLOCKSDS
Cargo.toml                      workspace root + the `bevy-ds` game binary
src/main.rs                     the game: pure Bevy components + systems (no FFI)
Justfile                        build / rom / run / preview tasks
crates/bevy_nds/                umbrella: re-exports + DsPlugins plugin group
crates/bevy_nds_runtime/        allocator, panic handler, critical-section, vblank run()
crates/bevy_nds_video/          DsScreen + Consoles + VideoPlugin (both LCDs)
crates/bevy_nds_input/          DsButton + ButtonInput<DsButton>, Touches (InputPlugin)
crates/bevy_nds_gesture/        tap/long-press/swipe/drag from the touch stream (GesturePlugin)
crates/bevy_nds_time/           real-time Time from the hardware timer (TimePlugin)
crates/bevy_nds_diagnostics/    smoothed Fps resource (DiagnosticsPlugin)
crates/bevy_nds_text/           Glyph/DsText/TilePos + diffed render system (TextRenderPlugin)
crates/bevy_nds_sprite/         OAM (hardware sprites) plugin + .sprite asset parser (SpritePlugin)
crates/png2sprite/              host CLI/lib: PNG -> .sprite NitroFS asset via grit (used by build.rs)
crates/bevy_nds_nitrofs/        NitroFsPlugin + read_file + flush_dcache (shared by 3d/audio)
crates/bevy_nds_3d/             hardware 3D backend (Transform3d, DsMesh, Camera3d)
  src/lib.rs                      meshes, culling, NitroFS loading, render system
  src/ffi.rs                      FFI to the geometry engine
crates/bevy_nds_3d_obj/         host OBJ -> display-list encoder (shared packing math)
crates/bevy_nds_3d_macros/      include_obj! proc-macro (bakes a model into the ROM)
crates/bevy_nds_3d_cull/        pure, host-testable view-frustum culling math
crates/bevy_nds_math/           20.12 fixed-point + hardware divide/sqrt wrappers (host-testable)
crates/bevy_nds_cothread/       libnds cooperative threads as Tasks/Task<T> (non-blocking IO)
crates/bevy_nds_audio/          maxmod audio backend (Music resource, PlaySfx events)
  src/lib.rs                      AudioPlugin, Music/PlaySfx API, soundbank loading
  src/ffi.rs                      FFI to the maxmod ARM9 API
  src/sfx.rs                      pure, host-testable volume/panning + load bookkeeping
crates/wav2bank/                host CLI/lib: WAV -> soundbank.bin via mmutil (used by build.rs)
crates/obj2dl/                  host CLI/lib: OBJ -> .dl NitroFS asset (used by build.rs)
assets/                         uncompiled source models (e.g. teapot.obj)
audio/                          uncompiled source sounds (music/*.wav, sfx/*.wav)
build/nitrofs/                  compiled .dl + soundbank.bin, packed into the ROM (gitignored)
```

### Audio pipeline

Sound on the DS is mixed on the **ARM7** core by [maxmod], driven from the ARM9
over the FIFO/IPC — so audio is the project's first second-core dependency: the
ROM must embed the maxmod ARM7 core (`just rom` selects `arm7_maxmod.elf`) and
link against `-lmm9`. Like models, sounds are baked host-side: `wav2bank` wraps
BlocksDS's `mmutil` to turn `audio/{music,sfx}/*.wav` into a single
`soundbank.bin`, emitting a Rust module of the generated `SFX_*` IDs that the
game `include!`s (so it never hard-codes indices). `build.rs` runs it into
`build/nitrofs/soundbank.bin`, which `just rom` packs into the ROM; maxmod loads
it from `nitro:/` at runtime. Music loops because `wav2bank` injects a
forward-loop `smpl` chunk into `music/*.wav` before baking (maxmod only loops a
PCM sample that carries one). Game code plays sound through ordinary Bevy: a
declarative `Music` resource (looping background track) and one-shot `PlaySfx`
events, with no FFI.

[maxmod]: https://maxmod.devkitpro.org/

### Asset pipeline

The DS has no asset server: a model is always *bytes at a ROM address*. A single
host-side encoder (`bevy_nds_3d_obj`) turns a Wavefront OBJ into a libnds
**display list** — the exact geometry-engine command block the hardware draws in
one `glCallList` DMA burst, with all fixed-point and normal packing done on the
host. That encoder feeds two delivery paths:

- **Baked into the binary.** `include_obj!("model.obj")` parses the OBJ at
  compile time and embeds a `&'static` display list in the ARM9 binary.
- **Loaded from NitroFS.** The demo's `build.rs` runs `obj2dl` over `assets/*.obj`
  into `build/nitrofs/*.dl`; `just rom` packs that directory into the ROM
  filesystem (`ndstool -d`), and `DsMesh::load("nitro:/model.dl")` reads it at
  runtime (with a cache flush before the DMA). This keeps large models out of
  precious main RAM and lets assets change without relinking.

Both paths produce byte-identical geometry. Build-time options (`center`,
`offset`, `compress` for `VTX_10` vertices) adjust the baked output at no runtime
cost. Meshes carry a local AABB, which the renderer uses for view-frustum culling
(`bevy_nds_3d_cull`), the DS analogue of Bevy's culling.

## Writing a game

A game is a `no_std` binary that adds `DsPlugins`, registers its systems, and
calls `bevy_nds::run`:

```rust
#![no_std]
#![no_main]

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins);
    app.add_systems(Startup, |mut commands: Commands| {
        commands.spawn((DsScreen::Top, TilePos::new(4, 2), DsText::new("Hello, DS!")));
    });
    bevy_nds::run(app)
}
```

`src/main.rs` is the full example: the hardware-lit teapot loaded from NitroFS,
D-pad movement across both screens (the `Display3d` swap), ABXY rotation, looping
maxmod music with a click SFX on teapot selection (`AudioPlugin`), and the HUD.

## Build details

- **Target.** `armv5te-nintendo-ds.json` describes the ARM946E-S core (no std,
  `panic = "abort"`, soft-float). It's Tier 3, so `core`/`alloc` are built from
  source with `-Z build-std` (set in `.cargo/config.toml`).
- **Linking.** `build.rs` reads `$BLOCKSDS` (set by the dev shell) and passes the
  ARM9 crt0/linker script via `-specs=…/ds_arm9.specs`, plus `-lnds9 -lc -lgcc`.
  `libgcc` is required because the BlocksDS specs alias `__sync_synchronize` to a
  helper that lives in it.
- **Atomics.** The DS has no atomic compare-and-swap, so `portable-atomic`
  (pulled in by Bevy) is backed by the `critical-section` impl in
  `crates/bevy_nds_runtime/src/lib.rs`, which disables interrupts around the section.
- **Packaging.** `ndstool` combines the ARM9 ELF with a stock BlocksDS ARM7 core
  (`arm7_minimal.elf`) and the `build/nitrofs` asset directory (`-d`) into the
  final `.nds`.
- **Performance.** The dev profile leaves the *game* crate unoptimized for fast
  rebuilds but compiles every dependency *and* our engine subcrates at
  `opt-level = 3` (they sit on the per-frame hot path), so the debug ROM runs at
  60 fps on the 33 MHz ARM9. Build `release` for the smallest, fastest ROM.

## Limitations / next steps

- The `bevy_nds_sprite` MVP exposes a single hard-coded 16x16 sprite shared
  by all spawned `Sprite` entities. A handle-based `SpriteSheet` API (and
  multi-sheet asset baking) is a natural follow-up.
- No Wi-Fi (dswifi). Add a `bevy_nds_wifi` crate alongside `bevy_nds_audio`
  (link `-ldswifi9`, embed the matching ARM7 core).
- Keep entity counts modest; the DS has ~4 MB of RAM.

## References

- BlocksDS SDK — https://github.com/blocksds/sdk
- blocksds-nix (Nix packaging) — https://github.com/pgattic/blocksds-nix
- nds-rs (libnds Rust bindings / target spec reference) — https://github.com/BlueTheDuck/nds-rs
- Bevy `no_std` docs — https://github.com/bevyengine/bevy/blob/main/docs/cargo_features.md
