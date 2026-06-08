//! `wav2bank` — bake `audio/*.wav` into a maxmod soundbank for `bevy_nds_audio`.
//!
//! The DS has no runtime asset server and no audio decoder: sound is *samples
//! in a soundbank*, mixed on the ARM7 by maxmod. This crate is the audio
//! counterpart to `obj2dl`. It wraps the BlocksDS `mmutil` tool to turn a tree
//! of WAVs into:
//!
//! - `soundbank.bin` — packed into NitroFS by `just rom` and mounted at runtime
//!   with `mmInitDefault("nitro:/soundbank.bin")`; and
//! - a generated Rust module of the numeric IDs `mmutil` assigns, so game code
//!   refers to sounds by name (`SFX_BLIP_SELECT`) with no manual bookkeeping.
//!
//! Source layout (a convention, mirroring `assets/` for models):
//!
//! ```text
//! audio/
//!   music/*.wav   # looped forever — a forward-loop point is injected
//!   sfx/*.wav     # one-shot effects, used verbatim
//! ```
//!
//! Usable as a **library** (from a `build.rs`) via [`build_dir`], or as the
//! **`wav2bank` CLI** for one-off bakes. The loop-injection and header parsing
//! are pure and unit-tested (see [`wav`], [`ids`]).

use std::path::{Path, PathBuf};
use std::process::Command;

pub mod ids;
pub mod wav;

/// Subdirectory of looped "music" sources (a forward loop is injected).
pub const MUSIC_DIR: &str = "music";
/// Subdirectory of one-shot "sfx" sources (used verbatim).
pub const SFX_DIR: &str = "sfx";

/// Outputs of a successful [`build_dir`].
#[derive(Clone, Debug)]
pub struct Built {
    /// The compiled soundbank blob (`soundbank.bin`).
    pub soundbank: PathBuf,
    /// The generated Rust IDs module.
    pub ids_rs: PathBuf,
    /// Source WAVs consumed, in the order their IDs were assigned. A `build.rs`
    /// emits `cargo:rerun-if-changed` for each.
    pub inputs: Vec<PathBuf>,
}

/// Locate the `mmutil` binary: honour `$MMUTIL`, then `$BLOCKSDS/tools/mmutil`,
/// then the `PATH`. Returns `None` if none exist (e.g. outside `nix develop`).
pub fn find_mmutil() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MMUTIL") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(blocksds) = std::env::var("BLOCKSDS") {
        let p = PathBuf::from(blocksds).join("tools/mmutil/mmutil");
        if p.is_file() {
            return Some(p);
        }
    }
    // Fall back to a bare `mmutil` on the PATH.
    if Command::new("mmutil").arg("-V").output().is_ok() {
        return Some(PathBuf::from("mmutil"));
    }
    None
}

/// Collect `*.wav` files in `dir`, sorted by path for a deterministic ID order.
/// A missing directory is treated as empty.
fn collect_wavs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut wavs = Vec::new();
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?
    {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wav") {
            wavs.push(path);
        }
    }
    wavs.sort();
    Ok(wavs)
}

/// Predict the generated sound-ID constants *without* running `mmutil`, by
/// replicating its naming and ordering (music sources first, then sfx, each
/// sorted by name; ids are assigned `0..N`).
///
/// Used as an offline fallback so dependent code still compiles when `mmutil`
/// is unavailable (e.g. `cargo check` outside `nix develop`).
pub fn predict_ids(src_dir: &Path) -> Result<Vec<ids::Define>, String> {
    let music = collect_wavs(&src_dir.join(MUSIC_DIR))?;
    let sfx = collect_wavs(&src_dir.join(SFX_DIR))?;
    let mut out = Vec::new();
    for (i, path) in music.iter().chain(sfx.iter()).enumerate() {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad file name: {}", path.display()))?;
        out.push(ids::Define {
            name: ids::sample_const_name(stem),
            value: i as u32,
        });
    }
    Ok(out)
}

/// Bake the `audio/` tree at `src_dir` into `out_bin` (+ generated `out_ids_rs`)
/// using `mmutil`.
///
/// `music/*.wav` get a forward loop injected (written to `work_dir`) so they
/// loop as background music; `sfx/*.wav` are used as-is. IDs are assigned
/// music-first then sfx, each group sorted by name, so the generated constants
/// are stable across builds. `work_dir` must be outside any directory packed
/// into the ROM (the loop-patched copies are an implementation detail).
pub fn build_dir(
    src_dir: &Path,
    out_bin: &Path,
    out_ids_rs: &Path,
    mmutil: &Path,
    work_dir: &Path,
) -> Result<Built, String> {
    let music = collect_wavs(&src_dir.join(MUSIC_DIR))?;
    let sfx = collect_wavs(&src_dir.join(SFX_DIR))?;
    if music.is_empty() && sfx.is_empty() {
        return Err(format!("no .wav files under {}", src_dir.display()));
    }

    // A work directory holds the loop-patched copies we hand to mmutil, keeping
    // the source tree pristine. The caller places it outside the ROM tree.
    let work = work_dir.to_path_buf();
    std::fs::create_dir_all(&work)
        .map_err(|e| format!("could not create {}: {e}", work.display()))?;
    if let Some(parent) = out_ids_rs.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    // Prepare the files mmutil will read: loop-patched music, verbatim sfx. The
    // basename's stem becomes the generated constant name, so it is preserved.
    let mut prepared = Vec::new();
    let mut inputs = Vec::new();
    for path in music.iter() {
        let bytes =
            std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let looped =
            wav::inject_forward_loop(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let dst = work.join(path.file_name().unwrap());
        std::fs::write(&dst, &looped)
            .map_err(|e| format!("could not write {}: {e}", dst.display()))?;
        prepared.push(dst);
        inputs.push(path.clone());
    }
    for path in sfx.iter() {
        let dst = work.join(path.file_name().unwrap());
        std::fs::copy(path, &dst).map_err(|e| format!("could not copy {}: {e}", path.display()))?;
        prepared.push(dst);
        inputs.push(path.clone());
    }

    if let Some(parent) = out_bin.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    // Run: mmutil <wavs...> -d -o<soundbank.bin> -h<header.h>
    // `-d` selects the NDS soundbank format.
    let header = work.join("soundbank.h");
    let mut cmd = Command::new(mmutil);
    cmd.args(&prepared);
    cmd.arg("-d");
    cmd.arg(format!("-o{}", out_bin.display()));
    cmd.arg(format!("-h{}", header.display()));
    let output = cmd
        .output()
        .map_err(|e| format!("could not run mmutil ({}): {e}", mmutil.display()))?;
    if !output.status.success() {
        return Err(format!(
            "mmutil failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    // Turn the generated C header into a Rust IDs module.
    let header_src = std::fs::read_to_string(&header)
        .map_err(|e| format!("could not read {}: {e}", header.display()))?;
    let defines = ids::parse_header(&header_src);
    let rust = ids::emit_rust(&defines);
    std::fs::write(out_ids_rs, rust)
        .map_err(|e| format!("could not write {}: {e}", out_ids_rs.display()))?;

    Ok(Built {
        soundbank: out_bin.to_path_buf(),
        ids_rs: out_ids_rs.to_path_buf(),
        inputs,
    })
}
