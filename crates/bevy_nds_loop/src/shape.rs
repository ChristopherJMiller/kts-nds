//! Stroke **shape classification** — what the player just drew (#29).
//!
//! The loop half of this crate answers *did the stroke close* and *what does it
//! enclose*; this module answers the next question: **which shape was it?** A
//! closed polygon becomes [`LoopShape::Circle`] / [`LoopShape::Triangle`] /
//! [`LoopShape::Square`] via [`classify_loop`], and an open stroke becomes a
//! straight slash via [`classify_open`] + [`crosses_circle`] (the through-cut).
//!
//! It is deliberately **game-agnostic**: no enemy kinds, no vulnerability
//! matrix, no policy about which shape beats what. The caller supplies a
//! polygon (in whatever 2D space it draws in — touch pixels, tactical-map
//! pixels, a projected world plane) and gets a shape plus a 0..=1 quality back.
//!
//! # How a shape is recognised
//!
//! 1. **Arc-length resample** the closed ring to [`RESAMPLE_N`] points
//!    ([`resample_closed`]), so density stops depending on how fast the stylus
//!    moved: a slow corner and a fast straight get the same say.
//! 2. **Count corners by turning angle** ([`corner_count`]): a ±2-sample window
//!    centred on each *gap* between samples — the chord arriving at the gap
//!    versus the one leaving it. A turn sharper than [`COS_CORNER`] flags, and
//!    adjacent flags cluster into one corner (wrap-aware), so a corner smeared
//!    over two samples still counts once.
//! 3. **Cross-check the count against compactness.** A corner count alone is
//!    not enough at capture scale: on a 24-point ring a *perfect* circle already
//!    turns 45° across the window against a 60° threshold, so three or four
//!    ordinary hand-drawn lumps flag as corners. [`regularity`] — the
//!    isoperimetric quotient the loop metrics already compute — is the
//!    independent second opinion: a loop too round to be a triangle
//!    ([`TRIANGLE_MAX_REG`]) or a square ([`SQUARE_MAX_REG`]) is neither,
//!    whatever its corners counted.
//! 4. **3 corners → triangle, 4 → square, anything else → circle-class.** A
//!    pentagon or a lumpy blob is circle-class on purpose: a loop that is not
//!    clearly a triangle or a square captures a circle-vulnerable target exactly
//!    as every loop did before this module existed. Circle-class is the
//!    **fallthrough**, so every gate above can only ever send a loop *back* to
//!    it — the one thing #29 promises a Basic capture.
//!
//! Douglas–Peucker was rejected for step 2: any epsilon that preserves a
//! hand-drawn square's corners also collapses a ~30-point circle into 4–6
//! vertices, which then read as corners.
//!
//! # Cost
//!
//! Everything is 20.12 fixed point ([`bevy_nds_math`]) — no `f32` on the call
//! path — and every entry point is meant to be called **once per closure or
//! once per pen-up**, never per frame. Pixel coordinates are assumed to stay
//! within ±512, so raw² sums fit an `i64` without scaling.

use alloc::vec::Vec;

use bevy_nds_math::{Fx32, FxVec2};

use crate::{densify, perimeter, regularity};

// --- Tuning knobs ------------------------------------------------------------
//
// All **provisional, tune from playtest** (#29 / #26 OQ-3): they are reasoned
// from the geometry below, not yet validated against how a square *feels* to
// draw around a ~8 px blip under fire. They are `pub const` and adjacent so a
// playtest pass can move them in one place.

/// A gap is a **corner** when the turn across it exceeds ~60°, i.e. the dot
/// product of the in/out chord directions falls below this.
///
/// Provisional, tune from playtest. The margins it sits between, measured on a
/// 24-point ring: a circle turns 45° across the window (dot 0.707), a pentagon
/// 72° (0.309), a square 90° (0.0) and a triangle 120° (−0.5).
pub const COS_CORNER: f32 = 0.5;

/// How many points the closed ring is resampled to before corner counting.
///
/// Provisional, tune from playtest. 24 divides evenly by 3 and 4 (so triangle
/// and square sides get whole numbers of samples) and leaves 6 samples per side
/// of a square — enough that a corner's ±2-sample window stays inside one side.
pub const RESAMPLE_N: usize = 24;

/// Loops shorter than this (pixels of closed perimeter) are not shapes — a
/// flick or a tight scribble, not a drawn gesture.
///
/// Provisional, tune from playtest.
pub const MIN_LOOP_PERIM: f32 = 40.0;

/// Minimum [`regularity`] for a closed stroke to be a shape at all. Below it the
/// loop is a sliver or a self-crossing scribble and [`classify_loop`] returns
/// `None`.
///
/// Provisional, tune from playtest. Set **low** on purpose: it is the only
/// recognition gate, so a merely-messy circle still captures exactly as it did
/// before shapes existed (a 12:1 sliver is ≈0.22, a regular pentagon ≈0.87).
pub const SCRIBBLE_FLOOR: f32 = 0.28;

/// A 3-corner count is only a **triangle** while [`regularity`] stays under
/// this. Above it the loop is too *compact* to be a triangle — it is a lumpy
/// circle whose bulges happened to land on three corner windows.
///
/// Provisional, tune from playtest. Measured (see
/// `lumpy_capture_scale_circles_stay_circle_class`) on the real pipeline at
/// capture scale: hand triangles with up to 22 % rounded corners sit at
/// regularity p99 ≈ 0.884 (max 0.920), while three-lobe wobbly circles that
/// falsely counted 3 corners never fell below 0.896. `0.89` splits them.
pub const TRIANGLE_MAX_REG: f32 = 0.89;

/// A 4-corner count is only a **square** while [`regularity`] stays under this
/// — the [`TRIANGLE_MAX_REG`] rule for the four-lobed case, and the looser of
/// the two because a square is intrinsically rounder than a triangle
/// ([`ideal_regularity`] 0.785 against 0.605).
///
/// Provisional, tune from playtest. This is the **deliberately asymmetric**
/// knob of the pair: a four-lobe hand wobble and a heavily rounded hand square
/// are, at a ~30 px capture-scale loop, geometrically the *same curve*, so no
/// classifier separates them and the threshold only chooses which way to err.
/// #29's invariant says err **circle** — a Basic capture must never regress —
/// so `0.91` is set where a drawn square with crisp corners still reads Square
/// (measured 98 % at 0 % corner rounding, 91 % at 8 %) while an 8 % / 10 %
/// four-lobe hand wobble reads Circle (measured 100 % / 98 %). Raising it
/// recovers very round squares at the cost of circle captures; lowering it does
/// the reverse. The two populations still overlap around 0.91–0.94, so this is
/// the knob most likely to move after the §9.12 playtest.
pub const SQUARE_MAX_REG: f32 = 0.91;

/// Minimum end-to-end chord, in pixels, for an open stroke to be a slash.
///
/// Provisional, tune from playtest.
pub const LINE_MIN_LEN: f32 = 24.0;

/// Minimum [`straightness`] (chord ÷ arc length) for an open stroke to be a
/// slash. 0.92 admits a slightly bowed drag but not a hook.
///
/// Provisional, tune from playtest.
pub const LINE_MIN_STRAIGHT: f32 = 0.92;

/// How far, as a fraction of the chord, any point of a slash may stray from the
/// chord itself. Catches the S-curve that [`straightness`] alone forgives.
///
/// Provisional, tune from playtest.
pub const LINE_MAX_DEV_FRAC: f32 = 0.10;

/// [`regularity`] of an ideal equilateral triangle, `π·√3 / 9`. Used as the
/// denominator when scoring a triangle so a perfect one scores 1.0.
const IDEAL_TRIANGLE: f32 = 0.604_599_8;

// --- Types -------------------------------------------------------------------

/// What a closed stroke was read as. Note there is no `Line` — a line cannot be
/// a closed polygon without being a sliver no classifier separates from a
/// scribble; open strokes go through [`classify_open`] instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoopShape {
    /// Neither a triangle nor a square: a circle, an ellipse, a pentagon, a
    /// lumpy blob. Circle-class by design.
    Circle,
    /// Three clustered corners.
    Triangle,
    /// Four clustered corners.
    Square,
}

/// The result of [`classify_loop`]: which shape, how cleanly it was drawn, and
/// the corner count that decided it (kept for HUD instrumentation and tuning).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LoopFit {
    pub shape: LoopShape,
    /// `0..=1` — [`regularity`] measured against the ideal regularity of the
    /// recognised shape, so a perfect square and a perfect circle both score 1.
    pub quality: Fx32,
    /// Clustered corners found on the resampled ring.
    pub corners: usize,
}

// --- Closed-stroke classification --------------------------------------------

/// Arc-length resample of the **closed** ring `poly` (its last→first edge
/// included) to at most `n` evenly spaced points, winding preserved.
///
/// This is what makes classification independent of stylus speed: the raw path
/// has samples where the pen was slow and gaps where it was fast, which biases a
/// turning-angle count badly. Fewer than 3 points (or `n == 0`) is not a ring —
/// the input is returned unchanged.
pub fn resample_closed(poly: &[FxVec2], n: usize) -> Vec<FxVec2> {
    if poly.len() < 3 || n == 0 {
        return poly.to_vec();
    }
    let perim = perimeter(poly);
    if perim <= Fx32::ZERO {
        return poly.to_vec();
    }
    let step = perim / Fx32::from_int(n as i32);
    if step <= Fx32::ZERO {
        return poly.to_vec();
    }
    // Walk the ring as an open path that returns to its start; `densify` emits
    // `poly[0]` plus one point per `step`, and the `n` cap drops the duplicate
    // that would land back on `poly[0]` at exactly one perimeter.
    let mut ring = Vec::with_capacity(poly.len() + 1);
    ring.extend_from_slice(poly);
    ring.push(poly[0]);
    densify(&ring, step, n)
}

/// Count the **corners** of a resampled closed `ring`.
///
/// Each index `i` tests a ±2-sample window centred on the **gap** between
/// `ring[i]` and `ring[i+1]`: the incoming chord is `ring[i] − ring[i−2]` and
/// the outgoing one `ring[i+3] − ring[i+1]` (both wrap-aware). The gap is
/// flagged when the angle between them exceeds the `cos_corner` threshold, and
/// a zero-length chord (coincident samples) is never a corner.
///
/// **Why centre on the gap and not on the sample.** A resampled ring almost
/// never puts a sample exactly on a corner. Chords that meet *at* `ring[i]`
/// then straddle the true corner and each absorb part of the turn, so the
/// measured angle shrinks — measurably: a 72° pentagon corner reads as little
/// as 59.6° that way and slips under a 60° threshold, which made a pentagon
/// count 3 corners and masquerade as a triangle. Gap-centred chords stop at
/// `ring[i]` and restart at `ring[i+1]`, so neither can cross a corner lying
/// between them and the full turn is always seen, wherever the corner fell.
///
/// Flags are then **clustered**: maximal runs of consecutive flagged gaps count
/// as one corner, wrapping around the end of the ring, because a corner sitting
/// on (or very near) a sample flags the gaps on both sides of it. Rings shorter
/// than 8 samples have no room for the window and score 0.
pub fn corner_count(ring: &[FxVec2], cos_corner: Fx32) -> usize {
    let n = ring.len();
    if n < 8 {
        return 0;
    }
    let mut flags = Vec::with_capacity(n);
    for i in 0..n {
        let u = (ring[i] - ring[(i + n - 2) % n]).normalize_or_zero();
        let v = (ring[(i + 3) % n] - ring[(i + 1) % n]).normalize_or_zero();
        // A degenerate direction carries no turn information at all — treating
        // it as a corner would make a stalled stylus sample into a vertex.
        let degenerate =
            (u.x == Fx32::ZERO && u.y == Fx32::ZERO) || (v.x == Fx32::ZERO && v.y == Fx32::ZERO);
        flags.push(!degenerate && u.dot(v) < cos_corner);
    }
    // An everywhere-turning ring (a tight scribble) is one run, not `n` corners.
    if flags.iter().all(|&f| f) {
        return 1;
    }
    // A run is a flagged sample whose predecessor is not flagged — counted
    // around the wrap, so a corner straddling index 0 is still one corner.
    (0..n)
        .filter(|&i| flags[i] && !flags[(i + n - 1) % n])
        .count()
}

/// The [`regularity`] a perfectly drawn `shape` has — the denominator that
/// turns a raw isoperimetric quotient into a 0..=1 quality.
///
/// Circle `1.0`, square `π/4 ≈ 0.785`, equilateral triangle `π√3/9 ≈ 0.605`.
pub fn ideal_regularity(shape: LoopShape) -> Fx32 {
    match shape {
        LoopShape::Circle => Fx32::ONE,
        LoopShape::Square => Fx32::from_f32(core::f32::consts::FRAC_PI_4),
        LoopShape::Triangle => Fx32::from_f32(IDEAL_TRIANGLE),
    }
}

/// Classify a **closed** polygon (as returned by [`find_closed_loop_within`]).
///
/// Gates, in order: too small a perimeter ([`MIN_LOOP_PERIM`]) is not a gesture;
/// a [`regularity`] under [`SCRIBBLE_FLOOR`] is a sliver or a scribble, not a
/// shape. Everything that survives is a shape — 3 clustered corners a triangle
/// and 4 a square **provided** the loop is angular enough for them
/// ([`TRIANGLE_MAX_REG`] / [`SQUARE_MAX_REG`]), anything else circle-class.
///
/// Note the asymmetry that is the point of the design: those two ceilings can
/// only ever move a loop *into* circle-class, never out of it, so the only gate
/// a circle capture ever faces is still [`SCRIBBLE_FLOOR`].
///
/// `quality` is surfaced (HUD, `CaptureResolved`) but the caller is expected to
/// **not** scale effectiveness by it yet: whether loop quality should pay out is
/// still open on #29 / #32.
///
/// [`find_closed_loop_within`]: crate::find_closed_loop_within
pub fn classify_loop(poly: &[FxVec2]) -> Option<LoopFit> {
    if perimeter(poly) < Fx32::from_f32(MIN_LOOP_PERIM) {
        return None;
    }
    let reg = regularity(poly);
    if reg < Fx32::from_f32(SCRIBBLE_FLOOR) {
        return None;
    }
    let ring = resample_closed(poly, RESAMPLE_N);
    let corners = corner_count(&ring, Fx32::from_f32(COS_CORNER));
    // Corner count says *how many* bends; regularity says whether the loop is
    // angular enough for them to be corners at all. Both must agree, and the
    // fallthrough is circle-class — the permissive default (#29).
    let shape = match corners {
        3 if reg < Fx32::from_f32(TRIANGLE_MAX_REG) => LoopShape::Triangle,
        4 if reg < Fx32::from_f32(SQUARE_MAX_REG) => LoopShape::Square,
        _ => LoopShape::Circle,
    };
    let ideal = ideal_regularity(shape);
    let mut quality = if ideal > Fx32::ZERO { reg / ideal } else { reg };
    if quality > Fx32::ONE {
        quality = Fx32::ONE;
    }
    if quality < Fx32::ZERO {
        quality = Fx32::ZERO;
    }
    Some(LoopFit {
        shape,
        quality,
        corners,
    })
}

// --- Open-stroke classification (the slash) ----------------------------------

/// How straight an open `path` is: end-to-end chord ÷ arc length, in `0..=1`.
///
/// `1.0` is a perfectly straight polyline, `2/π ≈ 0.64` a semicircle, and `0` a
/// stroke that returns to where it started. Fewer than 2 points, or a
/// zero-length arc, is `0`.
pub fn straightness(path: &[FxVec2]) -> Fx32 {
    let n = path.len();
    if n < 2 {
        return Fx32::ZERO;
    }
    let mut arc = Fx32::ZERO;
    for w in path.windows(2) {
        arc += (w[1] - w[0]).length();
    }
    if arc <= Fx32::ZERO {
        return Fx32::ZERO;
    }
    (path[n - 1] - path[0]).length() / arc
}

/// Is this open `path` a **slash** — a deliberate straight stroke rather than a
/// drag, a hook or the opening arc of a loop? `Some(straightness)` if so.
///
/// Three independent gates, because each alone has a hole: the chord must be at
/// least [`LINE_MIN_LEN`] (a flick or a tap is not a cut), [`straightness`] must
/// reach [`LINE_MIN_STRAIGHT`] (a hook is not a cut) and **no** point may stray
/// more than [`LINE_MAX_DEV_FRAC`] of the chord from the chord segment (an
/// S-curve averages out to a good straightness but is not a cut).
pub fn classify_open(path: &[FxVec2]) -> Option<Fx32> {
    let n = path.len();
    if n < 2 {
        return None;
    }
    let (a, b) = (path[0], path[n - 1]);
    let chord = (b - a).length();
    if chord < Fx32::from_f32(LINE_MIN_LEN) {
        return None;
    }
    let straight = straightness(path);
    if straight < Fx32::from_f32(LINE_MIN_STRAIGHT) {
        return None;
    }
    // Compare squared, in raw² units, so the per-point test costs no sqrt.
    let max_dev = chord * Fx32::from_f32(LINE_MAX_DEV_FRAC);
    let limit = (max_dev.raw() as i64) * (max_dev.raw() as i64);
    for &p in path {
        if dist_sq_raw(p, a, b) > limit {
            return None;
        }
    }
    Some(straight)
}

/// Squared distance from `p` to the segment `a→b`, in **raw² units**.
///
/// The units are 20.12 squared, i.e. 40.24: a distance of `d` pixels comes back
/// as `(d · 4096)²`. Compare against another raw² value (`(r.raw() as i64)²`),
/// never against an [`Fx32`]. Returning raw² is the point — it is the
/// sqrt-free companion to [`crate::dist_point_segment`], so a per-point loop costs one
/// hardware divide for the projection and no square roots at all.
///
/// Degenerate (`a == b`) segments reduce to the squared distance to `a`, and the
/// projection parameter is clamped to the span exactly as
/// [`crate::dist_point_segment`] clamps it, so the two agree.
pub fn dist_sq_raw(p: FxVec2, a: FxVec2, b: FxVec2) -> i64 {
    let ab = b - a;
    let denom = ab.dot(ab);
    let foot = if denom == Fx32::ZERO {
        a
    } else {
        let mut t = (p - a).dot(ab) / denom;
        if t < Fx32::ZERO {
            t = Fx32::ZERO;
        } else if t > Fx32::ONE {
            t = Fx32::ONE;
        }
        a + ab * t
    };
    let dx = (p.x.raw() - foot.x.raw()) as i64;
    let dy = (p.y.raw() - foot.y.raw()) as i64;
    dx * dx + dy * dy
}

/// Does `path` **cut through** the circle at `center` of `radius` — the open
/// stroke's answer to [`encloses_circle`](crate::encloses_circle)?
///
/// A through-cut, not a touch: some segment must pass within `radius` of the
/// centre **and both endpoints must lie outside the circle**, so a poke, a tap
/// or a stroke that merely ends on the target is not a cut. A cheap AABB reject
/// (the path's bounding box inflated by `radius` must contain the centre) runs
/// first, so a stroke nowhere near the target costs no per-segment work.
///
/// Space-agnostic: `path`, `center` and `radius` just have to share one 2D
/// frame, so a caller converting world units to map pixels does it at the call
/// site.
pub fn crosses_circle(path: &[FxVec2], center: FxVec2, radius: Fx32) -> bool {
    let n = path.len();
    if n < 2 {
        return false;
    }
    // Cheap reject: is the centre even inside the stroke's inflated AABB?
    let (mut min_x, mut max_x) = (path[0].x, path[0].x);
    let (mut min_y, mut max_y) = (path[0].y, path[0].y);
    for p in &path[1..] {
        if p.x < min_x {
            min_x = p.x;
        }
        if p.x > max_x {
            max_x = p.x;
        }
        if p.y < min_y {
            min_y = p.y;
        }
        if p.y > max_y {
            max_y = p.y;
        }
    }
    if center.x < min_x - radius
        || center.x > max_x + radius
        || center.y < min_y - radius
        || center.y > max_y + radius
    {
        return false;
    }
    let r_raw = radius.raw() as i64;
    let r_sq = r_raw * r_raw;
    // Both ends outside: a stroke that starts or stops on the target is a poke.
    if dist_sq_raw(path[0], center, center) < r_sq
        || dist_sq_raw(path[n - 1], center, center) < r_sq
    {
        return false;
    }
    path.windows(2)
        .any(|w| dist_sq_raw(center, w[0], w[1]) < r_sq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dist_point_segment, encloses_circle, find_closed_loop_within, smooth};
    use alloc::vec;

    fn v(x: f32, y: f32) -> FxVec2 {
        FxVec2::from_f32(x, y)
    }

    /// A regular `n`-gon of circumradius `r` centred at `(cx, cy)`, rotated by
    /// `rot` radians. CCW in screen coordinates.
    fn ngon(n: usize, r: f32, cx: f32, cy: f32, rot: f32) -> Vec<FxVec2> {
        (0..n)
            .map(|i| {
                let a = rot + core::f32::consts::TAU * (i as f32) / (n as f32);
                v(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    }

    /// An axis-aligned rectangle of the given full width/height, centred on the
    /// origin of the test space (offset to stay in positive pixel coordinates).
    fn rect(w: f32, h: f32) -> Vec<FxVec2> {
        let (x, y) = (100.0, 100.0);
        vec![
            v(x - w / 2.0, y - h / 2.0),
            v(x + w / 2.0, y - h / 2.0),
            v(x + w / 2.0, y + h / 2.0),
            v(x - w / 2.0, y + h / 2.0),
        ]
    }

    /// A tiny LCG so the jitter tests are deterministic and dependency-free.
    /// Returns a value in `-1.0..=1.0`.
    fn jitter(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((*seed >> 8) as f32 / (1 << 23) as f32) * 2.0 - 1.0
    }

    /// Densify a polygon's edges so a shape authored as 3/4 vertices becomes a
    /// realistic, many-sample drawn stroke before classification.
    fn draw(poly: &[FxVec2], spacing: f32) -> Vec<FxVec2> {
        let mut ring: Vec<FxVec2> = poly.to_vec();
        ring.push(poly[0]);
        densify(&ring, Fx32::from_f32(spacing), 512)
    }

    #[test]
    fn resample_closed_is_uniform_and_preserves_winding() {
        let sq = rect(80.0, 80.0);
        let ring = resample_closed(&sq, 24);
        assert!(
            (23..=24).contains(&ring.len()),
            "24 requested, got {}",
            ring.len()
        );
        // Uniform spacing: perimeter 320 / 24 ≈ 13.33 px per step.
        let step = 320.0 / 24.0;
        for w in ring.windows(2) {
            let d = (w[1] - w[0]).length().to_f32();
            assert!((d - step).abs() < 0.6, "step {d} vs {step}");
        }
        // Winding preserved: the resampled ring encloses the same signed area
        // sense, and starts at the original first vertex.
        assert!((ring[0].x - sq[0].x).to_f32().abs() < 0.01);
        assert!((ring[0].y - sq[0].y).to_f32().abs() < 0.01);
        // …and walks the same way round: the second sample is on the first edge.
        assert!(ring[1].y.to_f32() - sq[0].y.to_f32() < 0.6);
        assert!(ring[1].x.to_f32() > sq[0].x.to_f32());

        // Degenerate inputs come straight back.
        assert_eq!(resample_closed(&[], 24).len(), 0);
        assert_eq!(resample_closed(&[v(1.0, 1.0), v(2.0, 2.0)], 24).len(), 2);
        assert_eq!(resample_closed(&sq, 0).len(), sq.len());
    }

    #[test]
    fn classify_32gon_is_circle_with_zero_corners_and_high_quality() {
        let circle = ngon(32, 40.0, 100.0, 100.0, 0.0);
        let fit = classify_loop(&circle).expect("a 32-gon is a shape");
        assert_eq!(fit.shape, LoopShape::Circle);
        assert_eq!(fit.corners, 0, "a circle has no corners");
        assert!(fit.quality.to_f32() >= 0.95, "quality {:?}", fit.quality);
    }

    #[test]
    fn classify_equilateral_and_scalene_triangles() {
        let equi = draw(&ngon(3, 50.0, 100.0, 100.0, 0.3), 3.0);
        let fit = classify_loop(&equi).expect("a triangle is a shape");
        assert_eq!(fit.shape, LoopShape::Triangle, "corners {}", fit.corners);
        assert_eq!(fit.corners, 3);
        assert!(
            fit.quality.to_f32() >= 0.95,
            "equilateral quality {:?}",
            fit.quality
        );

        // A right scalene triangle: still three corners, lower quality.
        let scalene = draw(&[v(40.0, 40.0), v(140.0, 40.0), v(40.0, 110.0)], 3.0);
        let fit = classify_loop(&scalene).expect("a scalene triangle is a shape");
        assert_eq!(fit.shape, LoopShape::Triangle, "corners {}", fit.corners);
        assert_eq!(fit.corners, 3);
    }

    #[test]
    fn classify_square_rotated_30_and_2to1_rectangle_are_square() {
        for (label, poly) in [
            ("axis-aligned", draw(&rect(80.0, 80.0), 3.0)),
            (
                "rotated 30°",
                draw(
                    &ngon(4, 56.0, 100.0, 100.0, core::f32::consts::FRAC_PI_6),
                    3.0,
                ),
            ),
            ("2:1 rectangle", draw(&rect(100.0, 50.0), 3.0)),
        ] {
            let fit = classify_loop(&poly).unwrap_or_else(|| panic!("{label} is a shape"));
            assert_eq!(
                fit.shape,
                LoopShape::Square,
                "{label} corners {}",
                fit.corners
            );
            assert_eq!(fit.corners, 4, "{label}");
        }
        // The perfect square scores 1.0 against `ideal_regularity(Square)`.
        let fit = classify_loop(&draw(&rect(80.0, 80.0), 3.0)).unwrap();
        assert!(fit.quality.to_f32() >= 0.95, "quality {:?}", fit.quality);
    }

    #[test]
    fn classify_is_scale_invariant_40px_and_160px_square() {
        // The 24-point resample is a *fixed* count, so a small and a large
        // square must classify identically — this is what guards it.
        for side in [40.0_f32, 160.0] {
            let poly = draw(&rect(side, side), (side / 20.0).max(2.0));
            let fit = classify_loop(&poly).unwrap_or_else(|| panic!("{side} px square"));
            assert_eq!(fit.shape, LoopShape::Square, "{side} px: {:?}", fit.corners);
            assert_eq!(fit.corners, 4, "{side} px");
        }
    }

    #[test]
    fn classify_tolerates_jitter() {
        // ±8 % radial jitter on a circle.
        let mut seed = 7u32;
        let wobbly: Vec<FxVec2> = (0..32)
            .map(|i| {
                let a = core::f32::consts::TAU * (i as f32) / 32.0;
                let r = 45.0 * (1.0 + 0.08 * jitter(&mut seed));
                v(100.0 + r * a.cos(), 100.0 + r * a.sin())
            })
            .collect();
        let fit = classify_loop(&wobbly).expect("a wobbly circle is still a shape");
        assert_eq!(fit.shape, LoopShape::Circle, "corners {}", fit.corners);

        // ±2 px jitter on every sample of a drawn square / triangle.
        let mut seed = 11u32;
        let sq: Vec<FxVec2> = draw(&rect(90.0, 90.0), 4.0)
            .into_iter()
            .map(|p| {
                v(
                    p.x.to_f32() + 2.0 * jitter(&mut seed),
                    p.y.to_f32() + 2.0 * jitter(&mut seed),
                )
            })
            .collect();
        // Raw, the jitter is noise the classifier must survive…
        let fit = classify_loop(&sq).expect("a jittered square is a shape");
        assert_eq!(fit.shape, LoopShape::Square, "raw corners {}", fit.corners);
        // …and through `smooth()`, which is exactly what `draw_capture` feeds it.
        let fit = classify_loop(&smooth(&sq)).expect("a smoothed jittered square is a shape");
        assert_eq!(
            fit.shape,
            LoopShape::Square,
            "smoothed corners {}",
            fit.corners
        );

        let mut seed = 23u32;
        let tri: Vec<FxVec2> = draw(&ngon(3, 60.0, 100.0, 100.0, 0.2), 4.0)
            .into_iter()
            .map(|p| {
                v(
                    p.x.to_f32() + 2.0 * jitter(&mut seed),
                    p.y.to_f32() + 2.0 * jitter(&mut seed),
                )
            })
            .collect();
        let fit = classify_loop(&tri).expect("a jittered triangle is a shape");
        assert_eq!(fit.shape, LoopShape::Triangle, "corners {}", fit.corners);
    }

    /// One hand-drawn loop at **capture scale**, through the pipeline
    /// `draw_capture` actually runs: samples laid down at `MIN_SPACING` (4 px),
    /// `smooth`, then `find_closed_loop_within(CLOSE_TOL = 2.0)`.
    ///
    /// `lobes`/`amp` are a **low-frequency radial wobble** — the shape of real
    /// hand noise, and the one `smooth()` cannot remove, unlike the
    /// uncorrelated per-sample jitter in [`classify_tolerates_jitter`]. `± 0.5`
    /// px of uncorrelated noise rides on top. The stroke runs 1.12 turns so the
    /// overshoot crosses its own trail, exactly as a player's does.
    fn hand_loop(lobes: usize, amp: f32, r0: f32, seed0: u32) -> Option<Vec<FxVec2>> {
        let mut seed = seed0.wrapping_mul(2_654_435_761).wrapping_add(12_345);
        let phase = seed0 as f32 * 0.157;
        let n = ((core::f32::consts::TAU * r0 / 4.0) as usize).max(8);
        let path: Vec<FxVec2> = (0..((n as f32 * 1.12) as usize))
            .map(|i| {
                let a = core::f32::consts::TAU * (i as f32) / (n as f32);
                let r = r0 * (1.0 + amp * (lobes as f32 * a + phase).cos());
                v(
                    100.0 + r * a.cos() + 0.5 * jitter(&mut seed),
                    100.0 + r * a.sin() + 0.5 * jitter(&mut seed),
                )
            })
            .collect();
        find_closed_loop_within(&smooth(&path), Fx32::from_f32(2.0))
    }

    /// **The #29 invariant, at the scale it is actually drawn.** The only new
    /// gate a circle capture faces is [`SCRIBBLE_FLOOR`]: a lumpy hand loop
    /// around a ~7.7 px blip must still read circle-class and capture a Basic
    /// exactly as it did before this module existed.
    ///
    /// The loops here are ~12–20 px in radius (a loop drawn around a blip is
    /// *small* — ~75–125 px of perimeter, not the 250–350 px of the ideal
    /// n-gons the rest of this suite uses) and carry a three- or four-lobe
    /// radial wobble, which is what a hand does and what `smooth()` cannot
    /// flatten. On a 24-point ring a perfect circle already turns 45° across
    /// the corner window against a 60° threshold, so those lobes flag as
    /// corners and land on the Triangle/Square arms of the match — the corner
    /// count alone would silently classify 58 % of these as a wrong shape and
    /// score the player zero. [`TRIANGLE_MAX_REG`]/[`SQUARE_MAX_REG`] are the
    /// compactness cross-check that sends them back to circle-class.
    #[test]
    fn lumpy_capture_scale_circles_stay_circle_class() {
        let blip = Fx32::from_f32(7.7); // CAPTURE_RADIUS 0.18 × MAP_SCALE 42.8
        for lobes in [3usize, 4] {
            for amp in [0.08f32, 0.10] {
                for r0 in [12.0f32, 16.0, 20.0] {
                    let (mut loops, mut circles) = (0, 0);
                    for seed0 in 0..24u32 {
                        let Some(poly) = hand_loop(lobes, amp, r0, seed0) else {
                            continue;
                        };
                        // Only loops that would actually capture count: the
                        // question is what a *successful* enclosure classifies as.
                        if !encloses_circle(&poly, v(100.0, 100.0), blip) {
                            continue;
                        }
                        loops += 1;
                        let fit = classify_loop(&poly).unwrap_or_else(|| {
                            panic!(
                                "{lobes}-lobe {amp} r{r0} seed {seed0} \
                                 classified as nothing — a hand circle is never a scribble"
                            )
                        });
                        if fit.shape == LoopShape::Circle {
                            circles += 1;
                        }
                    }
                    assert!(
                        loops >= 20,
                        "{lobes}-lobe {amp} r{r0}: only {loops}/24 seeds enclosed the blip — \
                         the generator, not the classifier, is wrong"
                    );
                    assert!(
                        circles * 100 >= loops * 95,
                        "{lobes}-lobe {amp} wobble at r{r0}: only {circles}/{loops} hand circles \
                         stayed circle-class; a Basic capture must not regress (#29)"
                    );
                }
            }
        }
    }

    /// The other side of the ceiling: it must not have bought circle-safety by
    /// making the square and the triangle undrawable **at the same scale**. A
    /// crisply drawn capture-scale square/triangle still classifies.
    #[test]
    fn capture_scale_squares_and_triangles_still_classify() {
        for (label, sides, want) in [
            ("triangle", 3usize, LoopShape::Triangle),
            ("square", 4, LoopShape::Square),
        ] {
            for r0 in [16.0f32, 22.0, 30.0] {
                let (mut tot, mut ok) = (0, 0);
                for seed0 in 0..24u32 {
                    let mut seed = seed0.wrapping_mul(2_654_435_761).wrapping_add(99);
                    let base = ngon(sides, r0, 100.0, 100.0, seed0 as f32 * 0.21);
                    let drawn: Vec<FxVec2> = draw(&base, 4.0)
                        .into_iter()
                        .map(|p| {
                            v(
                                p.x.to_f32() + 0.8 * jitter(&mut seed),
                                p.y.to_f32() + 0.8 * jitter(&mut seed),
                            )
                        })
                        .collect();
                    let Some(poly) = classify_input(&drawn) else {
                        continue;
                    };
                    tot += 1;
                    if classify_loop(&poly).map(|f| f.shape) == Some(want) {
                        ok += 1;
                    }
                }
                assert!(
                    tot >= 20 && ok * 100 >= tot * 90,
                    "capture-scale {label} r{r0}: only {ok}/{tot} classified as {want:?}"
                );
            }
        }
    }

    /// `smooth` + close, the half of the pipeline shared by both scale tests.
    fn classify_input(drawn: &[FxVec2]) -> Option<Vec<FxVec2>> {
        let sm = smooth(drawn);
        let mut closed = sm.clone();
        closed.push(sm[0]);
        find_closed_loop_within(&closed, Fx32::from_f32(2.0))
    }

    #[test]
    fn corner_smeared_over_two_samples_counts_once_including_wraparound() {
        let cos = Fx32::from_f32(COS_CORNER);
        // A square whose corners land EXACTLY on samples: each corner flags the
        // gap on either side of it, so the raw flag count is 8 and only the run
        // clustering brings it back to 4.
        let on_sample = resample_closed(&rect(80.0, 80.0), 24);
        let raw = raw_corner_flags(&on_sample, cos);
        assert_eq!(raw, 8, "each on-sample corner smears over two gaps");
        assert_eq!(corner_count(&on_sample, cos), 4, "clustered back to 4");

        // The same square started mid-edge, so every corner falls exactly
        // half-way between two samples — the other smearing case.
        let mut shifted = rect(80.0, 80.0);
        let mid = shifted[0] + (shifted[1] - shifted[0]) * Fx32::from_f32(0.5);
        shifted.push(shifted[0]);
        shifted[0] = mid;
        let between = resample_closed(&shifted, 24);
        assert_eq!(corner_count(&between, cos), 4, "corners between samples");

        // Rotating the ring moves a corner across index 0, which is the
        // wrap-around half of the clustering — a separate branch from the
        // interior one, and the count must not move.
        for ring in [&on_sample, &between] {
            for shift in 0..ring.len() {
                let mut r = ring.clone();
                r.rotate_left(shift);
                assert_eq!(corner_count(&r, cos), 4, "shift {shift}");
            }
        }
    }

    /// The unclustered flag count, so a test can show that clustering is doing
    /// real work rather than the ring happening to flag once per corner.
    fn raw_corner_flags(ring: &[FxVec2], cos_corner: Fx32) -> usize {
        let n = ring.len();
        (0..n)
            .filter(|&i| {
                let u = (ring[i] - ring[(i + n - 2) % n]).normalize_or_zero();
                let v = (ring[(i + 3) % n] - ring[(i + 1) % n]).normalize_or_zero();
                u.dot(v) < cos_corner
            })
            .count()
    }

    #[test]
    fn negatives_sliver_scribble_tiny_pentagon() {
        // A 12:1 sliver is a stray drag, not a shape (regularity ≈ 0.22).
        assert!(classify_loop(&draw(&rect(120.0, 10.0), 3.0)).is_none());

        // A self-crossing scribble: the shoelace area largely cancels, so the
        // regularity collapses under the floor.
        let scribble = vec![
            v(40.0, 40.0),
            v(140.0, 60.0),
            v(45.0, 62.0),
            v(138.0, 42.0),
            v(42.0, 50.0),
            v(140.0, 55.0),
            v(44.0, 45.0),
        ];
        assert!(
            classify_loop(&scribble).is_none(),
            "scribble must not classify"
        );

        // Under MIN_LOOP_PERIM: a 9 px square (perimeter 36 < 40).
        assert!(classify_loop(&rect(9.0, 9.0)).is_none());

        // DOCUMENTED: a regular pentagon has 5 corners, so it is circle-class.
        // That is the deliberate rule — anything that is not clearly a triangle
        // or a square captures a circle-vulnerable target, as every loop did
        // before this module existed.
        let fit = classify_loop(&draw(&ngon(5, 55.0, 100.0, 100.0, 0.1), 3.0))
            .expect("a pentagon is a shape");
        assert_eq!(fit.shape, LoopShape::Circle);
        assert_eq!(fit.corners, 5);
    }

    /// **The load-bearing one.** `draw_capture` never hands a clean polygon to
    /// the classifier: it smooths the raw touch samples, finds the
    /// self-crossing, and passes the crossing-first, tail-trimmed polygon that
    /// `find_closed_loop_within` returns. This replays that exact pipeline on a
    /// synthetic 4 px-spaced square touch path with an overshoot past the start.
    #[test]
    fn end_to_end_touch_square_smooth_close_classify() {
        let mut path: Vec<FxVec2> = Vec::new();
        let (x0, y0, side, step) = (60.0_f32, 60.0_f32, 60.0_f32, 4.0_f32);
        let n = (side / step) as i32;
        // Start a little up the left edge so the overshoot crosses the trail.
        for i in 0..n {
            path.push(v(x0, y0 + 12.0 + i as f32 * step));
        }
        for i in 0..n {
            path.push(v(x0 + i as f32 * step, y0 + side + 12.0));
        }
        for i in 0..n {
            path.push(v(x0 + side, y0 + side + 12.0 - i as f32 * step));
        }
        for i in 0..n {
            path.push(v(x0 + side - i as f32 * step, y0 + 12.0));
        }
        // Overshoot: continue down the left edge past the starting point.
        for i in 1..6 {
            path.push(v(x0, y0 + 12.0 + i as f32 * step));
        }

        let smoothed = smooth(&path);
        let poly = find_closed_loop_within(&smoothed, Fx32::from_f32(2.0))
            .expect("the overshoot closes the loop");
        let fit = classify_loop(&poly).expect("the closed polygon is a shape");
        assert_eq!(
            fit.shape,
            LoopShape::Square,
            "a drawn square must read as Square (corners {}, quality {:?})",
            fit.corners,
            fit.quality
        );
        assert_eq!(fit.corners, 4);
    }

    #[test]
    fn straightness_line_arc_and_there_and_back() {
        // A straight polyline is 1.0.
        let line: Vec<FxVec2> = (0..10).map(|i| v(i as f32 * 9.0, 40.0)).collect();
        assert!(
            (straightness(&line).to_f32() - 1.0).abs() < 0.02,
            "{:?}",
            straightness(&line)
        );

        // A semicircle: chord 2r over arc πr = 0.637.
        let arc: Vec<FxVec2> = (0..=16)
            .map(|i| {
                let a = core::f32::consts::PI * (i as f32) / 16.0;
                v(100.0 + 40.0 * a.cos(), 100.0 + 40.0 * a.sin())
            })
            .collect();
        let s = straightness(&arc).to_f32();
        assert!(s < 0.7, "semicircle straightness {s}");

        // Out and back: the chord is ~0.
        let mut back: Vec<FxVec2> = (0..8).map(|i| v(i as f32 * 10.0, 20.0)).collect();
        for i in (0..8).rev() {
            back.push(v(i as f32 * 10.0, 20.0));
        }
        assert!(straightness(&back).to_f32() < 0.02);

        // Degenerate.
        assert_eq!(straightness(&[]), Fx32::ZERO);
        assert_eq!(straightness(&[v(1.0, 1.0)]), Fx32::ZERO);
        assert_eq!(straightness(&[v(1.0, 1.0), v(1.0, 1.0)]), Fx32::ZERO);
    }

    #[test]
    fn classify_open_accepts_slash_rejects_hook_flick_arc_and_short() {
        // A 90 px slash.
        let slash: Vec<FxVec2> = (0..=18).map(|i| v(30.0 + i as f32 * 5.0, 80.0)).collect();
        let q = classify_open(&slash).expect("a straight 90 px stroke is a slash");
        assert!(q.to_f32() >= 0.92, "slash quality {q:?}");

        // A hook: straight then a hard turn at the end.
        let mut hook: Vec<FxVec2> = (0..=12).map(|i| v(30.0 + i as f32 * 5.0, 80.0)).collect();
        for i in 1..=8 {
            hook.push(v(90.0, 80.0 + i as f32 * 5.0));
        }
        assert!(classify_open(&hook).is_none(), "a hook is not a slash");

        // A 10 px flick is too short.
        let flick: Vec<FxVec2> = (0..=5).map(|i| v(30.0 + i as f32 * 2.0, 80.0)).collect();
        assert!(classify_open(&flick).is_none());

        // A shallow arc that sags 20 % of the chord: straightness alone might
        // forgive it, the deviation gate must not.
        let sag: Vec<FxVec2> = (0..=20)
            .map(|i| {
                let t = i as f32 / 20.0;
                v(
                    30.0 + t * 100.0,
                    80.0 + 20.0 * (core::f32::consts::PI * t).sin(),
                )
            })
            .collect();
        assert!(
            classify_open(&sag).is_none(),
            "a sagging arc is not a slash"
        );

        // One point is not a stroke.
        assert!(classify_open(&[v(10.0, 10.0)]).is_none());
        assert!(classify_open(&[]).is_none());
    }

    #[test]
    fn crosses_circle_cases() {
        let c = v(100.0, 100.0);
        let r = Fx32::from_f32(8.0);

        // A through-cut: straight across the circle, both ends well outside.
        let cut: Vec<FxVec2> = (0..=10).map(|i| v(60.0 + i as f32 * 8.0, 100.0)).collect();
        assert!(crosses_circle(&cut, c, r), "a through-cut crosses");

        // An endpoint inside: a poke, not a cut.
        let poke: Vec<FxVec2> = (0..=6).map(|i| v(60.0 + i as f32 * 7.0, 100.0)).collect();
        assert!(
            !crosses_circle(&poke, c, r),
            "a stroke ending on the target is a poke"
        );
        // …and the mirror case, starting inside.
        let mut from_inside = poke.clone();
        from_inside.reverse();
        assert!(!crosses_circle(&from_inside, c, r));

        // Tangent, missing by 1 px.
        let tangent: Vec<FxVec2> = (0..=10)
            .map(|i| v(60.0 + i as f32 * 8.0, 100.0 - 9.0))
            .collect();
        assert!(!crosses_circle(&tangent, c, r), "1 px clear is a miss");

        // AABB miss: nowhere near.
        let away: Vec<FxVec2> = (0..=10).map(|i| v(10.0 + i as f32 * 2.0, 10.0)).collect();
        assert!(!crosses_circle(&away, c, r));

        // Degenerate paths.
        assert!(!crosses_circle(&[], c, r));
        assert!(!crosses_circle(&[c], c, r));
        // A zero-length "segment" at the centre has both ends inside.
        assert!(!crosses_circle(&[c, c], c, r));
    }

    #[test]
    fn dist_sq_raw_agrees_with_dist_point_segment() {
        let cases = [
            // (p, a, b): perpendicular foot inside the span…
            (v(50.0, 30.0), v(20.0, 10.0), v(80.0, 10.0)),
            // …clamped to `a`…
            (v(5.0, 30.0), v(20.0, 10.0), v(80.0, 10.0)),
            // …clamped to `b`…
            (v(120.0, 30.0), v(20.0, 10.0), v(80.0, 10.0)),
            // …a degenerate segment…
            (v(50.0, 30.0), v(20.0, 10.0), v(20.0, 10.0)),
            // …a diagonal, and a point exactly on the segment.
            (v(60.0, 60.0), v(10.0, 90.0), v(90.0, 10.0)),
            (v(50.0, 50.0), v(10.0, 10.0), v(90.0, 90.0)),
        ];
        for (p, a, b) in cases {
            let want = dist_point_segment(p, a, b).to_f32();
            let got = (dist_sq_raw(p, a, b) as f32).sqrt() / 4096.0;
            assert!(
                (got - want).abs() < 0.02,
                "dist_sq_raw {got} vs dist_point_segment {want} for {:?}",
                (p.x.to_f32(), p.y.to_f32())
            );
        }
    }

    #[test]
    fn degenerate_inputs_never_panic() {
        // `Fx32::sqrt` debug-asserts on negatives, so every path that reaches a
        // length must be fed a non-negative squared value even for nonsense
        // input. Run every public entry point over degenerate shapes.
        let same = v(42.0, 42.0);
        let inputs: [Vec<FxVec2>; 8] = [
            vec![],
            vec![same],
            vec![same, same],
            vec![same, same, same],
            vec![v(0.0, 0.0), v(1.0, 0.0), v(2.0, 0.0)],
            vec![same; 7],
            vec![same; 24],
            vec![v(0.0, 0.0); 40],
        ];
        for path in &inputs {
            let _ = classify_loop(path);
            let _ = classify_open(path);
            let _ = straightness(path);
            let _ = crosses_circle(path, same, Fx32::from_f32(4.0));
            let _ = crosses_circle(path, same, Fx32::ZERO);
            let ring = resample_closed(path, RESAMPLE_N);
            let _ = corner_count(&ring, Fx32::from_f32(COS_CORNER));
            let _ = corner_count(path, Fx32::from_f32(COS_CORNER));
            for &p in path {
                let _ = dist_sq_raw(p, same, same);
                let _ = dist_sq_raw(p, v(0.0, 0.0), same);
            }
        }
        for shape in [LoopShape::Circle, LoopShape::Triangle, LoopShape::Square] {
            assert!(ideal_regularity(shape) > Fx32::ZERO);
        }
    }
    /// The property the whole matrix rests on: **only** a 3-gon may count 3 and
    /// **only** a 4-gon may count 4, at every rotation. A pentagon that counted
    /// 3 at some rotations would let a sloppy loop capture a triangle-vulnerable
    /// enemy — that is exactly the bug the gap-centred window fixed, and this
    /// sweep is what would catch its return.
    #[test]
    fn regular_ngon_corner_counts_are_rotation_invariant() {
        let cos = Fx32::from_f32(COS_CORNER);
        for k in 0..40 {
            let rot = k as f32 * 0.04;
            for (n, want) in [(3usize, Some(3usize)), (4, Some(4)), (5, Some(5))] {
                let ring =
                    resample_closed(&draw(&ngon(n, 55.0, 100.0, 100.0, rot), 3.0), RESAMPLE_N);
                assert_eq!(
                    corner_count(&ring, cos),
                    want.unwrap(),
                    "{n}-gon at rot {rot}"
                );
            }
            // Six sides and up are round enough that no corner reads sharply —
            // and crucially never 3 or 4, so they all stay circle-class.
            for n in [6usize, 8, 12, 24, 32] {
                let ring =
                    resample_closed(&draw(&ngon(n, 55.0, 100.0, 100.0, rot), 3.0), RESAMPLE_N);
                let c = corner_count(&ring, cos);
                assert!(c != 3 && c != 4, "{n}-gon at rot {rot} counted {c}");
            }
        }
    }
}
