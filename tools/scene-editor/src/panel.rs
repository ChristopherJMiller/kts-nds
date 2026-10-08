//! The right-hand side panel + top menu bar: level manifest (name/entry/zone
//! list), active-zone camera/place/bounds, the instance list (literals + prefab
//! uses), and the selected-instance editor.

use std::collections::BTreeMap;

use eframe::egui;
use egui::Vec2;
use scene2bin::schema::Role;
use scene2bin::{Camera, Instance, Issue, Material, Placement, Severity};

use crate::app::{EditorApp, Prim, Sel, View, ViewMode};
use crate::widgets::{
    Footprint, MeshThumb, camera_tag, default_camera, default_prefab, drag_row, footprint,
    footprint_extent, inside_footprint, named_flags_ui, new_instance, opt_named_flags_ui,
    opt_vec3_row, placement_label, role_kind_ui, thumb_widget, vec3_row,
};

impl EditorApp {
    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Level browser (#55): pick from `assets/levels/*` or scaffold a new one.
            ui.label("level:");
            let current = self.current_level_name();
            let names = self.level_names();
            let mut pick = None;
            egui::ComboBox::from_id_salt("level-pick")
                .selected_text(if current.is_empty() {
                    "(none)"
                } else {
                    &current
                })
                .show_ui(ui, |ui| {
                    for n in &names {
                        if ui.selectable_label(current == *n, n).clicked() {
                            pick = Some(n.clone());
                        }
                    }
                });
            if let Some(n) = pick {
                self.open_level(&n);
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.new_level)
                    .desired_width(90.0)
                    .hint_text("new level"),
            );
            if ui.button("+ new").clicked() {
                let n = self.new_level.clone();
                self.create_level(&n);
                self.new_level.clear();
            }
            ui.separator();
            if ui
                .add_enabled(self.can_undo(), egui::Button::new("↶"))
                .on_hover_text("Undo (Ctrl+Z)")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.can_redo(), egui::Button::new("↷"))
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .clicked()
            {
                self.redo();
            }
            ui.separator();
            ui.checkbox(&mut self.snap, "snap")
                .on_hover_text("Snap drags to the grid (hold Alt to disable)");
            ui.add(
                egui::DragValue::new(&mut self.grid_step)
                    .speed(0.01)
                    .range(0.01..=8.0)
                    .prefix("step "),
            );
            ui.separator();
            ui.selectable_value(&mut self.view_mode, ViewMode::TopDown, "2D");
            ui.selectable_value(&mut self.view_mode, ViewMode::Perspective, "3D");
            match self.view_mode {
                ViewMode::TopDown => {
                    if ui.button("Reset view").clicked() {
                        self.view = View {
                            center: egui::Vec2::ZERO,
                            scale: 90.0,
                        };
                    }
                    if ui.button("Frame all").clicked() {
                        self.frame_all();
                    }
                    ui.checkbox(&mut self.show_connections, "connections");
                }
                ViewMode::Perspective => {
                    ui.checkbox(&mut self.wireframe, "wireframe");
                }
            }
            ui.separator();
            if ui.button("?").on_hover_text("Shortcuts (F1)").clicked() {
                self.show_help = !self.show_help;
            }
        });
        if !self.status.is_empty() {
            ui.label(egui::RichText::new(&self.status).weak());
        }
    }

    pub(crate) fn side_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.level_ui(ui);
            ui.separator();
            self.zone_ui(ui);
            ui.separator();
            self.instance_list_ui(ui);
            ui.separator();
            self.selected_instance_ui(ui);
            ui.separator();
            self.prefab_ui(ui);
            ui.separator();
            self.problems_ui(ui);
        });
    }

    /// In-editor prefab create / edit / delete (#51): the library list (each
    /// click-to-edit, ✕ to delete), a create field, and — when a prefab is open
    /// — a full editor writing RON via `scene2bin::to_prefab_ron`.
    fn prefab_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Prefabs");
        let mut create = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_prefab)
                    .desired_width(100.0)
                    .hint_text("new prefab"),
            );
            if ui.button("+ prefab").clicked() {
                create = true;
            }
        });
        if create {
            let name = self.new_prefab.trim().to_string();
            if name.is_empty() {
                self.status = "prefab name empty".to_string();
            } else {
                self.edit_prefab = Some((name, default_prefab()));
                self.new_prefab.clear();
            }
        }

        let names = self.prefab_names();
        let editing = self.edit_prefab.as_ref().map(|(n, _)| n.clone());
        let mut to_edit = None;
        let mut to_delete = None;
        for n in &names {
            ui.horizontal(|ui| {
                let is = editing.as_deref() == Some(n.as_str());
                if ui.selectable_label(is, n).clicked() {
                    to_edit = Some(n.clone());
                }
                if ui.small_button("✕").clicked() {
                    to_delete = Some(n.clone());
                }
            });
        }
        if let Some(n) = to_edit {
            if let Some(p) = self.prefabs.get(&n).cloned() {
                self.edit_prefab = Some((n, p));
            }
        }
        if let Some(n) = to_delete {
            self.delete_prefab(&n);
        }

        // Draft editor — take the draft out so the mesh picker can read
        // `self.meshes` / `self.mesh_thumbs` without a borrow clash.
        if let Some((mut name, mut prefab)) = self.edit_prefab.take() {
            ui.separator();
            ui.label(egui::RichText::new("Prefab editor").strong());
            ui.horizontal(|ui| {
                ui.label("name:");
                ui.text_edit_singleline(&mut name);
            });
            // No `avatar`: a prefab is a reusable template, the avatar is the
            // level's single literal in its entry zone.
            role_kind_ui(ui, "prefab", &mut prefab.role, &mut prefab.kind, false);
            ui.horizontal(|ui| {
                let cur = prefab.mesh.as_deref().and_then(|m| self.mesh_thumbs.get(m));
                thumb_widget(ui, cur, 34.0);
                let label = prefab.mesh.clone().unwrap_or_else(|| "(none)".into());
                egui::ComboBox::from_id_salt("prefab-mesh")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(prefab.mesh.is_none(), "(none)")
                            .clicked()
                        {
                            prefab.mesh = None;
                        }
                        for m in &self.meshes {
                            let is = prefab.mesh.as_deref() == Some(m.as_str());
                            ui.horizontal(|ui| {
                                thumb_widget(ui, self.mesh_thumbs.get(m), 22.0);
                                if ui.selectable_label(is, m).clicked() {
                                    prefab.mesh = Some(m.clone());
                                }
                            });
                        }
                    });
            });
            ui.label("rotation (rx, ry, rz)");
            vec3_row(ui, "rot", &mut prefab.rot, 0.01);
            ui.label("scale");
            vec3_row(ui, "scale", &mut prefab.scale, 0.005);
            let mut has_mat = prefab.material.is_some();
            if ui.checkbox(&mut has_mat, "material").changed() {
                prefab.material = has_mat.then_some(Material {
                    diffuse: [200, 200, 210],
                    ambient: [40, 40, 55],
                });
            }
            if let Some(m) = &mut prefab.material {
                ui.horizontal(|ui| {
                    ui.label("diffuse");
                    ui.color_edit_button_srgb(&mut m.diffuse);
                    ui.label("ambient");
                    ui.color_edit_button_srgb(&mut m.ambient);
                });
            }
            // Only the bits this role may carry. An unparsed role has no mask to
            // apply, so the editor shows nothing rather than coercing it to some
            // other role's rules (same as the `Use`-with-missing-prefab arm).
            match Role::parse(&prefab.role) {
                Some(r) => named_flags_ui(ui, "flags", &mut prefab.flags, r),
                None => {
                    ui.weak("flags unavailable — unknown role");
                }
            }
            ui.horizontal(|ui| {
                ui.label(format!("path ({} pts)", prefab.path.len()));
                if ui.button("+ wp").clicked() {
                    let last = prefab.path.last().copied().unwrap_or([0.0, 0.0]);
                    prefab.path.push(last);
                }
                if ui.button("− wp").clicked() {
                    prefab.path.pop();
                }
            });
            let mut action = 0u8; // 0 keep · 1 save · 2 cancel
            ui.horizontal(|ui| {
                if ui.button("Save prefab").clicked() {
                    action = 1;
                }
                if ui.button("Cancel").clicked() {
                    action = 2;
                }
            });
            match action {
                1 => {
                    self.save_prefab(&name, &prefab);
                    self.edit_prefab = Some((name, prefab));
                }
                2 => self.edit_prefab = None,
                _ => self.edit_prefab = Some((name, prefab)),
            }
        }
    }

    /// Live problems panel (#53): re-runs `assemble` + `validate_all` +
    /// `isolation_warnings` every frame (the same validator `build.rs` bakes
    /// against and `just check-levels` exits on) and lists every [`Issue`],
    /// click-to-focus on its zone *and* its instance — so an invalid state
    /// surfaces immediately, not only at save.
    fn problems_ui(&mut self, ui: &mut egui::Ui) {
        let problems = self.compute_problems();
        let errors = problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
            .count();
        ui.horizontal(|ui| {
            ui.heading("Problems");
            if errors > 0 {
                ui.weak(format!("({errors} error / {} total)", problems.len()));
            } else {
                ui.weak(format!("({})", problems.len()));
            }
        });
        if problems.is_empty() {
            ui.weak("none — level is valid");
            return;
        }
        let mut focus = None;
        for p in &problems {
            let (glyph, color) = match p.severity {
                // Red blocks the bake; amber bakes but wants a look.
                Severity::Error => ("✖", egui::Color32::from_rgb(232, 94, 82)),
                Severity::Warning => ("⚠", egui::Color32::from_rgb(224, 150, 90)),
            };
            let text = match &p.zone {
                Some(_) => format!("{glyph}  {}: {}", p.scope(), p.msg),
                None => format!("{glyph}  {}", p.msg),
            };
            let label = egui::Label::new(egui::RichText::new(text).color(color))
                .sense(egui::Sense::click())
                .wrap();
            if ui.add(label).clicked() {
                focus = p.zone.clone().map(|z| (z, p.instance));
            }
        }
        if let Some((z, inst)) = focus
            && self.level.zones.contains_key(&z)
        {
            self.active = Some(z);
            // Focus the offending instance too, not just its zone — an Issue
            // carries the index, so a click lands on the thing that's wrong.
            self.sel = match inst {
                Some(i) => Sel::single(i),
                None => Sel::none(),
            };
        }
    }

    /// Collect the current validation / isolation problems (see [`Self::problems_ui`]).
    /// One `validate_all` pass — the same one the bake runs — plus the isolation
    /// sweep, which is advisory and has no [`Issue`] of its own.
    fn compute_problems(&self) -> Vec<Issue> {
        let mut out = Vec::new();
        match scene2bin::assemble(&self.level, &self.contents, &self.prefabs) {
            Ok(zones) => {
                let mesh_exists = |name: &str| self.meshes.iter().any(|m| m == name);
                out.extend(scene2bin::validate_all(&self.level, &zones, mesh_exists));
                let conns = scene2bin::derive_connections(&zones);
                for stem in scene2bin::isolation_warnings(&conns).keys() {
                    out.push(Issue {
                        zone: Some(stem.clone()),
                        instance: None,
                        severity: Severity::Warning,
                        msg: "isolated zone (abuts no neighbour)".to_string(),
                    });
                }
                self.solid_footprint_warnings(&zones, &mut out);
            }
            // A parse/assemble failure (unknown prefab, missing content file) has
            // no zones to validate against — report it whole-level.
            Err(e) => out.push(Issue {
                zone: None,
                instance: None,
                severity: Severity::Error,
                msg: e,
            }),
        }
        out
    }

    /// Editor-only spatial warnings about **blocking footprints** (#12).
    ///
    /// These can't live in `scene2bin::validate_all`: it has no mesh extents, so
    /// it cannot know how big a collider actually is. The editor does (it has
    /// the baked thumbnails), so it warns here instead — never an Error, since
    /// both cases are judgement calls, not broken data.
    ///
    /// Runs every frame with the rest of `compute_problems`, so the scan is
    /// capped at 64 solids × 64 waypoints per zone.
    fn solid_footprint_warnings(&self, zones: &[(String, scene2bin::Space)], out: &mut Vec<Issue>) {
        const SCAN_CAP: usize = 64;
        for (stem, space) in zones {
            // One pass: collect this zone's solid footprints (capped), and warn
            // about any that spill outside the zone the avatar is clamped to.
            let mut solids: Vec<Footprint> = Vec::new();
            for (i, inst) in space.instances.iter().enumerate() {
                if solids.len() >= SCAN_CAP {
                    break;
                }
                let thumb = inst.mesh.as_deref().and_then(|m| self.mesh_thumbs.get(m));
                let Some(f) = footprint(inst, thumb) else {
                    continue;
                };
                // The yaw-turned rectangle's *actual* reach on each world axis,
                // not its circumradius: an axis-aligned box that sits inside the
                // bounds must not be reported just because its diagonal would
                // poke out if it were turned 45°.
                let reach = footprint_extent(&f);
                let b = &space.bounds;
                if f.center.x - reach.x < b.min[0]
                    || f.center.x + reach.x > b.max[0]
                    || f.center.y - reach.y < b.min[1]
                    || f.center.y + reach.y > b.max[1]
                {
                    out.push(Issue {
                        zone: Some(stem.clone()),
                        instance: Some(i),
                        severity: Severity::Warning,
                        msg: "solid footprint overhangs zone bounds".to_string(),
                    });
                }
                solids.push(f);
            }
            if solids.is_empty() {
                continue;
            }
            // Enemies do not collide in this slice (#26, OQ-10 open), so a patrol
            // route through a wall walks through it. Flag it where it is authored.
            for (i, inst) in space.instances.iter().enumerate() {
                if Role::parse(&inst.role) != Some(Role::Enemy) {
                    continue;
                }
                for (k, w) in inst.path.iter().take(SCAN_CAP).enumerate() {
                    let p = Vec2::new(w[0], w[1]);
                    if solids.iter().any(|f| inside_footprint(f, p)) {
                        out.push(Issue {
                            zone: Some(stem.clone()),
                            instance: Some(i),
                            severity: Severity::Warning,
                            msg: format!(
                                "patrol waypoint #{k} lies inside a solid footprint (enemies do not collide — route around it)"
                            ),
                        });
                    }
                }
            }
        }
    }

    /// Level manifest: name, entry zone, and the zone list (pick the active one,
    /// add / remove a zone).
    fn level_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Level");
        ui.horizontal(|ui| {
            ui.label("name:");
            ui.text_edit_singleline(&mut self.level.name);
        });

        let stems: Vec<String> = self.level.zones.keys().cloned().collect();
        ui.horizontal(|ui| {
            ui.label("entry:");
            egui::ComboBox::from_id_salt("entry")
                .selected_text(&self.level.entry)
                .show_ui(ui, |ui| {
                    for s in &stems {
                        ui.selectable_value(&mut self.level.entry, s.clone(), s);
                    }
                });
        });

        ui.label("zones:");
        for s in &stems {
            let is_active = self.active.as_deref() == Some(s.as_str());
            let tag = if self.level.entry == *s {
                format!("{s}  ★")
            } else {
                s.clone()
            };
            ui.horizontal(|ui| {
                if ui.selectable_label(is_active, tag).clicked() {
                    self.active = Some(s.clone());
                    self.sel = Sel::none();
                }
                if ui.small_button("✕").clicked() {
                    self.level.zones.remove(s);
                    self.contents.remove(s);
                    if self.active.as_deref() == Some(s.as_str()) {
                        self.active = self.level.zones.keys().next().cloned();
                        self.sel = Sel::none();
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_zone)
                    .desired_width(120.0)
                    .hint_text("new zone stem"),
            );
            if ui.button("+ zone").clicked() {
                self.add_zone();
            }
        });
    }

    /// Active-zone placement + walkable bounds + camera framing (all from the
    /// manifest). Connections to neighbours derive from `place`/`bounds` at bake.
    fn zone_ui(&mut self, ui: &mut egui::Ui) {
        let Some(stem) = self.active.clone() else {
            ui.weak("(no zone selected)");
            return;
        };
        let mut do_clone = false;
        ui.horizontal(|ui| {
            ui.heading(format!("Zone · {stem}"));
            if ui
                .button("clone")
                .on_hover_text("Duplicate this zone under a new stem")
                .clicked()
            {
                do_clone = true;
            }
        });
        if do_clone {
            self.clone_active_zone();
            return;
        }
        // The entry zone authors the level's single persistent avatar; every
        // other zone authors none (#27 / #54 — an amendment to #27's 2026-06-28
        // "zones no longer author an avatar" line, recorded 2026-09-18).
        if self.level.entry == stem {
            ui.label(
                egui::RichText::new(
                    "★ entry zone — authors the level's one avatar (other zones: none)",
                )
                .color(egui::Color32::from_rgb(110, 180, 235)),
            );
        }
        let Some(entry) = self.level.zones.get_mut(&stem) else {
            return;
        };

        let mut tag = camera_tag(&entry.camera);
        egui::ComboBox::from_id_salt("cam")
            .selected_text(tag)
            .show_ui(ui, |ui| {
                for t in ["Follow", "TopDown", "Rail2_5D", "CaptureFraming"] {
                    ui.selectable_value(&mut tag, t, t);
                }
            });
        if tag != camera_tag(&entry.camera) {
            entry.camera = default_camera(tag);
        }
        match &mut entry.camera {
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
                drag_row(ui, "height", height, 0.01);
                drag_row(ui, "dist", dist, 0.01);
                drag_row(ui, "pitch", pitch, 0.01);
            }
            Camera::TopDown { height } => drag_row(ui, "height", height, 0.01),
            Camera::CaptureFraming => {}
        }

        ui.horizontal(|ui| {
            ui.label("place (global x,z)");
            ui.add(
                egui::DragValue::new(&mut entry.place[0])
                    .speed(0.05)
                    .prefix("x "),
            );
            ui.add(
                egui::DragValue::new(&mut entry.place[1])
                    .speed(0.05)
                    .prefix("z "),
            );
        });
        ui.horizontal(|ui| {
            ui.label("bounds min");
            ui.add(
                egui::DragValue::new(&mut entry.bounds.min[0])
                    .speed(0.05)
                    .prefix("x "),
            );
            ui.add(
                egui::DragValue::new(&mut entry.bounds.min[1])
                    .speed(0.05)
                    .prefix("z "),
            );
        });
        ui.horizontal(|ui| {
            ui.label("bounds max");
            ui.add(
                egui::DragValue::new(&mut entry.bounds.max[0])
                    .speed(0.05)
                    .prefix("x "),
            );
            ui.add(
                egui::DragValue::new(&mut entry.bounds.max[1])
                    .speed(0.05)
                    .prefix("z "),
            );
        });
        ui.label(egui::RichText::new("Connections derive from placement at bake.").weak());
    }

    fn instance_list_ui(&mut self, ui: &mut egui::Ui) {
        let Some(stem) = self.active.clone() else {
            return;
        };
        let shift = ui.input(|i| i.modifiers.shift);
        ui.horizontal(|ui| {
            ui.heading("Instances");
            if self.sel.len() > 1 {
                ui.weak(format!("({} selected)", self.sel.len()));
            }
            if ui.button("+ literal").clicked() {
                if let Some(zone) = self.contents.get_mut(&stem) {
                    let idx = zone.instances.len();
                    zone.instances
                        .push(Placement::Lit(new_instance(self.view.center)));
                    self.sel = Sel::single(idx);
                }
            }
        });

        // Gray-box primitive blocking (#44): drop a sized box / ramp / cylinder.
        ui.horizontal(|ui| {
            ui.label("+ prim:");
            if ui.button("box").clicked() {
                self.add_primitive(Prim::Box);
            }
            if ui.button("ramp").clicked() {
                self.add_primitive(Prim::Ramp);
            }
            if ui.button("cylinder").clicked() {
                self.add_primitive(Prim::Cylinder);
            }
        });

        // "+ use <prefab>" picker — insert a prefab use at the view centre.
        let prefab_names = self.prefab_names();
        if !prefab_names.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("+ use:");
                for name in &prefab_names {
                    if ui.small_button(name).clicked() {
                        if let Some(zone) = self.contents.get_mut(&stem) {
                            let idx = zone.instances.len();
                            zone.instances.push(Placement::Use {
                                name: name.clone(),
                                pos: [self.view.center.x, 0.0, self.view.center.y],
                                rot: None,
                                scale: None,
                                material: None,
                                flags: None,
                                path: Vec::new(),
                            });
                            self.sel = Sel::single(idx);
                        }
                    }
                }
            });
        }

        let prefabs = &self.prefabs;
        let Some(zone) = self.contents.get_mut(&stem) else {
            return;
        };
        let mut to_delete = None;
        for i in 0..zone.instances.len() {
            let selected = self.sel.contains(i);
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(selected, placement_label(&zone.instances[i], prefabs))
                    .clicked()
                {
                    if shift {
                        self.sel.toggle(i);
                    } else {
                        self.sel.set_single(i);
                    }
                }
                if ui.small_button("✕").clicked() {
                    to_delete = Some(i);
                }
            });
        }
        if let Some(i) = to_delete {
            zone.instances.remove(i);
            self.sel = Sel::none();
        }
    }

    fn selected_instance_ui(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.sel.primary() else {
            ui.weak("(no instance selected)");
            return;
        };
        let Some(stem) = self.active.clone() else {
            return;
        };
        let prefabs = &self.prefabs;
        let meshes = &self.meshes;
        let thumbs = &self.mesh_thumbs;
        // Only the entry zone may author the level's one `avatar` (#27 / #54),
        // so only its literals get that role in the picker.
        let is_entry = stem == self.level.entry;
        let Some(zone) = self.contents.get_mut(&stem) else {
            return;
        };
        if i >= zone.instances.len() {
            self.sel = Sel::none();
            return;
        }

        match &mut zone.instances[i] {
            Placement::Lit(inst) => literal_instance_ui(ui, inst, meshes, thumbs, is_entry),
            Placement::Use {
                name,
                pos,
                rot,
                scale,
                material,
                flags,
                path,
            } => {
                ui.heading("Selected · use");
                // Role *and* kind are prefab-owned and read-only here — the
                // override surface is deliberately minimal (#27); a mixed
                // encounter is authored by placing a different prefab.
                let pf = prefabs.get(name);
                let role = pf.map(|p| p.role.clone()).unwrap_or_else(|| "?".into());
                let kind = pf
                    .and_then(|p| p.kind.clone())
                    .unwrap_or_else(|| "default".into());
                ui.label(format!("prefab: {name}  ({role} · {kind})"));
                ui.label("position (x, y, z)");
                vec3_row(ui, "pos", pos, 0.01);
                opt_vec3_row(ui, "rot override", rot, 0.01);
                opt_vec3_row(ui, "scale override", scale, 0.005);
                // The flags override is masked to the *prefab's* role. A missing
                // prefab has no effective role, so there's nothing to mask with.
                match pf.and_then(|p| Role::parse(&p.role)) {
                    Some(r) => opt_named_flags_ui(ui, "flags override", flags, r),
                    None => {
                        ui.weak("flags override unavailable — prefab missing or has a bad role");
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(format!("path override ({} pts)", path.len()));
                    if ui.button("+ wp").clicked() {
                        let last = path.last().copied().unwrap_or([pos[0], pos[2]]);
                        path.push(last);
                    }
                    if ui.button("− wp").clicked() {
                        path.pop();
                    }
                });
                let mut has_mat = material.is_some();
                if ui.checkbox(&mut has_mat, "material override").changed() {
                    *material = has_mat.then_some(Material {
                        diffuse: [200, 200, 210],
                        ambient: [40, 40, 55],
                    });
                }
                if let Some(m) = material {
                    ui.horizontal(|ui| {
                        ui.label("diffuse");
                        ui.color_edit_button_srgb(&mut m.diffuse);
                        ui.label("ambient");
                        ui.color_edit_button_srgb(&mut m.ambient);
                    });
                }
            }
        }

        // Promote a literal into the prefab editor (#51). The zone borrow above
        // has ended, so calling back into `self` here is fine.
        let is_lit = matches!(
            self.contents.get(&stem).and_then(|z| z.instances.get(i)),
            Some(Placement::Lit(_))
        );
        if is_lit {
            ui.separator();
            if ui
                .button("→ prefab")
                .on_hover_text("Promote this literal into the prefab editor")
                .clicked()
            {
                self.promote_selection_to_prefab();
            }
        }
    }
}

/// The literal-instance editor (the old per-instance side panel).
fn literal_instance_ui(
    ui: &mut egui::Ui,
    inst: &mut Instance,
    meshes: &[String],
    thumbs: &BTreeMap<String, MeshThumb>,
    is_entry: bool,
) {
    ui.heading("Selected · literal");

    // role + kind — both picked from the shared `kts_schema` vocabulary. There
    // is no free-text role any more: an unknown role is a hard bake Error, so
    // letting one be typed only produced a level that wouldn't build. In the
    // entry zone the picker also offers `avatar`, the one role a level is
    // *required* to author exactly once.
    role_kind_ui(ui, "lit", &mut inst.role, &mut inst.kind, is_entry);

    // mesh — a wireframe thumbnail of the current pick, then a combo whose rows
    // each carry their own preview (#52).
    ui.horizontal(|ui| {
        let cur = inst.mesh.as_deref().and_then(|m| thumbs.get(m));
        thumb_widget(ui, cur, 40.0);
        let mesh_label = inst.mesh.clone().unwrap_or_else(|| "(none)".into());
        egui::ComboBox::from_id_salt("mesh")
            .selected_text(mesh_label)
            .show_ui(ui, |ui| {
                if ui.selectable_label(inst.mesh.is_none(), "(none)").clicked() {
                    inst.mesh = None;
                }
                for m in meshes {
                    let is = inst.mesh.as_deref() == Some(m.as_str());
                    ui.horizontal(|ui| {
                        thumb_widget(ui, thumbs.get(m), 28.0);
                        if ui.selectable_label(is, m).clicked() {
                            inst.mesh = Some(m.clone());
                        }
                    });
                }
            });
    });

    ui.label("position (x, y, z)");
    vec3_row(ui, "pos", &mut inst.pos, 0.01);
    ui.label("rotation (rx, ry, rz)");
    vec3_row(ui, "rot", &mut inst.rot, 0.01);
    ui.label("scale");
    vec3_row(ui, "scale", &mut inst.scale, 0.005);

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

    // As in the prefab editor: an unparsed role (only reachable from RON edited
    // outside the editor) gets no flag UI rather than another role's mask.
    match Role::parse(&inst.role) {
        Some(r) => named_flags_ui(ui, "flags", &mut inst.flags, r),
        None => {
            ui.weak("flags unavailable — unknown role");
        }
    }

    ui.horizontal(|ui| {
        ui.label(format!("path ({} pts)", inst.path.len()));
        if ui.button("+ wp").clicked() {
            let last = inst
                .path
                .last()
                .copied()
                .unwrap_or([inst.pos[0], inst.pos[2]]);
            inst.path.push(last);
        }
        if ui.button("− wp").clicked() {
            inst.path.pop();
        }
    });
}
