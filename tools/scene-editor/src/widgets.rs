//! Reusable side-panel rows + role/label/camera display helpers. Pure
//! presentation — no editor state.

use std::collections::BTreeMap;

use eframe::egui;
use egui::{Color32, Pos2, Vec2};
use scene2bin::{Camera, Instance, Level, Material, Placement, PrefabLib};

pub(crate) fn empty_level() -> Level {
    Level {
        name: String::new(),
        entry: String::new(),
        zones: BTreeMap::new(),
    }
}

pub(crate) fn new_instance(at: Vec2) -> Instance {
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

pub(crate) fn camera_tag(c: &Camera) -> &'static str {
    match c {
        Camera::Follow { .. } => "Follow",
        Camera::TopDown { .. } => "TopDown",
        Camera::Rail2_5D { .. } => "Rail2_5D",
        Camera::CaptureFraming => "CaptureFraming",
    }
}

pub(crate) fn default_camera(tag: &str) -> Camera {
    match tag {
        "TopDown" => Camera::TopDown { height: 3.2 },
        "Rail2_5D" => Camera::Rail2_5D { height: 1.7, dist: 2.0, pitch: -0.7 },
        "CaptureFraming" => Camera::CaptureFraming,
        _ => Camera::Follow { height: 1.7, dist: 2.0, pitch: -0.7 },
    }
}

/// (fill colour, radius) for an instance marker, keyed on role.
pub(crate) fn role_style(role: &str) -> (Color32, f32) {
    match role {
        "avatar" => (Color32::from_rgb(110, 180, 235), 8.0),
        "enemy" => (Color32::from_rgb(225, 80, 70), 8.0),
        "landmark" => (Color32::from_rgb(150, 150, 168), 7.0),
        "prop" => (Color32::from_rgb(120, 130, 120), 7.0),
        "spawn" => (Color32::from_rgb(110, 200, 140), 6.0),
        _ => (Color32::from_rgb(210, 210, 220), 6.0),
    }
}

pub(crate) fn diamond(c: Pos2, r: f32) -> Vec<Pos2> {
    vec![
        Pos2::new(c.x, c.y - r),
        Pos2::new(c.x + r, c.y),
        Pos2::new(c.x, c.y + r),
        Pos2::new(c.x - r, c.y),
    ]
}

pub(crate) fn drag_row<N: egui::emath::Numeric>(ui: &mut egui::Ui, label: &str, v: &mut N, speed: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::DragValue::new(v).speed(speed));
    });
}

pub(crate) fn vec3_row(ui: &mut egui::Ui, label: &str, v: &mut [f32; 3], speed: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        for x in v.iter_mut() {
            ui.add(egui::DragValue::new(x).speed(speed));
        }
    });
}

/// A `Some([f32;3])` row with a checkbox to toggle the override on/off.
pub(crate) fn opt_vec3_row(ui: &mut egui::Ui, label: &str, v: &mut Option<[f32; 3]>, speed: f64) {
    ui.horizontal(|ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, label).changed() {
            *v = on.then_some([0.0, 0.0, 0.0]);
        }
        if let Some(arr) = v {
            for x in arr.iter_mut() {
                ui.add(egui::DragValue::new(x).speed(speed));
            }
        }
    });
}

/// A `Some(u32)` flags row with a checkbox to toggle the override on/off.
pub(crate) fn opt_flags_row(ui: &mut egui::Ui, label: &str, v: &mut Option<u32>) {
    ui.horizontal(|ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, label).changed() {
            *v = on.then_some(0);
        }
        if let Some(f) = v {
            ui.add(egui::DragValue::new(f).speed(1.0));
        }
    });
}

/// A placement's effective role for display: a literal's own role, or the
/// prefab's role for a use (`?name` if the prefab is missing).
pub(crate) fn placement_role(p: &Placement, prefabs: &PrefabLib) -> String {
    match p {
        Placement::Lit(i) => i.role.clone(),
        Placement::Use { name, .. } => {
            prefabs.get(name).map(|pf| pf.role.clone()).unwrap_or_else(|| format!("?{name}"))
        }
    }
}

/// A placement's one-line label for the instance list.
pub(crate) fn placement_label(p: &Placement, prefabs: &PrefabLib) -> String {
    match p {
        Placement::Lit(i) => format!("{}  [{}]", i.role, i.mesh.as_deref().unwrap_or("—")),
        Placement::Use { name, .. } => format!("use {name}  ({})", placement_role(p, prefabs)),
    }
}

/// List the file stems with `ext` under `dir`, sorted.
pub(crate) fn stems(dir: &str, ext: &str) -> Vec<String> {
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
