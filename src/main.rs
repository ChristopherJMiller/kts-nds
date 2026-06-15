//! **Spike A — stylus virtual-analog locomotion** (Milestone 1, issue #18).
//!
//! A throwaway feel-spike, not production code. It exists to answer the single
//! highest-risk unknown in the design (OQ-1): *does a relative stylus
//! virtual-stick feel like an analog stick, not mud* — in both open and tight
//! spaces? Everything else in the control model is built on the assumption that
//! it does, so we prove it before any systems work.
//!
//! Layout (locked in #17): the **top** LCD shows the 3D world (a flat arena with
//! a hero teapot threading a grid of low-poly pillars), the **bottom** LCD is
//! the touch surface — a *relative* virtual stick: pen-down sets an origin, the
//! drag offset becomes a continuous heading + magnitude. Because the avatar and
//! the touch panel live on different screens there is no shared coordinate
//! space, so the stick must be relative (no absolute point-to-move).
//!
//! The feel-critical conditioning (radial deadzone, magnitude ramp, low-pass
//! smoothing) is the pure, host-tested [`bevy_nds_math::stick`] module; this
//! file is the ROM-side arena that wires it to hardware and exposes every knob
//! to live tuning so the feel pass can dial it in. The camera is a position-only
//! soft-follow (the DS 3D view matrix is translation-only — it can pan but not
//! rotate), matching the "authored camera, no player camera control" decision.
//!
//! Controls (right-handed defaults; handedness mirroring is a later epic):
//! - **Pen on bottom screen** — move (drag from the touch-down origin).
//! - **L / R** — deadzone − / +      **Left / Right** — max-radius − / +
//! - **Up / Down** — max-speed + / −  **Y / B** — smoothing + / −
//! - **X** — recentre the hero        **Start** — reset all tunables

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_3d::prelude::*;
use bevy_nds_math::stick::{StickConfig, smooth, stick_vector};
use bevy_nds_math::{Fx32, FxVec2};

// --- Tunable feel constants (starting points for the feel pass) --------------

/// Half-extent of the square play-field, in world units. The hero is clamped
/// here; the pillar grid sits inside it, leaving an open margin ring outside.
const ARENA_HALF: f32 = 2.0;

/// Landmark/obstacle pillars, in world XY. Kept deliberately few: each drawn
/// mesh costs ~2 ms of per-frame CPU matrix compose on the 33 MHz ARM9
/// (see #34), so the scene is object-count-bound, not polygon-bound, at 60 fps.
/// The top pair forms a narrow **gate** to thread (the "tight space" feel test);
/// the lower pair are spread landmarks giving motion parallax across the **open**
/// field. The hero starts at the origin and can roam the open margin to
/// ±`ARENA_HALF`.
const PILLAR_CELLS: [(f32, f32); 4] = [
    (-0.25, 0.7),
    (0.25, 0.7),
    (-1.2, -0.85),
    (1.2, -0.95),
];

/// Camera height above the z=0 play-plane (it looks straight down −Z).
const CAM_Z: f32 = 3.6;
/// Per-frame camera follow lerp (0 = locked, 1 = instant snap).
const CAM_FOLLOW: f32 = 0.15;

/// Hero collision radius (world units).
const PLAYER_RADIUS: f32 = 0.12;
/// Pillar collision radius (world units).
const PILLAR_RADIUS: f32 = 0.13;

/// How far ahead of the hero the heading-nose marker sits.
const NOSE_DIST: f32 = 0.26;
/// Below this smoothed speed the hero is "stopped" and the nose hides.
const MOVING_EPS: f32 = 0.03;

/// Common forward tilt applied to every object so the straight-down camera
/// reads as an angled 3/4 view (the view matrix can't tilt, so the meshes do).
const TILT_X: f32 = -1.0;
const PLAYER_SCALE: f32 = 0.11;
const PILLAR_SCALE: f32 = 0.18;
const NOSE_SCALE: f32 = 0.06;

// --- Resources ----------------------------------------------------------------

/// Live feel knobs, all adjustable on-device so the feel pass can tune without
/// a rebuild. `deadzone` / `max_radius` are in touch pixels; `max_speed` is in
/// world units/second; `smoothing` is the low-pass factor in `[0, 1)`.
#[derive(Resource, Clone, Copy)]
struct StickTuning {
    deadzone: Fx32,
    max_radius: Fx32,
    max_speed: Fx32,
    smoothing: Fx32,
}

impl Default for StickTuning {
    fn default() -> Self {
        // Values from the Spike A feel pass (2026-06-14): a slightly raised
        // deadzone, generous max-radius and moderate smoothing read as a
        // responsive analog stick (OQ-1).
        Self {
            deadzone: Fx32::from_f32(8.0),
            max_radius: Fx32::from_f32(70.0),
            max_speed: Fx32::from_f32(1.6),
            smoothing: Fx32::from_f32(0.5),
        }
    }
}

impl StickTuning {
    fn config(&self) -> StickConfig {
        StickConfig {
            deadzone: self.deadzone,
            max_radius: self.max_radius,
            smoothing: self.smoothing,
        }
    }
}

/// Mutable per-frame stick state: where the current drag started, the smoothed
/// movement vector (unit direction × magnitude in `[0, 1]`), and whether the
/// pen is currently down (so a fresh touch re-seats the origin).
#[derive(Resource, Default)]
struct StickState {
    origin: FxVec2,
    vel: FxVec2,
    active: bool,
}

/// Static pillar centres (world XY), built once at setup and read by the
/// collision pass.
#[derive(Resource, Default)]
struct Pillars(Vec<FxVec2>);

// --- Components ---------------------------------------------------------------

/// The pen-driven hero.
#[derive(Component)]
struct Player;

/// A small marker that floats ahead of the hero in its heading direction —
/// visual proof that the stick yields a *continuous* heading, not 8-way snaps.
#[derive(Component)]
struct Nose;

/// The hero's authoritative world position, in fixed-point. Movement and
/// collision run here (hardware sqrt/divide on the hot path); `sync_player`
/// copies it into the float [`Transform3d`] the renderer wants.
#[derive(Component, Clone, Copy)]
struct WorldPos(FxVec2);

/// HUD line: live stick readout (heading magnitude + components).
#[derive(Component)]
struct StickHud;
/// HUD line: the current tunable values.
#[derive(Component)]
struct TuneHud;
/// HUD line: fps + hero position.
#[derive(Component)]
struct StatHud;

/// Program entry point, called by the BlocksDS crt0.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins)
        .add_plugins(Ds3dPlugin)
        .add_plugins(SpikePlugin);
    bevy_nds::run(app)
}

/// The whole spike, as one Bevy plugin.
struct SpikePlugin;

impl Plugin for SpikePlugin {
    fn build(&self, app: &mut App) {
        // 3D on the top LCD; the sub-engine text console + touch land on the
        // bottom LCD (the two engines swap together — see bevy_nds_3d::Display3d).
        app.insert_resource(Display3d {
            screen: DsScreen::Top,
        })
        .init_resource::<StickTuning>()
        .init_resource::<StickState>()
        .init_resource::<Pillars>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            // Chained so the camera + transforms read the freshly-moved hero,
            // and the HUD reflects this frame's state. The DS is single-core, so
            // sequential ordering costs nothing here.
            (
                adjust_tuning,
                drive_player,
                sync_player,
                sync_nose,
                follow_camera,
                update_hud,
            )
                .chain(),
        );
    }
}

// --- Setup -------------------------------------------------------------------

fn setup(mut commands: Commands, mut camera: ResMut<Camera3d>, mut pillars: ResMut<Pillars>) {
    // Low-poly cube (12 tris) for the pillars — cheap enough to instance a whole
    // grid under the DS ~2048-poly/frame budget, unlike the 552-face teapot.
    let cube = include_obj!("assets/cube.obj", center);
    // The hero is the teapot, so it reads as a distinct "character".
    let teapot = include_obj!("assets/teapot.obj", center);

    // Hero — starts at the open centre cell.
    commands.spawn((
        Player,
        WorldPos(FxVec2::ZERO),
        teapot,
        DsMaterial {
            diffuse: [110, 180, 235],
            ambient: [26, 40, 58],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(-1.3, 0.5, 0.0),
            scale: Vec3::splat(PLAYER_SCALE),
        },
    ));

    // Heading nose — bright, hidden (scale 0) until the hero moves.
    commands.spawn((
        Nose,
        cube.clone(),
        DsMaterial {
            diffuse: [245, 220, 70],
            ambient: [60, 52, 16],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(TILT_X, 0.0, 0.0),
            scale: Vec3::ZERO,
        },
    ));

    // Pillars: a few hand-placed landmarks/obstacles (see PILLAR_CELLS).
    let mut centres = Vec::new();
    for (i, &(x, y)) in PILLAR_CELLS.iter().enumerate() {
        centres.push(FxVec2::from_f32(x, y));
        commands.spawn((
            cube.clone(),
            DsMaterial {
                diffuse: [120, 120, 138],
                ambient: [34, 34, 44],
            },
            Transform3d {
                translation: Vec3::new(x, y, 0.0),
                // A little yaw variety so the boxes don't look stamped.
                rotation: Vec3::new(TILT_X, 0.4 * i as f32, 0.0),
                scale: Vec3::splat(PILLAR_SCALE),
            },
        ));
    }
    pillars.0 = centres;

    // Camera starts centred over the hero.
    camera.position = Vec3::new(0.0, 0.0, CAM_Z);

    // Bottom-screen text HUD (sub engine). Top is the 3D view.
    let s = DsScreen::Bottom;
    commands.spawn((s, TilePos::new(1, 0), DsText::new("Spike A: stylus virtual-stick")));
    commands.spawn((s, TilePos::new(1, 1), DsText::new("drag below to move the hero")));
    commands.spawn((s, TilePos::new(1, 3), StickHud, DsText::new("")));
    commands.spawn((s, TilePos::new(1, 4), TuneHud, DsText::new("")));
    commands.spawn((s, TilePos::new(1, 5), StatHud, DsText::new("")));
    commands.spawn((s, TilePos::new(1, 18), DsText::new("L/R deadzone  <>/ max radius")));
    commands.spawn((s, TilePos::new(1, 19), DsText::new("Up/Dn speed   Y/B smoothing")));
    commands.spawn((s, TilePos::new(1, 20), DsText::new("X recentre    START reset knobs")));
}

// --- Movement ----------------------------------------------------------------

/// Read the relative virtual stick, smooth it, and integrate the hero's
/// fixed-point world position with arena-bound clamping and circular pillar
/// push-out. This is the heart of the spike.
fn drive_player(
    time: Res<Time>,
    touches: Res<Touches>,
    tuning: Res<StickTuning>,
    pillars: Res<Pillars>,
    mut state: ResMut<StickState>,
    mut query: Query<&mut WorldPos, With<Player>>,
) {
    let cfg = tuning.config();

    // Target movement vector from the pen, in world axes (screen-y points down,
    // world-y points up, so flip y). Releasing the pen targets zero, and the
    // low-pass below makes the hero glide to a stop rather than snap.
    let target = if let Some(touch) = touches.iter().next() {
        let p = touch.position();
        let cur = FxVec2::from_f32(p.x, p.y);
        if !state.active {
            state.origin = cur;
            state.active = true;
        }
        let raw = cur - state.origin;
        let offset = FxVec2::new(raw.x, -raw.y);
        stick_vector(offset, &cfg)
    } else {
        state.active = false;
        FxVec2::ZERO
    };

    state.vel = smooth(state.vel, target, cfg.smoothing);

    // Integrate: delta = dir·magnitude × speed × dt.
    let dt = Fx32::from_f32(time.delta_secs());
    let delta = state.vel * (tuning.max_speed * dt);

    let bound = Fx32::from_f32(ARENA_HALF);
    let min_dist = Fx32::from_f32(PLAYER_RADIUS + PILLAR_RADIUS);
    for mut pos in &mut query {
        let mut np = pos.0 + delta;
        np.x = np.x.clamp(-bound, bound);
        np.y = np.y.clamp(-bound, bound);

        // Push out of any pillar we've entered (sequential resolution is fine
        // for this sparse, non-overlapping grid).
        for &c in &pillars.0 {
            let sep = np - c;
            let d = sep.length();
            if d > Fx32::ZERO && d < min_dist {
                np = c + sep.normalize_or_zero() * min_dist;
            }
        }
        pos.0 = np;
    }
}

/// Copy the hero's fixed-point world position into its float transform.
fn sync_player(mut query: Query<(&WorldPos, &mut Transform3d), With<Player>>) {
    for (pos, mut transform) in &mut query {
        transform.translation.x = pos.0.x.to_f32();
        transform.translation.y = pos.0.y.to_f32();
    }
}

/// Float the heading nose ahead of the hero in its travel direction, or hide it
/// (scale 0) when stopped.
fn sync_nose(
    state: Res<StickState>,
    player: Query<&WorldPos, With<Player>>,
    mut nose: Query<&mut Transform3d, With<Nose>>,
) {
    let Some(pos) = player.iter().next() else {
        return;
    };
    let speed = state.vel.length();
    let moving = speed > Fx32::from_f32(MOVING_EPS);
    let dir = state.vel.normalize_or_zero();
    let nose_dist = Fx32::from_f32(NOSE_DIST);
    for mut transform in &mut nose {
        if moving {
            let p = pos.0 + dir * nose_dist;
            transform.translation = Vec3::new(p.x.to_f32(), p.y.to_f32(), 0.0);
            transform.scale = Vec3::splat(NOSE_SCALE);
        } else {
            transform.scale = Vec3::ZERO;
        }
    }
}

/// Position-only soft follow: the camera pans toward the hero but never rotates
/// (the DS view matrix is translation-only).
fn follow_camera(mut camera: ResMut<Camera3d>, player: Query<&WorldPos, With<Player>>) {
    let Some(pos) = player.iter().next() else {
        return;
    };
    let tx = pos.0.x.to_f32();
    let ty = pos.0.y.to_f32();
    camera.position.x += (tx - camera.position.x) * CAM_FOLLOW;
    camera.position.y += (ty - camera.position.y) * CAM_FOLLOW;
    camera.position.z = CAM_Z;
}

// --- Tuning ------------------------------------------------------------------

/// Adjust the feel knobs live from the button cluster, so the feel pass can dial
/// the stick in on real hardware without a rebuild.
fn adjust_tuning(input: Res<ButtonInput<DsButton>>, mut tuning: ResMut<StickTuning>) {
    let px = Fx32::from_int(1);
    let two_px = Fx32::from_int(2);
    let small = Fx32::from_f32(0.1);
    let fine = Fx32::from_f32(0.05);

    if input.just_pressed(DsButton::L) {
        tuning.deadzone = (tuning.deadzone - px).max(Fx32::ZERO);
    }
    if input.just_pressed(DsButton::R) {
        tuning.deadzone = (tuning.deadzone + px).min(Fx32::from_int(30));
    }
    if input.just_pressed(DsButton::Left) {
        let floor = tuning.deadzone + px;
        tuning.max_radius = (tuning.max_radius - two_px).max(floor);
    }
    if input.just_pressed(DsButton::Right) {
        tuning.max_radius = (tuning.max_radius + two_px).min(Fx32::from_int(100));
    }
    if input.just_pressed(DsButton::Up) {
        tuning.max_speed = (tuning.max_speed + small).min(Fx32::from_f32(5.0));
    }
    if input.just_pressed(DsButton::Down) {
        tuning.max_speed = (tuning.max_speed - small).max(Fx32::from_f32(0.2));
    }
    if input.just_pressed(DsButton::Y) {
        tuning.smoothing = (tuning.smoothing + fine).min(Fx32::from_f32(0.95));
    }
    if input.just_pressed(DsButton::B) {
        tuning.smoothing = (tuning.smoothing - fine).max(Fx32::ZERO);
    }
    if input.just_pressed(DsButton::Start) {
        *tuning = StickTuning::default();
    }
}

// --- HUD ---------------------------------------------------------------------

fn update_hud(
    fps: Res<Fps>,
    state: Res<StickState>,
    tuning: Res<StickTuning>,
    player: Query<&WorldPos, With<Player>>,
    mut stick_hud: Query<&mut DsText, (With<StickHud>, Without<TuneHud>, Without<StatHud>)>,
    mut tune_hud: Query<&mut DsText, (With<TuneHud>, Without<StatHud>)>,
    mut stat_hud: Query<&mut DsText, With<StatHud>>,
) {
    let mag = state.vel.length().to_f32();
    for mut text in &mut stick_hud {
        text.0.clear();
        let _ = write!(
            text.0,
            "mag={:.2} dir {:+.2},{:+.2}",
            mag,
            state.vel.x.to_f32(),
            state.vel.y.to_f32(),
        );
    }
    for mut text in &mut tune_hud {
        text.0.clear();
        let _ = write!(
            text.0,
            "dz{:.0} mR{:.0} spd{:.1} sm{:.2}",
            tuning.deadzone.to_f32(),
            tuning.max_radius.to_f32(),
            tuning.max_speed.to_f32(),
            tuning.smoothing.to_f32(),
        );
    }
    let (px, py) = match player.iter().next() {
        Some(p) => (p.0.x.to_f32(), p.0.y.to_f32()),
        None => (0.0, 0.0),
    };
    for mut text in &mut stat_hud {
        text.0.clear();
        let _ = write!(text.0, "fps={:>2.0} pos {:+.2},{:+.2}", fps.0, px, py);
    }
}
