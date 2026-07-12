//! The shared top-down (XZ) canvas: draws every zone at its global `place`
//! (bounds + instances + waypoint paths) and handles pointer interaction (drag
//! instances / waypoints / whole zones, click-to-select / switch zone, pan,
//! scroll-zoom).

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use scene2bin::{Placement, Zone};

use crate::app::{Drag, EditorApp, Sel};
use crate::widgets::{diamond, placement_role, role_style};

/// A placement's Y rotation (a `Use`'s `None` override reads as 0).
fn placement_rot_y(pl: &Placement) -> f32 {
    match pl {
        Placement::Lit(i) => i.rot[1],
        Placement::Use { rot, .. } => rot.map(|r| r[1]).unwrap_or(0.0),
    }
}

/// Set a placement's Y rotation (a `Use` gains a rot override if it had none).
fn placement_set_rot_y(pl: &mut Placement, ry: f32) {
    match pl {
        Placement::Lit(i) => i.rot[1] = ry,
        Placement::Use { rot, .. } => rot.get_or_insert([0.0, 0.0, 0.0])[1] = ry,
    }
}

/// A placement's scale (a `Use`'s `None` override reads as unit scale).
fn placement_scale(pl: &Placement) -> [f32; 3] {
    match pl {
        Placement::Lit(i) => i.scale,
        Placement::Use { scale, .. } => scale.unwrap_or([1.0, 1.0, 1.0]),
    }
}

/// Set a placement's scale (a `Use` gains a scale override if it had none).
fn placement_set_scale(pl: &mut Placement, s: [f32; 3]) {
    match pl {
        Placement::Lit(i) => i.scale = s,
        Placement::Use { scale, .. } => *scale = Some(s),
    }
}

impl EditorApp {
    pub(crate) fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(24, 27, 36));

        let (scroll, shift, ctrl, no_snap, mmb, rmb, pdelta, pointer) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.modifiers.shift,
                i.modifiers.command || i.modifiers.ctrl,
                i.modifiers.alt,
                i.pointer.middle_down(),
                i.pointer.secondary_down(),
                i.pointer.delta(),
                i.pointer.hover_pos(),
            )
        });

        // zoom on scroll (about the canvas centre).
        if scroll.abs() > 0.0 && resp.hovered() {
            self.view.scale = (self.view.scale * (1.0 + scroll * 0.001)).clamp(20.0, 400.0);
        }
        // Pan with the middle or secondary button — the primary drag is reserved
        // for box-select / moving objects (#46).
        if resp.hovered() && (mmb || rmb) {
            self.view.center -= self.screen_to_world_delta(pdelta);
        }

        self.draw_grid(&painter, rect);
        self.draw_zones(&painter, rect);
        self.draw_bounds_handles(&painter, rect);
        self.draw_gizmo(&painter, rect);
        if self.show_connections {
            self.draw_overlay(&painter, rect);
        }
        // Hold Alt to momentarily disable grid snapping for a free-float drag (#42).
        self.handle_pointer(&resp, rect, no_snap, shift, ctrl);

        // Rubber-band box-select overlay (#46).
        if let (Drag::Box(start), Some(cur)) = (self.drag.clone(), pointer) {
            let r = Rect::from_two_pos(start, cur);
            painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(120, 150, 220, 40));
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0_f32, Color32::from_rgb(150, 175, 235)),
                egui::StrokeKind::Inside,
            );
        }
    }

    /// Overlay (#41): the baker-derived connections (seam segments, coloured by
    /// gate) plus isolated-zone highlights. Recomputed live through `scene2bin`
    /// (`assemble` → `derive_connections` → `isolation_warnings`) so it always
    /// mirrors exactly what `build.rs` will bake — the editor stops being blind to
    /// whether zones actually connect.
    fn draw_overlay(&self, painter: &egui::Painter, rect: Rect) {
        let Ok(zones) = scene2bin::assemble(&self.level, &self.contents, &self.prefabs) else {
            return; // unresolvable layout (bad prefab / missing content) — derive nothing
        };
        let conns = scene2bin::derive_connections(&zones);
        let isolated = scene2bin::isolation_warnings(&conns);

        // Isolated zones (abut nothing in a multi-zone map): a warning ring.
        for stem in isolated.keys() {
            if let Some(entry) = self.level.zones.get(stem) {
                let place = Vec2::new(entry.place[0], entry.place[1]);
                let min = self.world_to_screen(
                    rect,
                    place + Vec2::new(entry.bounds.min[0], entry.bounds.min[1]),
                );
                let max = self.world_to_screen(
                    rect,
                    place + Vec2::new(entry.bounds.max[0], entry.bounds.max[1]),
                );
                painter.rect_stroke(
                    Rect::from_two_pos(min, max).expand(2.0),
                    0.0,
                    Stroke::new(2.0_f32, Color32::from_rgb(220, 90, 80)),
                    egui::StrokeKind::Outside,
                );
            }
        }

        // Seam segments. `lo`/`hi` are in the zone's local frame, on the axis
        // parallel to the abutting edge; rebuild the segment and offset by `place`.
        for (stem, cs) in &conns {
            let Some(entry) = self.level.zones.get(stem) else {
                continue;
            };
            let place = Vec2::new(entry.place[0], entry.place[1]);
            let b = &entry.bounds;
            for c in cs {
                let (a, z) = match c.side {
                    scene2bin::SIDE_EAST => ([b.max[0], c.lo], [b.max[0], c.hi]),
                    scene2bin::SIDE_WEST => ([b.min[0], c.lo], [b.min[0], c.hi]),
                    scene2bin::SIDE_NORTH => ([c.lo, b.max[1]], [c.hi, b.max[1]]),
                    _ => ([c.lo, b.min[1]], [c.hi, b.min[1]]), // SIDE_SOUTH
                };
                let p0 = self.world_to_screen(rect, place + Vec2::new(a[0], a[1]));
                let p1 = self.world_to_screen(rect, place + Vec2::new(z[0], z[1]));
                let col = if c.gate == 0 {
                    Color32::from_rgb(90, 210, 150) // always-open
                } else {
                    Color32::from_rgb(230, 170, 70) // gated (closed until objectives)
                };
                painter.line_segment([p0, p1], Stroke::new(3.5_f32, col));
            }
        }

        // Compact legend.
        painter.text(
            rect.left_bottom() + Vec2::new(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            "— open connection   — gated   ▢ isolated zone",
            FontId::proportional(11.0),
            Color32::from_rgb(120, 130, 150),
        );
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: Rect) {
        let stroke = Stroke::new(1.0_f32, Color32::from_rgb(34, 38, 50));
        let axis = Stroke::new(1.0_f32, Color32::from_rgb(70, 78, 104));
        for w in -10..=10 {
            let v = w as f32;
            let vx = self.world_to_screen(rect, Vec2::new(v, 0.0)).x;
            let hy = self.world_to_screen(rect, Vec2::new(0.0, v)).y;
            painter.line_segment(
                [Pos2::new(vx, rect.top()), Pos2::new(vx, rect.bottom())],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(rect.left(), hy), Pos2::new(rect.right(), hy)],
                stroke,
            );
        }
        let o = self.world_to_screen(rect, Vec2::ZERO);
        painter.line_segment(
            [Pos2::new(rect.left(), o.y), Pos2::new(rect.right(), o.y)],
            axis,
        );
        painter.line_segment(
            [Pos2::new(o.x, rect.top()), Pos2::new(o.x, rect.bottom())],
            axis,
        );
    }

    /// Draw every zone at its global `place`: bounds rect, label, then its
    /// instances + waypoint paths. The active zone is highlighted; others dim.
    fn draw_zones(&self, painter: &egui::Painter, rect: Rect) {
        for (stem, entry) in &self.level.zones {
            let active = self.active.as_deref() == Some(stem.as_str());
            let place = Vec2::new(entry.place[0], entry.place[1]);

            let min = self.world_to_screen(
                rect,
                place + Vec2::new(entry.bounds.min[0], entry.bounds.min[1]),
            );
            let max = self.world_to_screen(
                rect,
                place + Vec2::new(entry.bounds.max[0], entry.bounds.max[1]),
            );
            let col = if active {
                Color32::from_rgb(90, 110, 150)
            } else {
                Color32::from_rgb(56, 64, 86)
            };
            let bounds_rect = Rect::from_two_pos(min, max);
            painter.rect_stroke(
                bounds_rect,
                0.0,
                Stroke::new(if active { 1.8_f32 } else { 1.2_f32 }, col),
                egui::StrokeKind::Inside,
            );
            painter.text(
                bounds_rect.left_top() + Vec2::new(4.0, 2.0),
                Align2::LEFT_TOP,
                stem,
                FontId::proportional(12.0),
                if active {
                    Color32::from_rgb(170, 190, 220)
                } else {
                    Color32::from_rgb(110, 120, 140)
                },
            );

            if let Some(zone) = self.contents.get(stem) {
                self.draw_zone_contents(painter, rect, place, zone, active);
            }
        }
    }

    fn draw_zone_contents(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        place: Vec2,
        zone: &Zone,
        active: bool,
    ) {
        // waypoint paths.
        for p in &zone.instances {
            let path = p.path();
            if path.is_empty() {
                continue;
            }
            let pts: Vec<Pos2> = path
                .iter()
                .map(|w| self.world_to_screen(rect, place + Vec2::new(w[0], w[1])))
                .collect();
            let stroke = Stroke::new(1.5_f32, Color32::from_rgb(150, 120, 60));
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], stroke);
            }
            if pts.len() > 2 {
                painter.line_segment(
                    [pts[pts.len() - 1], pts[0]],
                    Stroke::new(1.0_f32, Color32::from_rgb(90, 74, 40)),
                );
            }
            for pt in &pts {
                painter.circle_filled(*pt, 3.0, Color32::from_rgb(210, 170, 90));
            }
        }

        // instance markers.
        for (i, p) in zone.instances.iter().enumerate() {
            let lpos = p.pos();
            let sp = self.world_to_screen(rect, place + Vec2::new(lpos[0], lpos[2]));
            let role = placement_role(p, &self.prefabs);
            let (mut col, r) = role_style(&role);
            if !active {
                col = col.gamma_multiply(0.5);
            }
            let selected = active && self.sel.contains(i);
            let outline = if selected {
                Stroke::new(2.0_f32, Color32::WHITE)
            } else {
                Stroke::new(1.0_f32, Color32::from_black_alpha(160))
            };
            match role.as_str() {
                "enemy" => {
                    painter.add(Shape::convex_polygon(diamond(sp, r), col, outline));
                }
                "landmark" | "prop" => {
                    painter.rect(
                        Rect::from_center_size(sp, Vec2::splat(r * 1.7)),
                        0.0,
                        col,
                        outline,
                        egui::StrokeKind::Inside,
                    );
                }
                _ => {
                    painter.circle(sp, r, col, outline);
                }
            }
            if active {
                painter.text(
                    sp + Vec2::new(0.0, r + 2.0),
                    Align2::CENTER_TOP,
                    &role,
                    FontId::proportional(11.0),
                    Color32::from_rgb(200, 205, 220),
                );
            }
        }
    }

    /// Primary-button interaction on the canvas (pan is on the middle/secondary
    /// button, handled in [`Self::canvas`]). Priority at drag-start: bounds
    /// handle (#45) → rotate/scale gizmo (#49) → waypoint → instance (group
    /// move, #46) → Ctrl+zone-body → rubber-band box-select. A plain click
    /// selects / shift-toggles an instance, or switches the active zone.
    fn handle_pointer(
        &mut self,
        resp: &egui::Response,
        rect: Rect,
        no_snap: bool,
        shift: bool,
        ctrl: bool,
    ) {
        let pick_radius = 12.0;
        let place = self.active_place();

        if resp.drag_started() {
            self.drag = Drag::None;
            if let Some(p) = resp.interact_pointer_pos() {
                // 1. bounds edge/corner handle of the active zone (#45).
                if let Some(mask) = self.pick_bounds_handle(rect, p) {
                    self.drag = Drag::BoundsHandle(mask);
                }
                // 2. rotate / scale gizmo on the primary selection (#49).
                if self.drag == Drag::None {
                    if let Some(g) = self.pick_gizmo(rect, p) {
                        if g == Drag::Scale {
                            self.begin_scale(rect, p);
                        }
                        self.drag = g;
                    }
                }
                // 3. a waypoint of the primary selection.
                if self.drag == Drag::None {
                    if let (Some(si), Some(stem)) = (self.sel.primary(), self.active.clone()) {
                        if let Some(pl) = self.contents.get(&stem).and_then(|z| z.instances.get(si))
                        {
                            for (wi, wp) in pl.path().iter().enumerate() {
                                let sp =
                                    self.world_to_screen(rect, place + Vec2::new(wp[0], wp[1]));
                                if sp.distance(p) <= pick_radius {
                                    self.drag = Drag::Waypoint(si, wi);
                                    break;
                                }
                            }
                        }
                    }
                }
                // 4. an instance — grab it (keeping the group if it's part of one).
                if self.drag == Drag::None {
                    if let Some(i) = self.pick_instance(rect, p, pick_radius) {
                        if !shift && !self.sel.contains(i) {
                            self.sel.set_single(i);
                        }
                        self.drag = Drag::Instance(i);
                    }
                }
                // 5. Ctrl+drag inside the active zone body → move the whole zone.
                if self.drag == Drag::None && ctrl {
                    if let Some(stem) = self.zone_at(rect, p) {
                        if self.active.as_deref() == Some(stem.as_str()) {
                            self.drag = Drag::ZoneBody(stem);
                        }
                    }
                }
                // 6. empty space → rubber-band box-select.
                if self.drag == Drag::None {
                    self.drag = Drag::Box(p);
                }
            }
            // Seed the raw (un-snapped) drag position from the grabbed object so
            // snapping preserves sub-step pointer motion across the gesture (#42).
            self.drag_raw = self.drag_anchor();
        }

        if resp.dragged() {
            let dw = self.screen_to_world_delta(resp.drag_delta());
            match self.drag.clone() {
                Drag::Instance(i) => {
                    self.drag_raw += dw;
                    let s = self.snap_vec(self.drag_raw, no_snap);
                    // Move the whole selection by the grabbed instance's snapped delta.
                    let items = self.sel.items().to_vec();
                    if let Some(zone) = self.active_zone_mut() {
                        if let Some(cur) = zone.instances.get(i).map(|p| p.pos()) {
                            let d = Vec2::new(s.x - cur[0], s.y - cur[2]);
                            let targets = if items.contains(&i) { items } else { vec![i] };
                            for j in targets {
                                if let Some(pl) = zone.instances.get_mut(j) {
                                    let p = pl.pos_mut();
                                    p[0] += d.x;
                                    p[2] += d.y;
                                }
                            }
                        }
                    }
                }
                Drag::Waypoint(i, w) => {
                    self.drag_raw += dw;
                    let s = self.snap_vec(self.drag_raw, no_snap);
                    if let Some(wp) = self
                        .active_zone_mut()
                        .and_then(|z| z.instances.get_mut(i))
                        .and_then(|pl| pl.path_mut().get_mut(w))
                    {
                        wp[0] = s.x;
                        wp[1] = s.y;
                    }
                }
                Drag::ZoneBody(stem) => {
                    self.drag_raw += dw;
                    let s = self.snap_vec(self.drag_raw, no_snap);
                    if let Some(entry) = self.level.zones.get_mut(&stem) {
                        entry.place[0] = s.x;
                        entry.place[1] = s.y;
                    }
                }
                Drag::BoundsHandle(mask) => {
                    if let Some(sp) = resp.interact_pointer_pos() {
                        let wp = self.screen_to_world(rect, sp);
                        self.drag_bounds_handle(mask, wp, no_snap);
                    }
                }
                Drag::Rotate => self.drag_rotate(rect, resp),
                Drag::Scale => self.drag_scale(rect, resp),
                Drag::Box(_) | Drag::None => {}
            }
        }

        if resp.drag_stopped() {
            if let Drag::Box(start) = self.drag.clone() {
                if let Some(end) = resp.interact_pointer_pos() {
                    self.box_select(rect, start, end, shift);
                }
            }
            self.drag = Drag::None;
        }

        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                if let Some(i) = self.pick_instance(rect, p, pick_radius) {
                    if shift {
                        self.sel.toggle(i);
                    } else {
                        self.sel.set_single(i);
                    }
                } else if let Some(stem) = self.zone_at(rect, p) {
                    if self.active.as_deref() != Some(stem.as_str()) {
                        self.active = Some(stem);
                        self.sel = Sel::none();
                    } else if !shift {
                        self.sel = Sel::none();
                    }
                } else if !shift {
                    self.sel = Sel::none();
                }
            }
        }
    }

    /// Select every active-zone instance whose marker falls inside the drag rect
    /// (screen space). Additive (Shift) unions with the current selection (#46).
    fn box_select(&mut self, rect: Rect, a: Pos2, b: Pos2, additive: bool) {
        let sel_rect = Rect::from_two_pos(a, b);
        let Some(stem) = self.active.clone() else {
            return;
        };
        let place = self.active_place();
        let Some(zone) = self.contents.get(&stem) else {
            return;
        };
        let mut hits = Vec::new();
        for (i, pl) in zone.instances.iter().enumerate() {
            let lp = pl.pos();
            let sp = self.world_to_screen(rect, place + Vec2::new(lp[0], lp[2]));
            if sel_rect.contains(sp) {
                hits.push(i);
            }
        }
        if additive {
            for i in hits {
                if !self.sel.contains(i) {
                    self.sel.toggle(i);
                }
            }
        } else {
            self.sel.set_many(hits);
        }
    }

    /// The current coordinate the active drag grabs (instance/waypoint local XZ,
    /// or a zone's global `place`), used to seed [`Self::drag_raw`] at drag start.
    fn drag_anchor(&self) -> Vec2 {
        match &self.drag {
            Drag::Instance(i) => self
                .active
                .as_ref()
                .and_then(|s| self.contents.get(s))
                .and_then(|z| z.instances.get(*i))
                .map(|p| {
                    let q = p.pos();
                    Vec2::new(q[0], q[2])
                })
                .unwrap_or(Vec2::ZERO),
            Drag::Waypoint(i, w) => self
                .active
                .as_ref()
                .and_then(|s| self.contents.get(s))
                .and_then(|z| z.instances.get(*i))
                .and_then(|p| p.path().get(*w).copied())
                .map(|wp| Vec2::new(wp[0], wp[1]))
                .unwrap_or(Vec2::ZERO),
            Drag::ZoneBody(stem) => self
                .level
                .zones
                .get(stem)
                .map(|e| Vec2::new(e.place[0], e.place[1]))
                .unwrap_or(Vec2::ZERO),
            // These carry their own state (or none) and don't use `drag_raw`.
            Drag::BoundsHandle(_) | Drag::Rotate | Drag::Scale | Drag::Box(_) | Drag::None => {
                Vec2::ZERO
            }
        }
    }

    // --- bounds edge/corner drag handles (#45) ---

    /// Hit-test the active zone's bounds handles. Returns an edge bitmask
    /// (1=min-x, 2=max-x, 4=min-z, 8=max-z); corners set two bits.
    fn pick_bounds_handle(&self, rect: Rect, p: Pos2) -> Option<u8> {
        let stem = self.active.as_ref()?;
        let entry = self.level.zones.get(stem)?;
        let place = Vec2::new(entry.place[0], entry.place[1]);
        let (x0, z0, x1, z1) = (
            entry.bounds.min[0],
            entry.bounds.min[1],
            entry.bounds.max[0],
            entry.bounds.max[1],
        );
        let sc = |x: f32, z: f32| self.world_to_screen(rect, place + Vec2::new(x, z));
        let r = 8.0;
        // Corners first (two-edge masks), then edge midpoints (one-edge).
        let targets = [
            (sc(x0, z0), 1 | 4),
            (sc(x1, z0), 2 | 4),
            (sc(x1, z1), 2 | 8),
            (sc(x0, z1), 1 | 8),
            (sc(x0, (z0 + z1) * 0.5), 1),
            (sc(x1, (z0 + z1) * 0.5), 2),
            (sc((x0 + x1) * 0.5, z0), 4),
            (sc((x0 + x1) * 0.5, z1), 8),
        ];
        targets
            .into_iter()
            .find(|(sp, _)| sp.distance(p) <= r)
            .map(|(_, m)| m)
    }

    /// Move the masked bounds edges of the active zone to the (snapped) pointer,
    /// keeping the rect non-degenerate. `wp` is the pointer in world (x, z).
    fn drag_bounds_handle(&mut self, mask: u8, wp: Vec2, no_snap: bool) {
        let Some(stem) = self.active.clone() else {
            return;
        };
        let place = self.active_place();
        let s = self.snap_vec(wp - place, no_snap);
        let gap = self.grid_step.max(0.1);
        if let Some(entry) = self.level.zones.get_mut(&stem) {
            let b = &mut entry.bounds;
            if mask & 1 != 0 {
                b.min[0] = s.x.min(b.max[0] - gap);
            }
            if mask & 2 != 0 {
                b.max[0] = s.x.max(b.min[0] + gap);
            }
            if mask & 4 != 0 {
                b.min[1] = s.y.min(b.max[1] - gap);
            }
            if mask & 8 != 0 {
                b.max[1] = s.y.max(b.min[1] + gap);
            }
        }
    }

    /// Draw the grab handles on the active zone's bounds rect (#45).
    fn draw_bounds_handles(&self, painter: &egui::Painter, rect: Rect) {
        let Some(stem) = self.active.as_ref() else {
            return;
        };
        let Some(entry) = self.level.zones.get(stem) else {
            return;
        };
        let place = Vec2::new(entry.place[0], entry.place[1]);
        let (x0, z0, x1, z1) = (
            entry.bounds.min[0],
            entry.bounds.min[1],
            entry.bounds.max[0],
            entry.bounds.max[1],
        );
        let sc = |x: f32, z: f32| self.world_to_screen(rect, place + Vec2::new(x, z));
        let pts = [
            sc(x0, z0),
            sc(x1, z0),
            sc(x1, z1),
            sc(x0, z1),
            sc(x0, (z0 + z1) * 0.5),
            sc(x1, (z0 + z1) * 0.5),
            sc((x0 + x1) * 0.5, z0),
            sc((x0 + x1) * 0.5, z1),
        ];
        let fill = Color32::from_rgb(150, 175, 235);
        for sp in pts {
            painter.rect_filled(Rect::from_center_size(sp, Vec2::splat(6.0)), 1.0, fill);
        }
    }

    // --- rotate / scale gizmos on the primary selection (#49) ---

    /// Screen positions of the gizmo handles for the primary selection:
    /// `(center, rotate-handle, scale-handle)`.
    fn gizmo_handles(&self, rect: Rect) -> Option<(Pos2, Pos2, Pos2)> {
        let (stem, i) = self.selected()?;
        let place = self.active_place();
        let pl = self.contents.get(&stem)?.instances.get(i)?;
        let pos = pl.pos();
        let c = self.world_to_screen(rect, place + Vec2::new(pos[0], pos[2]));
        let ry = placement_rot_y(pl);
        // Heading maps (sin ry, cos ry) → screen (x right, y down = +z), so the
        // arrow direction and the atan2 in `drag_rotate` share one convention.
        let dir = Vec2::new(ry.sin(), ry.cos());
        Some((c, c + dir * 42.0, c + Vec2::new(38.0, 38.0)))
    }

    fn pick_gizmo(&self, rect: Rect, p: Pos2) -> Option<Drag> {
        let (_, rot_h, scale_h) = self.gizmo_handles(rect)?;
        if p.distance(rot_h) <= 8.0 {
            return Some(Drag::Rotate);
        }
        if p.distance(scale_h) <= 8.0 {
            return Some(Drag::Scale);
        }
        None
    }

    /// Capture the scale reference pose when a Scale gizmo drag begins (#49).
    fn begin_scale(&mut self, rect: Rect, p: Pos2) {
        if let (Some((c, _, _)), Some((stem, i))) = (self.gizmo_handles(rect), self.selected()) {
            self.gizmo_dist0 = (p - c).length().max(1.0);
            if let Some(pl) = self.contents.get(&stem).and_then(|z| z.instances.get(i)) {
                self.scale_ref = placement_scale(pl);
            }
        }
    }

    fn drag_rotate(&mut self, rect: Rect, resp: &egui::Response) {
        let Some((stem, i)) = self.selected() else {
            return;
        };
        let Some(sp) = resp.interact_pointer_pos() else {
            return;
        };
        let wp = self.screen_to_world(rect, sp);
        let place = self.active_place();
        if let Some(pl) = self
            .contents
            .get_mut(&stem)
            .and_then(|z| z.instances.get_mut(i))
        {
            let pos = pl.pos();
            let dx = wp.x - (place.x + pos[0]);
            let dz = wp.y - (place.y + pos[2]);
            if dx.abs() + dz.abs() > 1e-4 {
                placement_set_rot_y(pl, dx.atan2(dz));
            }
        }
    }

    fn drag_scale(&mut self, rect: Rect, resp: &egui::Response) {
        let Some((stem, i)) = self.selected() else {
            return;
        };
        let (Some((c, _, _)), Some(sp)) = (self.gizmo_handles(rect), resp.interact_pointer_pos())
        else {
            return;
        };
        let factor = ((sp - c).length() / self.gizmo_dist0).clamp(0.05, 20.0);
        let base = self.scale_ref;
        if let Some(pl) = self
            .contents
            .get_mut(&stem)
            .and_then(|z| z.instances.get_mut(i))
        {
            placement_set_scale(pl, [base[0] * factor, base[1] * factor, base[2] * factor]);
        }
    }

    /// Draw the rotate arrow + scale handle on the primary selection (#49).
    fn draw_gizmo(&self, painter: &egui::Painter, rect: Rect) {
        let Some((c, rot_h, scale_h)) = self.gizmo_handles(rect) else {
            return;
        };
        let arm = Color32::from_rgb(120, 200, 150);
        painter.line_segment([c, rot_h], Stroke::new(2.0_f32, arm));
        painter.circle_filled(rot_h, 5.0, arm);
        let sc = Color32::from_rgb(230, 200, 90);
        painter.rect_filled(Rect::from_center_size(scale_h, Vec2::splat(9.0)), 1.0, sc);
    }

    /// The closest instance in the *active* zone under `p`.
    fn pick_instance(&self, rect: Rect, p: Pos2, radius: f32) -> Option<usize> {
        let stem = self.active.as_ref()?;
        let entry = self.level.zones.get(stem)?;
        let zone = self.contents.get(stem)?;
        let place = Vec2::new(entry.place[0], entry.place[1]);
        let mut best: Option<(usize, f32)> = None;
        for (i, pl) in zone.instances.iter().enumerate() {
            let lpos = pl.pos();
            let sp = self.world_to_screen(rect, place + Vec2::new(lpos[0], lpos[2]));
            let d = sp.distance(p);
            if d <= radius && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// The (topmost) zone whose bounds rect contains screen point `p`.
    fn zone_at(&self, rect: Rect, p: Pos2) -> Option<String> {
        for (stem, entry) in &self.level.zones {
            let place = Vec2::new(entry.place[0], entry.place[1]);
            let min = self.world_to_screen(
                rect,
                place + Vec2::new(entry.bounds.min[0], entry.bounds.min[1]),
            );
            let max = self.world_to_screen(
                rect,
                place + Vec2::new(entry.bounds.max[0], entry.bounds.max[1]),
            );
            if Rect::from_two_pos(min, max).contains(p) {
                return Some(stem.clone());
            }
        }
        None
    }
}
