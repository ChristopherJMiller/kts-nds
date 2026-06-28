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

use bevy_ecs::prelude::*;
use bevy_nds_3d::prelude::Camera3d;
use bevy_nds_scene::{SceneConnData, SceneData, SceneInstance, space_path};

use crate::player::{Locomotion, PlayerState};
use crate::{
    Avatar, ConnMarker, Device, Landmarks, SpaceFloor, Stroke, WorldPos, frame_for,
    spawn_connection_markers, spawn_space_floor,
};

/// How close (world units) to a connected edge counts as crossing it. The
/// avatar is clamped to the edge, so this just needs to cover the clamp gap.
const EDGE_EPS: f32 = 0.18;

/// The current zone's walkable bounds + derived boundary connections. Drives the
/// avatar clamp ([`crate::player::move_player`]) and boundary crossing
/// ([`transition_spaces`]). Set from the loaded [`SceneData`] on boot and on
/// every crossing.
#[derive(Resource)]
pub struct Zone {
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
            bounds: [-1.0e6, -1.0e6, 1.0e6, 1.0e6],
            conns: Vec::new(),
        }
    }
}

impl Zone {
    /// Adopt a freshly loaded zone's bounds + connections.
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
    avatar: Query<&WorldPos, With<Avatar>>,
    mut tr: ResMut<Transition>,
    mut zone: ResMut<Zone>,
    mut commands: Commands,
    mut camera: ResMut<Camera3d>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
    mut loco: ResMut<Locomotion>,
    mut landmarks: ResMut<Landmarks>,
    instances: Query<Entity, With<SceneInstance>>,
    floors: Query<Entity, With<SpaceFloor>>,
    markers: Query<Entity, With<ConnMarker>>,
) {
    if state.is_deployed() {
        return;
    }
    let Some(a) = avatar.iter().next() else {
        return;
    };
    let (px, pz) = (a.0.x.to_f32(), a.0.y.to_f32());
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

    // Continuous position: the avatar's new local pos = current + the baked delta
    // into the neighbour's frame.
    let new_pos = [px + c.delta[0], pz + c.delta[1]];
    swap_zone(
        &space_path(&c.neighbour),
        new_pos,
        &mut zone,
        &mut commands,
        &mut camera,
        &mut device,
        &mut stroke,
        &mut loco,
        &mut landmarks,
        &instances,
        &floors,
        &markers,
    );
}

/// Instant swap to the neighbour zone, placing the avatar at `new_pos` (the
/// crossing point in the neighbour's frame). Runtime chrome (shadow, cursor,
/// projectile, trail-dot pool, HUD) carries no [`SceneInstance`]/[`SpaceFloor`],
/// so it survives.
#[allow(clippy::too_many_arguments)]
fn swap_zone(
    path: &[u8],
    new_pos: [f32; 2],
    zone: &mut Zone,
    commands: &mut Commands,
    camera: &mut Camera3d,
    device: &mut Device,
    stroke: &mut Stroke,
    loco: &mut Locomotion,
    landmarks: &mut Landmarks,
    instances: &Query<Entity, With<SceneInstance>>,
    floors: &Query<Entity, With<SpaceFloor>>,
    markers: &Query<Entity, With<ConnMarker>>,
) {
    // Load the neighbour first — if it fails, bail without tearing down the zone
    // we're standing in (no blank screen).
    let Some(mut scene) = bevy_nds_scene::load(path) else {
        return;
    };

    // Place the avatar at the crossing point (continuous global position),
    // overriding the neighbour's authored avatar spawn.
    if let Some(av) = scene.instances.iter_mut().find(|i| i.role == "avatar") {
        av.pos[0] = new_pos[0];
        av.pos[2] = new_pos[1];
    }

    // Despawn the old zone's instances + floor + doorway markers.
    for e in instances.iter() {
        commands.entity(e).despawn();
    }
    for e in floors.iter() {
        commands.entity(e).despawn();
    }
    for e in markers.iter() {
        commands.entity(e).despawn();
    }
    spawn_space_floor(commands, scene.camera);
    spawn_connection_markers(commands, &scene); // new zone's doorway gateposts

    // Per-zone state that mustn't carry over.
    landmarks.0.clear(); // `specialize_scene` re-harvests the new zone's set
    stroke.0.clear();
    device.progress = 0.0;
    device.hit_cd = 0;

    // Movement feel + walkable bounds follow the new zone.
    *loco = Locomotion::for_camera(scene.camera);
    zone.set(&scene);

    // Seed the camera on the crossing point so we land on the right framing.
    let (cpos, pitch, yaw) = frame_for(scene.camera, new_pos[0], new_pos[1], 0.0);
    camera.position = cpos;
    camera.pitch = pitch;
    camera.yaw = yaw;

    bevy_nds_scene::spawn(commands, scene);
}
