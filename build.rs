//! Build script with two jobs:
//!
//! 1. **Compile model assets.** Bakes every `assets/*.obj` into a display-list
//!    blob under `build/nitrofs/` (via the `obj2dl` library), which `just rom`
//!    packs into the ROM filesystem (NitroFS) for runtime loading.
//! 2. **Inject DS link arguments** using the BlocksDS install located via the
//!    `$BLOCKSDS` environment variable (set by the Nix dev shell). Keeping these
//!    out of the target JSON means the spec file path stays correct on Nix,
//!    where BlocksDS lives in the store rather than at `/opt/wonderful`.

use std::env;
use std::path::PathBuf;

/// Source directory of uncompiled model assets (`*.obj`).
const ASSET_DIR: &str = "assets";
/// Output directory for compiled NitroFS assets (gitignored; packed by `just rom`).
const NITROFS_DIR: &str = "build/nitrofs";

fn main() {
    compile_assets();
    emit_link_args();
}

/// Bake every `assets/*.obj` into `build/nitrofs/*.dl` using the `obj2dl`
/// library. This is the runtime asset path: the blobs are placed in NitroFS by
/// `just rom` (`ndstool -d build/nitrofs`) and loaded at runtime. Geometry is
/// centred to match how the models are authored to spin about their middle.
fn compile_assets() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join(ASSET_DIR);
    let dst = manifest.join(NITROFS_DIR);

    println!("cargo:rerun-if-changed={}", src.display());

    if !src.is_dir() {
        return;
    }

    let opts = obj2dl::Options {
        center: true,
        ..Default::default()
    };
    match obj2dl::build_dir(&src, &dst, &opts) {
        Ok(built) => {
            for b in &built {
                println!("cargo:rerun-if-changed={}", b.input.display());
            }
        }
        Err(e) => println!("cargo:warning=asset compilation failed: {e}"),
    }
}

/// Injects Nintendo DS link arguments using the BlocksDS install located via
/// the `$BLOCKSDS` environment variable (set by the Nix dev shell). Keeping
/// these out of the target JSON means the spec file path stays correct on Nix,
/// where BlocksDS lives in the store rather than at `/opt/wonderful`.
fn emit_link_args() {
    let target = env::var("TARGET").unwrap_or_default();

    // Only emit DS-specific link flags for the bare-metal ARM9 target.
    if !target.contains("nintendo-ds") {
        return;
    }

    let blocksds = match env::var("BLOCKSDS") {
        Ok(v) => v,
        Err(_) => {
            println!(
                "cargo:warning=BLOCKSDS is not set. Run `nix develop` first so the \
                 DS toolchain and libnds can be found."
            );
            return;
        }
    };

    // Select the correct multilib (crt0/libc/libgcc) for the ARM946E-S.
    println!("cargo:rustc-link-arg=-mthumb");
    println!("cargo:rustc-link-arg=-mcpu=arm946e-s+nofp");

    // BlocksDS ARM9 crt0 + linker script.
    println!("cargo:rustc-link-arg=-specs={blocksds}/sys/crts/ds_arm9.specs");

    // libnds (ARM9 build), newlib C library and libgcc (provides the atomic
    // barrier helpers the BlocksDS specs alias). Grouped to resolve circular
    // references between them.
    println!("cargo:rustc-link-search=native={blocksds}/libs/libnds/lib");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg=-lnds9");
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-lgcc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");

    println!("cargo:rerun-if-env-changed=BLOCKSDS");
}
