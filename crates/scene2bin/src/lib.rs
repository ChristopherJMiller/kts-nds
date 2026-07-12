//! `scene2bin` — bake `assets/levels/<name>/` level directories into `.scene`
//! NitroFS blobs (one per zone).
//!
//! The host counterpart to [`bevy_nds_scene`]. A **level** is the authoring unit:
//! a directory holding a [`Level`] manifest (`level.ron` — the zone-graph layout:
//! each zone's `place`/`bounds`/`camera`) plus one [`Zone`] content file per zone
//! (`<zone>.ron` — its instances + prefab uses). A **zone** is the runtime
//! streaming unit: [`build_levels_dir`] resolves each zone's [`Placement`]s
//! against the shared [`Prefab`] library (`assets/prefabs/*.ron`), [`assemble`]s
//! them into the per-zone [`Space`] intermediate, derives connections from the
//! whole level's layout ([`derive_connections`]), and [`encode`]s each into the
//! flat little-endian `.scene` blob that `bevy_nds_scene::asset::parse` reads at
//! runtime. **RON never reaches the DS** — only the packed blob does.
//!
//! Mirrors the `obj2dl` / `png2sprite` shape: a `build.rs` calls
//! [`build_levels_dir`] over `assets/levels/`, the `.scene` outputs land under
//! `build/nitrofs/levels/<name>/`, and [`emit_rust_consts`] writes a `levels.rs`
//! module of (per-level) NitroFS-path constants the game `include!`s.
//!
//! The on-disk `.scene` layout is documented (and round-trip tested) in
//! `bevy_nds_scene::asset`. [`encode`] here is the authoritative writer; keep
//! the two in sync. Prefabs are flattened away host-side — the blob format and
//! the DS runtime never learn what a prefab is.

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
/// NitroFS subdirectory holding baked levels (mirrors the source `levels/`).
/// Each level bakes to `<NITROFS_SUBDIR>/<level>/<zone>.scene`.
pub const NITROFS_SUBDIR: &str = "levels";

/// Filename of a level's manifest within its directory.
pub const MANIFEST_NAME: &str = "level.ron";

// --- Resolved intermediate ---------------------------------------------------

/// A single zone, fully resolved (manifest layout + prefab-expanded instances),
/// ready to feed [`derive_connections`] / [`encode`]. **Not** a file format any
/// more — it's assembled host-side by [`assemble`] from a [`Level`] manifest
/// entry + a [`Zone`] content file. A zone is one cell of the **Euclidean map**
/// (#27): it lives at `place` in the shared global frame and the baker derives
/// its connections from which zones abut it there — the author never writes
/// exits.
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
        Self {
            min: [-2.0, -2.0],
            max: [2.0, 2.0],
        }
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
        Camera::Follow {
            height: 1.7,
            dist: 2.0,
            pitch: -0.7,
        }
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

// --- RON authoring model (level / zone / prefab) -----------------------------

/// A **level** manifest (`level.ron`) — the authoring & distribution unit. It
/// owns the zone-graph *layout* (each zone's `place`/`bounds`/`camera`) in one
/// place; the per-zone *content* (instances) lives in sibling `<zone>.ron`
/// files. The map key is the zone's content filename stem (`"atrium"` ⇒
/// `atrium.ron`). `entry` names the zone the game boots into / the menu lands on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Level {
    /// Display name (for a future level-select menu).
    pub name: String,
    /// Zone the level starts in (must be a key of `zones`).
    pub entry: String,
    /// Zone graph: stem → its placement + framing. A `BTreeMap` so baking and
    /// constant emission are deterministic.
    pub zones: std::collections::BTreeMap<String, ZoneEntry>,
}

/// One zone's entry in a [`Level`] manifest: where it sits in the shared global
/// frame (`place`), its local walkable rect (`bounds`), and its camera framing.
/// The instances themselves live in the matching `<stem>.ron` ([`Zone`]).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ZoneEntry {
    #[serde(default)]
    pub place: [f32; 2],
    #[serde(default)]
    pub bounds: Bounds,
    #[serde(default)]
    pub camera: Camera,
}

/// A zone **content** file (`<zone>.ron`) — just the placed objects, as a single
/// ordered list of literal instances and prefab uses (`place`/`bounds`/`camera`
/// live up in the [`Level`] manifest, not here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    #[serde(default)]
    pub instances: Vec<Placement>,
}

/// One entry in a [`Zone`]'s instance list: either a literal instance authored
/// inline, or an instantiation of a named [`Prefab`] with a placement and
/// optional per-field overrides. Resolved to a flat [`Instance`] host-side by
/// [`resolve_placement`] — the DS never sees a prefab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Placement {
    /// A literal instance (the same shape as the old per-space format).
    Lit(Instance),
    /// Instantiate prefab `name` at `pos`; any `Some`/non-empty override field
    /// replaces the prefab's value.
    Use {
        name: String,
        #[serde(default)]
        pos: [f32; 3],
        #[serde(default)]
        rot: Option<[f32; 3]>,
        #[serde(default)]
        scale: Option<[f32; 3]>,
        #[serde(default)]
        material: Option<Material>,
        #[serde(default)]
        flags: Option<u32>,
        #[serde(default)]
        path: Vec<[f32; 2]>,
    },
}

impl Placement {
    /// The placement's position (`x`, height, `z`), in the zone's local frame —
    /// uniform across the `Lit`/`Use` split. Host tools that lay placements out
    /// (e.g. the desktop editor) read this without caring which variant it is.
    pub fn pos(&self) -> [f32; 3] {
        match self {
            Placement::Lit(i) => i.pos,
            Placement::Use { pos, .. } => *pos,
        }
    }

    /// Mutable access to the placement's position (see [`Placement::pos`]).
    pub fn pos_mut(&mut self) -> &mut [f32; 3] {
        match self {
            Placement::Lit(i) => &mut i.pos,
            Placement::Use { pos, .. } => pos,
        }
    }

    /// The placement's ground-plane waypoints. For a `Use`, this is the
    /// *override* path (empty ⇒ the prefab's default applies at bake), not the
    /// resolved one.
    pub fn path(&self) -> &[[f32; 2]] {
        match self {
            Placement::Lit(i) => &i.path,
            Placement::Use { path, .. } => path,
        }
    }

    /// Mutable access to the placement's waypoints (see [`Placement::path`]).
    pub fn path_mut(&mut self) -> &mut Vec<[f32; 2]> {
        match self {
            Placement::Lit(i) => &mut i.path,
            Placement::Use { path, .. } => path,
        }
    }
}

/// A reusable instance template (`assets/prefabs/<name>.ron`). The same fields
/// as an [`Instance`] minus `pos` (the placement supplies position). A [`Use`]
/// names one of these; [`resolve_placement`] expands it.
///
/// [`Use`]: Placement::Use
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefab {
    #[serde(default)]
    pub mesh: Option<String>,
    pub role: String,
    #[serde(default)]
    pub rot: [f32; 3],
    #[serde(default = "one3")]
    pub scale: [f32; 3],
    #[serde(default)]
    pub material: Option<Material>,
    #[serde(default)]
    pub flags: u32,
    /// Default ground-plane (XZ) waypoints; a [`Use`] with a non-empty `path`
    /// overrides these.
    ///
    /// [`Use`]: Placement::Use
    #[serde(default)]
    pub path: Vec<[f32; 2]>,
}

/// A named library of prefabs, keyed by file stem.
pub type PrefabLib = std::collections::BTreeMap<String, Prefab>;

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
pub fn derive_connections(
    zones: &[(String, Space)],
) -> std::collections::BTreeMap<String, Vec<Connection>> {
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
            push(
                SIDE_NORTH,
                (azmax - bzmin).abs() < TOL,
                xlo,
                xhi,
                a.place[0],
            );
            push(
                SIDE_SOUTH,
                (azmin - bzmax).abs() < TOL,
                xlo,
                xhi,
                a.place[0],
            );
        }
        out.insert(an.clone(), conns);
    }
    out
}

// --- Parse / serialise (level / zone / prefab) -------------------------------

fn parse_ron<T: serde::de::DeserializeOwned>(src: &str) -> Result<T, String> {
    ron::from_str(src).map_err(|e| format!("RON parse error: {e}"))
}

fn to_ron<T: Serialize>(value: &T) -> Result<String, String> {
    let cfg = ron::ser::PrettyConfig::new()
        .struct_names(true)
        .indentor("    ".to_string());
    ron::ser::to_string_pretty(value, cfg).map_err(|e| format!("RON serialize error: {e}"))
}

/// Parse a `level.ron` manifest.
pub fn parse_level_ron(src: &str) -> Result<Level, String> {
    parse_ron(src)
}

/// Parse a `<zone>.ron` content file.
pub fn parse_zone_ron(src: &str) -> Result<Zone, String> {
    parse_ron(src)
}

/// Parse a `<prefab>.ron` template.
pub fn parse_prefab_ron(src: &str) -> Result<Prefab, String> {
    parse_ron(src)
}

/// Serialise a level manifest to pretty RON (editor writer; round-trips through
/// [`parse_level_ron`]).
pub fn to_level_ron(level: &Level) -> Result<String, String> {
    to_ron(level)
}

/// Serialise a zone content file to pretty RON (round-trips through
/// [`parse_zone_ron`]).
pub fn to_zone_ron(zone: &Zone) -> Result<String, String> {
    to_ron(zone)
}

/// Serialise a prefab template to pretty RON (round-trips through
/// [`parse_prefab_ron`]).
pub fn to_prefab_ron(prefab: &Prefab) -> Result<String, String> {
    to_ron(prefab)
}

// --- Prefab resolution / assembly --------------------------------------------

/// Expand a [`Placement`] into a flat [`Instance`]. A [`Placement::Lit`] passes
/// through; a [`Placement::Use`] starts from the named prefab and applies the
/// placement's `pos` plus any override field. Errors if the prefab is unknown.
pub fn resolve_placement(p: &Placement, prefabs: &PrefabLib) -> Result<Instance, String> {
    match p {
        Placement::Lit(inst) => Ok(inst.clone()),
        Placement::Use {
            name,
            pos,
            rot,
            scale,
            material,
            flags,
            path,
        } => {
            let pf = prefabs
                .get(name)
                .ok_or_else(|| format!("unknown prefab `{name}`"))?;
            Ok(Instance {
                mesh: pf.mesh.clone(),
                role: pf.role.clone(),
                pos: *pos,
                rot: rot.unwrap_or(pf.rot),
                scale: scale.unwrap_or(pf.scale),
                material: material.or(pf.material),
                flags: flags.unwrap_or(pf.flags),
                path: if path.is_empty() {
                    pf.path.clone()
                } else {
                    path.clone()
                },
            })
        }
    }
}

/// Assemble a level's resolved zones: for each manifest entry, pair its
/// `place`/`bounds`/`camera` with the matching content file's prefab-expanded
/// instances. `zones` maps stem → parsed content; every manifest key must have
/// an entry. Returns `(stem, Space)` pairs in deterministic (manifest) order,
/// ready for [`derive_connections`] + [`encode`].
pub fn assemble(
    level: &Level,
    zones: &std::collections::BTreeMap<String, Zone>,
    prefabs: &PrefabLib,
) -> Result<Vec<(String, Space)>, String> {
    if !level.zones.contains_key(&level.entry) {
        return Err(format!(
            "manifest `entry` is `{}`, which is not one of the zones",
            level.entry
        ));
    }
    let mut out = Vec::with_capacity(level.zones.len());
    for (stem, entry) in &level.zones {
        let zone = zones.get(stem).ok_or_else(|| {
            format!("zone `{stem}` in the manifest has no `{stem}.ron` content file")
        })?;
        let mut instances = Vec::with_capacity(zone.instances.len());
        for (i, p) in zone.instances.iter().enumerate() {
            instances.push(
                resolve_placement(p, prefabs)
                    .map_err(|e| format!("zone `{stem}` instance {i}: {e}"))?,
            );
        }
        out.push((
            stem.clone(),
            Space {
                camera: entry.camera,
                place: entry.place,
                bounds: entry.bounds,
                instances,
            },
        ));
    }
    Ok(out)
}

/// Hard-validate a zone (errors that *guarantee* a broken ROM). `mesh_exists`
/// reports whether a referenced mesh has a source `.obj`.
pub fn validate(space: &Space, mesh_exists: impl Fn(&str) -> bool) -> Result<(), String> {
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
        Camera::Follow {
            height,
            dist,
            pitch,
        } => {
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
        Camera::Rail2_5D {
            height,
            dist,
            pitch,
        } => {
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

/// One compiled zone, returned by [`build_levels_dir`].
#[derive(Clone, Debug)]
pub struct Built {
    /// Source content `.ron` path (`<level>/<zone>.ron`).
    pub input: PathBuf,
    /// Destination `.scene` path (`<dst>/<level>/<zone>.scene`).
    pub output: PathBuf,
    /// Level name (directory stem) — groups the emitted constants.
    pub level: String,
    /// Zone stem (the generated constant name + NitroFS path leaf).
    pub stem: String,
    /// Non-fatal warnings raised while baking this zone.
    pub warnings: Vec<String>,
}

/// Load every `*.ron` in `prefab_dir` into a [`PrefabLib`] keyed by file stem.
/// A missing directory is fine (an empty library); a malformed prefab errors.
pub fn load_prefab_lib(prefab_dir: &Path) -> Result<PrefabLib, String> {
    let mut lib = PrefabLib::new();
    if !prefab_dir.is_dir() {
        return Ok(lib);
    }
    for path in read_dir_sorted(prefab_dir)? {
        if path.extension().and_then(|e| e.to_str()) != Some("ron") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad prefab file name: {}", path.display()))?
            .to_string();
        let src = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let prefab = parse_prefab_ron(&src).map_err(|e| format!("{}: {e}", path.display()))?;
        lib.insert(stem, prefab);
    }
    Ok(lib)
}

/// Bake every level directory under `levels_root` into
/// `<dst_root>/<level>/<zone>.scene`. A *level directory* is any immediate
/// subdirectory containing a [`MANIFEST_NAME`] manifest. `assets_dir` is the
/// geometry root (`assets/`) used to validate referenced meshes have a source
/// `.obj`; `prefab_dir` holds the shared [`Prefab`] library. Returns the
/// compiled zones (with any warnings) so a `build.rs` can emit
/// `rerun-if-changed` + `cargo:warning=` lines.
pub fn build_levels_dir(
    levels_root: &Path,
    dst_root: &Path,
    assets_dir: &Path,
    prefab_dir: &Path,
) -> Result<Vec<Built>, String> {
    let prefabs = load_prefab_lib(prefab_dir)?;
    let mesh_exists = |name: &str| assets_dir.join(format!("{name}.obj")).is_file();

    let mut built = Vec::new();
    for level_dir in read_dir_sorted(levels_root)? {
        let manifest_path = level_dir.join(MANIFEST_NAME);
        if !manifest_path.is_file() {
            continue; // not a level directory
        }
        let level_name = level_dir
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad level dir name: {}", level_dir.display()))?
            .to_string();

        let manifest_src = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("could not read {}: {e}", manifest_path.display()))?;
        let level = parse_level_ron(&manifest_src)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;

        // Load each zone's content file named by the manifest.
        let mut zone_contents = std::collections::BTreeMap::new();
        for stem in level.zones.keys() {
            let content_path = level_dir.join(format!("{stem}.ron"));
            let src = std::fs::read_to_string(&content_path)
                .map_err(|e| format!("could not read {}: {e}", content_path.display()))?;
            let zone =
                parse_zone_ron(&src).map_err(|e| format!("{}: {e}", content_path.display()))?;
            zone_contents.insert(stem.clone(), zone);
        }

        // Resolve prefabs → Space intermediates, then derive connections over
        // the whole level at once (each zone's neighbours need the global layout).
        let zones = assemble(&level, &zone_contents, &prefabs)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        for (stem, space) in &zones {
            validate(space, mesh_exists)
                .map_err(|e| format!("{} zone `{stem}`: {e}", manifest_path.display()))?;
        }

        let conns = derive_connections(&zones);
        let warns = isolation_warnings(&conns);

        for (stem, space) in &zones {
            let zone_conns = conns.get(stem).map(Vec::as_slice).unwrap_or(&[]);
            let output = dst_root
                .join(&level_name)
                .join(format!("{stem}.{ASSET_EXT}"));
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
            }
            std::fs::write(&output, encode(space, zone_conns))
                .map_err(|e| format!("could not write {}: {e}", output.display()))?;

            built.push(Built {
                input: level_dir.join(format!("{stem}.ron")),
                output,
                level: level_name.clone(),
                stem: stem.clone(),
                warnings: warns.get(stem).cloned().unwrap_or_default(),
            });
        }
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

/// Emit a `levels.rs` module of NUL-terminated NitroFS-path constants — one per
/// baked zone, grouped into a per-level submodule — for the game to `include!`
/// (mirrors `wav2bank`'s `sounds.rs`). A zone bakes to
/// `levels::<level>::<ZONE>` ⇒ `b"nitro:/levels/<level>/<zone>.scene\0"`.
pub fn emit_rust_consts(built: &[Built]) -> String {
    // Group by level (preserving the deterministic order Built arrives in).
    let mut by_level: std::collections::BTreeMap<String, Vec<&Built>> =
        std::collections::BTreeMap::new();
    for b in built {
        by_level.entry(b.level.clone()).or_default().push(b);
    }

    let mut s = String::new();
    s.push_str("// @generated by scene2bin from assets/levels/<name>/.\n");
    s.push_str("// Each constant is a NUL-terminated NitroFS path you can pass to\n");
    s.push_str("// `bevy_nds_scene::load` (or `LoadSpace { path }`).\n");
    for (level, zones) in &by_level {
        s.push_str(&format!(
            "pub mod {} {{\n",
            const_name(level).to_ascii_lowercase()
        ));
        for b in zones {
            s.push_str("    pub const ");
            s.push_str(&const_name(&b.stem));
            s.push_str(": &[u8] = b\"");
            s.push_str(&format!(
                "nitro:/{NITROFS_SUBDIR}/{}/{}.{ASSET_EXT}",
                b.level, b.stem
            ));
            s.push_str("\\0\";\n");
        }
        s.push_str("}\n");
    }
    s
}

/// Emit the `levels.rs` constants module from just the source tree's directory
/// layout, without parsing — the fallback `build.rs` uses when baking errors
/// out, so the game's `include!` always resolves (the zone simply won't load at
/// runtime). A level dir's zones are its `*.ron` files other than the manifest.
/// Mirrors `wav2bank::predict_ids`.
pub fn predict_consts(levels_root: &Path) -> String {
    let mut built: Vec<Built> = Vec::new();
    for level_dir in read_dir_sorted(levels_root).unwrap_or_default() {
        if !level_dir.join(MANIFEST_NAME).is_file() {
            continue;
        }
        let Some(level) = level_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(String::from)
        else {
            continue;
        };
        for path in read_dir_sorted(&level_dir).unwrap_or_default() {
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            if path.file_name().and_then(|s| s.to_str()) == Some(MANIFEST_NAME) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            built.push(Built {
                output: path.with_extension(ASSET_EXT),
                input: path,
                level: level.clone(),
                stem,
                warnings: Vec::new(),
            });
        }
    }
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
    use std::collections::BTreeMap;

    const MANIFEST: &str = r#"
        Level(
            name: "Facility",
            entry: "atrium",
            zones: {
                "atrium":   (place: (0.0, 0.0), bounds: (min: (-2.0, -2.0), max: (2.0, 2.0)), camera: Follow(height: 1.7, dist: 2.0, pitch: -0.7)),
                "corridor": (place: (4.2, 0.0), bounds: (min: (-2.2, -0.55), max: (2.2, 0.55)), camera: Rail2_5D(height: 1.4, dist: 2.4, pitch: -0.35)),
            },
        )
    "#;

    const ATRIUM_ZONE: &str = r#"
        Zone(instances: [
            Lit(Instance(
                mesh: Some("teapot"),
                role: "avatar",
                rot: (-1.5708, 0.0, 0.0),
                scale: (0.11, 0.11, 0.11),
                material: Some((diffuse: (110, 180, 235), ambient: (26, 40, 58))),
            )),
            Use(name: "patroller", pos: (-1.4, 0.0, 0.0), path: [(-1.4, 0.0), (1.4, 0.0)]),
            Use(name: "landmark_block", pos: (-1.25, 0.0, 0.95)),
            Use(name: "landmark_block", pos: (1.25, 0.0, -0.95), rot: Some((0.0, 0.3, 0.0))),
        ])
    "#;

    const PATROLLER: &str = r#"Prefab(mesh: Some("cube"), role: "enemy", scale: (0.16, 0.16, 0.16), material: Some((diffuse: (225, 80, 70), ambient: (56, 20, 18))))"#;
    const LANDMARK: &str = r#"Prefab(mesh: Some("cube"), role: "landmark", scale: (0.16, 0.16, 0.16), material: Some((diffuse: (120, 120, 138), ambient: (34, 34, 44))))"#;

    fn prefabs() -> PrefabLib {
        PrefabLib::from([
            (
                "patroller".to_string(),
                parse_prefab_ron(PATROLLER).unwrap(),
            ),
            (
                "landmark_block".to_string(),
                parse_prefab_ron(LANDMARK).unwrap(),
            ),
        ])
    }

    fn zone(place: [f32; 2], min: [f32; 2], max: [f32; 2]) -> Space {
        Space {
            camera: Camera::default(),
            place,
            bounds: Bounds { min, max },
            instances: Vec::new(),
        }
    }

    #[test]
    fn lit_placement_passes_through() {
        let zone = parse_zone_ron(ATRIUM_ZONE).unwrap();
        let inst = resolve_placement(&zone.instances[0], &prefabs()).unwrap();
        assert_eq!(inst.role, "avatar");
        assert_eq!(inst.mesh.as_deref(), Some("teapot"));
        assert!(inst.material.is_some());
    }

    #[test]
    fn use_expands_prefab_with_overrides() {
        let zone = parse_zone_ron(ATRIUM_ZONE).unwrap();
        let lib = prefabs();

        // `patroller` use: prefab role/mesh/scale/material, placement pos + path.
        let patrol = resolve_placement(&zone.instances[1], &lib).unwrap();
        assert_eq!(patrol.role, "enemy");
        assert_eq!(patrol.mesh.as_deref(), Some("cube"));
        assert_eq!(patrol.scale, [0.16, 0.16, 0.16]); // from prefab
        assert_eq!(patrol.pos, [-1.4, 0.0, 0.0]); // from placement
        assert_eq!(patrol.path, std::vec![[-1.4, 0.0], [1.4, 0.0]]); // override
        assert_eq!(patrol.rot, [0.0, 0.0, 0.0]); // prefab default (no override)

        // `landmark_block` use with a rot override.
        let lm = resolve_placement(&zone.instances[3], &lib).unwrap();
        assert_eq!(lm.role, "landmark");
        assert_eq!(lm.rot, [0.0, 0.3, 0.0]); // override applied
    }

    #[test]
    fn unknown_prefab_errors() {
        let zone: Zone =
            parse_zone_ron(r#"Zone(instances: [Use(name: "ghost", pos: (0,0,0))])"#).unwrap();
        let err = resolve_placement(&zone.instances[0], &prefabs()).unwrap_err();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn assemble_combines_manifest_layout_with_zone_content() {
        let level = parse_level_ron(MANIFEST).unwrap();
        let zones = BTreeMap::from([
            ("atrium".to_string(), parse_zone_ron(ATRIUM_ZONE).unwrap()),
            (
                "corridor".to_string(),
                parse_zone_ron("Zone(instances: [])").unwrap(),
            ),
        ]);
        let assembled = assemble(&level, &zones, &prefabs()).unwrap();

        let atrium = &assembled.iter().find(|(s, _)| s == "atrium").unwrap().1;
        // Layout came from the manifest…
        assert!(matches!(atrium.camera, Camera::Follow { .. }));
        assert_eq!(atrium.place, [0.0, 0.0]);
        assert_eq!(atrium.bounds.max, [2.0, 2.0]);
        // …content (4 placements) resolved to 4 flat instances.
        assert_eq!(atrium.instances.len(), 4);
        let corridor = &assembled.iter().find(|(s, _)| s == "corridor").unwrap().1;
        assert!(matches!(corridor.camera, Camera::Rail2_5D { .. }));
    }

    #[test]
    fn assemble_rejects_missing_content_or_bad_entry() {
        let level = parse_level_ron(MANIFEST).unwrap();
        // Manifest names `corridor` but only `atrium` content supplied.
        let only_atrium =
            BTreeMap::from([("atrium".to_string(), parse_zone_ron(ATRIUM_ZONE).unwrap())]);
        assert!(assemble(&level, &only_atrium, &prefabs()).is_err());

        let bad_entry: Level =
            parse_level_ron(r#"Level(name: "X", entry: "nope", zones: {})"#).unwrap();
        assert!(assemble(&bad_entry, &BTreeMap::new(), &prefabs()).is_err());
    }

    #[test]
    fn assembled_zone_encodes_to_expected_header() {
        let level = parse_level_ron(MANIFEST).unwrap();
        let zones = BTreeMap::from([
            ("atrium".to_string(), parse_zone_ron(ATRIUM_ZONE).unwrap()),
            (
                "corridor".to_string(),
                parse_zone_ron("Zone(instances: [])").unwrap(),
            ),
        ]);
        let assembled = assemble(&level, &zones, &prefabs()).unwrap();
        let atrium = &assembled.iter().find(|(s, _)| s == "atrium").unwrap().1;
        let blob = encode(atrium, &[]);
        assert_eq!(&blob[0..4], b"BSC1");
        assert_eq!(u16::from_le_bytes([blob[4], blob[5]]), VERSION);
        assert_eq!(u16::from_le_bytes([blob[6], blob[7]]), 0); // Follow
    }

    #[test]
    fn validate_rejects_missing_mesh() {
        let level = parse_level_ron(MANIFEST).unwrap();
        let zones = BTreeMap::from([
            ("atrium".to_string(), parse_zone_ron(ATRIUM_ZONE).unwrap()),
            (
                "corridor".to_string(),
                parse_zone_ron("Zone(instances: [])").unwrap(),
            ),
        ]);
        let atrium = assemble(&level, &zones, &prefabs())
            .unwrap()
            .into_iter()
            .find(|(s, _)| s == "atrium")
            .unwrap()
            .1;
        // No mesh exists at all → the first meshed instance fails.
        let err = validate(&atrium, |_| false).unwrap_err();
        assert!(err.contains("teapot") || err.contains("cube"), "{err}");
        // All meshes present → ok.
        assert!(validate(&atrium, |_| true).is_ok());
    }

    #[test]
    fn derives_connection_between_abutting_zones() {
        // The facility layout: atrium (±2 pad) at the origin; corridor placed
        // east so its west edge (local -2.2 + place 4.2 = 2.0) meets the
        // atrium's east edge (2.0). Drive it from the parsed manifest entries.
        let level = parse_level_ron(MANIFEST).unwrap();
        let zones: Vec<(String, Space)> = level
            .zones
            .iter()
            .map(|(stem, e)| {
                (
                    stem.clone(),
                    Space {
                        camera: e.camera,
                        place: e.place,
                        bounds: e.bounds,
                        instances: Vec::new(),
                    },
                )
            })
            .collect();
        let conns = derive_connections(&zones);

        let a = &conns["atrium"];
        assert_eq!(a.len(), 1, "atrium should connect to exactly the corridor");
        assert_eq!(a[0].neighbour, "corridor");
        assert_eq!(a[0].side, SIDE_EAST);
        // Crossing east adds (place_atrium - place_corridor): (0 - 4.2, 0).
        assert!(
            (a[0].delta[0] - (-4.2)).abs() < 1e-4,
            "delta {:?}",
            a[0].delta
        );
        assert!(a[0].delta[1].abs() < 1e-4);
        assert!((a[0].lo - (-0.55)).abs() < 1e-4 && (a[0].hi - 0.55).abs() < 1e-4);

        let c = &conns["corridor"];
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].side, SIDE_WEST);
        assert!((c[0].delta[0] - 4.2).abs() < 1e-4, "delta {:?}", c[0].delta);
    }

    #[test]
    fn isolated_zone_warns_only_in_a_multi_zone_map() {
        let zones = std::vec![
            ("a".to_string(), zone([0.0, 0.0], [-1.0, -1.0], [1.0, 1.0])),
            (
                "b".to_string(),
                zone([100.0, 0.0], [-1.0, -1.0], [1.0, 1.0])
            ),
        ];
        let warns = isolation_warnings(&derive_connections(&zones));
        assert_eq!(warns.len(), 2);
        let lone = std::vec![(
            "solo".to_string(),
            zone([0.0, 0.0], [-1.0, -1.0], [1.0, 1.0])
        )];
        assert!(isolation_warnings(&derive_connections(&lone)).is_empty());
    }

    #[test]
    fn validate_rejects_degenerate_bounds() {
        let mut space = zone([0.0, 0.0], [-2.0, -2.0], [2.0, 2.0]);
        space.bounds = Bounds {
            min: [2.0, -2.0],
            max: [-2.0, 2.0],
        }; // min.x >= max.x
        assert!(validate(&space, |_| true).is_err());
    }

    #[test]
    fn ron_round_trips_through_parse() {
        // The editor saves via the `to_*_ron` writers; they must parse back.
        let level = parse_level_ron(MANIFEST).unwrap();
        let zone = parse_zone_ron(ATRIUM_ZONE).unwrap();
        let prefab = parse_prefab_ron(PATROLLER).unwrap();

        let level2 = parse_level_ron(&to_level_ron(&level).unwrap()).unwrap();
        let zone2 = parse_zone_ron(&to_zone_ron(&zone).unwrap()).unwrap();
        let prefab2 = parse_prefab_ron(&to_prefab_ron(&prefab).unwrap()).unwrap();

        // Compare via assembled-encode (covers every field without PartialEq).
        let zones = BTreeMap::from([
            ("atrium".to_string(), zone),
            (
                "corridor".to_string(),
                parse_zone_ron("Zone(instances: [])").unwrap(),
            ),
        ]);
        let zones2 = BTreeMap::from([
            ("atrium".to_string(), zone2),
            (
                "corridor".to_string(),
                parse_zone_ron("Zone(instances: [])").unwrap(),
            ),
        ]);
        let enc = |lv: &Level, zs| {
            let a = assemble(lv, zs, &prefabs()).unwrap();
            encode(&a.iter().find(|(s, _)| s == "atrium").unwrap().1, &[])
        };
        assert_eq!(enc(&level, &zones), enc(&level2, &zones2));
        assert_eq!(prefab.role, prefab2.role);
        assert_eq!(prefab.scale, prefab2.scale);
    }

    #[test]
    fn emit_consts_nests_per_level() {
        let built = std::vec![
            Built {
                input: PathBuf::from("assets/levels/facility/atrium.ron"),
                output: PathBuf::from("build/nitrofs/levels/facility/atrium.scene"),
                level: "facility".to_string(),
                stem: "atrium".to_string(),
                warnings: Vec::new(),
            },
            Built {
                input: PathBuf::from("assets/levels/facility/corridor.ron"),
                output: PathBuf::from("build/nitrofs/levels/facility/corridor.scene"),
                level: "facility".to_string(),
                stem: "corridor".to_string(),
                warnings: Vec::new(),
            },
        ];
        let rs = emit_rust_consts(&built);
        assert!(rs.contains("pub mod facility {"), "{rs}");
        assert!(
            rs.contains(r#"pub const ATRIUM: &[u8] = b"nitro:/levels/facility/atrium.scene\0";"#),
            "{rs}"
        );
        assert!(rs.contains("CORRIDOR"), "{rs}");
    }

    #[test]
    fn const_name_uppercases_and_guards_digits() {
        assert_eq!(const_name("corridor_b"), "CORRIDOR_B");
        assert_eq!(const_name("atrium"), "ATRIUM");
        assert_eq!(const_name("2nd-floor"), "_2ND_FLOOR");
    }
}
