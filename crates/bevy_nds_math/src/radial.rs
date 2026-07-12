//! Pure radial-wheel spoke selection, in fixed-point.
//!
//! Turns a stylus drag `offset` (`current - origin`, in touch pixels) into the
//! index of the nearest of **five spokes** arranged as a **point-up regular
//! pentagon** — the geometry behind the device + item radial wheel (issue #25).
//! Returns `None` inside a deadzone (a release there cancels the wheel).
//!
//! Sibling to [`crate::stick`]: the stick conditions a drag offset into a
//! movement *vector*; this conditions the same kind of offset into a discrete
//! *spoke*. Both are feel-critical, FFI-free, and host-tested.
//!
//! **Convention:** the offset is in **screen space, y-down** (raw touch pixels),
//! so "up" is `-y`. Spoke `0` is the top vertex (straight up) — the one the game
//! pins the capture device to, so "device = up" holds by construction; spokes
//! `1..=4` follow clockwise. A drag exactly between two spokes (e.g. straight
//! down, which sits between the two lower vertices — the pentagon has no bottom
//! vertex) resolves deterministically to the **lower index**.

use crate::fx::Fx32;
use crate::fx_vec::FxVec2;

/// Number of spokes — a point-up pentagon.
pub const SPOKES: u8 = 5;

/// The five spoke directions in screen space (y-down, "up" is `-y`), unit
/// vectors, clockwise from the top. Selection ranks a drag by dot product
/// against these, so there is no runtime trig. Index 0 (straight up) is the
/// device spoke.
const SPOKE_DIRS: [(f32, f32); 5] = [
    (0.0, -1.0),                // 0  up         (device)
    (0.951_056_5, -0.309_017),  // 1  up-right
    (0.587_785_25, 0.809_017),  // 2  down-right
    (-0.587_785_25, 0.809_017), // 3  down-left
    (-0.951_056_5, -0.309_017), // 4  up-left
];

/// The unit direction of spoke `i` (0 = up, screen y-down), for laying the wheel
/// out on screen. These are the same vectors [`nearest_spoke`] ranks against, so
/// the drawn wheel and the picked spoke never disagree. An out-of-range index
/// clamps to spoke 0.
pub fn spoke_dir(i: u8) -> FxVec2 {
    let (dx, dy) = SPOKE_DIRS.get(i as usize).copied().unwrap_or(SPOKE_DIRS[0]);
    FxVec2::from_f32(dx, dy)
}

/// Map a drag `offset` to the nearest spoke index `0..SPOKES`, or `None` if the
/// drag is still within `deadzone` of the origin (releasing there cancels).
///
/// Picks the spoke whose direction best aligns with the drag — a dot-product
/// ranking against the pentagon's unit vectors (no trig). Ties resolve to the
/// lower index (the seed is spoke 0), so straight-down is deterministic.
pub fn nearest_spoke(offset: FxVec2, deadzone: Fx32) -> Option<u8> {
    if offset.length() < deadzone {
        return None;
    }
    let dot_at = |i: usize| {
        let (dx, dy) = SPOKE_DIRS[i];
        offset.dot(FxVec2::from_f32(dx, dy))
    };
    let mut best = 0usize;
    let mut best_dot = dot_at(0);
    for i in 1..SPOKE_DIRS.len() {
        let d = dot_at(i);
        if d > best_dot {
            best_dot = d;
            best = i;
        }
    }
    Some(best as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dz() -> Fx32 {
        Fx32::from_f32(12.0)
    }

    #[test]
    fn inside_deadzone_is_none() {
        assert_eq!(nearest_spoke(FxVec2::from_f32(0.0, -5.0), dz()), None);
        assert_eq!(nearest_spoke(FxVec2::ZERO, dz()), None);
    }

    #[test]
    fn straight_up_is_spoke_zero() {
        assert_eq!(nearest_spoke(FxVec2::from_f32(0.0, -30.0), dz()), Some(0));
        // A little skew still snaps to the top spoke.
        assert_eq!(nearest_spoke(FxVec2::from_f32(4.0, -30.0), dz()), Some(0));
        assert_eq!(nearest_spoke(FxVec2::from_f32(-4.0, -30.0), dz()), Some(0));
    }

    #[test]
    fn upper_right_is_spoke_one() {
        assert_eq!(nearest_spoke(FxVec2::from_f32(28.0, -12.0), dz()), Some(1));
    }

    #[test]
    fn lower_sectors_split_left_right() {
        assert_eq!(nearest_spoke(FxVec2::from_f32(20.0, 20.0), dz()), Some(2));
        assert_eq!(nearest_spoke(FxVec2::from_f32(-20.0, 20.0), dz()), Some(3));
    }

    #[test]
    fn left_is_spoke_four() {
        assert_eq!(nearest_spoke(FxVec2::from_f32(-30.0, -2.0), dz()), Some(4));
    }

    #[test]
    fn straight_down_ties_to_lower_index() {
        // No bottom vertex; straight down sits exactly between spokes 2 and 3 →
        // the lower index (2) wins deterministically.
        assert_eq!(nearest_spoke(FxVec2::from_f32(0.0, 30.0), dz()), Some(2));
    }

    #[test]
    fn every_spoke_direction_selects_itself() {
        for i in 0..SPOKES {
            let (dx, dy) = SPOKE_DIRS[i as usize];
            let off = FxVec2::from_f32(dx * 40.0, dy * 40.0);
            assert_eq!(nearest_spoke(off, dz()), Some(i));
        }
    }
}
