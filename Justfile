# Justfile — build & run tasks for the Bevy-on-Nintendo-DS project.
# Run `just` (or `just --list`) inside `nix develop`.

set shell := ["bash", "-uc"]

# Path to the compiled ARM9 ELF for a given cargo profile.
target_dir := "target/armv5te-nintendo-ds"
rom := "bevy-ds.nds"

# Default: build a debug ROM and launch it in melonDS.
default: run

# Compile the ARM9 ELF (debug build).
build:
    cargo build

# Compile the ARM9 ELF (release build — smaller and faster).
build-release:
    cargo build --release

# Type-check without producing a ROM.
check:
    cargo check

# Run the host-side unit tests.
#
# `bevy_nds` normally builds for the DS (a no_std target with no test harness),
# so its tests can't run there. Instead we build for the host triple and let the
# standard test harness run the pure-logic tests. `bevy_nds` is `no_std` only
# when not under `cfg(test)`, so this links against the host `std`.
#
# We override the project's `build-std`/panic config just for that crate:
# building full `std` from source keeps a single `core` (avoiding a duplicate
# lang-item clash) and `panic = "unwind"` matches the test harness. The first
# run compiles `std`, so it is slow; later runs are fast.
#
# `bevy_nds_3d_macros` is an ordinary host proc-macro crate, and the
# `bevy_nds_3d_obj` (shared OBJ encoder) and `obj2dl` (asset baker) crates are
# ordinary host libraries, so their tests run natively with no special flags.
#
# Usage: `just test` (all) or `just test <filter>` (e.g. `just test render`).
#
# Crates split into two groups by dependency shape:
# - `bevy_nds_3d_obj` / `obj2dl` / `bevy_nds_3d_macros` have no external deps, so
#   they build cleanly against the prebuilt host std (plain `--target host`).
# - The platform subcrates and `bevy_nds_3d_cull` pull in crates compiled
#   against `core` (Bevy; `libm`), so the host test needs `std` built from
#   source to keep a single `core` (avoiding a duplicate-lang-item clash) and
#   `panic = "unwind"` to match the test harness. `wav2bank` has no external
#   deps, but a *clean* host build still trips the duplicate-`core` clash under
#   the project's global `build-std`, so it rides in this group too (building
#   `std` from source fixes it).
test *args:
    host="$(rustc -vV | sed -n 's/^host: //p')"; \
    cargo test -p bevy_nds_3d_obj -p obj2dl -p bevy_nds_3d_macros -p png2sprite \
        --target "$host" {{args}}
    cargo test \
        -p bevy_nds_diagnostics \
        -p bevy_nds_time \
        -p bevy_nds_input \
        -p bevy_nds_gesture \
        -p bevy_nds_text \
        -p bevy_nds_sprite \
        -p bevy_nds_3d_cull \
        -p wav2bank \
        -p bevy_nds_audio \
        -p bevy_nds_math \
        -p bevy_nds_cothread \
        --target "$(rustc -vV | sed -n 's/^host: //p')" \
        --config 'unstable.build-std=["std","panic_unwind","proc_macro"]' \
        --config 'profile.dev.panic="unwind"' \
        {{args}}

# Format the Rust sources.
fmt:
    cargo fmt

# Package a compiled ELF into a bootable .nds ROM.
#
# `build/nitrofs/` (populated by build.rs from `assets/*.obj`) is packed into the
# ROM filesystem with `-d`, so models load at runtime from `nitro:/`. The
# directory is created on first build; tolerate it being empty.
# Usage: `just rom` (debug) or `just rom release`.
rom profile="debug": (_build profile)
    #!/usr/bin/env bash
    set -euo pipefail
    : "${BLOCKSDS:?Run 'nix develop' first so BLOCKSDS is set}"
    elf="{{target_dir}}/{{profile}}/bevy-ds.elf"
    ndstool="$BLOCKSDS/tools/ndstool/ndstool"
    # Audio is mixed on the ARM7 by maxmod, so the ROM must embed the maxmod ARM7
    # core (the `minimal` core has no sound). Prefer it; fall back to minimal only
    # if this BlocksDS install somehow lacks it (audio would then be silent).
    arm7="$BLOCKSDS/sys/arm7/main_core/arm7_maxmod.elf"
    [ -f "$arm7" ] || arm7="$BLOCKSDS/sys/arm7/main_core/arm7_minimal.elf"
    nitrofs_args=()
    [ -d build/nitrofs ] && nitrofs_args=(-d build/nitrofs)
    "$ndstool" -c "{{rom}}" -7 "$arm7" -9 "$elf" \
        "${nitrofs_args[@]}" \
        -h 0x200 -g BEVY ME "Bevy DS"
    echo "Wrote {{rom}} from $elf"

# Build a ROM (debug by default) and run it in the melonDS emulator.
# Usage: `just run` or `just run release`.
run profile="debug": (rom profile)
    melonDS "{{rom}}"

# Headlessly boot the ROM in desmume and save a screenshot — handy for quickly
# seeing what the ROM renders without a GUI (also usable in CI).
# Usage: `just preview` or `just preview release`. Output: preview.png
# Override with OUT=foo.png, WAIT=12 (seconds), DISP=:99.
preview profile="debug": (rom profile)
    #!/usr/bin/env bash
    set -euo pipefail
    out="${OUT:-preview.png}"
    wait_s="${WAIT:-10}"
    disp="${DISP:-:99}"
    echo "Booting {{rom}} in desmume (headless) on $disp …"
    timeout $((wait_s + 20)) Xvfb "$disp" -screen 0 256x384x24 >/tmp/bevy-ds-xvfb.log 2>&1 &
    sleep 2
    # `--disable-sound` keeps the preview silent: the preview is meant for
    # eyes-only (CI, quick screenshots), and an emulator that suddenly bursts
    # into music is startling when you forget you launched it. Also dodges any
    # SDL audio device the headless Xvfb session doesn't have.
    DISPLAY="$disp" SDL_VIDEODRIVER=x11 SDL_AUDIODRIVER=dummy \
        timeout $((wait_s + 6)) desmume-cli --nojoy --disable-sound "{{rom}}" >/tmp/bevy-ds-emu.log 2>&1 &
    sleep "$wait_s"
    DISPLAY="$disp" import -window root "$out"
    echo "Saved $out (emulator log: /tmp/bevy-ds-emu.log)"
    wait 2>/dev/null || true

# Headlessly boot the ROM in desmume and grab the first stable frame — the
# fast variant of `just preview`, tuned for README banner / changelog snaps
# rather than "let the demo run for a while". Default WAIT is short (~2s,
# enough for desmume to bring up the X window and for the ROM to draw a
# couple of frames). Like `preview`, override with OUT=, WAIT=, DISP=.
# WAIT accepts fractional seconds (e.g. WAIT=1.5).
snap profile="debug": (rom profile)
    #!/usr/bin/env bash
    set -euo pipefail
    out="${OUT:-preview.png}"
    wait_s="${WAIT:-2}"
    disp="${DISP:-:99}"
    # bash arithmetic is integer-only; round wait_s up for the timeout budgets.
    wait_int=$(awk -v w="$wait_s" 'BEGIN { printf "%d", (w == int(w) ? w : int(w) + 1) }')
    echo "Booting {{rom}} in desmume (headless) on $disp, sleeping ${wait_s}s …"
    timeout $((wait_int + 20)) Xvfb "$disp" -screen 0 256x384x24 >/tmp/bevy-ds-xvfb.log 2>&1 &
    sleep 2
    DISPLAY="$disp" SDL_VIDEODRIVER=x11 SDL_AUDIODRIVER=dummy \
        timeout $((wait_int + 6)) desmume-cli --nojoy --disable-sound "{{rom}}" >/tmp/bevy-ds-emu.log 2>&1 &
    sleep "$wait_s"
    DISPLAY="$disp" import -window root "$out"
    echo "Saved $out (emulator log: /tmp/bevy-ds-emu.log)"
    wait 2>/dev/null || true

# Remove build artifacts and the generated ROM.
clean:
    cargo clean
    rm -f "{{rom}}"

# Internal: compile for the requested profile.
_build profile="debug":
    cargo build {{ if profile == "release" { "--release" } else { "" } }}
