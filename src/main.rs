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
//! Capture (now the real model, #26 — see `capture`): each loop that fully
//! encloses the enemy's footprint on the map adds progress. Past the destroy
//! threshold it's *breakable* — retract and **dash into it to destroy** (the
//! expedient exit); draw all the way to full to **liberate** it (the rewarded
//! exit). Enemy contact while deployed costs that enemy's progress (the
//! pressure) — unless you're mid-roll (i-frames). **START** re-arms the enemy.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_3d::prelude::*;
use bevy_nds_loop::{densify, smooth as path_smooth};
use bevy_nds_math::{Fx32, FxVec2};
use bevy_nds_scene::{CameraMode, LoadedScene, SceneInstance, ScenePath};
use bevy_nds_sprite::prelude::*;

mod capture;
mod control;
mod flags;
mod menu;
mod player;
mod radial;
mod transition;

use player::{Health, Height, Locomotion, Motion, PlayerState, Shadow, StickState};
use transition::{Transition, Zone};

mod sprites {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/sprites.rs"));
}

mod levels {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/levels.rs"));
}

/// The level the game boots into. Single-level for now (a level-select menu —
/// and a level `exit` seam — are deferred; see #27). Matches the
/// `assets/levels/<BOOT_LEVEL>/` directory, so neighbour zones resolve within it.
const BOOT_LEVEL: &str = "facility";
/// The boot level's entry zone stem (the zone the avatar seeds into). Matches the
/// `entry` in `assets/levels/facility/level.ron`.
const BOOT_ZONE: &str = "atrium";
/// How many zones in [`BOOT_LEVEL`] carry level objectives — the total the level
/// exit needs (see [`flags::LevelProgress`]). A **hardcoded stand-in** for the
/// deferred runtime level-header blob (#27); `facility` has two (atrium +
/// corridor). Moves into the level-header when the menu / level-select lands.
const BOOT_LEVEL_OBJECTIVE_ZONES: usize = 2;

// --- Arena / camera ----------------------------------------------------------

const ARENA_HALF: f32 = 2.0;
/// Ground plane height. Meshes are centred on their origin and rendered with
/// their centre at Y=0, so the floor (and the ground shadow) sit a half-object
/// below — at the objects' feet rather than slicing through their middles.
const GROUND_Y: f32 = -0.16;

// --- Camera director (#23) ---------------------------------------------------
// The world is Y-up (ground = XZ plane). The top screen gets a real angled
// camera; the bottom tactical map stays top-down (the two are now decoupled).
/// Soft-follow: camera height above the ground and distance behind (+Z) the
/// avatar, with a fixed downward pitch → a 3/4 view that stands objects upright.
const CAM_HEIGHT: f32 = 1.7;
const CAM_DIST: f32 = 2.0;
const CAM_PITCH: f32 = -0.7; // ≈ -40°, looking down at the ground
/// Top-down toggle (cluster ▲): straight above, looking down — correlates with
/// the tactical map.
const CAM_TD_HEIGHT: f32 = 3.2;
/// Position low-pass factor (0 = locked, 1 = instant). Drives the soft
/// avatar-follow, so it stays snappy.
const CAM_SMOOTH: f32 = 0.18;
/// Pitch low-pass factor — slower than [`CAM_SMOOTH`] so a zone transition's
/// framing tilt (e.g. Follow → Rail2.5D) eases as a gentle morph. Pitch only
/// changes on a transition or the top-down toggle, never during following, so
/// slowing it leaves the avatar-follow untouched. Tune for transition speed.
const CAM_TURN_SMOOTH: f32 = 0.07;
/// Progress per frame of the zone-transition camera **warp** (`CamWarp`): the
/// timed ease-in-out + slerp blend between the old and new zone framings. At 60
/// fps, `0.02` ≈ a 50-frame (~0.8 s) morph. Tune for transition duration.
const CAM_WARP_STEP: f32 = 0.02;

const CURSOR_SCALE: f32 = 0.12;

/// Avatar↔landmark separation enforced by collision (radii summed). The
/// landmark *positions* now come from the loaded space (the `Landmarks`
/// resource), not a const — only the collision radius is tuning.
const LANDMARK_COLLIDE: f32 = 0.26;

// Player locomotion tuning + the Stowed↔Deployed controller live in `player`.

// --- Enemy + projectile ------------------------------------------------------

const ENEMY_SPEED: f32 = 0.8;
/// Process-range cull radius (world units) for resident-neighbour gameplay (#27
/// follow-up): enemies farther than this from the avatar skip their per-frame
/// tick (patrol). Kept ≥ the render range so anything *visible* still animates —
/// the cull only silences enemies deep in a neighbour zone, off-screen behind the
/// fog. The explicit per-object twin of `bevy_nds_3d`'s render-side `RenderRange`.
const PROCESS_RANGE: f32 = 14.0;
// The enemy's patrol path now lives on its `ScenePath` (authored in the space),
// not a const — see `assets/spaces/atrium.ron`.
/// The enemy pauses at each waypoint (stop-and-go), opening capture windows.
const ENEMY_PAUSE: u8 = 45; // frames (~0.75 s)
const CONTACT_DIST: f32 = 0.28;
const CONTACT_COOLDOWN: u8 = 30; // frames between body hits
/// The enemy lobs a projectile at the avatar while deployed — the real dodge
/// threat. Roll (i-frames) or move out of the way.
const PROJ_SPEED: f32 = 1.7;
const PROJ_SCALE: f32 = 0.07;
const FIRE_INTERVAL: u8 = 80; // frames between shots
const PROJ_HIT_DIST: f32 = 0.18;
const PROJ_DESPAWN: f32 = ARENA_HALF + 0.4; // out-of-bounds cutoff

// --- Tactical map ------------------------------------------------------------

// The tactical map mirrors the top camera's view so the two screens correlate:
// scale = (screen_half_height) / (camera visible half-height at z=0)
//       = 96 / (CAM_Z * tan(fov/2)) = 96 / (3.2 * tan(35°)) ≈ 42.8 px/world-unit.
// Same aspect (256/192) as the camera, so one uniform scale fits both axes.
const MAP_SCALE: f32 = 42.8;
const MAP_CX: f32 = 128.0;
const MAP_CY: f32 = 96.0;
const PARK_Y: i16 = 200; // off-screen park for hidden sprites

// --- Radial wheel overlay (#25) ----------------------------------------------

const RADIAL_RADIUS: f32 = 44.0; // px from the wheel centre to each spoke icon
const RADIAL_LINE_DOTS: usize = 10; // dots along the centre→stylus pointer line
const RADIAL_HOVER_POP: f32 = 8.0; // extra px the hovered spoke pops outward

// --- Loop draw (Spike B) -----------------------------------------------------

const MIN_SPACING: f32 = 4.0;
const MAX_POINTS: usize = 80;
const DOT_POOL: usize = 90;
const TRAIL_STEP: f32 = 4.0;
const CLOSE_TOL: f32 = 2.0;

/// World XY → tactical-map screen pixels. The map matches the top screen's
/// orientation 1:1 for the fixed capture framing (camera at +Z looking −Z):
/// depth **+Z reads *down* the map** (near the camera), −Z up (into the
/// distance) — same as the 3D view — so a dodge/draw direction never needs
/// mental rotation between screens. `+X` is right on both.
fn world_to_map(p: FxVec2) -> (i16, i16) {
    let x = MAP_CX + p.x.to_f32() * MAP_SCALE;
    let y = MAP_CY + p.y.to_f32() * MAP_SCALE;
    (x as i16, y as i16)
}

/// A flat, **unlit**, single-colour quad in the horizontal **XZ** plane (the
/// ground). Used for the floor and the player's ground shadow — the unlit path
/// honours the vertex colour directly (unlike [`DsMesh::cube`], whose per-face
/// colours ignore any material).
fn flat_quad_xz(half_w: f32, half_d: f32, color: [u8; 3]) -> DsMesh {
    let v = |x: f32, z: f32| Vertex::new(Vec3::new(x, 0.0, z), color);
    let tris = alloc::vec![
        [v(-half_w, -half_d), v(half_w, -half_d), v(half_w, half_d)],
        [v(-half_w, -half_d), v(half_w, half_d), v(-half_w, half_d)],
    ];
    DsMesh {
        tris: Cow::Owned(tris),
        lit: false,
        baked: None,
    }
}

/// Tactical-map screen pixels → world ground position (inverse of
/// [`world_to_map`]), placed on the XZ ground plane (`y = 0`).
fn map_to_world(sx: f32, sy: f32) -> Vec3 {
    Vec3::new((sx - MAP_CX) / MAP_SCALE, 0.0, (sy - MAP_CY) / MAP_SCALE)
}

/// Marker for a space's ground plane. Tagged so it's swapped with its space — a
/// transition despawns it and spawns a fresh one sized to the new space's
/// camera. Not a `SceneInstance` (it's procedural chrome, not authored geometry),
/// so the transition handles it explicitly.
#[derive(Component)]
struct SpaceFloor;

/// Spawn the ground plane for a zone, sized to its **walkable bounds** and
/// placed at `offset` in the active frame. Sizing to bounds (rather than a fixed
/// camera pad) makes adjacent zones' floors *abut* at their shared edge — with
/// neighbours resident (#27 seamless streaming) the atrium's floor (x∈[-2,2])
/// meets the corridor's (x∈[2,6.4]) exactly at the seam, so the ground reads as
/// one continuous surface. The arena-square / rail-strip shapes fall out of the
/// bounds themselves (the corridor authors a long, shallow rect).
/// Height (world units) of a gate barrier wall — about avatar-tall, enough to
/// read as an obstacle without hiding the fogged neighbour zone behind it.
const GATE_BARRIER_HEIGHT: f32 = 0.5;
/// Locked-gate wall colour (a warning red).
const GATE_BARRIER_COLOR: [u8; 3] = [200, 60, 50];

/// A vertical wall quad (unlit), rising from local `y = 0` to `height`, spanning
/// ±`half_span` along X (`along_x`) or Z — the gate-barrier slab.
fn wall_quad(along_x: bool, half_span: f32, height: f32, color: [u8; 3]) -> DsMesh {
    let v = |s: f32, y: f32| {
        let p = if along_x {
            Vec3::new(s, y, 0.0)
        } else {
            Vec3::new(0.0, y, s)
        };
        Vertex::new(p, color)
    };
    let tris = alloc::vec![
        [v(-half_span, 0.0), v(half_span, 0.0), v(half_span, height)],
        [
            v(-half_span, 0.0),
            v(half_span, height),
            v(-half_span, height)
        ],
    ];
    DsMesh {
        tris: Cow::Owned(tris),
        lit: false,
        baked: None,
    }
}

/// A wall at a **locked gated boundary** (#27): visible while its `gate` flag is
/// down, hidden the moment it's raised — the in-world "you're gated into this
/// zone" tell (`update_gate_barriers` reconciles its visibility).
#[derive(Component)]
struct GateBarrier {
    gate: u32,
}

/// Spawn a wall at each **gated** boundary of the active zone (`gate != 0`), sized
/// to the connection's span. Called on zone load (boot + crossing); the barriers
/// are per-zone chrome, torn down with the zone on the next crossing.
fn spawn_gate_barriers(commands: &mut Commands, zone: &Zone, flags: &flags::Flags) {
    let [min_x, min_z, max_x, max_z] = zone.bounds;
    for c in &zone.conns {
        if c.gate == 0 {
            continue; // ungated edge — no barrier
        }
        let half = (c.hi - c.lo).abs() * 0.5;
        let center = (c.lo + c.hi) * 0.5;
        let (along_x, tx, tz) = match c.side {
            0 => (false, min_x, center), // west −X
            1 => (false, max_x, center), // east +X
            2 => (true, center, min_z),  // south −Z
            3 => (true, center, max_z),  // north +Z
            _ => continue,
        };
        let mut e = commands.spawn((
            GateBarrier { gate: c.gate },
            wall_quad(along_x, half, GATE_BARRIER_HEIGHT, GATE_BARRIER_COLOR),
            Transform3d {
                translation: Vec3::new(tx, GROUND_Y, tz),
                rotation: Vec3::ZERO,
                scale: Vec3::ONE,
            },
        ));
        // If the gate is already open (re-entering a cleared zone), spawn the wall
        // already `Hidden` so it never flashes before `update_gate_barriers` runs.
        if flags.is_raised(c.gate) {
            e.insert(Hidden);
        }
    }
}

/// Show a gate barrier while its flag is down, hide it once raised (#27). Uses
/// `Hidden` (not despawn) so a START reset — which re-locks the gate — brings the
/// wall back with no respawn bookkeeping.
fn update_gate_barriers(
    mut commands: Commands,
    flags: Res<flags::Flags>,
    barriers: Query<(Entity, &GateBarrier, Has<Hidden>)>,
) {
    for (e, barrier, hidden) in &barriers {
        match (flags.is_raised(barrier.gate), hidden) {
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

fn spawn_zone_floor(commands: &mut Commands, bounds: [f32; 4], offset: (f32, f32)) {
    let [min_x, min_z, max_x, max_z] = bounds;
    let half_x = (max_x - min_x) * 0.5;
    let half_z = (max_z - min_z) * 0.5;
    let cx = (min_x + max_x) * 0.5 + offset.0;
    let cz = (min_z + max_z) * 0.5 + offset.1;
    commands.spawn((
        SpaceFloor,
        flat_quad_xz(half_x, half_z, [50, 56, 78]),
        Transform3d {
            translation: Vec3::new(cx, GROUND_Y, cz),
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
        },
    ));
}

/// Spawn the **resident neighbour** zones' geometry (#27 seamless streaming):
/// render-only, fogged entities placed at each neighbour's offset in the active
/// frame, so the player sees into adjacent zones. A connection's `delta` is
/// `place_active − place_neighbour`, so the neighbour's geometry sits at `−delta`
/// in the active frame. Reuses the current zone's already-derived `conns`.
fn spawn_resident_neighbours(commands: &mut Commands, zone: &Zone, snapshot: &ZoneCaptureState) {
    for c in &zone.conns {
        let path = bevy_nds_scene::level_space_path(&zone.level, &c.neighbour);
        let Some(scene) = bevy_nds_scene::load(&path) else {
            continue;
        };
        let offset = (-c.delta[0], -c.delta[1]);
        spawn_zone_floor(commands, scene.bounds, offset); // neighbour's ground, abutting ours
        spawn_neighbour(commands, &scene, offset, &c.neighbour, snapshot);
    }
}

/// Spawn one neighbour zone's instances as render-only entities, offset into the
/// active frame. No `SceneInstance` (so `specialize_scene` skips them — no
/// duplicate gameplay entity) and no map sprite; just mesh + transform +
/// material, tagged `NeighbourInstance` so the next crossing can clear them. The
/// avatar instance is skipped — the avatar is the single persistent entity.
fn spawn_neighbour(
    commands: &mut Commands,
    scene: &bevy_nds_scene::SceneData,
    offset: (f32, f32),
    stem: &str,
    snapshot: &ZoneCaptureState,
) {
    for inst in &scene.instances {
        if inst.role == "avatar" {
            continue;
        }
        // Enemy identity: its LOCAL (pre-offset) spawn position + zone stem, plus
        // the render `offset` so its live position persists in the local frame.
        let enemy_member = (inst.role == "enemy").then(|| ZoneMember {
            stem: alloc::string::String::from(stem),
            key: zone_key(inst.pos[0], inst.pos[2]),
            offset,
        });
        let restored = enemy_member.as_ref().and_then(|m| snapshot.restore(m));
        // Skip-at-spawn (#27): a captured neighbour enemy isn't spawned at all —
        // not even its render entity — so there's no flash and nothing to process.
        if restored.is_some_and(|s| s.resolved.is_some()) {
            continue;
        }
        // Spawn at the restored *local* position (into the active frame via the
        // offset) if we've seen this enemy, else at its authored position.
        let world = match restored {
            Some(st) => Vec3::new(st.local.0 + offset.0, inst.pos[1], st.local.1 + offset.1),
            None => Vec3::new(inst.pos[0] + offset.0, inst.pos[1], inst.pos[2] + offset.1),
        };
        let mut e = commands.spawn((
            NeighbourInstance,
            Transform3d {
                translation: world,
                rotation: Vec3::from_array(inst.rot),
                scale: Vec3::from_array(inst.scale),
            },
        ));
        if let Some(name) = &inst.mesh {
            if let Some(mesh) = bevy_nds_scene::load_mesh(name) {
                e.insert(mesh);
            }
        }
        if let Some((diffuse, ambient)) = inst.material {
            e.insert(DsMaterial { diffuse, ambient });
        }
        // Enemy neighbours are **full gameplay entities** at their offset (#27
        // follow-up): they patrol (range-culled) and are capturable through the
        // fog, and their full runtime state persists across a crossing (`ZoneMember`
        // + the snapshot). Non-enemy neighbours (landmarks) stay render-only.
        if let Some(member) = enemy_member {
            let (enemy, cap) = match restored {
                Some(st) => (
                    Enemy {
                        wp: st.wp,
                        pause: st.pause,
                    },
                    capture::Capture {
                        progress: st.progress,
                        resolved: None,
                    },
                ),
                None => (Enemy { wp: 1, pause: 0 }, capture::Capture::default()),
            };
            e.insert((
                enemy,
                cap,
                capture::VulnerabilityShape::circle(),
                WorldPos(FxVec2::from_f32(world.x, world.z)),
                Stylized,
                Sprite::new(sprites::BLIP).at(0, PARK_Y),
                member,
                // Patrol waypoints offset into the active frame (the path is
                // authored in the neighbour's local coords).
                ScenePath(
                    inst.path
                        .iter()
                        .map(|p| bevy_nds_scene::Vec2::new(p[0] + offset.0, p[1] + offset.1))
                        .collect(),
                ),
            ));
            if inst.flags & flags::OBJECTIVE != 0 {
                e.insert(flags::Objective);
            }
            if inst.flags & flags::LEVEL_OBJECTIVE != 0 {
                e.insert(flags::LevelObjectiveTag);
            }
        }
    }
}

// --- Resources ---------------------------------------------------------------

/// The capture device: a brief cooldown after a hit so contact doesn't drain
/// progress every frame. Capture progress is now **per enemy**
/// ([`capture::Capture`], #26), not here; deploy state lives in [`PlayerState`].
#[derive(Resource, Default)]
struct Device {
    hit_cd: u8,
}

/// Enemy fire cadence (frames until the next shot).
#[derive(Resource, Default)]
struct EnemyFire {
    cd: u8,
}

#[derive(Resource, Default)]
struct Stroke(Vec<FxVec2>);

/// The player's top-down camera toggle (cluster ▲, [`control::Action::CamTopDown`]).
/// The *base* framing is now authored per-space (the loaded space's
/// [`CameraMode`], read from [`LoadedScene`] — #27); this is the one player-facing
/// override the control model locks in (#17): force a straight-down tactical view
/// over whatever the space authored. Off = use the authored framing. Gated to
/// while-stowed (deploying locks the frame — CaptureFraming).
#[derive(Resource, Default)]
struct TopDownToggle(bool);

/// OrbitSet camera state (#23, last deferred camera mode). Hold cluster ◄
/// ([`control::Action::CamOrbit`]) and drag the stylus to choose a **yaw** the
/// camera orbits the avatar at; release **locks** it; a bare tap (hold with no
/// drag) **resets** to the space's default framing. It's a yaw *offset* on the
/// still-avatar-following [`CameraMode::Follow`] camera — not a free camera (#17:
/// "No free player-driven camera"). Stowed-only (deploying locks the frame) and
/// Follow-only (the open-arena mode #17 pairs orbit-set with).
#[derive(Resource, Default)]
struct Orbit {
    /// Locked yaw offset (radians). 0 = the default (camera directly behind).
    yaw: f32,
    /// Live yaw while ◄ is held + dragging (previewed before it locks).
    live: Option<f32>,
    /// Stylus x at the start of the current drag + the yaw it builds on.
    anchor: Option<f32>,
    base: f32,
    /// Did the stylus move far enough this hold to count as a drag (vs a tap)?
    dragged: bool,
    /// Was ◄ held last frame (to detect the release that resolves the gesture)?
    holding: bool,
}

// --- Components ---------------------------------------------------------------

#[derive(Component)]
struct Avatar;

/// Marks runtime entities that **survive a zone crossing** — the single
/// persistent avatar (#27 seamless streaming). The transition despawns the old
/// zone's `SceneInstance` + `NeighbourInstance` entities but never a
/// `Persistent` one, so the avatar carries across levels-worth of zones.
#[derive(Component)]
struct Persistent;

/// A **render-only** entity from a *resident neighbour* zone — mesh + transform
/// + material at the neighbour's offset in the active frame, carrying no
/// gameplay (no `SceneInstance`, no map sprite). Tagged so a crossing can clear
/// the old resident set before spawning the new one (#27 seamless streaming).
#[derive(Component)]
struct NeighbourInstance;

/// A static landmark obstacle, attached by `specialize_scene` to every scene
/// instance with `role: "landmark"`.
#[derive(Component)]
struct Landmark;

/// Landmark world positions, harvested from the loaded space by
/// `specialize_scene` so avatar collision has a single source of truth (no
/// duplicated const). Populated once when the space's instances first appear.
#[derive(Resource, Default)]
struct Landmarks(alloc::vec::Vec<FxVec2>);

/// The enemy's patrol AI: current waypoint index + dwell timer. Capture state
/// (progress / resolution) is a **separate** [`capture::Capture`] component on
/// the same entity (#26), so it persists across stow/deploy and scales to the
/// shape matrix — the enemy identity and the capture model stay decoupled.
#[derive(Component)]
struct Enemy {
    wp: usize,
    pause: u8,
}

/// Stable per-enemy identity for **runtime-state persistence across zone
/// crossings** (#27 follow-up): the enemy's zone stem + its **local** (pre-offset)
/// spawn position (quantized to millimetres), plus the world-frame `offset` it's
/// currently rendered at (`(0,0)` when its zone is active, `−delta` while a
/// resident neighbour). The stem+key survive despawn/respawn because the `.scene`
/// always re-spawns the enemy at the same local spawn position; the offset lets
/// [`mirror_capture`] record the *local* live position so it's frame-independent.
#[derive(Component, Clone)]
struct ZoneMember {
    stem: alloc::string::String,
    key: (i32, i32),
    offset: (f32, f32),
}

/// Quantise a local (x, z) spawn position into a stable [`ZoneMember`] key.
fn zone_key(x: f32, z: f32) -> (i32, i32) {
    ((x * 1000.0) as i32, (z * 1000.0) as i32)
}

/// The full runtime state of one enemy, persisted so it resumes *exactly* across
/// a zone crossing (#27): capture progress/outcome, current **local** position,
/// and patrol waypoint + dwell. Position/patrol are stored in the zone's local
/// frame so they're valid whether the zone reloads as active or as a neighbour.
#[derive(Clone, Copy)]
struct EnemyState {
    progress: f32,
    resolved: Option<capture::CaptureOutcome>,
    local: (f32, f32),
    wp: usize,
    pause: u8,
}

/// Persistent snapshot of every resident enemy's runtime state, keyed by
/// `(zone stem, local x·1000, local z·1000)` (#27 persistence). [`mirror_capture`]
/// refreshes it live each frame; the spawn path reads it inline the moment an
/// enemy (re)spawns, so a zone that despawned on a crossing and respawns later
/// resumes each enemy where it was. Persistence holds within the resident window
/// (current zone + its 1-hop neighbours); a fully-unloaded zone resets.
#[derive(Resource, Default)]
struct ZoneCaptureState(
    alloc::collections::BTreeMap<(alloc::string::String, i32, i32), EnemyState>,
);

impl ZoneCaptureState {
    /// The persisted state for an enemy, or `None` if never seen (spawn fresh).
    fn restore(&self, m: &ZoneMember) -> Option<EnemyState> {
        self.0.get(&(m.stem.clone(), m.key.0, m.key.1)).copied()
    }
    fn save(&mut self, m: &ZoneMember, st: EnemyState) {
        self.0.insert((m.stem.clone(), m.key.0, m.key.1), st);
    }
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

/// A radial-wheel spoke icon (index 0 = device/up). A placeholder OAM sprite,
/// laid out around the wheel origin while the wheel is open (#25).
#[derive(Component)]
struct RadialSpoke(u8);

/// A dot on the pointer line drawn from the wheel centre to the stylus (#25).
#[derive(Component)]
struct RadialLine;

#[derive(Component)]
struct InfoHud;

/// The capture-tally line (liberated / destroyed counts) under the status line.
#[derive(Component)]
struct TallyHud;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins)
        .add_plugins(Ds3dPlugin)
        .add_plugins(SpritePlugin)
        .add_plugins(menu::MenuPlugin)
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
        .init_resource::<TopDownToggle>()
        .init_resource::<Orbit>()
        .init_resource::<CamWarp>()
        .init_resource::<EnemyFire>()
        .init_resource::<Stroke>()
        .init_resource::<Landmarks>()
        .init_resource::<Transition>()
        .init_resource::<Zone>()
        .init_resource::<flags::Flags>()
        .init_resource::<flags::LevelProgress>()
        .init_resource::<ZoneCaptureState>()
        .init_resource::<capture::CaptureTally>()
        .init_resource::<radial::Radial>()
        .init_resource::<Health>()
        .add_event::<capture::CaptureResolved>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                // Sim + gameplay: run the space transition / reset (which spawn a
                // zone's entities), THEN specialise them the **same frame** — an
                // auto-inserted sync point flushes the spawns first — so a restored
                // enemy is positioned before it ever renders (no 1-frame spawn-pos
                // blip). Then controller, cameras, enemies, and capture.
                (
                    transition::transition_spaces,
                    radial::drive_radial,
                    player::toggle_tuning,
                    reset_enemy,
                    specialize_scene,
                    player::move_player,
                    player::sync_shadow,
                    orbit_camera,
                    drive_camera,
                    patrol_enemy,
                    fire_projectile,
                    move_projectile,
                    capture::draw_capture,
                    capture::dash_destroy,
                    capture::enemy_contact,
                    capture::tally_captures,
                    // Zone-clear source (#27): after this frame's captures
                    // resolve, raise the arena's flag so the next crossing check
                    // sees the gate open.
                    flags::clear_zone,
                    // Level-objective source (#27 tier 2): roll freeform captures
                    // up to the level exit.
                    flags::tally_level_objective,
                    // Persist this frame's capture state so it survives a zone
                    // despawn/respawn on crossing (#27). Runs after captures
                    // resolve and after `restore_capture`, so it never clobbers a
                    // just-restored snapshot.
                    mirror_capture,
                )
                    .chain()
                    // Paused while the options menu is open (Select). Rendering
                    // keeps running below, so the frozen world stays on screen
                    // and render-style toggles are visible live.
                    .run_if(menu::playing),
                // Rendering: mirror world state onto the two screens.
                (
                    sync_3d,
                    update_cursor,
                    sync_map_markers,
                    update_trail,
                    update_radial_overlay,
                    update_gate_barriers,
                    update_hud,
                )
                    .chain(),
            )
                .chain(),
        );
    }
}

// --- Setup -------------------------------------------------------------------

fn setup(
    mut commands: Commands,
    mut camera: ResMut<Camera3d>,
    mut zone: ResMut<Zone>,
    mut level: ResMut<flags::LevelProgress>,
    snapshot: Res<ZoneCaptureState>,
    game_flags: Res<flags::Flags>,
) {
    // The level exit needs every level-objective zone cleared (hardcoded total —
    // the deferred level-header's stand-in, #27).
    level.needed = BOOT_LEVEL_OBJECTIVE_ZONES;
    let cube = include_obj!("assets/cube.obj", center);

    // Level content — avatar, enemy, landmarks — is authored data (issue #27).
    // Load the baked space and spawn its instances; `specialize_scene` attaches
    // the gameplay components (Avatar / Enemy / Landmark) by role. The ground
    // plane is part of the space (sized to its camera; swapped on a transition);
    // the shadow, cursor, projectile, trail-dot pool and HUD (below) stay runtime
    // chrome — pools and systems, not level geometry.
    // Default 3/4 follow framing — the fallback if the space fails to load (the
    // authored camera below overrides it, and `drive_camera` takes over per-frame).
    camera.position = Vec3::new(0.0, CAM_HEIGHT, CAM_DIST);
    camera.pitch = CAM_PITCH;
    zone.level.clear();
    zone.level.push_str(BOOT_LEVEL); // neighbour zones resolve within this level
    zone.stem.clear();
    zone.stem.push_str(BOOT_ZONE); // the entry zone's stable key (level-objective)
    if let Some(scene) = bevy_nds_scene::load(levels::facility::ATRIUM) {
        // Seed the initial framing from the space's authored camera (avatar at
        // origin) so boot lands on the right view instead of gliding in from the
        // default. The per-frame `drive_camera` director then takes over.
        let (pos, pitch, yaw) = frame_for(scene.camera, 0.0, 0.0, 0.0);
        camera.position = pos;
        camera.pitch = pitch;
        camera.yaw = yaw;
        zone.set(&scene); // boot zone's bounds + connections
        spawn_zone_floor(&mut commands, scene.bounds, (0.0, 0.0)); // active floor (sized to bounds)
        bevy_nds_scene::spawn(&mut commands, scene); // active zone (incl. the avatar)
    }
    // Walls at this zone's locked gated edges (#27): the in-world "gated in" tell.
    spawn_gate_barriers(&mut commands, &zone, &game_flags);
    // Resident neighbours (#27 seamless streaming): render-only, fogged, each
    // with its own floor at its offset, so you see into the next zone over
    // continuous ground. Read from the just-set `zone.conns`.
    spawn_resident_neighbours(&mut commands, &zone, &snapshot);

    // Ground shadow — a flat dark quad (no `Height`) that stays at the avatar's
    // ground position, so a jump's screen-Y lift opens a visible gap above it.
    // Slightly wider than tall to read as a contact shadow; sits just in front
    // of the floor. `sync_shadow` keeps it under the avatar.
    commands.spawn((
        Shadow,
        WorldPos(FxVec2::ZERO),
        flat_quad_xz(0.14, 0.1, [16, 18, 26]),
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
        },
    ));

    // Stylus cursor — bright cube, starts Hidden (the mesh is skipped by the
    // renderer) until deployed + drawing.
    commands.spawn((
        Cursor,
        Stylized,
        cube.clone(),
        DsMaterial {
            diffuse: [245, 240, 180],
            ambient: [70, 68, 40],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(0.0, 0.6, 0.0),
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
        Stylized,
        cube.clone(),
        DsMaterial {
            diffuse: [255, 150, 40],
            ambient: [70, 42, 12],
        },
        Transform3d {
            translation: Vec3::ZERO,
            rotation: Vec3::new(0.0, 0.8, 0.0),
            scale: Vec3::splat(PROJ_SCALE),
        },
        Hidden,
    ));

    // Trail-dot pool (map), parked.
    for _ in 0..DOT_POOL {
        commands.spawn((PathDot, Sprite::new(sprites::DOT).at(0, PARK_Y)));
    }

    // Radial-wheel overlay (#25): 5 spoke icons + a pointer line of dots, parked
    // off-screen until the wheel opens. Placeholder art — OBSTACLE for the spokes,
    // DOT for the pointer (both already loaded, so no extra palette bank).
    for i in 0..bevy_nds_math::radial::SPOKES {
        commands.spawn((RadialSpoke(i), Sprite::new(sprites::OBSTACLE).at(0, PARK_Y)));
    }
    for _ in 0..RADIAL_LINE_DOTS {
        commands.spawn((RadialLine, Sprite::new(sprites::DOT).at(0, PARK_Y)));
    }

    let b = DsScreen::Bottom;
    commands.spawn((b, TilePos::new(1, 0), InfoHud, DsText::new("")));
    commands.spawn((b, TilePos::new(1, 1), TallyHud, DsText::new("")));
    commands.spawn((
        b,
        TilePos::new(1, 22),
        DsText::new("L: hold+flick=deploy tap=stow"),
    ));
    commands.spawn((
        b,
        TilePos::new(1, 23),
        DsText::new("up=cam START=reset SELECT=menu"),
    ));
}

/// The game-specific half of the scene pipeline: turn freshly loaded, opaque
/// scene instances into gameplay entities by their authored `role`.
/// `bevy_nds_scene` stays game-agnostic (it only knows meshes, transforms,
/// materials, and a role string); this is where `"avatar"` / `"enemy"` /
/// `"landmark"` become the game's components. The `Added` filter runs it once
/// per instance; a loaded instance's ground position comes from its spawned
/// `Transform3d` (x, z), seeding the `WorldPos` that `sync_3d` then drives.
fn specialize_scene(
    mut commands: Commands,
    mut landmarks: ResMut<Landmarks>,
    zone: Res<Zone>,
    snapshot: Res<ZoneCaptureState>,
    q: Query<(Entity, &SceneInstance, &Transform3d), Added<SceneInstance>>,
) {
    for (e, inst, tf) in &q {
        let pos = WorldPos(FxVec2::from_f32(tf.translation.x, tf.translation.z));
        match inst.role.as_str() {
            "avatar" => {
                // The avatar is the single persistent entity (#27 seamless
                // streaming): consumed once from the entry zone at boot, it drops
                // its `SceneInstance` and gains `Persistent` so no crossing ever
                // despawns it (later zone spawns strip their avatar instance, so
                // this arm fires exactly once).
                commands.entity(e).remove::<SceneInstance>().insert((
                    Avatar,
                    Persistent,
                    pos,
                    Height::default(),
                    Sprite::new(sprites::PLAYER).at(0, PARK_Y),
                ));
            }
            "enemy" => {
                // Active zone is origin-centric (offset 0), so the spawned
                // transform *is* the local position — the stable persistence key.
                let member = ZoneMember {
                    stem: zone.stem.clone(),
                    key: zone_key(tf.translation.x, tf.translation.z),
                    offset: (0.0, 0.0),
                };
                let restored = snapshot.restore(&member);
                // Skip-at-spawn (#27): an already-captured enemy isn't spawned at
                // all — no lingering Hidden entity, no per-frame processing, no
                // flash. The crate already spawned a render entity for it, so drop
                // that. Completion is recorded in the persistent Flags/LevelProgress,
                // so nothing downstream needs the dead enemy. START-reset clears the
                // snapshot + reloads the zone to bring captured enemies back.
                if restored.is_some_and(|s| s.resolved.is_some()) {
                    commands.entity(e).despawn();
                    continue;
                }
                // Resume the enemy's full state (position + patrol + progress) if
                // it was seen before, else spawn fresh at its authored position.
                let (enemy, cap, epos) = match restored {
                    Some(st) => (
                        Enemy {
                            wp: st.wp,
                            pause: st.pause,
                        },
                        capture::Capture {
                            progress: st.progress,
                            resolved: None,
                        },
                        WorldPos(FxVec2::from_f32(st.local.0, st.local.1)), // offset 0
                    ),
                    None => (Enemy { wp: 1, pause: 0 }, capture::Capture::default(), pos),
                };
                let mut ec = commands.entity(e);
                ec.insert((
                    enemy,
                    cap,
                    capture::VulnerabilityShape::circle(),
                    epos,
                    // Outlined + cel-shaded so the threat reads at a glance;
                    // terrain stays smooth (see `Stylized`).
                    Stylized,
                    Sprite::new(sprites::BLIP).at(0, PARK_Y),
                    member,
                    // Correct the render transform now (not next frame via
                    // `sync_3d`): a restored enemy's crate-spawned transform sits at
                    // its *authored* position, so overwrite it with the resumed one.
                    Transform3d {
                        translation: Vec3::new(
                            epos.0.x.to_f32(),
                            tf.translation.y,
                            epos.0.y.to_f32(),
                        ),
                        rotation: tf.rotation,
                        scale: tf.scale,
                    },
                ));
                // Objective enemies (OBJECTIVE bit) count toward the zone-clear
                // gate (#27); freeform ones don't. `flags` rides the `SceneInstance`.
                if inst.flags & flags::OBJECTIVE != 0 {
                    ec.insert(flags::Objective);
                }
                // Level-objective enemies (LEVEL_OBJECTIVE bit) roll up to the
                // level exit instead of a zone gate (#27 tier 2).
                if inst.flags & flags::LEVEL_OBJECTIVE != 0 {
                    ec.insert(flags::LevelObjectiveTag);
                }
            }
            "landmark" => {
                landmarks.0.push(pos.0);
                commands.entity(e).insert((
                    Landmark,
                    pos,
                    Sprite::new(sprites::OBSTACLE).at(0, PARK_Y),
                ));
            }
            // Unknown roles render (mesh + transform) but carry no behaviour.
            _ => {}
        }
    }
}

/// Mirror every resident enemy's live runtime state (capture + position + patrol)
/// into [`ZoneCaptureState`], so a zone that despawns on a crossing and respawns
/// later resumes each enemy exactly (#27 persistence). Covers active *and*
/// neighbour enemies. Position is de-offset to the zone's **local** frame so it's
/// valid regardless of how the zone later reloads.
fn mirror_capture(
    mut snapshot: ResMut<ZoneCaptureState>,
    q: Query<(&ZoneMember, &capture::Capture, &WorldPos, &Enemy)>,
) {
    for (member, cap, pos, enemy) in &q {
        let local = (
            pos.0.x.to_f32() - member.offset.0,
            pos.0.y.to_f32() - member.offset.1,
        );
        snapshot.save(
            member,
            EnemyState {
                progress: cap.progress,
                resolved: cap.resolved,
                local,
                wp: enemy.wp,
                pause: enemy.pause,
            },
        );
    }
}

// --- Enemy reset -------------------------------------------------------------

/// START replays the run: drop all progress and **reload the current zone**, so
/// every enemy — including ones skipped-at-spawn because they were captured —
/// comes back un-armed at its patrol start. The persistent avatar stays put.
#[allow(clippy::too_many_arguments)]
fn reset_enemy(
    input: Res<ButtonInput<DsButton>>,
    mut pending: ResMut<menu::PendingReset>,
    mut tally: ResMut<capture::CaptureTally>,
    mut health: ResMut<Health>,
    mut flags: ResMut<flags::Flags>,
    mut level: ResMut<flags::LevelProgress>,
    mut snapshot: ResMut<ZoneCaptureState>,
    mut commands: Commands,
    mut zone: ResMut<Zone>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
    mut loco: ResMut<Locomotion>,
    mut landmarks: ResMut<Landmarks>,
    despawnable: Query<
        Entity,
        Or<(
            With<SceneInstance>,
            With<SpaceFloor>,
            With<NeighbourInstance>,
            With<GateBarrier>,
        )>,
    >,
) {
    // START, or the options-menu Reset item (which closes the menu and latches
    // this so the reset runs here, in the Playing-gated chain).
    if !input.just_pressed(DsButton::Start) && !pending.0 {
        return;
    }
    pending.0 = false;
    *tally = capture::CaptureTally::default();
    *health = Health::default();
    // Drop raised flags + level progress so gated arenas re-lock and the level
    // objective resets, and clear the capture snapshot so the reload respawns
    // every enemy un-captured. Then rebuild the current zone from its `.scene`.
    flags.clear();
    level.reset();
    snapshot.0.clear();
    transition::reload_zone(
        &snapshot,
        &flags,
        &mut zone,
        &mut commands,
        &mut device,
        &mut stroke,
        &mut loco,
        &mut landmarks,
        &despawnable,
    );
}

/// The enemy lobs a projectile at the avatar while deployed (the dodge threat),
/// reusing the pooled [`Projectile`] when it's free. Deploying resets the fire
/// cadence to a full interval so the enemy can't fire the instant you pull the
/// stylus — you get a beat to orient (#26 feel pass).
fn fire_projectile(
    state: Res<PlayerState>,
    mut fire: ResMut<EnemyFire>,
    mut was_deployed: Local<bool>,
    avatar: Query<&WorldPos, With<Avatar>>,
    // Only the *active* zone's enemies shoot — a resident neighbour (across the
    // seam) patrols + is capturable but doesn't fire until you enter its zone
    // (#27 follow-up); also avoids firing from an arbitrary off-screen neighbour.
    enemy: Query<(&WorldPos, &capture::Capture), (With<Enemy>, Without<NeighbourInstance>)>,
    mut proj: Query<(&mut Projectile, &mut WorldPos), (Without<Avatar>, Without<Enemy>)>,
) {
    fire.cd = fire.cd.saturating_sub(1);
    // Rising edge of deploy → grant the orient grace.
    let deployed = state.is_deployed();
    if deployed && !*was_deployed {
        fire.cd = FIRE_INTERVAL;
    }
    *was_deployed = deployed;
    let (Some(a), Some((e, cap))) = (avatar.iter().next(), enemy.iter().next()) else {
        return;
    };
    if !deployed || cap.is_resolved() || fire.cd > 0 {
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

/// Fly active projectiles; a hit costs a hit point (unless mid-roll i-frames);
/// out-of-bounds despawns. Inactive ones are Hidden (free).
fn move_projectile(
    time: Res<Time>,
    motion: Res<Motion>,
    mut state: ResMut<PlayerState>,
    mut stroke: ResMut<Stroke>,
    mut health: ResMut<Health>,
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
            // A projectile hit chips health; capture progress is kept, and you
            // stay deployed unless it downs you (#26 feel pass).
            p.active = false;
            capture::damage(&mut health, &mut state, &mut stroke);
        }
    }
}

/// Enemy walks its authored patrol loop (the `ScenePath` from the space; frozen
/// once captured).
fn patrol_enemy(
    time: Res<Time>,
    avatar: Query<&WorldPos, With<Avatar>>,
    mut q: Query<(&mut Enemy, &capture::Capture, &mut WorldPos, &ScenePath), Without<Avatar>>,
) {
    let step = Fx32::from_f32(ENEMY_SPEED * time.delta_secs());
    let apos = avatar.iter().next().map(|w| w.0);
    let range = Fx32::from_f32(PROCESS_RANGE);
    for (mut enemy, cap, mut pos, path) in &mut q {
        if cap.is_resolved() || path.0.is_empty() {
            continue;
        }
        // Process-range cull (#27 follow-up): a resident-neighbour enemy deep in
        // its zone (off-screen behind the fog) freezes rather than ticking. Active
        // enemies sit near the avatar, so they always run.
        if apos.is_some_and(|a| (pos.0 - a).length() > range) {
            continue;
        }
        if enemy.pause > 0 {
            enemy.pause -= 1; // dwelling at a waypoint — a capture window
            continue;
        }
        let wp = path.0[enemy.wp % path.0.len()];
        let target = FxVec2::from_f32(wp.x, wp.y);
        let to = target - pos.0;
        if to.length() <= step {
            pos.0 = target;
            enemy.wp = (enemy.wp + 1) % path.0.len();
            enemy.pause = ENEMY_PAUSE;
        } else {
            pos.0 = pos.0 + to.normalize_or_zero() * step;
        }
    }
}

// --- Capture -----------------------------------------------------------------
// The capture model itself (loop → progress → liberate, the dash-destroy exit,
// and enemy-contact knockout) lives in `capture` (#26).

/// The forced retract a hit causes: drop to Stowed and abandon the in-flight
/// stroke. Shared by body contact and projectile hits.
fn knock_device_offline(state: &mut PlayerState, stroke: &mut Stroke) {
    *state = PlayerState::Stowed;
    stroke.0.clear();
}

// --- Camera director (#23 / #27) ---------------------------------------------

/// A small offset off straight-down so a top-down view never looks *exactly*
/// along -Y (degenerate orientation); pushes the look-at just off the avatar.
const TD_EPS: f32 = 0.001;

/// An in-flight **camera warp**: a timed ease-in-out + slerp blend from the
/// camera's pose at the crossing to the new zone's framing (#27 seamless
/// streaming). `t` runs 0→1 (≥1 = idle); `from_*` is the camera's **actual**
/// pose captured at the crossing (after the re-base), *including* its follow
/// lag — so `s = 0` reproduces exactly where the camera already is (no snap),
/// and the blend eases (smoothstep) + slerps the offset around the avatar (it
/// arcs) toward the live target framing.
#[derive(Resource, Clone, Copy)]
struct CamWarp {
    t: f32,
    from_pos: Vec3,
    from_pitch: f32,
    from_yaw: f32,
}

impl Default for CamWarp {
    fn default() -> Self {
        // Idle (t ≥ 1); `from_*` is unused until a crossing captures it.
        Self {
            t: 1.0,
            from_pos: Vec3::ZERO,
            from_pitch: 0.0,
            from_yaw: 0.0,
        }
    }
}

/// Cubic ease-in-out (smoothstep): 0→0, 1→1, flat slope at both ends.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Spherical interpolation of `a`→`b`: **rotates** the direction along the
/// shorter arc while interpolating the magnitude linearly, so a camera offset
/// swings around the avatar instead of sliding through it. Falls back to a plain
/// `lerp` when the two are near-parallel or degenerate (where slerp is unstable
/// and visually identical anyway).
fn slerp_vec3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    let (la, lb) = (a.length(), b.length());
    if la < 1e-4 || lb < 1e-4 {
        return a.lerp(b, t);
    }
    let mag = la + (lb - la) * t;
    let (na, nb) = (a / la, b / lb);
    let dot = na.dot(nb).clamp(-1.0, 1.0);
    if dot > 0.9995 {
        return a.lerp(b, t);
    }
    let theta = libm::acosf(dot) * t;
    let rel = (nb - na * dot).normalize_or_zero();
    (na * libm::cosf(theta) + rel * libm::sinf(theta)) * mag
}

/// Drive the top-screen camera from the **space's authored base mode**
/// ([`LoadedScene`] — Follow / TopDown / Rail2.5D / CaptureFraming), with the
/// player's cluster-▲ top-down toggle layered on top (#17 control model) and a
/// **locked frame while deployed** (CaptureFraming pressure). The framing params
/// are authored data now — no hardcoded camera mode.
fn drive_camera(
    state: Res<PlayerState>,
    input: Res<ButtonInput<DsButton>>,
    handed: Res<Handedness>,
    scene: Option<Res<LoadedScene>>,
    mut topdown: ResMut<TopDownToggle>,
    orbit: Res<Orbit>,
    mut warp: ResMut<CamWarp>,
    mut camera: ResMut<Camera3d>,
    avatar: Query<&WorldPos, With<Avatar>>,
) {
    // Player top-down override (only while stowed — deploying locks the frame).
    if !state.is_deployed() && control::just_pressed(control::Action::CamTopDown, *handed, &input) {
        topdown.0 = !topdown.0;
    }
    // While deployed, drive the canonical **CaptureFraming** (fixed at the world
    // origin, yaw 0) regardless of the stowed framing (#26). The tactical map
    // plots absolute world positions centred on the origin, so this fixed,
    // origin-centred, yaw-0 view is exactly 1:1 with it — draw/dodge directions
    // read the same on both screens, even if you orbit-set a yaw before
    // deploying. Ease position + pitch so pulling the pen reframes smoothly.
    if state.is_deployed() {
        let (target, pitch, yaw) = frame_for(CameraMode::CaptureFraming, 0.0, 0.0, 0.0);
        camera.position = camera.position.lerp(target, CAM_SMOOTH);
        camera.pitch += (pitch - camera.pitch) * CAM_TURN_SMOOTH;
        camera.yaw = yaw;
        return;
    }
    let Some(a) = avatar.iter().next() else {
        return;
    };
    let (ax, az) = (a.0.x.to_f32(), a.0.y.to_f32());

    // The space's authored framing (a soft-follow default if no space loaded).
    let base = scene.map(|s| s.0.camera).unwrap_or(CameraMode::Follow {
        height: CAM_HEIGHT,
        dist: CAM_DIST,
        pitch: CAM_PITCH,
    });
    // The live orbit angle previews while dragging, else the locked offset.
    let orbit_yaw = orbit.live.unwrap_or(orbit.yaw);
    // The player's top-down toggle overrides whatever the space authored.
    let (target, pitch, yaw) = if topdown.0 {
        (
            Vec3::new(ax, CAM_TD_HEIGHT, az + TD_EPS),
            -core::f32::consts::FRAC_PI_2,
            0.0,
        )
    } else {
        frame_for(base, ax, az, orbit_yaw)
    };

    if !topdown.0 && warp.t < 1.0 {
        // Zone-transition warp: ease-in-out + slerp from the camera's **captured
        // crossing pose** to the new zone's framing. `from_*` is the actual pose
        // at the crossing (re-based, lag included), so at `s = 0` this reproduces
        // exactly where the camera is — no snap, no lag discarded. The offset
        // around the avatar is slerped (it arcs) and `s` is smoothstepped (no
        // abrupt start/stop); the target is the live framing, so the camera tracks
        // the avatar increasingly as the warp completes.
        let ground = Vec3::new(ax, 0.0, az);
        let s = smoothstep(warp.t);
        camera.position = ground + slerp_vec3(warp.from_pos - ground, target - ground, s);
        camera.pitch = warp.from_pitch + (pitch - warp.from_pitch) * s;
        camera.yaw = warp.from_yaw + (yaw - warp.from_yaw) * s;
        warp.t += CAM_WARP_STEP;
    } else {
        // Steady state: soft-follow the position; ease pitch (yaw stays instant so
        // OrbitSet drag is responsive). Also smooths the top-down toggle's tilt.
        camera.position = camera.position.lerp(target, CAM_SMOOTH);
        camera.pitch += (pitch - camera.pitch) * CAM_TURN_SMOOTH;
        camera.yaw = yaw;
    }
}

/// Resolve an authored [`CameraMode`] + the avatar's ground position `(ax, az)`
/// into a camera `(position, pitch, yaw)`. The single source of truth for how
/// each authored framing maps to the hardware camera (used by both the per-frame
/// director and the boot-time seed in `setup`). `orbit_yaw` is the OrbitSet angle
/// (radians) applied to the Follow framing; 0 = the default (camera behind).
fn frame_for(mode: CameraMode, ax: f32, az: f32, orbit_yaw: f32) -> (Vec3, f32, f32) {
    match mode {
        // Soft 3/4 follow (open arenas): camera trails the avatar at distance
        // `dist`, on a circle the player can orbit with the pen (OrbitSet). At
        // yaw 0 it sits directly behind (+Z); a yaw θ rotates that offset around
        // the avatar (offset = (sinθ, cosθ)·dist), and the camera yaw matches so
        // it keeps looking at the avatar — a chosen angle, never a free camera.
        CameraMode::Follow {
            height,
            dist,
            pitch,
        } => {
            let (s, c) = (libm::sinf(orbit_yaw), libm::cosf(orbit_yaw));
            (
                Vec3::new(ax + dist * s, height, az + dist * c),
                pitch,
                orbit_yaw,
            )
        }
        // Straight-down tactical framing.
        CameraMode::TopDown { height } => (
            Vec3::new(ax, height, az + TD_EPS),
            -core::f32::consts::FRAC_PI_2,
            0.0,
        ),
        // Side-on **rail** for 2.5D corridors. The corridor runs along world X;
        // the camera tracks the avatar's X but its depth is **locked** to the
        // rail (fixed Z = `dist`), so the avatar's depth excursions never move
        // the camera — "can't fall the wrong way". `pitch` tilts it down onto
        // the floor; yaw stays 0 (looking into -Z), so the corridor reads as a
        // flat side-on plane.
        CameraMode::Rail2_5D {
            height,
            dist,
            pitch,
        } => (Vec3::new(ax, height, dist), pitch, 0.0),
        // Capture-framing: a fixed elevated frame on the arena origin that does
        // **not** track the avatar — the camera holds while you draw + dodge.
        CameraMode::CaptureFraming => (Vec3::new(0.0, CAM_HEIGHT, CAM_DIST), CAM_PITCH, 0.0),
    }
}

/// Radians of camera yaw per screen-pixel of stylus drag (a full 256-px sweep ≈
/// 165°, enough to swing the arena well past side-on).
const ORBIT_SENS: f32 = 0.0113;
/// Stylus travel (px) before a ◄-hold counts as a *drag* (set the angle) rather
/// than a *tap* (reset to default) — debounces a jittery touch on a bare tap.
const ORBIT_DRAG_MIN: f32 = 6.0;

/// OrbitSet (#23): while stowed in a Follow space, holding cluster ◄
/// ([`control::Action::CamOrbit`]) and dragging the stylus chooses the yaw the
/// camera orbits the avatar at (`drive_camera`/`frame_for` apply it). Releasing
/// after a drag **locks** the angle; releasing a bare tap **resets** to default.
/// Locomotion is suppressed during the hold (`player::move_player`), so the same
/// drag doesn't also move the avatar — the pen is borrowed for the camera.
fn orbit_camera(
    state: Res<PlayerState>,
    input: Res<ButtonInput<DsButton>>,
    handed: Res<Handedness>,
    touches: Res<Touches>,
    scene: Option<Res<LoadedScene>>,
    mut orbit: ResMut<Orbit>,
) {
    // Only meaningful for the open-arena Follow base, and only while stowed
    // (deploying locks the frame). Otherwise the ◄ input is inert here.
    let follow_base = scene
        .map(|s| matches!(s.0.camera, CameraMode::Follow { .. }))
        .unwrap_or(true);
    let held = !state.is_deployed()
        && follow_base
        && control::pressed(control::Action::CamOrbit, *handed, &input);

    if held {
        // Track the stylus drag → a live yaw the camera previews.
        if let Some(touch) = touches.iter().next() {
            let x = touch.position().x;
            match orbit.anchor {
                None => {
                    orbit.anchor = Some(x);
                    orbit.base = orbit.yaw; // build on the currently locked angle
                }
                Some(a) => {
                    let dx = x - a;
                    if dx.abs() >= ORBIT_DRAG_MIN {
                        orbit.dragged = true;
                    }
                    orbit.live = Some(orbit.base + dx * ORBIT_SENS);
                }
            }
        }
        orbit.holding = true;
    } else {
        // The frame ◄ is released: resolve the gesture (a drag locks the chosen
        // angle; a bare tap resets to the default framing), then clear transients.
        if orbit.holding {
            if orbit.dragged {
                if let Some(live) = orbit.live {
                    orbit.yaw = live;
                }
            } else {
                orbit.yaw = 0.0;
            }
        }
        orbit.holding = false;
        orbit.anchor = None;
        orbit.live = None;
        orbit.dragged = false;
    }
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
        Option<&capture::Capture>,
        Option<&Height>,
        Has<Shadow>,
        Has<Hidden>,
    )>,
) {
    for (e, pos, mut t, cap, height, is_shadow, hidden) in &mut q {
        // Y-up world: the 2D ground `WorldPos(x, y)` lands on the XZ plane. The
        // shadow rides the floor (`GROUND_Y`); other objects render centred at
        // Y=0 (mesh-centred, so they rest on the floor); the avatar lifts on +Y.
        t.translation.x = pos.0.x.to_f32();
        t.translation.y = if is_shadow {
            GROUND_Y
        } else {
            height.map_or(0.0, |h| h.z.to_f32())
        };
        t.translation.z = pos.0.y.to_f32();
        if let Some(cap) = cap {
            match (cap.is_resolved(), hidden) {
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
    mut q: Query<(&WorldPos, &mut Sprite, Option<&capture::Capture>)>,
) {
    for (pos, mut sprite, cap) in &mut q {
        let hide = !state.is_deployed() || cap.is_some_and(|c| c.is_resolved());
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
        densify(
            &path_smooth(&stroke.0),
            Fx32::from_f32(TRAIL_STEP),
            DOT_POOL,
        )
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

/// Draw the radial-wheel placeholder overlay while it's open (#25): the 5 spoke
/// icons laid out around the wheel origin (a point-up pentagon), plus a pointer
/// line of dots from the wheel centre to the live stylus position. Everything
/// parks off-screen when the wheel is closed. The spoke layout comes from the
/// same `bevy_nds_math::radial` geometry the picker uses, so the drawn wheel and
/// the selected spoke can't disagree.
fn update_radial_overlay(
    radial: Res<radial::Radial>,
    touches: Res<Touches>,
    mut spokes: Query<(&RadialSpoke, &mut Sprite), (With<RadialSpoke>, Without<RadialLine>)>,
    mut line: Query<&mut Sprite, (With<RadialLine>, Without<RadialSpoke>)>,
) {
    let origin = if radial.open { radial.origin } else { None };
    // The spoke currently under the pen — pops out + swaps to a highlight icon so
    // you can see what a release would confirm before letting go (#25).
    let hovered = radial.preview.map(|s| s.index());

    // Spoke icons around the wheel origin (16×16, so centre with a -8 offset).
    for (spoke, mut s) in &mut spokes {
        if let Some(o) = origin {
            let dir = bevy_nds_math::radial::spoke_dir(spoke.0);
            let is_hovered = hovered == Some(spoke.0);
            let radius = if is_hovered {
                RADIAL_RADIUS + RADIAL_HOVER_POP
            } else {
                RADIAL_RADIUS
            };
            s.image = if is_hovered {
                sprites::BLIP_HIT
            } else {
                sprites::OBSTACLE
            };
            s.x = (o.x.to_f32() + dir.x.to_f32() * radius) as i16 - 8;
            s.y = (o.y.to_f32() + dir.y.to_f32() * radius) as i16 - 8;
        } else {
            s.y = PARK_Y;
        }
    }

    // Pointer line: dots evenly spaced from the wheel centre toward the stylus
    // (8×8, -4 offset). Parks when the wheel is closed or the pen is up.
    let cur = touches
        .iter()
        .next()
        .map(|t| (t.position().x, t.position().y));
    for (i, mut s) in line.iter_mut().enumerate() {
        match (origin, cur) {
            (Some(o), Some((tx, ty))) => {
                let f = (i as f32 + 1.0) / (RADIAL_LINE_DOTS as f32 + 1.0);
                s.x = (o.x.to_f32() + (tx - o.x.to_f32()) * f) as i16 - 4;
                s.y = (o.y.to_f32() + (ty - o.y.to_f32()) * f) as i16 - 4;
            }
            _ => s.y = PARK_Y,
        }
    }
}

fn update_hud(
    pstate: Res<PlayerState>,
    health: Res<Health>,
    tally: Res<capture::CaptureTally>,
    radial: Res<radial::Radial>,
    zone: Res<Zone>,
    flags: Res<flags::Flags>,
    level: Res<flags::LevelProgress>,
    caps: Query<&capture::Capture>,
    mut hud: Query<(&mut DsText, Has<InfoHud>), Or<(With<InfoHud>, With<TallyHud>)>>,
) {
    use capture::CaptureOutcome;
    let cap = caps.iter().next();
    for (mut text, is_status) in &mut hud {
        text.0.clear();
        if !is_status {
            // The tally line: health + how the two exits have played out (#32),
            // plus the compact progression state — zone gate + level exit — kept
            // within the 32-col console grid.
            let _ = write!(
                text.0,
                "hp {}/{} lib {} des {}",
                health.hp, health.max, tally.liberated, tally.destroyed
            );
            // Zone-gate hint (#27 tier 1): whether this zone's arena-trap gate is
            // open yet (`g:LK`/`g:OP`).
            if let Some(gate) = zone.conns.iter().map(|c| c.gate).find(|&g| g != 0) {
                let _ = write!(
                    text.0,
                    " g:{}",
                    if flags.is_raised(gate) { "OP" } else { "LK" }
                );
            }
            // Level-objective hint (#27 tier 2): progress toward the level exit,
            // or `EXIT` once the level objective is met.
            if level.needed > 0 {
                if level.complete() {
                    let _ = write!(text.0, " EXIT");
                } else {
                    let _ = write!(text.0, " lv{}/{}", level.done(), level.needed);
                }
            }
            continue;
        }
        // While the wheel is open, the status line previews the spoke under the
        // pen (#25) — a text stand-in until the graphical overlay lands (the
        // "which screen" feel question is still open on #25).
        if radial.open {
            match radial.preview {
                Some(spoke) => {
                    let _ = write!(text.0, "RADIAL  {}", spoke.label());
                }
                None => {
                    let _ = write!(text.0, "RADIAL  cancel");
                }
            }
            continue;
        }
        if health.is_downed() {
            let _ = write!(text.0, "DOWNED     START to reset");
            continue;
        }
        match cap.and_then(|c| c.resolved) {
            Some(CaptureOutcome::Liberated) => {
                let _ = write!(text.0, "LIBERATED!  START to re-arm");
            }
            Some(CaptureOutcome::Destroyed) => {
                let _ = write!(text.0, "DESTROYED.  START to re-arm");
            }
            None => {
                let pct = (cap.map_or(0.0, |c| c.progress) * 100.0) as i32;
                if cap.is_some_and(|c| c.is_breakable()) {
                    // Past the threshold: advertise the expedient dash exit.
                    let _ = write!(text.0, "BREAKABLE {pct:>3}%  dash=destroy");
                } else {
                    let label = if pstate.is_deployed() {
                        "DEPLOYED"
                    } else {
                        "stowed "
                    };
                    let _ = write!(text.0, "{label}  capture {pct:>3}%");
                }
            }
        }
    }
}
