//! `scene-editor` — a desktop editor for Kill the Serpent *levels* (issue #27).
//! It is a pure front-end over the `scene2bin` RON format: open a level
//! directory (`assets/levels/<name>/`), lay its zones out on one shared top-down
//! (XZ) canvas at their global `place`, drag instances / waypoints / whole
//! zones, tweak camera / bounds / roles / prefab uses in a side panel, and save
//! the manifest + zone files back. Connections between zones are **derived** at
//! bake time from where their bounds abut, so there are no exits to author.
//!
//! The build pipeline (`scene2bin` → `.scene` → `bevy_nds_scene`) is unchanged,
//! and `preview-rom` remains the DS-faithful check — this tool is for fast
//! spatial layout, not pixel-accurate preview.
//!
//! Modules: [`app`] (state + IO + the `eframe::App` entry), [`canvas`] (the
//! top-down drawing + pointer interaction), [`panel`] (the side panel UI), and
//! [`widgets`] (reusable rows + display helpers).
//!
//! Run it from this directory: `cargo run` (or `just edit` / `just edit <dir>`
//! from the repo root).

#![windows_subsystem = "windows"]

mod app;
mod canvas;
mod panel;
mod widgets;

use app::EditorApp;

fn main() -> eframe::Result {
    let start_path = std::env::args().nth(1);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([1180.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "kts · level editor",
        options,
        Box::new(move |_cc| Ok(Box::new(EditorApp::new(start_path)))),
    )
}
