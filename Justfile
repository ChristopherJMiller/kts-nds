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
    arm7="$BLOCKSDS/sys/arm7/main_core/arm7.elf"
    [ -f "$arm7" ] || arm7="$BLOCKSDS/sys/arm7/main_core/arm7_maxmod.elf"
    "$ndstool" -c "{{rom}}" -7 "$arm7" -9 "$elf" \
        -t "Bevy DS" -h 0x200
    echo "Wrote {{rom}} from $elf"

# Build a ROM (debug by default) and run it in the melonDS emulator.
# Usage: `just run` or `just run release`.
run profile="debug": (rom profile)
    melonDS "{{rom}}"

# Remove build artifacts and the generated ROM.
clean:
    cargo clean
    rm -f "{{rom}}"

# Internal: compile for the requested profile.
_build profile="debug":
    cargo build {{ if profile == "release" { "--release" } else { "" } }}
