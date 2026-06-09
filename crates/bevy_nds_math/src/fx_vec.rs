//! 20.12 fixed-point vectors: [`FxVec2`] and [`FxVec3`].
//!
//! These are the fixed-point analogue of `glam::Vec2`/`Vec3`. They're sized to
//! the 20.12 format the DS Geometry Engine uses for vertex positions, so a
//! `FxVec3` can be fed straight to `MTX_TRANSLATE` / `VTX_16` without a per-call
//! float conversion. Length / normalize go through the hardware sqrt+divide
//! coprocessor (see [`crate::hw`]).

use core::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

use crate::fx::{FRAC_BITS, Fx32};
use crate::hw;

/// 2D fixed-point vector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FxVec2 {
    pub x: Fx32,
    pub y: Fx32,
}

/// 3D fixed-point vector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FxVec3 {
    pub x: Fx32,
    pub y: Fx32,
    pub z: Fx32,
}

impl FxVec2 {
    pub const ZERO: Self = Self {
        x: Fx32::ZERO,
        y: Fx32::ZERO,
    };

    #[inline]
    pub const fn new(x: Fx32, y: Fx32) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn dot(self, rhs: Self) -> Fx32 {
        self.x * rhs.x + self.y * rhs.y
    }

    /// `|self|^2` in raw `u64` units (i.e. `(x*ONE_RAW)^2 + (y*ONE_RAW)^2`).
    /// Stays in `u64` so it can be fed straight to [`hw::sqrt_u64`] without
    /// losing precision the way a `Fx32::sqrt(length_sq)` would.
    #[inline]
    fn length_sq_raw_u64(self) -> u64 {
        let x = self.x.raw() as i64;
        let y = self.y.raw() as i64;
        ((x * x) + (y * y)) as u64
    }

    /// `|self|` as 20.12 fixed-point, hardware-accelerated.
    #[inline]
    pub fn length(self) -> Fx32 {
        // sqrt((x*2^12)^2 + (y*2^12)^2) = sqrt(x^2 + y^2) * 2^12, exactly the
        // 20.12 representation of the Euclidean length. No extra shifting.
        Fx32::from_raw(hw::sqrt_u64(self.length_sq_raw_u64()) as i32)
    }
}

impl FxVec3 {
    pub const ZERO: Self = Self {
        x: Fx32::ZERO,
        y: Fx32::ZERO,
        z: Fx32::ZERO,
    };

    #[inline]
    pub const fn new(x: Fx32, y: Fx32, z: Fx32) -> Self {
        Self { x, y, z }
    }

    /// Convenience constructor from f32 components (compile-/startup-time only;
    /// per-frame code should already be in `Fx32`).
    #[inline]
    pub fn from_f32(x: f32, y: f32, z: f32) -> Self {
        Self::new(Fx32::from_f32(x), Fx32::from_f32(y), Fx32::from_f32(z))
    }

    #[inline]
    pub fn dot(self, rhs: Self) -> Fx32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    /// Cross product (right-handed).
    #[inline]
    pub fn cross(self, rhs: Self) -> Self {
        Self {
            x: self.y * rhs.z - self.z * rhs.y,
            y: self.z * rhs.x - self.x * rhs.z,
            z: self.x * rhs.y - self.y * rhs.x,
        }
    }

    #[inline]
    fn length_sq_raw_u64(self) -> u64 {
        let x = self.x.raw() as i64;
        let y = self.y.raw() as i64;
        let z = self.z.raw() as i64;
        ((x * x) + (y * y) + (z * z)) as u64
    }

    /// `|self|` as 20.12 fixed-point, using the hardware sqrt unit.
    #[inline]
    pub fn length(self) -> Fx32 {
        Fx32::from_raw(hw::sqrt_u64(self.length_sq_raw_u64()) as i32)
    }

    /// Unit vector. Hardware-accelerated: one sqrt + three divides on the math
    /// coprocessor (each ~30 ARM9 cycles), versus a software `f32::sqrt` +
    /// three `f32` divides (hundreds of cycles each). Returns [`Self::ZERO`]
    /// when the input length is zero.
    #[inline]
    pub fn normalize_or_zero(self) -> Self {
        let len_raw = hw::sqrt_u64(self.length_sq_raw_u64()) as i32;
        if len_raw == 0 {
            return Self::ZERO;
        }
        // For each component c (20.12), the normalized component is
        // c / length = (c << 12) / length_raw in 20.12.
        let n = |c: Fx32| -> Fx32 {
            Fx32::from_raw(hw::div_64_32((c.raw() as i64) << FRAC_BITS, len_raw))
        };
        Self {
            x: n(self.x),
            y: n(self.y),
            z: n(self.z),
        }
    }
}

// --- Arithmetic ---------------------------------------------------------------

impl Add for FxVec2 {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}
impl AddAssign for FxVec2 {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
    }
}
impl Sub for FxVec2 {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}
impl SubAssign for FxVec2 {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
    }
}
impl Neg for FxVec2 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}
impl Mul<Fx32> for FxVec2 {
    type Output = Self;
    #[inline]
    fn mul(self, s: Fx32) -> Self {
        Self::new(self.x * s, self.y * s)
    }
}

impl Add for FxVec3 {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}
impl AddAssign for FxVec3 {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.x += rhs.x;
        self.y += rhs.y;
        self.z += rhs.z;
    }
}
impl Sub for FxVec3 {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}
impl SubAssign for FxVec3 {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.x -= rhs.x;
        self.y -= rhs.y;
        self.z -= rhs.z;
    }
}
impl Neg for FxVec3 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}
impl Mul<Fx32> for FxVec3 {
    type Output = Self;
    #[inline]
    fn mul(self, s: Fx32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(v: f32) -> Fx32 {
        Fx32::from_f32(v)
    }

    #[test]
    fn vec3_dot_and_cross() {
        let a = FxVec3::from_f32(1.0, 2.0, 3.0);
        let b = FxVec3::from_f32(4.0, 5.0, 6.0);
        assert_eq!(a.dot(b).to_f32(), 32.0);
        let c = a.cross(b);
        assert_eq!(c.x.to_f32(), -3.0);
        assert_eq!(c.y.to_f32(), 6.0);
        assert_eq!(c.z.to_f32(), -3.0);
    }

    #[test]
    fn vec3_length_3_4_0_is_5() {
        let v = FxVec3::from_f32(3.0, 4.0, 0.0);
        assert_eq!(v.length().to_f32(), 5.0);
    }

    #[test]
    fn vec3_normalize_unit_length() {
        let v = FxVec3::from_f32(0.0, 3.0, 4.0);
        let n = v.normalize_or_zero();
        let len = n.length();
        // Hardware result is exact-integer floor; allow one LSB drift from the
        // discretised divide.
        assert!((len.to_f32() - 1.0).abs() < 1e-2, "len = {}", len.to_f32());
        // Direction is preserved (y:z = 3:4 → 0.6:0.8).
        assert!((n.y.to_f32() - 0.6).abs() < 1e-2);
        assert!((n.z.to_f32() - 0.8).abs() < 1e-2);
    }

    #[test]
    fn vec3_normalize_zero_is_zero() {
        assert_eq!(FxVec3::ZERO.normalize_or_zero(), FxVec3::ZERO);
    }

    #[test]
    fn vec2_arithmetic() {
        let a = FxVec2::new(fx(1.5), fx(2.5));
        let b = FxVec2::new(fx(0.5), fx(1.0));
        assert_eq!((a + b).x.to_f32(), 2.0);
        assert_eq!((a - b).y.to_f32(), 1.5);
        assert_eq!((a * fx(2.0)).x.to_f32(), 3.0);
    }
}
