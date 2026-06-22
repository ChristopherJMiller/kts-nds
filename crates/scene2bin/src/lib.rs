//! `scene2bin` — bake `assets/spaces/*.ron` into `.scene` NitroFS blobs.
//!
//! The host counterpart to [`bevy_nds_scene`]: it parses the human-authored RON
//! sidecar (the format the desktop editor also reads/writes), validates it
//! against the available meshes and neighbouring spaces, and encodes it into the
//! flat little-endian `.scene` blob that `bevy_nds_scene::asset::parse` reads at
//! runtime. **RON never reaches the DS** — only the packed blob does.
//!
//! Mirrors the `obj2dl` / `png2sprite` shape: a `build.rs` calls [`build_dir`]
//! over `assets/spaces/`, the `.scene` outputs land under
//! `build/nitrofs/spaces/`, and [`emit_rust_consts`] writes a `spaces.rs`
//! module of NitroFS-path constants the game `include!`s.
//!
//! The on-disk `.scene` layout is documented (and round-trip tested) in
//! `bevy_nds_scene::asset`. [`encode`] here is the authoritative writer; keep
//! the two in sync.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// ASCII `"BSC1"` — magic prefix of a baked `.scene` file. Matches
/// `bevy_nds_scene::asset::MAGIC`.
pub const ASSET_MAGIC: u32 = u32::from_le_bytes(*b"BSC1");
/// `.scene` format version. Matches `bevy_nds_scene::asset::VERSION`.
pub const VERSION: u16 = 1;
/// Extension of a baked space.
pub const ASSET_EXT: &str = "scene";
/// NitroFS subdirectory holding baked spaces (mirrors the source `spaces/`).
pub const NITROFS_SUBDIR: &str = "spaces";

// --- RON authoring model -----------------------------------------------------

/// A space as authored in RON. Terse: most fields default so a hand-written
/// file only states what differs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    /// Per-space camera framing (#27). Defaults to a soft follow.
    #[serde(default)]
    pub camera: Camera,
    /// Placed objects (geometry + role).
    #[serde(default)]
    pub instances: Vec<Instance>,
    /// Connections to neighbouring spaces in the level graph.
    #[serde(default)]
    pub exits: Vec<Exit>,
}

/// Per-space authored camera. Variant order is the wire enum (#27); extend at
/// the end to keep the encoding stable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Camera {
    Follow { height: f32, dist: f32, pitch: f32 },
    TopDown { height: f32 },
    Rail2_5D { height: f32, dist: f32, pitch: f32 },
    CaptureFraming,
}

impl Default for Camera {
    fn default() -> Self {
        // Spike-C follow defaults (src/main.rs CAM_* constants).
        Camera::Follow { height: 1.7, dist: 2.0, pitch: -0.7 }
    }
}

/// One placed object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// Bare mesh name (`"teapot"` ⇒ `nitro:/teapot.dl`); omit for a
    /// transform-only marker (spawn point, logical node).
    #[serde(default)]
    pub mesh: Option<String>,
    /// Game-defined role tag the runtime keeps opaque.
    pub role: String,
    #[serde(default)]
    pub pos: [f32; 3],
    #[serde(default)]
    pub rot: [f32; 3],
    #[serde(default = "one3")]
    pub scale: [f32; 3],
    #[serde(default)]
    pub material: Option<Material>,
    #[serde(default)]
    pub flags: u32,
    /// Ground-plane (XZ) waypoints (enemy patrol, rail).
    #[serde(default)]
    pub path: Vec<[f32; 2]>,
}

fn one3() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

/// Lit-material colours for an instance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Material {
    pub diffuse: [u8; 3],
    pub ambient: [u8; 3],
}

/// A graph connection to a neighbouring space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exit {
    /// Name (stem) of the neighbouring space, e.g. `"corridor_b"`. Use
    /// `"UNRESOLVED"` to deliberately defer a neighbour (#27 checklist).
    pub to: String,
    #[serde(default)]
    pub at: [f32; 3],
    /// Gate/objective id (0 = always open).
    #[serde(default)]
    pub gate: u32,
}

/// Sentinel a designer writes for a not-yet-authored neighbour, so a dangling
/// exit is an intentional marker, not a typo.
pub const UNRESOLVED: &str = "UNRESOLVED";

// --- Parse / validate / encode ----------------------------------------------

/// Parse a RON space file. Returns a human-readable error on syntax problems.
pub fn parse_ron(src: &str) -> Result<Space, String> {
    ron::from_str(src).map_err(|e| format!("RON parse error: {e}"))
}

/// Serialise a space back to pretty RON — the writer the desktop editor uses, so
/// its output round-trips through [`parse_ron`] (same `ron` version on both
/// sides). Field names match the authoring schema above.
pub fn to_ron(space: &Space) -> Result<String, String> {
    let cfg = ron::ser::PrettyConfig::new()
        .struct_names(true)
        .indentor("    ".to_string());
    ron::ser::to_string_pretty(space, cfg).map_err(|e| format!("RON serialize error: {e}"))
}

/// Hard-validate a space (errors that *guarantee* a broken ROM). `mesh_exists`
/// reports whether a referenced mesh has a source `.obj`; `space_exists` whether
/// a neighbour space file is present. Soft issues are returned via
/// [`validate_warnings`] instead.
pub fn validate(
    space: &Space,
    mesh_exists: impl Fn(&str) -> bool,
) -> Result<(), String> {
    for (i, inst) in space.instances.iter().enumerate() {
        if inst.role.trim().is_empty() {
            return Err(format!("instance {i}: empty `role`"));
        }
        if let Some(mesh) = &inst.mesh {
            if !mesh_exists(mesh) {
                return Err(format!(
                    "instance {i} (role `{}`): mesh `{mesh}` has no source `{mesh}.obj`",
                    inst.role
                ));
            }
        }
    }
    Ok(())
}

/// Non-fatal authoring warnings (a dangling exit not marked `UNRESOLVED`). The
/// caller (build.rs) surfaces these as `cargo:warning=` lines.
pub fn validate_warnings(space: &Space, space_exists: impl Fn(&str) -> bool) -> Vec<String> {
    let mut warnings = Vec::new();
    for exit in &space.exits {
        if exit.to != UNRESOLVED && !space_exists(&exit.to) {
            warnings.push(format!(
                "exit to `{}`: no such space `{}.ron` (mark it `UNRESOLVED` to defer)",
                exit.to, exit.to
            ));
        }
    }
    warnings
}

/// Encode a space to the `.scene` blob. Authoritative writer; the inverse is
/// `bevy_nds_scene::asset::parse`.
pub fn encode(space: &Space) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32(ASSET_MAGIC);
    w.u16(VERSION);
    match space.camera {
        Camera::Follow { height, dist, pitch } => {
            w.u16(0);
            w.f32(height);
            w.f32(dist);
            w.f32(pitch);
            w.f32(0.0);
        }
        Camera::TopDown { height } => {
            w.u16(1);
            w.f32(height);
            w.f32(0.0);
            w.f32(0.0);
            w.f32(0.0);
        }
        Camera::Rail2_5D { height, dist, pitch } => {
            w.u16(2);
            w.f32(height);
            w.f32(dist);
            w.f32(pitch);
            w.f32(0.0);
        }
        Camera::CaptureFraming => {
            w.u16(3);
            w.f32(0.0);
            w.f32(0.0);
            w.f32(0.0);
            w.f32(0.0);
        }
    }
    w.u32(space.instances.len() as u32);
    for inst in &space.instances {
        w.string(inst.mesh.as_deref().unwrap_or(""));
        w.string(&inst.role);
        for v in inst.pos {
            w.f32(v);
        }
        for v in inst.rot {
            w.f32(v);
        }
        for v in inst.scale {
            w.f32(v);
        }
        match inst.material {
            Some(m) => {
                w.u8(1);
                for v in m.diffuse {
                    w.u8(v);
                }
                for v in m.ambient {
                    w.u8(v);
                }
            }
            None => {
                w.u8(0);
                for _ in 0..6 {
                    w.u8(0);
                }
            }
        }
        w.u32(inst.flags);
        w.u16(inst.path.len() as u16);
        for p in &inst.path {
            w.f32(p[0]);
            w.f32(p[1]);
        }
    }
    w.u32(space.exits.len() as u32);
    for exit in &space.exits {
        w.string(&exit.to);
        for v in exit.at {
            w.f32(v);
        }
        w.u32(exit.gate);
    }
    w.0
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn string(&mut self, s: &str) {
        self.u16(s.len() as u16);
        self.0.extend_from_slice(s.as_bytes());
    }
}

// --- Build-directory driver --------------------------------------------------

/// One compiled space, returned by [`build_dir`].
#[derive(Clone, Debug)]
pub struct Built {
    /// Source `.ron` path.
    pub input: PathBuf,
    /// Destination `.scene` path.
    pub output: PathBuf,
    /// Space stem (used for the generated constant name and NitroFS path).
    pub stem: String,
    /// Non-fatal warnings raised while baking this space.
    pub warnings: Vec<String>,
}

/// Bake every `*.ron` in `src_dir` into `<dst_dir>/<stem>.scene`. `assets_dir`
/// is the geometry root (`assets/`) used to validate that referenced meshes
/// have a source `.obj`. Returns the compiled spaces (with any warnings) so a
/// `build.rs` can emit `rerun-if-changed` + `cargo:warning=` lines.
pub fn build_dir(src_dir: &Path, dst_dir: &Path, assets_dir: &Path) -> Result<Vec<Built>, String> {
    let mut stems: Vec<String> = Vec::new();
    let mut inputs: Vec<PathBuf> = Vec::new();
    for entry in read_dir_sorted(src_dir)? {
        if entry.extension().and_then(|e| e.to_str()) != Some("ron") {
            continue;
        }
        let stem = entry
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad file name: {}", entry.display()))?
            .to_string();
        stems.push(stem);
        inputs.push(entry);
    }

    let mesh_exists = |name: &str| assets_dir.join(format!("{name}.obj")).is_file();

    let mut built = Vec::new();
    for (stem, input) in stems.iter().zip(inputs.iter()) {
        let src = std::fs::read_to_string(input)
            .map_err(|e| format!("could not read {}: {e}", input.display()))?;
        let space = parse_ron(&src).map_err(|e| format!("{}: {e}", input.display()))?;
        validate(&space, mesh_exists).map_err(|e| format!("{}: {e}", input.display()))?;
        let space_exists = |name: &str| stems.iter().any(|s| s == name);
        let warnings = validate_warnings(&space, space_exists);

        let output = dst_dir.join(format!("{stem}.{ASSET_EXT}"));
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        std::fs::write(&output, encode(&space))
            .map_err(|e| format!("could not write {}: {e}", output.display()))?;

        built.push(Built {
            input: input.clone(),
            output,
            stem: stem.clone(),
            warnings,
        });
    }
    Ok(built)
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("could not read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Emit a `spaces.rs` module of NUL-terminated NitroFS-path constants — one per
/// baked space — for the game to `include!` (mirrors `wav2bank`'s `sounds.rs`).
pub fn emit_rust_consts(built: &[Built]) -> String {
    let mut s = String::new();
    s.push_str("// @generated by scene2bin from assets/spaces/*.ron.\n");
    s.push_str("// Each constant is a NUL-terminated NitroFS path you can pass to\n");
    s.push_str("// `bevy_nds_scene::load` (or `LoadSpace { path }`).\n");
    for b in built {
        s.push_str("pub const ");
        s.push_str(&const_name(&b.stem));
        s.push_str(": &[u8] = b\"");
        s.push_str(&format!("nitro:/{NITROFS_SUBDIR}/{}.{ASSET_EXT}", b.stem));
        s.push_str("\\0\";\n");
    }
    s
}

/// Emit the `spaces.rs` constants module from just the source directory's
/// `*.ron` stems, without parsing — the fallback `build.rs` uses when baking
/// errors out, so the game's `include!` always resolves (the space simply won't
/// load at runtime). Mirrors `wav2bank::predict_ids`.
pub fn predict_consts(src_dir: &Path) -> String {
    let mut built: Vec<Built> = read_dir_sorted(src_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ron"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_string();
            Some(Built {
                output: p.with_extension(ASSET_EXT),
                input: p,
                stem,
                warnings: Vec::new(),
            })
        })
        .collect();
    built.sort_by(|a, b| a.stem.cmp(&b.stem));
    emit_rust_consts(&built)
}

/// `corridor_b` → `CORRIDOR_B`. Mirrors the sprite/sound constant naming.
pub fn const_name(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        Space(
            camera: Follow(height: 1.7, dist: 2.0, pitch: -0.7),
            instances: [
                Instance(
                    mesh: Some("teapot"),
                    role: "avatar",
                    rot: (-1.5708, 0.0, 0.0),
                    scale: (0.11, 0.11, 0.11),
                    material: Some((diffuse: (110, 180, 235), ambient: (26, 40, 58))),
                ),
                Instance(
                    mesh: Some("cube"),
                    role: "enemy",
                    pos: (1.2, 0.0, 0.6),
                    scale: (0.16, 0.16, 0.16),
                    path: [(1.2, 0.6), (1.2, -0.6)],
                ),
                Instance(role: "spawn"),
            ],
            exits: [ Exit(to: "corridor_b", at: (2.0, 0.0, 0.0)) ],
        )
    "#;

    #[test]
    fn parses_terse_ron_with_defaults() {
        let space = parse_ron(SAMPLE).unwrap();
        assert_eq!(space.instances.len(), 3);
        // Marker instance defaulted: no mesh, identity scale, no material.
        let marker = &space.instances[2];
        assert_eq!(marker.mesh, None);
        assert_eq!(marker.scale, [1.0, 1.0, 1.0]);
        assert!(marker.material.is_none());
        assert!(matches!(space.camera, Camera::Follow { .. }));
    }

    #[test]
    fn encodes_with_expected_header() {
        let space = parse_ron(SAMPLE).unwrap();
        let blob = encode(&space);
        assert_eq!(&blob[0..4], b"BSC1");
        assert_eq!(u16::from_le_bytes([blob[4], blob[5]]), VERSION);
        assert_eq!(u16::from_le_bytes([blob[6], blob[7]]), 0); // Follow
    }

    #[test]
    fn validate_rejects_missing_mesh() {
        let space = parse_ron(SAMPLE).unwrap();
        // No mesh exists at all → the first meshed instance fails.
        let err = validate(&space, |_| false).unwrap_err();
        assert!(err.contains("teapot"), "{err}");
        // All meshes present → ok.
        assert!(validate(&space, |_| true).is_ok());
    }

    #[test]
    fn warns_on_dangling_exit() {
        let space = parse_ron(SAMPLE).unwrap();
        let warns = validate_warnings(&space, |_| false);
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("corridor_b"));
        // Present neighbour → no warning.
        assert!(validate_warnings(&space, |n| n == "corridor_b").is_empty());
    }

    #[test]
    fn to_ron_round_trips_through_parse() {
        // The editor saves via `to_ron`; it must parse back byte-for-meaning.
        let original = parse_ron(SAMPLE).unwrap();
        let text = to_ron(&original).unwrap();
        let reparsed = parse_ron(&text).unwrap();
        // Compare the encoded blobs (covers every field without deriving PartialEq).
        assert_eq!(encode(&original), encode(&reparsed));
    }

    #[test]
    fn const_name_uppercases_and_guards_digits() {
        assert_eq!(const_name("corridor_b"), "CORRIDOR_B");
        assert_eq!(const_name("atrium"), "ATRIUM");
        assert_eq!(const_name("2nd-floor"), "_2ND_FLOOR");
    }
}
