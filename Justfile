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

# Format the Rust sources.
fmt:
    cargo fmt

# Package a compiled ELF into a bootable .nds ROM.
# Usage: `just rom` (debug) or `just rom release`.
rom profile="debug": (_build profile)
    #!/usr/bin/env bash
    set -euo pipefail
    : "${BLOCKSDS:?Run 'nix develop' first so BLOCKSDS is set}"
    elf="{{target_dir}}/{{profile}}/bevy-ds.elf"
    ndstool="$BLOCKSDS/tools/ndstool/ndstool"
    arm7="$BLOCKSDS/sys/arm7/main_core/arm7_minimal.elf"
    [ -f "$arm7" ] || arm7="$BLOCKSDS/sys/arm7/main_core/arm7_maxmod.elf"
    "$ndstool" -c "{{rom}}" -7 "$arm7" -9 "$elf" \
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
    DISPLAY="$disp" SDL_VIDEODRIVER=x11 \
        timeout $((wait_s + 6)) desmume-cli --nojoy "{{rom}}" >/tmp/bevy-ds-emu.log 2>&1 &
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
