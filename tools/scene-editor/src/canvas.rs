//! The shared top-down (XZ) canvas: draws every zone at its global `place`
//! (bounds + instances + waypoint paths) and handles pointer interaction (drag
//! instances / waypoints / whole zones, click-to-select / switch zone, pan,
//! scroll-zoom).

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use scene2bin::Zone;

use crate::app::{Drag, EditorApp, Sel};
use crate::widgets::{diamond, placement_role, role_style};

impl EditorApp {
    pub(crate) fn canvas(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(24, 27, 36));

        // zoom on scroll (about the canvas centre).
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 && resp.hovered() {
            self.view.scale = (self.view.scale * (1.0 + scroll * 0.001)).clamp(20.0, 400.0);
        }

        self.draw_grid(&painter, rect);
        self.draw_zones(&painter, rect);
        if self.show_connections {
            self.draw_overlay(&painter, rect);
        }
        self.handle_pointer(&resp, rect);
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
            let selected = active && self.sel == Sel::Instance(i);
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

    /// Pick + drag instances / waypoints in the active zone, move a whole zone's
    /// place, or pan. Click (no drag) selects an instance in the active zone, or
    /// switches the active zone when another zone's bounds are clicked.
    fn handle_pointer(&mut self, resp: &egui::Response, rect: Rect) {
        let pick_radius = 12.0;
        let place = self.active_place();

        if resp.drag_started() {
            self.drag = Drag::Pan;
            if let Some(p) = resp.interact_pointer_pos() {
                if let Some(stem) = self.active.clone() {
                    if let Some(zone) = self.contents.get(&stem) {
                        // waypoints of the selected instance take priority.
                        if let Sel::Instance(si) = self.sel {
                            if let Some(pl) = zone.instances.get(si) {
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
                        if self.drag == Drag::Pan {
                            if let Some(i) = self.pick_instance(rect, p, pick_radius) {
                                self.drag = Drag::Instance(i);
                                self.sel = Sel::Instance(i);
                            }
                        }
                    }
                }
                // Grab inside the active zone's bounds (away from an instance) →
                // move the whole zone.
                if self.drag == Drag::Pan {
                    if let Some(stem) = self.zone_at(rect, p) {
                        if self.active.as_deref() == Some(stem.as_str()) {
                            self.drag = Drag::ZoneBody(stem);
                        }
                    }
                }
            }
        }

        if resp.dragged() {
            let dw = self.screen_to_world_delta(resp.drag_delta());
            match self.drag.clone() {
                Drag::Pan => self.view.center -= dw,
                Drag::Instance(i) => {
                    if let Some(pl) = self.active_zone_mut().and_then(|z| z.instances.get_mut(i)) {
                        let pos = pl.pos_mut();
                        pos[0] += dw.x;
                        pos[2] += dw.y;
                    }
                }
                Drag::Waypoint(i, w) => {
                    if let Some(wp) = self
                        .active_zone_mut()
                        .and_then(|z| z.instances.get_mut(i))
                        .and_then(|pl| pl.path_mut().get_mut(w))
                    {
                        wp[0] += dw.x;
                        wp[1] += dw.y;
                    }
                }
                Drag::ZoneBody(stem) => {
                    if let Some(entry) = self.level.zones.get_mut(&stem) {
                        entry.place[0] += dw.x;
                        entry.place[1] += dw.y;
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
                if let Some(i) = self.pick_instance(rect, p, pick_radius) {
                    self.sel = Sel::Instance(i);
                } else if let Some(stem) = self.zone_at(rect, p) {
                    if self.active.as_deref() != Some(stem.as_str()) {
                        self.active = Some(stem);
                        self.sel = Sel::None;
                    } else {
                        self.sel = Sel::None;
                    }
                } else {
                    self.sel = Sel::None;
                }
            }
        }
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
