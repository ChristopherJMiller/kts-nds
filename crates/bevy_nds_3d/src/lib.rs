//! Hardware-accelerated 3D for `bevy_nds`.
//!
//! This crate is an *additive* rendering backend: it drives the Nintendo DS
//! **hardware 3D geometry engine** and exposes it through ordinary Bevy
//! components and resources, keeping the same "describe a scene, the hardware
//! draws it" shape as desktop Bevy — but the "GPU" is the DS 3D core and the
//! draw calls are Geometry Engine command-register writes (see [`ffi`]).
//!
//! It deliberately mirrors Bevy where the hardware allows and stays honest
//! where it does not:
//!
//! - [`Transform3d`] — translation + Euler rotation, in friendly `f32` units;
//!   the DS **matrix stack** applies it in hardware (no CPU matrix maths).
//! - [`DsMesh`] — a small list of vertex-coloured triangles (with a [`DsMesh::cube`]
//!   helper). There is no asset server; meshes are spawned directly.
//! - [`Camera3d`] — a single camera resource (the DS has one projection matrix,
//!   and the 3D core only feeds the **top** screen).
//!
//! # Hardware ownership
//!
//! The 3D core lives on the DS *main* engine, so this backend takes over the
//! **top** screen. Put text/HUD on the bottom screen (the sub engine) when using
//! it. Setup runs in [`Startup`], after `bevy_nds`'s `PreStartup` video
//! bring-up, and switches the main engine into a 3D video mode.
//!
//! ```ignore
//! app.add_plugins(DsPlugins)        // bevy_nds platform layer
//!    .add_plugins(Ds3dPlugin);      // this crate
//! // ...then spawn (Transform3d, DsMesh::cube(0.6)) entities.
//! ```

#![no_std]

extern crate alloc;

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::f32::consts::TAU;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_math::Vec3;
use bevy_nds::DsScreen;

mod ffi;

use ffi::gl;

/// DS angle units in a full circle (`DEGREES_IN_CIRCLE`, see `<nds/arm9/trig_lut.h>`).
const ANGLE_FULL_CIRCLE: f32 = 32768.0;
/// 20.12 fixed-point scale (`1 << 12`).
const FIX12: f32 = 4096.0;

/// Convert a length in world units to 20.12 fixed-point.
fn to_fix(v: f32) -> i32 {
    (v * FIX12) as i32
}

/// Convert a world-unit length to a 16-bit (`v16`) vertex component.
fn to_v16(v: f32) -> i16 {
    (v * FIX12) as i16
}

/// Convert radians to DS angle units.
fn rad_to_angle(rad: f32) -> i32 {
    (rad * (ANGLE_FULL_CIRCLE / TAU)) as i32
}

/// A single coloured vertex in model space. Colour channels are 0-255 (the DS
/// keeps the top 5 bits). `normal` is the surface normal used for hardware
/// lighting; it is ignored for unlit meshes (and is `Vec3::ZERO` by default).
#[derive(Clone, Copy)]
pub struct Vertex {
    pub pos: Vec3,
    pub normal: Vec3,
    pub color: [u8; 3],
}

impl Vertex {
    /// An unlit vertex (no normal); use this for flat vertex-coloured meshes.
    pub const fn new(pos: Vec3, color: [u8; 3]) -> Self {
        Self {
            pos,
            normal: Vec3::ZERO,
            color,
        }
    }

    /// A vertex carrying a surface normal, for hardware-lit meshes. The normal
    /// should be unit length (the lit render path does not renormalise).
    pub const fn with_normal(pos: Vec3, normal: Vec3, color: [u8; 3]) -> Self {
        Self { pos, normal, color }
    }

    /// Construct from raw component arrays. This is the const-friendly form the
    /// `include_obj!` macro emits (it avoids needing `Vec3` in generated code).
    pub const fn from_raw(pos: [f32; 3], normal: [f32; 3], color: [u8; 3]) -> Self {
        Self {
            pos: Vec3::new(pos[0], pos[1], pos[2]),
            normal: Vec3::new(normal[0], normal[1], normal[2]),
            color,
        }
    }
}

/// Pre-packed Geometry Engine command words for a static lit mesh, computed at
/// build time by `include_obj!`. The render loop streams these straight to the
/// hardware with no per-frame float maths (see [`ffi::gl::stream_lit`]), which is
/// what keeps a ~650-triangle model at frame rate on the 33 MHz ARM9.
#[derive(Clone)]
pub struct BakedMesh {
    /// Flat command words, three per vertex: `[normal, vertex16-xy, vertex16-z]`.
    pub words: Cow<'static, [u32]>,
    /// Local-space axis-aligned bounds (`[min, max]`), for frustum culling.
    pub aabb: [Vec3; 2],
}

/// A drawable mesh: a flat list of triangles. Small by design — the DS has a
/// hard per-frame budget of a couple thousand polygons.
///
/// `tris` is a [`Cow`] so a mesh can either own its triangles (e.g. [`DsMesh::cube`])
/// or borrow `&'static` data baked into the ROM at compile time (e.g. the
/// `include_obj!` macro), with no per-spawn heap copy in the latter case.
///
/// When `baked` is `Some`, it carries a pre-packed command stream used by the
/// fast lit render path; `tris` is then left empty (the geometry lives in the
/// baked words). This is what `include_obj!` emits.
#[derive(Component, Clone, Default)]
pub struct DsMesh {
    pub tris: Cow<'static, [[Vertex; 3]]>,
    /// When set, the mesh is drawn with the DS hardware lighting pipeline
    /// (per-vertex normals + the [`DsLights`] resource + a [`DsMaterial`]).
    /// When clear, vertices use their flat [`Vertex::color`] directly.
    pub lit: bool,
    /// Pre-packed GE command words for the fast static-lit path (see [`BakedMesh`]).
    pub baked: Option<BakedMesh>,
}

impl DsMesh {
    /// Build a mesh that borrows `&'static` triangle data (no allocation). This
    /// is used for hand-authored static meshes; the lit path still packs each
    /// vertex per frame, so prefer [`DsMesh::from_baked`] (what `include_obj!`
    /// emits) for large lit models.
    pub const fn from_static(tris: &'static [[Vertex; 3]], lit: bool) -> Self {
        Self {
            tris: Cow::Borrowed(tris),
            lit,
            baked: None,
        }
    }

    /// Build a hardware-lit mesh from a pre-packed `&'static` command stream and
    /// its local-space bounds. This is the form `include_obj!` emits: all the
    /// fixed-point/normal packing happens at build time, so rendering is just
    /// MMIO writes. `words` must be three per vertex (see [`BakedMesh`]).
    pub const fn from_baked(words: &'static [u32], aabb_min: [f32; 3], aabb_max: [f32; 3]) -> Self {
        Self {
            tris: Cow::Borrowed(&[]),
            lit: true,
            baked: Some(BakedMesh {
                words: Cow::Borrowed(words),
                aabb: [
                    Vec3::new(aabb_min[0], aabb_min[1], aabb_min[2]),
                    Vec3::new(aabb_max[0], aabb_max[1], aabb_max[2]),
                ],
            }),
        }
    }

    /// An axis-aligned cube of side `2 * half`, with each face a distinct
    /// flat colour. Handy as a "hello, triangle" for the 3D pipeline.
    pub fn cube(half: f32) -> Self {
        let h = half;
        // (corner-a, b, c, d) wound CCW, plus the face colour.
        let faces: [([Vec3; 4], [u8; 3]); 6] = [
            // +Z front (red)
            (
                [
                    Vec3::new(-h, -h, h),
                    Vec3::new(h, -h, h),
                    Vec3::new(h, h, h),
                    Vec3::new(-h, h, h),
                ],
                [220, 40, 40],
            ),
            // -Z back (green)
            (
                [
                    Vec3::new(h, -h, -h),
                    Vec3::new(-h, -h, -h),
                    Vec3::new(-h, h, -h),
                    Vec3::new(h, h, -h),
                ],
                [40, 200, 40],
            ),
            // +X right (blue)
            (
                [
                    Vec3::new(h, -h, h),
                    Vec3::new(h, -h, -h),
                    Vec3::new(h, h, -h),
                    Vec3::new(h, h, h),
                ],
                [60, 90, 230],
            ),
            // -X left (yellow)
            (
                [
                    Vec3::new(-h, -h, -h),
                    Vec3::new(-h, -h, h),
                    Vec3::new(-h, h, h),
                    Vec3::new(-h, h, -h),
                ],
                [230, 210, 40],
            ),
            // +Y top (cyan)
            (
                [
                    Vec3::new(-h, h, h),
                    Vec3::new(h, h, h),
                    Vec3::new(h, h, -h),
                    Vec3::new(-h, h, -h),
                ],
                [40, 210, 210],
            ),
            // -Y bottom (magenta)
            (
                [
                    Vec3::new(-h, -h, -h),
                    Vec3::new(h, -h, -h),
                    Vec3::new(h, -h, h),
                    Vec3::new(-h, -h, h),
                ],
                [210, 60, 210],
            ),
        ];

        let mut tris = Vec::with_capacity(12);
        for (c, color) in faces {
            tris.push([
                Vertex::new(c[0], color),
                Vertex::new(c[1], color),
                Vertex::new(c[2], color),
            ]);
            tris.push([
                Vertex::new(c[0], color),
                Vertex::new(c[2], color),
                Vertex::new(c[3], color),
            ]);
        }
        Self {
            tris: Cow::Owned(tris),
            lit: false,
            baked: None,
        }
    }
}

/// Position and orientation of a 3D entity. The DS-native analogue of Bevy's
/// `Transform`: rotation is Euler angles (radians), applied X then Y then Z, and
/// `scale` is a per-axis multiplier (use [`Vec3::splat`] for uniform scale).
#[derive(Component, Clone, Copy)]
pub struct Transform3d {
    pub translation: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
}

impl Default for Transform3d {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
        }
    }
}

impl Transform3d {
    pub const fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
        }
    }

    /// Set a uniform scale, builder-style.
    pub const fn with_scale(mut self, scale: f32) -> Self {
        self.scale = Vec3::splat(scale);
        self
    }
}

/// The (single) 3D camera. The DS has one projection matrix and the 3D core
/// only drives one screen at a time, so this is a resource, not a component.
#[derive(Resource, Clone, Copy)]
pub struct Camera3d {
    /// Vertical field of view, in degrees.
    pub fov_degrees: f32,
    /// Near clip plane (world units).
    pub near: f32,
    /// Far clip plane (world units).
    pub far: f32,
    /// Camera position; the world is drawn relative to it.
    pub position: Vec3,
}

impl Default for Camera3d {
    fn default() -> Self {
        Self {
            fov_degrees: 70.0,
            near: 0.1,
            far: 40.0,
            // Pulled back along +Z, looking toward the origin.
            position: Vec3::new(0.0, 0.0, 3.0),
        }
    }
}

/// Which physical LCD shows the 3D output.
///
/// The DS 3D core is wired to the *main* 2D engine, and a single hardware bit
/// selects which LCD the main engine drives (the *sub* engine — the text
/// consoles — always takes the other). So this picks the 3D screen, but the two
/// engines swap *together*: moving 3D to one screen sends the text to the other.
/// Mutate it at runtime and the change is applied automatically.
#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub struct Display3d {
    /// The screen the 3D output is drawn on.
    pub screen: DsScreen,
}

impl Default for Display3d {
    fn default() -> Self {
        // Matches the DS power-on default: main engine -> top screen.
        Self {
            screen: DsScreen::Top,
        }
    }
}

/// A single hardware directional light: a direction and a colour. The DS has up
/// to four, applied per vertex in hardware.
#[derive(Clone, Copy)]
pub struct DirectionalLight {
    /// The direction the light travels (it is normalised before use). A surface
    /// whose normal faces the *opposite* way is lit brightest.
    pub direction: Vec3,
    /// Light colour, 0-255 per channel.
    pub color: [u8; 3],
}

/// The scene's (up to four) hardware directional lights. Only meshes with
/// [`DsMesh::lit`] set are affected. Mutate at runtime to move the lights.
#[derive(Resource, Clone)]
pub struct DsLights {
    pub lights: [Option<DirectionalLight>; 4],
}

impl Default for DsLights {
    fn default() -> Self {
        // A single white key light from the upper front, so lit meshes read as
        // solid out of the box.
        let mut lights = [None; 4];
        lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.4, -0.5, -0.77),
            color: [255, 255, 255],
        });
        Self { lights }
    }
}

/// Reflective material for a lit [`DsMesh`]. Lit meshes without one fall back to
/// [`DsMaterial::default`]. Ignored by unlit meshes (which use vertex colours).
#[derive(Component, Clone, Copy)]
pub struct DsMaterial {
    /// Diffuse reflection colour (the main surface colour under direct light).
    pub diffuse: [u8; 3],
    /// Ambient reflection colour (the colour in shadow / fill light).
    pub ambient: [u8; 3],
}

impl Default for DsMaterial {
    fn default() -> Self {
        Self {
            diffuse: [200, 200, 210],
            ambient: [40, 40, 55],
        }
    }
}

/// Apply the [`Display3d`] LCD assignment whenever it changes (and once at
/// startup, since `Added` resources count as changed).
fn apply_display(display: Res<Display3d>) {
    if display.is_changed() {
        unsafe { gl::set_main_lcd_on_top(display.screen == DsScreen::Top) };
    }
}

/// Bring up the DS 3D engine: power it on, switch the main engine to a 3D video
/// mode, and set the rear-plane colour / depth. Runs in [`Startup`] so it lands
/// after `bevy_nds`'s `PreStartup` 2D video setup.
fn init_3d() {
    unsafe {
        gl::enable_3d_video();
        ffi::glInit();
        ffi::glClearColor(2, 2, 6, 31);
        gl::clear_depth(ffi::GL_MAX_DEPTH);
        gl::viewport(0, 0, 255, 191);
    }
}

/// Submit every [`DsMesh`] to the 3D hardware each frame, transformed by its
/// [`Transform3d`] via the hardware matrix stack and viewed through [`Camera3d`].
/// Lit meshes are shaded by the [`DsLights`] resource using their per-vertex
/// normals and (optional) [`DsMaterial`]; unlit meshes use flat vertex colours.
/// Runs in [`Last`], after game systems have updated transforms.
fn render_3d(
    camera: Res<Camera3d>,
    lights: Res<DsLights>,
    meshes: Query<(&Transform3d, &DsMesh, Option<&DsMaterial>)>,
) {
    let aspect = to_fix(256.0 / 192.0);
    let fovy = rad_to_angle(camera.fov_degrees * (TAU / 360.0));

    unsafe {
        gl::viewport(0, 0, 255, 191);

        // Projection.
        gl::matrix_mode(ffi::GL_PROJECTION);
        gl::load_identity();
        ffi::gluPerspectivef32(fovy, aspect, to_fix(camera.near), to_fix(camera.far));

        // View: draw the world relative to the camera.
        gl::matrix_mode(ffi::GL_MODELVIEW);
        gl::load_identity();
        gl::translate(
            to_fix(-camera.position.x),
            to_fix(-camera.position.y),
            to_fix(-camera.position.z),
        );

        // Configure the directional lights in view space (their direction is
        // latched relative to the current modelview, before any per-object
        // transform) and remember which ones to enable on lit polygons.
        let mut light_mask = 0u32;
        for (id, light) in lights.lights.iter().enumerate() {
            if let Some(light) = light {
                let d = light.direction.normalize_or_zero();
                gl::light(
                    id as u32,
                    ffi::rgb15(light.color[0], light.color[1], light.color[2]),
                    ffi::normal_pack(d.x, d.y, d.z),
                );
                light_mask |= ffi::poly_light(id as u32);
            }
        }

        for (transform, mesh, mat) in &meshes {
            gl::push_matrix();
            gl::translate(
                to_fix(transform.translation.x),
                to_fix(transform.translation.y),
                to_fix(transform.translation.z),
            );
            ffi::glRotatef32i(rad_to_angle(transform.rotation.x), 1 << 12, 0, 0);
            ffi::glRotatef32i(rad_to_angle(transform.rotation.y), 0, 1 << 12, 0);
            ffi::glRotatef32i(rad_to_angle(transform.rotation.z), 0, 0, 1 << 12);
            gl::scale(
                to_fix(transform.scale.x),
                to_fix(transform.scale.y),
                to_fix(transform.scale.z),
            );

            if mesh.lit {
                let m = mat.copied().unwrap_or_default();
                gl::material(
                    ffi::rgb15(m.diffuse[0], m.diffuse[1], m.diffuse[2]),
                    ffi::rgb15(m.ambient[0], m.ambient[1], m.ambient[2]),
                    true,
                );
                gl::poly_fmt(ffi::poly_alpha(31) | ffi::POLY_CULL_BACK | light_mask);

                gl::begin(ffi::GL_TRIANGLES);
                if let Some(baked) = &mesh.baked {
                    // Fast path: stream pre-packed command words, no float maths.
                    gl::stream_lit(&baked.words);
                } else {
                    for tri in mesh.tris.iter() {
                        for v in tri {
                            // Normals are expected unit length (baked meshes pack
                            // them at build time), so no runtime sqrt here.
                            let n = v.normal;
                            gl::normal(ffi::normal_pack(n.x, n.y, n.z));
                            gl::vertex_v16(to_v16(v.pos.x), to_v16(v.pos.y), to_v16(v.pos.z));
                        }
                    }
                }
                gl::end();
            } else {
                gl::poly_fmt(ffi::poly_alpha(31) | ffi::POLY_CULL_NONE);

                gl::begin(ffi::GL_TRIANGLES);
                for tri in mesh.tris.iter() {
                    for v in tri {
                        gl::color3b(v.color[0], v.color[1], v.color[2]);
                        gl::vertex_v16(to_v16(v.pos.x), to_v16(v.pos.y), to_v16(v.pos.z));
                    }
                }
                gl::end();
            }

            gl::pop_matrix(1);
        }

        gl::flush();
    }
}

/// Drives the DS hardware 3D engine, rendering [`DsMesh`] + [`Transform3d`]
/// entities through a [`Camera3d`]. The 3D output goes to the screen selected by
/// the [`Display3d`] resource (top by default). Add it *after* `bevy_nds`'s
/// `DsPlugins`.
pub struct Ds3dPlugin;

impl Plugin for Ds3dPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Camera3d>()
            .init_resource::<Display3d>()
            .init_resource::<DsLights>()
            .add_systems(Startup, init_3d)
            .add_systems(Last, (apply_display, render_3d).chain());
    }
}

/// Common imports for games using the 3D backend.
pub mod prelude {
    pub use crate::{
        BakedMesh, Camera3d, DirectionalLight, Display3d, Ds3dPlugin, DsLights, DsMaterial, DsMesh,
        Transform3d, Vertex,
    };
    pub use bevy_math::Vec3;
    pub use bevy_nds_3d_macros::include_obj;
}
