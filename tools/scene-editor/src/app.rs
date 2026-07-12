//! Editor state, file IO (load/save the manifest + zone files), and the
//! `eframe::App` entry point. Drawing lives in [`crate::canvas`]; the side panel
//! in [`crate::panel`].

use std::collections::BTreeMap;

use bevy_nds_3d_obj::PreviewMesh;
use eframe::egui;
use egui::{Pos2, Rect, Vec2};
use scene2bin::{Camera, Level, PrefabLib, Zone, ZoneEntry};

use crate::viewport::OrbitCam;
use crate::widgets::{empty_level, stems};

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
    Pan,
    /// Move a placement within the active zone (local pos).
    Instance(usize),
    /// Move a waypoint of a placement in the active zone (local XZ).
    Waypoint(usize, usize),
    /// Move a whole zone — its global `place`.
    ZoneBody(String),
}

/// The current selection within the active zone (drives the properties panel).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Sel {
    None,
    Instance(usize),
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
            sel: Sel::None,
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
        };
        app.rescan();
        app.load();
        app
    }

    /// Refresh the mesh + prefab libraries from disk (for the pickers).
    pub(crate) fn rescan(&mut self) {
        self.meshes = stems(&self.assets_dir, "obj");
        self.prefabs =
            scene2bin::load_prefab_lib(std::path::Path::new(&self.prefabs_dir)).unwrap_or_default();
        // Drop cached previews so edited `.obj` files re-parse on next draw.
        self.mesh_cache.clear();
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
        self.sel = Sel::None;
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
        self.sel = Sel::None;
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
}

impl eframe::App for EditorApp {
    // eframe 0.34 made `ui` the required entry point (`update` is deprecated).
    // We own the layout, so nest panels into the provided root `ui`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show_inside(ui, |ui| self.menu_bar(ui));
        egui::Panel::right("props")
            .resizable(true)
            .default_size(340.0)
            .show_inside(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show_inside(ui, |ui| match self.view_mode {
            ViewMode::TopDown => self.canvas(ui),
            ViewMode::Perspective => self.viewport(ui),
        });
    }
}
