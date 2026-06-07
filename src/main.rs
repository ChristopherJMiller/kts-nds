//! The game: a Bevy app that runs on the Nintendo DS.
//!
//! Everything here is ordinary Bevy — components, systems, resources. The DS
//! itself is handled entirely by the [`bevy_nds`] library via [`DsPlugins`]:
//! this file contains no FFI, no allocator and no panic handler.
//!
//! The top screen shows a title and a live HUD (driven by the `Time` and input
//! resources); the bottom screen shows an `@` marker you move with the D-pad.

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;

/// Program entry point, called by the BlocksDS crt0.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins).add_plugins(GamePlugin);
    bevy_nds::run(app)
}

/// The actual game, as a self-contained Bevy plugin.
struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            .add_systems(Update, (move_player, update_hud));
    }
}

/// The D-pad-controlled marker.
#[derive(Component)]
struct Player;

/// The live status line on the top screen.
#[derive(Component)]
struct Hud;

fn setup(mut commands: Commands) {
    // Top screen: a title and a HUD that updates every frame.
    commands.spawn((
        DsScreen::Top,
        TilePos::new(4, 2),
        DsText::new("Bevy ECS on Nintendo DS"),
    ));
    commands.spawn((DsScreen::Top, TilePos::new(4, 4), Hud, DsText::new("")));

    // Bottom screen: the player marker and a hint.
    commands.spawn((Player, DsScreen::Bottom, TilePos::new(16, 12), Glyph(b'@')));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(6, 22),
        DsText::new("D-pad: move the @"),
    ));
}

/// Move the marker one tile per frame in the held D-pad direction.
fn move_player(input: Res<ButtonInput<DsButton>>, mut query: Query<&mut TilePos, With<Player>>) {
    for mut pos in &mut query {
        if input.pressed(DsButton::Left) {
            pos.x -= 1;
        }
        if input.pressed(DsButton::Right) {
            pos.x += 1;
        }
        if input.pressed(DsButton::Up) {
            pos.y -= 1;
        }
        if input.pressed(DsButton::Down) {
            pos.y += 1;
        }
        pos.x = pos.x.clamp(0, 31);
        pos.y = pos.y.clamp(0, 23);
    }
}

/// Refresh the top-screen HUD from the `Time`, `Fps` and input resources.
fn update_hud(
    time: Res<Time>,
    fps: Res<Fps>,
    input: Res<ButtonInput<DsButton>>,
    mut query: Query<&mut DsText, With<Hud>>,
) {
    let secs = time.elapsed_secs() as u32;
    let fps = fps.0;
    let held = input.get_pressed().count();
    for mut text in &mut query {
        // Reuse the existing String's capacity instead of allocating anew.
        text.0.clear();
        let _ = write!(text.0, "t={secs:>4}s  fps={fps:>2.0}  held={held}");
    }
}
