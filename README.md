# bevy-ds

Running the [Bevy](https://bevyengine.org/) engine's ECS on the **Nintendo DS**,
packaged into a bootable `.nds` ROM and previewed on an emulator — all from a
reproducible Nix dev shell.

The bottom screen shows a `@` marker that you move with the D-pad. The marker is
an entity in a real Bevy `World`; its movement and rendering are ordinary Bevy
**systems** running every frame on the DS's ARM9 CPU.

```
        ┌────────────────────────────┐
        │                            │   top screen (unused)
        ├────────────────────────────┤
        │ Bevy ECS on Nintendo DS    │
        │                            │   bottom screen = libnds text console
        │             @              │   ← Bevy entity, moved by the D-pad
        │ D-pad: move the @          │
        └────────────────────────────┘
```

## How it works

Full Bevy (the `bevy` crate) depends on `wgpu`/`winit` and cannot run on the DS.
But since Bevy 0.16, the engine's **core is `no_std`-capable**, so we use just
those pieces and provide the platform layer ourselves:

| Layer                | What we use                                                        |
| -------------------- | ------------------------------------------------------------------ |
| ECS + App + schedule | `bevy_ecs` + `bevy_app` (`default-features = false`, `critical-section`) |
| Rendering / input    | [libnds](https://github.com/blocksds/libnds) via a small FFI shim (`src/libnds.rs`) |
| Allocator            | newlib heap (DS crt0) exposed as a Rust `#[global_allocator]`       |
| Atomics / sync       | `critical-section` impl that toggles the DS interrupt-enable register |
| Toolchain / SDK      | [BlocksDS](https://github.com/blocksds/sdk) + WonderfulToolchain (libnds, crt0, specs, `ndstool`) |

The game loop (`src/main.rs`) waits for vblank, reads the keys with libnds,
copies them into a Bevy `Resource`, then calls `app.update()` — which runs the
Bevy schedule (`movement` then `render`) just like on desktop.

This is the same overall approach as
[`bevy_mod_gba`](https://github.com/bushrat011899/bevy_mod_gba) takes for the
Game Boy Advance.

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

| Command              | Description                                                |
| -------------------- | ---------------------------------------------------------- |
| `just build`         | Compile the ARM9 ELF (debug).                              |
| `just build-release` | Compile the ARM9 ELF (release).                            |
| `just rom [profile]` | Package an ELF into `bevy-ds.nds` (`ndstool`).             |
| `just run [profile]` | Build, package, and run in **melonDS** (interactive).      |
| `just preview [profile]` | Build, package, boot in **desmume** headlessly and save `preview.png`. Override with `OUT=`, `WAIT=`, `DISP=`. |
| `just check`         | `cargo check`.                                             |
| `just fmt`           | `cargo fmt`.                                               |
| `just clean`         | Remove build artifacts and the ROM.                        |

## Project layout

```
flake.nix                     dev shell: Rust nightly + BlocksDS + emulators + preview tools
rust-toolchain.toml           pins nightly + rust-src (for build-std)
armv5te-nintendo-ds.json      custom Tier-3 target spec (ARM946E-S, no_std)
.cargo/config.toml            build-std + target selection
build.rs                      injects libnds/specs/libgcc link args from $BLOCKSDS
Cargo.toml                    bevy_ecs + bevy_app (no_std) + critical-section
src/main.rs                   allocator, panic handler, critical-section, the Bevy app
src/libnds.rs                 hand-written FFI to the libnds functions we use
Justfile                      build / rom / run / preview tasks
```

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
  `src/main.rs`, which disables interrupts for the duration of the section.
- **Packaging.** `ndstool` combines our ARM9 ELF with a stock BlocksDS ARM7 core
  (`arm7_minimal.elf`) into the final `.nds`.

## Limitations / next steps

- The demo renders to the libnds **text console**; sprite/tile graphics would use
  libnds OAM/backgrounds (and `grit` for asset conversion, already in the shell).
- No audio (maxmod) or Wi-Fi (dswifi) — swap in the matching ARM7 core and link
  `-lmm9` / `-ldswifi9` to enable them.
- Keep entity counts small: the DS has ~4 MB of RAM.
- `bevy_time`, `bevy_math`, etc. are also `no_std`-capable and can be added the
  same way (`default-features = false`).

## References

- BlocksDS SDK — https://github.com/blocksds/sdk
- blocksds-nix (Nix packaging) — https://github.com/pgattic/blocksds-nix
- nds-rs (libnds Rust bindings / target spec reference) — https://github.com/BlueTheDuck/nds-rs
- Bevy `no_std` docs — https://github.com/bevyengine/bevy/blob/main/docs/cargo_features.md
