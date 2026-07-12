//! The right-hand side panel + top menu bar: level manifest (name/entry/zone
//! list), active-zone camera/place/bounds, the instance list (literals + prefab
//! uses), and the selected-instance editor.

use std::collections::BTreeMap;

use eframe::egui;
use scene2bin::{Camera, Instance, Material, Placement};

use crate::app::{EditorApp, Prim, Sel, View, ViewMode};
use crate::widgets::{
    MeshThumb, camera_tag, default_camera, default_prefab, drag_row, named_flags_ui, new_instance,
    opt_named_flags_ui, opt_vec3_row, placement_label, thumb_widget, vec3_row,
};

/// Common roles offered in the role picker for literal instances (free text
/// still allowed). Prefab uses take their role from the prefab. `avatar` is
/// **not** offered: since #27 (2026-06-28) the avatar is one per-level persistent
/// entity seeded at the `entry` zone, never authored per-zone (#54).
const ROLES: &[&str] = &["enemy", "landmark", "spawn", "prop", "block"];

/// One entry in the live problems panel (#53): a message, optionally scoped to a
/// zone (click-to-focus).
struct Problem {
    zone: Option<String>,
    msg: String,
}

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
            ui.horizontal(|ui| {
                ui.label("role:");
                ui.text_edit_singleline(&mut prefab.role);
            });
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
            named_flags_ui(ui, "flags", &mut prefab.flags);
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

    /// Live problems panel (#53): re-runs `assemble` + `validate` +
    /// `isolation_warnings` every frame (the same path `build.rs` bakes and the
    /// overlay draws) and lists issues, each click-to-focus on its zone — so an
    /// invalid state surfaces immediately, not only at save.
    fn problems_ui(&mut self, ui: &mut egui::Ui) {
        let problems = self.compute_problems();
        ui.horizontal(|ui| {
            ui.heading("Problems");
            ui.weak(format!("({})", problems.len()));
        });
        if problems.is_empty() {
            ui.weak("none — level is valid");
            return;
        }
        let mut focus = None;
        for p in &problems {
            let text = match &p.zone {
                Some(z) => format!("⚠  {z}: {}", p.msg),
                None => format!("⚠  {}", p.msg),
            };
            let label = egui::Label::new(
                egui::RichText::new(text).color(egui::Color32::from_rgb(224, 150, 90)),
            )
            .sense(egui::Sense::click())
            .wrap();
            if ui.add(label).clicked() {
                focus = p.zone.clone();
            }
        }
        if let Some(z) = focus {
            if self.level.zones.contains_key(&z) {
                self.active = Some(z);
                self.sel = Sel::none();
            }
        }
    }

    /// Collect the current validation / isolation problems (see [`Self::problems_ui`]).
    fn compute_problems(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        match scene2bin::assemble(&self.level, &self.contents, &self.prefabs) {
            Ok(zones) => {
                let mesh_exists = |name: &str| self.meshes.iter().any(|m| m == name);
                for (stem, space) in &zones {
                    if let Err(e) = scene2bin::validate(space, mesh_exists) {
                        out.push(Problem {
                            zone: Some(stem.clone()),
                            msg: format!("{e}"),
                        });
                    }
                }
                let conns = scene2bin::derive_connections(&zones);
                for stem in scene2bin::isolation_warnings(&conns).keys() {
                    out.push(Problem {
                        zone: Some(stem.clone()),
                        msg: "isolated zone (abuts no neighbour)".to_string(),
                    });
                }
            }
            Err(e) => out.push(Problem {
                zone: None,
                msg: format!("{e}"),
            }),
        }
        out
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
        // The entry zone seeds the level's single persistent avatar (#27 / #54) —
        // zones never author an avatar instance themselves.
        if self.level.entry == stem {
            ui.label(
                egui::RichText::new("★ entry zone — the level's one persistent avatar seeds here")
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
        let Some(zone) = self.contents.get_mut(&stem) else {
            return;
        };
        if i >= zone.instances.len() {
            self.sel = Sel::none();
            return;
        }

        match &mut zone.instances[i] {
            Placement::Lit(inst) => literal_instance_ui(ui, inst, meshes, thumbs),
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
                let role = prefabs
                    .get(name)
                    .map(|p| p.role.clone())
                    .unwrap_or_else(|| "?".into());
                ui.label(format!("prefab: {name}  (role {role})"));
                ui.label("position (x, y, z)");
                vec3_row(ui, "pos", pos, 0.01);
                opt_vec3_row(ui, "rot override", rot, 0.01);
                opt_vec3_row(ui, "scale override", scale, 0.005);
                opt_named_flags_ui(ui, "flags override", flags);
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
) {
    ui.heading("Selected · literal");

    // role — common presets via combo, plus free text.
    egui::ComboBox::from_id_salt("role")
        .selected_text(&inst.role)
        .show_ui(ui, |ui| {
            for r in ROLES {
                ui.selectable_value(&mut inst.role, r.to_string(), *r);
            }
        });
    ui.horizontal(|ui| {
        ui.label("role:");
        ui.text_edit_singleline(&mut inst.role);
    });

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

    named_flags_ui(ui, "flags", &mut inst.flags);

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
