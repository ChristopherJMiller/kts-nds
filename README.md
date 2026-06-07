# bevy-ds

Running the [Bevy](https://bevyengine.org/) engine's ECS on the **Nintendo DS**,
packaged into a bootable `.nds` ROM and previewed on an emulator — all from a
reproducible Nix dev shell.

The project is split into two crates:

- **`bevy_nds`** (`crates/bevy_nds`) — a reusable library that wires Bevy's
  `no_std` ECS/App core to the DS hardware (via [libnds](https://github.com/blocksds/libnds))
  and exposes it as idiomatic Bevy **plugins**, **components** and **resources**.
- **`bevy-ds`** (the root crate) — the game, a *pure-Bevy consumer* of
  `bevy_nds`. It contains no FFI, no allocator and no panic handler: just
  components and systems.

```
        ┌────────────────────────────┐
        │ Bevy ECS on Nintendo DS    │   top screen  (main 2D engine)
        │ t=  12s   held=0           │   ← live HUD from the Time/input resources
        ├────────────────────────────┤
        │                            │
        │             @              │   bottom screen (sub 2D engine)
        │                            │   ← Bevy entity, moved by the D-pad
        │ D-pad: move the @          │
        └────────────────────────────┘
```

## How it works

Full Bevy (the `bevy` crate) depends on `wgpu`/`winit` and cannot run on the DS.
But since Bevy 0.16 the engine's **core is `no_std`-capable**, so `bevy_nds`
uses just those pieces and provides the platform layer itself. The key idea is
to map DS hardware onto ordinary Bevy concepts so that game code never has to
think about the hardware:

| DS hardware              | `bevy_nds` exposes                                                    | Plugin              |
| ------------------------ | -------------------------------------------------------------------- | ------------------- |
| Top / bottom LCDs        | `DsScreen::{Top,Bottom}` component + `Consoles` resource             | `VideoPlugin`       |
| Buttons (`REG_KEYINPUT`) | the standard `ButtonInput<DsButton>` resource                        | `InputPlugin`       |
| Vertical-blank @ ~60 Hz  | a `set_runner` frame loop + a real `Time` resource (hardware timer)  | `TimePlugin`        |
| —                        | a smoothed `Fps` resource for diagnostics                            | `DiagnosticsPlugin` |
| Tiled text background    | `Glyph` / `DsText` + `TilePos`, drawn by an extraction system        | `RenderPlugin`      |

`DsPlugins` bundles them all; `bevy_nds::run(app)` installs the runner and owns
the frame loop (`swiWaitForVBlank` → `app.update()`).

### Rendering model

Desktop Bevy extracts entities to the GPU each frame; `bevy_nds` keeps the same
*shape* but the "GPU" is the DS text console (a tiled background) and the draw
call is libnds `printf`. A drawable is any entity with a `TilePos` + `DsScreen`
and either a `Glyph` (a single character — the DS analogue of a text sprite) or
a `DsText` (a run of text).

To avoid flicker, the renderer is **double-buffered at the grid level**: each
screen keeps a statically-sized `front` buffer (mirroring the live tilemap) and
a `back` buffer (composed fresh each frame). The render system stamps every
drawable into `back`, then writes *only the cells that differ* to the hardware
tilemap and copies them into `front`. The display is never blanked — so there is
no flicker — and a typical frame only touches a handful of tiles. This avoids
both the visible blank of a full `consoleClear()` and any per-frame heap churn.

`bevy_text` (cosmic-text font rasterisation) is far too heavy for the DS, so we
shed it and rebuild a lightweight, `no_std` text concept on the hardware tile
engine instead.

This is the same overall approach as
[`bevy_mod_gba`](https://github.com/bushrat011899/bevy_mod_gba) takes for the
Game Boy Advance.

### Bare-metal runtime

`bevy_nds` also provides the pieces a bare-metal Rust program needs, so the game
doesn't have to (`crates/bevy_nds/src/runtime.rs`):

- a `#[global_allocator]` backed by newlib's heap (set up by the BlocksDS crt0),
- a `#[panic_handler]`, and
- a `critical-section` implementation that toggles the DS interrupt-enable
  register — this is what Bevy's atomics (`portable-atomic`) build upon.

## Prerequisites

- [Nix](https://nixos.org/) with flakes enabled.
- That's it — the dev shell provides the Rust nightly toolchain, the BlocksDS
  SDK, `ndstool`, the melonDS and desmume emulators, and the preview tooling.

The BlocksDS SDK is provided as a proper Nix derivation (no `buildFHSEnv`) via
[`pgattic/blocksds-nix`](https://github.com/pgattic/blocksds-nix), which patches
the official BlocksDS toolchain into the Nix store and exports `$BLOCKSDS` /
`$WONDERFUL_TOOLCHAIN`.

## Quick start

```sh
nix develop          # enter the dev shell (first run builds/fetches the toolchain)

just build           # compile the ARM9 ELF (debug)
just rom             # package it into bevy-ds.nds with ndstool
just run             # build + package + launch melonDS
just preview         # build + package + headless desmume screenshot -> preview.png
```

Release build (smaller, faster): append `release`, e.g. `just run release`.

### Tasks

| Command                  | Description                                                |
| ------------------------ | ---------------------------------------------------------- |
| `just build`             | Compile the ARM9 ELF (debug).                              |
| `just build-release`     | Compile the ARM9 ELF (release).                            |
| `just rom [profile]`     | Package an ELF into `bevy-ds.nds` (`ndstool`).             |
| `just run [profile]`     | Build, package, and run in **melonDS** (interactive).      |
| `just preview [profile]` | Build, package, boot in **desmume** headlessly and save `preview.png`. Override with `OUT=`, `WAIT=`, `DISP=`. |
| `just check`             | `cargo check`.                                             |
| `just fmt`               | `cargo fmt`.                                               |
| `just clean`             | Remove build artifacts and the ROM.                        |

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
crates/bevy_nds/                the reusable Bevy <-> Nintendo DS library
  src/lib.rs                      crate root, plugin/component re-exports, run()
  src/ffi.rs                      hand-written FFI to the libnds functions we use
  src/runtime.rs                  allocator, panic handler, critical-section impl
  src/screen.rs                   DsScreen, Consoles, VideoPlugin (both screens)
  src/input.rs                    DsButton + ButtonInput<DsButton> (InputPlugin)
  src/time.rs                     real-time Time from the hardware timer (TimePlugin)
  src/diagnostics.rs              smoothed Fps resource (DiagnosticsPlugin)
  src/render.rs                   Glyph/DsText/TilePos + diffed render system (RenderPlugin)
  src/runner.rs                   the vblank App runner + DsPlugins group
```

## Writing a game

A game is just a `no_std` binary that adds `DsPlugins`, registers its own
systems, and calls `bevy_nds::run`:

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

See `src/main.rs` for the full example (two screens, D-pad movement and a HUD).

## Build details

- **Target.** `armv5te-nintendo-ds.json` describes the ARM946E-S core (no std,
  `panic = "abort"`, soft-float). Because it is Tier 3 we build `core`/`alloc`
  from source with `-Z build-std` (configured in `.cargo/config.toml`).
- **Linking.** `build.rs` reads `$BLOCKSDS` (set by the dev shell) and passes the
  ARM9 crt0/linker-script via `-specs=…/ds_arm9.specs`, plus
  `-lnds9 -lc -lgcc`. `libgcc` is required because the BlocksDS specs alias
  `__sync_synchronize` to a helper that lives there.
- **Atomics.** The DS has no atomic compare-and-swap, so `portable-atomic`
  (pulled in by Bevy) is backed by the `critical-section` implementation in
  `crates/bevy_nds/src/runtime.rs`, which disables interrupts for the duration
  of the section.
- **Packaging.** `ndstool` combines our ARM9 ELF with a stock BlocksDS ARM7 core
  (`arm7_minimal.elf`) into the final `.nds`.
- **Performance.** Per Bevy's guidance, the dev profile leaves our own crates
  unoptimized (fast rebuilds) but optimizes all *dependencies*
  (`[profile.dev.package."*"] opt-level = 3`), so even the debug ROM runs the
  ECS at a locked 60 fps on the 33 MHz ARM9. For the smallest/fastest ROM, build
  `release` (`just run release`).

## Limitations / next steps

- Rendering uses the libnds **text console**. True sprite/tile graphics would
  use libnds OAM/backgrounds (and `grit` for asset conversion, already in the
  shell) behind the same `RenderPlugin` extraction model.
- No audio (maxmod) or Wi-Fi (dswifi) — swap in the matching ARM7 core and link
  `-lmm9` / `-ldswifi9` to enable them.
- Keep entity counts small: the DS has ~4 MB of RAM.

## References

- BlocksDS SDK — https://github.com/blocksds/sdk
- blocksds-nix (Nix packaging) — https://github.com/pgattic/blocksds-nix
- nds-rs (libnds Rust bindings / target spec reference) — https://github.com/BlueTheDuck/nds-rs
- Bevy `no_std` docs — https://github.com/bevyengine/bevy/blob/main/docs/cargo_features.md
