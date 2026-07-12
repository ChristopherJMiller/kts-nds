//! Options menu + the app's first state machine.
//!
//! This is the bones of a reusable settings surface. [`GameMode`] is a
//! lightweight state resource (Playing ↔ Options) — a deliberate stand-in for a
//! full `bevy_state` machine, kept dependency-free for the `no_std` DS build and
//! swappable later. [`Settings`] is the user-facing model the menu edits;
//! [`apply_settings`] mirrors it onto the live engine resources ([`DsRenderStyle`]
//! for the 3D "look", [`Handedness`] for input). Persistence (via
//! `bevy_nds_save`) is deliberately deferred — serialize [`Settings`] when we get
//! there.
//!
//! The menu lives on the bottom (touch) screen and is driven by **stylus tap**
//! (Pillar 1 — the pen is the power) with **cluster + face-button** navigation as
//! a fallback. Opening it (Select) pauses gameplay, so the top-screen 3D freezes
//! and every render toggle is visible live as you flip it.

use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_3d::prelude::DsRenderStyle;

/// The app's coarse state. Gameplay systems run only in [`GameMode::Playing`];
/// [`GameMode::Options`] pauses the sim and shows the menu.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameMode {
    #[default]
    Playing,
    Options,
}

/// Run condition: the gameplay sim is live.
pub fn playing(mode: Res<GameMode>) -> bool {
    *mode == GameMode::Playing
}

/// Run condition: the options menu is open.
pub fn in_options(mode: Res<GameMode>) -> bool {
    *mode == GameMode::Options
}

/// The user-facing settings model — the single source of truth the menu edits.
/// [`apply_settings`] pushes it onto the live engine resources. Add fields here
/// (audio levels, difficulty, …) as options grow; this is what a future save
/// slot would serialize.
#[derive(Resource, Clone, Copy)]
pub struct Settings {
    pub edge_marking: bool,
    pub handedness: Handedness,
}

impl Default for Settings {
    fn default() -> Self {
        // Must match the DsRenderStyle / Handedness defaults so the first
        // `apply_settings` is a no-op.
        Self {
            edge_marking: true,
            handedness: Handedness::Right,
        }
    }
}

/// Latched request to re-arm the enemies, raised by the menu's Reset item and
/// consumed by the game's reset system (which also honours the START button).
#[derive(Resource, Default)]
pub struct PendingReset(pub bool);

/// Transient menu cursor (which row the cluster has highlighted).
#[derive(Resource, Default)]
pub struct MenuUi {
    pub selected: usize,
}

/// A menu text line on the bottom screen. `item` is the [`MenuItem`] index it
/// renders, or `None` for the title line.
#[derive(Component)]
struct MenuLine {
    item: Option<usize>,
}

/// The selectable rows, in display order.
#[derive(Clone, Copy)]
enum MenuItem {
    EdgeMarking,
    Handedness,
    ResetEnemies,
    Close,
}

const ITEMS: [MenuItem; 4] = [
    MenuItem::EdgeMarking,
    MenuItem::Handedness,
    MenuItem::ResetEnemies,
    MenuItem::Close,
];

/// Tile row of the first item line; the title sits two rows above. The libnds
/// console font is 8 px per tile, so screen-Y `py` maps to item
/// `py / 8 - ITEM_Y0`.
const ITEM_Y0: usize = 6;
/// Tile row of the title line.
const TITLE_Y: usize = 4;
/// Left tile column the menu text starts at.
const MENU_X: i16 = 2;

/// Spawn the (persistent) menu text lines up front — one title + one per item.
/// They render blank while playing (the text renderer diffs, so an empty string
/// costs nothing) and fill in when the menu opens, avoiding per-frame
/// spawn/despawn churn.
fn setup_menu(mut commands: Commands) {
    let b = DsScreen::Bottom;
    commands.spawn((
        b,
        TilePos::new(MENU_X, TITLE_Y as i16),
        MenuLine { item: None },
        DsText::new(""),
    ));
    for i in 0..ITEMS.len() {
        commands.spawn((
            b,
            TilePos::new(MENU_X, (ITEM_Y0 + i) as i16),
            MenuLine { item: Some(i) },
            DsText::new(""),
        ));
    }
}

/// Select opens / closes the menu, toggling [`GameMode`]. Opening resets the
/// cluster cursor to the top item.
fn toggle_menu(
    input: Res<ButtonInput<DsButton>>,
    mut mode: ResMut<GameMode>,
    mut ui: ResMut<MenuUi>,
) {
    if !input.just_pressed(DsButton::Select) {
        return;
    }
    *mode = match *mode {
        GameMode::Playing => {
            ui.selected = 0;
            GameMode::Options
        }
        GameMode::Options => GameMode::Playing,
    };
}

/// Handle menu navigation + activation while it's open: cluster Up/Down move the
/// highlight, a face button (A) activates it, and a stylus tap on a row both
/// selects and activates it in one motion.
fn menu_input(
    buttons: Res<ButtonInput<DsButton>>,
    touches: Res<Touches>,
    mut ui: ResMut<MenuUi>,
    mut settings: ResMut<Settings>,
    mut mode: ResMut<GameMode>,
    mut pending: ResMut<PendingReset>,
) {
    let n = ITEMS.len();

    // Cluster navigation (raw buttons — handedness doesn't matter for a menu).
    if buttons.just_pressed(DsButton::Up) {
        ui.selected = (ui.selected + n - 1) % n;
    }
    if buttons.just_pressed(DsButton::Down) {
        ui.selected = (ui.selected + 1) % n;
    }
    if buttons.just_pressed(DsButton::A) {
        activate(ui.selected, &mut settings, &mut mode, &mut pending);
        return;
    }

    // Stylus: a fresh touch on an item row selects and activates it directly.
    if let Some(touch) = touches.iter_just_pressed().next() {
        let row = (touch.position().y as usize) / 8;
        if row >= ITEM_Y0 && row < ITEM_Y0 + n {
            let item = row - ITEM_Y0;
            ui.selected = item;
            activate(item, &mut settings, &mut mode, &mut pending);
        }
    }
}

/// Apply the effect of the item at `idx`.
fn activate(
    idx: usize,
    settings: &mut Settings,
    mode: &mut GameMode,
    pending: &mut PendingReset,
) {
    match ITEMS[idx] {
        MenuItem::EdgeMarking => settings.edge_marking = !settings.edge_marking,
        MenuItem::Handedness => {
            settings.handedness = match settings.handedness {
                Handedness::Right => Handedness::Left,
                Handedness::Left => Handedness::Right,
            };
        }
        MenuItem::ResetEnemies => {
            // Close so the (Playing-gated) reset system runs the reset next frame.
            pending.0 = true;
            *mode = GameMode::Playing;
        }
        MenuItem::Close => *mode = GameMode::Playing,
    }
}

/// Redraw the menu lines: labels + current values while open, blank while
/// playing. Reuses each `DsText`'s `String` capacity (clear + `write!`) per the
/// no-per-frame-heap convention.
fn render_menu(
    mode: Res<GameMode>,
    ui: Res<MenuUi>,
    settings: Res<Settings>,
    mut lines: Query<(&MenuLine, &mut DsText)>,
) {
    let open = *mode == GameMode::Options;
    for (line, mut text) in &mut lines {
        text.0.clear();
        if !open {
            continue;
        }
        match line.item {
            None => {
                let _ = write!(text.0, "== OPTIONS ==  Select: close");
            }
            Some(i) => {
                let cursor = if ui.selected == i { '>' } else { ' ' };
                let _ = write!(text.0, "{} {}", cursor, item_text(ITEMS[i], &settings));
            }
        }
    }
}

/// The label + value string for one item.
fn item_text(item: MenuItem, s: &Settings) -> alloc::string::String {
    use alloc::string::ToString;
    let on = |b: bool| if b { "ON " } else { "OFF" };
    match item {
        MenuItem::EdgeMarking => alloc::format!("Outlines     [{}]", on(s.edge_marking)),
        MenuItem::Handedness => alloc::format!(
            "Handedness   [{}]",
            match s.handedness {
                Handedness::Right => "R",
                Handedness::Left => "L",
            }
        ),
        MenuItem::ResetEnemies => "Reset enemies".to_string(),
        MenuItem::Close => "Close".to_string(),
    }
}

/// Mirror [`Settings`] onto the live engine resources whenever it changes.
/// Mutating [`DsRenderStyle`] re-runs the 3D backend's `apply_render_style`, so
/// the top screen updates the instant a toggle flips.
fn apply_settings(
    settings: Res<Settings>,
    mut style: ResMut<DsRenderStyle>,
    mut handed: ResMut<Handedness>,
) {
    if !settings.is_changed() {
        return;
    }
    style.edge_marking = settings.edge_marking;
    *handed = settings.handedness;
}

/// Register the menu: state + settings resources, the setup, and the per-frame
/// systems (input gated to the open menu; the rest run every frame so the menu
/// can blank itself and settings stay mirrored).
pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameMode>()
            .init_resource::<Settings>()
            .init_resource::<MenuUi>()
            .init_resource::<PendingReset>()
            .add_systems(Startup, setup_menu)
            .add_systems(
                Update,
                (
                    toggle_menu,
                    menu_input.run_if(in_options),
                    apply_settings,
                    render_menu,
                )
                    .chain(),
            );
    }
}
