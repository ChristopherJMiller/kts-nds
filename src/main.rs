//! The game: a Bevy app that runs on the Nintendo DS.
//!
//! Everything here is ordinary Bevy — components, systems, resources. The DS
//! itself is handled entirely by the [`bevy_nds`] library via [`DsPlugins`]
//! (the platform layer) and [`bevy_nds_3d`] via [`Ds3dPlugin`] (the hardware 3D
//! backend): this file contains no FFI, no allocator and no panic handler.
//!
//! A hardware-rendered 3D cube starts on the bottom screen. The D-pad moves it
//! around, and when it runs off the top or bottom edge it *travels to the other
//! screen* — because the DS 3D core is wired to the main engine, "moving" the
//! cube across screens is a coupled LCD swap that sends the text HUD the
//! opposite way. ABXY tumble the cube so you can see the 3D engine shade it.

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_3d::prelude::*;

/// Program entry point, called by the BlocksDS crt0.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins)
        .add_plugins(Ds3dPlugin)
        .add_plugins(GamePlugin);
    bevy_nds::run(app)
}

/// The actual game, as a self-contained Bevy plugin.
struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        // Start with the 3D cube on the bottom screen (text rides the other one).
        app.insert_resource(Display3d {
            screen: DsScreen::Bottom,
        })
        .add_systems(Startup, setup)
        .add_systems(Update, (move_cube, rotate_cube, update_hud));
    }
}

/// The player-controlled cube.
#[derive(Component)]
struct Cube;

/// The live status line on the text screen.
#[derive(Component)]
struct Hud;

/// World-space Y past which the cube has left the screen and crosses to the
/// other one. Sized to the camera frustum so the cube is fully off-screen first.
const EDGE: f32 = 1.6;

fn setup(mut commands: Commands) {
    // The hardware-rendered cube. A non-zero initial tilt shows several faces at
    // once, so it reads as a cube (not a flat square) even before any input.
    commands.spawn((
        Cube,
        DsMesh::cube(0.6),
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(0.5, 0.6, 0.0),
        },
    ));

    // Text console (sub engine): title, a per-frame HUD, and a control hint.
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(5, 2),
        DsText::new("Bevy 3D on Nintendo DS"),
    ));
    commands.spawn((DsScreen::Bottom, TilePos::new(5, 4), Hud, DsText::new("")));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 21),
        DsText::new("D-pad: move (crosses screens)"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(6, 22),
        DsText::new("ABXY: rotate the cube"),
    ));
}

/// Move the cube with the D-pad. When it runs off the top or bottom edge, swap
/// which screen the 3D engine draws to and re-enter from the opposite edge, so
/// the cube appears to travel between the two LCDs.
fn move_cube(
    input: Res<ButtonInput<DsButton>>,
    mut display: ResMut<Display3d>,
    mut query: Query<&mut Transform3d, With<Cube>>,
) {
    const SPEED: f32 = 0.04;
    for mut transform in &mut query {
        if input.pressed(DsButton::Left) {
            transform.translation.x -= SPEED;
        }
        if input.pressed(DsButton::Right) {
            transform.translation.x += SPEED;
        }
        if input.pressed(DsButton::Up) {
            transform.translation.y += SPEED;
        }
        if input.pressed(DsButton::Down) {
            transform.translation.y -= SPEED;
        }
        transform.translation.x = transform.translation.x.clamp(-1.5, 1.5);

        // Off the top: hop to the other screen, re-entering from the bottom.
        if transform.translation.y > EDGE {
            transform.translation.y = -EDGE;
            display.screen = other(display.screen);
        // Off the bottom: hop the other way, re-entering from the top.
        } else if transform.translation.y < -EDGE {
            transform.translation.y = EDGE;
            display.screen = other(display.screen);
        }
    }
}

/// Tumble the cube with the face buttons so the 3D shading is visible: Y/A yaw
/// left/right, X/B pitch up/down.
fn rotate_cube(input: Res<ButtonInput<DsButton>>, mut query: Query<&mut Transform3d, With<Cube>>) {
    const SPEED: f32 = 0.06;
    for mut transform in &mut query {
        if input.pressed(DsButton::A) {
            transform.rotation.y += SPEED;
        }
        if input.pressed(DsButton::Y) {
            transform.rotation.y -= SPEED;
        }
        if input.pressed(DsButton::X) {
            transform.rotation.x -= SPEED;
        }
        if input.pressed(DsButton::B) {
            transform.rotation.x += SPEED;
        }
    }
}

/// The opposite screen.
fn other(screen: DsScreen) -> DsScreen {
    match screen {
        DsScreen::Top => DsScreen::Bottom,
        DsScreen::Bottom => DsScreen::Top,
    }
}

/// Refresh the HUD from the `Time`, `Fps` and `Display3d` resources.
fn update_hud(
    time: Res<Time>,
    fps: Res<Fps>,
    display: Res<Display3d>,
    mut query: Query<&mut DsText, With<Hud>>,
) {
    let secs = time.elapsed_secs() as u32;
    let fps = fps.0;
    let cube_on = match display.screen {
        DsScreen::Top => "top",
        DsScreen::Bottom => "bottom",
    };
    for mut text in &mut query {
        // Reuse the existing String's capacity instead of allocating anew.
        text.0.clear();
        let _ = write!(text.0, "t={secs:>4}s fps={fps:>2.0} cube={cube_on}");
    }
}
