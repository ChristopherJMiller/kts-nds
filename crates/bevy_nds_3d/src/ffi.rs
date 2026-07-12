//! Hand-written FFI to the Nintendo DS 3D engine (libnds), mirroring the style
//! of `bevy_nds`'s own `ffi.rs`: no bindgen, minimal surface.
//!
//! There are two kinds of entry point here:
//!
//! - A handful of **real libnds functions** ([`glInit`], [`glClearColor`],
//!   [`gluPerspectivef32`], [`glRotatef32i`]) declared `extern "C"` and resolved
//!   against `libnds9.a` at final link (the game crate's `build.rs` adds it).
//! - The per-vertex / per-matrix `gl*` calls, which in libnds are
//!   `static inline` functions that simply poke the Geometry Engine's
//!   memory-mapped **command registers**. Inline header functions are not
//!   linkable symbols, so we reimplement them in Rust (the [`gl`] module) by
//!   writing those same registers directly — citing `<nds/arm9/videoGL.h>` and
//!   `<nds/arm9/video.h>` for each address.

#![allow(non_snake_case)]

use core::ffi::c_int;
use core::ptr::{read_volatile, write_volatile};

// --- Display / power registers (see <nds/arm9/video.h>, <nds/system.h>) ------

/// 3D control register (`DISP3DCNT`/`GFX_CONTROL`): render-mode bits incl. edge
/// anti-aliasing. See `<nds/arm9/videoGL.h>` `DISP3DCNT_ENUM`.
const GFX_CONTROL: *mut u16 = 0x0400_0060 as *mut u16;
/// `GL_ANTIALIAS = BIT(4)`: hardware edge anti-aliasing.
const GFX_ANTIALIAS: u16 = 1 << 4;
/// Main engine display control.
const REG_DISPCNT: *mut u32 = 0x0400_0000 as *mut u32;
/// ARM9 power control (16-bit). We OR in the 3D core + matrix engine bits.
const REG_POWERCNT: *mut u16 = 0x0400_0304 as *mut u16;

/// `MODE_0_3D = MODE_0_2D | DISPLAY_BG0_ACTIVE | ENABLE_3D`: video mode 0 on the
/// main engine with BG0 reassigned to the 3D core.
pub const MODE_0_3D: u32 = (1 << 16) | (1 << 8) | (1 << 3);
/// `POWER_3D_CORE | POWER_MATRIX` (the low 16 bits actually written to POWERCNT).
const POWER_3D: u16 = (1 << 3) | (1 << 2);
/// `POWER_SWAP_LCDS` (`BIT(15)`): when set the main engine drives the **top**
/// LCD, when clear it drives the **bottom** (the sub engine always takes the
/// other). See `<nds/system.h>` (`lcdMainOnTop` / `lcdMainOnBottom`).
const POWER_SWAP_LCDS: u16 = 1 << 15;

// --- Geometry Engine command registers (see <nds/arm9/video.h>) --------------

const GFX_COLOR: *mut u32 = 0x0400_0480 as *mut u32;
const GFX_VERTEX16: *mut u32 = 0x0400_048C as *mut u32;
const GFX_NORMAL: *mut u32 = 0x0400_0484 as *mut u32;
const GFX_CLEAR_DEPTH: *mut u16 = 0x0400_0354 as *mut u16;
const GFX_POLY_FORMAT: *mut u32 = 0x0400_04A4 as *mut u32;
const GFX_BEGIN: *mut u32 = 0x0400_0500 as *mut u32;
const GFX_END: *mut u32 = 0x0400_0504 as *mut u32;
const GFX_FLUSH: *mut u32 = 0x0400_0540 as *mut u32;
const GFX_VIEWPORT: *mut u32 = 0x0400_0580 as *mut u32;

// Fog registers (see <nds/arm9/video.h>). Depth fog fades polygons toward
// `GFX_FOG_COLOR` as their depth crosses `GFX_FOG_OFFSET` + the density table.
const GFX_FOG_COLOR: *mut u32 = 0x0400_0358 as *mut u32;
const GFX_FOG_OFFSET: *mut u32 = 0x0400_035C as *mut u32;
/// 32-entry fog density table (one byte each, 0-127).
const GFX_FOG_TABLE: *mut u8 = 0x0400_0360 as *mut u8;
/// `GFX_CONTROL` (DISP3DCNT) fog-shift field mask (bits 8-11).
const GFX_FOG_SHIFT_MASK: u16 = 0xF0FF;

// Edge-marking registers. See <nds/arm9/video.h> / <nds/arm9/videoGL.h>. The
// rear/clear-plane polygon ID (`GFX_CLEAR_COLOR` bits 24-29) is set through
// libnds' `glClearPolyID` so it stays consistent with libnds' cached
// clear-colour word.
/// Edge-marking colour table: 8 RGB15 entries. The high 3 bits of a polygon's
/// ID select which entry outlines it (`glSetOutlineColor`).
const GFX_EDGE_TABLE: *mut u16 = 0x0400_0330 as *mut u16;
/// `GL_OUTLINE = BIT(5)` of `GFX_CONTROL`: edge-marking (polygon outline) enable.
const GFX_OUTLINE: u16 = 1 << 5;

// Geometry test / status registers used for hardware picking (see
// <nds/arm9/video.h>, <nds/arm9/postest.h>).
/// 3D engine status; bit 0 is "position/box/vertex test busy", bit 27 is the
/// general geometry-engine-busy flag.
const GFX_STATUS: *const u32 = 0x0400_0600 as *const u32;
/// Position-test command register: write `VERTEX_PACK(x, y)` then `z`.
const GFX_POS_TEST: *mut u32 = 0x0400_05C4 as *mut u32;
/// Position-test result vector `[x, y, z, w]` (20.12 fixed); `[3]` is the W
/// magnitude, i.e. distance from the camera.
const GFX_POS_RESULT: *const i32 = 0x0400_0620 as *const i32;
/// Running count of polygons submitted this frame (resets at flush / vblank). A
/// jump after drawing one object means that object had geometry under the test
/// point.
const GFX_POLYGON_RAM_USAGE: *const u16 = 0x0400_0604 as *const u16;

/// `GFX_STATUS_TEST_BUSY = BIT(0)`.
const GFX_STATUS_TEST_BUSY: u32 = 1 << 0;
/// `GFX_STATUS_BUSY = BIT(27)`.
const GFX_STATUS_BUSY: u32 = 1 << 27;

// Lighting / material command registers (see <nds/arm9/video.h>).
const GFX_LIGHT_VECTOR: *mut u32 = 0x0400_04C8 as *mut u32;
const GFX_LIGHT_COLOR: *mut u32 = 0x0400_04CC as *mut u32;
const GFX_DIFFUSE_AMBIENT: *mut u32 = 0x0400_04C0 as *mut u32;
const GFX_SPECULAR_EMISSION: *mut u32 = 0x0400_04C4 as *mut u32;

const MATRIX_CONTROL: *mut u32 = 0x0400_0440 as *mut u32;
const MATRIX_PUSH: *mut u32 = 0x0400_0444 as *mut u32;
const MATRIX_POP: *mut u32 = 0x0400_0448 as *mut u32;
const MATRIX_TRANSLATE: *mut i32 = 0x0400_0470 as *mut i32;
const MATRIX_MULT_4X4: *mut i32 = 0x0400_0460 as *mut i32;
const MATRIX_IDENTITY: *mut u32 = 0x0400_0454 as *mut u32;

// --- GL enums / constants (see <nds/arm9/videoGL.h>) -------------------------

/// Reset value for the depth buffer (`GL_MAX_DEPTH`).
pub const GL_MAX_DEPTH: u16 = 0x7FFF;
/// `glBegin` primitive: a list of independent triangles.
pub const GL_TRIANGLES: u32 = 0;
/// Matrix-mode selectors for `glMatrixMode`.
pub const GL_PROJECTION: u32 = 0;
pub const GL_MODELVIEW: u32 = 2;
/// Don't cull any polygons (`POLY_CULL_NONE = 3 << 6`).
pub const POLY_CULL_NONE: u32 = 3 << 6;
/// Cull back-facing polygons (`POLY_CULL_BACK = 2 << 6`).
pub const POLY_CULL_BACK: u32 = 2 << 6;
/// `POLY_FOG = BIT(15)`: apply fog to polygons drawn with this format.
pub const POLY_FOG: u32 = 1 << 15;
/// `GL_FOG = BIT(7)` of `GFX_CONTROL` (DISP3DCNT): the fog master enable, passed
/// to [`gl::enable`].
pub const GL_FOG: u16 = 1 << 7;

/// `POLY_ALPHA(n) = n << 16`: polygon alpha (0-31) for `glPolyFmt`.
pub const fn poly_alpha(n: u32) -> u32 {
    n << 16
}

/// `POLY_FORMAT_LIGHT{i} = BIT(i)`: enable hardware light `i` (0-3) for the
/// following polygons. OR these into the polygon format.
pub const fn poly_light(id: u32) -> u32 {
    1 << id
}

/// `POLY_ID(n) = n << 24`: polygon ID (0-63) for the following polys. Edge
/// marking outlines the boundary between differing IDs (and the differing rear
/// plane), and antialiasing keys its blending off it. Give each mesh a distinct
/// ID so object silhouettes separate.
pub const fn poly_id(n: u32) -> u32 {
    (n & 0x3F) << 24
}


/// Pack a 0-255-per-channel RGB colour into the DS 15-bit `RGB15` format (5 bits
/// per channel), used by light and material colour registers.
pub const fn rgb15(r: u8, g: u8, b: u8) -> u32 {
    (r as u32 >> 3) | ((g as u32 >> 3) << 5) | ((b as u32 >> 3) << 10)
}

/// Convert a normalised float component to the DS 10-bit signed `v10` fixed
/// format (1.0 maps to `0x1FF`), as `floattov10` does in `<nds/arm9/videoGL.h>`.
pub fn float_to_v10(v: f32) -> u32 {
    let x = if v >= 1.0 {
        0x1FF // largest representable +ve (≈ +0.998); 0x200 would read as -1.0
    } else if v < -1.0 {
        0x200 // -1.0 in 10-bit two's complement
    } else {
        ((v * 512.0) as i32) & 0x3FF
    };
    x as u32
}

/// Pack three `v10` normal/direction components into a single command word
/// (`NORMAL_PACK`), 10 bits each.
pub fn normal_pack(x: f32, y: f32, z: f32) -> u32 {
    float_to_v10(x) | (float_to_v10(y) << 10) | (float_to_v10(z) << 20)
}

/// Pack a 20.12 fixed-point normal/direction directly into the `NORMAL_PACK`
/// word, skipping the f32 → v10 conversion. v10 has 9 fractional bits; 20.12
/// has 12, so the conversion is `raw >> 3` (with saturation at `±1`). Used by
/// the per-frame light-direction normalize, which already lives in fixed-point.
pub fn normal_pack_fx(
    x: bevy_nds_math::Fx32,
    y: bevy_nds_math::Fx32,
    z: bevy_nds_math::Fx32,
) -> u32 {
    fx32_to_v10(x) | (fx32_to_v10(y) << 10) | (fx32_to_v10(z) << 20)
}

fn fx32_to_v10(v: bevy_nds_math::Fx32) -> u32 {
    // 20.12: 1.0 = 0x1000 (4096), so the v10 saturation point is at raw 4096.
    let r = v.raw();
    let clamped = if r >= 0x1000 {
        0x1FF
    } else if r <= -0x1000 {
        0x200
    } else {
        (r >> 3) & 0x3FF
    };
    clamped as u32
}

unsafe extern "C" {
    /// Initialise the 3D engine: resets the matrix stacks and geometry state.
    pub fn glInit() -> c_int;
    /// Set the rear-plane (clear) colour; each channel and alpha are 0-31.
    pub fn glClearColor(red: u8, green: u8, blue: u8, alpha: u8);
    /// Load a perspective projection. `fovy` is in DS angle units
    /// (`degrees * 32768 / 360`); `aspect`, `znear`, `zfar` are 20.12 fixed.
    pub fn gluPerspectivef32(fovy: c_int, aspect: c_int, znear: c_int, zfar: c_int);
    /// Send a packed display list to the Geometry Engine via asynchronous DMA.
    /// The first word of `list` is the body length in `u32`s, followed by the
    /// packed command stream. See `<nds/arm9/videoGL.h>`.
    pub fn glCallList(list: *const u32);

    /// Multiply the current (projection) matrix by a "pick matrix" that restricts
    /// rendering to a `width`x`height` pixel box centred on (`x`, `y`), in the
    /// given `viewport` (`[x, y, w, h]`). Used for hardware picking: combined
    /// with the normal projection it makes the Geometry Engine clip away
    /// everything not under the cursor. See `<nds/arm9/videoGL.h>`.
    pub fn gluPickMatrix(x: c_int, y: c_int, width: c_int, height: c_int, viewport: *const c_int);

    /// Hardware trig LUT (`<nds/arm9/trig_lut.h>`). `angle` is in DS angle units
    /// (`DEGREES_IN_CIRCLE = 1 << 15 = 32768` per full circle); the result is
    /// 20.12 fixed-point (`4096 = 1.0`). A table lookup + lerp — vastly cheaper
    /// than the software `sinf`/`cosf` that capped scene density (issue #34).
    pub fn sinLerp(angle: i16) -> i16;
    pub fn cosLerp(angle: i16) -> i16;

    /// Enable/disable fog on the rear (clear) plane. A real libnds symbol (not
    /// inline), unlike the other `glFog*` setters. See `<nds/arm9/videoGL.h>`.
    pub fn glClearFogEnable(enable: bool);

    /// Set the rear/clear-plane polygon ID (0-63). A real libnds symbol that
    /// updates libnds' cached clear-colour word, so it stays consistent with
    /// [`glClearColor`] / [`glClearFogEnable`]. Edge marking outlines every
    /// object whose polygon ID differs from this. See `<nds/arm9/videoGL.h>`.
    pub fn glClearPolyID(id: u8);
}

/// Safe-to-call (still `unsafe`: they touch MMIO) reimplementations of the
/// libnds inline `gl*` command-register writes.
pub mod gl {
    use super::*;

    /// Power on the 3D core + matrix engine, then switch the main engine to a
    /// 3D video mode. Call once, after the 2D consoles are up.
    ///
    /// # Safety
    /// Must run on the DS with the display hardware initialised.
    pub unsafe fn enable_3d_video() {
        unsafe {
            let p = read_volatile(REG_POWERCNT) | POWER_3D;
            write_volatile(REG_POWERCNT, p);
            write_volatile(REG_DISPCNT, MODE_0_3D);
        }
    }

    /// OR `bits` into `GFX_CONTROL` (DISP3DCNT) — libnds `glEnable`. Used to flip
    /// the fog master enable ([`GL_FOG`]) and the render-style bits. Read-modify-
    /// write, so it preserves the render-mode bits `glInit` set.
    ///
    /// # Safety
    /// Call after `glInit`, on the DS with the 3D engine initialised.
    pub unsafe fn enable(bits: u16) {
        unsafe {
            let c = read_volatile(GFX_CONTROL);
            write_volatile(GFX_CONTROL, c | bits);
        }
    }

    /// AND `!bits` out of `GFX_CONTROL` (DISP3DCNT) — libnds `glDisable`.
    /// Read-modify-write, so it preserves the other render-mode bits.
    ///
    /// # Safety
    /// Call after `glInit`, on the DS with the 3D engine initialised.
    pub unsafe fn disable(bits: u16) {
        unsafe {
            let c = read_volatile(GFX_CONTROL);
            write_volatile(GFX_CONTROL, c & !bits);
        }
    }

    /// Toggle hardware edge anti-aliasing ([`GFX_ANTIALIAS`]). AA and edge
    /// marking interfere (they both touch silhouette pixels), so the render
    /// style enables exactly one of the two.
    ///
    /// # Safety
    /// Call after `glInit`, on the DS with the 3D engine initialised.
    pub unsafe fn set_antialias(on: bool) {
        unsafe {
            if on {
                enable(GFX_ANTIALIAS)
            } else {
                disable(GFX_ANTIALIAS)
            }
        }
    }

    /// Toggle edge marking ([`GFX_OUTLINE`]) and fill all 8 edge-table entries
    /// with `colour` (RGB15) so a polygon of any ID is outlined in the same
    /// colour. Objects are outlined wherever their polygon ID differs from a
    /// neighbour's or the rear plane — set the rear plane to a reserved ID with
    /// [`set_clear_poly_id`] and give meshes distinct IDs via [`poly_id`].
    ///
    /// # Safety
    /// Call after `glInit`, on the DS with the 3D engine initialised.
    pub unsafe fn set_outline(on: bool, colour: u32) {
        unsafe {
            if on {
                for i in 0..8 {
                    write_volatile(GFX_EDGE_TABLE.add(i), colour as u16);
                }
                enable(GFX_OUTLINE);
            } else {
                disable(GFX_OUTLINE);
            }
        }
    }

    /// Set the rear/clear-plane polygon ID (0-63) via libnds. Reserve one ID for
    /// the rear plane so every object silhouette (which uses a different ID)
    /// gets edge-marked against the background.
    ///
    /// # Safety
    /// Call after `glInit`, on the DS with the 3D engine initialised.
    pub unsafe fn set_clear_poly_id(id: u8) {
        unsafe { glClearPolyID(id) }
    }

    /// Configure depth fog: rear-plane fog on, fog `colour` (RGB + alpha, each
    /// 0-31), `shift` (each density-table entry spans `0x400 >> shift` depth
    /// units), `offset` (depth where fog begins, 0-0x7FFF), and a 32-entry
    /// `density` table (each 0-127). Mirrors the libnds `glFog*` inline setters
    /// (`GFX_CONTROL` fog-shift field + `GFX_FOG_COLOR/OFFSET/TABLE`), plus the
    /// real `glClearFogEnable`. Enable [`GL_FOG`] separately via [`enable`].
    ///
    /// # Safety
    /// Call after `glInit`, on the DS with the 3D engine initialised.
    pub unsafe fn setup_fog(colour: (u8, u8, u8, u8), shift: u16, offset: u32, density: &[u8; 32]) {
        unsafe {
            glClearFogEnable(true);
            let (r, g, b, a) = colour;
            write_volatile(
                GFX_FOG_COLOR,
                rgb15(r << 3, g << 3, b << 3) | ((a as u32) << 16),
            );
            write_volatile(GFX_FOG_OFFSET, offset);
            // Fog-shift lives in bits 8-11 of GFX_CONTROL (read-modify-write).
            let c = read_volatile(GFX_CONTROL) & GFX_FOG_SHIFT_MASK;
            write_volatile(GFX_CONTROL, c | ((shift & 0xF) << 8));
            for (i, &d) in density.iter().enumerate() {
                write_volatile(GFX_FOG_TABLE.add(i), d);
            }
        }
    }

    /// Choose which physical LCD the main engine (and thus the 3D output) drives.
    /// `true` puts it on the top screen, `false` on the bottom; the sub engine
    /// (text consoles) always takes the other screen. This is a single coupled
    /// hardware toggle — both engines swap together.
    ///
    /// # Safety
    /// Must run on the DS with the display hardware initialised.
    pub unsafe fn set_main_lcd_on_top(on_top: bool) {
        unsafe {
            let p = read_volatile(REG_POWERCNT);
            let p = if on_top {
                p | POWER_SWAP_LCDS
            } else {
                p & !POWER_SWAP_LCDS
            };
            write_volatile(REG_POWERCNT, p);
        }
    }

    /// Reset the depth buffer's clear value.
    pub unsafe fn clear_depth(depth: u16) {
        unsafe { write_volatile(GFX_CLEAR_DEPTH, depth) }
    }

    /// Set the drawing viewport (inclusive pixel bounds).
    pub unsafe fn viewport(x1: u8, y1: u8, x2: u8, y2: u8) {
        let v = x1 as u32 | (y1 as u32) << 8 | (x2 as u32) << 16 | (y2 as u32) << 24;
        unsafe { write_volatile(GFX_VIEWPORT, v) }
    }

    /// Select the matrix the following matrix ops act on.
    pub unsafe fn matrix_mode(mode: u32) {
        unsafe { write_volatile(MATRIX_CONTROL, mode) }
    }

    /// Load the identity matrix into the current matrix.
    pub unsafe fn load_identity() {
        unsafe { write_volatile(MATRIX_IDENTITY, 0) }
    }

    /// Push the current matrix onto its stack.
    pub unsafe fn push_matrix() {
        unsafe { write_volatile(MATRIX_PUSH, 0) }
    }

    /// Pop `num` matrices off the current stack.
    pub unsafe fn pop_matrix(num: i32) {
        unsafe { write_volatile(MATRIX_POP, num as u32) }
    }

    /// Multiply the current matrix by a translation, components in 20.12 fixed.
    pub unsafe fn translate(x: i32, y: i32, z: i32) {
        unsafe {
            write_volatile(MATRIX_TRANSLATE, x);
            write_volatile(MATRIX_TRANSLATE, y);
            write_volatile(MATRIX_TRANSLATE, z);
        }
    }

    /// Multiply the current matrix by a full 4x4 matrix (`MTX_MULT_4x4`),
    /// column-major, each component in 20.12 fixed. Composing an object's
    /// transform on the CPU and sending it as one matrix replaces the separate
    /// translate/rotate/rotate/rotate/scale Geometry Engine commands.
    pub unsafe fn mult_matrix_4x4(m: &[i32; 16]) {
        unsafe {
            for &word in m {
                write_volatile(MATRIX_MULT_4X4, word);
            }
        }
    }

    /// Set the polygon attributes for following polygons.
    pub unsafe fn poly_fmt(params: u32) {
        unsafe { write_volatile(GFX_POLY_FORMAT, params) }
    }

    /// Begin a primitive group.
    pub unsafe fn begin(mode: u32) {
        unsafe { write_volatile(GFX_BEGIN, mode) }
    }

    /// End the current primitive group.
    pub unsafe fn end() {
        unsafe { write_volatile(GFX_END, 0) }
    }

    /// Set the colour for following vertices, from 8-bit RGB (low 3 bits drop).
    pub unsafe fn color3b(red: u8, green: u8, blue: u8) {
        let c = (red as u32 >> 3) | ((green as u32 >> 3) << 5) | ((blue as u32 >> 3) << 10);
        unsafe { write_volatile(GFX_COLOR, c) }
    }

    /// Emit a vertex with 16-bit (20.12 fixed) components.
    pub unsafe fn vertex_v16(x: i16, y: i16, z: i16) {
        let xy = ((y as u16 as u32) << 16) | (x as u16 as u32);
        unsafe {
            write_volatile(GFX_VERTEX16, xy);
            write_volatile(GFX_VERTEX16, z as u16 as u32);
        }
    }

    /// Set the current normal (triggers the per-vertex lighting calculation for
    /// the following vertex when lighting is enabled). `packed` is a
    /// [`normal_pack`] word.
    pub unsafe fn normal(packed: u32) {
        unsafe { write_volatile(GFX_NORMAL, packed) }
    }

    /// Draw a baked display list via libnds `glCallList`. The Geometry Engine
    /// consumes the whole self-contained command block (its own `begin` …
    /// vertices … `end`) in one asynchronous DMA burst, so the 33 MHz ARM9 does
    /// no per-vertex work — this is what keeps large `include_obj!` models at
    /// frame rate. `words` is a libnds display list (leading body-length word
    /// then packed commands), as produced by the macro at build time.
    ///
    /// # Safety
    /// Poly format / material / lights must be set up beforehand. Unlike the
    /// per-vertex calls this must **not** be wrapped in [`begin`]/[`end`] — the
    /// list carries its own. `words` must be a valid display list.
    pub unsafe fn call_list(words: &[u32]) {
        unsafe { glCallList(words.as_ptr()) }
    }

    /// Configure directional light `id` (0-3): its `color` (RGB15) and its
    /// direction (a [`normal_pack`] word). The direction is transformed by the
    /// current modelview matrix, so set lights after loading the view.
    pub unsafe fn light(id: u32, color: u32, direction: u32) {
        unsafe {
            write_volatile(GFX_LIGHT_VECTOR, (id << 30) | direction);
            write_volatile(GFX_LIGHT_COLOR, (id << 30) | color);
        }
    }

    /// Set the material's diffuse and ambient reflection colours (RGB15). With
    /// `set_vertex_color`, the diffuse colour also seeds the vertex colour so a
    /// lit, untextured surface shows the material colour. Specular and emission
    /// are cleared.
    pub unsafe fn material(diffuse: u32, ambient: u32, set_vertex_color: bool) {
        let svc = if set_vertex_color { 1 << 15 } else { 0 };
        unsafe {
            write_volatile(GFX_DIFFUSE_AMBIENT, diffuse | svc | (ambient << 16));
            write_volatile(GFX_SPECULAR_EMISSION, 0);
        }
    }

    /// Wait-free flush: hand the assembled geometry to the renderer, which
    /// swaps buffers at the next vertical blank.
    pub unsafe fn flush() {
        unsafe { write_volatile(GFX_FLUSH, 0) }
    }

    /// True while a position / box / vertex test is still running.
    pub unsafe fn pos_test_busy() -> bool {
        unsafe { read_volatile(GFX_STATUS) & GFX_STATUS_TEST_BUSY != 0 }
    }

    /// True while the Geometry Engine is still drawing.
    pub unsafe fn gfx_busy() -> bool {
        unsafe { read_volatile(GFX_STATUS) & GFX_STATUS_BUSY != 0 }
    }

    /// Start an asynchronous position test for the point (`x`, `y`, `z`) — in
    /// 20.12 `v16` components — under the *current* modelview matrix. The result
    /// (its distance from the camera) is read back with [`pos_test_w`] once
    /// [`pos_test_busy`] clears. Mirrors libnds `PosTest_Asynch`.
    pub unsafe fn pos_test(x: i16, y: i16, z: i16) {
        let xy = (x as u16 as u32) | ((y as u16 as u32) << 16);
        unsafe {
            write_volatile(GFX_POS_TEST, xy);
            write_volatile(GFX_POS_TEST, z as i32 as u32);
        }
    }

    /// The W magnitude (distance from the camera) of the last position test.
    /// Smaller is nearer. Mirrors libnds `PosTestWresult`.
    pub unsafe fn pos_test_w() -> i32 {
        unsafe { read_volatile(GFX_POS_RESULT.add(3)) }
    }

    /// The number of polygons submitted to the Geometry Engine so far this
    /// frame. Compare before/after drawing an object to tell whether any of its
    /// geometry survived clipping (i.e. fell under a pick matrix).
    pub unsafe fn polygon_ram_usage() -> u16 {
        unsafe { read_volatile(GFX_POLYGON_RAM_USAGE) }
    }

    /// Multiply the current matrix by a pick matrix (see [`super::gluPickMatrix`]).
    pub unsafe fn pick_matrix(x: i32, y: i32, width: i32, height: i32, viewport: &[i32; 4]) {
        unsafe { gluPickMatrix(x, y, width, height, viewport.as_ptr()) }
    }
}
