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
//! - [`TouchPick`] — hardware touch-screen picking: which mesh entity is under
//!   the pen, via the DS position-test + pick-matrix pipeline.
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
use bevy_input::touch::Touches;
use bevy_math::Vec3;
use bevy_nds_3d_cull::{Frustum, world_aabb_fx};
use bevy_nds_math::{Fx32, FxVec3};
use bevy_nds_video::DsScreen;

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

/// `(sin, cos)` of a radian angle via the DS **hardware trig LUT**
/// (`sinLerp`/`cosLerp`), as 20.12 [`Fx32`]. This is the cheap replacement for
/// the soft-float `sin`/`cos` that `model_matrix` and `world_aabb` used to pay
/// per object per frame (issue #34): a table lookup instead of hundreds of
/// emulated-float cycles. The angle is reduced into one circle (`[0, 32768)`)
/// so it fits the `i16` the LUT takes, regardless of how many turns it carries.
fn sin_cos_fx(rad: f32) -> (Fx32, Fx32) {
    let angle = rad_to_angle(rad).rem_euclid(ANGLE_FULL_CIRCLE as i32) as i16;
    // sinLerp/cosLerp return Q12 (4096 = 1.0), i.e. a raw 20.12 value.
    let s = unsafe { ffi::sinLerp(angle) } as i32;
    let c = unsafe { ffi::cosLerp(angle) } as i32;
    (Fx32::from_raw(s), Fx32::from_raw(c))
}

/// The **inverse** camera-rotation matrix `R⁻¹ = Rx(-pitch)·Ry(-yaw)` as a
/// column-major 20.12 4x4 — the rotation half of the view transform (the
/// camera orientation is `R = Ry(yaw)·Rx(pitch)`). Reuses the fixed-point
/// compose + hardware trig LUT from #34, so the per-frame view rotation costs
/// no soft-float. Multiply it onto the modelview before the view translate.
fn view_rotation(pitch: f32, yaw: f32) -> [i32; 16] {
    let sincos = [
        sin_cos_fx(-pitch),
        sin_cos_fx(-yaw),
        (Fx32::ZERO, Fx32::ONE), // (sin 0, cos 0): no roll
    ];
    bevy_nds_math::model_matrix(FxVec3::default(), sincos, FxVec3::from_f32(1.0, 1.0, 1.0))
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

/// A baked libnds **display list** for a static lit mesh, produced at build time
/// by `include_obj!`. The render loop hands it to the GPU with a single
/// `glCallList` (asynchronous DMA), so the 33 MHz ARM9 does no per-frame
/// fixed-point or normal maths — which is what keeps a ~650-triangle model at
/// frame rate.
#[derive(Clone)]
pub struct BakedMesh {
    /// The display list: a leading body-length word, then packed Geometry Engine
    /// commands (begin, per-vertex normal + vertex16, end). Consumed by
    /// [`ffi::gl::call_list`].
    pub words: Cow<'static, [u32]>,
    /// Local-space axis-aligned bounds (`[min, max]`), for frustum culling.
    pub aabb: [Vec3; 2],
}

/// Per-object render data derived from a [`Transform3d`] (+ the mesh's local
/// AABB), cached so it is recomputed **only when the transform or mesh changes**
/// — not every frame. Composing the model matrix used to cost ~2 ms/object of
/// soft-float trig every frame, which capped scenes at a handful of meshes
/// (issue #34); static geometry now composes once and dynamic objects use the
/// cheap hardware-LUT path ([`sin_cos_fx`]).
///
/// A **required component** of [`DsMesh`], so every mesh entity has one
/// automatically. [`recompute_mesh_draw`] keeps it current; [`render_3d`] and
/// [`pick_3d`] read it. Fields are crate-private (an implementation detail).
#[derive(Component, Clone, Copy)]
pub struct MeshDraw {
    /// Composed column-major 20.12 model matrix (`T · Rx · Ry · Rz · S`), fed to
    /// `MTX_MULT_4x4` each frame.
    model: [i32; 16],
    /// Cached world-space AABB (`[min, max]`) for frustum culling. Only
    /// meaningful when `has_bounds` is set.
    world_min: [f32; 3],
    world_max: [f32; 3],
    /// Whether the mesh carried a baked AABB (so culling can use `world_*`).
    /// Hand-authored meshes without bounds always draw, as before.
    has_bounds: bool,
}

impl Default for MeshDraw {
    fn default() -> Self {
        // Identity matrix; recomputed before the first draw (a freshly added
        // Transform3d reads as `is_changed`).
        Self {
            model: [
                ONE_RAW_I32,
                0,
                0,
                0, //
                0,
                ONE_RAW_I32,
                0,
                0, //
                0,
                0,
                ONE_RAW_I32,
                0, //
                0,
                0,
                0,
                ONE_RAW_I32,
            ],
            world_min: [0.0; 3],
            world_max: [0.0; 3],
            has_bounds: false,
        }
    }
}

/// Raw 20.12 value of `1.0` (`1 << 12`), for the identity [`MeshDraw`] default.
const ONE_RAW_I32: i32 = 1 << 12;

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
#[require(MeshDraw)]
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

    /// Build a hardware-lit mesh from a baked `&'static` display list and its
    /// local-space bounds. This is the form `include_obj!` emits: all the
    /// fixed-point/normal packing and command encoding happen at build time, so
    /// rendering is a single `glCallList`. `words` must be a libnds display list
    /// (see [`BakedMesh`]).
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

    /// Build a hardware-lit mesh from an **owned** (heap) display list and its
    /// local-space bounds. This is the runtime counterpart to [`from_baked`]:
    /// the words come from a NitroFS asset loaded at runtime rather than baked
    /// into the ROM, so the list is owned rather than `&'static`. The buffer is
    /// already cache-flushed by [`load`](DsMesh::load) before reaching here.
    ///
    /// [`from_baked`]: DsMesh::from_baked
    pub fn from_owned(words: Vec<u32>, aabb_min: [f32; 3], aabb_max: [f32; 3]) -> Self {
        Self {
            tris: Cow::Borrowed(&[]),
            lit: true,
            baked: Some(BakedMesh {
                words: Cow::Owned(words),
                aabb: [
                    Vec3::new(aabb_min[0], aabb_min[1], aabb_min[2]),
                    Vec3::new(aabb_max[0], aabb_max[1], aabb_max[2]),
                ],
            }),
        }
    }

    /// Load a display-list model from the ROM filesystem (NitroFS) at runtime.
    ///
    /// `path` is a NUL-terminated `nitro:/` path (e.g. `b"nitro:/teapot.dl\0"`).
    /// The file is the `.dl` format written by `obj2dl` / the `build.rs` asset
    /// pipeline: a small header (magic + AABB + word count) followed by the
    /// libnds display list. The bytes are flushed from the data cache so the
    /// Geometry Engine's DMA (`glCallList`) reads them correctly.
    ///
    /// Returns `None` if the file is missing, truncated, or not a valid asset.
    /// [`bevy_nds_nitrofs::NitroFsPlugin`] must have mounted the filesystem first
    /// (it's included in `DsPlugins`).
    pub fn load(path: &[u8]) -> Option<Self> {
        let bytes = bevy_nds_nitrofs::read_file(path)?;
        let (words, aabb_min, aabb_max) = parse_dl_asset(&bytes)?;
        // SAFETY: `words` is a contiguous `[u32]`; reinterpret as bytes purely to
        // hand its address/length to the cache-flush. No aliasing writes occur.
        let word_bytes =
            unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, words.len() * 4) };
        bevy_nds_nitrofs::flush_dcache(word_bytes);
        Some(Self::from_owned(words, aabb_min, aabb_max))
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

/// Parse the `.dl` runtime asset format written by `obj2dl` /
/// `bevy_nds_3d_obj::model_to_le_bytes`: a little-endian header (magic `"BDL1"`,
/// six `f32` AABB components, a `u32` word count) followed by the display list.
/// Returns the owned word buffer and the bounds, or `None` if malformed.
fn parse_dl_asset(bytes: &[u8]) -> Option<(Vec<u32>, [f32; 3], [f32; 3])> {
    /// ASCII `"BDL1"`, matching `bevy_nds_3d_obj::ASSET_MAGIC`.
    const MAGIC: u32 = u32::from_le_bytes(*b"BDL1");
    const HEADER: usize = 32; // magic(4) + aabb(24) + count(4)

    if bytes.len() < HEADER || read_u32(bytes, 0) != MAGIC {
        return None;
    }

    let mut aabb = [0.0f32; 6];
    for (i, slot) in aabb.iter_mut().enumerate() {
        *slot = f32::from_bits(read_u32(bytes, 4 + i * 4));
    }

    let count = read_u32(bytes, 28) as usize;
    if bytes.len() < HEADER + count * 4 {
        return None;
    }

    let mut words = Vec::with_capacity(count);
    for i in 0..count {
        words.push(read_u32(bytes, HEADER + i * 4));
    }

    Some((
        words,
        [aabb[0], aabb[1], aabb[2]],
        [aabb[3], aabb[4], aabb[5]],
    ))
}

/// Read a little-endian `u32` at `offset` (caller guarantees the bytes exist).
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

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

/// Marker: skip drawing (and picking) this mesh entity. Insert to hide, remove
/// to show — the analogue of Bevy's `Visibility::Hidden`, the cheap way to toggle
/// a mesh off without despawning it or zeroing its scale (a scale-0 mesh still
/// plots a degenerate point).
#[derive(Component, Default, Clone, Copy)]
pub struct Hidden;

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
    /// Pitch (rotation about X), radians: negative looks **down**. With
    /// [`yaw`](Self::yaw) the camera orientation is `R = Ry(yaw)·Rx(pitch)`;
    /// both default to `0` (look straight down `-Z`, the legacy behaviour).
    pub pitch: f32,
    /// Yaw (rotation about Y), radians.
    pub yaw: f32,
}

impl Default for Camera3d {
    fn default() -> Self {
        Self {
            fov_degrees: 70.0,
            near: 0.1,
            far: 40.0,
            // Pulled back along +Z, looking toward the origin.
            position: Vec3::new(0.0, 0.0, 3.0),
            pitch: 0.0,
            yaw: 0.0,
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

/// Maximum distance (world units) from the camera at which a bounded mesh still
/// renders. Beyond it, [`render_3d`] skips the draw — a per-object range cull
/// that holds the polygon budget when neighbour zones are resident (#27 seamless
/// streaming), instead of "only the current zone renders". Pairs with the depth
/// fog, which is tuned to be opaque by roughly this distance, so culled geometry
/// fades into the rear plane rather than popping. Hand-authored meshes without a
/// baked AABB are never range-culled (same as frustum culling).
#[derive(Resource, Clone, Copy)]
pub struct RenderRange(pub f32);

impl Default for RenderRange {
    fn default() -> Self {
        // Generous default: the facility's neighbour zone sits ~4–8 units out, so
        // it renders; the fog does the visible fade. Lower it for tighter culls.
        Self(12.0)
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

/// Rear-plane (clear) colour, 0-31 per channel — a near-black cyber blue. Fog
/// fades geometry toward this, so things dissolve into the background.
const CLEAR_RGB: (u8, u8, u8) = (2, 2, 6);
/// Fog-shift: each density-table entry spans `0x400 >> FOG_SHIFT` depth units.
/// 6 → 16 units/entry, a ~0x200-wide band over the 32 entries.
const FOG_SHIFT: u16 = 6;
/// Depth at which fog begins (0-0x7FFF). With `near`/`far` = 0.1/40 perspective
/// compresses all scene geometry into the top of the depth buffer (~0x7B00 near
/// the avatar … ~0x7F00 for a resident neighbour ~8u out), so fog starts high so
/// the active zone stays clear and only the neighbour fades. Tuned via preview.
const FOG_OFFSET: u32 = 0x7E00;

/// Bring up the DS 3D engine: power it on, switch the main engine to a 3D video
/// mode, set the rear-plane colour / depth, and configure depth fog. Runs in
/// [`Startup`] so it lands after `bevy_nds`'s `PreStartup` 2D video setup.
fn init_3d() {
    unsafe {
        gl::enable_3d_video();
        ffi::glInit();
        let (r, g, b) = CLEAR_RGB;
        ffi::glClearColor(r, g, b, 31); // alpha 31 is required for the rear plane to fog
        gl::clear_depth(ffi::GL_MAX_DEPTH);
        gl::viewport(0, 0, 255, 191);
        // Smooth polygon silhouettes — a near-free win on the 256×192 LCD.
        gl::enable_antialias();

        // Depth fog (#27 seamless streaming): fades resident-neighbour geometry
        // into the rear plane as it recedes, masking the range cull / zone seam.
        // A linear density ramp to opaque; colour = the clear colour so geometry
        // dissolves into the background rather than greying out. Tuned via
        // preview-rom (FOG_OFFSET/SHIFT) so the active zone stays clear.
        let mut density = [0u8; 32];
        let mut i = 0;
        while i < density.len() {
            density[i] = ((i * 127) / (density.len() - 1)) as u8;
            i += 1;
        }
        gl::setup_fog((r, g, b, 31), FOG_SHIFT, FOG_OFFSET, &density);
        gl::enable(ffi::GL_FOG);
    }
}

/// True if a mesh's **cached** world bounds intersect the camera frustum (so it
/// should be drawn). The world AABB was composed once by [`recompute_mesh_draw`]
/// (the expensive trig is off the per-frame path); here we only shift it into
/// camera-relative space and run the cheap plane test ([`bevy_nds_3d_cull`]).
fn aabb_visible(frustum: &Frustum, draw: &MeshDraw) -> bool {
    // `frustum` is already in world space (built via `Frustum::to_world` for the
    // current camera position + orientation), so the cached world AABB is tested
    // directly — no per-object shift, and it handles a rotated camera.
    frustum.contains_aabb(draw.world_min, draw.world_max)
}

/// True if a mesh's cached world-AABB centre is within `range` units of the
/// camera — the per-object range cull for resident-neighbour streaming (#27).
/// Squared compare, so no `sqrt` on the per-frame path.
fn within_range(draw: &MeshDraw, cam: Vec3, range: f32) -> bool {
    let cx = (draw.world_min[0] + draw.world_max[0]) * 0.5 - cam.x;
    let cy = (draw.world_min[1] + draw.world_max[1]) * 0.5 - cam.y;
    let cz = (draw.world_min[2] + draw.world_max[2]) * 0.5 - cam.z;
    cx * cx + cy * cy + cz * cz <= range * range
}

/// Recompute each mesh's cached [`MeshDraw`] (model matrix + world AABB) — but
/// **only** for entities whose [`Transform3d`] or [`DsMesh`] changed since last
/// frame. Static geometry composes once; moving objects use the hardware trig
/// LUT ([`sin_cos_fx`]). This is the fix for the per-object compose cost in
/// issue #34. Runs in [`Last`], before [`render_3d`].
fn recompute_mesh_draw(mut meshes: Query<(Ref<Transform3d>, Ref<DsMesh>, &mut MeshDraw)>) {
    for (transform, mesh, mut draw) in &mut meshes {
        // `Added` reads as changed, so newly spawned entities compose here too.
        if !transform.is_changed() && !mesh.is_changed() {
            continue;
        }

        let sincos = [
            sin_cos_fx(transform.rotation.x),
            sin_cos_fx(transform.rotation.y),
            sin_cos_fx(transform.rotation.z),
        ];
        let translation = FxVec3::from_f32(
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        );
        let scale = FxVec3::from_f32(transform.scale.x, transform.scale.y, transform.scale.z);

        draw.model = bevy_nds_math::model_matrix(translation, sincos, scale);

        if let Some(baked) = &mesh.baked {
            let [lmin, lmax] = baked.aabb;
            // All fixed-point: reuses the LUT sin/cos + the same translation/scale
            // the matrix used, so culling adds no soft-float on a moving mesh (#34).
            let (wmin, wmax) = world_aabb_fx(
                [
                    Fx32::from_f32(lmin.x),
                    Fx32::from_f32(lmin.y),
                    Fx32::from_f32(lmin.z),
                ],
                [
                    Fx32::from_f32(lmax.x),
                    Fx32::from_f32(lmax.y),
                    Fx32::from_f32(lmax.z),
                ],
                translation,
                sincos,
                scale,
            );
            draw.world_min = [wmin[0].to_f32(), wmin[1].to_f32(), wmin[2].to_f32()];
            draw.world_max = [wmax[0].to_f32(), wmax[1].to_f32(), wmax[2].to_f32()];
            draw.has_bounds = true;
        } else {
            draw.has_bounds = false;
        }
    }
}

/// Submit every [`DsMesh`] to the 3D hardware each frame, transformed by its
/// [`Transform3d`] via the hardware matrix stack and viewed through [`Camera3d`].
/// Lit meshes are shaded by the [`DsLights`] resource using their per-vertex
/// normals and (optional) [`DsMaterial`]; unlit meshes use flat vertex colours.
/// Runs in [`Last`], after game systems have updated transforms.
fn render_3d(
    camera: Res<Camera3d>,
    range: Res<RenderRange>,
    lights: Res<DsLights>,
    meshes: Query<(&MeshDraw, &DsMesh, Option<&DsMaterial>), Without<Hidden>>,
) {
    let aspect = to_fix(256.0 / 192.0);
    let fovy = rad_to_angle(camera.fov_degrees * (TAU / 360.0));

    // View-frustum culling (à la Bevy): reject meshes whose world bounds fall
    // entirely outside the camera frustum before issuing any Geometry Engine
    // work. Transformed into world space for the camera's position + orientation
    // so the cached world AABBs can be tested directly.
    let frustum = Frustum::perspective(
        camera.fov_degrees * (TAU / 360.0),
        256.0 / 192.0,
        camera.near,
        camera.far,
    )
    .to_world(camera.pitch, camera.yaw, camera.position.to_array());

    unsafe {
        gl::viewport(0, 0, 255, 191);

        // Projection.
        gl::matrix_mode(ffi::GL_PROJECTION);
        gl::load_identity();
        ffi::gluPerspectivef32(fovy, aspect, to_fix(camera.near), to_fix(camera.far));

        // View: rotate by R⁻¹ then translate by -position (so vertices land in
        // the camera's rotated, translated frame).
        gl::matrix_mode(ffi::GL_MODELVIEW);
        gl::load_identity();
        gl::mult_matrix_4x4(&view_rotation(camera.pitch, camera.yaw));
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
                // Hot path: a softfloat `Vec3::normalize_or_zero()` costs an
                // `f32::sqrt` plus three `f32` divs (hundreds of ARM9 cycles
                // each). The fixed-point path goes through the DS math
                // coprocessor for ~one sqrt + three divides at < 40 cycles
                // apiece. Same result, much cheaper per frame per light.
                let d = bevy_nds_math::FxVec3::from_f32(
                    light.direction.x,
                    light.direction.y,
                    light.direction.z,
                )
                .normalize_or_zero();
                gl::light(
                    id as u32,
                    ffi::rgb15(light.color[0], light.color[1], light.color[2]),
                    ffi::normal_pack_fx(d.x, d.y, d.z),
                );
                light_mask |= ffi::poly_light(id as u32);
            }
        }

        for (draw, mesh, mat) in &meshes {
            // Cull bounded meshes (baked / loaded models) that are off-screen or
            // beyond the render range (resident-neighbour streaming, #27).
            // Hand-authored meshes without an AABB always draw.
            if draw.has_bounds
                && (!aabb_visible(&frustum, draw)
                    || !within_range(draw, camera.position, range.0))
            {
                continue;
            }

            gl::push_matrix();
            gl::mult_matrix_4x4(&draw.model);

            if mesh.lit {
                let m = mat.copied().unwrap_or_default();
                gl::material(
                    ffi::rgb15(m.diffuse[0], m.diffuse[1], m.diffuse[2]),
                    ffi::rgb15(m.ambient[0], m.ambient[1], m.ambient[2]),
                    true,
                );
                gl::poly_fmt(ffi::poly_alpha(31) | ffi::POLY_CULL_BACK | ffi::POLY_FOG | light_mask);

                if let Some(baked) = &mesh.baked {
                    // Fast path: hand the whole display list to the GPU via DMA
                    // (it carries its own begin/end), no per-vertex CPU work.
                    gl::call_list(&baked.words);
                } else {
                    gl::begin(ffi::GL_TRIANGLES);
                    for tri in mesh.tris.iter() {
                        for v in tri {
                            // Normals are expected unit length (baked meshes pack
                            // them at build time), so no runtime sqrt here.
                            let n = v.normal;
                            gl::normal(ffi::normal_pack(n.x, n.y, n.z));
                            gl::vertex_v16(to_v16(v.pos.x), to_v16(v.pos.y), to_v16(v.pos.z));
                        }
                    }
                    gl::end();
                }
            } else {
                gl::poly_fmt(ffi::poly_alpha(31) | ffi::POLY_CULL_NONE | ffi::POLY_FOG);

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
    }
}

/// Hand the current frame's assembled geometry to the renderer. Split out of
/// [`render_3d`] so the picking pass ([`pick_3d`]) can run *between* drawing and
/// the flush, sharing this frame's matrices and geometry-engine state.
fn flush_3d() {
    unsafe { gl::flush() }
}

/// The result of touch-screen 3D picking: which mesh entity (if any) is under
/// the pen this frame, nearest the camera.
///
/// Every entity with a [`DsMesh`] and [`Transform3d`] is pickable. Read this
/// resource alongside the standard `Touches` input — e.g. `touches.any_just_pressed()`
/// gated on `picking.entity == Some(my_entity)` is "the player tapped my object".
/// `entity` is `None` when the screen is not being touched, or when no mesh sits
/// under the touch point. Picking is meaningful only while the 3D output is on
/// the screen being touched (the DS touch panel is the physical bottom LCD).
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchPick {
    /// The nearest mesh entity under the touch point, if any.
    pub entity: Option<Entity>,
}

/// Side length, in pixels, of the square pick region sampled under the pen. A
/// few pixels gives a forgiving hit target without picking distant neighbours.
const PICK_BOX: i32 = 4;

/// Hardware 3D picking: while the screen is touched, find which mesh sits under
/// the pen and record it in [`TouchPick`].
///
/// This mirrors the classic DS technique (see libnds' picking example): re-draw
/// the scene a second time through a [`gluPickMatrix`](ffi::gluPickMatrix)
/// projection that clips away everything outside a small box under the cursor,
/// into an off-screen viewport so nothing is visible. For each object a hardware
/// **position test** records its distance from the camera, and the polygon
/// counter reveals whether any of its geometry survived the clip. The nearest
/// object that did is the one being touched. It runs after [`render_3d`] and
/// before [`flush_3d`], reusing this frame's geometry-engine state.
fn pick_3d(
    camera: Res<Camera3d>,
    touches: Res<Touches>,
    mut pick: ResMut<TouchPick>,
    meshes: Query<(Entity, &MeshDraw, &DsMesh), Without<Hidden>>,
) {
    // Nothing under the pen if the pen is up.
    let Some(touch) = touches.iter().next() else {
        if pick.entity.is_some() {
            pick.entity = None;
        }
        return;
    };
    let px = touch.position().x as i32;
    let py = touch.position().y as i32;

    let aspect = to_fix(256.0 / 192.0);
    let fovy = rad_to_angle(camera.fov_degrees * (TAU / 360.0));
    let viewport = [0, 0, 255, 191];
    let frustum = Frustum::perspective(
        camera.fov_degrees * (TAU / 360.0),
        256.0 / 192.0,
        camera.near,
        camera.far,
    )
    .to_world(camera.pitch, camera.yaw, camera.position.to_array());

    let mut nearest = i32::MAX;
    let mut hovered = None;

    unsafe {
        // Render the picking pass off-screen so it never shows.
        gl::viewport(0, 192, 0, 192);

        // Projection: the same perspective as the display pass, but pre-multiplied
        // by a pick matrix so only geometry under the pen survives clipping. The
        // pick matrix expects GL-style (bottom-up) Y, hence `191 - py`.
        gl::matrix_mode(ffi::GL_PROJECTION);
        gl::load_identity();
        gl::pick_matrix(px, 191 - py, PICK_BOX, PICK_BOX, &viewport);
        ffi::gluPerspectivef32(fovy, aspect, to_fix(camera.near), to_fix(camera.far));

        // View: identical to the display pass (rotate by R⁻¹, then translate).
        gl::matrix_mode(ffi::GL_MODELVIEW);
        gl::load_identity();
        gl::mult_matrix_4x4(&view_rotation(camera.pitch, camera.yaw));
        gl::translate(
            to_fix(-camera.position.x),
            to_fix(-camera.position.y),
            to_fix(-camera.position.z),
        );

        // Count every polygon under the pen regardless of facing.
        gl::poly_fmt(ffi::poly_alpha(31) | ffi::POLY_CULL_NONE);

        for (entity, draw, mesh) in &meshes {
            if draw.has_bounds && !aabb_visible(&frustum, draw) {
                continue;
            }

            gl::push_matrix();
            gl::mult_matrix_4x4(&draw.model);

            // Begin checking this object: wait for the previous test/draw to
            // finish, test the object's origin, and snapshot the polygon count.
            while gl::pos_test_busy() {}
            while gl::gfx_busy() {}
            gl::pos_test(0, 0, 0);
            let polys_before = gl::polygon_ram_usage();

            submit_geometry(mesh);

            // Finish: if this object drew any polygons under the pen and it is
            // nearer than the current best, it becomes the hit.
            while gl::gfx_busy() {}
            while gl::pos_test_busy() {}
            if gl::polygon_ram_usage() > polys_before {
                let w = gl::pos_test_w();
                if w <= nearest {
                    nearest = w;
                    hovered = Some(entity);
                }
            }

            gl::pop_matrix(1);
        }
    }

    if pick.entity != hovered {
        pick.entity = hovered;
    }
}

/// Submit a mesh's bare geometry to the Geometry Engine (no colour, normals or
/// material) — enough for the picking pass to count polygons under the pen.
///
/// # Safety
/// The matrices and polygon format must already be set up; runs on the DS.
unsafe fn submit_geometry(mesh: &DsMesh) {
    unsafe {
        if let Some(baked) = &mesh.baked {
            gl::call_list(&baked.words);
        } else {
            gl::begin(ffi::GL_TRIANGLES);
            for tri in mesh.tris.iter() {
                for v in tri {
                    gl::vertex_v16(to_v16(v.pos.x), to_v16(v.pos.y), to_v16(v.pos.z));
                }
            }
            gl::end();
        }
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
            .init_resource::<RenderRange>()
            .init_resource::<DsLights>()
            .init_resource::<TouchPick>()
            .add_systems(Startup, init_3d)
            .add_systems(
                Last,
                (
                    apply_display,
                    recompute_mesh_draw,
                    render_3d,
                    pick_3d,
                    flush_3d,
                )
                    .chain(),
            );
    }
}

/// Common imports for games using the 3D backend.
pub mod prelude {
    pub use crate::{
        BakedMesh, Camera3d, DirectionalLight, Display3d, Ds3dPlugin, DsLights, DsMaterial, DsMesh,
        Hidden, TouchPick, Transform3d, Vertex,
    };
    pub use bevy_math::Vec3;
    pub use bevy_nds_3d_macros::include_obj;
}
