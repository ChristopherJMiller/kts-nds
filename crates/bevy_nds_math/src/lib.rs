//! Fixed-point math + hardware divide/sqrt for the Nintendo DS.
//!
//! The ARM946E-S has no FPU, so every `f32` op the engine touches is
//! software-emulated (hundreds of cycles each on the 33 MHz core). This crate
//! supplies the two things that close that gap on the hot paths:
//!
//! - [`Fx32`] — 20.12 signed fixed-point, the same format the DS 3D Geometry
//!   Engine consumes natively. Add/subtract are single-cycle `i32` ops, multiply
//!   uses an `i64` intermediate, divide / square root go through the hardware
//!   coprocessor (a `1/x` is one MMIO write + a < 40-cycle spinwait + one read).
//! - [`FxVec2`] / [`FxVec3`] — the fixed-point analogue of `glam::Vec2`/`Vec3`.
//!   `length` / `normalize` go through the same hardware path, so the
//!   per-frame normalizes the renderer does on lighting direction etc. no longer
//!   pay a softfloat sqrt.
//! - [`hw`] — the bare divide/sqrt register wrappers (`<nds/arm9/math.h>`),
//!   shaded by `target_vendor = "nintendo"` with software fallbacks so the same
//!   code runs under the host test harness.
//!
//! ## When to reach for fixed-point
//!
//! - **Yes:** anything inside a per-frame, per-entity loop (vector math in
//!   physics/AI, distance checks, vertex transforms, lighting normalize) — the
//!   savings compound. Anything you'd hand to the Geometry Engine anyway.
//! - **Probably yes:** matrix composition, frustum culling math, trig (consider
//!   the DS hardware trig LUT instead of `libm::sinf` / `cosf`).
//! - **No:** one-shot setup code, asset baking, anywhere `glam` is more
//!   readable and the cost is dwarfed by the I/O around it.
//!
//! ## Range and overflow
//!
//! 20.12 represents values in `[-524288.0, 524287.999...]` with a resolution
//! of `2^-12 ≈ 2.4e-4`. Multiplication uses an `i64` intermediate (so a
//! 200 × 200 = 40 000 multiply doesn't overflow before the `>> 12` rescale),
//! but addition/subtraction wrap silently — keep accumulators inside the range.
//! For lengths above ~700, `length_sq` (`x² + y² + z²`) starts approaching the
//! top of `i64`; the renderer's world units stay well below that.
//!
//! ## Notes: f32 vs fixed-point on the DS
//!
//! The numbers below are order-of-magnitude estimates from the published
//! ARM946E-S and DS coprocessor latencies, not a microbenchmark — accurate to
//! within roughly 2× and good enough to drive the design.
//!
//! | Operation               | softfloat `f32` (no FPU) | this crate                |
//! | ----------------------- | -------------------------- | ------------------------- |
//! | add / sub               | ~30-60 cycles              | 1 cycle (`i32` add)       |
//! | multiply                | ~50-100 cycles             | ~4 cycles (`i64` mul + shift) |
//! | divide                  | 200+ cycles                | ~30 cycles ([`hw::div_32`]) |
//! | square root             | 300+ cycles                | ~35 cycles ([`hw::sqrt_u32`]) |
//! | normalize a `Vec3`      | sqrt + 3 div (~900 cycles) | sqrt + 3 hw-div (~150 cycles) |
//!
//! At 33 MHz, a single softfloat sqrt is ~9 µs — and a frame budget is 16.6
//! ms. Each individual op is therefore "fine" on its own; the problem is that
//! per-frame, per-entity loops multiply this cost by N. The light-direction
//! normalize the renderer does on every enabled directional light each frame is
//! the canonical example, and the one this crate already replaces.
//!
//! What this means in practice:
//!
//! - **Glue code stays in `f32` / glam.** Asset baking, setup, anything outside
//!   the per-frame loop — leave it readable.
//! - **Per-entity per-frame math wants fixed-point.** Distance checks, AABB vs
//!   point, AI/physics integration, projectile updates. Three components mul
//!   + add + a hardware sqrt is faster than a single softfloat add.
//! - **Coordinates that are about to hit the Geometry Engine were already in
//!   20.12.** Staying in `Fx32` means the `to_v16` / `to_fix` conversions in
//!   `bevy_nds_3d` become a single shift instead of a float multiply.
//!
//! Trig (`sinf` / `cosf` in `bevy_nds_3d_cull`) is the next big lever; the DS
//! has a hardware trig LUT (`<nds/arm9/trig_lut.h>`) but it's a separate
//! exercise from divide/sqrt and lives outside this crate.

#![no_std]

mod fx;
mod fx_vec;
pub mod hw;

pub use fx::{FRAC_BITS, Fx32, ONE_RAW};
pub use fx_vec::{FxVec2, FxVec3};
