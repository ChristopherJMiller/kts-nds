//! **Spike C — the fuse: dual-screen capture mode** (Milestone 1, issue #20).
//!
//! A throwaway feel-spike, not production code. It fuses Spike A (stylus
//! locomotion) and Spike B (loop-draw capture) into the real dual-screen loop
//! and answers **OQ-2**: is two-screen split attention fun or overwhelming
//! (the TWEWY question), and is cluster-dodge-while-drawing mobile enough to not
//! be a sitting duck?
//!
//! **Phase 1** (this build) is the MVP fuse — deploy, dodge-while-draw, capture
//! — with **simplified stand-ins** for the locked-but-unbuilt systems: deploy is
//! a shoulder *toggle* (no radial-wheel UI — that's epic #25), dodge is d-pad
//! steps + a roll. Phase 2 layers the crutches (tap-retract, hit-knocks-device-
//! offline).
//!
//! Layout:
//! - **Top LCD (3D):** the arena. Avatar (teapot) + one circle-vulnerable enemy
//!   (cube) on a waypoint patrol + landmark cubes you collide with. Position-only
//!   follow camera. When deployed, a bright **cursor** shows the stylus position
//!   in world space — the pen, made visible in 3D.
//! - **Bottom LCD:** *stowed* → the stylus is a virtual-stick (Spike A) moving
//!   the avatar. *Deployed* (tap **L/R** to toggle) → a top-down **tactical
//!   map**: avatar (blue) + enemy (red) plotted at their world positions; the
//!   stylus draws a loop (Spike B) to enclose the enemy while the **d-pad
//!   dodges** (B = roll).
//!
//! Capture: each loop that encloses the enemy on the map adds progress; two
//! captures it (it vanishes). Enemy contact while deployed costs progress (the
//! pressure) — unless you're mid-roll (i-frames). **START** re-arms the enemy.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_3d::prelude::*;
use bevy_nds_loop::{densify, enclosed, find_closed_loop_within, smooth as path_smooth};
use bevy_nds_math::stick::{StickConfig, smooth as vel_smooth, stick_vector};
use bevy_nds_math::{Fx32, FxVec2};
use bevy_nds_sprite::prelude::*;

mod sprites {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/sprites.rs"));
}

// --- Arena / camera ----------------------------------------------------------

const ARENA_HALF: f32 = 2.0;
/// Fixed camera height above the z=0 plane. Chosen so the whole ±ARENA_HALF
/// arena fits on screen, so the 3D top and the tactical-map bottom share **one
/// world frame** (camera at the world origin, no follow) — circling the enemy on
/// the map then lines up with circling it on top. (A perspective camera over a
/// flat z=0 plane is already effectively orthographic: uniform depth → linear.)
const CAM_Z: f32 = 3.2;
const TILT_X: f32 = -1.0;
const AVATAR_SCALE: f32 = 0.11;
const ENEMY_SCALE: f32 = 0.16;
const LANDMARK_SCALE: f32 = 0.16;
const CURSOR_SCALE: f32 = 0.12;

/// Static landmark cubes (world XY) — spatial reference *and* obstacles. Kept to
/// three so the scene stays at 6 meshes (avatar + enemy + cursor + 3) for 60 fps
/// (per-object render cost, #34).
const LANDMARKS: [(f32, f32); 2] = [(-1.25, 0.95), (1.25, -0.95)];
/// Avatar↔landmark separation enforced by collision (radii summed).
const LANDMARK_COLLIDE: f32 = 0.26;

// --- Stowed locomotion (Spike A defaults, locked 2026-06-14) -----------------

const STOW_DEADZONE: f32 = 8.0;
const STOW_MAX_RADIUS: f32 = 70.0;
const STOW_SPEED: f32 = 1.6;
const STOW_SMOOTH: f32 = 0.5;

// --- Deployed dodge (one-handed: all on the d-pad, stylus is the other hand) --

/// Deployed steady movement is a quarter of stowed speed — the pen is out, so
/// you're slow; the roll is the only fast move (the evasive burst).
const DODGE_SPEED: f32 = STOW_SPEED * 0.25;
const ROLL_SPEED: f32 = 3.8; // dash speed; × ROLL_FRAMES sets the dodge distance
const ROLL_FRAMES: u8 = 10; // duration + i-frame window
/// Double-tap window (frames) for a d-pad direction to trigger a roll.
const DOUBLE_TAP_WINDOW: u8 = 12;

// --- Enemy + projectile ------------------------------------------------------

const ENEMY_SPEED: f32 = 0.8;
const ENEMY_WAYPOINTS: [(f32, f32); 4] = [(-1.4, 0.0), (0.0, 1.4), (1.4, 0.0), (0.0, -1.4)];
/// The enemy pauses at each waypoint (stop-and-go), opening capture windows.
const ENEMY_PAUSE: u8 = 45; // frames (~0.75 s)
const CONTACT_DIST: f32 = 0.28;
const CONTACT_LOSS: f32 = 0.34; // progress lost per body contact
const CONTACT_COOLDOWN: u8 = 30; // frames between body hits
/// The enemy lobs a projectile at the avatar while deployed — the real dodge
/// threat. Roll (i-frames) or move out of the way.
const PROJ_SPEED: f32 = 1.7;
const PROJ_SCALE: f32 = 0.07;
const FIRE_INTERVAL: u8 = 80; // frames between shots
const PROJ_HIT_DIST: f32 = 0.18;
const PROJ_LOSS: f32 = 0.34; // progress lost per projectile hit
const PROJ_DESPAWN: f32 = ARENA_HALF + 0.4; // out-of-bounds cutoff

// --- Capture -----------------------------------------------------------------

const CAPTURE_PER_LOOP: f32 = 0.5; // 2 enclosing loops -> captured

// --- Tactical map ------------------------------------------------------------

// The tactical map mirrors the top camera's view so the two screens correlate:
// scale = (screen_half_height) / (camera visible half-height at z=0)
//       = 96 / (CAM_Z * tan(fov/2)) = 96 / (3.2 * tan(35°)) ≈ 42.8 px/world-unit.
// Same aspect (256/192) as the camera, so one uniform scale fits both axes.
const MAP_SCALE: f32 = 42.8;
const MAP_CX: f32 = 128.0;
const MAP_CY: f32 = 96.0;
const PARK_Y: i16 = 200; // off-screen park for hidden sprites

// --- Loop draw (Spike B) -----------------------------------------------------

const MIN_SPACING: f32 = 4.0;
const MAX_POINTS: usize = 80;
const DOT_POOL: usize = 90;
const TRAIL_STEP: f32 = 4.0;
const CLOSE_TOL: f32 = 2.0;

/// World XY → tactical-map screen pixels (y flipped: world +y is up).
fn world_to_map(p: FxVec2) -> (i16, i16) {
    let x = MAP_CX + p.x.to_f32() * MAP_SCALE;
    let y = MAP_CY - p.y.to_f32() * MAP_SCALE;
    (x as i16, y as i16)
}

/// Tactical-map screen pixels → world XY (inverse of [`world_to_map`]).
fn map_to_world(sx: f32, sy: f32) -> Vec3 {
    Vec3::new((sx - MAP_CX) / MAP_SCALE, (MAP_CY - sy) / MAP_SCALE, 0.0)
}

// --- Resources ---------------------------------------------------------------

/// The capture device: deployed while toggled; accrues capture progress; brief
/// cooldown after a hit so contact doesn't drain every frame.
#[derive(Resource, Default)]
struct Device {
    deployed: bool,
    progress: f32,
    hit_cd: u8,
}

#[derive(Resource, Default)]
struct StickState {
    origin: FxVec2,
    vel: FxVec2,
    active: bool,
}

#[derive(Resource, Default)]
struct Dodge {
    roll: u8,
    roll_dir: FxVec2,
    /// Per-direction double-tap countdown [Left, Right, Up, Down].
    tap: [u8; 4],
}

/// Enemy fire cadence (frames until the next shot).
#[derive(Resource, Default)]
struct EnemyFire {
    cd: u8,
}

#[derive(Resource, Default)]
struct Stroke(Vec<FxVec2>);

// --- Components ---------------------------------------------------------------

#[derive(Component)]
struct Avatar;

/// The enemy: patrol waypoint index + whether it's been captured (hidden, inert,
/// until START re-arms it). Captured state lives here, not on the device, so the
/// player can still stow / move / re-deploy after a capture.
#[derive(Component)]
struct Enemy {
    wp: usize,
    captured: bool,
    pause: u8,
}

/// The 3D stylus cursor (shows the pen position in world space while deployed).
#[derive(Component)]
struct Cursor;

/// An enemy projectile — the dodge threat. Inactive ones are pooled (Hidden,
/// inert) and reused.
#[derive(Component)]
struct Projectile {
    active: bool,
    vel: FxVec2,
}

/// World XY (fixed-point) — source of truth for the 3D transform and map marker.
#[derive(Component, Clone, Copy)]
struct WorldPos(FxVec2);

#[derive(Component)]
struct PathDot;

#[derive(Component)]
struct InfoHud;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins)
        .add_plugins(Ds3dPlugin)
        .add_plugins(SpritePlugin)
        .add_plugins(SpikePlugin);
    bevy_nds::run(app)
}

struct SpikePlugin;

impl Plugin for SpikePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Display3d {
            screen: DsScreen::Top,
        })
        .init_resource::<Device>()
        .init_resource::<StickState>()
        .init_resource::<Dodge>()
        .init_resource::<EnemyFire>()
        .init_resource::<Stroke>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                toggle_device,
                reset_enemy,
                move_avatar,
                patrol_enemy,
                fire_projectile,
                move_projectile,
                draw_capture,
                enemy_contact,
                sync_3d,
                update_cursor,
                sync_map_markers,
                update_trail,
                update_hud,
            )
                .chain(),
        );
    }
}

// --- Setup -------------------------------------------------------------------

fn setup(mut commands: Commands, mut camera: ResMut<Camera3d>) {
    let cube = include_obj!("assets/cube.obj", center);
    let teapot = include_obj!("assets/teapot.obj", center);

    // Avatar — one entity carrying its 3D mesh (top) and map marker (bottom).
    commands.spawn((
        Avatar,
        WorldPos(FxVec2::ZERO),
        teapot,
        DsMaterial {
            diffuse: [110, 180, 235],
            ambient: [26, 40, 58],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(-1.3, 0.5, 0.0),
            scale: Vec3::splat(AVATAR_SCALE),
        },
        Sprite::new(sprites::PLAYER).at(0, PARK_Y),
    ));

    // Enemy — circle-vulnerable, patrols the waypoints.
    let estart = FxVec2::from_f32(ENEMY_WAYPOINTS[0].0, ENEMY_WAYPOINTS[0].1);
    commands.spawn((
        Enemy {
            wp: 1,
            captured: false,
            pause: 0,
        },
        WorldPos(estart),
        cube.clone(),
        DsMaterial {
            diffuse: [225, 80, 70],
            ambient: [56, 20, 18],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(TILT_X, 0.4, 0.0),
            scale: Vec3::splat(ENEMY_SCALE),
        },
        Sprite::new(sprites::BLIP).at(0, PARK_Y),
    ));

    // Stylus cursor — bright cube, starts Hidden (the mesh is skipped by the
    // renderer) until deployed + drawing.
    commands.spawn((
        Cursor,
        cube.clone(),
        DsMaterial {
            diffuse: [245, 240, 180],
            ambient: [70, 68, 40],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(TILT_X, 0.6, 0.0),
            scale: Vec3::splat(CURSOR_SCALE),
        },
        Hidden,
    ));

    // Enemy projectile (3D-only threat) — pooled, starts inactive + Hidden.
    commands.spawn((
        Projectile {
            active: false,
            vel: FxVec2::ZERO,
        },
        WorldPos(FxVec2::ZERO),
        cube.clone(),
        DsMaterial {
            diffuse: [255, 150, 40],
            ambient: [70, 42, 12],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(TILT_X, 0.8, 0.0),
            scale: Vec3::splat(PROJ_SCALE),
        },
        Hidden,
    ));

    // Landmark cubes — 3D obstacles. A WorldPos + obstacle Sprite makes them
    // show on the tactical map too, auto-positioned by `sync_map_markers` (the
    // same path the avatar/enemy use).
    for (i, &(x, y)) in LANDMARKS.iter().enumerate() {
        commands.spawn((
            WorldPos(FxVec2::from_f32(x, y)),
            cube.clone(),
            DsMaterial {
                diffuse: [120, 120, 138],
                ambient: [34, 34, 44],
            },
            Transform3d {
                translation: Vec3::new(x, y, 0.0),
                rotation: Vec3::new(TILT_X, 0.3 * i as f32, 0.0),
                scale: Vec3::splat(LANDMARK_SCALE),
            },
            Sprite::new(sprites::OBSTACLE).at(0, PARK_Y),
        ));
    }

    // Trail-dot pool (map), parked.
    for _ in 0..DOT_POOL {
        commands.spawn((PathDot, Sprite::new(sprites::DOT).at(0, PARK_Y)));
    }

    camera.position = Vec3::new(0.0, 0.0, CAM_Z);

    let b = DsScreen::Bottom;
    commands.spawn((b, TilePos::new(1, 0), InfoHud, DsText::new("")));
    commands.spawn((b, TilePos::new(1, 22), DsText::new("L/R deploy  dpad move")));
    commands.spawn((b, TilePos::new(1, 23), DsText::new("dbl-tap=roll  START reset")));
}

// --- Device + movement -------------------------------------------------------

/// Tap a shoulder (L or R) to toggle the capture device deployed/stowed — a
/// stand-in for the eventual hold→radial-wheel→equip flow (#25). State change
/// drops the in-flight stroke.
fn toggle_device(
    input: Res<ButtonInput<DsButton>>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
) {
    if input.just_pressed(DsButton::R) || input.just_pressed(DsButton::L) {
        device.deployed = !device.deployed;
        stroke.0.clear();
    }
}

/// START re-arms the enemy (un-capture, back to its patrol start) and resets
/// capture progress, so the loop is replayable.
fn reset_enemy(
    input: Res<ButtonInput<DsButton>>,
    mut device: ResMut<Device>,
    mut q: Query<(&mut Enemy, &mut WorldPos)>,
) {
    if !input.just_pressed(DsButton::Start) {
        return;
    }
    for (mut enemy, mut pos) in &mut q {
        enemy.captured = false;
        enemy.wp = 1;
        enemy.pause = 0;
        pos.0 = FxVec2::from_f32(ENEMY_WAYPOINTS[0].0, ENEMY_WAYPOINTS[0].1);
    }
    device.progress = 0.0;
}

/// Move the avatar: stowed → stylus virtual-stick (Spike A); deployed → d-pad
/// move + double-tap roll. Integrate the fixed-point world position, clamp to the arena,
/// and push out of landmark obstacles.
fn move_avatar(
    time: Res<Time>,
    touches: Res<Touches>,
    input: Res<ButtonInput<DsButton>>,
    device: Res<Device>,
    mut stick: ResMut<StickState>,
    mut dodge: ResMut<Dodge>,
    mut q: Query<&mut WorldPos, With<Avatar>>,
) {
    let dt = Fx32::from_f32(time.delta_secs());
    let bound = Fx32::from_f32(ARENA_HALF);
    let Some(mut pos) = q.iter_mut().next() else {
        return;
    };

    let delta = if device.deployed {
        deployed_dodge(&input, &mut dodge, dt)
    } else {
        stowed_locomotion(&touches, &mut stick, dt)
    };

    let mut np = pos.0 + delta;
    np.x = np.x.clamp(-bound, bound);
    np.y = np.y.clamp(-bound, bound);

    // Collide with the landmark obstacles (circular push-out).
    let min = Fx32::from_f32(LANDMARK_COLLIDE);
    for &(lx, ly) in &LANDMARKS {
        let c = FxVec2::from_f32(lx, ly);
        let sep = np - c;
        let d = sep.length();
        if d > Fx32::ZERO && d < min {
            np = c + sep.normalize_or_zero() * min;
        }
    }
    pos.0 = np;
}

fn stowed_locomotion(touches: &Touches, stick: &mut StickState, dt: Fx32) -> FxVec2 {
    let cfg = StickConfig {
        deadzone: Fx32::from_f32(STOW_DEADZONE),
        max_radius: Fx32::from_f32(STOW_MAX_RADIUS),
        smoothing: Fx32::from_f32(STOW_SMOOTH),
    };
    let target = if let Some(touch) = touches.iter().next() {
        let p = touch.position();
        let cur = FxVec2::from_f32(p.x, p.y);
        if !stick.active {
            stick.origin = cur;
            stick.active = true;
        }
        let raw = cur - stick.origin;
        stick_vector(FxVec2::new(raw.x, -raw.y), &cfg)
    } else {
        stick.active = false;
        FxVec2::ZERO
    };
    stick.vel = vel_smooth(stick.vel, target, cfg.smoothing);
    stick.vel * (Fx32::from_f32(STOW_SPEED) * dt)
}

fn deployed_dodge(input: &ButtonInput<DsButton>, dodge: &mut Dodge, dt: Fx32) -> FxVec2 {
    // Tick double-tap windows.
    for t in &mut dodge.tap {
        *t = t.saturating_sub(1);
    }

    // A roll already in progress: keep dashing along its locked direction.
    if dodge.roll > 0 {
        dodge.roll -= 1;
        return dodge.roll_dir * (Fx32::from_f32(ROLL_SPEED) * dt);
    }

    // [Left, Right, Up, Down] → unit world directions.
    let dirs = [
        (DsButton::Left, FxVec2::new(Fx32::NEG_ONE, Fx32::ZERO)),
        (DsButton::Right, FxVec2::new(Fx32::ONE, Fx32::ZERO)),
        (DsButton::Up, FxVec2::new(Fx32::ZERO, Fx32::ONE)),
        (DsButton::Down, FxVec2::new(Fx32::ZERO, Fx32::NEG_ONE)),
    ];

    // Double-tap a direction → roll that way (one-handed: no face button).
    for (i, (btn, vec)) in dirs.iter().enumerate() {
        if input.just_pressed(*btn) {
            if dodge.tap[i] > 0 {
                dodge.roll = ROLL_FRAMES;
                dodge.roll_dir = *vec;
                dodge.tap[i] = 0;
                return *vec * (Fx32::from_f32(ROLL_SPEED) * dt);
            }
            dodge.tap[i] = DOUBLE_TAP_WINDOW;
        }
    }

    // Steady (held) movement at quarter speed.
    let mut dir = FxVec2::ZERO;
    for (btn, vec) in &dirs {
        if input.pressed(*btn) {
            dir = dir + *vec;
        }
    }
    dir.normalize_or_zero() * (Fx32::from_f32(DODGE_SPEED) * dt)
}

/// The enemy lobs a projectile at the avatar while deployed (the dodge threat),
/// reusing the pooled [`Projectile`] when it's free.
fn fire_projectile(
    device: Res<Device>,
    mut fire: ResMut<EnemyFire>,
    avatar: Query<&WorldPos, With<Avatar>>,
    enemy: Query<(&WorldPos, &Enemy)>,
    mut proj: Query<(&mut Projectile, &mut WorldPos), (Without<Avatar>, Without<Enemy>)>,
) {
    fire.cd = fire.cd.saturating_sub(1);
    let (Some(a), Some((e, en))) = (avatar.iter().next(), enemy.iter().next()) else {
        return;
    };
    if !device.deployed || en.captured || fire.cd > 0 {
        return;
    }
    let Some((mut p, mut ppos)) = proj.iter_mut().next() else {
        return;
    };
    if p.active {
        return; // one shot in flight at a time
    }
    let dir = (a.0 - e.0).normalize_or_zero();
    if dir == FxVec2::ZERO {
        return;
    }
    p.active = true;
    p.vel = dir * Fx32::from_f32(PROJ_SPEED);
    ppos.0 = e.0;
    fire.cd = FIRE_INTERVAL;
}

/// Fly active projectiles; a hit costs progress (unless mid-roll i-frames);
/// out-of-bounds despawns. Inactive ones are Hidden (free).
fn move_projectile(
    time: Res<Time>,
    dodge: Res<Dodge>,
    mut device: ResMut<Device>,
    mut commands: Commands,
    avatar: Query<&WorldPos, With<Avatar>>,
    mut proj: Query<(Entity, &mut Projectile, &mut WorldPos, Has<Hidden>), Without<Avatar>>,
) {
    let dt = Fx32::from_f32(time.delta_secs());
    let bound = Fx32::from_f32(PROJ_DESPAWN);
    let a = avatar.iter().next().map(|w| w.0);
    for (e, mut p, mut pos, hidden) in &mut proj {
        if !p.active {
            if !hidden {
                commands.entity(e).insert(Hidden);
            }
            continue;
        }
        if hidden {
            commands.entity(e).remove::<Hidden>();
        }
        pos.0 = pos.0 + p.vel * dt;
        if pos.0.x.abs() > bound || pos.0.y.abs() > bound {
            p.active = false;
            continue;
        }
        if let Some(a) = a
            && dodge.roll == 0
            && (pos.0 - a).length() < Fx32::from_f32(PROJ_HIT_DIST)
        {
            device.progress = (device.progress - PROJ_LOSS).max(0.0);
            p.active = false;
        }
    }
}

/// Enemy walks its waypoint loop (frozen once captured).
fn patrol_enemy(time: Res<Time>, mut q: Query<(&mut Enemy, &mut WorldPos)>) {
    let step = Fx32::from_f32(ENEMY_SPEED * time.delta_secs());
    for (mut enemy, mut pos) in &mut q {
        if enemy.captured {
            continue;
        }
        if enemy.pause > 0 {
            enemy.pause -= 1; // dwelling at a waypoint — a capture window
            continue;
        }
        let (tx, ty) = ENEMY_WAYPOINTS[enemy.wp];
        let target = FxVec2::from_f32(tx, ty);
        let to = target - pos.0;
        if to.length() <= step {
            pos.0 = target;
            enemy.wp = (enemy.wp + 1) % ENEMY_WAYPOINTS.len();
            enemy.pause = ENEMY_PAUSE;
        } else {
            pos.0 = pos.0 + to.normalize_or_zero() * step;
        }
    }
}

// --- Capture -----------------------------------------------------------------

/// While deployed, capture the stylus path and, on closure, test whether it
/// encloses the (live) enemy's map position; each enclosing loop accrues
/// progress, and two captures it.
fn draw_capture(
    touches: Res<Touches>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
    mut enemy: Query<(&WorldPos, &mut Enemy)>,
) {
    let Some((epos, mut enemy)) = enemy.iter_mut().next() else {
        return;
    };
    if !device.deployed || enemy.captured {
        stroke.0.clear();
        return;
    }
    let Some(touch) = touches.iter().next() else {
        stroke.0.clear();
        return;
    };

    let p = touch.position();
    let cur = FxVec2::from_f32(p.x, p.y);
    let push = stroke
        .0
        .last()
        .is_none_or(|&last| (cur - last).length() >= Fx32::from_f32(MIN_SPACING));
    if push {
        stroke.0.push(cur);
        if stroke.0.len() > MAX_POINTS {
            stroke.0.remove(0);
        }
    }
    if stroke.0.len() < 4 {
        return;
    }

    let path = path_smooth(&stroke.0);
    let Some(poly) = find_closed_loop_within(&path, Fx32::from_f32(CLOSE_TOL)) else {
        return;
    };

    let (ex, ey) = world_to_map(epos.0);
    let enemy_px = [FxVec2::from_f32(ex as f32, ey as f32)];
    if !enclosed(&poly, &enemy_px).is_empty() {
        device.progress += CAPTURE_PER_LOOP;
        if device.progress >= 1.0 {
            // Stay deployed (don't silently stow) so the pen/cursor keeps
            // working; the enemy just vanishes. START re-arms.
            enemy.captured = true;
        }
    }
    stroke.0.clear();
}

/// Enemy contact while deployed costs capture progress — unless you're mid-roll
/// (i-frames). Phase 1's pressure (no forced retract yet).
fn enemy_contact(
    dodge: Res<Dodge>,
    mut device: ResMut<Device>,
    avatar: Query<&WorldPos, With<Avatar>>,
    enemy: Query<(&WorldPos, &Enemy)>,
) {
    if device.hit_cd > 0 {
        device.hit_cd -= 1;
    }
    let (Some(a), Some((e, enemy))) = (avatar.iter().next(), enemy.iter().next()) else {
        return;
    };
    if !device.deployed || enemy.captured || dodge.roll > 0 || device.hit_cd > 0 {
        return;
    }
    if (a.0 - e.0).length() < Fx32::from_f32(CONTACT_DIST) {
        device.progress = (device.progress - CONTACT_LOSS).max(0.0);
        device.hit_cd = CONTACT_COOLDOWN;
    }
}

// --- Rendering ---------------------------------------------------------------

/// WorldPos → 3D transform; toggle the captured enemy's mesh off via [`Hidden`].
fn sync_3d(
    mut commands: Commands,
    mut q: Query<(Entity, &WorldPos, &mut Transform3d, Option<&Enemy>, Has<Hidden>)>,
) {
    for (e, pos, mut t, enemy, hidden) in &mut q {
        t.translation.x = pos.0.x.to_f32();
        t.translation.y = pos.0.y.to_f32();
        if let Some(en) = enemy {
            match (en.captured, hidden) {
                (true, false) => {
                    commands.entity(e).insert(Hidden);
                }
                (false, true) => {
                    commands.entity(e).remove::<Hidden>();
                }
                _ => {}
            }
        }
    }
}

/// Place the 3D stylus cursor at the pen's world position while deployed +
/// drawing; otherwise hide its mesh via [`Hidden`].
fn update_cursor(
    device: Res<Device>,
    touches: Res<Touches>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform3d, Has<Hidden>), With<Cursor>>,
) {
    let Some((e, mut t, hidden)) = q.iter_mut().next() else {
        return;
    };
    let show = device.deployed && touches.iter().next().is_some();
    if show {
        let p = touches.iter().next().unwrap().position();
        t.translation = map_to_world(p.x, p.y);
        if hidden {
            commands.entity(e).remove::<Hidden>();
        }
    } else if !hidden {
        commands.entity(e).insert(Hidden);
    }
}

/// Map markers: shown at world→map while deployed (captured enemy parked),
/// parked while stowed.
fn sync_map_markers(device: Res<Device>, mut q: Query<(&WorldPos, &mut Sprite, Option<&Enemy>)>) {
    for (pos, mut sprite, enemy) in &mut q {
        let hide = !device.deployed || enemy.is_some_and(|e| e.captured);
        if hide {
            sprite.y = PARK_Y;
        } else {
            let (x, y) = world_to_map(pos.0);
            sprite.x = x - 8; // 16×16 markers
            sprite.y = y - 8;
        }
    }
}

/// Trail dots along the densified stroke (deployed + drawing); parked otherwise.
fn update_trail(
    device: Res<Device>,
    stroke: Res<Stroke>,
    mut dots: Query<&mut Sprite, With<PathDot>>,
) {
    let line = if device.deployed {
        densify(&path_smooth(&stroke.0), Fx32::from_f32(TRAIL_STEP), DOT_POOL)
    } else {
        Vec::new()
    };
    for (i, mut sprite) in dots.iter_mut().enumerate() {
        if let Some(p) = line.get(i) {
            sprite.x = p.x.to_f32() as i16 - 4;
            sprite.y = p.y.to_f32() as i16 - 4;
        } else {
            sprite.y = PARK_Y;
        }
    }
}

fn update_hud(
    device: Res<Device>,
    enemy: Query<&Enemy>,
    mut hud: Query<&mut DsText, With<InfoHud>>,
) {
    let captured = enemy.iter().next().is_some_and(|e| e.captured);
    for mut text in &mut hud {
        text.0.clear();
        if captured {
            let _ = write!(text.0, "CAPTURED!  START to re-arm");
        } else {
            let pct = (device.progress * 100.0) as i32;
            let state = if device.deployed { "DEPLOYED" } else { "stowed " };
            let _ = write!(text.0, "{state}  capture {pct:>3}%");
        }
    }
}
