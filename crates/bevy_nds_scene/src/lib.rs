//! `bevy_nds_scene` — load baked `.scene` *space* blobs from NitroFS and spawn
//! them as ECS entities.
//!
//! A "space" (issue #27) is the unit of the level graph. This crate is the
//! **runtime** half of the pipeline:
//!
//! ```text
//! assets/spaces/*.ron  ──scene2bin──▶  build/nitrofs/spaces/*.scene  ──┐
//!                       (host baker)                                    │
//!                                                       this crate ◀────┘
//!                                          load() → parse() → spawn()
//! ```
//!
//! It is deliberately **game-agnostic**: it spawns each authored instance as a
//! rendered mesh entity tagged with a [`SceneInstance`] carrying an *opaque*
//! `role` string (and any authored [`ScenePath`]). The game watches for those
//! and attaches its own behaviour by role (`"enemy"` → its `Enemy` component,
//! etc.), so engine code never learns game-specific names. The parsed data is
//! also kept in a [`LoadedScene`] resource for graph-level needs (camera mode,
//! exits).
//!
//! It adds no allocator / panic handler; it composes [`bevy_nds_nitrofs`]
//! (bytes) and [`bevy_nds_3d`] (meshes, transforms, materials).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod asset;

pub use asset::{CameraMode, SceneConnData, SceneData, SceneInstanceData, parse};

use alloc::string::String;
use alloc::vec::Vec;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_nds_3d::prelude::{DsMaterial, DsMesh, Transform3d, Vec3};

/// Marker + metadata on every entity spawned from a scene instance. The game
/// queries this (e.g. with an `Added<SceneInstance>` filter) and specialises by
/// `role` — the bridge between the game-agnostic loader and game components.
#[derive(Component, Clone, Debug)]
pub struct SceneInstance {
    /// The opaque authored role tag (`"avatar"`, `"enemy"`, `"landmark"`, …).
    pub role: String,
    /// Opaque per-instance flags (game-defined; see [`SceneInstanceData`]).
    pub flags: u32,
}

/// Ground-plane (XZ) waypoints authored on an instance (an enemy patrol path, a
/// rail). Only present when the instance declared a non-empty path.
#[derive(Component, Clone, Debug, Default)]
pub struct ScenePath(pub Vec<Vec2>);

/// The most recently loaded space, kept for graph-level reads (camera framing,
/// exits to neighbouring spaces). Inserted by [`spawn`].
#[derive(Resource, Clone)]
pub struct LoadedScene(pub SceneData);

/// Read + parse a `.scene` blob from NitroFS. `path` is a NUL-terminated
/// `nitro:/` path (e.g. `b"nitro:/spaces/atrium.scene\0"`). Mirrors
/// [`DsMesh::load`] / `bevy_nds_sprite::asset::load`. Returns `None` if the
/// filesystem isn't mounted, the file is missing, or the blob is invalid.
pub fn load(path: &[u8]) -> Option<SceneData> {
    let bytes = bevy_nds_nitrofs::read_file(path)?;
    asset::parse(&bytes)
}

/// Spawn every instance in `scene` as a rendered entity (mesh + [`Transform3d`]
/// + optional [`DsMaterial`]) tagged with a [`SceneInstance`] (and a
/// [`ScenePath`] when authored), and stash the scene in [`LoadedScene`] for the
/// game's camera director / transition logic to read.
///
/// Game-agnostic: it does **not** attach gameplay components — the game does
/// that by `role` (see crate docs). Missing meshes leave a transform-only
/// entity (still tagged) rather than failing the whole load.
pub fn spawn(commands: &mut Commands, scene: SceneData) {
    for inst in &scene.instances {
        let mut e = commands.spawn((
            Transform3d {
                translation: Vec3::from_array(inst.pos),
                rotation: Vec3::from_array(inst.rot),
                scale: Vec3::from_array(inst.scale),
            },
            SceneInstance {
                role: inst.role.clone(),
                flags: inst.flags,
            },
        ));
        if let Some(name) = &inst.mesh {
            if let Some(mesh) = load_mesh(name) {
                e.insert(mesh);
            }
        }
        if let Some((diffuse, ambient)) = inst.material {
            e.insert(DsMaterial { diffuse, ambient });
        }
        if !inst.path.is_empty() {
            let pts = inst.path.iter().map(|p| Vec2::new(p[0], p[1])).collect();
            e.insert(ScenePath(pts));
        }
    }
    commands.insert_resource(LoadedScene(scene));
}

/// Resolve a bare mesh name to its NitroFS `.dl` and load it: `"teapot"` →
/// `nitro:/teapot.dl`. The baked `.dl` files come from the same `obj2dl` build
/// step the rest of the engine uses, so geometry is identical to `include_obj!`.
/// Public so a game can spawn extra (e.g. resident-neighbour) render entities
/// from a mesh name without re-deriving the path.
pub fn load_mesh(name: &str) -> Option<DsMesh> {
    const PREFIX: &[u8] = b"nitro:/";
    const SUFFIX: &[u8] = b".dl\0";
    let mut path = Vec::with_capacity(PREFIX.len() + name.len() + SUFFIX.len());
    path.extend_from_slice(PREFIX);
    path.extend_from_slice(name.as_bytes());
    path.extend_from_slice(SUFFIX);
    DsMesh::load(&path)
}

/// Build the NUL-terminated NitroFS path for a zone within a level, from the
/// level directory + the zone's bare stem:
/// `("facility", "corridor")` → `b"nitro:/levels/facility/corridor.scene\0"`.
/// A connection's `neighbour` is a bare zone stem in the *same* level, so the
/// runtime resolves it with the current level dir. Mirrors `scene2bin`'s
/// `NITROFS_SUBDIR` (`levels`) + `ASSET_EXT` (`scene`) — keep the two in sync.
pub fn level_space_path(level: &str, zone: &str) -> Vec<u8> {
    const PREFIX: &[u8] = b"nitro:/levels/";
    const SUFFIX: &[u8] = b".scene\0";
    let mut path =
        Vec::with_capacity(PREFIX.len() + level.len() + 1 + zone.len() + SUFFIX.len());
    path.extend_from_slice(PREFIX);
    path.extend_from_slice(level.as_bytes());
    path.push(b'/');
    path.extend_from_slice(zone.as_bytes());
    path.extend_from_slice(SUFFIX);
    path
}

/// Sent to request loading a zone at runtime (a graph transition). For the
/// startup case, call [`load`] + [`spawn`] directly. `path` is a NUL-terminated
/// `nitro:/` path (see [`level_space_path`]).
#[derive(Event, Clone)]
pub struct LoadSpace {
    pub path: Vec<u8>,
}

impl LoadSpace {
    /// Request a neighbour zone by its stem within a level
    /// (see [`level_space_path`]).
    pub fn by_name(level: &str, zone: &str) -> Self {
        Self { path: level_space_path(level, zone) }
    }
}

/// Drains [`LoadSpace`] events: loads + spawns each requested space. The game is
/// responsible for despawning the previous space's entities (only the current
/// space renders — #27); this crate intentionally doesn't guess what to clear.
fn handle_load_space(mut commands: Commands, mut events: EventReader<LoadSpace>) {
    for ev in events.read() {
        if let Some(scene) = load(&ev.path) {
            spawn(&mut commands, scene);
        }
    }
}

/// Registers the [`LoadSpace`] event flow. The loader's free functions
/// ([`load`] / [`spawn`]) work without it, but adding the plugin lets the game
/// drive graph transitions by sending [`LoadSpace`].
pub struct ScenePlugin;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<LoadSpace>()
            .add_systems(Update, handle_load_space);
    }
}

pub mod prelude {
    pub use crate::{
        CameraMode, LoadSpace, LoadedScene, ScenePath, ScenePlugin, SceneConnData,
        SceneData, SceneInstance, SceneInstanceData, level_space_path,
    };
}

#[cfg(test)]
mod tests {
    use super::level_space_path;

    #[test]
    fn level_space_path_builds_nul_terminated_nitro_path() {
        assert_eq!(
            level_space_path("facility", "corridor"),
            b"nitro:/levels/facility/corridor.scene\0"
        );
        assert_eq!(
            level_space_path("facility", "atrium"),
            b"nitro:/levels/facility/atrium.scene\0"
        );
        assert_eq!(level_space_path("facility", "corridor").last(), Some(&0));
    }
}
