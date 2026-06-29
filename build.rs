//! Build script with three jobs:
//!
//! 1. **Compile model assets.** Bakes every `assets/*.obj` into a display-list
//!    blob under `build/nitrofs/` (via the `obj2dl` library), which `just rom`
//!    packs into the ROM filesystem (NitroFS) for runtime loading.
//! 2. **Compile audio assets.** Bakes `audio/{music,sfx}/*.wav` into
//!    `build/nitrofs/soundbank.bin` (via the `wav2bank` library, which wraps
//!    `mmutil`) and emits a Rust module of the sound IDs into `OUT_DIR` for the
//!    game to `include!`.
//! 3. **Inject DS link arguments** using the BlocksDS install located via the
//!    `$BLOCKSDS` environment variable (set by the Nix dev shell). Keeping these
//!    out of the target JSON means the spec file path stays correct on Nix,
//!    where BlocksDS lives in the store rather than at `/opt/wonderful`.

use std::env;
use std::path::PathBuf;

/// Source directory of uncompiled model assets (`*.obj`).
const ASSET_DIR: &str = "assets";
/// Source directory of uncompiled audio assets (`music/*.wav`, `sfx/*.wav`).
const AUDIO_DIR: &str = "audio";
/// Source directory of uncompiled sprite PNGs.
const SPRITE_DIR: &str = "assets/sprites";
/// Source directory of uncompiled background PNGs (`tiled/` and `bitmap/`).
const BG_DIR: &str = "assets/backgrounds";
/// Source root of authored level directories (`<name>/level.ron` + `<zone>.ron`).
const LEVELS_DIR: &str = "assets/levels";
/// Source directory of the shared prefab library (`*.ron`).
const PREFAB_DIR: &str = "assets/prefabs";
/// Output directory for compiled NitroFS assets (gitignored; packed by `just rom`).
const NITROFS_DIR: &str = "build/nitrofs";

fn main() {
    compile_assets();
    compile_audio();
    compile_sprites();
    compile_backgrounds();
    compile_levels();
    emit_link_args();
}

/// Bake every `assets/levels/<name>/` into `build/nitrofs/levels/<name>/*.scene`
/// via the `scene2bin` library (issue #27), and always emit `$OUT_DIR/levels.rs`
/// — the (per-level) constants module of NitroFS paths the game `include!`s.
/// Unlike the sprite / audio bakers, `scene2bin` is pure Rust (no external
/// tool), so it runs identically inside or outside `nix develop`; only RON /
/// validation errors fall back to *predicted* constants so the game still
/// compiles.
fn compile_levels() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join(LEVELS_DIR);
    let dst = manifest.join(NITROFS_DIR).join(scene2bin::NITROFS_SUBDIR);
    let assets = manifest.join(ASSET_DIR);
    let prefabs = manifest.join(PREFAB_DIR);
    let out_rs = PathBuf::from(env::var("OUT_DIR").unwrap()).join("levels.rs");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-changed={}", prefabs.display());
    // Mesh validation reads the geometry source dir, so a new `.obj` can flip a
    // zone from invalid to valid.
    println!("cargo:rerun-if-changed={}", assets.display());

    if !src.is_dir() {
        std::fs::write(&out_rs, scene2bin::predict_consts(&src)).ok();
        return;
    }

    match scene2bin::build_levels_dir(&src, &dst, &assets, &prefabs) {
        Ok(built) => {
            for b in &built {
                println!("cargo:rerun-if-changed={}", b.input.display());
                for w in &b.warnings {
                    println!("cargo:warning={}: {w}", b.input.display());
                }
            }
            if let Err(e) = std::fs::write(&out_rs, scene2bin::emit_rust_consts(&built)) {
                println!("cargo:warning=could not write {}: {e}", out_rs.display());
            }
        }
        Err(e) => {
            println!("cargo:warning=level baking failed: {e}");
            std::fs::write(&out_rs, scene2bin::predict_consts(&src)).ok();
        }
    }
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

/// Bake `audio/{music,sfx}/*.wav` into `build/nitrofs/soundbank.bin` (via the
/// `wav2bank` library, which wraps `mmutil`) and write the generated sound-ID
/// constants to `$OUT_DIR/sounds.rs` for the game to `include!`.
///
/// `mmutil` is only available inside `nix develop`. When it is missing (e.g. a
/// plain `cargo check` outside the shell) we still emit a *predicted* IDs module
/// so the game keeps compiling; only the soundbank blob is skipped.
fn compile_audio() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join(AUDIO_DIR);
    let out_bin = manifest.join(NITROFS_DIR).join("soundbank.bin");
    let out_ids = PathBuf::from(env::var("OUT_DIR").unwrap()).join("sounds.rs");
    // The loop-patch scratch dir must stay out of the NitroFS tree, or its temp
    // WAVs would be packed into the ROM.
    let work = PathBuf::from(env::var("OUT_DIR").unwrap()).join("wav2bank-work");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-env-changed=BLOCKSDS");
    println!("cargo:rerun-if-env-changed=MMUTIL");

    if !src.is_dir() {
        write_predicted_ids(&src, &out_ids);
        return;
    }

    match wav2bank::find_mmutil() {
        Some(mmutil) => match wav2bank::build_dir(&src, &out_bin, &out_ids, &mmutil, &work) {
            Ok(built) => {
                for input in &built.inputs {
                    println!("cargo:rerun-if-changed={}", input.display());
                }
            }
            Err(e) => {
                println!("cargo:warning=soundbank baking failed: {e}");
                write_predicted_ids(&src, &out_ids);
            }
        },
        None => {
            println!(
                "cargo:warning=mmutil not found (run inside `nix develop`); \
                 soundbank.bin not baked — audio will be silent in the ROM"
            );
            write_predicted_ids(&src, &out_ids);
        }
    }
}

/// Emit the predicted sound-ID module (no `mmutil`), so the game still compiles.
fn write_predicted_ids(src: &std::path::Path, out_ids: &std::path::Path) {
    let defines = wav2bank::predict_ids(src).unwrap_or_default();
    let rust = wav2bank::ids::emit_rust(&defines);
    if let Err(e) = std::fs::write(out_ids, rust) {
        println!("cargo:warning=could not write {}: {e}", out_ids.display());
    }
}

/// Bake every `assets/sprites/**/*.png` into `build/nitrofs/sprites/*.sprite`
/// via BlocksDS's `grit` (wrapped by the `png2sprite` library). Always emits
/// `$OUT_DIR/sprites.rs` — a Rust constants module of NitroFS paths the game
/// `include!`s — even when `grit` is missing, so `cargo check` outside `nix
/// develop` still compiles (only the binary assets are skipped, so loads
/// silently fail at runtime).
fn compile_sprites() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join(SPRITE_DIR);
    let dst = manifest.join(NITROFS_DIR).join(png2sprite::NITROFS_SUBDIR);
    let work = PathBuf::from(env::var("OUT_DIR").unwrap()).join("png2sprite-work");
    let out_rs = PathBuf::from(env::var("OUT_DIR").unwrap()).join("sprites.rs");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-env-changed=BLOCKSDS");
    println!("cargo:rerun-if-env-changed=GRIT");

    // Even if the source tree is empty, write a stub so `include!` works.
    if !src.is_dir() {
        write_sprite_consts(&out_rs, &[]);
        return;
    }

    let grit = png2sprite::find_grit();
    let items = match &grit {
        Some(grit) => {
            match png2sprite::build_dir(&src, &dst, grit, &work, &png2sprite::Options::default()) {
                Ok(built) => {
                    for input in built.inputs() {
                        println!("cargo:rerun-if-changed={}", input.display());
                    }
                    built.items
                }
                Err(e) => {
                    println!("cargo:warning=sprite baking failed: {e}");
                    png2sprite::predict_dir(&src).unwrap_or_default()
                }
            }
        }
        None => {
            println!(
                "cargo:warning=grit not found (run inside `nix develop`); \
                 sprite PNGs not baked — sprites will silently fail to load at runtime"
            );
            png2sprite::predict_dir(&src).unwrap_or_default()
        }
    };
    write_sprite_consts(&out_rs, &items);
}

/// Emit the generated `sprites.rs` constants module, mirroring how
/// `compile_audio` writes `sounds.rs`.
fn write_sprite_consts(out_rs: &std::path::Path, items: &[png2sprite::Baked]) {
    let rust = png2sprite::emit_rust_consts(items);
    if let Err(e) = std::fs::write(out_rs, rust) {
        println!("cargo:warning=could not write {}: {e}", out_rs.display());
    }
}

/// Bake every PNG under `assets/backgrounds/{tiled,bitmap}/**/*.png` into the
/// matching `.bg` / `.bbg` blob under `build/nitrofs/backgrounds/`. Always
/// emits `$OUT_DIR/backgrounds.rs` (the constants module the game
/// `include!`s) even when `grit` is missing — only the binaries are skipped
/// so `cargo check` outside `nix develop` still compiles, with the
/// `Backgrounds` setters silently failing to load at runtime.
fn compile_backgrounds() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join(BG_DIR);
    let dst = manifest.join(NITROFS_DIR).join(png2bg::NITROFS_SUBDIR);
    let work = PathBuf::from(env::var("OUT_DIR").unwrap()).join("png2bg-work");
    let out_rs = PathBuf::from(env::var("OUT_DIR").unwrap()).join("backgrounds.rs");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-env-changed=BLOCKSDS");
    println!("cargo:rerun-if-env-changed=GRIT");

    if !src.is_dir() {
        write_background_consts(&out_rs, &[]);
        return;
    }

    let grit = png2bg::find_grit();
    let items = match &grit {
        Some(grit) => match png2bg::build_dir(&src, &dst, grit, &work) {
            Ok(built) => {
                for input in built.inputs() {
                    println!("cargo:rerun-if-changed={}", input.display());
                }
                built.items
            }
            Err(e) => {
                println!("cargo:warning=background baking failed: {e}");
                png2bg::predict_dir(&src).unwrap_or_default()
            }
        },
        None => {
            println!(
                "cargo:warning=grit not found (run inside `nix develop`); \
                 background PNGs not baked — backgrounds will silently fail to load at runtime"
            );
            png2bg::predict_dir(&src).unwrap_or_default()
        }
    };
    write_background_consts(&out_rs, &items);
}

/// Emit the generated `backgrounds.rs` constants module.
fn write_background_consts(out_rs: &std::path::Path, items: &[png2bg::Baked]) {
    let rust = png2bg::emit_rust_consts(items);
    if let Err(e) = std::fs::write(out_rs, rust) {
        println!("cargo:warning=could not write {}: {e}", out_rs.display());
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
    // references between them. `libmm9` (maxmod, ARM9) provides the audio mixer
    // commands that `bevy_nds_audio` calls; it lives in its own lib dir.
    println!("cargo:rustc-link-search=native={blocksds}/libs/libnds/lib");
    println!("cargo:rustc-link-search=native={blocksds}/libs/maxmod/lib");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg=-lnds9");
    println!("cargo:rustc-link-arg=-lmm9");
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-lgcc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");

    println!("cargo:rerun-if-env-changed=BLOCKSDS");
}
