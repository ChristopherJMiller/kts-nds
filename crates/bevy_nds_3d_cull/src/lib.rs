//! View-frustum culling math — the DS analogue of Bevy's view-frustum culling.
//!
//! Bevy skips drawing entities whose bounding volume lies entirely outside the
//! camera's view frustum. This crate does the same, in plain `f32` so it runs
//! under the host test harness (the DS-target render crate, which pokes
//! memory-mapped hardware registers, can't be unit-tested directly).
//!
//! The flow mirrors the renderer:
//! 1. [`Frustum::perspective`] builds the six clip planes from the camera.
//! 2. [`world_aabb`] transforms a mesh's local AABB by its object transform
//!    (the same translate → rotate(X,Y,Z) → scale order the hardware matrix
//!    stack applies).
//! 3. [`Frustum::contains_aabb`] conservatively tests the (camera-relative) AABB
//!    against the planes; a `false` result means "definitely off-screen, skip
//!    the draw call".
//!
//! The camera here only translates (no rotation), matching `bevy_nds_3d`'s view
//! matrix, so the test is done in *camera-relative* space: subtract the camera
//! position from the world AABB and compare against an origin-apex frustum.

#![no_std]

/// A clip plane `n·p + d ≥ 0` for points inside the frustum. Normals point
/// **inward**. Not normalised — fine for half-space containment tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    /// Inward-pointing normal `(x, y, z)`.
    pub n: [f32; 3],
    /// Plane offset.
    pub d: f32,
}

impl Plane {
    /// Signed half-space value for `p`; `≥ 0` is inside.
    fn eval(&self, p: [f32; 3]) -> f32 {
        self.n[0] * p[0] + self.n[1] * p[1] + self.n[2] * p[2] + self.d
    }
}

/// A view frustum as six inward-facing planes, in **camera-relative** space
/// (camera at the origin, looking down `-Z`).
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    pub planes: [Plane; 6],
}

impl Frustum {
    /// Build a perspective frustum matching `gluPerspective`.
    ///
    /// `fov_y_radians` is the vertical field of view, `aspect` is width / height,
    /// and `near` / `far` are positive clip distances. The four side planes pass
    /// through the origin (the camera), so their offset is zero.
    pub fn perspective(fov_y_radians: f32, aspect: f32, near: f32, far: f32) -> Self {
        let th = libm::tanf(fov_y_radians * 0.5);
        let thx = th * aspect;
        Self {
            planes: [
                // Looking down -Z: a point is inside when -z ∈ [near, far] and
                // |x| ≤ -z·thx, |y| ≤ -z·th.
                Plane {
                    n: [1.0, 0.0, -thx],
                    d: 0.0,
                }, // left
                Plane {
                    n: [-1.0, 0.0, -thx],
                    d: 0.0,
                }, // right
                Plane {
                    n: [0.0, 1.0, -th],
                    d: 0.0,
                }, // bottom
                Plane {
                    n: [0.0, -1.0, -th],
                    d: 0.0,
                }, // top
                Plane {
                    n: [0.0, 0.0, -1.0],
                    d: -near,
                }, // near
                Plane {
                    n: [0.0, 0.0, 1.0],
                    d: far,
                }, // far
            ],
        }
    }

    /// Conservative AABB-vs-frustum test. `min`/`max` are the box corners in the
    /// same camera-relative space as the frustum. Returns `false` only when the
    /// box is **entirely** outside some plane (safe to cull); otherwise `true`.
    pub fn contains_aabb(&self, min: [f32; 3], max: [f32; 3]) -> bool {
        for plane in &self.planes {
            // The "positive vertex": the AABB corner farthest along the inward
            // normal. If even it is outside, the whole box is outside.
            let p = [
                if plane.n[0] >= 0.0 { max[0] } else { min[0] },
                if plane.n[1] >= 0.0 { max[1] } else { min[1] },
                if plane.n[2] >= 0.0 { max[2] } else { min[2] },
            ];
            if plane.eval(p) < 0.0 {
                return false;
            }
        }
        true
    }
}

/// Transform a local-space AABB into world space under a translate → rotate
/// (Euler X, then Y, then Z) → scale transform, returning the enclosing world
/// AABB.
///
/// The rotation order matches the hardware matrix stack in `bevy_nds_3d`'s
/// renderer (successive `glRotate` X, Y, Z calls post-multiply, so a vertex is
/// effectively `T · Rx · Ry · Rz · S · v`). Because rotation tilts the box, all
/// eight corners are transformed and re-bounded.
pub fn world_aabb(
    local_min: [f32; 3],
    local_max: [f32; 3],
    translation: [f32; 3],
    rotation_radians: [f32; 3],
    scale: [f32; 3],
) -> ([f32; 3], [f32; 3]) {
    let (sx, cx) = (
        libm::sinf(rotation_radians[0]),
        libm::cosf(rotation_radians[0]),
    );
    let (sy, cy) = (
        libm::sinf(rotation_radians[1]),
        libm::cosf(rotation_radians[1]),
    );
    let (sz, cz) = (
        libm::sinf(rotation_radians[2]),
        libm::cosf(rotation_radians[2]),
    );

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];

    for i in 0..8 {
        // Pick this corner from min/max along each axis.
        let c = [
            if i & 1 == 0 {
                local_min[0]
            } else {
                local_max[0]
            },
            if i & 2 == 0 {
                local_min[1]
            } else {
                local_max[1]
            },
            if i & 4 == 0 {
                local_min[2]
            } else {
                local_max[2]
            },
        ];
        // Scale.
        let s = [c[0] * scale[0], c[1] * scale[1], c[2] * scale[2]];
        // Rotate Z, then Y, then X (applied to the vertex in that order).
        let rz = [s[0] * cz - s[1] * sz, s[0] * sz + s[1] * cz, s[2]];
        let ry = [rz[0] * cy + rz[2] * sy, rz[1], -rz[0] * sy + rz[2] * cy];
        let rx = [ry[0], ry[1] * cx - ry[2] * sx, ry[1] * sx + ry[2] * cx];
        // Translate.
        let w = [
            rx[0] + translation[0],
            rx[1] + translation[1],
            rx[2] + translation[2],
        ];
        for axis in 0..3 {
            if w[axis] < min[axis] {
                min[axis] = w[axis];
            }
            if w[axis] > max[axis] {
                max[axis] = w[axis];
            }
        }
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frustum() -> Frustum {
        // 90° vertical fov, square aspect, near 0.1, far 40 — easy boundaries.
        Frustum::perspective(core::f32::consts::FRAC_PI_2, 1.0, 0.1, 40.0)
    }

    #[test]
    fn box_in_front_is_visible() {
        let f = frustum();
        // A unit box a few units down -Z, centred on the axis.
        assert!(f.contains_aabb([-0.5, -0.5, -5.5], [0.5, 0.5, -4.5]));
    }

    #[test]
    fn box_behind_camera_is_culled() {
        let f = frustum();
        // Entirely behind the near plane (+Z side).
        assert!(!f.contains_aabb([-0.5, -0.5, 1.0], [0.5, 0.5, 2.0]));
    }

    #[test]
    fn box_beyond_far_is_culled() {
        let f = frustum();
        assert!(!f.contains_aabb([-0.5, -0.5, -60.0], [0.5, 0.5, -59.0]));
    }

    #[test]
    fn box_far_to_the_side_is_culled() {
        let f = frustum();
        // At z=-2 the half-extent is 2 (90° fov); x≈100 is way outside.
        assert!(!f.contains_aabb([100.0, -0.5, -2.5], [101.0, 0.5, -1.5]));
    }

    #[test]
    fn straddling_box_is_kept() {
        let f = frustum();
        // Spans from behind to in front of the camera — must not be culled.
        assert!(f.contains_aabb([-0.5, -0.5, -5.0], [0.5, 0.5, 5.0]));
    }

    #[test]
    fn world_aabb_translates() {
        let (min, max) = world_aabb(
            [-1.0, -1.0, -1.0],
            [1.0, 1.0, 1.0],
            [10.0, 0.0, -5.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        );
        assert_eq!(min, [9.0, -1.0, -6.0]);
        assert_eq!(max, [11.0, 1.0, -4.0]);
    }

    #[test]
    fn world_aabb_scales() {
        let (min, max) = world_aabb(
            [-1.0, -1.0, -1.0],
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [2.0, 3.0, 4.0],
        );
        assert_eq!(min, [-2.0, -3.0, -4.0]);
        assert_eq!(max, [2.0, 3.0, 4.0]);
    }

    #[test]
    fn world_aabb_rotation_grows_bounds() {
        // A 90° turn about Z swaps X/Y extents; a flat box becomes tall.
        let (min, max) = world_aabb(
            [-2.0, -0.5, -0.5],
            [2.0, 0.5, 0.5],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, core::f32::consts::FRAC_PI_2],
            [1.0, 1.0, 1.0],
        );
        // X extent shrinks to ~0.5, Y extent grows to ~2.0.
        assert!((max[0] - 0.5).abs() < 1e-4, "max.x = {}", max[0]);
        assert!((max[1] - 2.0).abs() < 1e-4, "max.y = {}", max[1]);
        assert!((min[0] + 0.5).abs() < 1e-4);
        assert!((min[1] + 2.0).abs() < 1e-4);
    }
}
