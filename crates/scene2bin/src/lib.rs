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
/// `.scene` format version. Matches `bevy_nds_scene::asset::VERSION`. v2 replaced
/// hand-authored `exits` with a zone `bounds` + baker-**derived** `connections`
/// (the Euclidean map rework, #27).
pub const VERSION: u16 = 2;
/// Extension of a baked space.
pub const ASSET_EXT: &str = "scene";
/// NitroFS subdirectory holding baked spaces (mirrors the source `spaces/`).
pub const NITROFS_SUBDIR: &str = "spaces";

// --- RON authoring model -----------------------------------------------------

/// A zone as authored in RON. Terse: most fields default so a hand-written file
/// only states what differs. A zone is one cell of the **Euclidean map** (#27):
/// it lives at `place` in the shared global frame and the baker derives its
/// connections from which zones abut it there — the author never writes exits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    /// Per-zone camera framing (#27). Defaults to a soft follow.
    #[serde(default)]
    pub camera: Camera,
    /// This zone's placement in the shared global map frame (XZ). **Authoring
    /// only** — the baker uses it to derive connections; it never reaches the
    /// runtime (only the resulting per-connection deltas do).
    #[serde(default)]
    pub place: [f32; 2],
    /// The zone's walkable extent, in **local** coordinates. Drives the runtime
    /// clamp and the boundary derivation. Defaults to a ±2 arena pad.
    #[serde(default)]
    pub bounds: Bounds,
    /// Placed objects (geometry + role), in local coordinates.
    #[serde(default)]
    pub instances: Vec<Instance>,
}

/// A rectangle on the ground (XZ) plane, in a zone's **local** coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bounds {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Default for Bounds {
    fn default() -> Self {
        Self { min: [-2.0, -2.0], max: [2.0, 2.0] }
    }
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

/// Edge of a zone a boundary lies on, west/east along ±X and south/north along
/// ±Z. The wire value baked into the `.scene` blob.
pub const SIDE_WEST: u8 = 0; // -X
pub const SIDE_EAST: u8 = 1; // +X
pub const SIDE_SOUTH: u8 = 2; // -Z
pub const SIDE_NORTH: u8 = 3; // +Z

/// A **derived** connection from one zone across a shared boundary to a
/// neighbour — computed by [`derive_connections`] from the zones' global
/// placement, never hand-authored. `side` is which edge of *this* zone the
/// boundary lies on; `lo`/`hi` bound the boundary segment along that edge (in
/// this zone's local coordinates, on the axis parallel to the edge); `delta` is
/// added to the avatar's local position when it crosses, placing it in the
/// neighbour's frame with its global position unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    pub neighbour: String,
    pub side: u8,
    pub lo: f32,
    pub hi: f32,
    pub delta: [f32; 2],
    pub gate: u32,
}

/// Derive each zone's connections from the whole map's global layout: two zones
/// connect wherever their `bounds` (placed at `place`) **abut** along a shared
/// edge with overlapping extent. This is the heart of the Euclidean model — the
/// designer lays zones out in one frame and the connections (and the cross-over
/// `delta`) fall out of the geometry. Pure + host-tested.
pub fn derive_connections(zones: &[(String, Space)]) -> std::collections::BTreeMap<String, Vec<Connection>> {
    /// Abutment / overlap tolerance (world units).
    const TOL: f32 = 0.01;
    let mut out = std::collections::BTreeMap::new();
    for (an, a) in zones {
        let (axmin, axmax) = (a.place[0] + a.bounds.min[0], a.place[0] + a.bounds.max[0]);
        let (azmin, azmax) = (a.place[1] + a.bounds.min[1], a.place[1] + a.bounds.max[1]);
        let mut conns: Vec<Connection> = Vec::new();
        for (bn, b) in zones {
            if bn == an {
                continue;
            }
            let (bxmin, bxmax) = (b.place[0] + b.bounds.min[0], b.place[0] + b.bounds.max[0]);
            let (bzmin, bzmax) = (b.place[1] + b.bounds.min[1], b.place[1] + b.bounds.max[1]);
            let delta = [a.place[0] - b.place[0], a.place[1] - b.place[1]];
            // East/west edges share a vertical (Z) seam; north/south share a
            // horizontal (X) seam. `lo`/`hi` are the overlap along the seam,
            // expressed back in A's local coordinates.
            let mut push = |side: u8, edge_meets: bool, lo_g: f32, hi_g: f32, axis_origin: f32| {
                if edge_meets && hi_g - lo_g > TOL {
                    conns.push(Connection {
                        neighbour: bn.clone(),
                        side,
                        lo: lo_g - axis_origin,
                        hi: hi_g - axis_origin,
                        delta,
                        gate: 0,
                    });
                }
            };
            let zlo = azmin.max(bzmin);
            let zhi = azmax.min(bzmax);
            push(SIDE_EAST, (axmax - bxmin).abs() < TOL, zlo, zhi, a.place[1]);
            push(SIDE_WEST, (axmin - bxmax).abs() < TOL, zlo, zhi, a.place[1]);
            let xlo = axmin.max(bxmin);
            let xhi = axmax.min(bxmax);
            push(SIDE_NORTH, (azmax - bzmin).abs() < TOL, xlo, xhi, a.place[0]);
            push(SIDE_SOUTH, (azmin - bzmax).abs() < TOL, xlo, xhi, a.place[0]);
        }
        out.insert(an.clone(), conns);
    }
    out
}

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

/// Hard-validate a zone (errors that *guarantee* a broken ROM). `mesh_exists`
/// reports whether a referenced mesh has a source `.obj`.
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
    let b = &space.bounds;
    if b.min[0] >= b.max[0] || b.min[1] >= b.max[1] {
        return Err(format!(
            "bounds min {:?} must be strictly less than max {:?} on both axes",
            b.min, b.max
        ));
    }
    Ok(())
}

/// Non-fatal authoring warnings, given the whole map's derived connections: a
/// zone that abuts nothing (isolated) in a multi-zone map is almost certainly a
/// misplacement. The caller (build.rs) surfaces these as `cargo:warning=` lines.
pub fn isolation_warnings(
    conns: &std::collections::BTreeMap<String, Vec<Connection>>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut warnings = std::collections::BTreeMap::new();
    if conns.len() < 2 {
        return warnings; // a single-zone map is legitimately unconnected
    }
    for (stem, cs) in conns {
        if cs.is_empty() {
            warnings.insert(
                stem.clone(),
                std::vec![format!(
                    "zone `{stem}` is isolated — no other zone's bounds abut it; check its `place`/`bounds`"
                )],
            );
        }
    }
    warnings
}

/// Encode a zone to the `.scene` blob, with its baker-derived `conns` (see
/// [`derive_connections`]). Authoritative writer; the inverse is
/// `bevy_nds_scene::asset::parse`.
pub fn encode(space: &Space, conns: &[Connection]) -> Vec<u8> {
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
    // Zone bounds (local rect: min_x, min_z, max_x, max_z).
    w.f32(space.bounds.min[0]);
    w.f32(space.bounds.min[1]);
    w.f32(space.bounds.max[0]);
    w.f32(space.bounds.max[1]);
    // Derived connections.
    w.u32(conns.len() as u32);
    for c in conns {
        w.string(&c.neighbour);
        w.u8(c.side);
        w.f32(c.lo);
        w.f32(c.hi);
        w.f32(c.delta[0]);
        w.f32(c.delta[1]);
        w.u32(c.gate);
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

    // Parse + hard-validate every zone first — connection derivation needs the
    // whole map at once (each zone's neighbours depend on the global layout).
    let mut zones: Vec<(String, Space)> = Vec::new();
    for (stem, input) in stems.iter().zip(inputs.iter()) {
        let src = std::fs::read_to_string(input)
            .map_err(|e| format!("could not read {}: {e}", input.display()))?;
        let space = parse_ron(&src).map_err(|e| format!("{}: {e}", input.display()))?;
        validate(&space, mesh_exists).map_err(|e| format!("{}: {e}", input.display()))?;
        zones.push((stem.clone(), space));
    }

    let conns = derive_connections(&zones);
    let warns = isolation_warnings(&conns);

    let mut built = Vec::new();
    for ((stem, space), input) in zones.iter().zip(inputs.iter()) {
        let zone_conns = conns.get(stem).map(Vec::as_slice).unwrap_or(&[]);
        let output = dst_dir.join(format!("{stem}.{ASSET_EXT}"));
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        std::fs::write(&output, encode(space, zone_conns))
            .map_err(|e| format!("could not write {}: {e}", output.display()))?;

        built.push(Built {
            input: input.clone(),
            output,
            stem: stem.clone(),
            warnings: warns.get(stem).cloned().unwrap_or_default(),
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
        )
    "#;

    fn zone(place: [f32; 2], min: [f32; 2], max: [f32; 2]) -> Space {
        Space {
            camera: Camera::default(),
            place,
            bounds: Bounds { min, max },
            instances: Vec::new(),
        }
    }

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
        let blob = encode(&space, &[]);
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
    fn derives_connection_between_abutting_zones() {
        // Atrium (±2 pad) at the origin; corridor placed east so its west edge
        // (local -2.2 + place 4.2 = 2.0) meets the atrium's east edge (2.0).
        let zones = std::vec![
            ("atrium".to_string(), zone([0.0, 0.0], [-2.0, -2.0], [2.0, 2.0])),
            ("corridor".to_string(), zone([4.2, 0.0], [-2.2, -0.55], [2.2, 0.55])),
        ];
        let conns = derive_connections(&zones);

        let a = &conns["atrium"];
        assert_eq!(a.len(), 1, "atrium should connect to exactly the corridor");
        assert_eq!(a[0].neighbour, "corridor");
        assert_eq!(a[0].side, SIDE_EAST);
        // Crossing east adds (place_atrium - place_corridor) to land in the
        // corridor's frame: (0 - 4.2, 0) = (-4.2, 0).
        assert!((a[0].delta[0] - (-4.2)).abs() < 1e-4, "delta {:?}", a[0].delta);
        assert!(a[0].delta[1].abs() < 1e-4);
        // Boundary span = the z-overlap (the corridor's narrow rail), in atrium-local z.
        assert!((a[0].lo - (-0.55)).abs() < 1e-4 && (a[0].hi - 0.55).abs() < 1e-4);

        let c = &conns["corridor"];
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].side, SIDE_WEST);
        assert!((c[0].delta[0] - 4.2).abs() < 1e-4, "delta {:?}", c[0].delta);
    }

    #[test]
    fn isolated_zone_warns_only_in_a_multi_zone_map() {
        // Two zones far apart → both isolated → both warn.
        let zones = std::vec![
            ("a".to_string(), zone([0.0, 0.0], [-1.0, -1.0], [1.0, 1.0])),
            ("b".to_string(), zone([100.0, 0.0], [-1.0, -1.0], [1.0, 1.0])),
        ];
        let warns = isolation_warnings(&derive_connections(&zones));
        assert_eq!(warns.len(), 2);
        // A lone zone is legitimately unconnected → no warning.
        let lone = std::vec![("solo".to_string(), zone([0.0, 0.0], [-1.0, -1.0], [1.0, 1.0]))];
        assert!(isolation_warnings(&derive_connections(&lone)).is_empty());
    }

    #[test]
    fn validate_rejects_degenerate_bounds() {
        let mut space = parse_ron(SAMPLE).unwrap();
        space.bounds = Bounds { min: [2.0, -2.0], max: [-2.0, 2.0] }; // min.x >= max.x
        assert!(validate(&space, |_| true).is_err());
    }

    #[test]
    fn to_ron_round_trips_through_parse() {
        // The editor saves via `to_ron`; it must parse back byte-for-meaning.
        let original = parse_ron(SAMPLE).unwrap();
        let text = to_ron(&original).unwrap();
        let reparsed = parse_ron(&text).unwrap();
        // Compare the encoded blobs (covers every field without deriving PartialEq).
        assert_eq!(encode(&original, &[]), encode(&reparsed, &[]));
    }

    #[test]
    fn const_name_uppercases_and_guards_digits() {
        assert_eq!(const_name("corridor_b"), "CORRIDOR_B");
        assert_eq!(const_name("atrium"), "ATRIUM");
        assert_eq!(const_name("2nd-floor"), "_2ND_FLOOR");
    }
}
