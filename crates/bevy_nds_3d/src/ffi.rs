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
const GFX_CLEAR_DEPTH: *mut u16 = 0x0400_0354 as *mut u16;
const GFX_POLY_FORMAT: *mut u32 = 0x0400_04A4 as *mut u32;
const GFX_BEGIN: *mut u32 = 0x0400_0500 as *mut u32;
const GFX_END: *mut u32 = 0x0400_0504 as *mut u32;
const GFX_FLUSH: *mut u32 = 0x0400_0540 as *mut u32;
const GFX_VIEWPORT: *mut u32 = 0x0400_0580 as *mut u32;

const MATRIX_CONTROL: *mut u32 = 0x0400_0440 as *mut u32;
const MATRIX_PUSH: *mut u32 = 0x0400_0444 as *mut u32;
const MATRIX_POP: *mut u32 = 0x0400_0448 as *mut u32;
const MATRIX_TRANSLATE: *mut i32 = 0x0400_0470 as *mut i32;
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

/// `POLY_ALPHA(n) = n << 16`: polygon alpha (0-31) for `glPolyFmt`.
pub const fn poly_alpha(n: u32) -> u32 {
    n << 16
}

unsafe extern "C" {
    /// Initialise the 3D engine: resets the matrix stacks and geometry state.
    pub fn glInit() -> c_int;
    /// Set the rear-plane (clear) colour; each channel and alpha are 0-31.
    pub fn glClearColor(red: u8, green: u8, blue: u8, alpha: u8);
    /// Load a perspective projection. `fovy` is in DS angle units
    /// (`degrees * 32768 / 360`); `aspect`, `znear`, `zfar` are 20.12 fixed.
    pub fn gluPerspectivef32(fovy: c_int, aspect: c_int, znear: c_int, zfar: c_int);
    /// Multiply the current matrix by a rotation of `angle` (DS angle units)
    /// about the axis `(x, y, z)`.
    pub fn glRotatef32i(angle: c_int, x: i32, y: i32, z: i32);
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

    /// Wait-free flush: hand the assembled geometry to the renderer, which
    /// swaps buffers at the next vertical blank.
    pub unsafe fn flush() {
        unsafe { write_volatile(GFX_FLUSH, 0) }
    }
}
