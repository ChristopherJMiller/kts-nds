//! Hardware divide and square-root coprocessor on the ARM946E-S.
//!
//! The DS exposes a small math coprocessor at IO registers `0x0400_0280..0x0400_02C0`
//! (see `<nds/arm9/math.h>` and the GBATEK "DS Maths" section). It runs in
//! parallel with the CPU and finishes a 64-bit division or 32-bit square root
//! in a fixed number of cycles regardless of operands — handy on a chip with no
//! FPU and a software-emulated `f32`. This module is the thin "is the chip
//! ready yet?" wrapper.
//!
//! The same API exists on the host: every function has a `target_vendor =
//! "nintendo"` arm that pokes the MMIO and a `not(target_vendor = "nintendo")`
//! arm that does the equivalent in software, so the same code paths run under
//! `just test` and on the device.

#[cfg(target_vendor = "nintendo")]
use core::ptr::{read_volatile, write_volatile};

// --- DS math coprocessor registers (see <nds/arm9/math.h>) -------------------

/// Divider mode control + status. Low two bits select mode; bit 15 is "busy",
/// bit 14 latches division-by-zero. See GBATEK "4000280h - DIVCNT".
#[cfg(target_vendor = "nintendo")]
const REG_DIVCNT: *mut u16 = 0x0400_0280 as *mut u16;
/// 64-bit dividend (numerator). The hardware latches it on write.
#[cfg(target_vendor = "nintendo")]
const REG_DIV_NUMER: *mut i64 = 0x0400_0290 as *mut i64;
/// Divisor (32 or 64 bits depending on `DIVCNT` mode).
#[cfg(target_vendor = "nintendo")]
const REG_DIV_DENOM: *mut i64 = 0x0400_0298 as *mut i64;
/// 64-bit quotient.
#[cfg(target_vendor = "nintendo")]
const REG_DIV_RESULT: *const i64 = 0x0400_02A0 as *const i64;

/// Square root mode + status. Bit 0 selects 32 vs 64 bit input; bit 15 is busy.
#[cfg(target_vendor = "nintendo")]
const REG_SQRTCNT: *mut u16 = 0x0400_02B0 as *mut u16;
/// 32-bit square-root result.
#[cfg(target_vendor = "nintendo")]
const REG_SQRT_RESULT: *const u32 = 0x0400_02B4 as *const u32;
/// 64-bit input to the square root unit.
#[cfg(target_vendor = "nintendo")]
const REG_SQRT_PARAM: *mut u64 = 0x0400_02B8 as *mut u64;

/// DIVCNT mode `0`: signed `i32 / i32 → i32` quotient + `i32` remainder.
#[cfg(target_vendor = "nintendo")]
const DIV_32_32: u16 = 0;
/// DIVCNT mode `1`: signed `i64 / i32 → i64` quotient + `i32` remainder. The
/// 64-bit numerator lets a 20.12 division pre-shift `num << 12` without
/// losing precision.
#[cfg(target_vendor = "nintendo")]
const DIV_64_32: u16 = 1;
/// DIVCNT bit 15: divider currently computing.
#[cfg(target_vendor = "nintendo")]
const DIV_BUSY: u16 = 1 << 15;

/// SQRTCNT mode `0`: 32-bit input.
#[cfg(target_vendor = "nintendo")]
const SQRT_32: u16 = 0;
/// SQRTCNT mode `1`: 64-bit input.
#[cfg(target_vendor = "nintendo")]
const SQRT_64: u16 = 1;
/// SQRTCNT bit 15: sqrt unit currently computing.
#[cfg(target_vendor = "nintendo")]
const SQRT_BUSY: u16 = 1 << 15;

// --- Public API ---------------------------------------------------------------

/// 32-bit signed integer division: `num / den`.
///
/// Matches libnds `div32()`. Division by zero produces the same sentinel the
/// hardware does (`±i32::MAX` depending on sign), rather than panicking — the
/// caller should already have checked when correctness matters.
#[inline]
pub fn div_32(num: i32, den: i32) -> i32 {
    #[cfg(target_vendor = "nintendo")]
    unsafe {
        write_volatile(REG_DIVCNT, DIV_32_32);
        wait_div();
        write_volatile(REG_DIV_NUMER, num as i64);
        write_volatile(REG_DIV_DENOM, den as i64);
        wait_div();
        read_volatile(REG_DIV_RESULT) as i32
    }
    #[cfg(not(target_vendor = "nintendo"))]
    {
        soft_div_32(num, den)
    }
}

/// 64/32-bit signed integer division: `num / den`, returning the 64-bit quotient.
///
/// This is the mode 20.12 fixed-point division wants (numerator pre-shifted by
/// 12 bits into a 64-bit value, divisor in 32). Matches libnds `div32` with a
/// pre-widened numerator.
#[inline]
pub fn div_64_32(num: i64, den: i32) -> i32 {
    #[cfg(target_vendor = "nintendo")]
    unsafe {
        write_volatile(REG_DIVCNT, DIV_64_32);
        wait_div();
        write_volatile(REG_DIV_NUMER, num);
        write_volatile(REG_DIV_DENOM, den as i64);
        wait_div();
        read_volatile(REG_DIV_RESULT) as i32
    }
    #[cfg(not(target_vendor = "nintendo"))]
    {
        soft_div_64_32(num, den)
    }
}

/// 32-bit unsigned integer square root: `floor(sqrt(x))`.
///
/// Matches libnds `sqrt32()`. The 16 fractional bits the hardware can produce
/// are discarded — callers wanting fixed-point precision should pre-shift
/// their input and use [`sqrt_u64`].
#[inline]
pub fn sqrt_u32(x: u32) -> u32 {
    #[cfg(target_vendor = "nintendo")]
    unsafe {
        write_volatile(REG_SQRTCNT, SQRT_32);
        wait_sqrt();
        write_volatile(REG_SQRT_PARAM, x as u64);
        wait_sqrt();
        read_volatile(REG_SQRT_RESULT)
    }
    #[cfg(not(target_vendor = "nintendo"))]
    {
        soft_sqrt_u32(x)
    }
}

/// 64-bit unsigned integer square root: `floor(sqrt(x))` clipped to 32 bits.
///
/// The hardware result register is 32 bits, which is enough for any input up
/// to `2^64 - 1` (since `sqrt(2^64 - 1) < 2^32`).
#[inline]
pub fn sqrt_u64(x: u64) -> u32 {
    #[cfg(target_vendor = "nintendo")]
    unsafe {
        write_volatile(REG_SQRTCNT, SQRT_64);
        wait_sqrt();
        write_volatile(REG_SQRT_PARAM, x);
        wait_sqrt();
        read_volatile(REG_SQRT_RESULT)
    }
    #[cfg(not(target_vendor = "nintendo"))]
    {
        soft_sqrt_u64(x)
    }
}

// --- DS spinwaits -------------------------------------------------------------

#[cfg(target_vendor = "nintendo")]
#[inline]
unsafe fn wait_div() {
    // A 64-bit division latency on the DS is < 40 cycles; this loop is the same
    // shape libnds uses (`while (DIVCNT & DIV_BUSY) ;`).
    unsafe { while read_volatile(REG_DIVCNT) & DIV_BUSY != 0 {} }
}

#[cfg(target_vendor = "nintendo")]
#[inline]
unsafe fn wait_sqrt() {
    unsafe { while read_volatile(REG_SQRTCNT) & SQRT_BUSY != 0 {} }
}

// --- Host-side software equivalents ------------------------------------------

#[cfg(any(not(target_vendor = "nintendo"), test))]
fn soft_div_32(num: i32, den: i32) -> i32 {
    if den == 0 {
        if num >= 0 { i32::MAX } else { i32::MIN }
    } else {
        // Match the hardware: i32::MIN / -1 wraps on the DS.
        num.wrapping_div(den)
    }
}

#[cfg(any(not(target_vendor = "nintendo"), test))]
fn soft_div_64_32(num: i64, den: i32) -> i32 {
    if den == 0 {
        return if num >= 0 { i32::MAX } else { i32::MIN };
    }
    let q = num.wrapping_div(den as i64);
    // Clamp to i32 the way the hardware result register does (it's 32 bits).
    q as i32
}

#[cfg(any(not(target_vendor = "nintendo"), test))]
fn soft_sqrt_u32(x: u32) -> u32 {
    soft_sqrt_u64(x as u64)
}

#[cfg(any(not(target_vendor = "nintendo"), test))]
fn soft_sqrt_u64(x: u64) -> u32 {
    // Newton-Raphson on integers; produces `floor(sqrt(x))` bit-identical to
    // what the DS sqrt coprocessor returns. The standard "halve the result
    // until it stops moving" loop converges in <= 32 iterations for u64.
    if x == 0 {
        return 0;
    }
    let mut r: u64 = 1u64 << ((64 - x.leading_zeros()).div_ceil(2));
    loop {
        let next = (r + x / r) >> 1;
        if next >= r {
            return r as u32;
        }
        r = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div32_basics() {
        assert_eq!(div_32(20, 4), 5);
        assert_eq!(div_32(-20, 4), -5);
        assert_eq!(div_32(20, -4), -5);
        assert_eq!(div_32(7, 2), 3);
    }

    #[test]
    fn div32_by_zero_does_not_panic() {
        // Hardware returns ±MAX rather than trapping; mimic that.
        assert_eq!(div_32(1, 0), i32::MAX);
        assert_eq!(div_32(-1, 0), i32::MIN);
    }

    #[test]
    fn div64_32_basics() {
        // Integer divide with a 64-bit numerator (what 20.12 division uses
        // after pre-shifting the numerator left by 12).
        assert_eq!(div_64_32(1_000_000_000_000_i64, 1_000_000), 1_000_000);
        // 7.0 / 2.0 in 20.12: num pre-shifted by 12, denom is the raw 2.0<<12.
        let num = (7_i64 << 12) << 12;
        let den = 2 << 12;
        assert_eq!(div_64_32(num, den), (35 << 12) / 10); // = 3.5 in 20.12
    }

    #[test]
    fn sqrt_u32_basics() {
        assert_eq!(sqrt_u32(0), 0);
        assert_eq!(sqrt_u32(1), 1);
        assert_eq!(sqrt_u32(4), 2);
        assert_eq!(sqrt_u32(99), 9); // floor
        assert_eq!(sqrt_u32(100), 10);
        assert_eq!(sqrt_u32(u32::MAX), 65535); // floor(sqrt(2^32-1))
    }

    #[test]
    fn sqrt_u64_floors() {
        assert_eq!(sqrt_u64(0), 0);
        assert_eq!(sqrt_u64(1 << 40), 1 << 20);
        // floor(sqrt((1<<40) - 1)) = (1<<20) - 1
        assert_eq!(sqrt_u64((1u64 << 40) - 1), (1u32 << 20) - 1);
    }
}
