//! The game: a Bevy app that runs on the Nintendo DS.
//!
//! Everything here is ordinary Bevy — components, systems, resources. The DS
//! itself is handled entirely by the [`bevy_nds`] library via [`DsPlugins`]
//! (the platform layer), [`bevy_nds_3d`] via [`Ds3dPlugin`] (the hardware 3D
//! backend) and [`bevy_nds_audio`] via [`AudioPlugin`] (maxmod sound): this
//! file contains no FFI, no allocator and no panic handler.
//!
//! A hardware-rendered, hardware-*lit* Utah teapot starts on the bottom screen,
//! with a smaller second teapot spinning beside it. The model is loaded at
//! runtime from the ROM filesystem (NitroFS): `build.rs` bakes
//! `assets/teapot.obj` into `nitro:/teapot.dl` and [`DsMesh::load`] reads it on
//! startup, falling back to a copy baked into the binary with [`include_obj!`]
//! if the filesystem is unavailable. The D-pad moves the player's teapot around,
//! and when it runs off the top or bottom edge it *travels to the other screen*
//! (a coupled LCD swap, since the DS 3D core is wired to the main engine). ABXY
//! tumble it so you can watch the hardware lighting play across the surface.
//! Looping piano music plays from the baked soundbank (START toggles it), and
//! tapping a teapot fires a click SFX.

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_3d::prelude::*;
use bevy_nds_audio::prelude::*;

/// Numeric sound IDs generated at build time by `wav2bank` from the soundbank
/// header (e.g. `SFX_PIANO_LOOP`, `SFX_BLIP_SELECT`), so game code never hard-codes
/// indices. Written to `$OUT_DIR/sounds.rs` by `build.rs`.
mod sounds {
    #![allow(dead_code)] // mmutil also emits MSL_* bank-metadata counts we don't use.
    include!(concat!(env!("OUT_DIR"), "/sounds.rs"));
}

/// Program entry point, called by the BlocksDS crt0.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins)
        .add_plugins(Ds3dPlugin)
        .add_plugins(NitroFsPlugin)
        .add_plugins(AudioPlugin)
        .add_plugins(GamePlugin);
    bevy_nds::run(app)
}

/// The actual game, as a self-contained Bevy plugin.
struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        // Start with the 3D model on the bottom screen (text rides the other one).
        app.insert_resource(Display3d {
            screen: DsScreen::Bottom,
        })
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                move_model,
                rotate_model,
                spin_companion,
                update_hud,
                update_touch_hud,
                update_pick_hud,
                update_gesture_hud,
                poke_picked,
                toggle_music,
                update_audio_hud,
            ),
        );
    }
}

/// The player-controlled model.
#[derive(Component)]
struct Model;

/// A second, autonomous teapot that simply spins in place. It shares the
/// player's geometry but has its own [`Transform3d`], so every frame the
/// renderer composes and uploads two independent model matrices.
#[derive(Component)]
struct Companion;

/// The live status line on the text screen.
#[derive(Component)]
struct Hud;

/// A second status line that echoes the touch-screen position.
#[derive(Component)]
struct TouchHud;

/// A status line naming which teapot the pen is currently over (via picking).
#[derive(Component)]
struct PickHud;

/// A status line showing the most recent touch gesture.
#[derive(Component)]
struct GestureHud;

/// A status line reflecting the music state (playing/muted).
#[derive(Component)]
struct AudioHud;

/// World-space Y past which the model has left the screen and crosses to the
/// other one. Sized to the camera frustum so the model is fully off-screen first.
const EDGE: f32 = 1.6;

fn setup(mut commands: Commands, nitrofs: Res<NitroFs>, mut music: ResMut<Music>) {
    // The Utah teapot. We prefer to load it at runtime from the ROM filesystem
    // (NitroFS) — `build.rs` bakes `assets/teapot.obj` into `nitro:/teapot.dl`,
    // which `just rom` packs into the ROM. This keeps large models out of the
    // ARM9 binary (precious main RAM) and lets us swap assets without relinking.
    // If the filesystem isn't available (e.g. a loader that doesn't provide
    // argv[0]), we fall back to the copy baked straight into the ROM by
    // `include_obj!`. Either way the geometry is byte-identical (shared encoder).
    //
    // The model is authored sitting on the XY plane (pivot at its base), so both
    // paths recentre it (`center`) so it rotates about its visual middle.
    let loaded = nitrofs
        .ready
        .then(|| DsMesh::load(b"nitro:/teapot.dl\0"))
        .flatten();
    let from_nitrofs = loaded.is_some();
    let teapot = loaded.unwrap_or_else(|| include_obj!("assets/teapot.obj", center));
    // The companion shares the same geometry (cheap Cow clone of the display list).
    let companion = teapot.clone();

    commands.spawn((
        Model,
        teapot,
        DsMaterial {
            diffuse: [120, 170, 215],
            ambient: [28, 36, 56],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(-1.3, 0.5, 0.0),
            scale: Vec3::splat(0.4),
        },
    ));

    // A smaller second teapot, off to the side, that spins on its own. It proves
    // out multiple transformed meshes per frame (and the per-object CPU matrix
    // compose + frustum culling that go with them).
    commands.spawn((
        Companion,
        companion,
        DsMaterial {
            diffuse: [215, 150, 90],
            ambient: [48, 34, 20],
        },
        Transform3d {
            translation: Vec3::new(0.95, -0.55, 0.0),
            rotation: Vec3::new(-1.3, 0.0, 0.0),
            scale: Vec3::splat(0.22),
        },
    ));

    // Text console (sub engine): title, a per-frame HUD, and a control hint.
    // Title doubles as proof of where the model came from this boot.
    let source = if from_nitrofs {
        "teapot from nitro:/teapot.dl"
    } else {
        "teapot baked in (no NitroFS)"
    };
    commands.spawn((DsScreen::Bottom, TilePos::new(2, 2), DsText::new(source)));
    commands.spawn((DsScreen::Bottom, TilePos::new(5, 4), Hud, DsText::new("")));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 6),
        TouchHud,
        DsText::new("touch: --"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 7),
        PickHud,
        DsText::new("picked: none"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 8),
        GestureHud,
        DsText::new("gesture: --"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 9),
        AudioHud,
        DsText::new("music: --"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 20),
        DsText::new("tap a teapot to pick it"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 21),
        DsText::new("D-pad: move (crosses screens)"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(5, 22),
        DsText::new("ABXY: rotate  START: mute"),
    ));

    // Kick off the looping piano background music. `Music` is declarative — the
    // audio backend reconciles the hardware to this each frame — so starting it
    // here in `Startup` is safe regardless of when the soundbank finishes
    // mounting in `PreStartup`.
    music.play(SoundId(sounds::SFX_PIANO_LOOP));
}

/// Move the model with the D-pad. When it runs off the top or bottom edge, swap
/// which screen the 3D engine draws to and re-enter from the opposite edge, so
/// the model appears to travel between the two LCDs.
fn move_model(
    input: Res<ButtonInput<DsButton>>,
    mut display: ResMut<Display3d>,
    mut query: Query<&mut Transform3d, With<Model>>,
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

/// Tumble the model with the face buttons so the lighting is visible: Y/A yaw
/// left/right, X/B pitch up/down.
fn rotate_model(
    input: Res<ButtonInput<DsButton>>,
    mut query: Query<&mut Transform3d, With<Model>>,
) {
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

/// Slowly spin the autonomous companion teapot so the two models animate
/// independently.
fn spin_companion(time: Res<Time>, mut query: Query<&mut Transform3d, With<Companion>>) {
    let dt = time.delta_secs();
    for mut transform in &mut query {
        transform.rotation.y += dt;
    }
}

/// Echo the touch-screen state to its HUD line. Reads the standard Bevy
/// [`Touches`] resource that `bevy_nds` populates from the DS digitizer, showing
/// the live pixel coordinates while the pen is down and `--` when it is up.
fn update_touch_hud(touches: Res<Touches>, mut query: Query<&mut DsText, With<TouchHud>>) {
    for mut text in &mut query {
        text.0.clear();
        if let Some(touch) = touches.iter().next() {
            let pos = touch.position();
            let _ = write!(text.0, "touch: {:>3},{:>3}", pos.x as i32, pos.y as i32);
        } else {
            let _ = write!(text.0, "touch: --");
        }
    }
}

/// Name the entity the pen is over, by checking it against the teapot markers.
fn pick_name(pick: &TouchPick, model: Entity, companion: Entity) -> &'static str {
    match pick.entity {
        Some(e) if e == model => "player",
        Some(e) if e == companion => "companion",
        Some(_) => "?",
        None => "none",
    }
}

/// Report which teapot the pen is hovering over, using the engine's hardware
/// [`TouchPick`] result. This is the "did we touch teapot 1 or 2" readout: the
/// 3D engine picks whichever mesh is under the pen each frame.
fn update_pick_hud(
    pick: Res<TouchPick>,
    model: Single<Entity, With<Model>>,
    companion: Single<Entity, With<Companion>>,
    mut query: Query<&mut DsText, With<PickHud>>,
) {
    let name = pick_name(&pick, *model, *companion);
    for mut text in &mut query {
        text.0.clear();
        let _ = write!(text.0, "picked: {name}");
    }
}

/// Tapping a teapot gives it a visible nudge, proving the pick is the real
/// entity: the picked teapot (and only it) tumbles on each fresh pen-down. The
/// same pen-down fires a click SFX so selecting an object is audible.
fn poke_picked(
    touches: Res<Touches>,
    pick: Res<TouchPick>,
    mut sfx: EventWriter<PlaySfx>,
    mut query: Query<&mut Transform3d>,
) {
    if !touches.any_just_pressed() {
        return;
    }
    if let Some(entity) = pick.entity
        && let Ok(mut transform) = query.get_mut(entity)
    {
        transform.rotation.y += core::f32::consts::FRAC_PI_2;
        sfx.write(PlaySfx::new(SoundId(sounds::SFX_BLIP_SELECT)));
    }
}

/// Toggle the background music on and off with START, demonstrating the
/// declarative [`Music`] resource: setting/clearing the desired track is all the
/// game does; the backend reconciles the hardware.
fn toggle_music(input: Res<ButtonInput<DsButton>>, mut music: ResMut<Music>) {
    if input.just_pressed(DsButton::Start) {
        if music.is_playing() {
            music.stop();
        } else {
            music.play(SoundId(sounds::SFX_PIANO_LOOP));
        }
    }
}

/// Reflect the music state on its HUD line.
fn update_audio_hud(
    audio: Res<Audio>,
    music: Res<Music>,
    mut query: Query<&mut DsText, With<AudioHud>>,
) {
    let state = if !audio.ready {
        "unavailable"
    } else if music.is_playing() {
        "playing"
    } else {
        "muted"
    };
    for mut text in &mut query {
        text.0.clear();
        let _ = write!(text.0, "music: {state}");
    }
}

/// Show the latest touch gesture on its HUD line. Reads the `GestureEvent`
/// stream that `bevy_nds` derives from the touch input — tap, long-press,
/// 4-direction swipe and drag, with no per-game bookkeeping.
fn update_gesture_hud(
    mut events: EventReader<GestureEvent>,
    mut query: Query<&mut DsText, With<GestureHud>>,
) {
    let Some(GestureEvent(gesture)) = events.read().last() else {
        return;
    };
    let label = match gesture {
        Gesture::Tap(_) => "tap",
        Gesture::LongPress(_) => "long press",
        Gesture::Swipe { direction, .. } => match direction {
            SwipeDir::Up => "swipe up",
            SwipeDir::Down => "swipe down",
            SwipeDir::Left => "swipe left",
            SwipeDir::Right => "swipe right",
        },
        Gesture::DragStart(_) => "drag start",
        Gesture::Drag { .. } => "drag",
        Gesture::DragEnd(_) => "drag end",
    };
    for mut text in &mut query {
        text.0.clear();
        let _ = write!(text.0, "gesture: {label}");
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
    let model_on = match display.screen {
        DsScreen::Top => "top",
        DsScreen::Bottom => "bottom",
    };
    for mut text in &mut query {
        // Reuse the existing String's capacity instead of allocating anew.
        text.0.clear();
        let _ = write!(text.0, "t={secs:>4}s fps={fps:>2.0} pot={model_on}");
    }
}
