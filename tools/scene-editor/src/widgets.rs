//! Reusable side-panel rows + role/label/camera display helpers. Pure
//! presentation — no editor state.

use std::collections::BTreeMap;

use bevy_nds_3d_obj::PreviewMesh;
use eframe::egui;
use egui::{Color32, Pos2, Sense, Stroke, Vec2};
use scene2bin::{Camera, Instance, Level, Material, Placement, Prefab, PrefabLib};

/// A sensible starting prefab for the in-editor prefab editor (#51).
pub(crate) fn default_prefab() -> Prefab {
    Prefab {
        mesh: Some("cube".to_string()),
        role: "prop".to_string(),
        rot: [0.0, 0.0, 0.0],
        scale: [0.16, 0.16, 0.16],
        material: Some(Material {
            diffuse: [120, 120, 138],
            ambient: [34, 34, 44],
        }),
        flags: 0,
        path: Vec::new(),
    }
}

/// A cheap iso-projected wireframe of a mesh for the picker preview (#52),
/// normalised into the unit square so it maps onto any thumbnail rect.
pub(crate) struct MeshThumb {
    /// The mesh's world-space AABB extent (x, y, z), shown as a tooltip.
    pub(crate) size: [f32; 3],
    /// Triangle edges in `[0,1]²`.
    pub(crate) edges: Vec<[Pos2; 2]>,
}

/// Build a [`MeshThumb`] from a parsed preview mesh: iso-project the triangle
/// edges (capped for cost) and fit them to the unit square.
pub(crate) fn build_thumb(mesh: &PreviewMesh) -> MeshThumb {
    let [mn, mx] = mesh.aabb;
    let size = [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]];
    // Isometric: x-right/z-back at 30°, y up.
    let a = 0.5236_f32; // 30°
    let (cos, sin) = (a.cos(), a.sin());
    let proj = |p: [f32; 3]| ((p[0] - p[2]) * cos, (p[0] + p[2]) * sin - p[1]);

    let cap = mesh.tris.len().min(600);
    let mut raw: Vec<[(f32, f32); 3]> = Vec::with_capacity(cap);
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for t in mesh.tris.iter().take(cap) {
        let tri = [proj(t.pos[0]), proj(t.pos[1]), proj(t.pos[2])];
        for (x, y) in tri {
            lo_x = lo_x.min(x);
            lo_y = lo_y.min(y);
            hi_x = hi_x.max(x);
            hi_y = hi_y.max(y);
        }
        raw.push(tri);
    }
    let fit = 1.0 / (hi_x - lo_x).max(hi_y - lo_y).max(1e-3);
    // Centre the (possibly non-square) projection inside the unit box.
    let (ox, oy) = (
        (1.0 - (hi_x - lo_x) * fit) * 0.5,
        (1.0 - (hi_y - lo_y) * fit) * 0.5,
    );
    let map = |(x, y): (f32, f32)| Pos2::new((x - lo_x) * fit + ox, (y - lo_y) * fit + oy);
    let mut edges = Vec::with_capacity(raw.len() * 3);
    for [a, b, c] in raw {
        edges.push([map(a), map(b)]);
        edges.push([map(b), map(c)]);
        edges.push([map(c), map(a)]);
    }
    MeshThumb { size, edges }
}

/// A small square wireframe preview of `thumb` (or an empty tile) (#52).
pub(crate) fn thumb_widget(ui: &mut egui::Ui, thumb: Option<&MeshThumb>, px: f32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(px), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_rgb(30, 34, 46));
    if let Some(t) = thumb {
        let inset = rect.shrink(3.0);
        let st = Stroke::new(1.0_f32, Color32::from_rgb(150, 172, 205));
        for [a, b] in &t.edges {
            let pa = inset.min + Vec2::new(a.x * inset.width(), a.y * inset.height());
            let pb = inset.min + Vec2::new(b.x * inset.width(), b.y * inset.height());
            painter.line_segment([pa, pb], st);
        }
        resp.on_hover_text(format!(
            "{:.2} × {:.2} × {:.2}",
            t.size[0], t.size[1], t.size[2]
        ));
    }
}

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
        material: Some(Material {
            diffuse: [120, 120, 138],
            ambient: [34, 34, 44],
        }),
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
        "Rail2_5D" => Camera::Rail2_5D {
            height: 1.7,
            dist: 2.0,
            pitch: -0.7,
        },
        "CaptureFraming" => Camera::CaptureFraming,
        _ => Camera::Follow {
            height: 1.7,
            dist: 2.0,
            pitch: -0.7,
        },
    }
}

/// (fill colour, radius) for an instance marker, keyed on role.
pub(crate) fn role_style(role: &str) -> (Color32, f32) {
    match role {
        "avatar" => (Color32::from_rgb(110, 180, 235), 8.0),
        "enemy" => (Color32::from_rgb(225, 80, 70), 8.0),
        "landmark" => (Color32::from_rgb(150, 150, 168), 7.0),
        "block" => (Color32::from_rgb(110, 116, 130), 7.0),
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

pub(crate) fn drag_row<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut N,
    speed: f64,
) {
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

/// The named bits of the instance `flags: u32`, per the #27 (2026-07-11) generalized
/// flag model. Semantics live in the game/runtime; the editor only *names the
/// authorable bits* (#54). Extend as the reserved bits (vuln-state, item/supply,
/// tether) get defined.
pub(crate) const FLAG_BITS: &[(u32, &str)] = &[
    (0x1, "OBJECTIVE"),       // gate objective — counts toward its zone's clear_flag
    (0x2, "LEVEL_OBJECTIVE"), // freeform capture — rolls up to the level exit
];

/// Draw the named-bit checkboxes for `flags`, plus a raw value for undefined
/// bits (#54). Returns nothing; edits `flags` in place.
pub(crate) fn named_flags_ui(ui: &mut egui::Ui, label: &str, flags: &mut u32) {
    ui.label(label);
    ui.horizontal_wrapped(|ui| {
        for (bit, name) in FLAG_BITS {
            let mut on = *flags & bit != 0;
            if ui.checkbox(&mut on, *name).changed() {
                if on {
                    *flags |= bit;
                } else {
                    *flags &= !bit;
                }
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("raw u32");
        ui.add(egui::DragValue::new(flags).speed(1.0));
    });
}

/// A named-bit flags editor for a `Some(u32)` override (the prefab-`Use` case),
/// gated behind an on/off checkbox (#54).
pub(crate) fn opt_named_flags_ui(ui: &mut egui::Ui, label: &str, v: &mut Option<u32>) {
    let mut on = v.is_some();
    if ui.checkbox(&mut on, label).changed() {
        *v = on.then_some(0);
    }
    if let Some(f) = v {
        named_flags_ui(ui, "bits", f);
    }
}

/// A placement's effective role for display: a literal's own role, or the
/// prefab's role for a use (`?name` if the prefab is missing).
pub(crate) fn placement_role(p: &Placement, prefabs: &PrefabLib) -> String {
    match p {
        Placement::Lit(i) => i.role.clone(),
        Placement::Use { name, .. } => prefabs
            .get(name)
            .map(|pf| pf.role.clone())
            .unwrap_or_else(|| format!("?{name}")),
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
