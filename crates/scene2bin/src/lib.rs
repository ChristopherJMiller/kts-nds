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

/// The shared authored vocabulary (roles, role-scoped kinds, instance-flag bits,
/// reserved flag ids), re-exported so every consumer of the authoring format
/// reaches it through one edge: the game depends on `kts_schema` directly, and
/// the detached desktop editor reads it as `scene2bin::schema` rather than
/// adding a dependency of its own.
pub use kts_schema as schema;

use kts_schema::{Consumption, Role, flag_bits, flag_ids, kind_from_str};

pub use model2dsm::catalog::{Catalog, MeshEntry, TexUse};

/// Every mesh name under `assets_dir` (legacy `assets/*.obj` + `assets/models/**`)
/// without loading any model — for the editor's mesh picker (#66).
pub fn mesh_names(assets_dir: &Path) -> Vec<String> {
    model2dsm::catalog::mesh_names(assets_dir)
}

/// ASCII `"BSC1"` — magic prefix of a baked `.scene` file. Matches
/// `bevy_nds_scene::asset::MAGIC`.
pub const ASSET_MAGIC: u32 = u32::from_le_bytes(*b"BSC1");
/// `.scene` format version. Matches `bevy_nds_scene::asset::VERSION`. v2 replaced
/// hand-authored `exits` with a zone `bounds` + baker-**derived** `connections`
/// (the Euclidean map rework, #27). v3 added the zone `clear_flag` (the generalized
/// gating model, #27) — a `u32` after `bounds`. v4 adds a per-instance `u8 kind`
/// (role-scoped sub-archetype, #27/#29) after `flags`.
pub const VERSION: u16 = 4;
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
    /// Nonzero ⇒ this zone is a **gating arena**: clearing its objective enemies
    /// raises this flag (the runtime's `Flags`, #27). `0` ⇒ a **freeform** zone
    /// that gates nothing. Baked into the `.scene` blob (v3).
    #[serde(default)]
    pub clear_flag: u32,
    /// Directed gates on this zone's derived connections: a connection leaving
    /// toward the named neighbour requires the given flag (#27). Consumed by
    /// [`derive_connections`] into each [`Connection::gate`] — **not** baked as a
    /// separate field (the `gate` already rides the connection).
    #[serde(default)]
    pub gates: Vec<Gate>,
    /// Placed objects (geometry + role), in local coordinates.
    #[serde(default)]
    pub instances: Vec<Instance>,
}

/// A directed gate on a derived connection (#27): crossing this zone's boundary
/// **toward** `neighbour` requires `flag` to be raised at runtime. Authored on
/// the source zone (manifest [`ZoneEntry`]); [`derive_connections`] copies `flag`
/// onto the matching [`Connection::gate`]. A neighbour with no gate entry is
/// always open (`gate == 0`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gate {
    /// Stem of the neighbour zone the gated connection leads to.
    pub neighbour: String,
    /// Flag id that must be raised to cross (matches the arena's `clear_flag`,
    /// or a future switch/key/score source).
    pub flag: u32,
}

/// A rectangle on the ground (XZ) plane, in a zone's **local** coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    /// Bare mesh name (`"teapot"` ⇒ `nitro:/teapot.dl`); omit for a
    /// transform-only marker (spawn point, logical node).
    #[serde(default)]
    pub mesh: Option<String>,
    /// Game-defined role tag the runtime keeps opaque.
    pub role: String,
    /// Role-scoped sub-archetype name (one of [`Role::kinds`]); `None` = the
    /// role's default (wire `0`). Resolved to a `u8` at bake — the blob never
    /// carries the name.
    #[serde(default)]
    pub kind: Option<String>,
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
/// frame (`place`), its local walkable rect (`bounds`), its camera framing, and
/// its gating (`clear_flag` + `gates`, #27). The instances themselves live in the
/// matching `<stem>.ron` ([`Zone`]). No longer `Copy` — `gates: Vec<Gate>` — but
/// `PartialEq` so the editor can diff it for undo (#43).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneEntry {
    #[serde(default)]
    pub place: [f32; 2],
    #[serde(default)]
    pub bounds: Bounds,
    #[serde(default)]
    pub camera: Camera,
    /// Nonzero ⇒ gating arena raising this flag on clear; `0` ⇒ freeform (#27).
    #[serde(default)]
    pub clear_flag: u32,
    /// Directed gates on this zone's exits (#27). See [`Gate`].
    #[serde(default)]
    pub gates: Vec<Gate>,
}

/// A zone **content** file (`<zone>.ron`) — just the placed objects, as a single
/// ordered list of literal instances and prefab uses (`place`/`bounds`/`camera`
/// live up in the [`Level`] manifest, not here).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    #[serde(default)]
    pub instances: Vec<Placement>,
}

/// One entry in a [`Zone`]'s instance list: either a literal instance authored
/// inline, or an instantiation of a named [`Prefab`] with a placement and
/// optional per-field overrides. Resolved to a flat [`Instance`] host-side by
/// [`resolve_placement`] — the DS never sees a prefab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Role-scoped sub-archetype name (one of [`Role::kinds`]); `None` = the
    /// role's default (wire `0`). Prefab-owned: a [`Use`] can override `flags`
    /// but never `kind`, exactly like `role` (the deliberately minimal override
    /// surface, #27) — mixed encounters are authored by placing different
    /// prefabs.
    ///
    /// [`Use`]: Placement::Use
    #[serde(default)]
    pub kind: Option<String>,
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
            // A gate authored on A toward this neighbour locks the crossing until
            // its `flag` is raised (#27); absent ⇒ always open (`gate == 0`).
            let gate = a
                .gates
                .iter()
                .find(|g| &g.neighbour == bn)
                .map_or(0, |g| g.flag);
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
                        gate,
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
                // Prefab-owned like `role` — a `Use` carries no kind override.
                kind: pf.kind.clone(),
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
                clear_flag: entry.clear_flag,
                gates: entry.gates.clone(),
                instances,
            },
        ));
    }
    Ok(out)
}

// --- Validation ---------------------------------------------------------------

/// How badly an [`Issue`] breaks the bake. An `Error` fails it; a `Warning` is
/// surfaced (`cargo:warning=`, the editor's problems panel) and baked anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// One validation finding, scoped as precisely as the rule allows: `zone` names
/// the zone stem (`None` for a whole-level rule) and `instance` its index within
/// that zone's instance list (`None` for a zone- or level-wide rule). The editor
/// turns the pair into click-to-focus; the bake turns it into a message naming
/// exactly where to look.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub zone: Option<String>,
    pub instance: Option<usize>,
    pub severity: Severity,
    pub msg: String,
}

impl Issue {
    fn error(zone: &str, instance: Option<usize>, msg: String) -> Self {
        Self {
            zone: Some(zone.to_string()),
            instance,
            severity: Severity::Error,
            msg,
        }
    }

    fn warning(zone: &str, instance: Option<usize>, msg: String) -> Self {
        Self {
            zone: Some(zone.to_string()),
            instance,
            severity: Severity::Warning,
            msg,
        }
    }

    /// `"atrium#2"` / `"atrium"` / `"(level)"` — the scope prefix used by the CLI
    /// and by the bake's aggregated error message.
    pub fn scope(&self) -> String {
        match (&self.zone, self.instance) {
            (Some(z), Some(i)) => format!("{z}#{i}"),
            (Some(z), None) => z.clone(),
            (None, _) => "(level)".to_string(),
        }
    }
}

/// The comma-joined list of every valid role name, for error messages.
fn role_names() -> String {
    Role::ALL
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// **The** validator (#27): every authoring rule, instance-, zone- and
/// level-scoped, in one non-short-circuiting pass. Returns *all* findings so an
/// author fixes a level in one round trip instead of one error per bake; the
/// caller decides what a [`Severity::Error`] means (the bake fails, the editor
/// refuses to save, `--check` exits non-zero).
///
/// `zones` is the assembled level (see [`assemble`]); `mesh_exists` reports
/// whether a referenced mesh exists (existence only — the texture budget needs
/// [`validate_all_with_catalog`]).
///
/// Later work appends rules here rather than adding a second validator.
pub fn validate_all(
    level: &Level,
    zones: &[(String, Space)],
    mesh_exists: impl Fn(&str) -> bool,
) -> Vec<Issue> {
    validate_with(level, zones, &ExistsOnly(mesh_exists))
}

/// [`validate_all`] plus the rules that need to know what meshes *contain* — the
/// per-level texture budget (#66). The bake and `--check` use this.
pub fn validate_all_with_catalog(level: &Level, zones: &[(String, Space)], catalog: &Catalog) -> Vec<Issue> {
    validate_with(level, zones, catalog)
}

fn validate_with(level: &Level, zones: &[(String, Space)], meshes: &impl MeshLookup) -> Vec<Issue> {
    let mut out = Vec::new();
    for (stem, space) in zones {
        validate_zone(stem, space, meshes, &mut out);
    }
    validate_level(level, zones, &mut out);
    validate_textures(zones, meshes, &mut out);
    out
}

/// What the validator needs to know about mesh names. Private: callers pick an
/// entry point — [`validate_all`] (existence only) or
/// [`validate_all_with_catalog`] (existence + texture costs).
trait MeshLookup {
    fn exists(&self, mesh: &str) -> bool;
    fn textures(&self, mesh: &str) -> &[TexUse];
}

/// An existence-only lookup over a plain predicate (the editor's and the tests' form).
struct ExistsOnly<F>(F);

impl<F: Fn(&str) -> bool> MeshLookup for ExistsOnly<F> {
    fn exists(&self, mesh: &str) -> bool {
        (self.0)(mesh)
    }
    fn textures(&self, _mesh: &str) -> &[TexUse] {
        &[]
    }
}

impl MeshLookup for Catalog {
    fn exists(&self, mesh: &str) -> bool {
        self.contains(mesh)
    }
    fn textures(&self, mesh: &str) -> &[TexUse] {
        Catalog::textures(self, mesh)
    }
}

/// Instance- and zone-scoped rules for a single zone. Private so [`validate_all`]
/// stays the only entry point that sees a whole level; [`validate`] wraps it for
/// the first-error, single-zone callers.
fn validate_zone(
    stem: &str,
    space: &Space,
    meshes: &impl MeshLookup,
    out: &mut Vec<Issue>,
) {
    // Per-role tally of Scenery-consumption instances, aggregated into one
    // Warning at the end (never per instance — a gray-boxed zone would drown the
    // panel in noise). Since the collide item promoted `block` to Gameplay
    // (#12, 2026-09-18), `prop` is the only role that can land here.
    let mut scenery: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();

    for (i, inst) in space.instances.iter().enumerate() {
        if inst.role.trim().is_empty() {
            out.push(Issue::error(stem, Some(i), "empty `role`".to_string()));
            continue;
        }
        let Some(role) = Role::parse(&inst.role) else {
            out.push(Issue::error(
                stem,
                Some(i),
                format!(
                    "unknown role `{}` — valid roles: {}",
                    inst.role,
                    role_names()
                ),
            ));
            continue;
        };

        // Role-scoped sub-archetype.
        if let Some(kind) = &inst.kind {
            let kinds = role.kinds();
            if kinds.is_empty() {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "role `{}` has no kinds, but `kind: \"{kind}\"` is authored",
                        role.as_str()
                    ),
                ));
            } else if kind_from_str(role, kind).is_none() {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "unknown kind `{kind}` for role `{}` — valid kinds: {}",
                        role.as_str(),
                        kinds.join(", ")
                    ),
                ));
            }
        }

        // Instance flags: undefined bits, then role-incompatible ones.
        let unknown = flag_bits::unknown_bits(inst.flags);
        if unknown != 0 {
            out.push(Issue::error(
                stem,
                Some(i),
                format!(
                    "flags {:#x} sets undefined bit(s) {unknown:#x} — defined bits: {}",
                    inst.flags,
                    flag_bit_names()
                ),
            ));
        }
        let disallowed = inst.flags & !unknown & !role.allowed_flags();
        if disallowed != 0 {
            out.push(Issue::error(
                stem,
                Some(i),
                format!(
                    "flags {:#x} sets {} on role `{}`, which allows {}",
                    inst.flags,
                    named_bits(disallowed),
                    role.as_str(),
                    if role.allowed_flags() == 0 {
                        "none".to_string()
                    } else {
                        named_bits(role.allowed_flags())
                    }
                ),
            ));
        }

        // Solid roles (#12): a `landmark` or `block` is blocking geometry, and
        // its collider is *derived*, never authored — from the mesh's baked
        // AABB × the instance scale, turned by `rot.y` alone. All three facts
        // have a hard authoring consequence, so all three are Errors rather than
        // warnings: a meshless solid would silently block nothing, a tilted one
        // would render at an angle its collider can't represent, and a
        // non-positively-scaled one derives degenerate or inverted extents.
        if matches!(role, Role::Landmark | Role::Block) {
            if inst.mesh.is_none() {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "role `{}` is solid and needs a mesh — its collider is derived from the mesh's baked AABB × scale",
                        role.as_str()
                    ),
                ));
            }
            if inst.rot[0] != 0.0 || inst.rot[2] != 0.0 {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "solid role `{}` is yaw-only — zero rot.x/rot.z (or use role `prop` for a tilted decoration)",
                        role.as_str()
                    ),
                ));
            }
            // Non-positive scale. A zero component collapses the derived
            // collider to nothing on that axis (it renders, but blocks a plane
            // of zero thickness); a negative one *mirrors* the mesh, and the
            // runtime's half extents would go negative with it. The runtime
            // takes the magnitude so the ROM can't abort on old content, but
            // mirroring a solid is not an authoring move we support: the yawed
            // footprint and a ramp's rise direction stop matching what is drawn.
            // Turn it with `rot.y` instead.
            if let Some(k) = inst.scale.iter().position(|v| !v.is_finite() || *v <= 0.0) {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "solid role `{}` has {} = {} — a solid's collider is derived from its mesh AABB × scale, so every scale component must be > 0 and finite (turn it with `rot.y`; mirror a decoration with role `prop`)",
                        role.as_str(),
                        ["scale.x", "scale.y", "scale.z"][k],
                        inst.scale[k]
                    ),
                ));
            }
        }

        if let Some(mesh) = &inst.mesh {
            if !meshes.exists(mesh) {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "(role `{}`): mesh `{mesh}` has no source model — expected `assets/{mesh}.obj` or `assets/models/{mesh}.obj` / `.gltf` / `.glb`",
                        inst.role
                    ),
                ));
            }
        }

        if role.consumption() == Consumption::Scenery {
            *scenery.entry(role.as_str()).or_default() += 1;
        }
    }

    let b = &space.bounds;
    if b.min[0] >= b.max[0] || b.min[1] >= b.max[1] {
        out.push(Issue::error(
            stem,
            None,
            format!(
                "bounds min {:?} must be strictly less than max {:?} on both axes",
                b.min, b.max
            ),
        ));
    }

    if !scenery.is_empty() {
        let total: usize = scenery.values().sum();
        let by_role = scenery
            .iter()
            .map(|(role, n)| format!("{role} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Issue::warning(
            stem,
            None,
            format!("{total} instances render but carry no runtime behaviour yet: {by_role}"),
        ));
    }
}

/// Level-scoped rules — the invariants no single zone can see (#27 / #54).
fn validate_level(level: &Level, zones: &[(String, Space)], out: &mut Vec<Issue>) {
    // Exactly one avatar in the whole level, in the `entry` zone. This enforces
    // the entry-zone-authors-the-avatar rule — an AMENDMENT to #27's 2026-06-28
    // Locked line ("zones no longer author an avatar instance"), still PENDING
    // design-sync. Until that lands, the code is ahead of the design record.
    let mut avatars: Vec<(&str, usize)> = Vec::new();
    for (stem, space) in zones {
        for (i, inst) in space.instances.iter().enumerate() {
            if Role::parse(&inst.role) == Some(Role::Avatar) {
                avatars.push((stem.as_str(), i));
            }
        }
    }
    match avatars.len() {
        0 => out.push(Issue {
            zone: None,
            instance: None,
            severity: Severity::Error,
            msg: format!(
                "no `avatar` instance — the entry zone `{}` must author exactly one",
                level.entry
            ),
        }),
        1 => {
            let (stem, i) = avatars[0];
            if stem != level.entry {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "`avatar` is authored in zone `{stem}`, but only the entry zone `{}` may author one",
                        level.entry
                    ),
                ));
            }
        }
        n => {
            out.push(Issue {
                zone: None,
                instance: None,
                severity: Severity::Error,
                msg: format!(
                    "{n} `avatar` instances ({}) — a level has exactly one, in its entry zone `{}`",
                    avatars
                        .iter()
                        .map(|(z, i)| format!("{z}#{i}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    level.entry
                ),
            });
        }
    }

    for (stem, space) in zones {
        // A gate naming a zone that isn't in the manifest silently becomes a
        // permanently closed crossing in `derive_connections` — catch it here.
        for g in &space.gates {
            if !level.zones.contains_key(&g.neighbour) {
                out.push(Issue::error(
                    stem,
                    None,
                    format!(
                        "gate toward `{}` names no zone in this level (zones: {})",
                        g.neighbour,
                        level.zones.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
            if flag_ids::is_reserved(g.flag) {
                out.push(Issue::error(
                    stem,
                    None,
                    format!(
                        "gate toward `{}` requires flag {:#x}, which is in the engine-reserved range (>= {:#x})",
                        g.neighbour,
                        g.flag,
                        flag_ids::RESERVED_MIN
                    ),
                ));
            }
        }
        if flag_ids::is_reserved(space.clear_flag) {
            out.push(Issue::error(
                stem,
                None,
                format!(
                    "clear_flag {:#x} is in the engine-reserved range (>= {:#x})",
                    space.clear_flag,
                    flag_ids::RESERVED_MIN
                ),
            ));
        }
        // A gating arena with nothing to clear never raises its flag — the gate
        // it guards stays shut forever. A Warning, not an Error: the zone may be
        // mid-authoring.
        if space.clear_flag != 0
            && !space
                .instances
                .iter()
                .any(|i| i.flags & flag_bits::OBJECTIVE != 0)
        {
            out.push(Issue::warning(
                stem,
                None,
                format!(
                    "clear_flag {:#x} is set but no instance carries OBJECTIVE — nothing can raise it (soft-lock)",
                    space.clear_flag
                ),
            ));
        }
    }
}

/// Level-scope texture budget (#66): a level's whole texture set is resident
/// from boot, so every distinct texture any instance's mesh uses must fit,
/// together, in the texture VRAM (banks B + D) and the palette VRAM (bank F).
fn validate_textures(zones: &[(String, Space)], meshes: &impl MeshLookup, out: &mut Vec<Issue>) {
    let mut seen: std::collections::BTreeMap<&str, &TexUse> = std::collections::BTreeMap::new();
    for (_, space) in zones {
        for inst in &space.instances {
            if let Some(m) = &inst.mesh {
                for t in meshes.textures(m) {
                    seen.entry(t.key.as_str()).or_insert(t);
                }
            }
        }
    }
    let kb = |b: u32| format!("{:.1} KB", b as f32 / 1024.0);
    let texels: u32 = seen.values().map(|t| t.texel_bytes).sum();
    let palettes: u32 = seen.values().map(|t| t.palette_bytes).sum();
    if texels > model2dsm::TEXTURE_VRAM_BYTES {
        let mut largest: Vec<&TexUse> = seen.values().copied().collect();
        largest.sort_by(|a, b| b.texel_bytes.cmp(&a.texel_bytes).then(a.key.cmp(&b.key)));
        let top: Vec<String> = largest.iter().take(3).map(|t| format!("{} ({})", t.key, kb(t.texel_bytes))).collect();
        out.push(Issue {
            zone: None,
            instance: None,
            severity: Severity::Error,
            msg: format!(
                "textures need {} of texture VRAM; a level has {} (banks B + D, #66). Largest: {}",
                kb(texels),
                kb(model2dsm::TEXTURE_VRAM_BYTES),
                top.join(", ")
            ),
        });
    }
    if palettes > model2dsm::PALETTE_VRAM_BYTES {
        out.push(Issue {
            zone: None,
            instance: None,
            severity: Severity::Error,
            msg: format!(
                "texture palettes need {} of palette VRAM; a level has {} (bank F, #66) — use fewer colours per texture",
                kb(palettes),
                kb(model2dsm::PALETTE_VRAM_BYTES)
            ),
        });
    }
}

/// `"OBJECTIVE, LEVEL_OBJECTIVE"` — every defined instance-flag bit.
fn flag_bit_names() -> String {
    flag_bits::NAMED
        .iter()
        .map(|(b, n)| format!("{n} ({b:#x})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Name the set bits of `mask`, falling back to hex for anything undefined.
fn named_bits(mask: u32) -> String {
    let mut parts: Vec<String> = flag_bits::NAMED
        .iter()
        .filter(|(b, _)| mask & b != 0)
        .map(|(_, n)| n.to_string())
        .collect();
    let rest = flag_bits::unknown_bits(mask);
    if rest != 0 {
        parts.push(format!("{rest:#x}"));
    }
    parts.join(" | ")
}

/// Hard-validate a single zone, stopping at the first error — the
/// source-compatible wrapper over [`validate_all`]'s instance/zone-scope rules
/// for callers that hold one [`Space`] and no level context. `mesh_exists`
/// reports whether a referenced mesh has a source `.obj`.
///
/// Prefer [`validate_all`]: it also runs the level-scoped rules and reports
/// every finding at once.
pub fn validate(space: &Space, mesh_exists: impl Fn(&str) -> bool) -> Result<(), String> {
    let mut issues = Vec::new();
    validate_zone("", space, &ExistsOnly(mesh_exists), &mut issues);
    match issues.iter().find(|i| i.severity == Severity::Error) {
        Some(e) => Err(match e.instance {
            Some(i) => format!("instance {i}: {}", e.msg),
            None => e.msg.clone(),
        }),
        None => Ok(()),
    }
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
        // v4: the role-scoped sub-archetype, resolved to its index in
        // `Role::kinds()`. Infallible by construction — `validate_all` has
        // already rejected an unknown role or kind, and an absent kind is the
        // role's default (0, e.g. `EnemyKind::Basic`).
        w.u8(Role::parse(&inst.role)
            .and_then(|r| inst.kind.as_deref().and_then(|s| kind_from_str(r, s)))
            .unwrap_or(0));
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
    // Zone clear-flag (v3): the flag this arena raises when its objective enemies
    // are resolved (0 = freeform). The gate authoring itself is already folded
    // into each connection's `gate` by `derive_connections`.
    w.u32(space.clear_flag);
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

/// One level directory, parsed and prefab-resolved — the shared front half of
/// [`build_levels_dir`] and [`validate_levels_dir`], so the bake and the check
/// can never disagree about what a level *is*.
struct LoadedLevel {
    /// Directory stem (the level name).
    name: String,
    /// The level directory itself (source `.ron` paths hang off it).
    dir: PathBuf,
    /// Path to the manifest, used to prefix error messages.
    manifest_path: PathBuf,
    level: Level,
    /// Prefab-expanded zones in deterministic (manifest) order.
    zones: Vec<(String, Space)>,
}

/// Parse + assemble every level directory under `levels_root`. A *level
/// directory* is any immediate subdirectory containing a [`MANIFEST_NAME`]
/// manifest; `prefab_dir` holds the shared [`Prefab`] library.
fn load_levels_dir(levels_root: &Path, prefab_dir: &Path) -> Result<Vec<LoadedLevel>, String> {
    let prefabs = load_prefab_lib(prefab_dir)?;
    let mut out = Vec::new();
    for level_dir in read_dir_sorted(levels_root)? {
        let manifest_path = level_dir.join(MANIFEST_NAME);
        if !manifest_path.is_file() {
            continue; // not a level directory
        }
        let name = level_dir
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

        // Resolve prefabs → Space intermediates (connections are derived later,
        // over the whole level at once — each zone's neighbours need the layout).
        let zones = assemble(&level, &zone_contents, &prefabs)
            .map_err(|e| format!("{}: {e}", manifest_path.display()))?;

        out.push(LoadedLevel {
            name,
            dir: level_dir,
            manifest_path,
            level,
            zones,
        });
    }
    Ok(out)
}

/// Run [`validate_all`] over every level under `levels_root` **without writing
/// anything**, returning every [`Issue`] from every level (the `scene2bin
/// --check` / `just check-levels` path). Parse-level failures — malformed RON, a
/// missing content file, an unknown prefab — still surface as `Err`, since
/// there's nothing to validate then. `assets_dir` is the geometry root; its
/// legacy `*.obj` and `models/**` are scanned into a [`Catalog`] for mesh
/// existence and the texture budget.
pub fn validate_levels_dir(
    levels_root: &Path,
    assets_dir: &Path,
    prefab_dir: &Path,
) -> Result<Vec<Issue>, String> {
    let catalog = Catalog::scan(assets_dir)?;
    let mut out = Vec::new();
    for lv in load_levels_dir(levels_root, prefab_dir)? {
        out.extend(validate_all_with_catalog(&lv.level, &lv.zones, &catalog));
    }
    Ok(out)
}

/// Bake every level directory under `levels_root` into
/// `<dst_root>/<level>/<zone>.scene`. A *level directory* is any immediate
/// subdirectory containing a [`MANIFEST_NAME`] manifest. `assets_dir` is the
/// geometry root; its legacy `*.obj` and `models/**` are scanned into a
/// [`Catalog`] for mesh existence and the texture budget; `prefab_dir` holds
/// the shared [`Prefab`] library. Returns the compiled zones (with any
/// warnings) so a `build.rs` can emit `rerun-if-changed` + `cargo:warning=`
/// lines.
pub fn build_levels_dir(
    levels_root: &Path,
    dst_root: &Path,
    assets_dir: &Path,
    prefab_dir: &Path,
) -> Result<Vec<Built>, String> {
    let catalog = Catalog::scan(assets_dir)?;

    let mut built = Vec::new();
    for LoadedLevel {
        name: level_name,
        dir: level_dir,
        manifest_path,
        level,
        zones,
    } in load_levels_dir(levels_root, prefab_dir)?
    {
        // One non-short-circuiting pass over the whole level (#27): every Error
        // is reported at once, named by zone + instance index, so an author fixes
        // the level in a single round trip. Warnings ride through onto `Built`.
        let issues = validate_all_with_catalog(&level, &zones, &catalog);
        let errors: Vec<String> = issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| format!("{}: {}", i.scope(), i.msg))
            .collect();
        if !errors.is_empty() {
            return Err(format!(
                "{}: {}",
                manifest_path.display(),
                errors.join("; ")
            ));
        }

        let conns = derive_connections(&zones);
        let mut warns = isolation_warnings(&conns);
        for issue in issues.iter().filter(|i| i.severity == Severity::Warning) {
            // A level-scoped warning has no zone of its own; hang it on the entry
            // zone, which is where an author starts reading.
            let stem = issue.zone.clone().unwrap_or_else(|| level.entry.clone());
            let text = match issue.instance {
                Some(i) => format!("instance {i}: {}", issue.msg),
                None => issue.msg.clone(),
            };
            warns.entry(stem).or_default().push(text);
        }

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
            // A round block — the second kinded role (#12), wire byte 2.
            Lit(Instance(
                mesh: Some("prim_cylinder"),
                role: "block",
                kind: Some("round"),
                pos: (0.6, 0.0, 0.6),
                scale: (0.3, 0.3, 0.3),
            )),
        ])
    "#;

    const PATROLLER: &str = r#"Prefab(mesh: Some("cube"), role: "enemy", kind: Some("shielded"), scale: (0.16, 0.16, 0.16), material: Some((diffuse: (225, 80, 70), ambient: (56, 20, 18))))"#;
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
            clear_flag: 0,
            gates: Vec::new(),
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
        // …content (5 placements) resolved to 5 flat instances.
        assert_eq!(atrium.instances.len(), 5);
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
        assert_eq!(u16::from_le_bytes([blob[4], blob[5]]), 4, "v4 wire version");
        assert_eq!(u16::from_le_bytes([blob[6], blob[7]]), 0); // Follow

        // Walk the instance records and read each one's kind byte (the byte
        // immediately after `flags`). The avatar authors none → 0; the test
        // `patroller` prefab is `shielded` → 1; the block is `round` → 2.
        let mut p = 24; // magic(4) + version(2) + camera mode(2) + params(16)
        let n = u32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]]) as usize;
        p += 4;
        assert_eq!(n, atrium.instances.len());
        let mut kinds = Vec::new();
        let skip_str = |p: &mut usize| {
            let len = u16::from_le_bytes([blob[*p], blob[*p + 1]]) as usize;
            *p += 2 + len;
        };
        for _ in 0..n {
            skip_str(&mut p); // mesh
            skip_str(&mut p); // role
            p += 4 * 9; // pos/rot/scale
            p += 1 + 6; // has_material + diffuse/ambient
            p += 4; // flags
            kinds.push(blob[p]);
            p += 1; // kind
            let path_len = u16::from_le_bytes([blob[p], blob[p + 1]]) as usize;
            p += 2 + path_len * 8;
        }
        // atrium.ron order: avatar (Lit), patroller (Use), 2× landmark_block,
        // then a `round` block — `BlockKind::Round.wire() == 2`.
        assert_eq!(kinds, std::vec![0, 1, 0, 0, 2]);
    }

    #[test]
    fn legacy_ron_parses_with_kind_none_and_bakes_wire_zero() {
        // Verbatim copies of the shipped assets *before* this change: `kind` is
        // absent everywhere, so every existing level must keep parsing and bake
        // to the role default (wire 0).
        const LEGACY_PATROLLER: &str = r#"Prefab(
            mesh: Some("cube"),
            role: "enemy",
            rot: (0.0, 0.4, 0.0),
            scale: (0.16, 0.16, 0.16),
            material: Some((diffuse: (225, 80, 70), ambient: (56, 20, 18))),
        )"#;
        const LEGACY_ATRIUM: &str = r#"Zone(instances: [
            Lit(Instance(
                mesh: Some("teapot"),
                role: "avatar",
                pos: (0.0, 0.0, 0.0),
                rot: (-1.5708, 0.0, 0.0),
                scale: (0.11, 0.11, 0.11),
                material: Some((diffuse: (110, 180, 235), ambient: (26, 40, 58))),
            )),
            Use(name: "patroller", pos: (-1.4, 0.0, 0.0), flags: Some(1), path: [(-1.4, 0.0), (0.0, 1.4)]),
        ])"#;

        let pf = parse_prefab_ron(LEGACY_PATROLLER).unwrap();
        assert_eq!(pf.kind, None);
        let zone = parse_zone_ron(LEGACY_ATRIUM).unwrap();
        let Placement::Lit(avatar) = &zone.instances[0] else {
            panic!("first placement is a literal");
        };
        assert_eq!(avatar.kind, None);

        let lib = PrefabLib::from([("patroller".to_string(), pf)]);
        let enemy = resolve_placement(&zone.instances[1], &lib).unwrap();
        assert_eq!(enemy.kind, None);

        // The editor's writer keeps emitting the plain role string.
        let ron = to_zone_ron(&zone).unwrap();
        assert!(ron.contains(r#"role: "avatar""#), "{ron}");
        assert!(parse_zone_ron(&ron).is_ok());

        // And an absent kind bakes to 0 — today's circle-vulnerable enemy.
        let space = Space {
            camera: Camera::default(),
            place: [0.0, 0.0],
            bounds: Bounds::default(),
            clear_flag: 0,
            gates: Vec::new(),
            instances: std::vec![enemy],
        };
        let blob = encode(&space, &[]);
        // magic+version+camera(24) + count(4) + mesh("cube") + role("enemy")
        // + 9 f32 + material(7) + flags(4)
        let kind_at = 24 + 4 + (2 + 4) + (2 + 5) + 36 + 7 + 4;
        assert_eq!(blob[kind_at], 0);
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
                        clear_flag: e.clear_flag,
                        gates: e.gates.clone(),
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
    fn gate_authored_on_source_zone_lands_on_the_derived_connection() {
        // Two abutting zones; `a` gates its exit toward `b` on flag 7. The
        // derived a→b connection carries gate 7; the reverse b→a stays open.
        let mut a = zone([0.0, 0.0], [-1.0, -1.0], [1.0, 1.0]);
        a.gates = std::vec![Gate {
            neighbour: "b".to_string(),
            flag: 7,
        }];
        let b = zone([2.0, 0.0], [-1.0, -1.0], [1.0, 1.0]);
        let conns = derive_connections(&std::vec![("a".to_string(), a), ("b".to_string(), b)]);
        assert_eq!(conns["a"].len(), 1);
        assert_eq!(conns["a"][0].neighbour, "b");
        assert_eq!(
            conns["a"][0].gate, 7,
            "authored gate should ride the a→b conn"
        );
        assert_eq!(conns["b"][0].gate, 0, "reverse crossing stays open");
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

    // --- validate_all ---------------------------------------------------------

    /// A bare instance with the given role, no mesh (so mesh checks stay out of
    /// the way) and no flags.
    fn inst(role: &str) -> Instance {
        Instance {
            mesh: None,
            role: role.to_string(),
            kind: None,
            pos: [0.0; 3],
            rot: [0.0; 3],
            scale: [1.0; 3],
            material: None,
            flags: 0,
            path: Vec::new(),
        }
    }

    /// A **solid** instance (`landmark` / `block`). Solid roles must carry a
    /// mesh (#12) — their collider is derived from it — so the bare [`inst`]
    /// helper can't stand in for one.
    fn solid(role: &str) -> Instance {
        let mut i = inst(role);
        i.mesh = Some("cube".to_string());
        i
    }

    /// A single-zone level (entry == `stem`) whose manifest mirrors `space`, so
    /// gate/clear_flag rules see a consistent world.
    fn one_zone_level(stem: &str, space: Space) -> (Level, Vec<(String, Space)>) {
        let level = Level {
            name: "T".to_string(),
            entry: stem.to_string(),
            zones: BTreeMap::from([(
                stem.to_string(),
                ZoneEntry {
                    place: space.place,
                    bounds: space.bounds,
                    camera: space.camera,
                    clear_flag: space.clear_flag,
                    gates: space.gates.clone(),
                },
            )]),
        };
        let zones = std::vec![(stem.to_string(), space)];
        (level, zones)
    }

    fn with_instances(instances: Vec<Instance>) -> Space {
        let mut s = zone([0.0, 0.0], [-2.0, -2.0], [2.0, 2.0]);
        s.instances = instances;
        s
    }

    fn errors(issues: &[Issue]) -> Vec<&Issue> {
        issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .collect()
    }

    fn warnings(issues: &[Issue]) -> Vec<&Issue> {
        issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .collect()
    }

    #[test]
    fn validate_all_names_zone_and_instance_for_unknown_role() {
        let space = with_instances(std::vec![inst("avatar"), inst("blockk")]);
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert_eq!(errs[0].zone.as_deref(), Some("atrium"));
        assert_eq!(errs[0].instance, Some(1));
        assert!(errs[0].msg.contains("blockk"), "{}", errs[0].msg);
        // The message lists the whole valid vocabulary, so the fix is obvious.
        for r in schema::Role::ALL {
            assert!(
                errs[0].msg.contains(r.as_str()),
                "`{}` missing from `{}`",
                r.as_str(),
                errs[0].msg
            );
        }
        assert_eq!(errs[0].scope(), "atrium#1");
    }

    #[test]
    fn validate_all_rejects_kind_and_flag_misuse() {
        let mut kind_on_landmark = solid("landmark");
        kind_on_landmark.kind = Some("shielded".to_string());
        let mut bogus_kind = inst("enemy");
        bogus_kind.kind = Some("bogus".to_string());
        let mut undefined_bit = inst("enemy");
        undefined_bit.flags = 0x8;
        let mut objective_landmark = solid("landmark");
        objective_landmark.flags = 0x1;

        let space = with_instances(std::vec![
            inst("avatar"),
            kind_on_landmark,
            bogus_kind,
            undefined_bit,
            objective_landmark,
        ]);
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 4, "{issues:#?}");

        assert_eq!(errs[0].instance, Some(1));
        assert!(errs[0].msg.contains("no kinds"), "{}", errs[0].msg);
        assert_eq!(errs[1].instance, Some(2));
        assert!(errs[1].msg.contains("bogus"), "{}", errs[1].msg);
        // …and names the enemy kinds that *are* valid.
        assert!(errs[1].msg.contains("shielded"), "{}", errs[1].msg);
        assert_eq!(errs[2].instance, Some(3));
        assert!(errs[2].msg.contains("undefined"), "{}", errs[2].msg);
        assert_eq!(errs[3].instance, Some(4));
        assert!(errs[3].msg.contains("OBJECTIVE"), "{}", errs[3].msg);
        assert!(errs[3].msg.contains("landmark"), "{}", errs[3].msg);

        // The same flag bit on an enemy is fine.
        let mut ok_enemy = inst("enemy");
        ok_enemy.flags = 0x3;
        let (level, zones) = one_zone_level(
            "atrium",
            with_instances(std::vec![inst("avatar"), ok_enemy]),
        );
        assert!(errors(&validate_all(&level, &zones, |_| true)).is_empty());
    }

    #[test]
    fn validate_all_reports_multiple_issues_where_validate_stops_at_first() {
        let mut meshed = inst("enemy");
        meshed.mesh = Some("ghost".to_string());
        let space = with_instances(std::vec![inst("avatar"), inst(""), inst("nemy"), meshed,]);
        let (level, zones) = one_zone_level("atrium", space.clone());

        // `validate` is a first-Error wrapper — exactly one message.
        let first = validate(&space, |_| false).unwrap_err();
        assert!(first.contains("empty `role`"), "{first}");

        // `validate_all` never short-circuits: empty role, unknown role, missing
        // mesh — all three, each with its own instance index.
        let issues = validate_all(&level, &zones, |_| false);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 3, "{issues:#?}");
        assert_eq!(
            errs.iter().map(|e| e.instance).collect::<Vec<_>>(),
            std::vec![Some(1), Some(2), Some(3)]
        );
        assert!(errs[2].msg.contains("ghost.obj"), "{}", errs[2].msg);
    }

    #[test]
    fn validate_all_aggregates_scenery_into_one_warning_and_passes_facility_shape() {
        // Scenery instances collapse into ONE per-zone Warning. Since the
        // collide item promoted `block` to Gameplay (#12), the three blocks are
        // *not* in that tally any more — `prop` is the only role left in it.
        let space = with_instances(std::vec![
            inst("avatar"),
            solid("block"),
            solid("block"),
            solid("block"),
            inst("prop"),
        ]);
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        assert!(errors(&issues).is_empty(), "{issues:#?}");
        let warns = warnings(&issues);
        assert_eq!(warns.len(), 1, "{issues:#?}");
        assert_eq!(warns[0].zone.as_deref(), Some("atrium"));
        assert_eq!(warns[0].instance, None);
        assert!(warns[0].msg.contains("1 instances"), "{}", warns[0].msg);
        assert!(warns[0].msg.contains("prop ×1"), "{}", warns[0].msg);
        assert!(
            !warns[0].msg.contains("block"),
            "a solid block is no longer inert scenery: {}",
            warns[0].msg
        );

        // The shipped facility shape — avatar in the entry zone, two objective
        // enemies (one kind-carrying), two landmarks — is clean either way.
        let mut objective = inst("enemy");
        objective.flags = schema::flag_bits::OBJECTIVE;
        objective.kind = Some("shielded".to_string());
        let mut level_objective = inst("enemy");
        level_objective.flags = schema::flag_bits::LEVEL_OBJECTIVE;
        let mut facility = with_instances(std::vec![
            inst("avatar"),
            objective,
            level_objective,
            solid("landmark"),
            solid("landmark"),
        ]);
        facility.clear_flag = 1;
        let (level, zones) = one_zone_level("atrium", facility);
        let issues = validate_all(&level, &zones, |_| true);
        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn validate_all_rejects_meshless_solid_and_tilted_solid() {
        // A solid role derives its collider from the mesh, so a meshless one
        // would block nothing; and the collider is yaw-only, so a pitched or
        // rolled one would render at an angle it can't represent (#12).
        let mut tilted_block = solid("block");
        tilted_block.rot = [0.1, 0.0, 0.0];
        let mut rolled_landmark = solid("landmark");
        rolled_landmark.rot = [0.0, 0.7, -0.2]; // yaw is fine, roll is not
        let space = with_instances(std::vec![
            inst("avatar"),
            inst("landmark"), // meshless solid
            tilted_block,
            rolled_landmark,
        ]);
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 3, "{issues:#?}");

        assert_eq!(errs[0].scope(), "atrium#1");
        assert!(errs[0].msg.contains("needs a mesh"), "{}", errs[0].msg);
        assert!(errs[0].msg.contains("landmark"), "{}", errs[0].msg);
        assert_eq!(errs[1].scope(), "atrium#2");
        assert!(errs[1].msg.contains("yaw-only"), "{}", errs[1].msg);
        assert_eq!(errs[2].scope(), "atrium#3");
        assert!(errs[2].msg.contains("yaw-only"), "{}", errs[2].msg);

        // Non-solid roles are untouched: a tilted prop is a legitimate leaning
        // decoration, a tilted enemy a legitimate pose, and neither needs a mesh.
        let mut tilted_prop = inst("prop");
        tilted_prop.rot = [0.3, 0.0, 0.2];
        let mut tilted_enemy = inst("enemy");
        tilted_enemy.rot = [0.3, 0.0, 0.2];
        let (level, zones) = one_zone_level(
            "atrium",
            with_instances(std::vec![inst("avatar"), tilted_prop, tilted_enemy]),
        );
        assert!(errors(&validate_all(&level, &zones, |_| true)).is_empty());
    }

    #[test]
    fn validate_all_rejects_non_positive_solid_scale() {
        // A solid's collider is the mesh AABB × scale (#12). A zero component
        // collapses it to nothing on that axis; a negative one mirrors the mesh
        // and would hand the runtime negative half extents / an inverted span —
        // which fed `Ord::clamp` with `min > max`, i.e. a `panic = "abort"` ROM
        // death the first frame the avatar got near it. Mirroring is a normal
        // authoring move, so the bake has to say no out loud.
        let mut neg_x = solid("block");
        neg_x.scale = [-0.4, 0.24, 0.4];
        let mut neg_y = solid("landmark");
        neg_y.scale = [0.16, -0.16, 0.16];
        let mut zero_z = solid("block");
        zero_z.scale = [0.4, 0.24, 0.0];
        // RON parses `inf` and `NaN` literals, and neither is `<= 0.0`: an
        // infinite half extent saturates `Fx32` and overflows the broad-reject
        // bound (a debug-assert abort at zone load), so non-finite is rejected
        // by the same rule.
        let mut inf_x = solid("block");
        inf_x.scale = [f32::INFINITY, 0.24, 0.4];
        let mut nan_z = solid("landmark");
        nan_z.scale = [0.16, 0.16, f32::NAN];
        let space = with_instances(std::vec![
            inst("avatar"),
            neg_x,
            neg_y,
            zero_z,
            inf_x,
            nan_z
        ]);
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 5, "{issues:#?}");
        assert_eq!(errs[3].scope(), "atrium#4");
        assert!(errs[3].msg.contains("scale.x = inf"), "{}", errs[3].msg);
        assert_eq!(errs[4].scope(), "atrium#5");
        assert!(errs[4].msg.contains("scale.z = NaN"), "{}", errs[4].msg);

        assert_eq!(errs[0].scope(), "atrium#1");
        assert!(errs[0].msg.contains("scale.x = -0.4"), "{}", errs[0].msg);
        assert!(errs[0].msg.contains("must be > 0"), "{}", errs[0].msg);
        assert_eq!(errs[1].scope(), "atrium#2");
        assert!(errs[1].msg.contains("scale.y = -0.16"), "{}", errs[1].msg);
        assert!(errs[1].msg.contains("landmark"), "{}", errs[1].msg);
        assert_eq!(errs[2].scope(), "atrium#3");
        assert!(errs[2].msg.contains("scale.z = 0"), "{}", errs[2].msg);

        // Only the first offending axis is reported per instance — one Error
        // per instance, not three for a uniformly mirrored one.
        let mut all_neg = solid("block");
        all_neg.scale = [-0.4, -0.24, -0.4];
        let (level, zones) =
            one_zone_level("atrium", with_instances(std::vec![inst("avatar"), all_neg]));
        assert_eq!(errors(&validate_all(&level, &zones, |_| true)).len(), 1);

        // Non-solid roles are untouched: mirroring a `prop` is just a mirrored
        // decoration, and nothing derives a collider from it.
        let mut mirrored_prop = inst("prop");
        mirrored_prop.scale = [-0.3, 0.3, -0.3];
        let (level, zones) = one_zone_level(
            "atrium",
            with_instances(std::vec![inst("avatar"), mirrored_prop]),
        );
        assert!(errors(&validate_all(&level, &zones, |_| true)).is_empty());

        // …and a plain positive solid still bakes clean.
        let (level, zones) = one_zone_level(
            "atrium",
            with_instances(std::vec![inst("avatar"), solid("block")]),
        );
        assert!(errors(&validate_all(&level, &zones, |_| true)).is_empty());
    }

    #[test]
    fn validate_all_accepts_block_kinds() {
        // `block` gained the shape table (#12) — all three spellings bake.
        let mut ramp = solid("block");
        ramp.kind = Some("ramp".to_string());
        let mut round = solid("block");
        round.kind = Some("round".to_string());
        let (level, zones) = one_zone_level(
            "atrium",
            with_instances(std::vec![inst("avatar"), ramp, round, solid("block")]),
        );
        let issues = validate_all(&level, &zones, |_| true);
        assert!(errors(&issues).is_empty(), "{issues:#?}");

        // A `landmark` is still kindless — it is always a box — so the existing
        // "role has no kinds" Error still fires for it.
        let mut kinded_landmark = solid("landmark");
        kinded_landmark.kind = Some("ramp".to_string());
        let (level, zones) = one_zone_level(
            "atrium",
            with_instances(std::vec![inst("avatar"), kinded_landmark]),
        );
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 1, "{errs:#?}");
        assert!(errs[0].msg.contains("no kinds"), "{}", errs[0].msg);

        // …and an unknown block shape is the existing unknown-kind Error, which
        // now lists the block vocabulary.
        let mut bogus = solid("block");
        bogus.kind = Some("wedge".to_string());
        let (level, zones) =
            one_zone_level("atrium", with_instances(std::vec![inst("avatar"), bogus]));
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 1, "{errs:#?}");
        assert!(errs[0].msg.contains("wedge"), "{}", errs[0].msg);
        assert!(errs[0].msg.contains("box, ramp, round"), "{}", errs[0].msg);
    }

    #[test]
    fn validate_all_level_scope_rules() {
        let msgs = |issues: &[Issue], sev: Severity| {
            issues
                .iter()
                .filter(|i| i.severity == sev)
                .map(|i| format!("{}: {}", i.scope(), i.msg))
                .collect::<Vec<_>>()
                .join(" | ")
        };

        // Zero avatars.
        let (level, zones) = one_zone_level("atrium", with_instances(std::vec![inst("enemy")]));
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert!(errs[0].msg.contains("no `avatar`"), "{}", errs[0].msg);
        assert_eq!(errs[0].zone, None);

        // Two avatars.
        let (level, zones) = one_zone_level(
            "atrium",
            with_instances(std::vec![inst("avatar"), inst("avatar")]),
        );
        let issues = validate_all(&level, &zones, |_| true);
        assert!(
            msgs(&issues, Severity::Error).contains("2 `avatar` instances"),
            "{issues:#?}"
        );

        // The one avatar sits outside the entry zone.
        let level = Level {
            name: "T".to_string(),
            entry: "atrium".to_string(),
            zones: BTreeMap::from([
                (
                    "atrium".to_string(),
                    ZoneEntry {
                        place: [0.0, 0.0],
                        bounds: Bounds::default(),
                        camera: Camera::default(),
                        clear_flag: 0,
                        gates: Vec::new(),
                    },
                ),
                (
                    "corridor".to_string(),
                    ZoneEntry {
                        place: [4.0, 0.0],
                        bounds: Bounds::default(),
                        camera: Camera::default(),
                        clear_flag: 0,
                        gates: Vec::new(),
                    },
                ),
            ]),
        };
        let zones = std::vec![
            ("atrium".to_string(), with_instances(Vec::new())),
            (
                "corridor".to_string(),
                with_instances(std::vec![inst("avatar")])
            ),
        ];
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert_eq!(errs[0].zone.as_deref(), Some("corridor"));
        assert!(errs[0].msg.contains("entry zone"), "{}", errs[0].msg);

        // A gate naming a zone that doesn't exist (today `derive_connections`
        // silently drops it into a permanently closed crossing).
        let mut space = with_instances(std::vec![inst("avatar")]);
        space.gates = std::vec![Gate {
            neighbour: "ghost".to_string(),
            flag: 1,
        }];
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        let errs = errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert!(errs[0].msg.contains("ghost"), "{}", errs[0].msg);

        // An authored flag id in the engine-reserved range.
        let mut space = with_instances(std::vec![inst("avatar")]);
        space.clear_flag = schema::flag_ids::LEVEL_EXIT;
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        assert!(
            msgs(&issues, Severity::Error).contains("reserved"),
            "{issues:#?}"
        );
        let mut space = with_instances(std::vec![inst("avatar")]);
        space.gates = std::vec![Gate {
            neighbour: "atrium".to_string(),
            flag: 0x1000_0000,
        }];
        let (level, zones) = one_zone_level("atrium", space);
        assert!(
            msgs(&validate_all(&level, &zones, |_| true), Severity::Error).contains("reserved"),
            "reserved gate flag should error"
        );

        // A gating arena with nothing to clear: Warning only (soft-lock).
        let mut space = with_instances(std::vec![inst("avatar"), inst("enemy")]);
        space.clear_flag = 1;
        let (level, zones) = one_zone_level("atrium", space);
        let issues = validate_all(&level, &zones, |_| true);
        assert!(errors(&issues).is_empty(), "{issues:#?}");
        let warns = warnings(&issues);
        assert_eq!(warns.len(), 1, "{issues:#?}");
        assert!(warns[0].msg.contains("OBJECTIVE"), "{}", warns[0].msg);
    }

    fn prop(mesh: &str) -> Instance {
        let mut i = inst("prop");
        i.mesh = Some(mesh.to_string());
        i
    }

    /// A catalogue where each listed mesh uses the given textures.
    fn catalog(entries: &[(&str, Vec<TexUse>)]) -> Catalog {
        Catalog {
            meshes: entries
                .iter()
                .map(|(n, t)| {
                    (
                        n.to_string(),
                        MeshEntry { source: std::path::PathBuf::from(format!("{n}.obj")), textures: t.clone() },
                    )
                })
                .collect(),
        }
    }

    fn tex(key: &str, kb: u32) -> TexUse {
        TexUse { key: key.to_string(), texel_bytes: kb * 1024, palette_bytes: 32 }
    }

    fn texture_errors(issues: &[Issue]) -> Vec<&Issue> {
        issues
            .iter()
            .filter(|i| i.severity == Severity::Error && (i.msg.contains("texture") || i.msg.contains("palette")))
            .collect()
    }

    #[test]
    fn texture_budget_counts_shared_textures_once() {
        let cat = catalog(&[("a", vec![tex("shared.png", 200)]), ("b", vec![tex("shared.png", 200)])]);
        let (level, zones) =
            one_zone_level("atrium", with_instances(std::vec![inst("avatar"), prop("a"), prop("b")]));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        assert!(texture_errors(&issues).is_empty(), "{issues:#?}");
    }

    #[test]
    fn texture_budget_over_256_kb_is_a_level_error() {
        let cat = catalog(&[("a", vec![tex("a.png", 200)]), ("b", vec![tex("b.png", 100)])]);
        let (level, zones) =
            one_zone_level("atrium", with_instances(std::vec![inst("avatar"), prop("a"), prop("b")]));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        let errs = texture_errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert_eq!(errs[0].zone, None);
        assert!(errs[0].msg.contains("300.0 KB"), "{}", errs[0].msg);
        assert!(errs[0].msg.contains("a.png (200.0 KB)"), "{}", errs[0].msg);
    }

    #[test]
    fn palette_budget_over_16_kb_is_a_level_error() {
        let entries: Vec<(String, Vec<TexUse>)> = (0..33)
            .map(|i| {
                (
                    format!("m{i}"),
                    vec![TexUse { key: format!("t{i}.png"), texel_bytes: 64, palette_bytes: 512 }],
                )
            })
            .collect();
        let refs: Vec<(&str, Vec<TexUse>)> = entries.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
        let cat = catalog(&refs);
        let mut instances = std::vec![inst("avatar")];
        instances.extend(entries.iter().map(|(n, _)| prop(n)));
        let (level, zones) = one_zone_level("atrium", with_instances(instances));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        let errs = texture_errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert!(errs[0].msg.contains("palette"), "{}", errs[0].msg);
    }

    #[test]
    fn catalog_validation_reports_unknown_meshes() {
        let cat = catalog(&[]);
        let (level, zones) = one_zone_level("atrium", with_instances(std::vec![inst("avatar"), prop("ghost")]));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        assert!(errors(&issues).iter().any(|i| i.msg.contains("ghost.obj")), "{issues:#?}");
    }

    #[test]
    fn resolve_placement_copies_prefab_kind_through_use() {
        // `kind` is prefab-owned (no `Use` override), but a `Use` may still
        // replace `flags` — the two must not interfere.
        let zone = parse_zone_ron(
            r#"Zone(instances: [Use(name: "patroller", pos: (1.0, 0.0, 2.0), flags: Some(1))])"#,
        )
        .unwrap();
        let inst = resolve_placement(&zone.instances[0], &prefabs()).unwrap();
        assert_eq!(inst.role, "enemy");
        assert_eq!(inst.kind.as_deref(), Some("shielded"));
        assert_eq!(inst.flags, schema::flag_bits::OBJECTIVE);
        assert_eq!(inst.pos, [1.0, 0.0, 2.0]);
    }

    #[test]
    fn encode_round_trips_through_bevy_nds_scene_parse() {
        // The real writer against the real reader: this is what stops the
        // `bevy_nds_scene::asset` test-only mirror `Writer` from drifting.
        let mut enemy = inst("enemy");
        enemy.mesh = Some("cube".to_string());
        enemy.kind = Some("advanced".to_string()); // non-zero wire byte
        enemy.flags = schema::flag_bits::LEVEL_OBJECTIVE;
        enemy.pos = [1.2, 0.0, 0.6];
        enemy.rot = [0.0, 0.4, 0.0];
        enemy.scale = [0.16, 0.16, 0.16];
        enemy.material = Some(Material {
            diffuse: [225, 80, 70],
            ambient: [56, 20, 18],
        });
        enemy.path = std::vec![[1.2, 0.6], [1.2, -0.6]];

        let mut space = with_instances(std::vec![inst("avatar"), enemy]);
        space.camera = Camera::Rail2_5D {
            height: 1.4,
            dist: 2.4,
            pitch: -0.35,
        };
        space.clear_flag = 1;
        let conns = std::vec![Connection {
            neighbour: "corridor".to_string(),
            side: SIDE_EAST,
            lo: -0.55,
            hi: 0.55,
            delta: [-4.2, 0.0],
            gate: 1,
        }];

        let parsed = bevy_nds_scene::parse(&encode(&space, &conns)).expect("v4 blob parses");

        assert_eq!(
            parsed.camera,
            bevy_nds_scene::CameraMode::Rail2_5D {
                height: 1.4,
                dist: 2.4,
                pitch: -0.35,
            }
        );
        assert_eq!(
            parsed.bounds,
            [
                space.bounds.min[0],
                space.bounds.min[1],
                space.bounds.max[0],
                space.bounds.max[1]
            ]
        );
        assert_eq!(parsed.clear_flag, space.clear_flag);
        assert_eq!(parsed.instances.len(), space.instances.len());
        for (got, want) in parsed.instances.iter().zip(&space.instances) {
            assert_eq!(got.mesh.as_deref(), want.mesh.as_deref());
            assert_eq!(got.role, want.role);
            assert_eq!(got.pos, want.pos);
            assert_eq!(got.rot, want.rot);
            assert_eq!(got.scale, want.scale);
            assert_eq!(got.material, want.material.map(|m| (m.diffuse, m.ambient)));
            assert_eq!(got.flags, want.flags);
            assert_eq!(
                got.kind,
                schema::Role::parse(&want.role)
                    .and_then(|r| want
                        .kind
                        .as_deref()
                        .and_then(|s| schema::kind_from_str(r, s)))
                    .unwrap_or(0)
            );
            assert_eq!(got.path, want.path);
        }
        assert_eq!(parsed.instances[1].kind, 2); // `advanced`
        assert_eq!(parsed.connections.len(), 1);
        let c = &parsed.connections[0];
        assert_eq!(c.neighbour, "corridor");
        assert_eq!(c.side, SIDE_EAST);
        assert_eq!((c.lo, c.hi), (-0.55, 0.55));
        assert_eq!(c.delta, [-4.2, 0.0]);
        assert_eq!(c.gate, 1);
    }
}
