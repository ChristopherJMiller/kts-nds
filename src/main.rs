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
use bevy_nds_math::{Fx32, FxVec2};
use bevy_nds_sprite::prelude::*;

mod control;
mod player;

use player::{Height, Locomotion, Motion, PlayerState, Shadow, StickState};

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

// Player locomotion tuning + the Stowed↔Deployed controller live in `player`.

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

/// The capture device: accrues capture progress while deployed; brief cooldown
/// after a hit so contact doesn't drain every frame. Deploy state itself lives
/// in [`PlayerState`] (the controller's state machine), not here.
#[derive(Resource, Default)]
struct Device {
    progress: f32,
    hit_cd: u8,
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
        .init_resource::<PlayerState>()
        .init_resource::<Locomotion>()
        .init_resource::<StickState>()
        .init_resource::<Motion>()
        .init_resource::<EnemyFire>()
        .init_resource::<Stroke>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                player::transition_state,
                player::toggle_tuning,
                reset_enemy,
                player::move_player,
                player::sync_shadow,
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
    // `Height` is the jump axis (gravity-integrated in `player`).
    commands.spawn((
        Avatar,
        WorldPos(FxVec2::ZERO),
        Height::default(),
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

    // Ground shadow — a flat, dark cube under the avatar (no `Height`), so a
    // jump's screen-Y lift reads as height. `sync_shadow` keeps it under the
    // avatar; it has no map sprite (ground-only cue).
    commands.spawn((
        Shadow,
        WorldPos(FxVec2::ZERO),
        cube.clone(),
        DsMaterial {
            diffuse: [18, 20, 26],
            ambient: [8, 8, 12],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(TILT_X, 0.0, 0.0),
            scale: Vec3::new(AVATAR_SCALE * 0.9, AVATAR_SCALE * 0.22, AVATAR_SCALE * 0.9),
        },
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
    commands.spawn((b, TilePos::new(1, 22), DsText::new("L deploy   stylus move/draw")));
    commands.spawn((b, TilePos::new(1, 23), DsText::new("dpad: jump/roll  START reset")));
}

// --- Enemy reset -------------------------------------------------------------

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

/// The enemy lobs a projectile at the avatar while deployed (the dodge threat),
/// reusing the pooled [`Projectile`] when it's free.
fn fire_projectile(
    state: Res<PlayerState>,
    mut fire: ResMut<EnemyFire>,
    avatar: Query<&WorldPos, With<Avatar>>,
    enemy: Query<(&WorldPos, &Enemy)>,
    mut proj: Query<(&mut Projectile, &mut WorldPos), (Without<Avatar>, Without<Enemy>)>,
) {
    fire.cd = fire.cd.saturating_sub(1);
    let (Some(a), Some((e, en))) = (avatar.iter().next(), enemy.iter().next()) else {
        return;
    };
    if !state.is_deployed() || en.captured || fire.cd > 0 {
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
    motion: Res<Motion>,
    mut state: ResMut<PlayerState>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
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
            && !motion.invulnerable()
            && (pos.0 - a).length() < Fx32::from_f32(PROJ_HIT_DIST)
        {
            device.progress = (device.progress - PROJ_LOSS).max(0.0);
            p.active = false;
            knock_device_offline(&mut state, &mut stroke);
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
    state: Res<PlayerState>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
    mut enemy: Query<(&WorldPos, &mut Enemy)>,
) {
    let Some((epos, mut enemy)) = enemy.iter_mut().next() else {
        return;
    };
    if !state.is_deployed() || enemy.captured {
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

/// Enemy contact while deployed **knocks the capture device offline** — a forced
/// retract (back to Stowed) plus progress loss — unless you're mid-roll
/// (i-frames). The core pressure of capture-while-dodging.
fn enemy_contact(
    motion: Res<Motion>,
    mut state: ResMut<PlayerState>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
    avatar: Query<&WorldPos, With<Avatar>>,
    enemy: Query<(&WorldPos, &Enemy)>,
) {
    if device.hit_cd > 0 {
        device.hit_cd -= 1;
    }
    let (Some(a), Some((e, enemy))) = (avatar.iter().next(), enemy.iter().next()) else {
        return;
    };
    if !state.is_deployed() || enemy.captured || motion.invulnerable() || device.hit_cd > 0 {
        return;
    }
    if (a.0 - e.0).length() < Fx32::from_f32(CONTACT_DIST) {
        device.progress = (device.progress - CONTACT_LOSS).max(0.0);
        device.hit_cd = CONTACT_COOLDOWN;
        knock_device_offline(&mut state, &mut stroke);
    }
}

/// The forced retract a hit causes: drop to Stowed and abandon the in-flight
/// stroke. Shared by body contact and projectile hits.
fn knock_device_offline(state: &mut PlayerState, stroke: &mut Stroke) {
    *state = PlayerState::Stowed;
    stroke.0.clear();
}

// --- Rendering ---------------------------------------------------------------

/// WorldPos → 3D transform; toggle the captured enemy's mesh off via [`Hidden`].
/// Entities carrying a [`Height`] (the avatar) are lifted on screen-Y by their
/// jump height; everything else (incl. the ground [`Shadow`]) renders flat.
fn sync_3d(
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &WorldPos,
        &mut Transform3d,
        Option<&Enemy>,
        Option<&Height>,
        Has<Hidden>,
    )>,
) {
    for (e, pos, mut t, enemy, height, hidden) in &mut q {
        t.translation.x = pos.0.x.to_f32();
        let lift = height.map_or(0.0, |h| h.z.to_f32());
        t.translation.y = pos.0.y.to_f32() + lift;
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
    state: Res<PlayerState>,
    touches: Res<Touches>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform3d, Has<Hidden>), With<Cursor>>,
) {
    let Some((e, mut t, hidden)) = q.iter_mut().next() else {
        return;
    };
    let show = state.is_deployed() && touches.iter().next().is_some();
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
fn sync_map_markers(
    state: Res<PlayerState>,
    mut q: Query<(&WorldPos, &mut Sprite, Option<&Enemy>)>,
) {
    for (pos, mut sprite, enemy) in &mut q {
        let hide = !state.is_deployed() || enemy.is_some_and(|e| e.captured);
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
    state: Res<PlayerState>,
    stroke: Res<Stroke>,
    mut dots: Query<&mut Sprite, With<PathDot>>,
) {
    let line = if state.is_deployed() {
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
    pstate: Res<PlayerState>,
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
            let label = if pstate.is_deployed() {
                "DEPLOYED"
            } else {
                "stowed "
            };
            let _ = write!(text.0, "{label}  capture {pct:>3}%");
        }
    }
}
