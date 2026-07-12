//! Editor state, file IO (load/save the manifest + zone files), and the
//! `eframe::App` entry point. Drawing lives in [`crate::canvas`]; the side panel
//! in [`crate::panel`].

use std::collections::BTreeMap;

use bevy_nds_3d_obj::PreviewMesh;
use eframe::egui;
use egui::{Pos2, Rect, Vec2};
use scene2bin::{Camera, Instance, Level, Material, Placement, Prefab, PrefabLib, Zone, ZoneEntry};

use crate::history::History;
use crate::viewport::OrbitCam;
use crate::widgets::{MeshThumb, build_thumb, empty_level, stems};

/// Which view fills the central panel: the top-down layout canvas or the 3D
/// preview viewport (#40).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    TopDown,
    Perspective,
}

/// What the pointer is currently dragging (decided on drag-start so the grab
/// stays stable for the whole gesture).
#[derive(Clone, PartialEq)]
pub(crate) enum Drag {
    None,
    /// Move a placement within the active zone (local pos).
    Instance(usize),
    /// Move a waypoint of a placement in the active zone (local XZ).
    Waypoint(usize, usize),
    /// Move a whole zone — its global `place`.
    ZoneBody(String),
    /// Rubber-band box-select in progress (start point, in screen space) (#46).
    Box(Pos2),
    /// Drag a bounds edge/corner handle of the active zone (#45). The `u8` is a
    /// bitmask of which edges move: 1=min-x, 2=max-x, 4=min-z, 8=max-z.
    BoundsHandle(u8),
    /// Rotate the primary selection about Y via the on-canvas gizmo (#49).
    Rotate,
    /// Scale the primary selection via the on-canvas gizmo (#49).
    Scale,
}

/// The current selection within the active zone (#46). Zero or more instance
/// indices; the **primary** (last-added) drives the single-instance properties
/// editor and the gizmos, while group move/delete act over the whole set.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Sel {
    items: Vec<usize>,
}

impl Sel {
    pub(crate) fn none() -> Self {
        Self::default()
    }
    pub(crate) fn single(i: usize) -> Self {
        Self { items: vec![i] }
    }
    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }
    pub(crate) fn contains(&self, i: usize) -> bool {
        self.items.contains(&i)
    }
    /// The instance the properties panel / gizmo edits (the last one added).
    pub(crate) fn primary(&self) -> Option<usize> {
        self.items.last().copied()
    }
    pub(crate) fn items(&self) -> &[usize] {
        &self.items
    }
    /// Replace the selection with exactly `i`.
    pub(crate) fn set_single(&mut self, i: usize) {
        self.items.clear();
        self.items.push(i);
    }
    /// Replace the selection with `indices` (box-select / group duplicate).
    pub(crate) fn set_many(&mut self, indices: Vec<usize>) {
        self.items = indices;
    }
    /// Shift-click: drop `i` if present, else add it (and make it primary).
    pub(crate) fn toggle(&mut self, i: usize) {
        if let Some(p) = self.items.iter().position(|&x| x == i) {
            self.items.remove(p);
        } else {
            self.items.push(i);
        }
    }
    /// Drop selection entries that no longer point at a live instance.
    pub(crate) fn retain_below(&mut self, n: usize) {
        self.items.retain(|&i| i < n);
    }
}

pub(crate) struct View {
    /// World-space (x, z) point at the canvas centre.
    pub(crate) center: Vec2,
    /// Pixels per world unit.
    pub(crate) scale: f32,
}

pub(crate) struct EditorApp {
    /// The level directory (`assets/levels/<name>/`).
    pub(crate) level_dir: String,
    pub(crate) assets_dir: String,
    pub(crate) prefabs_dir: String,
    /// The manifest (zone-graph layout).
    pub(crate) level: Level,
    /// Per-zone content, keyed by stem (mirrors `level.zones`).
    pub(crate) contents: BTreeMap<String, Zone>,
    /// The prefab library (`assets/prefabs/*.ron`).
    pub(crate) prefabs: PrefabLib,
    pub(crate) meshes: Vec<String>,
    /// The zone currently being edited.
    pub(crate) active: Option<String>,
    /// Name field for adding a new zone.
    pub(crate) new_zone: String,
    /// Name field for scaffolding a new level (#55).
    pub(crate) new_level: String,
    pub(crate) sel: Sel,
    pub(crate) drag: Drag,
    pub(crate) view: View,
    pub(crate) status: String,
    /// Central-panel view: top-down layout vs 3D preview (#40).
    pub(crate) view_mode: ViewMode,
    /// Orbit camera for the 3D viewport.
    pub(crate) cam3: OrbitCam,
    /// Wireframe vs solid in the 3D viewport.
    pub(crate) wireframe: bool,
    /// Show the derived-connection / isolation overlay on the top-down canvas (#41).
    pub(crate) show_connections: bool,
    /// Lazily parsed OBJ preview meshes, keyed by mesh stem. `None` = a load that
    /// failed (missing/unparseable `.obj`); cached so we don't retry every frame.
    pub(crate) mesh_cache: BTreeMap<String, Option<PreviewMesh>>,
    /// Undo/redo history over the editable document (#43).
    pub(crate) history: History,
    /// Whether drags snap to the grid (#42).
    pub(crate) snap: bool,
    /// Grid step (world units) for snapping + keyboard nudge.
    pub(crate) grid_step: f32,
    /// Raw (un-snapped) drag position, tracked across a gesture so snapping
    /// doesn't lose sub-step pointer motion (see [`crate::canvas`]).
    pub(crate) drag_raw: Vec2,
    /// Single-instance clipboard for copy/paste (#47).
    pub(crate) clipboard: Option<Placement>,
    /// Whether the keyboard-shortcut help window is open (#48).
    pub(crate) show_help: bool,
    /// Scale-gizmo reference captured at drag-start: pointer→center distance and
    /// the instance's scale, so scaling is a ratio of the start pose (#49).
    pub(crate) gizmo_dist0: f32,
    pub(crate) scale_ref: [f32; 3],
    /// Iso-wireframe thumbnails per mesh stem for the picker (#52), rebuilt on
    /// [`Self::rescan`].
    pub(crate) mesh_thumbs: BTreeMap<String, MeshThumb>,
    /// The prefab currently open in the in-editor prefab editor (name + working
    /// copy), and the create-name field (#51).
    pub(crate) edit_prefab: Option<(String, Prefab)>,
    pub(crate) new_prefab: String,
}

impl EditorApp {
    pub(crate) fn new(start_path: Option<String>) -> Self {
        // Defaults assume the tool is run from `tools/scene-editor/` (its CWD),
        // so the repo's `assets/` is two levels up.
        let level_dir = start_path.unwrap_or_else(|| "../../assets/levels/facility".to_string());
        let mut app = Self {
            level_dir,
            assets_dir: "../../assets".to_string(),
            prefabs_dir: "../../assets/prefabs".to_string(),
            level: empty_level(),
            contents: BTreeMap::new(),
            prefabs: PrefabLib::new(),
            meshes: Vec::new(),
            active: None,
            new_zone: String::new(),
            new_level: String::new(),
            sel: Sel::none(),
            drag: Drag::None,
            view: View {
                center: Vec2::ZERO,
                scale: 90.0,
            },
            status: String::new(),
            view_mode: ViewMode::TopDown,
            cam3: OrbitCam::default(),
            wireframe: false,
            show_connections: true,
            mesh_cache: BTreeMap::new(),
            history: History::default(),
            snap: true,
            grid_step: 0.25,
            drag_raw: Vec2::ZERO,
            clipboard: None,
            show_help: false,
            gizmo_dist0: 1.0,
            scale_ref: [1.0, 1.0, 1.0],
            mesh_thumbs: BTreeMap::new(),
            edit_prefab: None,
            new_prefab: String::new(),
        };
        app.rescan();
        app.load();
        // `load` seeds the history on success; seed again so undo still works
        // from a clean baseline even if the initial load found no manifest.
        app.history_reset();
        app
    }

    /// Refresh the mesh + prefab libraries from disk (for the pickers).
    pub(crate) fn rescan(&mut self) {
        self.meshes = stems(&self.assets_dir, "obj");
        self.prefabs =
            scene2bin::load_prefab_lib(std::path::Path::new(&self.prefabs_dir)).unwrap_or_default();
        // Drop cached previews so edited `.obj` files re-parse on next draw.
        self.mesh_cache.clear();
        // Rebuild picker thumbnails (#52) from the (re)parsed preview meshes.
        self.mesh_thumbs.clear();
        for name in self.meshes.clone() {
            if let Some(mesh) = self.mesh_preview(&name) {
                let thumb = build_thumb(mesh);
                self.mesh_thumbs.insert(name, thumb);
            }
        }
    }

    pub(crate) fn prefab_names(&self) -> Vec<String> {
        self.prefabs.keys().cloned().collect()
    }

    /// Lazily parse + cache the preview mesh for `name` (from `<assets>/<name>.obj`)
    /// for the 3D viewport. Returns `None` for a missing/unparseable `.obj` — the
    /// failure is cached so it isn't retried every frame (cleared by [`Self::rescan`]).
    pub(crate) fn mesh_preview(&mut self, name: &str) -> Option<&PreviewMesh> {
        if !self.mesh_cache.contains_key(name) {
            let path = std::path::Path::new(&self.assets_dir).join(format!("{name}.obj"));
            let parsed = std::fs::read_to_string(&path)
                .ok()
                .and_then(|src| bevy_nds_3d_obj::obj_preview_mesh(&src).ok());
            self.mesh_cache.insert(name.to_string(), parsed);
        }
        self.mesh_cache.get(name).and_then(|m| m.as_ref())
    }

    pub(crate) fn load(&mut self) {
        let dir = std::path::Path::new(&self.level_dir);
        let manifest = dir.join(scene2bin::MANIFEST_NAME);
        let level = match std::fs::read_to_string(&manifest) {
            Ok(src) => match scene2bin::parse_level_ron(&src) {
                Ok(l) => l,
                Err(e) => {
                    self.status = format!("manifest parse error: {e}");
                    return;
                }
            },
            Err(e) => {
                self.status = format!("could not read {}: {e}", manifest.display());
                return;
            }
        };

        // Load each zone's content file named by the manifest.
        let mut contents = BTreeMap::new();
        for stem in level.zones.keys() {
            let path = dir.join(format!("{stem}.ron"));
            match std::fs::read_to_string(&path) {
                Ok(src) => match scene2bin::parse_zone_ron(&src) {
                    Ok(z) => {
                        contents.insert(stem.clone(), z);
                    }
                    Err(e) => {
                        self.status = format!("{}: parse error: {e}", path.display());
                        return;
                    }
                },
                Err(e) => {
                    self.status = format!("could not read {}: {e}", path.display());
                    return;
                }
            }
        }

        self.active = level.zones.keys().next().cloned();
        self.level = level;
        self.contents = contents;
        self.sel = Sel::none();
        self.history_reset();
        self.status = format!(
            "loaded {} ({} zones)",
            self.level_dir,
            self.level.zones.len()
        );
    }

    pub(crate) fn save(&mut self) {
        // Validate each assembled zone first so the editor can't write a level
        // that won't bake (unknown prefab, missing mesh, degenerate bounds).
        match scene2bin::assemble(&self.level, &self.contents, &self.prefabs) {
            Ok(zones) => {
                let mesh_exists = |name: &str| self.meshes.iter().any(|m| m == name);
                for (stem, space) in &zones {
                    if let Err(e) = scene2bin::validate(space, mesh_exists) {
                        self.status = format!("not saved — zone `{stem}`: {e}");
                        return;
                    }
                }
            }
            Err(e) => {
                self.status = format!("not saved — {e}");
                return;
            }
        }

        let dir = std::path::Path::new(&self.level_dir);
        // Manifest.
        match scene2bin::to_level_ron(&self.level) {
            Ok(text) => {
                if let Err(e) = std::fs::write(dir.join(scene2bin::MANIFEST_NAME), text) {
                    self.status = format!("write failed: {e}");
                    return;
                }
            }
            Err(e) => {
                self.status = format!("serialize failed: {e}");
                return;
            }
        }
        // Each zone content file.
        for (stem, zone) in &self.contents {
            match scene2bin::to_zone_ron(zone) {
                Ok(text) => {
                    if let Err(e) = std::fs::write(dir.join(format!("{stem}.ron")), text) {
                        self.status = format!("write failed ({stem}): {e}");
                        return;
                    }
                }
                Err(e) => {
                    self.status = format!("serialize failed ({stem}): {e}");
                    return;
                }
            }
        }
        self.status = format!(
            "saved {} ({} zones)",
            self.level_dir,
            self.level.zones.len()
        );
        self.rescan();
    }

    pub(crate) fn world_to_screen(&self, rect: Rect, w: Vec2) -> Pos2 {
        let c = rect.center();
        Pos2::new(
            c.x + (w.x - self.view.center.x) * self.view.scale,
            c.y + (w.y - self.view.center.y) * self.view.scale,
        )
    }

    pub(crate) fn screen_to_world_delta(&self, d: Vec2) -> Vec2 {
        d / self.view.scale
    }

    /// Inverse of [`Self::world_to_screen`]: canvas point → world (x, z).
    pub(crate) fn screen_to_world(&self, rect: Rect, s: Pos2) -> Vec2 {
        let c = rect.center();
        self.view.center + Vec2::new((s.x - c.x) / self.view.scale, (s.y - c.y) / self.view.scale)
    }

    /// The active zone's `place` (global offset), or origin if none.
    pub(crate) fn active_place(&self) -> Vec2 {
        self.active
            .as_ref()
            .and_then(|s| self.level.zones.get(s))
            .map(|e| Vec2::new(e.place[0], e.place[1]))
            .unwrap_or(Vec2::ZERO)
    }

    pub(crate) fn active_zone_mut(&mut self) -> Option<&mut Zone> {
        let stem = self.active.clone()?;
        self.contents.get_mut(&stem)
    }

    /// The directory that holds every level (`<assets>/levels`).
    fn levels_root(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.assets_dir).join("levels")
    }

    /// Discover level directories under `<assets>/levels`, sorted (#55).
    pub(crate) fn level_names(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.levels_root())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                p.is_dir()
                    .then(|| p.file_name().and_then(|s| s.to_str()).map(String::from))
                    .flatten()
            })
            .collect();
        v.sort();
        v
    }

    /// The current level's directory name (for the browser's selected label).
    pub(crate) fn current_level_name(&self) -> String {
        std::path::Path::new(&self.level_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    }

    /// Open the level directory `name` under `<assets>/levels` (#55).
    pub(crate) fn open_level(&mut self, name: &str) {
        self.level_dir = self.levels_root().join(name).to_string_lossy().into_owned();
        self.load();
    }

    /// Scaffold a new level (`<name>/level.ron` + a `start.ron` entry zone) and
    /// open it (#55).
    pub(crate) fn create_level(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.status = "level name empty".to_string();
            return;
        }
        let dir = self.levels_root().join(name);
        if dir.exists() {
            self.status = format!("level `{name}` already exists");
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.status = format!("could not create {}: {e}", dir.display());
            return;
        }
        self.level_dir = dir.to_string_lossy().into_owned();
        let mut zones = BTreeMap::new();
        zones.insert(
            "start".to_string(),
            ZoneEntry {
                place: [0.0, 0.0],
                bounds: scene2bin::Bounds::default(),
                camera: Camera::default(),
                clear_flag: 0,
                gates: Vec::new(),
            },
        );
        self.level = Level {
            name: name.to_string(),
            entry: "start".to_string(),
            zones,
        };
        self.contents = BTreeMap::new();
        self.contents.insert(
            "start".to_string(),
            Zone {
                instances: Vec::new(),
            },
        );
        self.active = Some("start".to_string());
        self.sel = Sel::none();
        self.history_reset();
        self.save();
    }

    /// A prefab file stem is valid if it's a bare name (no path separators / dots).
    fn valid_prefab_name(name: &str) -> bool {
        !name.is_empty() && !name.contains(['/', '\\', '.', ':'])
    }

    /// Write `prefab` to `<prefabs>/<name>.ron` and rescan the library (#51).
    pub(crate) fn save_prefab(&mut self, name: &str, prefab: &Prefab) {
        let name = name.trim();
        if !Self::valid_prefab_name(name) {
            self.status = format!("invalid prefab name `{name}`");
            return;
        }
        let text = match scene2bin::to_prefab_ron(prefab) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("prefab serialize failed: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&self.prefabs_dir) {
            self.status = format!("could not create {}: {e}", self.prefabs_dir);
            return;
        }
        let path = std::path::Path::new(&self.prefabs_dir).join(format!("{name}.ron"));
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.status = format!("saved prefab `{name}`");
                self.rescan();
            }
            Err(e) => self.status = format!("write failed: {e}"),
        }
    }

    /// Delete the prefab file `<prefabs>/<name>.ron` and rescan (#51).
    pub(crate) fn delete_prefab(&mut self, name: &str) {
        let path = std::path::Path::new(&self.prefabs_dir).join(format!("{name}.ron"));
        match std::fs::remove_file(&path) {
            Ok(()) => {
                self.status = format!("deleted prefab `{name}`");
                if self.edit_prefab.as_ref().map(|(n, _)| n.as_str()) == Some(name) {
                    self.edit_prefab = None;
                }
                self.rescan();
            }
            Err(e) => self.status = format!("delete failed: {e}"),
        }
    }

    /// Promote the primary selected literal instance into the prefab editor as a
    /// new draft (mesh/role/rot/scale/material/flags/path carried over) (#51).
    pub(crate) fn promote_selection_to_prefab(&mut self) {
        let Some((stem, i)) = self.selected() else {
            return;
        };
        let Some(Placement::Lit(inst)) = self.contents.get(&stem).and_then(|z| z.instances.get(i))
        else {
            self.status = "only literal instances can become prefabs".to_string();
            return;
        };
        let prefab = Prefab {
            mesh: inst.mesh.clone(),
            role: inst.role.clone(),
            rot: inst.rot,
            scale: inst.scale,
            material: inst.material,
            flags: inst.flags,
            path: inst.path.clone(),
        };
        self.edit_prefab = Some((inst.role.clone(), prefab));
    }

    /// Drop a gray-box **primitive** (#44) into the active zone at the view
    /// centre, sized via the instance transform. Bake path (b) (decided
    /// 2026-07-12): a primitive is an ordinary `Lit` instance referencing a
    /// unit-primitive `.obj` (box reuses `cube`; ramp/cylinder are generated on
    /// first use), baked by the normal `build.rs` → `obj2dl` → `.dl` path — no
    /// `.scene` format change, runtime untouched. Resize with the scale
    /// gizmo/fields; rotate with the rotate gizmo.
    pub(crate) fn add_primitive(&mut self, kind: Prim) {
        let mesh = match kind {
            Prim::Box => "cube".to_string(),
            Prim::Ramp => {
                self.ensure_prim_asset("prim_ramp", RAMP_OBJ);
                "prim_ramp".to_string()
            }
            Prim::Cylinder => {
                let obj = cylinder_obj(12);
                self.ensure_prim_asset("prim_cylinder", &obj);
                "prim_cylinder".to_string()
            }
        };
        let Some(stem) = self.active.clone() else {
            return;
        };
        let at = self.view.center;
        let inst = Instance {
            mesh: Some(mesh),
            role: "block".to_string(),
            pos: [at.x, 0.0, at.y],
            rot: [0.0, 0.0, 0.0],
            scale: [0.8, 0.8, 0.8],
            material: Some(Material {
                diffuse: [110, 116, 130],
                ambient: [30, 32, 40],
            }),
            flags: 0,
            path: Vec::new(),
        };
        if let Some(zone) = self.contents.get_mut(&stem) {
            let idx = zone.instances.len();
            zone.instances.push(Placement::Lit(inst));
            self.sel = Sel::single(idx);
        }
    }

    /// Write a generated primitive `.obj` into the top-level assets dir if it's
    /// not already there, then rescan so it appears in the mesh library +
    /// thumbnails (#44). Top-level (not a subdir) so `build.rs`'s `assets/*.obj`
    /// bake and the editor's mesh scan both pick it up with no pipeline change.
    fn ensure_prim_asset(&mut self, stem: &str, obj: &str) {
        let path = std::path::Path::new(&self.assets_dir).join(format!("{stem}.obj"));
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, obj) {
                self.status = format!("could not write {}: {e}", path.display());
                return;
            }
            self.rescan();
        }
    }

    /// Add a new (empty) zone at the view centre with default bounds/camera.
    pub(crate) fn add_zone(&mut self) {
        let stem = self.new_zone.trim().to_string();
        if stem.is_empty() || self.level.zones.contains_key(&stem) {
            self.status = "zone name empty or already exists".to_string();
            return;
        }
        self.level.zones.insert(
            stem.clone(),
            ZoneEntry {
                place: [self.view.center.x, self.view.center.y],
                bounds: scene2bin::Bounds::default(),
                camera: Camera::default(),
                clear_flag: 0,
                gates: Vec::new(),
            },
        );
        self.contents.insert(
            stem.clone(),
            Zone {
                instances: Vec::new(),
            },
        );
        self.active = Some(stem);
        self.new_zone.clear();
        self.sel = Sel::none();
    }

    /// Centre + zoom the view to fit every zone's bounds.
    pub(crate) fn frame_all(&mut self) {
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for entry in self.level.zones.values() {
            let place = Vec2::new(entry.place[0], entry.place[1]);
            min = min.min(place + Vec2::new(entry.bounds.min[0], entry.bounds.min[1]));
            max = max.max(place + Vec2::new(entry.bounds.max[0], entry.bounds.max[1]));
        }
        if min.x.is_finite() {
            self.view.center = (min + max) * 0.5;
            let span = (max - min).max(Vec2::splat(1.0));
            self.view.scale = (700.0 / span.x.max(span.y)).clamp(20.0, 400.0);
        }
    }

    /// Snap a world point to the active grid step, unless snapping is off or the
    /// hold-to-disable modifier (`disable`) is held (#42).
    pub(crate) fn snap_vec(&self, v: Vec2, disable: bool) -> Vec2 {
        if !self.snap || disable || self.grid_step <= 0.0 {
            return v;
        }
        let s = self.grid_step;
        Vec2::new((v.x / s).round() * s, (v.y / s).round() * s)
    }

    /// The active zone stem + the primary selected instance index, if any. Used
    /// by single-target ops (copy, promote-to-prefab, gizmos).
    pub(crate) fn selected(&self) -> Option<(String, usize)> {
        let i = self.sel.primary()?;
        self.active.clone().map(|s| (s, i))
    }

    /// The active zone's selected indices, sorted + de-duplicated.
    fn selection_indices(&self) -> Vec<usize> {
        let mut v = self.sel.items().to_vec();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Delete every selected instance (#48 / #46 group delete).
    pub(crate) fn delete_selection(&mut self) {
        let Some(stem) = self.active.clone() else {
            return;
        };
        let idx = self.selection_indices();
        if idx.is_empty() {
            return;
        }
        if let Some(zone) = self.contents.get_mut(&stem) {
            // Remove high→low so earlier indices stay valid.
            for &i in idx.iter().rev() {
                if i < zone.instances.len() {
                    zone.instances.remove(i);
                }
            }
        }
        self.sel = Sel::none();
    }

    /// Nudge every selected instance's local XZ by `(dx, dz)` (#48 / #46).
    pub(crate) fn nudge_selection(&mut self, dx: f32, dz: f32) {
        let Some(stem) = self.active.clone() else {
            return;
        };
        let idx = self.selection_indices();
        if let Some(zone) = self.contents.get_mut(&stem) {
            for i in idx {
                if let Some(pl) = zone.instances.get_mut(i) {
                    let p = pl.pos_mut();
                    p[0] += dx;
                    p[2] += dz;
                }
            }
        }
    }

    /// Copy the primary selected instance into the clipboard (#47).
    pub(crate) fn copy_selection(&mut self) {
        if let Some((stem, i)) = self.selected() {
            self.clipboard = self
                .contents
                .get(&stem)
                .and_then(|z| z.instances.get(i))
                .cloned();
        }
    }

    /// Duplicate every selected instance in place, offset by one grid step, and
    /// select the copies (#47 / #46 / Ctrl+D).
    pub(crate) fn duplicate_selection(&mut self) {
        let Some(stem) = self.active.clone() else {
            return;
        };
        let idx = self.selection_indices();
        if idx.is_empty() {
            return;
        }
        let step = self.grid_step.max(0.1);
        if let Some(zone) = self.contents.get_mut(&stem) {
            let mut copies = Vec::with_capacity(idx.len());
            for i in idx {
                if let Some(src) = zone.instances.get(i).cloned() {
                    let mut copy = src;
                    let p = copy.pos_mut();
                    p[0] += step;
                    p[2] += step;
                    copies.push(zone.instances.len());
                    zone.instances.push(copy);
                }
            }
            self.sel.set_many(copies);
        }
    }

    /// Paste the clipboard instance into the active zone at the view centre
    /// (snapped), and select it (#47).
    pub(crate) fn paste_clipboard(&mut self) {
        let Some(clip) = self.clipboard.clone() else {
            return;
        };
        let Some(stem) = self.active.clone() else {
            return;
        };
        let place = self.active_place();
        let at = self.snap_vec(self.view.center - place, false);
        if let Some(zone) = self.contents.get_mut(&stem) {
            let mut pl = clip;
            let p = pl.pos_mut();
            p[0] = at.x;
            p[2] = at.y;
            let idx = zone.instances.len();
            zone.instances.push(pl);
            self.sel = Sel::single(idx);
        }
    }

    /// Clone the active zone (manifest entry + content) under a fresh stem,
    /// offset east of the original, and make it active (#47).
    pub(crate) fn clone_active_zone(&mut self) {
        let Some(stem) = self.active.clone() else {
            return;
        };
        let Some(entry) = self.level.zones.get(&stem).cloned() else {
            return;
        };
        let content = self.contents.get(&stem).cloned().unwrap_or(Zone {
            instances: Vec::new(),
        });

        // Unique stem: `<stem>_copy`, `_copy2`, …
        let mut new_stem = format!("{stem}_copy");
        let mut n = 2;
        while self.level.zones.contains_key(&new_stem) {
            new_stem = format!("{stem}_copy{n}");
            n += 1;
        }

        // Offset east so the clone doesn't overlap the source.
        let width = (entry.bounds.max[0] - entry.bounds.min[0]).abs().max(1.0);
        let mut new_entry = entry;
        new_entry.place[0] += width + 0.5;

        self.level.zones.insert(new_stem.clone(), new_entry);
        self.contents.insert(new_stem.clone(), content);
        self.active = Some(new_stem);
        self.sel = Sel::none();
    }
}

impl EditorApp {
    /// Global keyboard shortcuts (#48). Consumed via the input queue so egui
    /// widgets don't also react. Save/undo/redo/help are always live; editing
    /// actions (delete/nudge/duplicate/paste/frame) are suppressed while a text
    /// or number field is focused so keystrokes go to the field instead.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};

        let editing = ctx.memory(|m| m.focused().is_some());
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers {
            command: true,
            shift: true,
            ..Modifiers::NONE
        };

        let mut save = false;
        let mut undo = false;
        let mut redo = false;
        let mut toggle_help = false;
        let mut delete = false;
        let mut duplicate = false;
        let mut copy = false;
        let mut paste = false;
        let mut frame = false;
        let mut deselect = false;
        let mut nudge = Vec2::ZERO;

        ctx.input_mut(|i| {
            save = i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::S));
            toggle_help = i.consume_key(Modifiers::NONE, Key::F1);
            if !editing {
                // Redo before undo so Ctrl+Shift+Z isn't eaten by the Ctrl+Z match.
                redo = i.consume_shortcut(&KeyboardShortcut::new(cmd_shift, Key::Z))
                    || i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::Y));
                undo = i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::Z));
                duplicate = i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::D));
                copy = i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::C));
                paste = i.consume_shortcut(&KeyboardShortcut::new(cmd, Key::V));
                delete = i.consume_key(Modifiers::NONE, Key::Delete)
                    || i.consume_key(Modifiers::NONE, Key::Backspace);
                frame = i.consume_key(Modifiers::NONE, Key::F);
                deselect = i.consume_key(Modifiers::NONE, Key::Escape);

                // Arrow nudge — grid step, or a coarse ×4 step with Shift.
                let shift = i.modifiers.shift;
                let mods = if shift {
                    Modifiers::SHIFT
                } else {
                    Modifiers::NONE
                };
                let step = self.grid_step.max(0.05) * if shift { 4.0 } else { 1.0 };
                if i.consume_key(mods, Key::ArrowUp) {
                    nudge.y -= step;
                }
                if i.consume_key(mods, Key::ArrowDown) {
                    nudge.y += step;
                }
                if i.consume_key(mods, Key::ArrowLeft) {
                    nudge.x -= step;
                }
                if i.consume_key(mods, Key::ArrowRight) {
                    nudge.x += step;
                }
            }
        });

        // Apply outside the input borrow. Undo/redo first so an action after an
        // undo starts from the restored state.
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
        if save {
            self.save();
        }
        if toggle_help {
            self.show_help = !self.show_help;
        }
        if delete {
            self.delete_selection();
        }
        if duplicate {
            self.duplicate_selection();
        }
        if copy {
            self.copy_selection();
        }
        if paste {
            self.paste_clipboard();
        }
        if frame {
            self.frame_all();
        }
        if deselect {
            self.sel = Sel::none();
        }
        if nudge != Vec2::ZERO {
            self.nudge_selection(nudge.x, nudge.y);
        }
    }

    /// The keyboard-shortcut cheat-sheet window (#48), toggled with F1.
    fn help_window(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        let mut open = true;
        egui::Window::new("Shortcuts")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let rows = [
                    ("Click / Shift+Click", "Select / add to selection"),
                    ("Drag empty canvas", "Box-select"),
                    ("Middle / Right drag", "Pan the canvas"),
                    ("Ctrl+drag zone", "Move a whole zone"),
                    ("Ctrl+Z / Ctrl+Shift+Z", "Undo / Redo"),
                    ("Ctrl+S", "Save level"),
                    ("Ctrl+D", "Duplicate selection"),
                    ("Ctrl+C / Ctrl+V", "Copy / Paste instance"),
                    ("Delete / Backspace", "Delete selection"),
                    ("Arrows", "Nudge by grid step"),
                    ("Shift + Arrows", "Nudge ×4"),
                    ("F", "Frame all zones"),
                    ("Esc", "Clear selection"),
                    ("F1", "Toggle this help"),
                ];
                egui::Grid::new("shortcut-grid")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for (keys, what) in rows {
                            ui.strong(keys);
                            ui.label(what);
                            ui.end_row();
                        }
                    });
            });
        self.show_help = open;
    }
}

impl eframe::App for EditorApp {
    // eframe 0.34 made `ui` the required entry point (`update` is deprecated).
    // We own the layout, so nest panels into the provided root `ui`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        egui::Panel::top("menu").show_inside(ui, |ui| self.menu_bar(ui));
        egui::Panel::right("props")
            .resizable(true)
            .default_size(340.0)
            .show_inside(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show_inside(ui, |ui| match self.view_mode {
            ViewMode::TopDown => self.canvas(ui),
            ViewMode::Perspective => self.viewport(ui),
        });
        self.help_window(&ctx);
        // Fold this frame's edits into the undo history once the gesture settles.
        self.commit_if_settled(&ctx);
    }
}

/// The fixed gray-box primitive set (#44). Kept small and fixed on purpose — a
/// blocking kit, not a CSG language.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prim {
    Box,
    Ramp,
    Cylinder,
}

/// A unit ramp wedge (1×1×1, sloping up along +Z). No `vn` records — the encoder
/// derives flat normals from face winding, like `assets/cube.obj`.
const RAMP_OBJ: &str = "\
# Generated unit ramp wedge — gray-box primitive (#44)
o Ramp
v -0.5 -0.5 -0.5
v 0.5 -0.5 -0.5
v 0.5 -0.5 0.5
v -0.5 -0.5 0.5
v -0.5 0.5 0.5
v 0.5 0.5 0.5
f 1 5 6 2
f 1 2 3 4
f 4 3 6 5
f 1 4 5
f 2 6 3
";

/// Generate a unit cylinder OBJ (radius 0.5, height 1, `sides` facets) as a
/// gray-box primitive (#44). Faces wound CCW-from-outside so the encoder's
/// backface cull keeps them visible.
fn cylinder_obj(sides: usize) -> String {
    use core::f32::consts::TAU;
    let (r, y0, y1) = (0.5_f32, -0.5_f32, 0.5_f32);
    let mut s = String::from("# Generated unit cylinder — gray-box primitive (#44)\no Cylinder\n");
    for i in 0..sides {
        let a = i as f32 / sides as f32 * TAU;
        s.push_str(&format!(
            "v {:.5} {:.5} {:.5}\n",
            r * a.cos(),
            y0,
            r * a.sin()
        ));
    }
    for i in 0..sides {
        let a = i as f32 / sides as f32 * TAU;
        s.push_str(&format!(
            "v {:.5} {:.5} {:.5}\n",
            r * a.cos(),
            y1,
            r * a.sin()
        ));
    }
    s.push_str(&format!("v 0.0 {y0:.5} 0.0\n")); // bottom centre = 2*sides+1
    s.push_str(&format!("v 0.0 {y1:.5} 0.0\n")); // top centre    = 2*sides+2
    let bc = 2 * sides + 1;
    let tc = 2 * sides + 2;
    for i in 0..sides {
        let (b0, b1) = (i + 1, (i + 1) % sides + 1);
        let (t0, t1) = (sides + i + 1, sides + (i + 1) % sides + 1);
        s.push_str(&format!("f {b0} {t0} {t1} {b1}\n")); // side quad
    }
    for i in 0..sides {
        let (b0, b1) = (i + 1, (i + 1) % sides + 1);
        s.push_str(&format!("f {bc} {b0} {b1}\n")); // bottom cap (-Y)
    }
    for i in 0..sides {
        let (t0, t1) = (sides + i + 1, sides + (i + 1) % sides + 1);
        s.push_str(&format!("f {tc} {t1} {t0}\n")); // top cap (+Y)
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated primitive OBJs must survive the *real* bake encoder
    /// (`obj_to_display_list`, the same one `build.rs`/`obj2dl` use) and the
    /// preview parser — otherwise a dropped ramp/cylinder wouldn't render on DS.
    fn assert_bakes(obj: &str) {
        let opts = bevy_nds_3d_obj::Options {
            center: true,
            ..Default::default()
        };
        let model = bevy_nds_3d_obj::obj_to_display_list(obj, &opts)
            .expect("primitive OBJ encodes to a display list");
        assert!(!model.words.is_empty(), "non-empty display list");
        let mesh =
            bevy_nds_3d_obj::obj_preview_mesh(obj).expect("primitive OBJ parses as a preview mesh");
        assert!(!mesh.tris.is_empty(), "preview mesh has triangles");
    }

    #[test]
    fn ramp_bakes() {
        assert_bakes(RAMP_OBJ);
    }

    #[test]
    fn cylinder_bakes() {
        assert_bakes(&cylinder_obj(12));
    }
}
