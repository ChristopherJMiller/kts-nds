//! `scene-editor` — a desktop editor for Kill the Serpent *space* sidecars
//! (issue #27). It is a pure front-end over the `scene2bin` RON format: load a
//! `.ron`, drag instances around a top-down ground-plane (XZ) canvas, tweak
//! roles / meshes / camera / exits in a side panel, and save the `.ron` back.
//! The build pipeline (`scene2bin` → `.scene` → `bevy_nds_scene`) is unchanged,
//! and `preview-rom` remains the DS-faithful check — this tool is for fast
//! spatial layout, not pixel-accurate preview.
//!
//! Run it from this directory: `cargo run` (or `just edit` / `just edit <file>`
//! from the repo root).

#![windows_subsystem = "windows"]

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use scene2bin::{Camera, Exit, Instance, Material, Space};

const ARENA_HALF: f32 = 2.0;
/// Common roles offered in the role picker (free text still allowed).
const ROLES: &[&str] = &["avatar", "enemy", "landmark", "spawn", "prop"];

fn main() -> eframe::Result {
    let start_path = std::env::args().nth(1);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "kts · space editor",
        options,
        Box::new(move |_cc| Ok(Box::new(EditorApp::new(start_path)))),
    )
}

/// What the pointer is currently dragging (decided on drag-start so the grab
/// stays stable for the whole gesture).
#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Pan,
    Instance(usize),
    Waypoint(usize, usize),
}

/// The current selection (drives the properties panel).
#[derive(Clone, Copy, PartialEq)]
enum Sel {
    None,
    Instance(usize),
}

struct View {
    /// World-space (x, z) point at the canvas centre.
    center: Vec2,
    /// Pixels per world unit.
    scale: f32,
}

struct EditorApp {
    path: String,
    assets_dir: String,
    spaces_dir: String,
    space: Space,
    meshes: Vec<String>,
    spaces: Vec<String>,
    sel: Sel,
    drag: Drag,
    view: View,
    status: String,
}

impl EditorApp {
    fn new(start_path: Option<String>) -> Self {
        // Defaults assume the tool is run from `tools/scene-editor/` (its CWD),
        // so the repo's `assets/` is two levels up.
        let path = start_path.unwrap_or_else(|| "../../assets/spaces/atrium.ron".to_string());
        let mut app = Self {
            path,
            assets_dir: "../../assets".to_string(),
            spaces_dir: "../../assets/spaces".to_string(),
            space: empty_space(),
            meshes: Vec::new(),
            spaces: Vec::new(),
            sel: Sel::None,
            drag: Drag::None,
            view: View { center: Vec2::ZERO, scale: 90.0 },
            status: String::new(),
        };
        app.rescan();
        app.load();
        app
    }

    /// Refresh the mesh + neighbour-space lists from disk (for the pickers).
    fn rescan(&mut self) {
        self.meshes = stems(&self.assets_dir, "obj");
        self.spaces = stems(&self.spaces_dir, "ron");
    }

    fn load(&mut self) {
        match std::fs::read_to_string(&self.path) {
            Ok(src) => match scene2bin::parse_ron(&src) {
                Ok(space) => {
                    self.space = space;
                    self.sel = Sel::None;
                    self.status = format!("loaded {}", self.path);
                }
                Err(e) => self.status = format!("parse error: {e}"),
            },
            Err(e) => self.status = format!("could not read {}: {e}", self.path),
        }
    }

    fn save(&mut self) {
        // Validate first so the editor can't write a space that won't bake.
        let mesh_exists = |name: &str| self.meshes.iter().any(|m| m == name);
        if let Err(e) = scene2bin::validate(&self.space, mesh_exists) {
            self.status = format!("not saved — {e}");
            return;
        }
        match scene2bin::to_ron(&self.space) {
            Ok(text) => match std::fs::write(&self.path, text) {
                Ok(()) => {
                    let warns = scene2bin::validate_warnings(&self.space, |n| {
                        self.spaces.iter().any(|s| s == n)
                    });
                    self.status = if warns.is_empty() {
                        format!("saved {}", self.path)
                    } else {
                        format!("saved {} ({})", self.path, warns.join("; "))
                    };
                    self.rescan();
                }
                Err(e) => self.status = format!("write failed: {e}"),
            },
            Err(e) => self.status = format!("serialize failed: {e}"),
        }
    }

    fn world_to_screen(&self, rect: Rect, w: Vec2) -> Pos2 {
        let c = rect.center();
        Pos2::new(
            c.x + (w.x - self.view.center.x) * self.view.scale,
            c.y + (w.y - self.view.center.y) * self.view.scale,
        )
    }

    fn screen_to_world_delta(&self, d: Vec2) -> Vec2 {
        d / self.view.scale
    }

    fn inst_world(inst: &Instance) -> Vec2 {
        Vec2::new(inst.pos[0], inst.pos[2])
    }
}

impl eframe::App for EditorApp {
    // eframe 0.34 made `ui` the required entry point (`update` is deprecated).
    // We own the layout, so nest panels into the provided root `ui`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show_inside(ui, |ui| self.menu_bar(ui));
        egui::Panel::right("props")
            .resizable(true)
            .default_size(320.0)
            .show_inside(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show_inside(ui, |ui| self.canvas(ui));
    }
}

impl EditorApp {
    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("file:");
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(360.0));
            if ui.button("Load").clicked() {
                self.load();
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            if ui.button("New").clicked() {
                self.space = empty_space();
                self.sel = Sel::None;
                self.status = "new (unsaved) space".to_string();
            }
            ui.separator();
            if ui.button("Reset view").clicked() {
                self.view = View { center: Vec2::ZERO, scale: 90.0 };
            }
        });
        if !self.status.is_empty() {
            ui.label(egui::RichText::new(&self.status).weak());
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.camera_ui(ui);
            ui.separator();
            self.instance_list_ui(ui);
            ui.separator();
            self.selected_instance_ui(ui);
            ui.separator();
            self.exits_ui(ui);
        });
    }

    fn camera_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Camera");
        let mut tag = camera_tag(&self.space.camera);
        egui::ComboBox::from_id_salt("cam")
            .selected_text(tag)
            .show_ui(ui, |ui| {
                for t in ["Follow", "TopDown", "Rail2_5D", "CaptureFraming"] {
                    ui.selectable_value(&mut tag, t, t);
                }
            });
        if tag != camera_tag(&self.space.camera) {
            self.space.camera = default_camera(tag);
        }
        match &mut self.space.camera {
            Camera::Follow { height, dist, pitch } | Camera::Rail2_5D { height, dist, pitch } => {
                drag_row(ui, "height", height, 0.01);
                drag_row(ui, "dist", dist, 0.01);
                drag_row(ui, "pitch", pitch, 0.01);
            }
            Camera::TopDown { height } => drag_row(ui, "height", height, 0.01),
            Camera::CaptureFraming => {}
        }
    }

    fn instance_list_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Instances");
            if ui.button("+ add").clicked() {
                let idx = self.space.instances.len();
                self.space.instances.push(new_instance(self.view.center));
                self.sel = Sel::Instance(idx);
            }
        });
        let mut to_delete = None;
        for i in 0..self.space.instances.len() {
            let selected = self.sel == Sel::Instance(i);
            ui.horizontal(|ui| {
                let label = format!(
                    "{}  [{}]",
                    self.space.instances[i].role,
                    self.space.instances[i].mesh.as_deref().unwrap_or("—")
                );
                if ui.selectable_label(selected, label).clicked() {
                    self.sel = Sel::Instance(i);
                }
                if ui.small_button("✕").clicked() {
                    to_delete = Some(i);
                }
            });
        }
        if let Some(i) = to_delete {
            self.space.instances.remove(i);
            self.sel = Sel::None;
        }
    }

    fn selected_instance_ui(&mut self, ui: &mut egui::Ui) {
        let Sel::Instance(i) = self.sel else {
            ui.weak("(no instance selected)");
            return;
        };
        if i >= self.space.instances.len() {
            self.sel = Sel::None;
            return;
        }
        ui.heading("Selected");

        // role — common presets via combo, plus free text.
        let role = self.space.instances[i].role.clone();
        egui::ComboBox::from_id_salt("role")
            .selected_text(&role)
            .show_ui(ui, |ui| {
                for r in ROLES {
                    ui.selectable_value(&mut self.space.instances[i].role, r.to_string(), *r);
                }
            });
        ui.horizontal(|ui| {
            ui.label("role:");
            ui.text_edit_singleline(&mut self.space.instances[i].role);
        });

        // mesh — "(none)" or any baked .obj stem.
        let mesh_label = self.space.instances[i]
            .mesh
            .clone()
            .unwrap_or_else(|| "(none)".into());
        egui::ComboBox::from_id_salt("mesh")
            .selected_text(mesh_label)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(self.space.instances[i].mesh.is_none(), "(none)")
                    .clicked()
                {
                    self.space.instances[i].mesh = None;
                }
                for m in &self.meshes {
                    let is = self.space.instances[i].mesh.as_deref() == Some(m.as_str());
                    if ui.selectable_label(is, m).clicked() {
                        self.space.instances[i].mesh = Some(m.clone());
                    }
                }
            });

        let inst = &mut self.space.instances[i];
        ui.label("position (x, y, z)");
        vec3_row(ui, "pos", &mut inst.pos, 0.01);
        ui.label("rotation (rx, ry, rz)");
        vec3_row(ui, "rot", &mut inst.rot, 0.01);
        ui.label("scale");
        vec3_row(ui, "scale", &mut inst.scale, 0.005);

        // material
        let mut has_mat = inst.material.is_some();
        if ui.checkbox(&mut has_mat, "lit material").changed() {
            inst.material = has_mat.then_some(Material {
                diffuse: [200, 200, 210],
                ambient: [40, 40, 55],
            });
        }
        if let Some(m) = &mut inst.material {
            ui.horizontal(|ui| {
                ui.label("diffuse");
                ui.color_edit_button_srgb(&mut m.diffuse);
                ui.label("ambient");
                ui.color_edit_button_srgb(&mut m.ambient);
            });
        }

        drag_row(ui, "flags", &mut inst.flags, 1.0);

        // path (ground-plane waypoints)
        ui.horizontal(|ui| {
            ui.label(format!("path ({} pts)", inst.path.len()));
            if ui.button("+ wp").clicked() {
                let last = inst.path.last().copied().unwrap_or([inst.pos[0], inst.pos[2]]);
                inst.path.push(last);
            }
            if ui.button("− wp").clicked() {
                inst.path.pop();
            }
        });
    }

    fn exits_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Exits");
            if ui.button("+ add").clicked() {
                self.space.exits.push(Exit {
                    to: scene2bin::UNRESOLVED.to_string(),
                    at: [0.0, 0.0, 0.0],
                    gate: 0,
                });
            }
        });
        let mut to_delete = None;
        for i in 0..self.space.exits.len() {
            ui.horizontal(|ui| {
                let cur = self.space.exits[i].to.clone();
                egui::ComboBox::from_id_salt(("exit", i))
                    .selected_text(&cur)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.space.exits[i].to,
                            scene2bin::UNRESOLVED.to_string(),
                            scene2bin::UNRESOLVED,
                        );
                        for s in &self.spaces {
                            ui.selectable_value(&mut self.space.exits[i].to, s.clone(), s);
                        }
                    });
                if ui.small_button("✕").clicked() {
                    to_delete = Some(i);
                }
            });
            vec3_row(ui, &format!("at##{i}"), &mut self.space.exits[i].at, 0.01);
        }
        if let Some(i) = to_delete {
            self.space.exits.remove(i);
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(24, 27, 36));

        // zoom on scroll (about the canvas centre).
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 && resp.hovered() {
            self.view.scale = (self.view.scale * (1.0 + scroll * 0.001)).clamp(20.0, 400.0);
        }

        self.draw_grid(&painter, rect);

        // arena bounds.
        let a = self.world_to_screen(rect, Vec2::splat(-ARENA_HALF));
        let b = self.world_to_screen(rect, Vec2::splat(ARENA_HALF));
        painter.rect_stroke(
            Rect::from_two_pos(a, b),
            0.0,
            Stroke::new(1.0_f32, Color32::from_rgb(60, 66, 88)),
            egui::StrokeKind::Inside,
        );

        self.draw_paths(&painter, rect);
        self.draw_exits(&painter, rect);
        self.draw_instances(&painter, rect);
        self.handle_pointer(&resp, rect);
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: Rect) {
        let stroke = Stroke::new(1.0_f32, Color32::from_rgb(34, 38, 50));
        let axis = Stroke::new(1.0_f32, Color32::from_rgb(70, 78, 104));
        for w in -6..=6 {
            let v = w as f32;
            let vx = self.world_to_screen(rect, Vec2::new(v, 0.0)).x;
            let hy = self.world_to_screen(rect, Vec2::new(0.0, v)).y;
            painter.line_segment([Pos2::new(vx, rect.top()), Pos2::new(vx, rect.bottom())], stroke);
            painter.line_segment([Pos2::new(rect.left(), hy), Pos2::new(rect.right(), hy)], stroke);
        }
        let o = self.world_to_screen(rect, Vec2::ZERO);
        painter.line_segment([Pos2::new(rect.left(), o.y), Pos2::new(rect.right(), o.y)], axis);
        painter.line_segment([Pos2::new(o.x, rect.top()), Pos2::new(o.x, rect.bottom())], axis);
    }

    fn draw_paths(&self, painter: &egui::Painter, rect: Rect) {
        for inst in &self.space.instances {
            if inst.path.is_empty() {
                continue;
            }
            let pts: Vec<Pos2> = inst
                .path
                .iter()
                .map(|p| self.world_to_screen(rect, Vec2::new(p[0], p[1])))
                .collect();
            let stroke = Stroke::new(1.5_f32, Color32::from_rgb(150, 120, 60));
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], stroke);
            }
            // close the loop faintly (patrols cycle).
            if pts.len() > 2 {
                painter.line_segment(
                    [pts[pts.len() - 1], pts[0]],
                    Stroke::new(1.0_f32, Color32::from_rgb(90, 74, 40)),
                );
            }
            for p in &pts {
                painter.circle_filled(*p, 3.0, Color32::from_rgb(210, 170, 90));
            }
        }
    }

    fn draw_exits(&self, painter: &egui::Painter, rect: Rect) {
        for exit in &self.space.exits {
            let p = self.world_to_screen(rect, Vec2::new(exit.at[0], exit.at[2]));
            let col = Color32::from_rgb(110, 200, 140);
            painter.circle_stroke(p, 7.0, Stroke::new(1.5_f32, col));
            painter.text(
                p + Vec2::new(9.0, -9.0),
                Align2::LEFT_BOTTOM,
                format!("→{}", exit.to),
                FontId::proportional(11.0),
                col,
            );
        }
    }

    fn draw_instances(&self, painter: &egui::Painter, rect: Rect) {
        for (i, inst) in self.space.instances.iter().enumerate() {
            let p = self.world_to_screen(rect, Self::inst_world(inst));
            let (col, r) = role_style(&inst.role);
            let selected = self.sel == Sel::Instance(i);
            let outline = if selected {
                Stroke::new(2.0_f32, Color32::WHITE)
            } else {
                Stroke::new(1.0_f32, Color32::from_black_alpha(160))
            };
            match inst.role.as_str() {
                "enemy" => {
                    painter.add(Shape::convex_polygon(diamond(p, r), col, outline));
                }
                "landmark" | "prop" => {
                    painter.rect(
                        Rect::from_center_size(p, Vec2::splat(r * 1.7)),
                        0.0,
                        col,
                        outline,
                        egui::StrokeKind::Inside,
                    );
                }
                _ => {
                    painter.circle(p, r, col, outline);
                }
            }
            painter.text(
                p + Vec2::new(0.0, r + 2.0),
                Align2::CENTER_TOP,
                &inst.role,
                FontId::proportional(11.0),
                Color32::from_rgb(200, 205, 220),
            );
        }
    }

    /// Pick + drag instances and waypoints; otherwise pan. Click (no drag)
    /// selects the instance under the pointer (or clears).
    fn handle_pointer(&mut self, resp: &egui::Response, rect: Rect) {
        let pick_radius = 12.0;

        if resp.drag_started() {
            self.drag = Drag::Pan;
            if let Some(p) = resp.interact_pointer_pos() {
                // waypoints of the selected instance take priority (drawn on top).
                if let Sel::Instance(si) = self.sel {
                    if let Some(inst) = self.space.instances.get(si) {
                        for (wi, wp) in inst.path.iter().enumerate() {
                            let sp = self.world_to_screen(rect, Vec2::new(wp[0], wp[1]));
                            if sp.distance(p) <= pick_radius {
                                self.drag = Drag::Waypoint(si, wi);
                                break;
                            }
                        }
                    }
                }
                if self.drag == Drag::Pan {
                    if let Some(i) = self.pick_instance(rect, p, pick_radius) {
                        self.drag = Drag::Instance(i);
                        self.sel = Sel::Instance(i);
                    }
                }
            }
        }

        if resp.dragged() {
            let dw = self.screen_to_world_delta(resp.drag_delta());
            match self.drag {
                Drag::Pan => self.view.center -= dw,
                Drag::Instance(i) => {
                    if let Some(inst) = self.space.instances.get_mut(i) {
                        inst.pos[0] += dw.x;
                        inst.pos[2] += dw.y;
                    }
                }
                Drag::Waypoint(i, w) => {
                    if let Some(wp) =
                        self.space.instances.get_mut(i).and_then(|inst| inst.path.get_mut(w))
                    {
                        wp[0] += dw.x;
                        wp[1] += dw.y;
                    }
                }
                Drag::None => {}
            }
        }

        if resp.drag_stopped() {
            self.drag = Drag::None;
        }

        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.sel = match self.pick_instance(rect, p, pick_radius) {
                    Some(i) => Sel::Instance(i),
                    None => Sel::None,
                };
            }
        }
    }

    fn pick_instance(&self, rect: Rect, p: Pos2, radius: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, inst) in self.space.instances.iter().enumerate() {
            let sp = self.world_to_screen(rect, Self::inst_world(inst));
            let d = sp.distance(p);
            if d <= radius && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }
}

// --- small helpers -----------------------------------------------------------

fn empty_space() -> Space {
    Space {
        camera: Camera::default(),
        instances: Vec::new(),
        exits: Vec::new(),
    }
}

fn new_instance(at: Vec2) -> Instance {
    Instance {
        mesh: Some("cube".to_string()),
        role: "landmark".to_string(),
        pos: [at.x, 0.0, at.y],
        rot: [0.0, 0.0, 0.0],
        scale: [0.16, 0.16, 0.16],
        material: Some(Material { diffuse: [120, 120, 138], ambient: [34, 34, 44] }),
        flags: 0,
        path: Vec::new(),
    }
}

fn camera_tag(c: &Camera) -> &'static str {
    match c {
        Camera::Follow { .. } => "Follow",
        Camera::TopDown { .. } => "TopDown",
        Camera::Rail2_5D { .. } => "Rail2_5D",
        Camera::CaptureFraming => "CaptureFraming",
    }
}

fn default_camera(tag: &str) -> Camera {
    match tag {
        "TopDown" => Camera::TopDown { height: 3.2 },
        "Rail2_5D" => Camera::Rail2_5D { height: 1.7, dist: 2.0, pitch: -0.7 },
        "CaptureFraming" => Camera::CaptureFraming,
        _ => Camera::Follow { height: 1.7, dist: 2.0, pitch: -0.7 },
    }
}

/// (fill colour, radius) for an instance marker, keyed on role.
fn role_style(role: &str) -> (Color32, f32) {
    match role {
        "avatar" => (Color32::from_rgb(110, 180, 235), 8.0),
        "enemy" => (Color32::from_rgb(225, 80, 70), 8.0),
        "landmark" => (Color32::from_rgb(150, 150, 168), 7.0),
        "prop" => (Color32::from_rgb(120, 130, 120), 7.0),
        "spawn" => (Color32::from_rgb(110, 200, 140), 6.0),
        _ => (Color32::from_rgb(210, 210, 220), 6.0),
    }
}

fn diamond(c: Pos2, r: f32) -> Vec<Pos2> {
    vec![
        Pos2::new(c.x, c.y - r),
        Pos2::new(c.x + r, c.y),
        Pos2::new(c.x, c.y + r),
        Pos2::new(c.x - r, c.y),
    ]
}

fn drag_row<N: egui::emath::Numeric>(ui: &mut egui::Ui, label: &str, v: &mut N, speed: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(v).speed(speed));
    });
}

fn vec3_row(ui: &mut egui::Ui, label: &str, v: &mut [f32; 3], speed: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        for x in v.iter_mut() {
            ui.add(egui::DragValue::new(x).speed(speed));
        }
    });
}

/// List the file stems with `ext` under `dir`, sorted.
fn stems(dir: &str, ext: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some(ext))
                .then(|| p.file_stem().and_then(|s| s.to_str()).map(String::from))
                .flatten()
        })
        .collect();
    out.sort();
    out
}
