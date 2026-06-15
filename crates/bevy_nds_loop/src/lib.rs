//! Capture-loop geometry — the pure, host-testable half of the loop-draw
//! capture verb (issue #19, growing into the `bevy_nds_loop` epic #22).
//!
//! The DS-native capture mechanic (a reimagining of Pokémon Ranger's, which
//! originated on this hardware): the player drags the stylus into a loop around
//! enemy "blips"; whatever the loop encloses is captured. This crate is the
//! math behind that — given the raw touch path it answers two questions:
//!
//! 1. **Did the stroke close into a loop?** ([`find_closed_loop`]) — detected by
//!    the newest path segment crossing an earlier one (self-intersection), which
//!    is what makes loop-drawing forgiving: you don't have to return exactly to
//!    where you started, just cross your own trail.
//! 2. **What's inside it?** ([`point_in_polygon`] / [`enclosed`]) — a fixed-point
//!    even-odd ray cast against the closed polygon.
//!
//! Plus [`smooth`] to tame the ~60 Hz, jitter-prone touch stream. Everything is
//! fixed-point ([`bevy_nds_math`]) and FFI-free, so it links into the ROM but is
//! unit-tested on the host. The game owns capturing the path from the touch
//! hardware and all per-frame state; this crate is stateless functions.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use bevy_nds_math::{Fx32, FxVec2};

/// 2D cross product (`z` component of the 3D cross): `a × b = a.x·b.y − a.y·b.x`.
/// Positive when `b` is counter-clockwise from `a`.
#[inline]
fn cross(a: FxVec2, b: FxVec2) -> Fx32 {
    a.x * b.y - a.y * b.x
}

/// Proper intersection point of segments `p1→p2` and `p3→p4`, or `None` if they
/// don't cross within both spans (parallel segments included).
///
/// Solves `p1 + t·(p2−p1) = p3 + u·(p4−p3)` for `t, u ∈ [0, 1]` via the
/// fixed-point divide coprocessor. Touch coordinates are pixel-scale, so the
/// cross-product intermediates stay well within 20.12 range.
pub fn segment_intersection(p1: FxVec2, p2: FxVec2, p3: FxVec2, p4: FxVec2) -> Option<FxVec2> {
    let d1 = p2 - p1;
    let d2 = p4 - p3;
    let denom = cross(d1, d2);
    if denom == Fx32::ZERO {
        return None; // parallel or degenerate
    }
    let diff = p3 - p1;
    let t = cross(diff, d2) / denom;
    let u = cross(diff, d1) / denom;
    if t < Fx32::ZERO || t > Fx32::ONE || u < Fx32::ZERO || u > Fx32::ONE {
        return None;
    }
    Some(p1 + d1 * t)
}

/// If the newest segment of `path` crosses an earlier, non-adjacent segment,
/// return the closed loop polygon (the crossing point followed by the path
/// vertices it encircles); otherwise `None`.
///
/// The earliest crossing is used, so the largest enclosed region wins when the
/// trail crosses itself more than once. Returns `None` for paths too short to
/// form a loop. Call it each frame while drawing: the frame it returns `Some`
/// is the frame the loop closed.
pub fn find_closed_loop(path: &[FxVec2]) -> Option<Vec<FxVec2>> {
    let n = path.len();
    if n < 4 {
        return None;
    }
    let a1 = path[n - 2];
    let a2 = path[n - 1];
    // Skip the segment adjacent to the newest one (shares vertex path[n-2]).
    for i in 0..n - 3 {
        if let Some(x) = segment_intersection(a1, a2, path[i], path[i + 1]) {
            // Loop = crossing point, then the encircled vertices path[i+1..=n-2].
            let mut poly = Vec::with_capacity(n - i);
            poly.push(x);
            poly.extend_from_slice(&path[i + 1..n - 1]);
            return Some(poly);
        }
    }
    None
}

/// Like [`find_closed_loop`], but *laxer*: if the trail doesn't actually
/// self-cross, it still closes when the newest point comes back within `tol` of
/// an earlier vertex (a near-miss loop-back). Exact crossings are tried first
/// (they give a precise polygon); proximity is the fallback.
///
/// The most-recent few vertices are ignored so a slow stroke doesn't "close" on
/// its own tail; the loop must be at least a handful of points around.
pub fn find_closed_loop_within(path: &[FxVec2], tol: Fx32) -> Option<Vec<FxVec2>> {
    if let Some(poly) = find_closed_loop(path) {
        return Some(poly);
    }
    let n = path.len();
    const SKIP_TAIL: usize = 8;
    if n <= SKIP_TAIL {
        return None;
    }
    let last = path[n - 1];
    // Earliest qualifying vertex → largest loop.
    for i in 0..n - SKIP_TAIL {
        if (last - path[i]).length() <= tol {
            return Some(path[i..n].to_vec());
        }
    }
    None
}

/// Resample a polyline to evenly-spaced points `step` apart along its arc
/// length (linear interpolation), capped at `max` points. Fills the gaps a fast
/// stroke leaves between raw samples, so a dot-per-point trail reads as a
/// continuous line rather than a dotted one — the sprite approximation of a
/// drawn stroke (OAM has no line primitive).
pub fn densify(path: &[FxVec2], step: Fx32, max: usize) -> Vec<FxVec2> {
    let mut out = Vec::new();
    let n = path.len();
    if n == 0 || max == 0 {
        return out;
    }
    out.push(path[0]);
    if n == 1 || step <= Fx32::ZERO {
        return out;
    }
    // Distance walked since the last emitted point.
    let mut since = Fx32::ZERO;
    for i in 1..n {
        let a = path[i - 1];
        let seg = path[i] - a;
        let seg_len = seg.length();
        if seg_len <= Fx32::ZERO {
            continue;
        }
        let dir = seg.normalize_or_zero();
        let mut at = Fx32::ZERO; // position along this segment
        loop {
            let need = step - since; // remaining distance to the next emit
            if at + need > seg_len {
                since = since + (seg_len - at);
                break;
            }
            at = at + need;
            out.push(a + dir * at);
            if out.len() >= max {
                return out;
            }
            since = Fx32::ZERO;
        }
    }
    out
}

/// Is point `p` inside polygon `poly` (even-odd / ray-casting rule)?
///
/// Fixed-point; casts a ray along +x and counts edge crossings. Points exactly
/// on an edge are not guaranteed either way (fine for capture — blips are areas,
/// not points).
pub fn point_in_polygon(poly: &[FxVec2], p: FxVec2) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        // Does edge pj→pi straddle the horizontal ray at p.y?
        if (pi.y > p.y) != (pj.y > p.y) {
            // x of the edge at height p.y.
            let t = (p.y - pi.y) / (pj.y - pi.y);
            let x_cross = pi.x + t * (pj.x - pi.x);
            if p.x < x_cross {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Indices of the `points` that lie inside `poly` — the blips a loop captures.
pub fn enclosed(poly: &[FxVec2], points: &[FxVec2]) -> Vec<usize> {
    points
        .iter()
        .enumerate()
        .filter(|(_, p)| point_in_polygon(poly, **p))
        .map(|(i, _)| i)
        .collect()
}

/// 3-point moving-average smoothing of a path, endpoints preserved. Tames the
/// jitter of raw ~60 Hz touch samples before closure/enclosure tests.
pub fn smooth(path: &[FxVec2]) -> Vec<FxVec2> {
    let n = path.len();
    if n < 3 {
        return path.to_vec();
    }
    let third = Fx32::from_int(3).recip();
    let mut out = Vec::with_capacity(n);
    out.push(path[0]);
    for i in 1..n - 1 {
        out.push((path[i - 1] + path[i] + path[i + 1]) * third);
    }
    out.push(path[n - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> FxVec2 {
        FxVec2::from_f32(x, y)
    }

    fn approx(a: Fx32, b: f32) -> bool {
        (a.to_f32() - b).abs() < 0.1
    }

    #[test]
    fn segments_cross_at_expected_point() {
        // A "+" : horizontal vs vertical through (5, 5).
        let x = segment_intersection(v(0.0, 5.0), v(10.0, 5.0), v(5.0, 0.0), v(5.0, 10.0)).unwrap();
        assert!(approx(x.x, 5.0) && approx(x.y, 5.0), "{:?}", (x.x.to_f32(), x.y.to_f32()));
    }

    #[test]
    fn segments_that_miss_return_none() {
        // Disjoint, non-overlapping spans.
        assert!(segment_intersection(v(0.0, 0.0), v(1.0, 0.0), v(0.0, 5.0), v(1.0, 5.0)).is_none());
        // Would cross if extended, but not within the spans.
        assert!(segment_intersection(v(0.0, 0.0), v(1.0, 1.0), v(5.0, 0.0), v(6.0, 1.0)).is_none());
    }

    #[test]
    fn point_in_square() {
        let sq = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        assert!(point_in_polygon(&sq, v(5.0, 5.0)));
        assert!(!point_in_polygon(&sq, v(15.0, 5.0)));
        assert!(!point_in_polygon(&sq, v(5.0, -1.0)));
        assert!(point_in_polygon(&sq, v(1.0, 9.0)));
    }

    #[test]
    fn point_in_concave_polygon() {
        // A "U"/notched shape: a point in the notch is outside.
        let u = [
            v(0.0, 0.0),
            v(10.0, 0.0),
            v(10.0, 10.0),
            v(7.0, 10.0),
            v(7.0, 3.0),
            v(3.0, 3.0),
            v(3.0, 10.0),
            v(0.0, 10.0),
        ];
        assert!(point_in_polygon(&u, v(1.0, 5.0))); // left arm
        assert!(point_in_polygon(&u, v(9.0, 5.0))); // right arm
        assert!(!point_in_polygon(&u, v(5.0, 8.0))); // in the notch
        assert!(point_in_polygon(&u, v(5.0, 1.0))); // in the base
    }

    #[test]
    fn open_path_has_no_loop() {
        // A simple non-crossing zigzag.
        let path = [v(0.0, 0.0), v(2.0, 1.0), v(4.0, 0.0), v(6.0, 1.0)];
        assert!(find_closed_loop(&path).is_none());
    }

    #[test]
    fn self_crossing_path_closes_a_loop() {
        // Draw a box and overshoot so the final segment crosses the first edge:
        // (0,0)->(10,0)->(10,10)->(0,10)->(0,-2) crosses the first segment at ~(0,0).
        let path = [
            v(0.0, 0.0),
            v(10.0, 0.0),
            v(10.0, 10.0),
            v(0.0, 10.0),
            v(0.0, -2.0),
        ];
        let poly = find_closed_loop(&path).expect("should close");
        // The enclosed polygon should contain the box interior...
        assert!(point_in_polygon(&poly, v(5.0, 5.0)));
        // ...and exclude a point well outside it.
        assert!(!point_in_polygon(&poly, v(50.0, 50.0)));
    }

    #[test]
    fn lax_closure_triggers_on_near_miss() {
        // An almost-closed box: the end returns near the start but never crosses.
        // 4px-ish spacing, enough vertices to clear SKIP_TAIL.
        let path = [
            v(0.0, 0.0),
            v(4.0, 0.0),
            v(8.0, 0.0),
            v(10.0, 4.0),
            v(10.0, 8.0),
            v(8.0, 10.0),
            v(4.0, 10.0),
            v(0.0, 8.0),
            v(0.0, 4.0),
            v(1.0, 2.0),
            v(2.0, 1.0), // ~2.2px from the start (0,0)
        ];
        // Exact crossing detection finds nothing here...
        assert!(find_closed_loop(&path).is_none());
        // ...but a lax tolerance closes it, and the loop contains the interior.
        let poly = find_closed_loop_within(&path, Fx32::from_f32(6.0)).expect("lax close");
        assert!(point_in_polygon(&poly, v(5.0, 5.0)));
        // Too tight a tolerance: no closure.
        assert!(find_closed_loop_within(&path, Fx32::from_f32(0.5)).is_none());
    }

    #[test]
    fn densify_fills_even_steps_along_a_line() {
        // A single long segment, step 10 → points at 0,10,20,30,40.
        let line = [v(0.0, 0.0), v(40.0, 0.0)];
        let d = densify(&line, Fx32::from_int(10), 100);
        assert_eq!(d.len(), 5);
        assert!(approx(d[1].x, 10.0) && approx(d[2].x, 20.0) && approx(d[4].x, 40.0));
        // `max` caps the output.
        assert_eq!(densify(&line, Fx32::from_int(10), 3).len(), 3);
    }

    #[test]
    fn enclosed_picks_only_inside_blips() {
        let sq = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        let blips = [v(5.0, 5.0), v(20.0, 5.0), v(2.0, 8.0)];
        let inside = enclosed(&sq, &blips);
        assert_eq!(inside, alloc::vec![0, 2]);
    }

    #[test]
    fn smooth_preserves_endpoints_and_shortens_spikes() {
        let path = [v(0.0, 0.0), v(0.0, 10.0), v(2.0, 0.0)];
        let s = smooth(&path);
        assert_eq!(s[0], v(0.0, 0.0));
        assert_eq!(s[2], v(2.0, 0.0));
        // The middle spike at y=10 is pulled toward the mean (~3.33).
        assert!(approx(s[1].y, 10.0 / 3.0));
    }
}
