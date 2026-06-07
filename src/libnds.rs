//! Minimal hand-written FFI bindings to the parts of libnds we use.
//!
//! This keeps the boilerplate self-contained (no bindgen / libclang build
//! dependency). Only a small surface is declared — enough to set up the text
//! console, read the input keys and synchronise to the display refresh.

#![allow(non_camel_case_types)]

use core::ffi::{c_char, c_int, c_void};

// libnds key bit masks (see <nds/input.h>).
pub const KEY_A: u32 = 1 << 0;
pub const KEY_B: u32 = 1 << 1;
pub const KEY_SELECT: u32 = 1 << 2;
pub const KEY_START: u32 = 1 << 3;
pub const KEY_RIGHT: u32 = 1 << 4;
pub const KEY_LEFT: u32 = 1 << 5;
pub const KEY_UP: u32 = 1 << 6;
pub const KEY_DOWN: u32 = 1 << 7;

unsafe extern "C" {
    /// Initialise a simple text console on the sub (bottom) screen.
    pub fn consoleDemoInit() -> *mut c_void;
    /// Clear the active console.
    pub fn consoleClear();
    /// printf to the active console (integer-only variant from libnds).
    pub fn iprintf(fmt: *const c_char, ...) -> c_int;
    /// Block until the next vertical blank (~60 Hz), pacing the game loop.
    pub fn swiWaitForVBlank();
    /// Latch the current button state; call once per frame before reading keys.
    pub fn scanKeys();
    /// Buttons currently held down (bitfield of `KEY_*`).
    pub fn keysHeld() -> u32;
}

unsafe extern "C" {
    /// newlib aligned allocation, backing our global allocator.
    pub fn memalign(align: usize, size: usize) -> *mut c_void;
    /// newlib free.
    pub fn free(ptr: *mut c_void);
}
