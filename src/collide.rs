//! Authored instances → static blocking geometry (#12).
//!
//! The game-side half of `bevy_nds_collide`: that crate is pure geometry and
//! knows nothing about scenes, roles or the renderer; this module is the single
//! place where an authored instance becomes a [`Collider`]. It is called from
//! exactly one site — [`crate::spawn::attach`]'s solid-role arms — so the active
//! zone and its resident neighbours can't drift apart the way the pre-#27 spawn
//! paths did.
//!
//! # The Height frame conversion (the one that matters)
//!
//! `bevy_nds_collide` works in the **Height frame**: the floor is `0` and the
//! avatar's feet are at `Height::z`. The renderer works in the render frame,
//! where the floor is [`crate::GROUND_Y`] and a mesh renders centred at `y = 0`
//! (so it *rests* on the floor). The two differ by exactly `GROUND_Y`, and this
//! module is where the subtraction happens — once, at zone load. Get it wrong
//! and the avatar sinks 0.16 into every surface in the game.
//!
//! # Extents are derived, never authored
//!
//! A solid instance's footprint is its **baked mesh AABB × the authored scale**,
//! turned by `rot.y` alone. The scale gizmo in the editor *is* the collider
//! editor; there is no second set of numbers to keep in sync. A mesh that fails
//! to load yields no collider at all — it doesn't render either, so blocking and
//! rendering always agree about what exists. `validate_all` makes a meshless, a
//! tilted, or a non-positively-scaled solid a bake Error, so none of the three is
//! a surprise at runtime; mirroring (a negative `scale.x`/`scale.z`) is handled
//! here by taking the magnitude, since a mirrored box has the same footprint.
//!
//! Decisions here are recorded on #12 (## Locked).

use alloc::vec::Vec;

use bevy_ecs::prelude::Resource;
use bevy_nds_collide::Collider;
use bevy_nds_math::{Fx32, FxVec2};
use kts_schema::{BlockKind, Role};

use crate::GROUND_Y;
use crate::spawn::{Authored, SpawnCtx};

/// Every static blocking volume the avatar can stand on or walk into: the
/// active zone's **and** its resident neighbours' (#27 — geometry is solid
/// across a seam, so you can't stand half inside a neighbour's wall and wedge
/// yourself on the crossing).
///
/// Harvested at zone load by [`crate::spawn::attach`] and cleared by
/// `transition::swap_zone`; never touched per frame except to read.
#[derive(Resource, Default)]
pub struct Colliders(pub Vec<Collider>);

/// Build the [`Collider`] for one authored instance, or `None` if it isn't
/// solid (or has no loaded mesh to take extents from).
///
/// Shape comes from the role-scoped kind byte: a `landmark` is always a box; a
/// `block` picks `box` / `ramp` / `round` from [`BlockKind`]. An unknown kind
/// byte (a blob from a newer build) falls back to the default box rather than
/// dropping the collider — a block you can't identify should still be solid.
pub fn harvest(a: &Authored, ctx: &SpawnCtx) -> Option<Collider> {
    if !matches!(a.role, Role::Landmark | Role::Block) {
        return None;
    }
    // No mesh ⇒ no extents ⇒ no collider. The same instance renders nothing, so
    // blocking and rendering agree; the bake already rejects the authored case.
    let [min, max] = a.aabb?;

    // Footprint half extents. `obj2dl` bakes with `center: true`, so the AABB is
    // symmetric about the origin on XZ and only its *size* matters here.
    //
    // Magnitudes, because mirroring a mesh with a negative `scale.x`/`scale.z`
    // is a normal authoring move and the footprint of a mirrored box is the same
    // box. Without the `abs` the scaled extents go negative and the per-frame
    // `Ord::clamp`s in `bevy_nds_collide` get `min > max`, which panics — and a
    // panic on the DS is an abort. `validate_all` makes a non-positive solid
    // scale a bake Error and the crate's constructors normalise too; this is the
    // conversion site, so it takes the magnitude at the point it is introduced.
    let half = FxVec2::new(
        Fx32::from_f32((max.x - min.x) * 0.5 * a.scale.x).abs(),
        Fx32::from_f32((max.z - min.z) * 0.5 * a.scale.z).abs(),
    );
    // The vertical span, converted out of the render frame exactly once —
    // ordered, so a negative `scale.y` flips the mesh without inverting the span.
    let y0 = Fx32::from_f32(a.local[1] + min.y * a.scale.y - GROUND_Y);
    let y1 = Fx32::from_f32(a.local[1] + max.y * a.scale.y - GROUND_Y);
    let (base, top) = (y0.min(y1), y0.max(y1));
    // Neighbour zones render at `−delta`; their geometry blocks there too.
    let center = FxVec2::from_f32(a.local[0] + ctx.offset.0, a.local[2] + ctx.offset.1);
    // Yaw only (pitch/roll never collide). Soft-float trig is fine here — this
    // runs once per instance at zone load, never per frame.
    let sin = Fx32::from_f32(libm::sinf(a.rot.y));
    let cos = Fx32::from_f32(libm::cosf(a.rot.y));

    let kind = match a.role {
        // A landmark is always a box — `Role::Landmark.kinds()` is empty, and
        // the bake rejects a `kind` on one.
        Role::Landmark => BlockKind::Box,
        _ => BlockKind::from_wire(a.kind).unwrap_or_default(),
    };
    Some(match kind {
        BlockKind::Box => Collider::block(center, half, sin, cos, base, top),
        // `prim_ramp` is modelled rising toward local +Z, which is the direction
        // `Collider::ramp` expects; `rot.y` aims it.
        BlockKind::Ramp => Collider::ramp(center, half, sin, cos, base, top),
        // A column collides as a circle of its X half extent rather than as the
        // square its AABB would otherwise give.
        BlockKind::Round => Collider::round(center, half.x, base, top),
    })
}
