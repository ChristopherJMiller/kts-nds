//! Minimal hand-written FFI bindings to the parts of libnds we use.
//!
//! This keeps the integration self-contained (no bindgen / libclang build
//! dependency). Only a small surface is declared — enough to set up text
//! consoles on both screens, read the input keys and synchronise to the
//! display refresh. The actual symbols are resolved against `libnds9.a` at
//! final link time (see the game crate's `build.rs`).

#![allow(non_camel_case_types)]
#![allow(dead_code)]

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
pub const KEY_R: u32 = 1 << 8;
pub const KEY_L: u32 = 1 << 9;
pub const KEY_X: u32 = 1 << 10;
pub const KEY_Y: u32 = 1 << 11;

// libnds background type / size enums (see <nds/arm9/background.h>). These are
// passed to `consoleInit` to describe the tiled text layer.
pub const BG_TYPE_TEXT_4BPP: c_int = 1; // BgType_Text4bpp
// Written as in the libnds header: BgSize_T_256x256 = (0 << 14) | (1 << 16).
#[allow(clippy::identity_op)]
pub const BG_SIZE_T_256X256: c_int = (0 << 14) | (1 << 16);

// Memory-mapped display/VRAM registers (see <nds/arm9/video.h>). We poke these
// directly to bring up the *main* engine for the top screen, since libnds only
// ships a one-call helper (`consoleDemoInit`) for the *sub* engine.
pub const REG_DISPCNT: *mut u32 = 0x0400_0000 as *mut u32;
pub const VRAM_A_CR: *mut u8 = 0x0400_0240 as *mut u8;
pub const MODE_0_2D: u32 = 1 << 16; // DISPLAY_VIDEO_MODE(0) | DISPLAY_MODE_NORMAL
pub const VRAM_ENABLE: u8 = 1 << 7;
pub const VRAM_A_MAIN_BG: u8 = 1; // map VRAM bank A to main-engine BG memory

/// Opaque storage for a libnds `PrintConsole`. The real struct is ~70 bytes;
/// we over-allocate (and over-align) so libnds can initialise it in place.
#[repr(C, align(8))]
pub struct PrintConsole {
    _opaque: [u8; 256],
}

impl PrintConsole {
    /// Zeroed storage suitable for handing to [`consoleInit`].
    pub const fn zeroed() -> Self {
        Self { _opaque: [0; 256] }
    }
}

unsafe extern "C" {
    /// Initialise a simple text console on the sub (bottom) screen and select
    /// it. Returns a pointer to the default console it set up.
    pub fn consoleDemoInit() -> *mut PrintConsole;
    /// Initialise a text console on the given background of the main or sub
    /// engine. `main_display` selects the engine; `load_graphics` loads the
    /// default font.
    pub fn consoleInit(
        console: *mut PrintConsole,
        layer: c_int,
        bg_type: c_int,
        bg_size: c_int,
        map_base: c_int,
        tile_base: c_int,
        main_display: bool,
        load_graphics: bool,
    ) -> *mut PrintConsole;
    /// Make `console` the target of subsequent console output (`printf`, etc.).
    pub fn consoleSelect(console: *mut PrintConsole) -> *mut PrintConsole;
    /// Clear the active console.
    pub fn consoleClear();
    /// printf to the active console (libnds redirects stdout to the console).
    pub fn printf(fmt: *const c_char, ...) -> c_int;
    /// Block until the next vertical blank (~60 Hz), pacing the game loop.
    pub fn swiWaitForVBlank();
    /// Latch the current button state; call once per frame before reading keys.
    pub fn scanKeys();
    /// Buttons currently held down (bitfield of `KEY_*`).
    pub fn keysHeld() -> u32;
    /// Start a free-running hardware timer (uses timers `timer` and `timer+1`
    /// as a 32-bit cascade) counting at the bus clock.
    pub fn cpuStartTiming(timer: c_int);
    /// Bus-clock ticks elapsed since [`cpuStartTiming`].
    pub fn cpuGetTiming() -> u32;
}

unsafe extern "C" {
    /// newlib aligned allocation, backing our global allocator.
    pub fn memalign(align: usize, size: usize) -> *mut c_void;
    /// newlib free.
    pub fn free(ptr: *mut c_void);
}
