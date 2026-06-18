//! Fixed-point model-matrix composition for the DS Geometry Engine.
//!
//! Composing an object's transform into a single 4x4 matrix and sending it with
//! one `MTX_MULT_4x4` replaces five separate Geometry Engine matrix commands
//! per object. The catch is the *compose* itself: doing it in `glam` `f32`
//! costs six software `sin`/`cos` (the Euler→matrix step) on the FPU-less
//! ARM946E-S — the per-object cost that caps scene density (see issue #34).
//!
//! This module does the compose in 20.12 fixed-point instead. It is
//! deliberately **pure** — it takes the sine/cosine of each Euler angle as
//! input rather than computing them, so the crate keeps its no-FFI charter and
//! the whole thing is host-testable. The caller supplies sin/cos from whatever
//! source is cheapest on its target (on the DS, the hardware trig LUT
//! `sinLerp`/`cosLerp`; see `bevy_nds_3d`).

use crate::{Fx32, FxVec3};

/// Compose a column-major 20.12 model matrix `T · Rx · Ry · Rz · S`, ready to
/// hand to `MTX_MULT_4x4`.
///
/// `sincos[i]` is `(sin, cos)` of the Euler rotation about axis `i`
/// (`0 = X`, `1 = Y`, `2 = Z`). The rotation order matches the Geometry
/// Engine's successive-`glRotate` convention and
/// [`bevy_nds_3d_cull::world_aabb`]: a vertex is rotated Z, then Y, then X
/// (i.e. `R = Rx · Ry · Rz`). `scale` is a per-axis multiplier applied first.
///
/// The result is the same layout `glam::Mat4::to_cols_array` produces (column
/// major: four columns of `[x, y, z, w]`), so it is a drop-in replacement for
/// the old `f32` `model_matrix`.
pub fn model_matrix(translation: FxVec3, sincos: [(Fx32, Fx32); 3], scale: FxVec3) -> [i32; 16] {
    let (sx, cx) = sincos[0];
    let (sy, cy) = sincos[1];
    let (sz, cz) = sincos[2];

    // R = Rx·Ry·Rz, expanded once symbolically (same product the f32 path built
    // via three Mat4 multiplies). Rows are the matrix rows; r[row][col].
    let r = [
        [cy * cz, -(cy * sz), sy],
        [cx * sz + sx * sy * cz, cx * cz - sx * sy * sz, -(sx * cy)],
        [sx * sz - cx * sy * cz, sx * cz + cx * sy * sz, cx * cy],
    ];
    let s = [scale.x, scale.y, scale.z];

    // Upper-left 3x3 is R · diag(scale): scaling post-multiplies, so column `c`
    // of R is scaled by `s[c]`. Translation fills the 4th column; bottom row is
    // `[0, 0, 0, 1]`. Emitted column-major.
    [
        (r[0][0] * s[0]).raw(),
        (r[1][0] * s[0]).raw(),
        (r[2][0] * s[0]).raw(),
        0,
        (r[0][1] * s[1]).raw(),
        (r[1][1] * s[1]).raw(),
        (r[2][1] * s[1]).raw(),
        0,
        (r[0][2] * s[2]).raw(),
        (r[1][2] * s[2]).raw(),
        (r[2][2] * s[2]).raw(),
        0,
        translation.x.raw(),
        translation.y.raw(),
        translation.z.raw(),
        Fx32::ONE.raw(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(sin, cos)` of an angle in radians, as `Fx32` — test helper only.
    fn sc(rad: f32) -> (Fx32, Fx32) {
        (
            Fx32::from_f32(libm::sinf(rad)),
            Fx32::from_f32(libm::cosf(rad)),
        )
    }

    const ZERO_ROT: [(Fx32, Fx32); 3] = [(Fx32::ZERO, Fx32::ONE); 3];

    /// Raw 20.12 close-enough comparison (fixed-point + trig rounding).
    fn approx(got: i32, want: f32) {
        let want_raw = (want * 4096.0) as i32;
        assert!(
            (got - want_raw).abs() <= 8,
            "got raw {got} (≈{}), want ≈{want}",
            got as f32 / 4096.0
        );
    }

    #[test]
    fn identity() {
        let m = model_matrix(FxVec3::default(), ZERO_ROT, FxVec3::from_f32(1.0, 1.0, 1.0));
        let want = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0,
        ];
        for (g, w) in m.iter().zip(want) {
            approx(*g, w);
        }
    }

    #[test]
    fn pure_translation_lands_in_column_3() {
        let m = model_matrix(
            FxVec3::from_f32(2.0, -3.0, 4.0),
            ZERO_ROT,
            FxVec3::from_f32(1.0, 1.0, 1.0),
        );
        approx(m[12], 2.0);
        approx(m[13], -3.0);
        approx(m[14], 4.0);
        approx(m[15], 1.0);
    }

    #[test]
    fn scale_lands_on_diagonal() {
        let m = model_matrix(FxVec3::default(), ZERO_ROT, FxVec3::from_f32(2.0, 3.0, 4.0));
        approx(m[0], 2.0);
        approx(m[5], 3.0);
        approx(m[10], 4.0);
    }

    #[test]
    fn rotation_about_z_90_deg() {
        // Rz(90°): x-axis maps to +y, y-axis maps to -x (column-major: column 0
        // is where the local x-axis points). cos90≈0, sin90≈1.
        let m = model_matrix(
            FxVec3::default(),
            [ZERO_ROT[0], ZERO_ROT[1], sc(core::f32::consts::FRAC_PI_2)],
            FxVec3::from_f32(1.0, 1.0, 1.0),
        );
        // Column 0 = R·x = (cos, sin, 0) = (0, 1, 0).
        approx(m[0], 0.0);
        approx(m[1], 1.0);
        // Column 1 = R·y = (-sin, cos, 0) = (-1, 0, 0).
        approx(m[4], -1.0);
        approx(m[5], 0.0);
        approx(m[10], 1.0);
    }

    #[test]
    fn rotation_matches_aabb_corner_order() {
        // A point rotated by this matrix must match bevy_nds_3d_cull's world_aabb
        // convention (Z then Y then X). Here we check the combined rotation of a
        // unit-x vector against a hand-rolled triple-rotation.
        let (ax, ay, az) = (0.3f32, -0.5f32, 0.9f32);
        let m = model_matrix(
            FxVec3::default(),
            [sc(ax), sc(ay), sc(az)],
            FxVec3::from_f32(1.0, 1.0, 1.0),
        );
        // Reference: rotate (1,0,0) by Rz, then Ry, then Rx in f32.
        let (sx, cx) = (libm::sinf(ax), libm::cosf(ax));
        let (sy, cy) = (libm::sinf(ay), libm::cosf(ay));
        let (sz, cz) = (libm::sinf(az), libm::cosf(az));
        let v = [1.0f32, 0.0, 0.0];
        let rz = [v[0] * cz - v[1] * sz, v[0] * sz + v[1] * cz, v[2]];
        let ry = [rz[0] * cy + rz[2] * sy, rz[1], -rz[0] * sy + rz[2] * cy];
        let rx = [ry[0], ry[1] * cx - ry[2] * sx, ry[1] * sx + ry[2] * cx];
        // Column 0 of the matrix is the image of the local x-axis.
        approx(m[0], rx[0]);
        approx(m[1], rx[1]);
        approx(m[2], rx[2]);
    }
}
