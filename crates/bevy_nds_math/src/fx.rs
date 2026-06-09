//! `Fx32` — a 20.12 signed fixed-point scalar.
//!
//! The 20.12 format (12 fractional bits, range ~[-524288.0, 524287.999]) is the
//! one the DS 3D Geometry Engine consumes natively (vertex positions, matrix
//! entries, view ranges). Keeping CPU-side math in the same format avoids a
//! conversion every time we hand a number to the hardware and replaces every
//! `f32` op (software-emulated on the no-FPU ARM946E-S) with an `i32`/`i64`
//! op the CPU does in one cycle.
//!
//! Arithmetic semantics:
//! - Addition / subtraction are plain `i32` ops; they wrap on overflow (same
//!   as `i32::wrapping_add`). Stay inside the representable range.
//! - Multiplication promotes to `i64` to avoid losing the high bits of the
//!   product before the `>> 12` rescale, then truncates back to `i32`.
//! - Division and square root delegate to the hardware coprocessor on the DS
//!   (see [`crate::hw`]); the software fallback used by host tests gives
//!   bit-identical results on the same inputs.

use core::ops::{Add, AddAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use crate::hw;

/// Number of fractional bits in [`Fx32`] (12 = 20.12 format).
pub const FRAC_BITS: u32 = 12;

/// The raw `i32` value of `1.0` in 20.12 (`4096`).
pub const ONE_RAW: i32 = 1 << FRAC_BITS;

/// 20.12 signed fixed-point scalar.
///
/// One [`Fx32`] worth of integer-domain range is `1 << 12 = 4096` raw units.
/// Construct with [`Fx32::from_int`], [`Fx32::from_raw`], or [`Fx32::from_f32`].
#[derive(Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Fx32(pub i32);

impl Fx32 {
    /// `0.0`.
    pub const ZERO: Self = Self(0);
    /// `1.0`.
    pub const ONE: Self = Self(ONE_RAW);
    /// `-1.0`.
    pub const NEG_ONE: Self = Self(-ONE_RAW);
    /// The smallest positive value (`2^-12 ≈ 0.000244`).
    pub const EPSILON: Self = Self(1);

    /// Wrap a raw 20.12 value.
    #[inline]
    pub const fn from_raw(v: i32) -> Self {
        Self(v)
    }

    /// The underlying raw 20.12 value.
    #[inline]
    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Promote a whole integer to 20.12.
    #[inline]
    pub const fn from_int(v: i32) -> Self {
        Self(v << FRAC_BITS)
    }

    /// Build from an `f32`. Truncates toward zero (the rounding `as i32` does).
    #[inline]
    pub fn from_f32(v: f32) -> Self {
        Self((v * (ONE_RAW as f32)) as i32)
    }

    /// Convert to an `f32`. Lossless for values that fit in 24 mantissa bits;
    /// magnitudes beyond ~16 (raw value > 1 << 24) lose the bottom bits.
    #[inline]
    pub fn to_f32(self) -> f32 {
        self.0 as f32 / (ONE_RAW as f32)
    }

    /// Integer part (rounded toward negative infinity, matching `>>`).
    #[inline]
    pub const fn floor_i32(self) -> i32 {
        self.0 >> FRAC_BITS
    }

    /// Absolute value.
    #[inline]
    pub const fn abs(self) -> Self {
        Self(self.0.wrapping_abs())
    }

    /// Hardware-accelerated reciprocal `1 / self`. Returns `i32::MAX` for `0`
    /// (matches what the divide register produces on a `1/0`).
    #[inline]
    pub fn recip(self) -> Self {
        Self::ONE / self
    }

    /// Hardware-accelerated square root. **Panics in debug / returns 0 in
    /// release for negative inputs.** Defined for `self ≥ 0`.
    #[inline]
    pub fn sqrt(self) -> Self {
        debug_assert!(self.0 >= 0, "Fx32::sqrt on negative value");
        if self.0 <= 0 {
            return Self::ZERO;
        }
        // sqrt(A * 2^12) we want represented in 20.12 = R * 2^12 where R = sqrt(A).
        // Solve: R * 2^12 = sqrt(A) * 2^12 = sqrt(A * 2^24) = sqrt((self.0 as u64) << 12).
        Self(hw::sqrt_u64((self.0 as u64) << FRAC_BITS) as i32)
    }
}

impl Add for Fx32 {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self(self.0.wrapping_add(rhs.0))
    }
}

impl AddAssign for Fx32 {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0.wrapping_add(rhs.0);
    }
}

impl Sub for Fx32 {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self(self.0.wrapping_sub(rhs.0))
    }
}

impl SubAssign for Fx32 {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 = self.0.wrapping_sub(rhs.0);
    }
}

impl Neg for Fx32 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self(self.0.wrapping_neg())
    }
}

impl Mul for Fx32 {
    type Output = Self;
    /// 20.12 × 20.12 → 20.12. Uses an `i64` intermediate so the product doesn't
    /// overflow before the `>> 12` rescale.
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        let p = (self.0 as i64) * (rhs.0 as i64);
        Self((p >> FRAC_BITS) as i32)
    }
}

impl MulAssign for Fx32 {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl core::ops::Div for Fx32 {
    type Output = Self;
    /// 20.12 ÷ 20.12 → 20.12 via the hardware divider. The numerator is
    /// pre-shifted by 12 (into 64 bits) so the quotient lands back in 20.12.
    #[inline]
    fn div(self, rhs: Self) -> Self {
        Self(hw::div_64_32((self.0 as i64) << FRAC_BITS, rhs.0))
    }
}

impl core::fmt::Debug for Fx32 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Show both the raw 20.12 and a decimal approximation — useful in tests.
        write!(f, "Fx32(raw={}, ≈{})", self.0, self.to_f32())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_round_trips() {
        assert_eq!(Fx32::ONE.raw(), 4096);
        assert_eq!(Fx32::ONE.to_f32(), 1.0);
        assert_eq!(Fx32::from_int(1), Fx32::ONE);
        assert_eq!(Fx32::from_f32(1.0), Fx32::ONE);
    }

    #[test]
    fn add_sub() {
        let a = Fx32::from_f32(1.5);
        let b = Fx32::from_f32(0.25);
        assert_eq!((a + b).to_f32(), 1.75);
        assert_eq!((a - b).to_f32(), 1.25);
        assert_eq!((-a).to_f32(), -1.5);
    }

    #[test]
    fn multiply_uses_i64() {
        // 200 * 200 = 40_000. In 20.12 the raw product is 200<<12 * 200<<12 =
        // 819_200 * 819_200 = 6.7e11, far beyond i32 range; without the i64
        // intermediate this would alias to garbage.
        let v = Fx32::from_int(200);
        assert_eq!((v * v).to_f32(), 40_000.0);
    }

    #[test]
    fn divide_round_trips() {
        let n = Fx32::from_f32(7.0);
        let d = Fx32::from_f32(2.0);
        let q = n / d;
        assert_eq!(q.to_f32(), 3.5);
        // 1 / 0.25 = 4
        assert_eq!((Fx32::ONE / Fx32::from_f32(0.25)).to_f32(), 4.0);
    }

    #[test]
    fn sqrt_round_trips() {
        assert_eq!(Fx32::from_int(0).sqrt().raw(), 0);
        assert_eq!(Fx32::from_int(1).sqrt().raw(), Fx32::ONE.raw());
        assert_eq!(Fx32::from_int(4).sqrt().to_f32(), 2.0);
        assert_eq!(Fx32::from_int(100).sqrt().to_f32(), 10.0);
        // Imperfect square: |sqrt(2) - 1.41421356| should be tiny.
        let two = Fx32::from_int(2).sqrt().to_f32();
        assert!((two - 1.4142135).abs() < 1e-3, "sqrt(2) = {two}");
    }

    #[test]
    fn recip_round_trips() {
        let v = Fx32::from_f32(8.0);
        assert_eq!(v.recip().to_f32(), 0.125);
    }

    #[test]
    fn floor_signs() {
        assert_eq!(Fx32::from_f32(2.75).floor_i32(), 2);
        // floor_i32 is `>>`, which rounds toward -infinity (i.e. floor).
        assert_eq!(Fx32::from_f32(-0.5).floor_i32(), -1);
    }
}
