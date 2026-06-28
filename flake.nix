{
  description = "Bevy on the Nintendo DS — no_std Bevy ECS driving a libnds homebrew ROM";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Derivation-driven BlocksDS SDK + WonderfulToolchain (libnds, crt0/specs,
    # default ARM7 cores, ndstool, grit, …). Pulls the official BlocksDS image
    # and patchelfs it into the Nix store — no buildFHSEnv required.
    blocksds-nix = {
      url = "github:pgattic/blocksds-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, flake-utils, rust-overlay, blocksds-nix, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import rust-overlay)
            blocksds-nix.overlays.default
          ];
        };

        # The "dev" image bundles the arm-none-eabi GCC toolchain that libnds
        # was built against, plus ndstool/grit/etc. under $BLOCKSDS.
        blocksds = pkgs.blocksdsNix.blocksdsDev;
        bd = blocksds.passthru;

        # Nightly Rust with rust-src so we can build-std for the custom
        # `armv5te-nintendo-ds` target (Tier 3, no std).
        rustToolchain = pkgs.rust-bin.selectLatestNightlyWith (
          toolchain:
          toolchain.default.override {
            extensions = [ "rust-src" "rustfmt" "clippy" ];
          }
        );

        # Upstream nixpkgs builds desmume without `-Dgdb-stub=true`, which means
        # the `--arm9gdb=PORT` / `--arm7gdb=PORT` flags are stripped out and the
        # emulator has no way to expose ARM9 main RAM to a debugger. We want
        # them for perf telemetry: the ROM writes a `PerfBlob` (frame-time ring)
        # into main RAM, and a small host tool connects to the gdbstub during
        # `just preview` to read it back. Override only the meson flags.
        desmumeWithGdbStub = pkgs.desmume.overrideAttrs (old: {
          mesonFlags = (old.mesonFlags or [ ]) ++ [ "-Dgdb-stub=true" ];
        });

        # Runtime libraries the desktop scene-editor (eframe/winit/glow) loads
        # via dlopen at runtime — they don't show up in `ldd`, so winit fails
        # with `NoWaylandLib` / no X11 connection unless they're on
        # LD_LIBRARY_PATH. The DS build/rom/run tasks don't need these; only
        # `just edit` (and `just check-editor` run) does.
        editorRuntimeLibs = [
          pkgs.wayland
          pkgs.libxkbcommon
          pkgs.libGL
          pkgs.libx11
          pkgs.libxcursor
          pkgs.libxi
          pkgs.libxrandr
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            blocksds
            pkgs.just
            # Primary emulator for interactive use.
            pkgs.melonds
            # Headless preview: desmume (SDL frontend) + Xvfb + ImageMagick let
            # `just preview` boot the ROM and capture a screenshot with no GUI.
            # Built with `gdb-stub=true` so `--arm9gdb=PORT` is available — see
            # `desmumeWithGdbStub` above.
            desmumeWithGdbStub
            pkgs.xvfb
            pkgs.imagemagick
            # bindgen / general build helpers
            pkgs.pkg-config
          ];

          # Consumed by build.rs to locate libnds, the specs file and ndstool.
          WONDERFUL_TOOLCHAIN = bd.WONDERFUL_TOOLCHAIN;
          BLOCKSDS = bd.BLOCKSDS;
          BLOCKSDSEXT = bd.BLOCKSDSEXT;

          shellHook = ''
            # Put the BlocksDS-bundled arm-none-eabi toolchain (the exact one
            # libnds was compiled with) on PATH for linking the ROM.
            export PATH="$WONDERFUL_TOOLCHAIN/toolchain/gcc-arm-none-eabi/bin:$PATH"
            # The desktop scene-editor (eframe/winit) dlopens Wayland/X11/GL at
            # runtime; surface them here plus the system GL driver (mesa) so
            # `just edit` opens a window instead of failing with NoWaylandLib.
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath editorRuntimeLibs}:/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            echo "kts-nds dev shell"
            echo "  BLOCKSDS = $BLOCKSDS"
            echo "  rustc    = $(rustc --version 2>/dev/null)"
            echo "  just <tab> for tasks (build, rom, run, …)"
          '';
        };
      }
    );
}
