//! `obj2dl` — bake Wavefront OBJ models into Bevy-DS display-list assets.
//!
//! The DS has no runtime asset server, so a model is always *bytes at an
//! address*. This crate is the runtime counterpart to the `include_obj!` macro:
//! instead of embedding the display list in the ARM9 binary, it writes a `.dl`
//! file (a libnds display list plus an AABB header — see
//! [`bevy_nds_3d_obj::model_to_le_bytes`]) that gets packed into NitroFS and
//! loaded at runtime. Both paths share the [`bevy_nds_3d_obj`] encoder, so the
//! geometry is byte-identical.
//!
//! It is usable two ways:
//! - as a **library** (e.g. from a `build.rs`) via [`convert_file`] /
//!   [`build_dir`], so OBJ assets are compiled into a build directory as part of
//!   the normal `cargo build`;
//! - as the **`obj2dl` CLI** for one-off conversions.

use std::path::{Path, PathBuf};

pub use bevy_nds_3d_obj::Options;

/// Extension used for compiled display-list assets.
pub const ASSET_EXT: &str = "dl";

/// Bake a single OBJ file into a `.dl` asset, creating parent directories.
pub fn convert_file(
    input: &Path,
    output: &Path,
    opts: &Options,
) -> Result<usize, String> {
    let source = std::fs::read_to_string(input)
        .map_err(|e| format!("could not read {}: {e}", input.display()))?;
    let model = bevy_nds_3d_obj::obj_to_display_list(&source, opts)
        .map_err(|e| format!("{}: {e}", input.display()))?;
    let bytes = bevy_nds_3d_obj::model_to_le_bytes(&model);

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(output, &bytes)
        .map_err(|e| format!("could not write {}: {e}", output.display()))?;
    Ok(model.words.len())
}

/// One compiled asset, returned by [`build_dir`].
#[derive(Clone, Debug)]
pub struct Built {
    /// Source `.obj` path.
    pub input: PathBuf,
    /// Destination `.dl` path.
    pub output: PathBuf,
}

/// Compile every `*.obj` in `src_dir` into `<dst_dir>/<stem>.dl`.
///
/// All models use the same [`Options`]; per-model settings are the job of the
/// compile-time `include_obj!` path. Returns the list of compiled assets so a
/// `build.rs` can emit `cargo:rerun-if-changed` lines for each source.
pub fn build_dir(
    src_dir: &Path,
    dst_dir: &Path,
    opts: &Options,
) -> Result<Vec<Built>, String> {
    let mut built = Vec::new();
    let entries = std::fs::read_dir(src_dir)
        .map_err(|e| format!("could not read {}: {e}", src_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", src_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("obj") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad file name: {}", path.display()))?;
        let output = dst_dir.join(format!("{stem}.{ASSET_EXT}"));
        convert_file(&path, &output, opts)?;
        built.push(Built {
            input: path,
            output,
        });
    }
    built.sort_by(|a, b| a.input.cmp(&b.input));
    Ok(built)
}
