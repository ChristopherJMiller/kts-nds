---
name: add-capability
description: Scaffold a new bevy_nds_<capability> crate following the "one capability, one crate" pattern. Use when adding a new DS hardware subsystem (save data, Wi-Fi, RTC, camera, …) — never expand an existing crate's FFI to host unrelated work. Wires up Cargo.toml, lib.rs skeleton, workspace members, dev-profile opt-level, optional DsPlugins re-export, and the right cfg gates.
---

# add-capability

CLAUDE.md mandates: when reaching for a new DS subsystem, **add a new crate,
don't expand an existing one**. This skill enforces that mechanically.

## Decide first

Ask the user (or pick yourself, in auto mode):

1. **Capability name** — `save`, `wifi`, `rtc`, `camera`, … (slug becomes
   `bevy_nds_<slug>`).
2. **Has FFI?** Yes if it touches libnds; no if it's pure logic over data
   another crate already exposes (cf. `bevy_nds_gesture`, which is FFI-free
   bookkeeping over `Touches`).
3. **NitroFS-dependent?** Yes if it loads bytes from the ROM filesystem.
   Then `PreStartup` work must be ordered `.after(bevy_nds_nitrofs::init_nitrofs)`.
4. **Per-frame hot path?** Yes for anything called every `Update` tick.
   Determines whether it needs an `opt-level = 3` override.
5. **Belongs in `DsPlugins`?** Yes if universally useful (most platform
   subcrates). No if it's opt-in (e.g. `bevy_nds_3d` is depended on directly,
   not bundled).

## Steps

### 1. Create the crate directory

    mkdir -p crates/bevy_nds_<slug>/src

### 2. `crates/bevy_nds_<slug>/Cargo.toml`

Model on the closest existing crate:

- FFI-free pure logic → copy `bevy_nds_gesture/Cargo.toml`.
- Bevy + libnds FFI → copy `bevy_nds_input/Cargo.toml`.
- NitroFS-backed asset loader → copy `bevy_nds_sprite/Cargo.toml`.

Set `name`, `description`, and trim deps you don't need. Always:

    [package]
    name = "bevy_nds_<slug>"
    version = "0.1.0"
    edition = "2024"
    description = "…"

    [lib]

Bevy deps **must** keep `default-features = false, features = ["critical-section"]` —
the `critical-section` impl in `bevy_nds_runtime` is what makes
`portable-atomic` work on the ARM946E-S (no hardware CAS). Removing it
breaks the build.

### 3. `src/lib.rs`

Boilerplate header:

    #![cfg_attr(not(test), no_std)]

    extern crate alloc; // only if you allocate

If you have FFI, either inline a small `extern "C"` block in `lib.rs` or
move it to `src/ffi.rs` (mirror `bevy_nds_3d/src/ffi.rs` / `bevy_nds_audio`
for heavier surfaces). **Cite the libnds header** in a comment above each
declaration — `// <nds/arm9/save.h>` etc. Use the `blocksds-docs` skill to
find the right signature.

Define the plugin:

    use bevy_app::prelude::*;
    use bevy_ecs::prelude::*;

    pub struct <Capability>Plugin;

    impl Plugin for <Capability>Plugin {
        fn build(&self, app: &mut App) {
            app.add_systems(PreStartup, init_<slug>); // hardware init
            // app.add_systems(Update, …);
        }
    }

NitroFS-backed init goes:

    app.add_systems(
        PreStartup,
        init_<slug>.after(bevy_nds_nitrofs::init_nitrofs),
    );

Gate any bare-metal items (allocator-touching globals, raw pointer statics)
on `#[cfg(target_vendor = "nintendo")]` so host tests don't pull them in.
Pure-logic helpers stay un-gated and get unit tests.

### 4. Workspace registration — root `Cargo.toml`

Add to `[workspace] members`:

    "crates/bevy_nds_<slug>",

If on the per-frame hot path, add a `[profile.dev.package.bevy_nds_<slug>]`
block with `opt-level = 3`. CLAUDE.md is explicit: the engine subcrates
*must* be optimized in dev or the 33 MHz ARM9 misses 60 fps.

### 5. Host testability

Bare-metal items (panic handler, allocator, raw FFI calls) must be inert
during host tests. Pattern:

- Pure logic → split out (e.g. `Grid::diff_runs` vs `Grid::flush`).
- FFI → `#[cfg(target_vendor = "nintendo")]` on the function, leaving a
  host-side stub or an `unimplemented!()` that tests never reach.
- **Never call FFI from a `#[test]`.**

Add the new crate to the second test group in `Justfile` (the
`-p bevy_nds_diagnostics …` line) unless it's deliberately FFI-only with
no testable surface — then skip and document why in the crate's lib.rs.

### 6. Umbrella wiring (optional)

If the capability is universally useful, in `crates/bevy_nds/Cargo.toml`:

    bevy_nds_<slug> = { path = "../bevy_nds_<slug>" }

And in `crates/bevy_nds/src/lib.rs`:

- `pub use bevy_nds_<slug>::{<Plugin>, <key types>};`
- Add `.add(<Plugin>)` inside the `DsPlugins` plugin group builder.
- Add the public items to `prelude`.
- Update the markdown table at the top of `lib.rs`.

Also append a row to the "DS hardware → ECS mapping" table in `CLAUDE.md`.

If the capability is opt-in, skip the umbrella; the game depends on
`bevy_nds_<slug>` directly via root `Cargo.toml`.

### 7. Sanity check

    nix develop --command just check
    nix develop --command just test -- bevy_nds_<slug>

## Don't

- Don't add to an existing crate's `ffi.rs` "for now."
- Don't share FFI across crates — duplicate `extern "C"` blocks are fine
  as long as signatures match.
- Don't forget the `critical-section` feature on Bevy deps.
- Don't leave the new crate at default opt-level if it's per-frame.
