//! The **one** authored-instance → gameplay-components dispatch (#27).
//!
//! `bevy_nds_scene` spawns render entities carrying an opaque `role` string, an
//! opaque `kind` byte and `flags`; this module is where those become the game's
//! components. Before this existed the mapping lived twice — once in
//! `specialize_scene` for the active zone, once inlined in `spawn_neighbour` for
//! resident neighbours — and the two drifted. Now both build an [`Authored`]
//! view, pick a [`Residency`], and call [`attach`].
//!
//! Two properties are deliberate and load-bearing:
//!
//! - **The `match a.role` in [`attach`] is exhaustive with no `_` arm.** Adding a
//!   [`Role`] variant must fail to compile *here*, so a new role can never
//!   silently become invisible scenery.
//! - **[`Authored`] carries everything a later feature could need** from the
//!   authored instance (kind, flags, local position, rotation, scale, waypoints,
//!   material, mesh AABB). A new behaviour edits one arm instead of adding a
//!   second query path over the same entities.
//!
//! An unknown role string (a stale blob, or a level from a newer build) never
//! reaches here: `Role::parse` returns `None` at the two call sites and the
//! entity is left as spawned — it renders, it does nothing. That is the one
//! place the old catch-all `_ => {}` semantics survive, and it is now explicit.

use alloc::string::String;

use bevy_ecs::system::EntityCommands;
use bevy_nds_3d::prelude::{DsMaterial, Stylized, Transform3d, Vec3};
use bevy_nds_math::FxVec2;
use bevy_nds_scene::{SceneInstance, ScenePath};
use bevy_nds_sprite::prelude::Sprite;
use kts_schema::{EnemyKind, Role};

use crate::capture::{self, VulnerabilityShape};
use crate::collide::{self, Colliders};
use crate::flags;
use crate::player::Height;
use crate::{
    Avatar, Enemy, Landmark, PARK_Y, Persistent, WorldPos, ZoneCaptureState, ZoneMember, sprites,
    zone_key,
};

/// Whether the instance belongs to the **active** zone or to a **resident
/// neighbour** rendered at an offset across the seam (#27 seamless streaming).
///
/// Made an explicit parameter rather than two code paths so every
/// residency-dependent difference is one visible guard.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Residency {
    /// The zone the avatar is standing in (offset `(0, 0)`).
    Active,
    /// A 1-hop neighbour, rendered at `−delta` in the active frame.
    Neighbour,
}

/// One authored instance, normalised across the two spawn paths.
///
/// Positions are the zone's **local** (un-offset) frame — that is what keys the
/// [`ZoneMember`] persistence snapshot, and it is frame-independent whether the
/// zone later reloads as active or as a neighbour.
pub(crate) struct Authored<'a> {
    /// The parsed role. Unparsed roles never get here (see the module docs).
    pub role: Role,
    /// The role-scoped sub-archetype byte (`Role::kinds()` index).
    pub kind: u8,
    /// Authored instance flags (`kts_schema::flag_bits`).
    pub flags: u32,
    /// Authored **local** position `(x, y, z)` — the `ZoneMember` key source.
    pub local: [f32; 3],
    pub rot: Vec3,
    pub scale: Vec3,
    /// Authored **local** ground-plane waypoints.
    pub path: &'a [[f32; 2]],
    /// The instance's lit material, if it authored one.
    #[allow(dead_code)] // carried for the item/tint work; see the module docs
    pub material: Option<DsMaterial>,
    /// The loaded mesh's local AABB (`min`, `max`), when it has one. Read by
    /// [`crate::collide::harvest`] — a solid instance's collider extents.
    pub aabb: Option<[Vec3; 2]>,
}

/// Where an [`Authored`] instance is being spawned: which zone owns it, the
/// world-frame offset its local coordinates are rendered at, and its residency.
pub(crate) struct SpawnCtx<'a> {
    /// Owning zone stem — the persistence key's namespace.
    pub stem: &'a str,
    /// Added to local XZ to reach the active frame. `(0, 0)` for the active zone.
    pub offset: (f32, f32),
    pub residency: Residency,
}

impl SpawnCtx<'_> {
    /// This instance's [`ZoneMember`] identity (stem + quantised local spawn
    /// position + the frame offset it renders at).
    fn member(&self, a: &Authored) -> ZoneMember {
        ZoneMember {
            stem: String::from(self.stem),
            key: zone_key(a.local[0], a.local[2]),
            offset: self.offset,
        }
    }
}

/// Should this instance be skipped entirely — no entity, no mesh load, no
/// per-frame processing?
///
/// Two cases, both pre-existing behaviour now stated once:
///
/// - A **neighbour avatar**: the avatar is the single persistent entity, seeded
///   from the level's entry zone; no other zone may spawn one.
/// - An **already-resolved enemy**: skip-at-spawn (#27), so a captured enemy
///   never flashes back into the world when its zone reloads. Completion lives
///   in the persistent `Flags` / `LevelProgress`, so nothing downstream needs it.
pub(crate) fn skip_spawn(a: &Authored, ctx: &SpawnCtx, snap: &ZoneCaptureState) -> bool {
    match a.role {
        Role::Avatar => ctx.residency == Residency::Neighbour,
        // Probed by `(stem, key)` rather than through `ctx.member(a)`: the
        // question is asked for every instance of every (re)loaded zone, and a
        // `ZoneMember` is only worth building for an enemy that survives it.
        Role::Enemy => snap
            .restore_at(ctx.stem, zone_key(a.local[0], a.local[2]))
            .is_some_and(|s| s.resolved.is_some()),
        Role::Landmark | Role::Block | Role::Prop => false,
    }
}

/// Attach the gameplay components for one authored instance.
///
/// `colliders` is the world's static blocking set — solid instances are
/// harvested into it at **both** residencies (#12 / #27: geometry is solid
/// across a seam). `None` only for a caller that doesn't own the resource.
///
/// The `match` below is the single role dispatch; see the module docs for why it
/// has no `_` arm.
pub(crate) fn attach(
    ec: &mut EntityCommands,
    a: &Authored,
    ctx: &SpawnCtx,
    snap: &ZoneCaptureState,
    colliders: Option<&mut Colliders>,
) {
    match a.role {
        Role::Avatar => {
            if ctx.residency == Residency::Active {
                // The single persistent entity (#27 seamless streaming):
                // consumed once from the entry zone at boot, it drops its
                // `SceneInstance` and gains `Persistent`, so no crossing ever
                // despawns it. Later zones author no avatar (and `swap_zone`
                // strips one anyway), so this fires exactly once.
                ec.remove::<SceneInstance>().insert((
                    Avatar,
                    Persistent,
                    WorldPos(FxVec2::from_f32(a.local[0], a.local[2])),
                    Height::default(),
                    Sprite::new(sprites::PLAYER).at(0, PARK_Y),
                ));
            }
            // Neighbour: already dropped by `skip_spawn`. The arm stays total.
        }
        Role::Enemy => {
            let member = ctx.member(a);
            let restored = snap.restore(&member);
            // Resume the enemy's full state (progress + patrol + local position)
            // if we've seen it, else spawn fresh at its authored position.
            let (enemy, cap, local_pos) = match restored {
                Some(st) => (
                    Enemy {
                        wp: st.wp,
                        pause: st.pause,
                    },
                    capture::Capture {
                        progress: st.progress,
                        resolved: None,
                    },
                    st.local,
                ),
                None => (
                    Enemy { wp: 1, pause: 0 },
                    capture::Capture::default(),
                    (a.local[0], a.local[2]),
                ),
            };
            let world = Vec3::new(
                local_pos.0 + ctx.offset.0,
                a.local[1],
                local_pos.1 + ctx.offset.1,
            );
            // The kind→shape pairing is the #29 matrix, declared once in
            // `kts_schema::EnemyKind::required_shape`; `for_kind` is the one
            // site that reads it. An unknown wire byte (a blob from a newer
            // build) falls back to `Basic`, i.e. circle-vulnerable.
            let shape =
                VulnerabilityShape::for_kind(EnemyKind::from_wire(a.kind).unwrap_or_default());
            ec.insert((
                enemy,
                cap,
                shape,
                // Per-enemy feedback state; `capture::update_enemy_tell` is the
                // single writer of the blip below (#29).
                capture::Tell::default(),
                WorldPos(FxVec2::from_f32(world.x, world.z)),
                // Outlined + cel-shaded so the threat reads at a glance;
                // terrain stays smooth (see `Stylized`).
                Stylized,
                // The blip advertises which gesture this machine answers to —
                // the shape-based (not colour-only) tell #27 locked.
                Sprite::new(shape.blip()).at(0, PARK_Y),
                member,
                // Correct the render transform now (not next frame via
                // `sync_3d`): a restored enemy's crate-spawned transform sits at
                // its *authored* position. Written for both residencies — with
                // offset (0, 0) an active enemy gets exactly its old values.
                Transform3d {
                    translation: world,
                    rotation: a.rot,
                    scale: a.scale,
                },
                // Patrol waypoints lifted into the active frame (the path is
                // authored in the owning zone's local coords).
                ScenePath(
                    a.path
                        .iter()
                        .map(|p| {
                            bevy_nds_scene::Vec2::new(p[0] + ctx.offset.0, p[1] + ctx.offset.1)
                        })
                        .collect(),
                ),
            ));
            // Objective enemies count toward the zone-clear gate (#27); freeform
            // ones don't.
            if a.flags & flags::OBJECTIVE != 0 {
                ec.insert(flags::Objective);
            }
            // Level-objective enemies roll up to the level exit instead (tier 2).
            if a.flags & flags::LEVEL_OBJECTIVE != 0 {
                ec.insert(flags::LevelObjectiveTag);
            }
        }
        Role::Landmark => {
            // Solid at BOTH residencies (#12, Locked): a landmark
            // across a seam blocks where it is drawn, so you can't stand half
            // inside a neighbour's wall and wedge yourself on the crossing. The
            // `Residency::Active` collision guard this arm used to carry is gone.
            if let Some(cs) = colliders
                && let Some(c) = collide::harvest(a, ctx)
            {
                cs.0.push(c);
            }
            if ctx.residency == Residency::Active {
                // The tactical-map blip stays active-zone only (the map is
                // origin-centric); whether solid footprints plot at all is open
                // on #26.
                ec.insert((
                    Landmark,
                    WorldPos(FxVec2::from_f32(a.local[0], a.local[2])),
                    Sprite::new(sprites::OBSTACLE).at(0, PARK_Y),
                ));
            }
        }
        Role::Block => {
            // Gray-box blocking geometry (#44 + #12): a collider and nothing
            // else. Deliberately **no `WorldPos`** — `sync_3d` pins every
            // `WorldPos` without a `Height` to `y = 0`, which would drop a
            // floor-resting or raised block onto the mesh-centred plane and
            // undo its authored height. Its `Transform3d` from the loader is
            // already correct, so leave it alone.
            if let Some(cs) = colliders
                && let Some(c) = collide::harvest(a, ctx)
            {
                cs.0.push(c);
            }
        }
        Role::Prop => {
            // Scenery consumption (`kts_schema::Consumption::Scenery`): renders
            // only, by design. The bake reports props as one per-zone Warning so
            // an author is told, not stopped.
        }
    }
}
