//! Reusable side-panel rows + role/label/camera display helpers. Pure
//! presentation — no editor state.

use std::collections::BTreeMap;

use bevy_nds_3d_obj::PreviewMesh;
use eframe::egui;
use egui::{Color32, Pos2, Sense, Stroke, Vec2};
use scene2bin::schema::Role;
use scene2bin::{Camera, Instance, Level, Material, Placement, Prefab, PrefabLib};

/// A sensible starting prefab for the in-editor prefab editor (#51).
pub(crate) fn default_prefab() -> Prefab {
    Prefab {
        mesh: Some("cube".to_string()),
        role: Role::Prop.as_str().to_string(),
        kind: None,
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

/// A solid instance's **blocking footprint** in the zone's local XZ frame (#12)
/// — what the runtime will actually collide, as opposed to the glyph that marks
/// where the instance sits.
///
/// Derived, not authored: half extents are the baked mesh AABB × the authored
/// scale, and the orientation is `rot.y` alone. That is exactly what
/// `kts::collide::harvest` does at runtime, which is the point — the canvas
/// draws the collider, not an approximation of it.
pub(crate) struct Footprint {
    pub center: Vec2,
    pub half: Vec2,
    pub yaw: f32,
    /// `kind: "round"` — collides as a disc of `half.x`, not a rectangle.
    pub round: bool,
    /// `kind: "ramp"` — rises along local +Z.
    pub ramp: bool,
}

/// The blocking footprint of `inst`, or `None` when it isn't solid or its mesh
/// has no thumbnail yet (an unparsed `.obj`, or one still being scanned).
pub(crate) fn footprint(inst: &Instance, thumb: Option<&MeshThumb>) -> Option<Footprint> {
    if !matches!(Role::parse(&inst.role), Some(Role::Landmark | Role::Block)) {
        return None;
    }
    let t = thumb?;
    let kind = inst.kind.as_deref().unwrap_or_default();
    Some(Footprint {
        center: Vec2::new(inst.pos[0], inst.pos[2]),
        // Magnitudes, matching `kts::collide::harvest`: a mirrored mesh
        // (negative `scale.x`/`scale.z`) occupies the same rectangle, and the
        // runtime takes the `abs` too — so the overlay draws the box the avatar
        // actually collides with rather than an inside-out one. (The bake rejects
        // a non-positive solid scale outright; this keeps the canvas honest while
        // the value is being dragged through zero.)
        half: Vec2::new(
            (t.size[0] * inst.scale[0] * 0.5).abs(),
            (t.size[2] * inst.scale[2] * 0.5).abs(),
        ),
        yaw: inst.rot[1],
        // A landmark is always a box, so its `kind` is always absent.
        round: kind == "round",
        ramp: kind == "ramp",
    })
}

/// The footprint's **world-axis** half extents: how far it actually reaches
/// along ±X and ±Z once `yaw` is applied.
///
/// Exact, not the circumradius. For a rectangle the reach along world X is
/// `|hx·cos| + |hz·sin|` (and symmetrically for Z), which collapses to `(hx,
/// hz)` for an axis-aligned box — whereas the circumradius reports
/// `sqrt(hx² + hz²)` for *every* yaw and so over-states the reach of anything
/// not turned 45°, flagging solids whose rectangle never leaves the zone. A
/// disc reaches `half.x` on both axes.
pub(crate) fn footprint_extent(f: &Footprint) -> Vec2 {
    if f.round {
        return Vec2::splat(f.half.x);
    }
    let (sin, cos) = (f.yaw.sin().abs(), f.yaw.cos().abs());
    Vec2::new(
        f.half.x * cos + f.half.y * sin,
        f.half.y * cos + f.half.x * sin,
    )
}

/// Is the zone-local point `p` inside `f`? The un-inflated footprint (no body
/// radius) — the editor asks this about authored points, not about the avatar.
pub(crate) fn inside_footprint(f: &Footprint, p: Vec2) -> bool {
    let d = p - f.center;
    if f.round {
        return d.length() <= f.half.x;
    }
    let (sin, cos) = (f.yaw.sin(), f.yaw.cos());
    // World → local: the inverse of the renderer's `R_y(+yaw)`, the same
    // transform `bevy_nds_collide::local` uses (local +Z points at `(sin, cos)`).
    let lx = d.x * cos - d.y * sin;
    let lz = d.x * sin + d.y * cos;
    lx.abs() <= f.half.x && lz.abs() <= f.half.y
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
        role: Role::Landmark.as_str().to_string(),
        kind: None,
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

/// An unparseable role's tint: loud magenta, so a stale or hand-typed role
/// reads as *broken* on the canvas rather than as a slightly-different grey.
pub(crate) const BAD_ROLE_COLOR: Color32 = Color32::from_rgb(235, 60, 220);

/// (fill colour, radius) for an instance marker, keyed on role. Takes the raw
/// authored string (a prefab `Use` resolves to one; see [`placement_role`]) and
/// matches the [`Role`] vocabulary exhaustively — an unknown role gets
/// [`BAD_ROLE_COLOR`].
pub(crate) fn role_style(role: &str) -> (Color32, f32) {
    match Role::parse(role) {
        Some(Role::Avatar) => (Color32::from_rgb(110, 180, 235), 8.0),
        Some(Role::Enemy) => (Color32::from_rgb(225, 80, 70), 8.0),
        Some(Role::Landmark) => (Color32::from_rgb(150, 150, 168), 7.0),
        Some(Role::Block) => (Color32::from_rgb(110, 116, 130), 7.0),
        Some(Role::Prop) => (Color32::from_rgb(120, 130, 120), 7.0),
        None => (BAD_ROLE_COLOR, 6.0),
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

/// Draw the named-bit checkboxes for `flags`, plus a raw value for anything
/// hand-set (#54). Only the bits `role` may actually carry
/// ([`Role::allowed_flags`]) are offered, and the raw `DragValue` is masked to
/// the same set — so the editor can't author a state the bake would reject.
pub(crate) fn named_flags_ui(ui: &mut egui::Ui, label: &str, flags: &mut u32, role: Role) {
    let allowed = role.allowed_flags();
    ui.label(label);
    if allowed == 0 {
        // Display only — never rewrite the document just because it was drawn.
        // A stray bit is a bake Error the problems panel already reports, and it
        // is cleared by an actual user edit, not by looking at the instance.
        ui.weak("(no authorable flags for this role)");
        if *flags != 0 {
            ui.colored_label(BAD_ROLE_COLOR, format!("raw {:#x} — won't bake", *flags));
        }
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (bit, name) in scene2bin::schema::flag_bits::NAMED {
            if allowed & bit == 0 {
                continue;
            }
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
        if ui.add(egui::DragValue::new(flags).speed(1.0)).changed() {
            *flags &= allowed;
        }
    });
}

/// A named-bit flags editor for a `Some(u32)` override (the prefab-`Use` case),
/// gated behind an on/off checkbox (#54). `role` is the *effective* role — the
/// prefab's, for a `Use` — so the same allowed-bit mask applies.
pub(crate) fn opt_named_flags_ui(ui: &mut egui::Ui, label: &str, v: &mut Option<u32>, role: Role) {
    let mut on = v.is_some();
    if ui.checkbox(&mut on, label).changed() {
        *v = on.then_some(0);
    }
    if let Some(f) = v {
        named_flags_ui(ui, "bits", f, role);
    }
}

/// A role picker plus the role-scoped `kind` combo, shared by the
/// literal-instance and prefab editors. Clears `kind` when the chosen role has
/// no kinds, so the two fields can never disagree.
///
/// `allow_avatar` widens the list from [`Role::AUTHORABLE`] to [`Role::ALL`]:
/// the bake demands exactly one `avatar` and it must sit in the level's entry
/// zone, so only a literal in *that* zone may pick it (a prefab never can — a
/// prefab is a reusable template and the avatar is unique). The instance's
/// **current** role is always offered as well, so an existing avatar stays
/// selectable after a mis-click instead of being a one-way trip out of the
/// vocabulary.
pub(crate) fn role_kind_ui(
    ui: &mut egui::Ui,
    id: &str,
    role: &mut String,
    kind: &mut Option<String>,
    allow_avatar: bool,
) {
    let choices: &[Role] = if allow_avatar {
        Role::ALL
    } else {
        Role::AUTHORABLE
    };
    ui.horizontal(|ui| {
        ui.label("role:");
        egui::ComboBox::from_id_salt(format!("{id}-role"))
            .selected_text(role.clone())
            .show_ui(ui, |ui| {
                for r in choices {
                    ui.selectable_value(role, r.as_str().to_string(), r.as_str());
                }
                // Keep the way back: a role this picker wouldn't offer fresh
                // (an `avatar` being edited outside the entry zone) is still
                // re-selectable, so no single click is irreversible.
                if let Some(cur) = Role::parse(role)
                    && !choices.contains(&cur)
                {
                    ui.selectable_value(role, cur.as_str().to_string(), cur.as_str());
                }
            });
    });

    let kinds = Role::parse(role).map(|r| r.kinds()).unwrap_or(&[]);
    if kinds.is_empty() {
        *kind = None;
        return;
    }
    ui.horizontal(|ui| {
        ui.label("kind:");
        egui::ComboBox::from_id_salt(format!("{id}-kind"))
            .selected_text(kind.clone().unwrap_or_else(|| "(default)".into()))
            .show_ui(ui, |ui| {
                if ui.selectable_label(kind.is_none(), "(default)").clicked() {
                    *kind = None;
                }
                for k in kinds {
                    if ui
                        .selectable_label(kind.as_deref() == Some(*k), *k)
                        .clicked()
                    {
                        *kind = Some((*k).to_string());
                    }
                }
            });
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(size: [f32; 3]) -> MeshThumb {
        MeshThumb {
            size,
            edges: Vec::new(),
        }
    }

    fn block(kind: Option<&str>) -> Instance {
        let mut i = new_instance(Vec2::ZERO);
        i.role = Role::Block.as_str().to_string();
        i.mesh = Some("cube".to_string());
        i.kind = kind.map(String::from);
        i
    }

    /// The canvas overlay must draw the collider the runtime will build: half
    /// extents are the **baked** mesh AABB × the authored scale, not the glyph
    /// radius and not the raw mesh size.
    #[test]
    fn footprint_matches_baked_extents() {
        // A unit `cube` at the shipped landmark scale: half extent 0.08, which
        // is exactly what `LANDMARK_COLLIDE = 0.26` minus the 0.18 body radius
        // used to encode.
        let unit = thumb([1.0, 1.0, 1.0]);
        let mut inst = block(None);
        inst.scale = [0.16, 0.16, 0.16];
        inst.pos = [1.25, 0.0, -0.95];
        inst.rot = [0.0, 0.3, 0.0];
        let f = footprint(&inst, Some(&unit)).expect("a block with a thumb is solid");
        assert!((f.half.x - 0.08).abs() < 1e-6, "{:?}", f.half);
        assert!((f.half.y - 0.08).abs() < 1e-6, "{:?}", f.half);
        assert_eq!(f.center, Vec2::new(1.25, -0.95));
        assert!((f.yaw - 0.3).abs() < 1e-6);
        assert!(!f.round && !f.ramp);

        // Non-uniform scale on a non-unit mesh: X and Z are independent, and Y
        // (the thumb's `size[1]`) never enters the ground-plane footprint.
        let oblong = thumb([2.0, 9.0, 0.5]);
        let mut inst = block(None);
        inst.scale = [0.4, 0.24, 0.7];
        let f = footprint(&inst, Some(&oblong)).unwrap();
        assert!((f.half.x - 0.4).abs() < 1e-6, "{:?}", f.half);
        assert!((f.half.y - 0.175).abs() < 1e-6, "{:?}", f.half);

        // Kinds map onto the shape flags.
        assert!(footprint(&block(Some("round")), Some(&unit)).unwrap().round);
        assert!(footprint(&block(Some("ramp")), Some(&unit)).unwrap().ramp);
        assert!(!footprint(&block(Some("ramp")), Some(&unit)).unwrap().round);

        // A landmark is solid (and always a box); a prop and an enemy are not,
        // and nothing without a loaded mesh thumbnail has a footprint at all.
        let mut lm = block(None);
        lm.role = Role::Landmark.as_str().to_string();
        assert!(footprint(&lm, Some(&unit)).is_some());
        for role in [Role::Prop, Role::Enemy, Role::Avatar] {
            let mut i = block(None);
            i.role = role.as_str().to_string();
            assert!(footprint(&i, Some(&unit)).is_none(), "{role:?}");
        }
        assert!(footprint(&block(None), None).is_none());

        // Mirroring (a negative scale) is a normal authoring drag, and the
        // rectangle it makes is the same rectangle — the overlay must draw the
        // box the runtime collides with, not an inside-out one. `harvest` takes
        // the same `abs`.
        let mut mirrored = block(None);
        mirrored.scale = [-0.4, 0.24, -0.7];
        let f = footprint(&mirrored, Some(&oblong)).unwrap();
        assert!((f.half.x - 0.4).abs() < 1e-6, "{:?}", f.half);
        assert!((f.half.y - 0.175).abs() < 1e-6, "{:?}", f.half);
    }

    /// The bounds-overhang warning tests the footprint's real reach on each
    /// world axis; the circumradius it used to test over-states everything that
    /// isn't turned 45°.
    #[test]
    fn footprint_extent_is_exact_per_axis() {
        let oblong = thumb([1.0, 1.0, 1.0]);
        let mut inst = block(None);
        inst.scale = [0.8, 0.2, 0.4]; // half extents (0.4, 0.2)

        // Axis aligned: exactly the half extents (circumradius would say 0.447).
        let e = footprint_extent(&footprint(&inst, Some(&oblong)).unwrap());
        assert!((e.x - 0.4).abs() < 1e-6, "{e:?}");
        assert!((e.y - 0.2).abs() < 1e-6, "{e:?}");

        // A quarter turn swaps the axes, still exact.
        inst.rot = [0.0, std::f32::consts::FRAC_PI_2, 0.0];
        let e = footprint_extent(&footprint(&inst, Some(&oblong)).unwrap());
        assert!((e.x - 0.2).abs() < 1e-6, "{e:?}");
        assert!((e.y - 0.4).abs() < 1e-6, "{e:?}");

        // In between, the reach is |hx·cos| + |hz·sin| — never more than the
        // circumradius of the *square* that bounds it, and never less than the
        // larger half extent.
        inst.rot = [0.0, 0.6, 0.0];
        let e = footprint_extent(&footprint(&inst, Some(&oblong)).unwrap());
        let (s, c) = (0.6_f32.sin(), 0.6_f32.cos());
        assert!((e.x - (0.4 * c + 0.2 * s)).abs() < 1e-6, "{e:?}");
        assert!((e.y - (0.2 * c + 0.4 * s)).abs() < 1e-6, "{e:?}");
        assert!(e.x >= 0.4 && e.y >= 0.2, "{e:?}");

        // A disc reaches its radius on both axes whatever the yaw.
        let mut round = block(Some("round"));
        round.scale = [0.8, 0.2, 0.4];
        round.rot = [0.0, 0.6, 0.0];
        let e = footprint_extent(&footprint(&round, Some(&oblong)).unwrap());
        assert!(
            (e.x - 0.4).abs() < 1e-6 && (e.y - 0.4).abs() < 1e-6,
            "{e:?}"
        );
    }

    #[test]
    fn inside_footprint_respects_yaw_and_shape() {
        let unit = thumb([1.0, 1.0, 1.0]);
        let mut inst = block(None);
        inst.scale = [2.0, 1.0, 0.5]; // half extents (1.0, 0.25)
        let f = footprint(&inst, Some(&unit)).unwrap();
        assert!(inside_footprint(&f, Vec2::new(0.9, 0.2)));
        assert!(!inside_footprint(&f, Vec2::new(0.9, 0.4)));

        // Turned a quarter turn, the long axis is now Z.
        inst.rot = [0.0, std::f32::consts::FRAC_PI_2, 0.0];
        let f = footprint(&inst, Some(&unit)).unwrap();
        assert!(inside_footprint(&f, Vec2::new(0.2, 0.9)));
        assert!(!inside_footprint(&f, Vec2::new(0.9, 0.2)));

        // A non-symmetric yaw, which a mirrored convention cannot fake (±90°
        // and 180° are their own mirrors, so the quarter turn above passes
        // either way). Local +X points at world `(cos, −sin)` — the renderer's
        // `R_y(+yaw)`, the same heading the rotate gizmo's arrow draws — so a
        // point 0.9 along the long axis is inside and its mirror is not.
        let yaw = 0.5_f32;
        inst.rot = [0.0, yaw, 0.0];
        let f = footprint(&inst, Some(&unit)).unwrap();
        let along = Vec2::new(0.9 * yaw.cos(), -0.9 * yaw.sin());
        assert!(inside_footprint(&f, along), "{along:?}");
        assert!(
            !inside_footprint(&f, Vec2::new(along.x, -along.y)),
            "the mirrored heading must be outside the long axis"
        );

        // A round block is a disc of `half.x`, so its corners are outside.
        let mut r = block(Some("round"));
        r.scale = [2.0, 1.0, 2.0]; // radius 1.0
        let f = footprint(&r, Some(&unit)).unwrap();
        assert!(inside_footprint(&f, Vec2::new(0.7, 0.7)));
        assert!(!inside_footprint(&f, Vec2::new(0.8, 0.8)));
    }
}
