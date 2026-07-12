//! The 3D preview viewport (#40): a self-contained software renderer that draws
//! the level's resolved instances in perspective straight onto egui's 2D painter
//! — no GL context, no extra heavy deps. Meshes come from
//! [`bevy_nds_3d_obj::obj_preview_mesh`] (the same OBJ reader the ROM baker uses),
//! and transforms apply the runtime's Euler-XYZ rotation so the preview matches
//! the DS. Solid mode uses the painter's algorithm (triangles sorted far→near)
//! with flat directional shading; wireframe draws triangle edges. Orbit with the
//! left button, pan with the right, zoom on scroll; click selects an instance
//! (shared with the top-down canvas).

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use scene2bin::{Camera, Instance, ZoneEntry};

use crate::app::{EditorApp, Sel};
use crate::widgets::role_style;

/// Orbit camera for the viewport: spherical coordinates around `target`.
pub(crate) struct OrbitCam {
    pub target: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
}

impl Default for OrbitCam {
    fn default() -> Self {
        Self {
            target: [0.0, 0.0, 0.0],
            yaw: 0.7,
            pitch: 0.55,
            dist: 9.0,
        }
    }
}

/// Ground-plane Y for the per-zone floor. Mirrors `src/main.rs`'s `GROUND_Y`
/// (meshes are centred on their origin at bake, so the floor sits a half-object
/// below) so the preview floor lines up with the ROM.
const GROUND_Y: f32 = -0.16;

// --- tiny f32 vec3 helpers (the editor is host std; no need for the DS Fx32) ---
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scl(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l > 1e-6 {
        scl(a, 1.0 / l)
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Euler-XYZ rotation matching the runtime `Transform3d` (apply X, then Y, then
/// Z). Keep in step with `bevy_nds_3d`'s model matrix so previews don't lie.
/// A representative camera rig for a zone's authored mode (#50): `(eye, forward,
/// up, target)`, target = zone ground-plane centre. Follow/Rail tilt by `pitch`;
/// TopDown looks straight down; CaptureFraming is a fixed 3/4 pose.
fn camera_pose(entry: &ZoneEntry) -> ([f32; 3], [f32; 3], [f32; 3], [f32; 3]) {
    let cx = entry.place[0] + (entry.bounds.min[0] + entry.bounds.max[0]) * 0.5;
    let cz = entry.place[1] + (entry.bounds.min[1] + entry.bounds.max[1]) * 0.5;
    let t = [cx, 0.0, cz];
    let rot_x = |v: [f32; 3], a: f32| {
        let (s, c) = (a.sin(), a.cos());
        [v[0], v[1] * c - v[2] * s, v[1] * s + v[2] * c]
    };
    match entry.camera {
        Camera::Follow {
            height,
            dist,
            pitch,
        }
        | Camera::Rail2_5D {
            height,
            dist,
            pitch,
        } => {
            let eye = [t[0], t[1] + height, t[2] + dist];
            (
                eye,
                rot_x([0.0, 0.0, -1.0], pitch),
                rot_x([0.0, 1.0, 0.0], pitch),
                t,
            )
        }
        Camera::TopDown { height } => (
            [t[0], t[1] + height, t[2] + 0.001],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, -1.0],
            t,
        ),
        Camera::CaptureFraming => {
            let eye = [t[0], t[1] + 2.2, t[2] + 2.8];
            (eye, norm(sub(t, eye)), [0.0, 1.0, 0.0], t)
        }
    }
}

fn rot_xyz(v: [f32; 3], r: [f32; 3]) -> [f32; 3] {
    let (sx, cx) = r[0].sin_cos();
    let v = [v[0], v[1] * cx - v[2] * sx, v[1] * sx + v[2] * cx];
    let (sy, cy) = r[1].sin_cos();
    let v = [v[0] * cy + v[2] * sy, v[1], -v[0] * sy + v[2] * cy];
    let (sz, cz) = r[2].sin_cos();
    [v[0] * cz - v[1] * sz, v[0] * sz + v[1] * cz, v[2]]
}

/// A precomputed perspective projection for one frame.
struct Proj {
    eye: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fwd: [f32; 3],
    focal: f32,
    center: Pos2,
}

impl Proj {
    fn new(cam: &OrbitCam, rect: Rect) -> Self {
        let (sy, cy) = cam.yaw.sin_cos();
        let (sp, cp) = cam.pitch.sin_cos();
        // Direction from target out to the eye.
        let dir = [cp * sy, sp, cp * cy];
        let eye = add(cam.target, scl(dir, cam.dist));
        let fwd = norm(sub(cam.target, eye));
        let right = norm(cross(fwd, [0.0, 1.0, 0.0]));
        let up = cross(right, fwd);
        // ~60° vertical FOV: focal ≈ (h/2) / tan(fov/2).
        let focal = rect.height() * 0.85;
        Self {
            eye,
            right,
            up,
            fwd,
            focal,
            center: rect.center(),
        }
    }

    /// Project a world point to screen + view-space depth, or `None` if it sits
    /// behind the near plane.
    fn project(&self, p: [f32; 3]) -> Option<(Pos2, f32)> {
        let rel = sub(p, self.eye);
        let vz = dot(rel, self.fwd);
        if vz < 0.05 {
            return None;
        }
        let vx = dot(rel, self.right);
        let vy = dot(rel, self.up);
        Some((
            Pos2::new(
                self.center.x + vx * self.focal / vz,
                self.center.y - vy * self.focal / vz,
            ),
            vz,
        ))
    }
}

/// One resolved instance ready to draw (prefab `Use`s already expanded).
struct RenderItem {
    place: Vec2,
    inst: Instance,
    active: bool,
}

/// Base colour for an instance: its material diffuse if set, else its role tint.
fn base_color(inst: &Instance) -> Color32 {
    match &inst.material {
        Some(m) => Color32::from_rgb(m.diffuse[0], m.diffuse[1], m.diffuse[2]),
        None => role_style(&inst.role).0,
    }
}

fn shade(c: Color32, s: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * s) as u8,
        (c.g() as f32 * s) as u8,
        (c.b() as f32 * s) as u8,
    )
}

impl EditorApp {
    pub(crate) fn viewport(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(18, 20, 28));

        // --- camera controls ---
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            self.cam3.yaw -= d.x * 0.01;
            self.cam3.pitch = (self.cam3.pitch + d.y * 0.01).clamp(-1.5, 1.5);
        }
        if resp.dragged_by(egui::PointerButton::Secondary) {
            let d = resp.drag_delta();
            let proj = Proj::new(&self.cam3, rect);
            let k = self.cam3.dist * 0.0015;
            self.cam3.target = add(
                self.cam3.target,
                add(scl(proj.right, -d.x * k), scl(proj.up, d.y * k)),
            );
        }
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 && resp.hovered() {
            self.cam3.dist = (self.cam3.dist * (1.0 - scroll * 0.001)).clamp(1.0, 80.0);
        }

        let proj = Proj::new(&self.cam3, rect);

        self.draw_ground_grid(&painter, &proj);

        // Phase 1: resolve every placement (immutable borrow of level/contents/prefabs).
        let mut items: Vec<RenderItem> = Vec::new();
        for (stem, entry) in &self.level.zones {
            let active = self.active.as_deref() == Some(stem.as_str());
            let place = Vec2::new(entry.place[0], entry.place[1]);
            if let Some(zone) = self.contents.get(stem) {
                for p in &zone.instances {
                    if let Ok(inst) = scene2bin::resolve_placement(p, &self.prefabs) {
                        items.push(RenderItem {
                            place,
                            inst,
                            active,
                        });
                    }
                }
            }
        }

        // Phase 2: make sure every referenced mesh is parsed + cached (mutable).
        for it in &items {
            if let Some(name) = &it.inst.mesh {
                self.mesh_preview(name);
            }
        }

        // Phase 3: build triangles / markers (cache read immutably).
        let light = norm([0.4, 1.0, 0.5]);
        let mut tris: Vec<(f32, [Pos2; 3], Color32)> = Vec::new();

        // Per-zone floor as real geometry in the same depth batch: a flat XZ quad
        // sized to the zone's bounds at `GROUND_Y` (mirrors the runtime
        // `spawn_zone_floor`), but **tessellated into unit cells** so the
        // painter's-algorithm centroid sort works — two huge triangles mis-sort
        // against small object triangles (the floor pops in front of a cube); cell
        // triangles are comparable in size, so the floor occludes and is occluded
        // correctly. A faint checkerboard gives the plane spatial cues (grid lines
        // can't depth-test; cell colours can).
        if !self.wireframe {
            let s = (0.35 + 0.65 * dot([0.0, 1.0, 0.0], light).max(0.0)).clamp(0.0, 1.0);
            for (stem, entry) in &self.level.zones {
                let dim = if self.active.as_deref() == Some(stem.as_str()) {
                    1.0
                } else {
                    0.5
                };
                let (px, pz) = (entry.place[0], entry.place[1]);
                let b = &entry.bounds;
                let (x0, z0) = (px + b.min[0], pz + b.min[1]);
                let (x1, z1) = (px + b.max[0], pz + b.max[1]);
                let nx = (x1 - x0).ceil().max(1.0) as i32;
                let nz = (z1 - z0).ceil().max(1.0) as i32;
                for ix in 0..nx {
                    for iz in 0..nz {
                        let cx0 = (x0 + ix as f32).min(x1);
                        let cx1 = (x0 + (ix + 1) as f32).min(x1);
                        let cz0 = (z0 + iz as f32).min(z1);
                        let cz1 = (z0 + (iz + 1) as f32).min(z1);
                        let rgb = if (ix + iz) % 2 == 0 {
                            [50, 56, 78]
                        } else {
                            [60, 68, 94]
                        };
                        let col =
                            shade(Color32::from_rgb(rgb[0], rgb[1], rgb[2]), s).gamma_multiply(dim);
                        let q = [
                            [cx0, GROUND_Y, cz0],
                            [cx1, GROUND_Y, cz0],
                            [cx1, GROUND_Y, cz1],
                            [cx0, GROUND_Y, cz1],
                        ];
                        for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                            if let (Some((a, az)), Some((bb, bz)), Some((cc, cz))) = (
                                proj.project(tri[0]),
                                proj.project(tri[1]),
                                proj.project(tri[2]),
                            ) {
                                tris.push(((az + bz + cz) / 3.0, [a, bb, cc], col));
                            }
                        }
                    }
                }
            }
        }

        for it in &items {
            let base = base_color(&it.inst);
            let dim = if it.active { 1.0 } else { 0.5 };
            let p_off = [it.place.x, 0.0, it.place.y];
            let origin = add(it.inst.pos, p_off);

            let mesh = it
                .inst
                .mesh
                .as_deref()
                .and_then(|n| self.mesh_cache.get(n)?.as_ref());
            let Some(mesh) = mesh else {
                // mesh-less (spawn/logical node) or failed load → flat marker.
                self.draw_marker(&painter, &proj, origin, base.gamma_multiply(dim), it.active);
                continue;
            };

            // The baker recentres geometry on its bbox midpoint (`obj2dl` is run
            // with `center: true`), so do the same here or models sit off the
            // floor relative to the ROM.
            let [mn, mx] = mesh.aabb;
            let mc = [
                (mn[0] + mx[0]) * 0.5,
                (mn[1] + mx[1]) * 0.5,
                (mn[2] + mx[2]) * 0.5,
            ];
            for t in &mesh.tris {
                let mut wp = [[0.0f32; 3]; 3];
                for k in 0..3 {
                    let v = sub(t.pos[k], mc);
                    let s = [
                        v[0] * it.inst.scale[0],
                        v[1] * it.inst.scale[1],
                        v[2] * it.inst.scale[2],
                    ];
                    wp[k] = add(add(rot_xyz(s, it.inst.rot), it.inst.pos), p_off);
                }
                let n = rot_xyz(t.normal, it.inst.rot);
                let centroid = scl(add(add(wp[0], wp[1]), wp[2]), 1.0 / 3.0);
                // Backface cull for solid; wireframe shows every edge.
                if !self.wireframe && dot(n, sub(proj.eye, centroid)) <= 0.0 {
                    continue;
                }
                let (Some((a, az)), Some((b, bz)), Some((c, cz))) = (
                    proj.project(wp[0]),
                    proj.project(wp[1]),
                    proj.project(wp[2]),
                ) else {
                    continue;
                };
                if self.wireframe {
                    let st = Stroke::new(1.0_f32, base.gamma_multiply(dim));
                    painter.line_segment([a, b], st);
                    painter.line_segment([b, c], st);
                    painter.line_segment([c, a], st);
                } else {
                    let s = (0.35 + 0.65 * dot(n, light).max(0.0)).clamp(0.0, 1.0);
                    tris.push((
                        (az + bz + cz) / 3.0,
                        [a, b, c],
                        shade(base, s).gamma_multiply(dim),
                    ));
                }
            }
        }

        // Painter's algorithm: draw far → near. Accumulate into ONE raw `Mesh`
        // rather than a `convex_polygon` per triangle: egui feathers each polygon's
        // edge for anti-aliasing, and hundreds of those semi-transparent borders
        // leave hairline seams between triangles (a false wireframe) that stretch
        // into spikes on sliver triangles. A raw mesh has no per-triangle
        // feathering, so adjacent triangles abut seamlessly (and it's one draw).
        if !self.wireframe {
            tris.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut mesh = egui::Mesh::default();
            for (_, pts, col) in &tris {
                let base = mesh.vertices.len() as u32;
                for p in pts {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: *p,
                        uv: egui::epaint::WHITE_UV,
                        color: *col,
                    });
                }
                mesh.indices.extend([base, base + 1, base + 2]);
            }
            painter.add(Shape::mesh(mesh));
        }

        // Zone bounds outline — drawn after the floor fill so the active-zone
        // edge stays visible on top of it.
        self.draw_zone_bounds_3d(&painter, &proj);

        // Active zone's camera frustum for its authored mode (#50).
        self.camera_frustum(&painter, &proj);

        // Selected-instance rings (active zone) — one per selected instance (#46).
        if let Some(stem) = self.active.clone() {
            if let (Some(entry), Some(zone)) =
                (self.level.zones.get(&stem), self.contents.get(&stem))
            {
                for &si in self.sel.items() {
                    if let Some(p) = zone.instances.get(si) {
                        let lp = p.pos();
                        let world = [lp[0] + entry.place[0], lp[1], lp[2] + entry.place[1]];
                        if let Some((s, _)) = proj.project(world) {
                            painter.circle_stroke(s, 11.0, Stroke::new(2.0_f32, Color32::WHITE));
                        }
                    }
                }
            }
        }

        if resp.clicked() {
            let shift = ui.input(|i| i.modifiers.shift);
            if let Some(pp) = resp.interact_pointer_pos() {
                self.pick_3d(&proj, pp, shift);
            }
        }

        painter.text(
            rect.left_bottom() + Vec2::new(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            "LMB orbit · RMB pan · scroll zoom · click/shift-click to select",
            FontId::proportional(11.0),
            Color32::from_rgb(110, 120, 140),
        );
    }

    /// A unit ground grid on the XZ plane (y = 0) for spatial reference.
    fn draw_ground_grid(&self, painter: &egui::Painter, proj: &Proj) {
        let n = 10;
        let minor = Stroke::new(1.0_f32, Color32::from_rgb(34, 38, 50));
        let axis = Stroke::new(1.2_f32, Color32::from_rgb(70, 78, 104));
        let seg = |painter: &egui::Painter, a: [f32; 3], b: [f32; 3], st: Stroke| {
            if let (Some((pa, _)), Some((pb, _))) = (proj.project(a), proj.project(b)) {
                painter.line_segment([pa, pb], st);
            }
        };
        for i in -n..=n {
            let v = i as f32;
            let st = if i == 0 { axis } else { minor };
            seg(painter, [v, 0.0, -n as f32], [v, 0.0, n as f32], st);
            seg(painter, [-n as f32, 0.0, v], [n as f32, 0.0, v], st);
        }
    }

    /// The active zone's camera pose for its authored mode: `(eye, forward, up,
    /// target)`. A *representative* rig (not a frame-exact runtime match) so
    /// switching the mode visibly changes what the frustum frames (#50). The
    /// target is the zone's ground-plane centre.
    fn camera_frustum(&self, painter: &egui::Painter, proj: &Proj) {
        let Some(stem) = self.active.as_ref() else {
            return;
        };
        let Some(entry) = self.level.zones.get(stem) else {
            return;
        };
        let (eye, dir, up, target) = camera_pose(entry);
        let f = norm(dir);
        let r = norm(cross(f, up));
        let u = cross(r, f);
        let fov_v = 0.7_f32;
        let aspect = 4.0 / 3.0; // DS screen
        let far =
            (sub(target, eye).iter().map(|c| c * c).sum::<f32>().sqrt() * 1.25).clamp(1.5, 12.0);
        let half_v = far * (fov_v * 0.5).tan();
        let half_h = far * (fov_v * aspect * 0.5).tan();
        let cf = add(eye, scl(f, far));
        let corners = [
            add(add(cf, scl(r, half_h)), scl(u, half_v)),
            add(add(cf, scl(r, -half_h)), scl(u, half_v)),
            add(add(cf, scl(r, -half_h)), scl(u, -half_v)),
            add(add(cf, scl(r, half_h)), scl(u, -half_v)),
        ];
        let col = Color32::from_rgb(90, 195, 225);
        let st = Stroke::new(1.4_f32, col);
        // Apex → far corners.
        if let Some((ae, _)) = proj.project(eye) {
            for c in &corners {
                if let Some((cp, _)) = proj.project(*c) {
                    painter.line_segment([ae, cp], st);
                }
            }
            painter.circle_filled(ae, 3.5, col);
        }
        // Far rectangle.
        for k in 0..4 {
            if let (Some((a, _)), Some((b, _))) =
                (proj.project(corners[k]), proj.project(corners[(k + 1) % 4]))
            {
                painter.line_segment([a, b], st);
            }
        }
    }

    /// Each zone's walkable `bounds` rect drawn on the ground at its global `place`.
    fn draw_zone_bounds_3d(&self, painter: &egui::Painter, proj: &Proj) {
        for (stem, entry) in &self.level.zones {
            let active = self.active.as_deref() == Some(stem.as_str());
            let (px, pz) = (entry.place[0], entry.place[1]);
            let (x0, z0) = (px + entry.bounds.min[0], pz + entry.bounds.min[1]);
            let (x1, z1) = (px + entry.bounds.max[0], pz + entry.bounds.max[1]);
            let corners = [[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]];
            let col = if active {
                Color32::from_rgb(90, 110, 150)
            } else {
                Color32::from_rgb(52, 60, 80)
            };
            let st = Stroke::new(if active { 1.8_f32 } else { 1.0 }, col);
            for k in 0..4 {
                if let (Some((a, _)), Some((b, _))) =
                    (proj.project(corners[k]), proj.project(corners[(k + 1) % 4]))
                {
                    painter.line_segment([a, b], st);
                }
            }
        }
    }

    /// Draw a flat marker for a mesh-less instance (spawn point / logical node).
    fn draw_marker(
        &self,
        painter: &egui::Painter,
        proj: &Proj,
        world: [f32; 3],
        col: Color32,
        active: bool,
    ) {
        if let Some((s, _)) = proj.project(world) {
            painter.circle(
                s,
                if active { 5.0 } else { 4.0 },
                col,
                Stroke::new(1.0_f32, Color32::from_black_alpha(120)),
            );
        }
    }

    /// Pick the instance whose projected origin is nearest the click (across all
    /// zones); selects it and makes its zone active. Mirrors the top-down pick.
    fn pick_3d(&mut self, proj: &Proj, p: Pos2, shift: bool) {
        let mut best: Option<(String, usize, f32)> = None;
        for (stem, entry) in &self.level.zones {
            let (px, pz) = (entry.place[0], entry.place[1]);
            if let Some(zone) = self.contents.get(stem) {
                for (i, pl) in zone.instances.iter().enumerate() {
                    let lp = pl.pos();
                    if let Some((s, _)) = proj.project([lp[0] + px, lp[1], lp[2] + pz]) {
                        let d = s.distance(p);
                        if d <= 14.0 && best.as_ref().is_none_or(|(_, _, bd)| d < *bd) {
                            best = Some((stem.clone(), i, d));
                        }
                    }
                }
            }
        }
        match best {
            Some((stem, i, _)) => {
                // Shift-toggle only extends within the active zone; picking in a
                // different zone switches active and starts a fresh selection.
                if shift && self.active.as_deref() == Some(stem.as_str()) {
                    self.sel.toggle(i);
                } else {
                    self.active = Some(stem);
                    self.sel = Sel::single(i);
                }
            }
            None => {
                if !shift {
                    self.sel = Sel::none();
                }
            }
        }
    }
}
