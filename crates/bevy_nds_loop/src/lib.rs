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
//! Plus [`smooth`] to tame the ~60 Hz, jitter-prone touch stream, and
//! [`area`] / [`perimeter`] / [`regularity`] loop-quality metrics (the hook for
//! shape-based scoring, #29). The geometry is all fixed-point
//! ([`bevy_nds_math`]) and FFI-free, so it links into the ROM but is unit-tested
//! on the host.
//!
//! On top of that pure core sits a thin Bevy layer ([`LoopPlugin`]), mirroring
//! [`bevy_nds_gesture`]: it gates the [`Touches`] stream into a [`StrokePath`]
//! resource (pen-down starts a stroke, minimum-spacing resampling, pen-up ends
//! it and fires a [`StrokeCompleted`] event). The buffer holds **raw
//! touch-screen pixels** and carries no game knowledge — the consumer maps them
//! to world / tactical-map space and decides when a stroke "counts" (e.g. only
//! while the capture device is deployed).
//!
//! [`bevy_nds_gesture`]: https://docs.rs/bevy_nds_gesture

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_input::touch::{Touches, touch_screen_input_system};
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

/// Shortest distance from point `p` to segment `a→b`. Projects `p` onto the
/// segment, clamping the parameter to `[0, 1]` so it measures to the nearer
/// endpoint when the foot of the perpendicular falls outside the span.
/// Degenerate (`a == b`) segments reduce to the distance to `a`.
fn dist_point_segment(p: FxVec2, a: FxVec2, b: FxVec2) -> Fx32 {
    let ab = b - a;
    let denom = ab.dot(ab);
    if denom == Fx32::ZERO {
        return (p - a).length();
    }
    let mut t = (p - a).dot(ab) / denom;
    if t < Fx32::ZERO {
        t = Fx32::ZERO;
    } else if t > Fx32::ONE {
        t = Fx32::ONE;
    }
    (p - (a + ab * t)).length()
}

/// Is the circle of radius `radius` centred at `center` *fully* inside `poly`?
///
/// The circle-vulnerable capture test (#26): a loop only captures an enemy when
/// it encloses the enemy's whole footprint, not merely its centre. True iff the
/// centre is inside **and** no polygon edge comes within `radius` of it (so the
/// circle can't poke through a side). `radius <= 0` degrades to a plain
/// point-in-polygon test.
pub fn encloses_circle(poly: &[FxVec2], center: FxVec2, radius: Fx32) -> bool {
    if !point_in_polygon(poly, center) {
        return false;
    }
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        if dist_point_segment(center, poly[j], poly[i]) < radius {
            return false;
        }
        j = i;
    }
    true
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

// --- Loop quality metrics ----------------------------------------------------

/// Enclosed area of polygon `poly` (shoelace formula), always non-negative.
///
/// Accumulates the cross-product sum in `i64` raw units before halving, so a
/// big loop (a near-full-screen ~256×192 px stroke) can't overflow the 20.12
/// range mid-sum the way a `Fx32` accumulator would. The result — the area
/// itself — comfortably fits `Fx32`.
pub fn area(poly: &[FxVec2]) -> Fx32 {
    let n = poly.len();
    if n < 3 {
        return Fx32::ZERO;
    }
    let mut acc: i64 = 0;
    let mut j = n - 1;
    for i in 0..n {
        // Each term is a 20.12 product (`>> 12` to rescale), summed in i64.
        acc += (poly[j].x.raw() as i64 * poly[i].y.raw() as i64) >> 12;
        acc -= (poly[i].x.raw() as i64 * poly[j].y.raw() as i64) >> 12;
        j = i;
    }
    Fx32::from_raw((acc.abs() / 2) as i32)
}

/// Total edge length of `poly` as a **closed** loop (includes the last→first
/// edge). Uses the fixed-point hardware sqrt per segment.
pub fn perimeter(poly: &[FxVec2]) -> Fx32 {
    let n = poly.len();
    if n < 2 {
        return Fx32::ZERO;
    }
    let mut total = Fx32::ZERO;
    let mut j = n - 1;
    for i in 0..n {
        total += (poly[i] - poly[j]).length();
        j = i;
    }
    total
}

/// Loop "regularity" — the isoperimetric quotient `4π·A / P²`, in `[0, 1]`.
///
/// `1.0` is a perfect circle; a square is ≈ `0.79`; a thin sliver or a jagged
/// scribble trends toward `0`. This is the shape-quality hook for later scoring
/// (#29) — "how clean was that capture loop." Returns `0` for a degenerate loop.
///
/// Computed as `((A / P) / P) · 4π` so the intermediates stay inside the 20.12
/// range (a raw `P²` would overflow for a large loop).
pub fn regularity(poly: &[FxVec2]) -> Fx32 {
    let p = perimeter(poly);
    if p <= Fx32::ZERO {
        return Fx32::ZERO;
    }
    let four_pi = Fx32::from_f32(4.0 * core::f32::consts::PI);
    area(poly) / p / p * four_pi
}

// --- Stroke rasterization (the painted line, #35) ----------------------------

/// Round a 20.12 fixed-point coordinate to the nearest integer pixel, staying in
/// raw fixed-point arithmetic (no soft-float — this is on the per-frame stroke
/// path). Adds a half (`1 << 11` = 0.5 in 20.12) then arithmetic-shifts down.
#[inline]
fn round_i32(x: Fx32) -> i32 {
    (x.raw() + (1 << 11)) >> 12
}

/// Rasterize the segment `a → b` into integer pixels, invoking `plot(x, y)` once
/// per pixel along the line (endpoints included), via integer Bresenham.
///
/// The replacement for the OAM dot-trail ([`densify`]): a true 1-px line instead
/// of a resampled row of square sprites (issue #35). `plot` receives **raw**
/// pixel coordinates that may fall outside any framebuffer — the caller bounds-
/// checks before writing VRAM. No allocation and no floating point, so it runs
/// per frame on the ARM9; the host tests pass a recording closure.
pub fn rasterize_line(a: FxVec2, b: FxVec2, mut plot: impl FnMut(i32, i32)) {
    let (x0, y0) = (round_i32(a.x), round_i32(a.y));
    let (x1, y1) = (round_i32(b.x), round_i32(b.y));
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        plot(x, y);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// Rasterize a whole polyline by chaining [`rasterize_line`] over consecutive
/// vertices. Shared joints are plotted by both adjoining segments — harmless for
/// an idempotent framebuffer write (same colour), so no dedup is done.
pub fn rasterize_polyline(points: &[FxVec2], mut plot: impl FnMut(i32, i32)) {
    match points.len() {
        0 => {}
        1 => plot(round_i32(points[0].x), round_i32(points[0].y)),
        _ => {
            for w in points.windows(2) {
                rasterize_line(w[0], w[1], &mut plot);
            }
        }
    }
}

/// Stamp a filled disc of the given pixel `radius` centred at `(cx, cy)`,
/// plotting each covered pixel. `radius <= 0` plots the single centre pixel.
/// The brush footprint that gives the painted stroke its width.
pub fn stamp_disc(cx: i32, cy: i32, radius: i32, mut plot: impl FnMut(i32, i32)) {
    if radius <= 0 {
        plot(cx, cy);
        return;
    }
    let r2 = radius * radius;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= r2 {
                plot(cx + dx, cy + dy);
            }
        }
    }
}

/// Rasterize a polyline as a brush stroke of the given `radius`: the centre line
/// via [`rasterize_polyline`], stamping a [`stamp_disc`] at every pixel. `radius
/// == 0` degrades to a 1-px [`rasterize_polyline`]. Pixels repeat where discs
/// overlap — fine for a framebuffer paint.
pub fn rasterize_thick_polyline(points: &[FxVec2], radius: i32, mut plot: impl FnMut(i32, i32)) {
    if radius <= 0 {
        rasterize_polyline(points, plot);
        return;
    }
    rasterize_polyline(points, |x, y| stamp_disc(x, y, radius, &mut plot));
}

// --- Anti-aliased stroke rasterization (#35) ---------------------------------
//
// The ½-resolution painted stroke lives in a 128×128 sub-engine bitmap that the
// hardware upscales 2× (nearest-neighbour) to fill the screen. Aliased pixels
// would upscale to blocky stairs, so we spend the 16bpp colour depth on
// **sub-pixel edge coverage** (Wu's algorithm): each edge pixel carries a 0..=255
// coverage the consumer blends into the line colour. All fixed-point — no
// soft-float on the per-frame path.

/// Floor a 20.12 coordinate to its integer part (toward −∞; arithmetic shift).
#[inline]
fn floor_i32(x: Fx32) -> i32 {
    x.raw() >> 12
}

/// The fractional part of a 20.12 coordinate as a 0..=255 coverage byte.
#[inline]
fn frac_u8(x: Fx32) -> u8 {
    // Low 12 bits are the fraction in `0..4096`; scale to `0..=255`.
    (((x.raw() & 0xFFF) * 255) >> 12) as u8
}

/// Rasterize segment `a → b` as a 1-px **anti-aliased** line (Xiaolin Wu),
/// invoking `plot(x, y, coverage)` where `coverage` is `0..=255` — how much of
/// that pixel the line covers. Each column across the major axis emits the two
/// straddling pixels with complementary coverage (they sum to 255), so a shallow
/// line reads as a smooth ramp rather than a staircase; an axis-aligned or exact
/// 45° line collapses to a single full-coverage pixel per step.
///
/// The consumer should combine overlapping writes by **max** coverage (a stroke
/// crosses itself and joins segments), never additively — additive blend would
/// over-brighten joints. No allocation, no floating point.
pub fn rasterize_line_aa(mut a: FxVec2, mut b: FxVec2, mut plot: impl FnMut(i32, i32, u8)) {
    // Work in a frame whose major axis is X; `steep` remaps the plot back.
    let steep = (b.y - a.y).raw().abs() > (b.x - a.x).raw().abs();
    if steep {
        core::mem::swap(&mut a.x, &mut a.y);
        core::mem::swap(&mut b.x, &mut b.y);
    }
    if a.x > b.x {
        core::mem::swap(&mut a, &mut b);
    }
    let mut emit = |px: i32, py: i32, cov: u8| {
        if cov == 0 {
            return;
        }
        if steep {
            plot(py, px, cov);
        } else {
            plot(px, py, cov);
        }
    };
    let x0 = round_i32(a.x);
    let x1 = round_i32(b.x);
    let dx = b.x - a.x;
    let gradient = if dx == Fx32::ZERO {
        Fx32::ZERO
    } else {
        (b.y - a.y) / dx
    };
    // y at the centre of column x0, stepped by `gradient` each column.
    let mut intery = a.y + gradient * (Fx32::from_int(x0) - a.x);
    for x in x0..=x1 {
        let iy = floor_i32(intery);
        let frac = frac_u8(intery);
        emit(x, iy, 255 - frac); // upper pixel
        emit(x, iy + 1, frac); // lower pixel
        intery += gradient;
    }
}

/// Anti-aliased polyline: [`rasterize_line_aa`] over consecutive vertices. A
/// single point plots at full coverage. Joints are written by both adjoining
/// segments — see [`rasterize_line_aa`] on combining by max coverage.
pub fn rasterize_polyline_aa(points: &[FxVec2], mut plot: impl FnMut(i32, i32, u8)) {
    match points.len() {
        0 => {}
        1 => plot(round_i32(points[0].x), round_i32(points[0].y), 255),
        _ => {
            for w in points.windows(2) {
                rasterize_line_aa(w[0], w[1], &mut plot);
            }
        }
    }
}

// --- Feathered glow brush (#35) ----------------------------------------------
//
// A soft halo around the painted stroke: full brightness in a small core, then a
// smooth quadratic falloff to zero. Stamped along the stroke's centreline, the
// feather *is* the anti-aliasing — the hard 1-px edge dissolves into bloom, so
// the 2× upscale's blockiness reads as glow. Coverage must be combined by **max**
// (a CPU buffer), never additively, or overlaps and self-crossings over-brighten.

/// Stamp a radially **feathered** brush at `(cx, cy)`: coverage `255` within
/// `core` px, falling off quadratically to `0` at `radius` px. `plot(x, y,
/// coverage)` fires once per covered pixel. `radius <= 0` plots the centre only.
/// No allocation, no floating point, no sqrt (the falloff is on squared
/// distance, which is a smoother glow than a linear ramp anyway).
pub fn stamp_feathered(
    cx: i32,
    cy: i32,
    core: i32,
    radius: i32,
    mut plot: impl FnMut(i32, i32, u8),
) {
    if radius <= 0 {
        plot(cx, cy, 255);
        return;
    }
    let core = core.clamp(0, radius);
    let r2 = radius * radius;
    let c2 = core * core;
    let span = (r2 - c2).max(1) as i64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let d2 = dx * dx + dy * dy;
            if d2 > r2 {
                continue;
            }
            let cov = if d2 <= c2 {
                255
            } else {
                // (r2 - d2) / (r2 - c2) · 255, in `0..255` since c2 < d2 <= r2.
                (((r2 - d2) as i64 * 255) / span) as u8
            };
            if cov > 0 {
                plot(cx + dx, cy + dy, cov);
            }
        }
    }
}

/// Rasterize a polyline as a **glowing** stroke: walk the 1-px centreline
/// ([`rasterize_polyline`]) and stamp a [`stamp_feathered`] brush at every pixel,
/// giving a continuous tube with a bright core and a soft halo. `core`/`radius`
/// are the brush's flat-core and falloff-edge radii in pixels. The consumer's
/// `plot` must **max-combine** overlapping coverage.
///
/// Generic over `plot`, so it monomorphizes into the *caller's* crate — fine for
/// host tests, but on the ARM9 hot path prefer [`paint_glow`], which is concrete
/// (stays in this optimised crate) and uses a coverage LUT instead of a per-pixel
/// divide.
pub fn rasterize_glow_polyline(
    points: &[FxVec2],
    core: i32,
    radius: i32,
    mut plot: impl FnMut(i32, i32, u8),
) {
    rasterize_polyline(points, |x, y| {
        stamp_feathered(x, y, core, radius, &mut plot)
    });
}

/// Paint a glowing stroke straight into a `width × height` **coverage buffer**
/// (row-major, one byte per pixel), max-combining overlaps — the per-frame path
/// for the #35 painted stroke. Everything hot lives here: a **non-generic**
/// signature (compiled once in this opt-3 crate, not the caller's), a
/// squared-distance → coverage **LUT** built once per call (no per-pixel divide),
/// and in-bounds writes only. Points are in canvas pixels; `core`/`radius` are
/// the flat-bright and falloff radii. Existing buffer contents are max-combined
/// (clear it first for a fresh frame).
pub fn paint_glow(
    cov: &mut [u8],
    width: usize,
    height: usize,
    points: &[FxVec2],
    core: i32,
    radius: i32,
) {
    if points.is_empty() || width == 0 || height == 0 {
        return;
    }
    let radius = radius.max(0);
    let r2 = radius * radius;
    let core = core.clamp(0, radius);
    let c2 = core * core;
    let span = (r2 - c2).max(1) as i64;
    // Squared-distance → coverage, indexed by `d2` in `0..=r2`. Built once.
    let mut lut = alloc::vec![0u8; r2 as usize + 1];
    for (d2, slot) in lut.iter_mut().enumerate() {
        let d2 = d2 as i32;
        *slot = if d2 <= c2 {
            255
        } else {
            (((r2 - d2) as i64 * 255) / span) as u8
        };
    }
    let (w, h) = (width as i32, height as i32);
    let mut stamp = |cx: i32, cy: i32| {
        for dy in -radius..=radius {
            let y = cy + dy;
            if y < 0 || y >= h {
                continue;
            }
            let row = y as usize * width;
            for dx in -radius..=radius {
                let d2 = dx * dx + dy * dy;
                if d2 > r2 {
                    continue;
                }
                let x = cx + dx;
                if x < 0 || x >= w {
                    continue;
                }
                let c = lut[d2 as usize];
                let i = row + x as usize;
                if c > cov[i] {
                    cov[i] = c; // max-combine
                }
            }
        }
    };
    rasterize_polyline(points, stamp);
}

// --- Touch-stream path buffer (the Bevy layer) -------------------------------

/// The in-progress stylus stroke, in **raw touch-screen pixels** (x `0..=255`,
/// y `0..=191`, matching [`Touches`]). [`LoopPlugin`] rebuilds it each frame
/// from the touch stream: pen-down starts a fresh stroke, each subsequent
/// sample is appended only once it is at least [`min_spacing`](Self::min_spacing)
/// px from the previous one (dedup + jitter rejection), and pen-up leaves the
/// completed stroke in place until the next pen-down clears it.
///
/// Game-agnostic by design: it holds no world/map mapping and no notion of
/// "deployed". The consumer reads [`points`](Self::points), maps them to
/// whatever space it captures in, and feeds them to [`find_closed_loop_within`]
/// / [`enclosed`].
#[derive(Resource, Debug, Clone)]
pub struct StrokePath {
    /// The stroke's points so far, oldest first, in touch-screen pixels.
    pub points: Vec<FxVec2>,
    /// Minimum pixel spacing between retained samples. Defaults to 4 px.
    pub min_spacing: Fx32,
    /// Whether the pen was down last frame (drives down/up edge detection).
    down: bool,
}

impl Default for StrokePath {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            min_spacing: Fx32::from_int(4),
            down: false,
        }
    }
}

impl StrokePath {
    /// True while a stroke is actively being drawn (the pen is down).
    pub fn is_drawing(&self) -> bool {
        self.down
    }
}

/// Fired on the frame the pen lifts, carrying the just-completed stroke (raw
/// touch-screen pixels). A one-shot companion to polling [`StrokePath`] — handy
/// for resolving a capture exactly when the loop is released.
#[derive(Event, Debug, Clone)]
pub struct StrokeCompleted(pub Vec<FxVec2>);

/// Accumulate the touch stream into [`StrokePath`], emitting [`StrokeCompleted`]
/// when the pen lifts. Runs after Bevy's touch system so [`Touches`] is current.
fn accumulate_stroke(
    touches: Res<Touches>,
    mut stroke: ResMut<StrokePath>,
    mut completed: EventWriter<StrokeCompleted>,
) {
    match touches.iter().next() {
        Some(touch) => {
            let p = FxVec2::from_f32(touch.position().x, touch.position().y);
            if !stroke.down {
                // Pen-down edge: start a fresh stroke.
                stroke.points.clear();
                stroke.points.push(p);
                stroke.down = true;
            } else {
                let far_enough = stroke
                    .points
                    .last()
                    .is_none_or(|&last| (p - last).length() >= stroke.min_spacing);
                if far_enough {
                    stroke.points.push(p);
                }
            }
        }
        None => {
            if stroke.down {
                // Pen-up edge: publish the finished stroke (left in `points` for
                // this frame; cleared on the next pen-down).
                stroke.down = false;
                if !stroke.points.is_empty() {
                    completed.write(StrokeCompleted(stroke.points.clone()));
                }
            }
        }
    }
}

/// Maintains the [`StrokePath`] resource and the [`StrokeCompleted`] event from
/// the [`Touches`] stream. Requires `bevy_nds_input`'s plugin (for `Touches`);
/// the pure geometry functions can be used without it.
pub struct LoopPlugin;

impl Plugin for LoopPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StrokePath>()
            .add_event::<StrokeCompleted>()
            .add_systems(
                PreUpdate,
                accumulate_stroke.after(touch_screen_input_system),
            );
    }
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
        assert!(
            approx(x.x, 5.0) && approx(x.y, 5.0),
            "{:?}",
            (x.x.to_f32(), x.y.to_f32())
        );
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

    #[test]
    fn area_of_square_and_triangle() {
        let sq = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        assert!(approx(area(&sq), 100.0));
        // Winding direction must not matter (absolute area).
        let cw = [v(0.0, 0.0), v(0.0, 10.0), v(10.0, 10.0), v(10.0, 0.0)];
        assert!(approx(area(&cw), 100.0));
        // Right triangle, legs 6 and 8 → area 24.
        let tri = [v(0.0, 0.0), v(6.0, 0.0), v(0.0, 8.0)];
        assert!(approx(area(&tri), 24.0));
        // Degenerate.
        assert_eq!(area(&[v(0.0, 0.0), v(1.0, 1.0)]), Fx32::ZERO);
    }

    #[test]
    fn area_handles_large_loop_without_overflow() {
        // A near-full-screen box (~256×192): the i64 shoelace accumulation must
        // not wrap the way a Fx32 accumulator would.
        let big = [v(2.0, 2.0), v(254.0, 2.0), v(254.0, 190.0), v(2.0, 190.0)];
        assert!(approx(area(&big), 252.0 * 188.0));
    }

    #[test]
    fn perimeter_of_square() {
        let sq = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        assert!(approx(perimeter(&sq), 40.0));
    }

    #[test]
    fn regularity_circle_beats_square_beats_sliver() {
        // Approximate a circle (radius 10) with 24 segments.
        let mut circle = alloc::vec::Vec::new();
        for k in 0..24 {
            let a = core::f32::consts::TAU * (k as f32) / 24.0;
            circle.push(v(10.0 * a.cos(), 10.0 * a.sin()));
        }
        let sq = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        let sliver = [v(0.0, 0.0), v(20.0, 0.0), v(20.0, 0.5), v(0.0, 0.5)];

        let (rc, rs, rl) = (
            regularity(&circle).to_f32(),
            regularity(&sq).to_f32(),
            regularity(&sliver).to_f32(),
        );
        // Circle ≈ 1 (polygon approximation slightly under), square ≈ 0.785.
        assert!(rc > 0.95 && rc <= 1.05, "circle reg = {rc}");
        assert!((rs - 0.785).abs() < 0.02, "square reg = {rs}");
        assert!(
            rl < rs,
            "sliver {rl} should be less regular than square {rs}"
        );
        assert_eq!(regularity(&[v(0.0, 0.0), v(1.0, 1.0)]), Fx32::ZERO);
    }

    #[test]
    fn dist_point_segment_cases() {
        let a = v(0.0, 0.0);
        let b = v(10.0, 0.0);
        // Perpendicular foot inside the span.
        assert!(approx(dist_point_segment(v(5.0, 4.0), a, b), 4.0));
        // On the segment.
        assert!(approx(dist_point_segment(v(3.0, 0.0), a, b), 0.0));
        // Past the end → distance to the nearer endpoint (5-12-13 triangle).
        assert!(approx(dist_point_segment(v(13.0, 4.0), a, b), 5.0));
        // Degenerate segment → distance to the point.
        assert!(approx(dist_point_segment(v(3.0, 4.0), a, a), 5.0));
    }

    #[test]
    fn circle_fully_inside_square() {
        let sq = [v(0.0, 0.0), v(20.0, 0.0), v(20.0, 20.0), v(0.0, 20.0)];
        // Centre + small radius clears every edge.
        assert!(encloses_circle(&sq, v(10.0, 10.0), Fx32::from_f32(5.0)));
        // Same centre, radius reaches the wall → pokes out.
        assert!(!encloses_circle(&sq, v(10.0, 10.0), Fx32::from_f32(11.0)));
        // Centre near a corner: fits as a point but the circle crosses the side.
        assert!(encloses_circle(&sq, v(3.0, 3.0), Fx32::from_f32(2.0)));
        assert!(!encloses_circle(&sq, v(3.0, 3.0), Fx32::from_f32(4.0)));
        // Centre outside is never enclosed.
        assert!(!encloses_circle(&sq, v(25.0, 10.0), Fx32::from_f32(1.0)));
    }

    #[test]
    fn circle_zero_radius_is_point_test() {
        let sq = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        assert!(encloses_circle(&sq, v(5.0, 5.0), Fx32::ZERO));
        assert!(!encloses_circle(&sq, v(15.0, 5.0), Fx32::ZERO));
    }

    /// Collect the deduplicated set of pixels a rasterizer plots.
    fn pixels(
        f: impl FnOnce(&mut dyn FnMut(i32, i32)),
    ) -> alloc::collections::BTreeSet<(i32, i32)> {
        let mut set = alloc::collections::BTreeSet::new();
        let mut plot = |x: i32, y: i32| {
            set.insert((x, y));
        };
        f(&mut plot);
        set
    }

    #[test]
    fn rasterize_horizontal_and_vertical_lines() {
        let h = pixels(|p| rasterize_line(v(0.0, 0.0), v(5.0, 0.0), p));
        assert_eq!(h.len(), 6); // 0..=5
        assert!(h.iter().all(|&(_, y)| y == 0));
        assert!(h.contains(&(0, 0)) && h.contains(&(5, 0)));

        let vert = pixels(|p| rasterize_line(v(2.0, 0.0), v(2.0, 4.0), p));
        assert_eq!(vert.len(), 5);
        assert!(vert.iter().all(|&(x, _)| x == 2));
    }

    #[test]
    fn rasterize_diagonal_line_hits_endpoints() {
        let d = pixels(|p| rasterize_line(v(0.0, 0.0), v(3.0, 3.0), p));
        assert_eq!(d.len(), 4);
        for k in 0..=3 {
            assert!(d.contains(&(k, k)), "missing ({k},{k})");
        }
        // Direction-independent: reversing the segment plots the same pixels.
        let rev = pixels(|p| rasterize_line(v(3.0, 3.0), v(0.0, 0.0), p));
        assert_eq!(d, rev);
    }

    #[test]
    fn rasterize_rounds_subpixel_endpoints() {
        // 2.4 → 2, 2.6 → 3 (round-to-nearest in fixed point).
        let s = pixels(|p| rasterize_line(v(2.4, 0.0), v(2.6, 0.0), p));
        assert_eq!(s, [(2, 0), (3, 0)].into_iter().collect());
    }

    #[test]
    fn rasterize_polyline_covers_every_segment() {
        // An "L": (0,0)->(4,0)->(4,3).
        let path = [v(0.0, 0.0), v(4.0, 0.0), v(4.0, 3.0)];
        let px = pixels(|p| rasterize_polyline(&path, p));
        assert!(px.contains(&(0, 0)) && px.contains(&(4, 0))); // first leg
        assert!(px.contains(&(4, 3))); // second leg
        // The shared corner is present exactly once (deduped set).
        assert!(px.contains(&(4, 0)));
        // Single-point and empty paths are safe.
        assert_eq!(
            pixels(|p| rasterize_polyline(&[v(7.0, 9.0)], p)),
            [(7, 9)].into_iter().collect()
        );
        assert!(pixels(|p| rasterize_polyline(&[], p)).is_empty());
    }

    #[test]
    fn stamp_disc_footprints() {
        assert_eq!(
            pixels(|p| stamp_disc(5, 5, 0, p)),
            [(5, 5)].into_iter().collect()
        );
        // radius 1 → a plus of 5 pixels.
        assert_eq!(pixels(|p| stamp_disc(0, 0, 1, p)).len(), 5);
        // radius 2 → 13 pixels (the discretized disc).
        assert_eq!(pixels(|p| stamp_disc(0, 0, 2, p)).len(), 13);
    }

    #[test]
    fn thick_polyline_widens_the_stroke() {
        let path = [v(0.0, 5.0), v(6.0, 5.0)];
        let thin = pixels(|p| rasterize_thick_polyline(&path, 0, p));
        let thick = pixels(|p| rasterize_thick_polyline(&path, 1, p));
        // radius 0 matches the 1-px polyline.
        assert_eq!(thin, pixels(|p| rasterize_polyline(&path, p)));
        // A radius-1 brush paints the rows above and below the centre line.
        assert!(thick.contains(&(3, 4)) && thick.contains(&(3, 5)) && thick.contains(&(3, 6)));
        assert!(thick.len() > thin.len());
    }

    /// Collect every (x, y, coverage) an AA rasterizer plots.
    fn aa_pixels(f: impl FnOnce(&mut dyn FnMut(i32, i32, u8))) -> alloc::vec::Vec<(i32, i32, u8)> {
        let mut out = alloc::vec::Vec::new();
        let mut plot = |x: i32, y: i32, c: u8| out.push((x, y, c));
        f(&mut plot);
        out
    }

    #[test]
    fn aa_axis_aligned_line_is_crisp() {
        // A horizontal line sits exactly on a row: full coverage, no bleed.
        let px = aa_pixels(|p| rasterize_line_aa(v(0.0, 3.0), v(5.0, 3.0), p));
        assert_eq!(px.len(), 6); // 0..=5, one pixel each
        assert!(px.iter().all(|&(_, y, c)| y == 3 && c == 255));
    }

    #[test]
    fn aa_exact_diagonal_is_crisp() {
        // A 45° line falls on integer pixels each step → single full-coverage px.
        let px = aa_pixels(|p| rasterize_line_aa(v(0.0, 0.0), v(4.0, 4.0), p));
        assert_eq!(px.len(), 5);
        for k in 0..=4 {
            assert!(px.contains(&(k, k, 255)), "missing crisp ({k},{k})");
        }
    }

    #[test]
    fn aa_shallow_line_ramps_coverage() {
        // Gradient 0.5: the true line runs between two rows, so mid-columns split
        // coverage across both. Per column the two pixels' coverage sums to 255.
        let px = aa_pixels(|p| rasterize_line_aa(v(0.0, 0.0), v(4.0, 2.0), p));
        let mut per_col: alloc::collections::BTreeMap<i32, u32> =
            alloc::collections::BTreeMap::new();
        for &(x, _, c) in &px {
            *per_col.entry(x).or_default() += c as u32;
        }
        for (&x, &sum) in &per_col {
            assert_eq!(sum, 255, "column {x} coverage should total 255, got {sum}");
        }
        // At least one column is genuinely split (a ~half-covered pixel exists).
        assert!(
            px.iter().any(|&(_, _, c)| (120..=135).contains(&c)),
            "no ~half-coverage pixel"
        );
    }

    #[test]
    fn aa_handles_steep_lines() {
        // dy > dx → the algorithm iterates over Y; endpoints must still land.
        let px = aa_pixels(|p| rasterize_line_aa(v(0.0, 0.0), v(2.0, 5.0), p));
        assert!(
            px.iter().any(|&(x, y, _)| x == 0 && y == 0),
            "missing start"
        );
        assert!(px.iter().any(|&(x, y, _)| x == 2 && y == 5), "missing end");
        // Spans the full height (one plot per Y row across the major axis).
        let ys: alloc::collections::BTreeSet<i32> = px.iter().map(|&(_, y, _)| y).collect();
        assert!((0..=5).all(|y| ys.contains(&y)));
    }

    #[test]
    fn aa_polyline_and_degenerate_cases() {
        let px = aa_pixels(|p| rasterize_polyline_aa(&[v(0.0, 0.0), v(4.0, 0.0), v(4.0, 4.0)], p));
        assert!(px.iter().any(|&(x, y, _)| x == 0 && y == 0));
        assert!(px.iter().any(|&(x, y, _)| x == 4 && y == 4));
        // Single point → one full-coverage pixel; empty → nothing.
        assert_eq!(
            aa_pixels(|p| rasterize_polyline_aa(&[v(2.0, 6.0)], p)),
            alloc::vec![(2, 6, 255)]
        );
        assert!(aa_pixels(|p| rasterize_polyline_aa(&[], p)).is_empty());
    }

    #[test]
    fn feathered_stamp_has_bright_core_and_falloff() {
        let px = aa_pixels(|p| stamp_feathered(10, 10, 1, 4, p));
        let cov_at = |x: i32, y: i32| {
            px.iter()
                .find(|&&(a, b, _)| a == x && b == y)
                .map(|&(_, _, c)| c)
        };
        // Centre + the core radius (dist ≤ 1) are full brightness.
        assert_eq!(cov_at(10, 10), Some(255));
        assert_eq!(cov_at(11, 10), Some(255));
        // Toward the edge it dims (dist 3, inside radius 4).
        let near_edge = cov_at(13, 10).expect("dist-3 pixel present");
        assert!(
            near_edge > 0 && near_edge < 255,
            "falloff pixel = {near_edge}"
        );
        // At/after the radius, nothing is plotted.
        assert_eq!(cov_at(14, 10), None); // dist 4 == radius → coverage 0
        assert_eq!(cov_at(15, 10), None);
        // radius 0 → just the centre.
        assert_eq!(
            aa_pixels(|p| stamp_feathered(5, 5, 2, 0, p)),
            alloc::vec![(5, 5, 255)]
        );
    }

    #[test]
    fn glow_polyline_is_a_bright_line_in_a_halo() {
        let px = aa_pixels(|p| rasterize_glow_polyline(&[v(0.0, 10.0), v(10.0, 10.0)], 1, 3, p));
        // The centreline is full brightness...
        assert!(px.iter().any(|&(x, y, c)| x == 5 && y == 10 && c == 255));
        // ...wrapped in a dimmer halo above and below.
        assert!(px.iter().any(|&(_, y, c)| y == 12 && c > 0 && c < 255));
        assert!(px.iter().any(|&(_, y, c)| y == 8 && c > 0 && c < 255));
        // No coverage ever exceeds full.
        assert!(px.iter().all(|&(_, _, c)| c <= 255));
    }

    #[test]
    fn paint_glow_writes_line_and_halo_into_buffer() {
        let (w, h) = (32usize, 16usize);
        let mut buf = alloc::vec![0u8; w * h];
        fn at(buf: &[u8], w: usize, x: usize, y: usize) -> u8 {
            buf[y * w + x]
        }
        paint_glow(&mut buf, w, h, &[v(2.0, 8.0), v(28.0, 8.0)], 1, 3);
        // Bright core on the line, dimmer halo above/below, background clear.
        assert_eq!(at(&buf, w, 15, 8), 255);
        assert!(at(&buf, w, 15, 10) > 0 && at(&buf, w, 15, 10) < 255);
        assert!(at(&buf, w, 15, 6) > 0 && at(&buf, w, 15, 6) < 255);
        assert_eq!(at(&buf, w, 15, 13), 0); // beyond the halo
        // Max-combine: a second overlapping pass never exceeds full / darkens.
        let before = at(&buf, w, 15, 8);
        paint_glow(&mut buf, w, h, &[v(2.0, 8.0), v(28.0, 8.0)], 1, 3);
        assert_eq!(at(&buf, w, 15, 8), before);
        // Out-of-bounds points don't panic (clipped).
        paint_glow(&mut buf, w, h, &[v(-50.0, -50.0), v(500.0, 500.0)], 1, 3);
    }
}
