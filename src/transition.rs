//! Euclidean zone streaming (#27).
//!
//! The world is **one global map** split into loadable zones. Each zone is
//! authored in its own local (origin-centric) frame but placed in the shared map
//! frame; the host baker (`scene2bin`) derives, from which zones abut, each
//! zone's **boundary connections** — the neighbour, the edge, and the `delta` to
//! add to the avatar's local position when it crosses, so its *global* position
//! is unchanged.
//!
//! At runtime the avatar walks in the current zone's local coords, clamped to
//! its [`Zone::bounds`] (so the tactical map / camera / capture math all stay
//! origin-centric per zone). Reaching a connected edge ([`transition_spaces`])
//! loads the neighbour and adds the connection's `delta` — a continuous step
//! across the seam, no teleport and no direction flip. This replaces the old
//! hand-matched exits + per-edge continuity hacks.

use alloc::vec::Vec;

use alloc::string::String;
use bevy_ecs::prelude::*;

use bevy_nds_3d::prelude::Camera3d;
use bevy_nds_scene::{SceneConnData, SceneData, SceneInstance, level_space_path};

use bevy_nds_math::FxVec2;

use crate::player::{Locomotion, PlayerState};
use crate::{
    Avatar, CamWarp, Device, Landmarks, NeighbourInstance, SpaceFloor, Stroke, WorldPos,
    spawn_resident_neighbours, spawn_zone_floor,
};

/// How close (world units) to a connected edge counts as crossing it. Kept
/// **tight**: the avatar is clamped to its bounds, so it presses right up to the
/// edge — and with the neighbour zone resident + visible (#27 seamless
/// streaming), crossing only when essentially *at* the seam avoids the
/// `EDGE_EPS`-sized teleport that a wider trigger caused (the avatar would re-base
/// past the neighbour's edge and get clamped back the next frame — a visible
/// snap once everything else is continuous). Small enough to be invisible, large
/// enough to absorb fixed-point rounding on the bounds.
const EDGE_EPS: f32 = 0.01;

/// The current zone's walkable bounds + derived boundary connections. Drives the
/// avatar clamp ([`crate::player::move_player`]) and boundary crossing
/// ([`transition_spaces`]). Set from the loaded [`SceneData`] on boot and on
/// every crossing.
#[derive(Resource)]
pub struct Zone {
    /// The current level's directory stem. A connection's `neighbour` is a bare
    /// zone stem within this same level, resolved with [`level_space_path`].
    /// Set once at boot (intra-level streaming keeps it constant; a level-exit
    /// seam that changes it is deferred — see #27).
    pub level: String,
    /// `[min_x, min_z, max_x, max_z]` in local coords.
    pub bounds: [f32; 4],
    /// Derived connections to neighbouring zones.
    pub conns: Vec<SceneConnData>,
}

impl Default for Zone {
    fn default() -> Self {
        // Effectively unclamped until the first zone loads (so a missing zone
        // never traps the avatar at the origin).
        Self {
            level: String::new(),
            bounds: [-1.0e6, -1.0e6, 1.0e6, 1.0e6],
            conns: Vec::new(),
        }
    }
}

impl Zone {
    /// Adopt a freshly loaded zone's bounds + connections (the level is constant
    /// across an intra-level swap, so it's left untouched).
    pub fn set(&mut self, scene: &SceneData) {
        self.bounds = scene.bounds;
        self.conns = scene.connections.clone();
    }
}

/// Re-arm guard: a boundary can only fire once the avatar has been clear of
/// *every* connected edge since the last load, so arriving on the neighbour's
/// return edge doesn't bounce straight back.
#[derive(Resource)]
pub struct Transition {
    armed: bool,
}

impl Default for Transition {
    fn default() -> Self {
        Self { armed: true }
    }
}

/// Is the avatar (local `px`,`pz`) at the connected edge `c` of `bounds`?
/// `[lo, hi]` spans the boundary along the edge: Z for east/west edges, X for
/// north/south.
fn at_edge(px: f32, pz: f32, bounds: &[f32; 4], c: &SceneConnData) -> bool {
    let [min_x, min_z, max_x, max_z] = *bounds;
    match c.side {
        0 => px <= min_x + EDGE_EPS && (c.lo..=c.hi).contains(&pz), // west −X
        1 => px >= max_x - EDGE_EPS && (c.lo..=c.hi).contains(&pz), // east +X
        2 => pz <= min_z + EDGE_EPS && (c.lo..=c.hi).contains(&px), // south −Z
        3 => pz >= max_z - EDGE_EPS && (c.lo..=c.hi).contains(&px), // north +Z
        _ => false,
    }
}

/// Boundary-crossing detection + zone swap. While stowed, when the avatar
/// reaches an open connected edge, stream the neighbour in and carry the avatar
/// across continuously. Deploying the capture device suppresses it — you don't
/// leave a zone mid-capture.
#[allow(clippy::too_many_arguments)]
pub fn transition_spaces(
    state: Res<PlayerState>,
    mut avatar: Query<&mut WorldPos, With<Avatar>>,
    mut tr: ResMut<Transition>,
    mut zone: ResMut<Zone>,
    mut commands: Commands,
    mut warp: ResMut<CamWarp>,
    mut camera: ResMut<Camera3d>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
    mut loco: ResMut<Locomotion>,
    mut landmarks: ResMut<Landmarks>,
    instances: Query<Entity, With<SceneInstance>>,
    floors: Query<Entity, With<SpaceFloor>>,
    neighbours: Query<Entity, With<NeighbourInstance>>,
) {
    if state.is_deployed() {
        return;
    }
    let Some((px, pz)) = avatar
        .iter()
        .next()
        .map(|a| (a.0.x.to_f32(), a.0.y.to_f32()))
    else {
        return;
    };
    let bounds = zone.bounds;

    // The first open connected edge in range; `clear` tracks whether the avatar
    // is off *all* connected edges (so we can re-arm). Clone the connection so we
    // stop borrowing `zone` before the swap re-sets it.
    let mut crossing: Option<SceneConnData> = None;
    let mut clear = true;
    for c in &zone.conns {
        if at_edge(px, pz, &bounds, c) {
            clear = false;
            // gate 0 = always open; gated boundaries wait for objectives (#26).
            if c.gate == 0 && crossing.is_none() {
                crossing = Some(c.clone());
            }
        }
    }

    if clear {
        tr.armed = true;
        return;
    }
    let (true, Some(c)) = (tr.armed, crossing) else {
        return;
    };
    tr.armed = false;

    // Continuous position: the avatar's new local pos = the crossing point + the
    // baked delta into the neighbour's frame. **Snap the crossing axis to the
    // exact edge** first (the avatar is within `EDGE_EPS` of it): adding `delta`
    // then lands the avatar precisely on the neighbour's abutting edge, so it
    // never overshoots the neighbour's bounds and gets clamped back — no teleport.
    let [min_x, min_z, max_x, max_z] = bounds;
    let (cx, cz) = match c.side {
        0 => (min_x, pz), // west −X
        1 => (max_x, pz), // east +X
        2 => (px, min_z), // south −Z
        3 => (px, max_z), // north +Z
        _ => (px, pz),
    };
    let new_pos = [cx + c.delta[0], cz + c.delta[1]];
    let path = level_space_path(&zone.level, &c.neighbour);
    swap_zone(
        &path,
        &mut zone,
        &mut commands,
        &mut device,
        &mut stroke,
        &mut loco,
        &mut landmarks,
        &instances,
        &floors,
        &neighbours,
    );

    // The avatar is the single persistent entity — carry it across by the delta
    // (it isn't respawned with the new zone).
    if let Some(mut a) = avatar.iter_mut().next() {
        a.0 = FxVec2::from_f32(new_pos[0], new_pos[1]);
    }

    // Re-base the camera by the same delta so it keeps tracking the avatar across
    // the seam (everything co-shifts → the view is unchanged at the crossing
    // instant, and the follow lag is preserved rather than snapped away).
    camera.position.x += c.delta[0];
    camera.position.z += c.delta[1];

    // Capture that exact (re-based) pose as the warp start, so `drive_camera`
    // eases the framing change *from where the camera actually is* — no snap.
    warp.from_pos = camera.position;
    warp.from_pitch = camera.pitch;
    warp.from_yaw = camera.yaw;
    warp.t = 0.0;
}

/// Make the crossed-into neighbour the new active zone: despawn the old active
/// zone + the previous resident neighbours, spawn the new active zone (minus the
/// avatar) + *its* neighbours, and adopt its bounds + movement preset. The
/// camera is re-based + eased by the caller; the persistent avatar + runtime
/// chrome (shadow, cursor, projectile, trail-dot pool, HUD) carry no
/// [`SceneInstance`]/[`SpaceFloor`]/[`NeighbourInstance`], so they survive.
#[allow(clippy::too_many_arguments)]
fn swap_zone(
    path: &[u8],
    zone: &mut Zone,
    commands: &mut Commands,
    device: &mut Device,
    stroke: &mut Stroke,
    loco: &mut Locomotion,
    landmarks: &mut Landmarks,
    instances: &Query<Entity, With<SceneInstance>>,
    floors: &Query<Entity, With<SpaceFloor>>,
    neighbours: &Query<Entity, With<NeighbourInstance>>,
) {
    // Load the neighbour first — if it fails, bail without tearing down the zone
    // we're standing in (no blank screen).
    let Some(mut scene) = bevy_nds_scene::load(path) else {
        return;
    };

    // The avatar is the single persistent entity, carried across by the caller —
    // never re-spawned. Strip the new active zone's avatar instance (only the
    // entry zone authors one, but it would otherwise re-specialize a duplicate).
    scene.instances.retain(|i| i.role != "avatar");

    // Despawn the old active zone's instances, the previous resident neighbours,
    // and all floors (the persistent avatar + chrome carry none of these markers,
    // so they survive).
    for e in instances
        .iter()
        .chain(neighbours.iter())
        .chain(floors.iter())
    {
        commands.entity(e).despawn();
    }
    spawn_zone_floor(commands, scene.bounds, (0.0, 0.0)); // active floor (sized to bounds)

    // Per-zone state that mustn't carry over. (Capture progress is per-enemy
    // now — the old zone's enemies are despawned above, the new zone's spawn
    // with a fresh `Capture` — so only the device cooldown needs clearing.)
    landmarks.0.clear(); // `specialize_scene` re-harvests the new zone's set
    stroke.0.clear();
    device.hit_cd = 0;

    // Movement feel + walkable bounds follow the new zone.
    *loco = Locomotion::for_camera(scene.camera);
    zone.set(&scene);

    bevy_nds_scene::spawn(commands, scene); // new active zone (minus the avatar)
    // …and its neighbours become resident (render-only, fogged) in turn.
    spawn_resident_neighbours(commands, zone);
}
