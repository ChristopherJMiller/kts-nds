//! The right-hand side panel + top menu bar: level manifest (name/entry/zone
//! list), active-zone camera/place/bounds, the instance list (literals + prefab
//! uses), and the selected-instance editor.

use eframe::egui;
use scene2bin::{Camera, Instance, Material, Placement};

use crate::app::{EditorApp, Sel, View};
use crate::widgets::{
    camera_tag, default_camera, drag_row, new_instance, opt_flags_row, opt_vec3_row,
    placement_label, vec3_row,
};

/// Common roles offered in the role picker for literal instances (free text
/// still allowed). Prefab uses take their role from the prefab.
const ROLES: &[&str] = &["avatar", "enemy", "landmark", "spawn", "prop"];

impl EditorApp {
    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("level dir:");
            ui.add(egui::TextEdit::singleline(&mut self.level_dir).desired_width(360.0));
            if ui.button("Load").clicked() {
                self.load();
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            ui.separator();
            if ui.button("Reset view").clicked() {
                self.view = View { center: egui::Vec2::ZERO, scale: 90.0 };
            }
            if ui.button("Frame all").clicked() {
                self.frame_all();
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
        });
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
            let tag = if self.level.entry == *s { format!("{s}  ★") } else { s.clone() };
            ui.horizontal(|ui| {
                if ui.selectable_label(is_active, tag).clicked() {
                    self.active = Some(s.clone());
                    self.sel = Sel::None;
                }
                if ui.small_button("✕").clicked() {
                    self.level.zones.remove(s);
                    self.contents.remove(s);
                    if self.active.as_deref() == Some(s.as_str()) {
                        self.active = self.level.zones.keys().next().cloned();
                        self.sel = Sel::None;
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
        let Some(entry) = self.level.zones.get_mut(&stem) else {
            return;
        };
        ui.heading(format!("Zone · {stem}"));

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
            Camera::Follow { height, dist, pitch } | Camera::Rail2_5D { height, dist, pitch } => {
                drag_row(ui, "height", height, 0.01);
                drag_row(ui, "dist", dist, 0.01);
                drag_row(ui, "pitch", pitch, 0.01);
            }
            Camera::TopDown { height } => drag_row(ui, "height", height, 0.01),
            Camera::CaptureFraming => {}
        }

        ui.horizontal(|ui| {
            ui.label("place (global x,z)");
            ui.add(egui::DragValue::new(&mut entry.place[0]).speed(0.05).prefix("x "));
            ui.add(egui::DragValue::new(&mut entry.place[1]).speed(0.05).prefix("z "));
        });
        ui.horizontal(|ui| {
            ui.label("bounds min");
            ui.add(egui::DragValue::new(&mut entry.bounds.min[0]).speed(0.05).prefix("x "));
            ui.add(egui::DragValue::new(&mut entry.bounds.min[1]).speed(0.05).prefix("z "));
        });
        ui.horizontal(|ui| {
            ui.label("bounds max");
            ui.add(egui::DragValue::new(&mut entry.bounds.max[0]).speed(0.05).prefix("x "));
            ui.add(egui::DragValue::new(&mut entry.bounds.max[1]).speed(0.05).prefix("z "));
        });
        ui.label(egui::RichText::new("Connections derive from placement at bake.").weak());
    }

    fn instance_list_ui(&mut self, ui: &mut egui::Ui) {
        let Some(stem) = self.active.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.heading("Instances");
            if ui.button("+ literal").clicked() {
                if let Some(zone) = self.contents.get_mut(&stem) {
                    let idx = zone.instances.len();
                    zone.instances.push(Placement::Lit(new_instance(self.view.center)));
                    self.sel = Sel::Instance(idx);
                }
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
                            self.sel = Sel::Instance(idx);
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
            let selected = self.sel == Sel::Instance(i);
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(selected, placement_label(&zone.instances[i], prefabs))
                    .clicked()
                {
                    self.sel = Sel::Instance(i);
                }
                if ui.small_button("✕").clicked() {
                    to_delete = Some(i);
                }
            });
        }
        if let Some(i) = to_delete {
            zone.instances.remove(i);
            self.sel = Sel::None;
        }
    }

    fn selected_instance_ui(&mut self, ui: &mut egui::Ui) {
        let Sel::Instance(i) = self.sel else {
            ui.weak("(no instance selected)");
            return;
        };
        let Some(stem) = self.active.clone() else {
            return;
        };
        let prefabs = &self.prefabs;
        let meshes = &self.meshes;
        let Some(zone) = self.contents.get_mut(&stem) else {
            return;
        };
        if i >= zone.instances.len() {
            self.sel = Sel::None;
            return;
        }

        match &mut zone.instances[i] {
            Placement::Lit(inst) => literal_instance_ui(ui, inst, meshes),
            Placement::Use { name, pos, rot, scale, material, flags, path } => {
                ui.heading("Selected · use");
                let role = prefabs.get(name).map(|p| p.role.clone()).unwrap_or_else(|| "?".into());
                ui.label(format!("prefab: {name}  (role {role})"));
                ui.label("position (x, y, z)");
                vec3_row(ui, "pos", pos, 0.01);
                opt_vec3_row(ui, "rot override", rot, 0.01);
                opt_vec3_row(ui, "scale override", scale, 0.005);
                opt_flags_row(ui, "flags override", flags);
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
                    *material =
                        has_mat.then_some(Material { diffuse: [200, 200, 210], ambient: [40, 40, 55] });
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
    }
}

/// The literal-instance editor (the old per-instance side panel).
fn literal_instance_ui(ui: &mut egui::Ui, inst: &mut Instance, meshes: &[String]) {
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

    // mesh — "(none)" or any baked .obj stem.
    let mesh_label = inst.mesh.clone().unwrap_or_else(|| "(none)".into());
    egui::ComboBox::from_id_salt("mesh")
        .selected_text(mesh_label)
        .show_ui(ui, |ui| {
            if ui.selectable_label(inst.mesh.is_none(), "(none)").clicked() {
                inst.mesh = None;
            }
            for m in meshes {
                let is = inst.mesh.as_deref() == Some(m.as_str());
                if ui.selectable_label(is, m).clicked() {
                    inst.mesh = Some(m.clone());
                }
            }
        });

    ui.label("position (x, y, z)");
    vec3_row(ui, "pos", &mut inst.pos, 0.01);
    ui.label("rotation (rx, ry, rz)");
    vec3_row(ui, "rot", &mut inst.rot, 0.01);
    ui.label("scale");
    vec3_row(ui, "scale", &mut inst.scale, 0.005);

    let mut has_mat = inst.material.is_some();
    if ui.checkbox(&mut has_mat, "lit material").changed() {
        inst.material = has_mat.then_some(Material { diffuse: [200, 200, 210], ambient: [40, 40, 55] });
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
