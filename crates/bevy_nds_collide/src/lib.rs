//! Static blocking geometry — yawed boxes, ramps and round columns (#12).
//!
//! The collision half of locomotion, kept deliberately small: **static**
//! geometry, **one** moving body (the avatar), **no** dynamics, no collision
//! events, no broadphase grid. A caller hands in a slice of [`Collider`]s and a
//! [`Body`]; the crate answers three questions:
//!
//! 1. [`resolve_move`] — where does a horizontal move actually end up?
//!    (sub-stepped so a fast move can't tunnel, radius-inflated push-out so a
//!    wall slides rather than sticks, and the caller's bounds clamp applied
//!    *inside* the loop so a collider on a zone edge can never eject the body
//!    out of bounds).
//! 2. [`ground_height`] — what is the support height under a point?
//! 3. [`settle_height`] — given that support, where does the vertical axis land
//!    this frame (step up a kerb, follow a ramp, land, or keep falling)?
//!
//! Like [`bevy_nds_3d_cull`](https://docs.rs/bevy_nds_3d_cull) it is pure: no
//! Bevy, no FFI, no allocation. Everything is [`Fx32`]/[`FxVec2`] (20.12 fixed
//! point) because this runs per frame on a 33 MHz FPU-less ARM946E-S.
//!
//! # The Height frame (load-bearing)
//!
//! Vertical values in this crate live in the **Height frame**: `y_h = y_render −
//! GROUND_Y`, i.e. the floor is `0` and a body's [`Body::height`] is measured up
//! from its feet. The renderer's ground plane offset is the caller's business —
//! *the caller converts by subtracting its own ground Y* before building a
//! [`Collider`] and before passing `feet`. Getting this wrong sinks the body
//! into every surface by exactly that offset, so it is stated once here and
//! honoured at the one conversion site (the game's `harvest`).
//!
//! # Footprints
//!
//! A collider's footprint is its mesh AABB × the authored scale, turned by a
//! **yaw only** (`rot.y`): pitch and roll never collide (a tilted decoration is
//! a different authored role). A [`Footprint::Round`] collider ignores its
//! rectangle and collides as a disc of radius `half.x` — that is the one shape
//! whose push-out reproduces the pre-#12 circle collision (raw-exactly, except
//! at the dead centre, which the old loop declined to resolve at all).
//!
//! # What a box and a ramp have in common
//!
//! Nothing branches on "box vs ramp": a box **is** a ramp with [`Collider::slope`]
//! zero. One [`surface`] function covers both, so the step / land / walk-up
//! rules have a single implementation.
//!
//! # Scope (#12, 2026-09-18 — pending design-sync)
//!
//! Avatar-only. Enemies and projectiles are not collided in this slice;
//! [`blocks_point`] ships as the hook for that decision (does static geometry
//! provide cover?) without answering it.

#![cfg_attr(not(test), no_std)]

use bevy_nds_math::{Fx32, FxVec2, ONE_RAW};

/// How a collider's ground-plane outline is shaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Footprint {
    /// An oriented rectangle: `half` are its half extents in the yaw frame.
    Rect,
    /// A disc of radius `half.x`, centred on `center` (yaw is irrelevant).
    Round,
}

/// One piece of static blocking geometry.
///
/// `half.x` / `half.y` are the **local XZ** half extents (`half.y` is the local
/// *Z* half extent — [`FxVec2`] is a ground-plane vector, its `y` is world Z).
/// `base` / `top` are the Height-frame span, and `top` is specifically the
/// surface height at the local **+Z** edge, which is where a ramp is highest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Collider {
    /// World XZ centre of the footprint.
    pub center: FxVec2,
    /// Local half extents `(x, z)`. [`Footprint::Round`] uses `half.x` as its
    /// radius and ignores `half.y` for the outline.
    ///
    /// **Always non-negative**, and `base <= top` — the constructors take the
    /// magnitude ([`normalize`]), because the per-frame path clamps against
    /// these and `Ord::clamp` panics on an inverted range. Build a [`Collider`]
    /// through `block` / `ramp` / `round`, never by struct literal.
    pub half: FxVec2,
    /// `sin(yaw)` — yaw only; pitch and roll never collide.
    pub sin: Fx32,
    /// `cos(yaw)`.
    pub cos: Fx32,
    /// Height-frame bottom of the span (used by the walk-under-overhang rule).
    pub base: Fx32,
    /// Height-frame surface height at the local `+Z` edge (the whole top of a
    /// box; the high edge of a ramp).
    pub top: Fx32,
    /// Rise per local-Z unit. **Zero for a box** — a box is a ramp with no rise.
    pub slope: Fx32,
    pub foot: Footprint,
    /// Footprint circumradius, for the squared-distance broad reject.
    pub bound: Fx32,
}

/// The moving body: a vertical cylinder that can step up small lips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Body {
    /// Horizontal radius.
    pub radius: Fx32,
    /// Height from the feet — anything whose `base` is above `feet + height` is
    /// an overhang the body walks under.
    pub height: Fx32,
    /// The tallest lip the body steps onto instead of being blocked by.
    pub step: Fx32,
}

/// Hard cap on [`resolve_move`]'s sub-steps.
///
/// Each sub-step advances at most [`Body::radius`], so a resolved move is at
/// most `MAX_SUBSTEPS × radius` long — with the shipped body (radius `0.18`)
/// that is `0.72` world units, ~11× the largest per-frame delta the controller
/// can produce (a roll at `3.8/60 ≈ 0.063`). The cap therefore never binds in
/// game; it exists so a pathological call can't loop for an unbounded time.
pub const MAX_SUBSTEPS: u32 = 4;

impl Collider {
    /// A flat-topped box: one surface height (`top`) over the whole footprint.
    pub fn block(
        center: FxVec2,
        half: FxVec2,
        yaw_sin: Fx32,
        yaw_cos: Fx32,
        base: Fx32,
        top: Fx32,
    ) -> Self {
        let (half, base, top) = normalize(half, base, top);
        Self {
            center,
            half,
            sin: yaw_sin,
            cos: yaw_cos,
            base,
            top,
            slope: Fx32::ZERO,
            foot: Footprint::Rect,
            bound: rect_bound(half),
        }
    }

    /// A ramp rising along local **+Z**: `base` at the `−Z` edge, `top` at the
    /// `+Z` edge. `rot.y` aims it; the slope is derived, never authored.
    pub fn ramp(
        center: FxVec2,
        half: FxVec2,
        yaw_sin: Fx32,
        yaw_cos: Fx32,
        base: Fx32,
        top: Fx32,
    ) -> Self {
        let (half, base, top) = normalize(half, base, top);
        let run = half.y + half.y;
        let mut slope = if run.raw() > 0 {
            (top - base) / run
        } else {
            Fx32::ZERO
        };
        // [`surface`] interpolates **down** from `top` (that is what lets a box
        // be a ramp with slope zero), and both the divide and the multiply
        // truncate — so a slope rounded down leaves the low edge a raw unit or
        // two *above* `base`. That residue is not cosmetic: an airborne body
        // standing on the floor in a ramp's low-edge skirt then reads the ramp
        // as a wall (airborne `step_allow` is zero) and is shoved backwards on
        // the jump frame. Round the slope up instead, so `top − slope·run`
        // lands on or below `base`, where `surface`'s clamp finishes the job.
        // Bounded and load-time only: one raw unit of slope moves the product
        // by `run / 4096`, so a short run needs a few passes.
        if run.raw() > 0 {
            let mut guard = 0;
            while top - slope * run > base && guard < 16 {
                slope = Fx32::from_raw(slope.raw() + 1);
                guard += 1;
            }
        }
        Self {
            center,
            half,
            sin: yaw_sin,
            cos: yaw_cos,
            base,
            top,
            slope,
            foot: Footprint::Rect,
            bound: rect_bound(half),
        }
    }

    /// A flat-topped round column. Yaw is meaningless for a disc, so the
    /// rotation is stored as identity and `local` is an exact translation —
    /// which is what makes the push-out identical to the pre-#12 circle
    /// collision everywhere it did anything (the one difference is a body at
    /// the exact centre, where the old loop left it alone; see [`push_out`]).
    pub fn round(center: FxVec2, radius: Fx32, base: Fx32, top: Fx32) -> Self {
        let radius = radius.abs();
        let (base, top) = (base.min(top), base.max(top));
        Self {
            center,
            half: FxVec2::new(radius, radius),
            sin: Fx32::ZERO,
            cos: Fx32::ONE,
            base,
            top,
            slope: Fx32::ZERO,
            foot: Footprint::Round,
            bound: radius,
        }
    }
}

/// Normalise a caller's extents into the shape the per-frame path assumes:
/// **non-negative** half extents and `base <= top`.
///
/// Mirroring a mesh with a negative `scale.x` / `scale.z` is a normal authoring
/// move, and it scales the baked AABB into *negative* half extents; a negative
/// `scale.y` swaps the ends of the vertical span. The hot path leans on
/// [`Ord::clamp`] — [`rect_contact`], and [`surface`]'s local-Z clamp — which
/// **panics** when `min > max`, and a panic on the DS is `panic = "abort"`: the
/// ROM would die the first frame the avatar came within `bound + radius` of such
/// a collider.
///
/// The bake now rejects a non-positive solid scale outright and the game's
/// `harvest` takes the magnitude itself, but this crate is public geometry — it
/// must be safe whoever calls it. The cost is two `abs` and a compare **at
/// construction** (zone load); the per-frame path gains no work.
///
/// Nothing is lost for a box or a column: a mirrored rectangle is the same
/// rectangle. A [`Collider::ramp`] always rises toward local `+Z` afterwards —
/// a ramp facing the other way is authored with `rot.y`, not a negative scale.
#[inline]
fn normalize(half: FxVec2, base: Fx32, top: Fx32) -> (FxVec2, Fx32, Fx32) {
    (
        FxVec2::new(half.x.abs(), half.y.abs()),
        base.min(top),
        base.max(top),
    )
}

// --- Pure geometry ------------------------------------------------------------

/// `|v|²` in raw 20.12 units, widened to `i64` so a 400-unit arena coordinate
/// can't overflow. [`FxVec2`] has no public `length_sq`, and squaring through
/// [`Fx32`] would lose the low bits this comparison needs.
#[inline]
fn raw_len_sq(v: FxVec2) -> i64 {
    let x = v.x.raw() as i64;
    let y = v.y.raw() as i64;
    x * x + y * y
}

#[inline]
fn raw_sq(v: Fx32) -> i64 {
    let a = v.raw() as i64;
    a * a
}

/// Footprint circumradius of a rectangle — the broad-reject bound.
fn rect_bound(half: FxVec2) -> Fx32 {
    // sqrt of the raw i64 squared length is the raw length: no intermediate
    // `Fx32::mul` truncation, so a saturated (absurd) half extent degrades to
    // a large bound instead of a negative product feeding `sqrt`. Load-time only.
    Fx32::from_raw(bevy_nds_math::hw::sqrt_u64(raw_len_sq(half) as u64) as i32)
}

/// World point → the collider's yaw frame (origin at `center`).
///
/// The inverse of [`world`] — see its note on the renderer's convention.
#[inline]
fn local(c: &Collider, p: FxVec2) -> FxVec2 {
    let d = p - c.center;
    FxVec2::new(d.x * c.cos - d.y * c.sin, d.x * c.sin + d.y * c.cos)
}

/// The collider's yaw frame → world.
///
/// **This must match the renderer**, or a yawed solid collides as the mirror
/// image of what is drawn. `bevy_nds_math::model_matrix` (used by
/// `bevy_nds_3d`, `bevy_nds_3d_cull` and the editor's viewport and rotate
/// gizmo) applies `R_y(+yaw)`, whose ground-plane part sends local `(x, z)` to
/// world `(x·cos + z·sin, −x·sin + z·cos)` — so local `+Z` points at
/// `(sin, cos)`, which is where a ramp authored with `rot.y` climbs.
/// `yaw_convention_matches_model_matrix` pins the two together.
#[inline]
fn world(c: &Collider, l: FxVec2) -> FxVec2 {
    c.center + FxVec2::new(l.x * c.cos + l.y * c.sin, l.y * c.cos - l.x * c.sin)
}

/// The surface height at a local Z. A box has `slope == 0`, so this collapses to
/// `top` with no branch; a ramp interpolates and **clamps** outside its run, so
/// the radius-inflated skirt around a ramp reads as its nearest edge height
/// rather than extrapolating into the air.
///
/// The result is clamped into `[base, top]` (two compares, no branch worth the
/// name): fixed-point rounding must never put the authored low edge *above*
/// `base` — that single raw unit is the difference between a ramp's foot being
/// walkable and being a wall to an airborne body at floor level. See
/// [`Collider::ramp`], which rounds the slope up so the clamp is exact rather
/// than merely safe.
#[inline]
fn surface(c: &Collider, lz: Fx32) -> Fx32 {
    let s = c.top - c.slope * (c.half.y - lz.clamp(-c.half.y, c.half.y));
    // `max` then `min` rather than `clamp`: `Ord::clamp` panics if a degenerate
    // collider ever arrived with `base > top`, and a panic on the DS is an abort.
    s.max(c.base).min(c.top)
}

/// The point of the (un-inflated) **rectangle** closest to `lp`, in local space.
///
/// Rect-only on purpose: the disc's closest point needs a normalize (a hardware
/// sqrt + two divides), and nothing on the per-frame path wants it — a
/// [`Footprint::Round`] collider is flat-topped, so its surface is `top`
/// wherever you are, and its push-out computes its own direction.
#[inline]
fn rect_contact(c: &Collider, lp: FxVec2) -> FxVec2 {
    FxVec2::new(
        lp.x.clamp(-c.half.x, c.half.x),
        lp.y.clamp(-c.half.y, c.half.y),
    )
}

/// Does the **bare** (un-inflated) footprint contain `lp`? Pure compares — no
/// sqrt, no divide, for either footprint shape.
#[inline]
fn inside_bare(c: &Collider, lp: FxVec2) -> bool {
    match c.foot {
        Footprint::Rect => lp.x.abs() <= c.half.x && lp.y.abs() <= c.half.y,
        Footprint::Round => raw_len_sq(lp) <= raw_sq(c.half.x),
    }
}

/// Does the **radius-inflated** footprint contain `lp`?
///
/// Rect uses the Minkowski sum (closest point on the rectangle, then a
/// squared-distance test against `r`); Round is a plain disc of `R + r`. Both
/// are strict, so a point resting exactly on the inflated boundary — which is
/// where [`push_out`] leaves it — is *not* inside, making the resolve
/// idempotent.
#[inline]
fn overlaps(c: &Collider, lp: FxVec2, r: Fx32) -> bool {
    match c.foot {
        Footprint::Rect => {
            let q = rect_contact(c, lp);
            raw_len_sq(lp - q) < raw_sq(r)
        }
        Footprint::Round => raw_len_sq(lp) < raw_sq(c.half.x + r),
    }
}

/// Move `p` to the nearest point on the radius-inflated footprint boundary.
/// Called only when [`overlaps`] said so, so it is off the no-contact path.
fn push_out(c: &Collider, r: Fx32, p: FxVec2) -> FxVec2 {
    let lp = local(c, p);
    let out = match c.foot {
        Footprint::Rect => {
            let q = rect_contact(c, lp);
            let d = lp - q;
            if d != FxVec2::ZERO {
                // Shallow overlap: slide out along the surface normal, which
                // preserves the tangential part of the move (walls slide).
                q + d.normalize_or_zero() * r
            } else {
                // Deep: `p` is inside the rectangle itself, so there is no
                // normal to follow. Leave along the least-penetration axis.
                let px = c.half.x + r - lp.x.abs();
                let pz = c.half.y + r - lp.y.abs();
                let sign = |v: Fx32, m: Fx32| if v.raw() < 0 { -m } else { m };
                if px <= pz {
                    FxVec2::new(sign(lp.x, c.half.x + r), lp.y)
                } else {
                    FxVec2::new(lp.x, sign(lp.y, c.half.y + r))
                }
            }
        }
        Footprint::Round => {
            let reach = c.half.x + r;
            if lp == FxVec2::ZERO {
                // Dead centre: no separation direction exists, so pick +X. The
                // pre-#12 circle loop guarded on `d > 0` and so did *nothing*
                // here, leaving the body stuck inside; this is the one input on
                // which the two differ, and ejecting is the better answer.
                FxVec2::new(reach, Fx32::ZERO)
            } else {
                lp.normalize_or_zero() * reach
            }
        }
    };
    world(c, out)
}

/// The surface height at a world XZ point, or `None` outside the *un-inflated*
/// footprint. The placement-probe flavour of [`ground_height`] (one collider, no
/// body radius) — handy for "what is the ground exactly here?".
pub fn surface_at(c: &Collider, p: FxVec2) -> Option<Fx32> {
    let lp = local(c, p);
    inside_bare(c, lp).then(|| surface(c, lp.y))
}

/// Slack on the step-over compare: two raw 20.12 units, i.e. half a millimetre
/// of world space.
///
/// A collider's `base` is quantised at zone load (`(render_y − ground_y)` in
/// 20.12), so a solid authored flush with the floor can land a raw unit above
/// it. Without this slack that unit turns a ramp's foot — or a floor-flush
/// kerb — into a wall for an airborne body standing at floor level, whose
/// `step_allow` is zero: pressing Jump beside one shoves the body backwards.
const STEP_EPS: Fx32 = Fx32::from_raw(2);

/// Does this collider block a body whose feet are at `feet` from standing at
/// `p`?
///
/// The whole blocking rule, in three clauses:
///
/// 1. **Walk-under**: a collider whose `base` is above `feet + body.height` is
///    an overhang; the body passes beneath it.
/// 2. **Step-over**: a surface no higher than `feet + step_allow` is a kerb, a
///    ramp's low edge, or a top the body already stands on — it supports, it
///    does not block. The caller passes `step_allow = body.step` while grounded
///    and `ZERO` while airborne, so nothing is stepped mid-jump.
///
///    A body whose centre is inside the **un-inflated** footprint is the one
///    exception: it keeps its full [`Body::step`] allowance however the caller
///    set `step_allow`. It can only have got in there by standing on (or
///    landing on) this very surface — the inflated skirt blocks every other
///    approach — so the airborne rule must not apply to it. Without this, the
///    frame a jump clears `grounded` while walking up a ramp, the surface one
///    sub-step further up-slope reads as a wall and clause 3 ejects the body
///    sideways off the ramp it is standing on.
///
///    The compare carries [`STEP_EPS`] of slack so a surface that quantises a
///    raw unit above the feet can't become a wall either.
/// 3. Otherwise the body is blocked iff it overlaps the radius-inflated
///    footprint.
///
/// Nothing here takes a square root: the surface lookup clamps, the bare-inside
/// test is compares, and the overlap test is a raw `i64` squared distance. The
/// one sqrt in the crate's hot path lives in [`push_out`], which only runs on an
/// actual overlap.
pub fn blocks(c: &Collider, body: &Body, feet: Fx32, step_allow: Fx32, p: FxVec2) -> bool {
    if c.base > feet + body.height {
        return false;
    }
    let lp = local(c, p);
    let allow = if inside_bare(c, lp) {
        step_allow.max(body.step)
    } else {
        step_allow
    };
    // `surface` clamps its own `lz`, so the un-clamped local Z is the surface at
    // the closest point of the footprint — no `rect_contact` needed, and the
    // Round path never reaches for a normalize it would only throw away.
    if surface(c, lp.y) <= feet + allow + STEP_EPS {
        return false;
    }
    overlaps(c, lp, body.radius)
}

/// Resolve a horizontal move from `from` toward `to` against static geometry.
///
/// Sub-stepped (see [`MAX_SUBSTEPS`]) so a long delta can't step over a
/// collider, and the caller's `bounds` (`[min_x, min_z, max_x, max_z]`) are
/// clamped **inside** the sub-step loop, after the push-out. That ordering is
/// the point: clamping first (as the pre-#12 controller did) lets a collider
/// sitting on a zone edge eject the body *outside* its own bounds, where the
/// crossing test's edge epsilon then fires.
pub fn resolve_move(
    cs: &[Collider],
    from: FxVec2,
    to: FxVec2,
    body: &Body,
    feet: Fx32,
    step_allow: Fx32,
    bounds: [Fx32; 4],
) -> FxVec2 {
    let delta = to - from;
    let n = substeps(delta, body.radius);
    let (stepv, capped) = substep_vec(delta, body.radius, n);
    let mut p = from;
    // Track what has actually been advanced so the last (uncapped) sub-step can
    // consume the exact remainder: `n` fixed-point fractions of `delta` do not
    // re-sum to `delta`, and without this an unobstructed move would land a raw
    // unit or two short of its target every frame.
    let mut moved = FxVec2::ZERO;
    for k in 0..n {
        let adv = if !capped && k + 1 == n {
            delta - moved
        } else {
            stepv
        };
        moved += adv;
        p += adv;
        for c in cs {
            // Broad reject: squared distance in raw i64, no sqrt, no divide.
            let reach = c.bound + body.radius;
            if raw_len_sq(p - c.center) >= raw_sq(reach) {
                continue;
            }
            if blocks(c, body, feet, step_allow, p) {
                p = push_out(c, body.radius, p);
            }
        }
        p = clamp_bounds(p, bounds);
    }
    p
}

/// How many sub-steps this delta needs: `ceil(|delta| / radius)`, at least one
/// and at most [`MAX_SUBSTEPS`]. The common case (a per-frame delta shorter than
/// the body radius) is answered by one raw `i64` compare — no sqrt, no divide.
fn substeps(delta: FxVec2, radius: Fx32) -> u32 {
    if radius.raw() <= 0 {
        return 1;
    }
    if raw_len_sq(delta) <= raw_sq(radius) {
        return 1;
    }
    let q = delta.length() / radius;
    let ceil = ((q.raw() as i64) + (ONE_RAW as i64) - 1) >> 12;
    (ceil.clamp(1, MAX_SUBSTEPS as i64)) as u32
}

/// The per-sub-step advance, and whether [`MAX_SUBSTEPS`] truncated the move.
///
/// The advance is never longer than [`Body::radius`], which is what makes
/// tunnelling impossible: every inflated footprint is at least `2·radius` across
/// through its centre, so a step of `radius` cannot cross one. When the cap
/// binds, `capped` is true and the call deliberately travels only
/// `MAX_SUBSTEPS × radius` rather than teleporting the remainder past whatever
/// is in the way.
fn substep_vec(delta: FxVec2, radius: Fx32, n: u32) -> (FxVec2, bool) {
    if n <= 1 {
        return (delta, false);
    }
    let v = delta * (Fx32::ONE / Fx32::from_int(n as i32));
    if raw_len_sq(v) <= raw_sq(radius) {
        (v, false)
    } else {
        (delta.normalize_or_zero() * radius, true)
    }
}

#[inline]
fn clamp_bounds(p: FxVec2, b: [Fx32; 4]) -> FxVec2 {
    // `max().min()` rather than `Ord::clamp`: the latter panics on `min > max`,
    // and a panic on the DS is an abort. The bake guarantees ordered bounds, but
    // this is a public crate and the two compares cost the same either way.
    FxVec2::new(p.x.max(b[0]).min(b[2]), p.y.max(b[1]).min(b[3]))
}

/// The support height under a body whose feet are at `feet` standing at `p`:
/// the highest collider surface this body may be supported by, else `floor`.
///
/// Support is **asymmetric about the feet**, and that asymmetry is the whole
/// design:
///
/// - A surface **at or below** `feet` — a top the body is already standing on —
///   supports through the *radius-inflated* footprint. That is deliberate ledge
///   forgiveness: walking off a box top the body stays supported until its
///   centre is more than `radius` past the edge, which is exactly where the box
///   stops blocking too, so stepping off a ledge is never a `radius`-sized
///   sideways shove.
/// - A surface **above** `feet` — a step up — needs the *bare* footprint to
///   contain the centre. Forgiveness there would be a `radius`-wide invisible
///   skirt you could ride: walking along the floor beside a ramp would lift the
///   body up the ramp's height profile while it is demonstrably not on it, and
///   a kerb would mount itself before the body reached it.
///
/// Walking *up* a ramp is unaffected — the centre really is inside its
/// footprint — and a box top abutting a ramp's high edge still joins smoothly,
/// because the step from one surface to the other is what `settle_height`
/// follows.
///
/// `ceiling` is the caller's "how far may I be lifted this frame": `feet + step`
/// while grounded, `feet` while airborne. It is **not** a hard cap, and the two
/// branches apply it in opposite directions — read the effective limits off
/// these, not off the parameter's name:
///
/// - Over the bare footprint the limit is `max(ceiling, feet + step)`, so
///   `ceiling` can only ever **widen** it. A surface the body is demonstrably
///   over is admitted up to `feet + step` whatever the caller passed, because an
///   airborne body descending onto a *rising* surface — landing while walking up
///   a ramp — has feet below the new surface by up to one frame's worth of
///   climb. Rejecting those is how the body sinks through a ramp and gets
///   ejected sideways two frames later. It cannot snap the body *up*, because a
///   rising body never lands ([`settle_height`] requires a non-positive `vz`).
/// - Out in the radius skirt the limit is `min(ceiling, feet)`, so `ceiling` can
///   only ever **narrow** it: the skirt never lifts.
///
/// For the game's two call patterns those collapse to `feet + step` and `feet`
/// respectively, i.e. `ceiling` currently changes nothing — it is the knob for a
/// caller whose body may not be lifted at all, or is being pushed down. A caller
/// wanting to *forbid* a step up wants a smaller [`Body::step`], not a smaller
/// `ceiling`.
pub fn ground_height(
    cs: &[Collider],
    p: FxVec2,
    body: &Body,
    feet: Fx32,
    ceiling: Fx32,
    floor: Fx32,
) -> Fx32 {
    let mut best = floor;
    // One add, hoisted out of the loop: the highest surface a collider the body
    // is standing over may present.
    let over = ceiling.max(feet + body.step);
    for c in cs {
        let reach = c.bound + body.radius;
        if raw_len_sq(p - c.center) >= raw_sq(reach) {
            continue;
        }
        let lp = local(c, p);
        if !overlaps(c, lp, body.radius) {
            continue;
        }
        // `surface` clamps `lz` itself, so this is the surface at the closest
        // point of the footprint without asking for one.
        let s = surface(c, lp.y);
        // The asymmetry, as one cap: over the real footprint you may be lifted
        // onto (or land on) a surface a step above your feet; out in the
        // radius skirt a surface may only ever hold you at or below them.
        let cap = if inside_bare(c, lp) {
            over
        } else {
            ceiling.min(feet)
        };
        if s <= cap && s > best {
            best = s;
        }
    }
    best
}

/// Settle the vertical axis for one frame.
///
/// `z`/`vz` are the caller's already gravity-integrated height and velocity,
/// `prev` the height at the start of the frame, `ground` the support height
/// from [`ground_height`]. Returns `(z, vz, grounded)`.
///
/// Three cases:
/// - **Follow the surface.** Grounded and the support moved by no more than
///   `step`: snap to it and *stay grounded*. This is the kerb step-up, the ramp
///   walk, and the small step down — staying grounded on a downslope is what
///   lets a jump off a ramp work at all.
/// - **Land.** Descending through a surface: snap to it, zero the velocity.
///   *Descending* is literal — a body still rising (`vz > 0`) passes a surface
///   its feet are momentarily below rather than being caught by it, so a jump
///   taken over a low top is never cut short by that top.
/// - **Fall.** Keep the integrated values.
pub fn settle_height(
    z: Fx32,
    vz: Fx32,
    prev: Fx32,
    grounded: bool,
    ground: Fx32,
    step: Fx32,
) -> (Fx32, Fx32, bool) {
    // Two different situations with the same outcome — sit on the surface,
    // grounded — so they share a branch rather than repeating it.
    let follow = grounded && (ground - prev).abs() <= step;
    let land = z <= ground && vz.raw() <= 0;
    if follow || land {
        (ground, Fx32::ZERO, true)
    } else {
        (z, vz, false)
    }
}

/// Is the point `p` at Height-frame altitude `h` inside solid geometry?
///
/// A zero-radius probe over the un-inflated footprints. Shipped as the hook for
/// the still-open cover question (#26: is a projectile stopped by a block, do
/// enemies path around solids?) — **the game does not call it in this slice**,
/// and its presence answers nothing.
pub fn blocks_point(cs: &[Collider], p: FxVec2, h: Fx32) -> bool {
    cs.iter().any(|c| match surface_at(c, p) {
        Some(s) => h >= c.base && h < s,
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped avatar body (`player::Locomotion`): radius 0.18, height 0.32,
    /// step 0.10.
    fn body() -> Body {
        Body {
            radius: fx(0.18),
            height: fx(0.32),
            step: fx(0.10),
        }
    }

    fn fx(v: f32) -> Fx32 {
        Fx32::from_f32(v)
    }

    fn v(x: f32, y: f32) -> FxVec2 {
        FxVec2::from_f32(x, y)
    }

    /// Arena-sized bounds that never bind in a test that isn't about them.
    fn wide() -> [Fx32; 4] {
        [fx(-100.0), fx(-100.0), fx(100.0), fx(100.0)]
    }

    /// An axis-aligned box resting on the floor: XZ half extents, top height.
    fn box_at(cx: f32, cz: f32, hx: f32, hz: f32, top: f32) -> Collider {
        Collider::block(
            v(cx, cz),
            v(hx, hz),
            Fx32::ZERO,
            Fx32::ONE,
            Fx32::ZERO,
            fx(top),
        )
    }

    fn yawed_box(cx: f32, cz: f32, hx: f32, hz: f32, top: f32, yaw: f32) -> Collider {
        Collider::block(
            v(cx, cz),
            v(hx, hz),
            fx(yaw.sin()),
            fx(yaw.cos()),
            Fx32::ZERO,
            fx(top),
        )
    }

    // --- surfaces -------------------------------------------------------------

    #[test]
    fn surface_flat_box_is_top_inside_none_outside() {
        let c = box_at(0.0, 0.0, 0.2, 0.3, 0.12);
        assert_eq!(c.slope, Fx32::ZERO, "a box has no rise");
        // Flat: the same height everywhere inside, corners included.
        for p in [v(0.0, 0.0), v(0.19, 0.29), v(-0.2, 0.3), v(0.0, -0.29)] {
            assert_eq!(surface_at(&c, p), Some(fx(0.12)), "{p:?}");
        }
        // …and nothing at all outside the (un-inflated) footprint.
        for p in [v(0.21, 0.0), v(0.0, 0.31), v(-0.5, -0.5)] {
            assert_eq!(surface_at(&c, p), None, "{p:?}");
        }
    }

    #[test]
    fn ramp_surface_is_linear_and_clamped() {
        // Rise 0.24 over a local-Z run of 0.70 (half 0.35), axis aligned.
        let c = Collider::ramp(
            v(0.0, 0.0),
            v(0.2, 0.35),
            Fx32::ZERO,
            Fx32::ONE,
            Fx32::ZERO,
            fx(0.24),
        );
        let near = |a: Fx32, b: f32| (a - fx(b)).abs() <= Fx32::from_raw(2);
        // Both ends are *exact*, not merely close: a raw unit of residue at the
        // low edge makes a ramp's foot a wall to an airborne body standing on
        // the floor beside it (`blocks`'s step-over compare), and a raw unit at
        // the high edge opens a seam where a ramp meets a box top.
        assert_eq!(
            surface_at(&c, v(0.0, -0.35)).unwrap(),
            c.base,
            "the low edge is the authored base, to the raw unit"
        );
        assert_eq!(
            surface_at(&c, v(0.0, 0.35)).unwrap(),
            c.top,
            "…and the high edge is the authored top"
        );
        assert!(near(surface_at(&c, v(0.0, 0.0)).unwrap(), 0.12), "midpoint");
        // The rise is along local Z only — X does not tilt it.
        assert_eq!(surface_at(&c, v(-0.2, 0.0)), surface_at(&c, v(0.2, 0.0)));
        // Outside the run the surface clamps to the nearest edge rather than
        // extrapolating (the radius skirt reads as edge height).
        assert!(near(surface(&c, fx(-9.0)), 0.0));
        assert!(near(surface(&c, fx(9.0)), 0.24));

        // A +90° yaw turns the gradient onto world X. Local +Z maps to world +X
        // (`world()` sends `(0, 1)` to `(sin, cos) = (1, 0)`, the renderer's
        // convention), so the high edge now faces +X and the footprint is 0.70
        // wide in X, 0.40 in Z.
        let turned = Collider::ramp(
            v(0.0, 0.0),
            v(0.2, 0.35),
            Fx32::ONE,
            Fx32::ZERO,
            Fx32::ZERO,
            fx(0.24),
        );
        assert!(near(surface_at(&turned, v(0.35, 0.0)).unwrap(), 0.24));
        assert!(near(surface_at(&turned, v(-0.35, 0.0)).unwrap(), 0.0));
        assert!(near(surface_at(&turned, v(0.0, 0.19)).unwrap(), 0.12));
        // The turned footprint really is the turned rectangle, not its AABB.
        assert_eq!(surface_at(&turned, v(0.0, 0.25)), None);
        assert!(surface_at(&turned, v(0.34, 0.19)).is_some());
    }

    /// The collider frame **is** the render frame: a yawed solid must collide
    /// exactly where its mesh is drawn. `bevy_nds_math::model_matrix` is the
    /// authority (`bevy_nds_3d::recompute_mesh_draw` feeds it the same
    /// `rot.y`), so push its local `+Z` through the matrix and assert the ramp
    /// is highest at the point that comes out. Under the mirrored convention
    /// (`R_y(−yaw)`) the surface there is the ramp's *low* edge — or nothing at
    /// all — so this pins the two implementations together.
    #[test]
    fn yaw_convention_matches_model_matrix() {
        use bevy_nds_math::{FxVec3, model_matrix};
        // A deliberately non-symmetric yaw: ±90° and 180° are their own mirrors.
        let yaw = 0.5_f32;
        let (s, c) = (fx(yaw.sin()), fx(yaw.cos()));
        // Column-major 4×4; column 2 is where the model's local +Z axis points.
        let m = model_matrix(
            FxVec3::default(),
            [(Fx32::ZERO, Fx32::ONE), (s, c), (Fx32::ZERO, Fx32::ONE)],
            FxVec3::from_f32(1.0, 1.0, 1.0),
        );
        let (ax, az) = (Fx32::from_raw(m[8]), Fx32::from_raw(m[10]));
        // The renderer's +Z, scaled to sit just inside a 0.35-deep ramp.
        let reach = fx(0.30);
        let high = FxVec2::new(ax * reach, az * reach);

        let ramp = Collider::ramp(v(0.0, 0.0), v(0.2, 0.35), s, c, Fx32::ZERO, fx(0.24));
        let at_high = surface_at(&ramp, high).expect("the renderer's +Z is on the ramp");
        let at_low = surface_at(&ramp, FxVec2::ZERO - high).expect("…and so is its −Z");
        assert!(
            at_high > fx(0.18) && at_low < fx(0.06),
            "the ramp must climb toward the renderer's local +Z: \
             high {at_high:?} low {at_low:?}"
        );
        // And the crate's own forward map agrees with the matrix, exactly.
        let fwd = world(&ramp, FxVec2::new(Fx32::ZERO, reach));
        assert!((fwd.x - high.x).abs() <= Fx32::from_raw(2), "{fwd:?}");
        assert!((fwd.y - high.y).abs() <= Fx32::from_raw(2), "{fwd:?}");
    }

    /// Mirroring a mesh with a negative `scale.x`/`scale.z` is a normal
    /// authoring move, and it scaled straight through to **negative** half
    /// extents; a negative `scale.y` inverted the vertical span. Both then fed
    /// `Ord::clamp` with `min > max` on the per-frame path ([`rect_contact`],
    /// [`surface`]'s local-Z clamp) — a panic, i.e. a `panic = "abort"` ROM
    /// death, the first frame the avatar came within `bound + radius`.
    ///
    /// [`normalize`] fixes it at construction, so a mirrored solid *is* the
    /// un-mirrored one: same collider, same answers, no work added per frame.
    #[test]
    fn negative_scale_does_not_panic() {
        let b = body();
        let (s, c) = (fx(0.3_f32.sin()), fx(0.3_f32.cos()));

        // A 0.4 × 0.6 box mirrored on both ground axes, span authored upside
        // down (`base` above `top`).
        let bad_box = Collider::block(
            v(0.0, 0.0),
            v(-0.2, -0.3),
            Fx32::ZERO,
            Fx32::ONE,
            fx(0.24),
            Fx32::ZERO,
        );
        assert_eq!(
            bad_box,
            box_at(0.0, 0.0, 0.2, 0.3, 0.24),
            "a mirrored box is the same box"
        );

        // A yawed ramp, likewise mirrored and upside down. It still rises toward
        // local +Z — a ramp facing the other way is authored with `rot.y`.
        let bad_ramp = Collider::ramp(v(1.2, 0.0), v(-0.2, -0.35), s, c, fx(0.24), Fx32::ZERO);
        let good_ramp = Collider::ramp(v(1.2, 0.0), v(0.2, 0.35), s, c, Fx32::ZERO, fx(0.24));
        assert_eq!(bad_ramp, good_ramp, "a mirrored ramp is the same ramp");
        assert!(bad_ramp.slope.raw() > 0, "and it still has a rise");

        // A column: a negative radius is still a disc of that radius.
        let bad_round = Collider::round(v(-1.2, 0.0), fx(-0.2), fx(0.24), Fx32::ZERO);
        assert_eq!(
            bad_round,
            Collider::round(v(-1.2, 0.0), fx(0.2), Fx32::ZERO, fx(0.24))
        );

        let bad = [bad_box, bad_ramp, bad_round];
        let good = [
            box_at(0.0, 0.0, 0.2, 0.3, 0.24),
            good_ramp,
            Collider::round(v(-1.2, 0.0), fx(0.2), Fx32::ZERO, fx(0.24)),
        ];

        // Drive the whole per-frame surface across all three, on a sweep that
        // walks through every footprint: each of these calls used to abort.
        for k in 0..=60 {
            let x = fx(k as f32 * 0.05 - 1.5);
            let from = FxVec2::new(x, fx(-0.9));
            let to = FxVec2::new(x, fx(0.9));
            let p = resolve_move(&bad, from, to, &b, Fx32::ZERO, b.step, wide());
            assert_eq!(
                p,
                resolve_move(&good, from, to, &b, Fx32::ZERO, b.step, wide()),
                "resolve_move differs at k={k}"
            );
            let g = ground_height(&bad, p, &b, Fx32::ZERO, b.step, Fx32::ZERO);
            assert_eq!(
                g,
                ground_height(&good, p, &b, Fx32::ZERO, b.step, Fx32::ZERO),
                "ground_height differs at k={k}"
            );
            // Sane: supported somewhere between the floor and the tallest top,
            // and never left inside a bare footprint.
            assert!(g >= Fx32::ZERO && g <= fx(0.24), "support {g:?} at k={k}");
            for cl in &bad {
                assert!(
                    !inside_bare(cl, local(cl, p)) || surface(cl, local(cl, p).y) <= g,
                    "left inside a solid at k={k}"
                );
            }
        }
    }

    // --- horizontal resolve ---------------------------------------------------

    #[test]
    fn walk_into_tall_box_stops_at_radius() {
        let b = body();
        let cs = [box_at(0.0, 0.0, 0.2, 0.2, 0.5)];
        // Axis aligned: settle exactly on the face + radius (2 raw units of
        // fixed-point slack, i.e. under half a millimetre of world space).
        let p = resolve_move(
            &cs,
            v(-1.0, 0.0),
            v(-0.1, 0.0),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        let want = -(fx(0.2) + b.radius);
        assert!(
            (p.x - want).abs() <= Fx32::from_raw(2),
            "{p:?} want {want:?}"
        );
        assert_eq!(p.y, Fx32::ZERO, "no lateral drift on a head-on approach");

        // Yawed 45°: the settle point lies on the *rotated* face plane, at
        // `half + radius` along the box's local axis the body came in on. With
        // the renderer's `R_y(+yaw)` (see `world`), world −X+Z is the box's
        // local −X at this yaw, so the body settles on the −X face.
        let yaw = core::f32::consts::FRAC_PI_4;
        let cs = [yawed_box(0.0, 0.0, 0.2, 0.2, 0.5, yaw)];
        let p = resolve_move(
            &cs,
            v(-0.45, 0.45),
            v(-0.02, 0.02),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        let lp = local(&cs[0], p);
        let want = fx(0.2) + b.radius;
        assert!(
            (lp.x + want).abs() <= Fx32::from_raw(8),
            "local {lp:?} want x≈−{want:?}"
        );
        assert!(
            lp.y.abs() <= fx(0.02),
            "contact is on the face, not a corner"
        );
        // The distance from the centre is the *rotated* face distance (0.38).
        // Under an AABB approximation the 45°-turned box would present half
        // extents of 0.283 and stop the body ~0.65 out — this catches that.
        let d = p.length();
        assert!(
            (d - want).abs() <= Fx32::from_raw(8),
            "centre distance {d:?}"
        );
    }

    #[test]
    fn kerb_within_step_does_not_block_when_grounded() {
        let b = body();
        // A 0.08 lip against a 0.10 step: walk straight over it.
        let cs = [box_at(0.0, 0.0, 0.2, 0.2, 0.08)];
        let p = resolve_move(
            &cs,
            v(-0.5, 0.0),
            v(0.0, 0.0),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        assert_eq!(
            p,
            v(0.0, 0.0),
            "a kerb inside the step allowance never blocks"
        );
    }

    #[test]
    fn same_kerb_blocks_when_airborne() {
        let b = body();
        let cs = [box_at(0.0, 0.0, 0.2, 0.2, 0.08)];
        // Airborne ⇒ `step_allow` is zero, so the same lip is a wall.
        let p = resolve_move(
            &cs,
            v(-0.5, 0.0),
            v(0.0, 0.0),
            &b,
            Fx32::ZERO,
            Fx32::ZERO,
            wide(),
        );
        let want = -(fx(0.2) + b.radius);
        assert!((p.x - want).abs() <= Fx32::from_raw(2), "{p:?}");
    }

    #[test]
    fn overhang_above_body_height_is_ignored() {
        let b = body();
        // A slab whose underside is above head height: walk under it.
        let slab = Collider::block(
            v(0.0, 0.0),
            v(0.4, 0.4),
            Fx32::ZERO,
            Fx32::ONE,
            fx(0.40), // base above feet (0) + body height (0.32)
            fx(0.60),
        );
        let cs = [slab];
        let p = resolve_move(
            &cs,
            v(-0.5, 0.0),
            v(0.0, 0.0),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        assert_eq!(p, v(0.0, 0.0), "an overhang above the body is not a wall");
        // Lower it to head height and it blocks again.
        let low = Collider::block(
            v(0.0, 0.0),
            v(0.4, 0.4),
            Fx32::ZERO,
            Fx32::ONE,
            fx(0.30),
            fx(0.60),
        );
        assert!(blocks(&low, &b, Fx32::ZERO, b.step, v(0.0, 0.0)));
    }

    #[test]
    fn standing_on_box_is_not_ejected_by_it() {
        let b = body();
        let cs = [box_at(0.0, 0.0, 0.3, 0.3, 0.12)];
        // Feet already at the top: the surface is not above `feet + step`, so
        // walking around up there is unobstructed.
        let p = resolve_move(&cs, v(0.0, 0.0), v(0.1, 0.05), &b, fx(0.12), b.step, wide());
        assert_eq!(p, v(0.1, 0.05));
        assert!(!blocks(&cs[0], &b, fx(0.12), b.step, v(0.0, 0.0)));
    }

    /// The shipped atrium ramp: rise 0.24 over a 0.70 run, axis aligned.
    fn atrium_ramp() -> Collider {
        Collider::ramp(
            v(1.45, 1.05),
            v(0.2, 0.35),
            Fx32::ZERO,
            Fx32::ONE,
            Fx32::ZERO,
            fx(0.24),
        )
    }

    #[test]
    fn jump_frame_on_ramp_does_not_eject() {
        let b = body();
        let cs = [atrium_ramp()];
        // Walking up-slope and pressing Jump: `grounded` clears *this* frame, so
        // the caller's `step_allow` is zero while `feet` is still the surface at
        // the old position. One sub-step further up the slope the surface is
        // ~0.009 higher — and if that counted as a wall, the body (whose centre
        // is inside the ramp's own footprint) would be ejected `half.x + radius`
        // = 0.38 sideways off the ramp.
        let from = v(1.45, 1.05);
        let feet = surface_at(&cs[0], from).expect("standing on the ramp");
        let to = v(1.45, 1.08);
        let p = resolve_move(&cs, from, to, &b, feet, Fx32::ZERO, wide());
        assert_eq!(
            p, to,
            "a jump mid-slope must not throw the body off the ramp"
        );
        assert!(!blocks(&cs[0], &b, feet, Fx32::ZERO, to));
        // The same is true walking *across* the slope, and down it.
        for target in [v(1.52, 1.08), v(1.45, 1.02), v(1.38, 1.05)] {
            assert_eq!(
                resolve_move(&cs, from, target, &b, feet, Fx32::ZERO, wide()),
                target,
                "{target:?}"
            );
        }
    }

    #[test]
    fn airborne_landing_uphill_on_ramp_does_not_eject() {
        let b = body();
        let cs = [atrium_ramp()];
        // Landing on a ramp while still moving up-slope: the feet sit a hair
        // above the surface at the old position and below the surface at the
        // new one — the ~0.003-wide window per frame that used to eject.
        let from = v(1.45, 1.05);
        let feet = surface_at(&cs[0], from).unwrap() + Fx32::from_raw(1);
        let to = v(1.45, 1.09);
        assert_eq!(
            resolve_move(&cs, from, to, &b, feet, Fx32::ZERO, wide()),
            to
        );
        // An airborne body *outside* the footprint is still blocked by the same
        // ramp's tall side, which is what makes this a narrow exception and not
        // a hole in the airborne rule.
        let side = v(1.15, 1.35);
        assert!(blocks(&cs[0], &b, fx(0.02), Fx32::ZERO, side));
    }

    #[test]
    fn ramp_low_edge_is_not_an_airborne_wall() {
        let b = body();
        let ramp = atrium_ramp();
        // The shipped ramp's foot (z = 0.70) is flush with the floor, exactly.
        assert_eq!(surface_at(&ramp, v(1.45, 0.70)), Some(ramp.base));
        assert_eq!(ramp.base, Fx32::ZERO);
        // An airborne body — the frame a jump clears `grounded`, so `step_allow`
        // is zero — standing on the floor anywhere inside the ramp's low-edge
        // skirt must not be blocked by it: the surface it would be "stepping
        // onto" is the floor it is already on. A raw unit of interpolation
        // residue here used to shove it backwards up to 0.18 on the jump frame.
        for z in [0.53, 0.58, 0.62, 0.66, 0.70] {
            let p = v(1.45, z);
            assert!(
                !blocks(&ramp, &b, Fx32::ZERO, Fx32::ZERO, p),
                "the ramp's foot is a wall at z = {z}"
            );
            let to = v(1.45, z + 0.03);
            assert_eq!(
                resolve_move(&[ramp], p, to, &b, Fx32::ZERO, Fx32::ZERO, wide()),
                to,
                "airborne approach displaced at z = {z}"
            );
        }
        // The same ramp's *tall* end is still a wall to a body at floor level.
        assert!(blocks(&ramp, &b, Fx32::ZERO, Fx32::ZERO, v(1.45, 1.35)));
    }

    /// One frame of `move_player`'s tail (`src/player.rs`), replayed exactly:
    /// the horizontal resolve first, then ground → gravity → settle, with the
    /// shipped arena constants. The jump impulse is applied *before* `prev` and
    /// `grounded` are read, as the real controller does (input runs first), so
    /// the jump frame is already airborne with `step_allow` zero — which is the
    /// state both ramp regressions lived in.
    struct Sim {
        pos: FxVec2,
        feet: Fx32,
        vz: Fx32,
        grounded: bool,
    }

    impl Sim {
        /// Advance one frame; returns where the move *wanted* to end up, so the
        /// caller can assert nothing pushed it elsewhere.
        fn step(&mut self, cs: &[Collider], b: &Body, delta: FxVec2, jump: bool) -> FxVec2 {
            let dt = Fx32::ONE / Fx32::from_int(60);
            if jump && self.grounded {
                self.vz = fx(2.2); // Locomotion::arena().jump_impulse
                self.grounded = false;
            }
            let prev = self.feet;
            let grounded = self.grounded;
            let step_allow = if grounded { b.step } else { Fx32::ZERO };
            let to = self.pos + delta;
            let np = resolve_move(cs, self.pos, to, b, prev, step_allow, wide());
            let ceiling = if grounded { prev + b.step } else { prev };
            let ground = ground_height(cs, np, b, prev, ceiling, Fx32::ZERO);
            let vz = self.vz - fx(9.0) * dt; // Locomotion::arena().gravity
            let z = prev + vz * dt;
            let (z, vz, g) = settle_height(z, vz, prev, grounded, ground, b.step);
            self.pos = np;
            self.feet = z;
            self.vz = vz;
            self.grounded = g;
            to
        }
    }

    #[test]
    fn jump_arc_up_the_ramp_never_sinks_or_ejects() {
        let b = body();
        // The ramp **alone**: in the atrium it abuts a box whose skirt would
        // catch a body that fell through, hiding exactly the bug this covers.
        let cs = [atrium_ramp()];
        let dt = Fx32::ONE / Fx32::from_int(60);
        let delta = FxVec2::new(Fx32::ZERO, fx(1.6) * dt); // stowed, up-slope
        let sink = Fx32::from_raw(4);

        // Walk at the ramp from the floor and up it, jumping at every phase:
        // approach, take-off beside the foot, mid-climb, and landing up-slope
        // (the frame the feet sit between the old and the new surface).
        let mut z0 = fx(0.50);
        while z0 <= fx(1.30) {
            for jump_frame in [0u32, 2, 5, 9, 14, 20] {
                let start = FxVec2::new(fx(1.45), z0);
                let feet0 = surface_at(&cs[0], start).unwrap_or(Fx32::ZERO);
                let mut s = Sim {
                    pos: start,
                    feet: feet0,
                    vz: Fx32::ZERO,
                    grounded: true,
                };
                for f in 0..56u32 {
                    // Stop walking near the high edge so the arc is always
                    // judged *on* the ramp (56 frames is a whole jump — the
                    // latest take-off here lands around frame 47).
                    let walk = if s.pos.y >= fx(1.35) {
                        FxVec2::ZERO
                    } else {
                        delta
                    };
                    let to = s.step(&cs, &b, walk, f == jump_frame);
                    assert_eq!(
                        s.pos, to,
                        "frame {f} (jump at {jump_frame}, start z {z0:?}): \
                         the move was displaced — the ramp ejected the body"
                    );
                    if let Some(surf) = surface_at(&cs[0], s.pos) {
                        assert!(
                            s.feet >= surf - sink,
                            "frame {f} (jump at {jump_frame}, start z {z0:?}): \
                             feet {:?} sank below the ramp surface {surf:?}",
                            s.feet
                        );
                    }
                }
                // Whatever it was doing, it ends the arc standing on the ramp.
                if let Some(surf) = surface_at(&cs[0], s.pos) {
                    assert!(s.grounded, "still airborne at the end of the arc");
                    assert!(
                        (s.feet - surf).abs() <= sink,
                        "settled at {:?}, surface {surf:?}",
                        s.feet
                    );
                }
            }
            z0 += fx(0.05);
        }
    }

    #[test]
    fn rising_body_is_not_caught_by_a_low_top() {
        let b = body();
        // The jump frame beside a 0.12 top the body's centre is over: the
        // integrated height is still below that surface, but the body is
        // *rising*, and landing on it would eat the jump. Only a descending
        // body lands.
        let (z, vz, g) = settle_height(fx(0.034), fx(2.05), Fx32::ZERO, false, fx(0.12), b.step);
        assert_eq!(
            (z, vz, g),
            (fx(0.034), fx(2.05), false),
            "a rising body passes a low top"
        );
        // Coming back down through the same top, it lands on it.
        let (z, vz, g) = settle_height(fx(0.11), fx(-0.6), fx(0.14), false, fx(0.12), b.step);
        assert_eq!((z, vz, g), (fx(0.12), Fx32::ZERO, true));
    }

    #[test]
    fn ledge_step_off_does_not_shove() {
        let b = body();
        let cs = [box_at(0.0, 0.0, 0.3, 0.3, 0.12)];
        // Centre crosses the +X edge while standing on top. Support is still
        // found (inflated footprint) and nothing pushes back horizontally.
        let from = v(0.28, 0.0);
        let to = v(0.34, 0.0);
        let p = resolve_move(&cs, from, to, &b, fx(0.12), b.step, wide());
        assert_eq!(p, to, "stepping off a top must not displace the body");
        assert_eq!(
            ground_height(&cs, p, &b, fx(0.12), fx(0.12) + b.step, Fx32::ZERO),
            fx(0.12),
            "still supported just past the edge (ledge forgiveness)"
        );
        // Past `radius` beyond the edge the support drops away — and the box
        // stops blocking at exactly the same place, which is the whole point.
        let far = v(0.3 + 0.19, 0.0);
        assert_eq!(
            ground_height(&cs, far, &b, fx(0.12), fx(0.12) + b.step, Fx32::ZERO),
            Fx32::ZERO
        );
        assert!(!blocks(&cs[0], &b, fx(0.12), Fx32::ZERO, far));
    }

    #[test]
    fn deep_penetration_ejects_least_axis_and_is_idempotent() {
        let b = body();
        // Long in Z, short in X ⇒ the least-penetration escape is along X.
        let cs = [box_at(0.0, 0.0, 0.1, 0.9, 0.5)];
        let inside = v(0.03, 0.4);
        let out = push_out(&cs[0], b.radius, inside);
        let want_x = fx(0.1) + b.radius;
        assert!((out.x - want_x).abs() <= Fx32::from_raw(2), "{out:?}");
        assert_eq!(out.y, inside.y, "the long axis is untouched");
        // On the inflated boundary, so a second pass is a no-op.
        assert!(!blocks(&cs[0], &b, Fx32::ZERO, Fx32::ZERO, out));
        let again = resolve_move(&cs, out, out, &b, Fx32::ZERO, Fx32::ZERO, wide());
        assert_eq!(again, out, "resolve is idempotent on the boundary");
        // A point on the −X side leaves the same way, mirrored.
        let out = push_out(&cs[0], b.radius, v(-0.03, -0.4));
        assert!((out.x + want_x).abs() <= Fx32::from_raw(2), "{out:?}");
    }

    #[test]
    fn round_footprint_reproduces_landmark_collide() {
        let b = body();
        // The pre-#12 constant: LANDMARK_COLLIDE = 0.26 = radius 0.18 + the
        // landmark half extent 0.08. A round footprint reproduces it exactly.
        let cs = [Collider::round(v(0.0, 0.0), fx(0.08), Fx32::ZERO, fx(0.24))];
        let p = resolve_move(
            &cs,
            v(-0.5, 0.0),
            v(-0.1, 0.0),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        assert_eq!(p.x, -(fx(0.08) + b.radius), "raw-exact 0.26 separation");
        assert_eq!(p.x, -Fx32::from_f32(0.26));
        // …from any direction, not just the axis.
        let p = resolve_move(
            &cs,
            v(0.3, 0.3),
            v(0.02, 0.02),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        let d = (p - v(0.0, 0.0)).length();
        assert!((d - fx(0.26)).abs() <= Fx32::from_raw(4), "{d:?}");
    }

    #[test]
    fn slide_along_wall_preserves_tangential_motion() {
        let b = body();
        // A long wall along Z at x = 0; push diagonally into it.
        let cs = [box_at(0.0, 0.0, 0.1, 2.0, 0.5)];
        let from = v(-0.30, 0.0);
        let to = v(-0.24, 0.06); // 0.06 into the wall, 0.06 along it
        let p = resolve_move(&cs, from, to, &b, Fx32::ZERO, b.step, wide());
        let want_x = -(fx(0.1) + b.radius);
        assert!(
            (p.x - want_x).abs() <= Fx32::from_raw(2),
            "stopped at the face: {p:?}"
        );
        assert!(
            (p.y - fx(0.06)).abs() <= Fx32::from_raw(2),
            "tangential motion survives: {p:?}"
        );
    }

    #[test]
    fn fast_move_does_not_tunnel() {
        let b = body();
        // A thin (0.1 wide) box and a 2.0-unit delta straight through it.
        let cs = [box_at(0.0, 0.0, 0.05, 0.05, 0.5)];
        let p = resolve_move(
            &cs,
            v(-0.5, 0.0),
            v(1.5, 0.0),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        let face = -(fx(0.05) + b.radius);
        assert!(p.x < Fx32::ZERO, "never reaches the far side: {p:?}");
        assert!(
            (p.x - face).abs() <= Fx32::from_raw(2),
            "stops at the near face: {p:?} want {face:?}"
        );
    }

    #[test]
    fn substep_count_is_capped() {
        let b = body();
        assert_eq!(substeps(v(0.05, 0.0), b.radius), 1, "a normal frame delta");
        assert_eq!(substeps(v(0.30, 0.0), b.radius), 2);
        assert_eq!(substeps(v(0.50, 0.0), b.radius), 3);
        assert_eq!(substeps(v(9.00, 0.0), b.radius), MAX_SUBSTEPS, "capped");
        // The cap bounds how far one call can travel: MAX_SUBSTEPS × radius.
        let p = resolve_move(
            &[],
            v(0.0, 0.0),
            v(9.0, 0.0),
            &b,
            Fx32::ZERO,
            b.step,
            wide(),
        );
        let reach = b.radius * Fx32::from_int(MAX_SUBSTEPS as i32);
        assert!(
            (p.x - reach).abs() <= Fx32::from_raw(4),
            "{p:?} want {reach:?}"
        );
    }

    #[test]
    fn empty_slice_is_identity() {
        let b = body();
        let to = v(0.31, -0.22);
        assert_eq!(
            resolve_move(&[], v(0.28, -0.20), to, &b, Fx32::ZERO, b.step, wide()),
            to
        );
        assert_eq!(
            ground_height(&[], to, &b, Fx32::ZERO, fx(9.0), Fx32::ZERO),
            Fx32::ZERO,
            "no geometry ⇒ the floor"
        );
        assert!(!blocks_point(&[], to, Fx32::ZERO));
    }

    #[test]
    fn bounds_clamp_inside_loop_never_ejects_outside() {
        let b = body();
        // A box straddling the +X bound: its push-out would throw the body past
        // the edge if the clamp ran first (the pre-#12 ordering bug).
        let bounds = [fx(-2.0), fx(-2.0), fx(2.0), fx(2.0)];
        let cs = [box_at(2.0, 0.0, 0.3, 0.3, 0.5)];
        for target in [v(1.95, 0.0), v(2.5, 0.0), v(1.99, 0.31)] {
            let p = resolve_move(&cs, v(1.2, 0.0), target, &b, Fx32::ZERO, b.step, bounds);
            assert!(p.x <= bounds[2] && p.x >= bounds[0], "{p:?} for {target:?}");
            assert!(p.y <= bounds[3] && p.y >= bounds[1], "{p:?} for {target:?}");
        }
        // A point sitting exactly on the edge with nothing near it is untouched.
        let edge = v(2.0, 0.5);
        assert_eq!(
            resolve_move(&[], edge, edge, &b, Fx32::ZERO, b.step, bounds),
            edge
        );
    }

    #[test]
    fn no_fx32_wrap_at_arena_scale() {
        let b = body();
        // 20.12 holds ±524288; a 400-unit coordinate squares to 2.7e12, which is
        // why the broad reject and the overlap test are raw i64.
        let cs = [box_at(400.0, -400.0, 0.2, 0.2, 0.5)];
        let p = resolve_move(
            &cs,
            v(399.0, -400.0),
            v(399.9, -400.0),
            &b,
            Fx32::ZERO,
            b.step,
            [fx(-500.0), fx(-500.0), fx(500.0), fx(500.0)],
        );
        let want = fx(400.0) - fx(0.2) - b.radius;
        assert!(
            (p.x - want).abs() <= Fx32::from_raw(2),
            "{p:?} want {want:?}"
        );
        assert_eq!(
            ground_height(&cs, v(400.0, -400.0), &b, Fx32::ZERO, fx(9.0), Fx32::ZERO),
            fx(0.5)
        );
    }

    // --- vertical -------------------------------------------------------------

    #[test]
    fn skirt_beside_ramp_does_not_lift() {
        let b = body();
        let cs = [atrium_ramp()];
        // 0.15 outside the ramp's −X side — inside the radius skirt, but plainly
        // standing on the floor. The skirt must not read the ramp's height here,
        // or an invisible `radius`-wide ramp flanks every real one.
        let beside = v(1.45 - 0.2 - 0.15, 1.20);
        assert!(
            surface_at(&cs[0], beside).is_none(),
            "the probe point is genuinely off the ramp"
        );
        assert_eq!(
            ground_height(&cs, beside, &b, Fx32::ZERO, b.step, Fx32::ZERO),
            Fx32::ZERO,
            "a surface above the feet needs the bare footprint"
        );
        // The forgiveness it replaces is still there in the direction that
        // needed it: feet already *at* that height (having walked off the side
        // of the ramp) keep the support until the centre is a radius clear.
        let s = surface(&cs[0], fx(1.20) - fx(1.05));
        assert_eq!(
            ground_height(&cs, beside, &b, s, s + b.step, Fx32::ZERO),
            s,
            "a surface at/below the feet keeps ledge forgiveness"
        );
    }

    #[test]
    fn floor_walk_beside_ramp_stays_on_floor() {
        let b = body();
        let cs = [atrium_ramp()];
        // Walk the whole length of the ramp on the floor, 0.10 outside its −X
        // edge (well inside the 0.18 skirt), grounded, one stowed-speed frame at
        // a time. The feet must never leave the floor.
        let x = fx(1.45 - 0.2 - 0.10);
        let mut feet = Fx32::ZERO;
        let mut z = fx(0.50);
        while z < fx(1.70) {
            let p = FxVec2::new(x, z);
            let ground = ground_height(&cs, p, &b, feet, feet + b.step, Fx32::ZERO);
            let (nz, _, g) = settle_height(feet, Fx32::ZERO, feet, true, ground, b.step);
            assert_eq!(
                nz,
                Fx32::ZERO,
                "levitating at z = {z:?} (ground {ground:?})"
            );
            assert!(g);
            feet = nz;
            z += fx(0.03);
        }
        // …while a centre actually *on* the ramp does climb it (0.93 is low
        // enough on the slope to be a single `step` from the floor).
        let on = ground_height(&cs, v(1.45, 0.93), &b, Fx32::ZERO, b.step, Fx32::ZERO);
        assert!(on > Fx32::ZERO && on <= b.step, "{on:?}");
    }

    #[test]
    fn ground_height_takes_highest_supporting_surface() {
        let b = body();
        // Two overlapping tops plus a ramp; the tallest reachable one wins.
        let cs = [
            box_at(0.0, 0.0, 0.5, 0.5, 0.10),
            box_at(0.2, 0.0, 0.3, 0.3, 0.22),
            Collider::ramp(
                v(-0.6, 0.0),
                v(0.2, 0.3),
                Fx32::ZERO,
                Fx32::ONE,
                Fx32::ZERO,
                fx(0.18),
            ),
        ];
        let ceiling = fx(9.0);
        assert_eq!(
            ground_height(&cs, v(0.2, 0.0), &b, Fx32::ZERO, ceiling, Fx32::ZERO),
            fx(0.22)
        );
        assert_eq!(
            ground_height(&cs, v(-0.4, 0.0), &b, Fx32::ZERO, ceiling, Fx32::ZERO),
            fx(0.10)
        );
        assert_eq!(
            ground_height(&cs, v(9.0, 9.0), &b, Fx32::ZERO, ceiling, Fx32::ZERO),
            Fx32::ZERO
        );
        // The ramp's high edge beats the low box under it.
        let on_ramp = ground_height(&cs, v(-0.6, 0.3), &b, Fx32::ZERO, ceiling, Fx32::ZERO);
        assert!(on_ramp > fx(0.17) && on_ramp <= fx(0.18), "{on_ramp:?}");
        // A ceiling below a surface hides it: only the low box is reachable.
        assert_eq!(
            ground_height(&cs, v(0.2, 0.0), &b, Fx32::ZERO, fx(0.15), Fx32::ZERO),
            fx(0.10)
        );
    }

    #[test]
    fn airborne_above_box_passes_over_and_lands_only_descending() {
        let b = body();
        let cs = [box_at(0.0, 0.0, 0.2, 0.2, 0.24)];
        // Airborne, feet *below* the top: the ceiling hides it — no snap-up
        // onto a pillar while rising past its side.
        assert_eq!(
            ground_height(&cs, v(0.0, 0.0), &b, fx(0.10), fx(0.10), Fx32::ZERO),
            Fx32::ZERO
        );
        // Airborne, feet at/above the top: the surface is available to land on.
        assert_eq!(
            ground_height(&cs, v(0.0, 0.0), &b, fx(0.24), fx(0.24), Fx32::ZERO),
            fx(0.24)
        );
        // …and horizontally the box does not block a body flying over it.
        assert!(!blocks(&cs[0], &b, fx(0.30), Fx32::ZERO, v(0.0, 0.0)));
        // Descending through the top lands; rising through the floor does not.
        let (z, vz, g) = settle_height(fx(0.22), fx(-0.5), fx(0.30), false, fx(0.24), b.step);
        assert_eq!((z, vz, g), (fx(0.24), Fx32::ZERO, true));
        let (z, _, g) = settle_height(fx(0.20), fx(0.5), fx(0.10), false, Fx32::ZERO, b.step);
        assert_eq!(
            (z, g),
            (fx(0.20), false),
            "still rising, nothing to land on"
        );
    }

    #[test]
    fn settle_height_step_down_keeps_grounded() {
        let b = body();
        // Grounded, support dropped 0.08 (inside the 0.10 step): follow it down
        // and stay grounded — otherwise gravity would un-ground the body every
        // frame on a downslope and a jump off a ramp would never fire.
        let (z, vz, g) = settle_height(fx(0.079), fx(-0.15), fx(0.08), true, Fx32::ZERO, b.step);
        assert_eq!((z, vz, g), (Fx32::ZERO, Fx32::ZERO, true));
        // And up the same lip.
        let (z, vz, g) = settle_height(Fx32::ZERO, fx(-0.15), Fx32::ZERO, true, fx(0.08), b.step);
        assert_eq!((z, vz, g), (fx(0.08), Fx32::ZERO, true));
    }

    #[test]
    fn ledge_drop_ungrounds() {
        let b = body();
        // Grounded on a 0.12 top, support falls away to the floor: 0.12 > step,
        // so this is a drop, not a step — the body leaves the ground.
        let z = fx(0.12) - fx(0.0025); // one frame of gravity
        let (z, vz, g) = settle_height(z, fx(-0.15), fx(0.12), true, Fx32::ZERO, b.step);
        assert!(!g, "walking off a 0.12 ledge must unground");
        assert_eq!(vz, fx(-0.15), "velocity is preserved into the fall");
        assert!(z > Fx32::ZERO && z < fx(0.12));
    }

    #[test]
    fn landing_zeroes_vz() {
        let b = body();
        let (z, vz, g) = settle_height(fx(-0.02), fx(-1.4), fx(0.05), false, Fx32::ZERO, b.step);
        assert_eq!((z, vz, g), (Fx32::ZERO, Fx32::ZERO, true));
    }

    #[test]
    fn blocks_point_respects_height() {
        // The cover hook (#26, open): solid between base and the surface only.
        let cs = [box_at(0.0, 0.0, 0.2, 0.2, 0.24)];
        assert!(blocks_point(&cs, v(0.0, 0.0), fx(0.10)));
        assert!(blocks_point(&cs, v(0.19, -0.19), Fx32::ZERO));
        assert!(
            !blocks_point(&cs, v(0.0, 0.0), fx(0.24)),
            "the top is not solid"
        );
        assert!(!blocks_point(&cs, v(0.0, 0.0), fx(0.50)), "above it");
        assert!(!blocks_point(&cs, v(0.5, 0.0), fx(0.10)), "beside it");
        // A raised slab is hollow underneath.
        let slab = [Collider::block(
            v(0.0, 0.0),
            v(0.4, 0.4),
            Fx32::ZERO,
            Fx32::ONE,
            fx(0.40),
            fx(0.60),
        )];
        assert!(!blocks_point(&slab, v(0.0, 0.0), fx(0.20)));
        assert!(blocks_point(&slab, v(0.0, 0.0), fx(0.50)));
    }
}
