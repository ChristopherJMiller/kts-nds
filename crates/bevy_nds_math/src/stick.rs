//! Pure analog-stick conditioning, in fixed-point.
//!
//! Turns a raw stylus offset (`current - origin`, in touch pixels) into an
//! analog movement vector: a unit *direction* times a *magnitude* in `[0, 1]`,
//! with a **radial deadzone** near the origin and a saturation radius beyond
//! which magnitude pins to full. A separate [`smooth`] applies an exponential
//! low-pass so the heading doesn't twitch on the ~60 Hz, jitter-prone touch
//! stream.
//!
//! This is the feel-critical core of Spike A (issue #18, the relative
//! stylus virtual-stick). It is deliberately FFI-free pure math so it can be
//! host-tested — the spike's "split the stick-vector math out and unit-test it"
//! task. The caller (the game) owns axis mapping (screen-y points *down*, world
//! +y points *up*) and the speed scale; this module is axis-agnostic.
//!
//! "Continuous heading" (issue #18) is expressed as the normalized direction
//! vector rather than an `atan2` angle: nothing downstream consumes an angle —
//! movement is a vector — and a direction avoids pulling in software trig the
//! `bevy_nds_math` coprocessor wrappers don't cover.

use crate::fx::Fx32;
use crate::fx_vec::FxVec2;

/// Tunables for [`stick_vector`]. All in touch-pixel units except `smoothing`.
///
/// The two radii define a radial deadzone: input within `deadzone` of the
/// origin reads as no movement; input at or beyond `max_radius` reads as full
/// magnitude; in between, magnitude ramps linearly from 0 to 1.
#[derive(Clone, Copy, Debug)]
pub struct StickConfig {
    /// Radius (px) below which input is ignored — kills origin jitter.
    pub deadzone: Fx32,
    /// Radius (px) at which magnitude saturates to 1.0 (full speed).
    pub max_radius: Fx32,
    /// Exponential low-pass factor in `[0, 1)` used by [`smooth`]: `0.0` snaps
    /// instantly, values toward `1.0` add lag. (Stored here so a single
    /// resource carries every feel knob.)
    pub smoothing: Fx32,
}

/// Condition a raw stylus `offset` (`current - origin`) into a movement vector:
/// the unit direction of `offset` scaled by a magnitude in `[0, 1]`.
///
/// Returns [`FxVec2::ZERO`] inside the deadzone. The direction is preserved
/// exactly (it's `offset` normalized); only the magnitude is reshaped by the
/// radial deadzone, so heading stays fully analog — not snapped to 8/16-way.
pub fn stick_vector(offset: FxVec2, cfg: &StickConfig) -> FxVec2 {
    let len = offset.length();
    if len <= cfg.deadzone {
        return FxVec2::ZERO;
    }
    let dir = offset.normalize_or_zero();
    let span = cfg.max_radius - cfg.deadzone;
    // Degenerate config (deadzone >= max_radius): anything past the deadzone is
    // simply full magnitude rather than dividing by zero/negative.
    let mag = if span <= Fx32::ZERO {
        Fx32::ONE
    } else {
        ((len - cfg.deadzone) / span).min(Fx32::ONE)
    };
    dir * mag
}

/// Exponential low-pass of `prev` toward `target`. `smoothing` in `[0, 1)`:
/// `0.0` returns `target` (no smoothing), values toward `1.0` lag more. Out of
/// range values are clamped so a bad tunable can't blow up the velocity.
#[inline]
pub fn smooth(prev: FxVec2, target: FxVec2, smoothing: Fx32) -> FxVec2 {
    let s = smoothing.clamp(Fx32::ZERO, Fx32::ONE);
    prev * s + target * (Fx32::ONE - s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StickConfig {
        StickConfig {
            deadzone: Fx32::from_int(8),
            max_radius: Fx32::from_int(40),
            smoothing: Fx32::ZERO,
        }
    }

    fn approx(a: Fx32, b: f32) -> bool {
        (a.to_f32() - b).abs() < 0.05
    }

    #[test]
    fn inside_deadzone_is_zero() {
        // 5px offset, deadzone 8px -> no movement.
        let v = stick_vector(FxVec2::from_f32(5.0, 0.0), &cfg());
        assert_eq!(v, FxVec2::ZERO);
    }

    #[test]
    fn saturates_to_unit_magnitude_at_max_radius() {
        // Exactly at max_radius along +x -> full magnitude, direction +x.
        let v = stick_vector(FxVec2::from_f32(40.0, 0.0), &cfg());
        assert!(approx(v.x, 1.0), "x = {}", v.x.to_f32());
        assert!(approx(v.y, 0.0));
        // Beyond max_radius stays clamped at 1.0.
        let far = stick_vector(FxVec2::from_f32(200.0, 0.0), &cfg());
        assert!(approx(far.length(), 1.0));
    }

    #[test]
    fn magnitude_ramps_linearly_between_radii() {
        // Halfway through the [deadzone, max_radius] span (len = 24, span = 32,
        // (24-8)/32 = 0.5) -> magnitude 0.5.
        let v = stick_vector(FxVec2::from_f32(24.0, 0.0), &cfg());
        assert!(approx(v.length(), 0.5), "len = {}", v.length().to_f32());
    }

    #[test]
    fn direction_is_preserved_not_snapped() {
        // A 3-4-5 offset (len 50, past max_radius) -> unit-magnitude vector
        // still pointing along (0.6, 0.8), i.e. analog heading, not 8-way.
        let v = stick_vector(FxVec2::from_f32(30.0, 40.0), &cfg());
        assert!(approx(v.x, 0.6), "x = {}", v.x.to_f32());
        assert!(approx(v.y, 0.8), "y = {}", v.y.to_f32());
    }

    #[test]
    fn smooth_endpoints() {
        let prev = FxVec2::from_f32(1.0, 0.0);
        let target = FxVec2::from_f32(0.0, 1.0);
        // 0.0 -> snap to target; 1.0 -> hold prev.
        assert_eq!(smooth(prev, target, Fx32::ZERO), target);
        assert_eq!(smooth(prev, target, Fx32::ONE), prev);
    }

    #[test]
    fn smooth_converges_toward_target() {
        let target = FxVec2::from_f32(1.0, 0.0);
        let half = Fx32::from_f32(0.5);
        let mut v = FxVec2::ZERO;
        for _ in 0..16 {
            v = smooth(v, target, half);
        }
        // After many half-steps the low-pass has essentially reached target.
        assert!(approx(v.x, 1.0), "x = {}", v.x.to_f32());
        assert!(approx(v.y, 0.0));
    }
}
