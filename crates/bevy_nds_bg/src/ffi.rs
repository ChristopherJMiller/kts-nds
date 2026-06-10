//! Hand-written FFI to libnds's background API plus the VRAM bank-B control
//! we use to give the bitmap BG its own 128 KiB region. Cited against
//! `<nds/arm9/background.h>` and `<nds/arm9/video.h>`.
//!
//! Style matches the rest of the workspace: no bindgen, minimal surface,
//! symbols resolved against `libnds9.a` at the demo's final link.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use core::ffi::c_int;
use core::ptr::write_volatile;

// --- BgType / BgSize enum values (see <nds/arm9/background.h>) ---------------

/// `BgType_Text4bpp`: 4bpp tiled background, 16-bit map entries (tile index +
/// flip + palette bank). What our `.bg` tile assets target.
pub const BG_TYPE_TEXT_4BPP: c_int = 1;
/// `BgType_Bmp16`: extended-mode 16bpp direct-color bitmap, RGB15 + alpha bit.
pub const BG_TYPE_BMP16: c_int = 5;

/// `BgSize_T_256x256` (text mode, one screen-fill).
#[allow(clippy::identity_op)]
pub const BG_SIZE_T_256X256: c_int = (0 << 14) | (1 << 16);

/// `BgSize_B16_256x256` (16-bit bitmap, 256x256 = 128 KiB).
pub const BG_SIZE_B16_256X256: c_int = (1 << 14) | (1 << 7) | (1 << 2) | (4 << 16);

// --- BG palette addresses (see <nds/arm9/background.h>) ----------------------

/// Main-engine BG palette (256 × u16 at `0x05000000`).
pub const BG_PALETTE: *mut u16 = 0x0500_0000 as *mut u16;
/// Sub-engine BG palette (256 × u16 at `0x05000400`).
pub const BG_PALETTE_SUB: *mut u16 = 0x0500_0400 as *mut u16;

// --- Display + VRAM control (see <nds/arm9/video.h>) -------------------------

/// `REG_DISPCNT`: main-engine display control register.
pub const REG_DISPCNT: *mut u32 = 0x0400_0000 as *mut u32;
/// `REG_DISPCNT_SUB`: sub-engine display control register.
pub const REG_DISPCNT_SUB: *mut u32 = 0x0400_1000 as *mut u32;
/// `DISPLAY_BG0_ACTIVE = 1 << 8`: bit position of the BG0-enabled bit in
/// `REG_DISPCNT(_SUB)`. BG1/2/3 are the next three bits up.
pub const DISPLAY_BG_BIT_SHIFT: u32 = 8;
/// MODE 5 selector for the `videoSetMode(MODE_5_2D)` value: video mode 5 +
/// `DISPLAY_MODE_NORMAL` (the `DISPLAY_BG*_ACTIVE` bits get OR'd in by
/// libnds's `bgInit` calls when needed).
pub const MODE_5_2D: u32 = 5 | (1 << 16);

/// `VRAM_B_CR` (8-bit).
pub const VRAM_B_CR: *mut u8 = 0x0400_0241 as *mut u8;
/// `VRAM_ENABLE = BIT(7)`.
pub const VRAM_ENABLE: u8 = 1 << 7;
/// `VRAM_B_MAIN_BG_0x06020000`: route VRAM bank B (128 KiB) to main-engine BG
/// memory starting at offset 0x06020000 — i.e. immediately after VRAM_A. This
/// is where our bitmap BG's 128 KiB framebuffer lives.
pub const VRAM_B_MAIN_BG_0X06020000: u8 = 1 | (1 << 3);

// --- libnds BG API -----------------------------------------------------------

unsafe extern "C" {
    /// `bgInitHidden(layer, type, size, mapBase, tileBase)` — main engine.
    /// Returns a small integer "bg id" the other `bg*` calls take.
    /// "Hidden" means the layer's `DISPLAY_BG*_ACTIVE` bit is left off until
    /// [`bgShow`] is called, so we can populate VRAM without flicker.
    pub fn bgInitHidden(
        layer: c_int,
        ty: c_int,
        size: c_int,
        mapBase: c_int,
        tileBase: c_int,
    ) -> c_int;
    /// Same as [`bgInitHidden`] but for the sub engine (bottom screen).
    pub fn bgInitHiddenSub(
        layer: c_int,
        ty: c_int,
        size: c_int,
        mapBase: c_int,
        tileBase: c_int,
    ) -> c_int;

    /// `bgGetGfxPtr(id)` — VRAM pointer to the layer's tile gfx or bitmap.
    pub fn bgGetGfxPtr(id: c_int) -> *mut u16;
    /// `bgGetMapPtr(id)` — VRAM pointer to the layer's tilemap. Not used for
    /// bitmap BGs (they have no map).
    pub fn bgGetMapPtr(id: c_int) -> *mut u16;

    /// `bgSetScrollf(id, x, y)` — internally the scroll registers are 24.8
    /// fixed-point, so libnds's inline `bgSetScroll(id, x, y)` is just this
    /// with `(x << 8, y << 8)`. We do the shift on the Rust side.
    pub fn bgSetScrollf(id: c_int, x: i32, y: i32);

    /// `bgUpdate()` — flush BG control + scroll shadow state to the hardware
    /// registers. Must run once per frame after any `bgSetScroll*` or
    /// [`show_bg`] / [`hide_bg`] call.
    pub fn bgUpdate();
}

/// libnds's `bgShow` is `static inline`, so it isn't a linkable symbol — we
/// inline its body here. Set the `DISPLAY_BG{layer}_ACTIVE` bit on the right
/// engine's `REG_DISPCNT(_SUB)`.
///
/// # Safety
/// Touches MMIO; must run on the DS.
pub unsafe fn show_bg(engine: BgEngine, layer: c_int) {
    let reg = match engine {
        BgEngine::Main => REG_DISPCNT,
        BgEngine::Sub => REG_DISPCNT_SUB,
    };
    unsafe {
        let cur = core::ptr::read_volatile(reg);
        write_volatile(reg, cur | (1u32 << (DISPLAY_BG_BIT_SHIFT + layer as u32)));
    }
}

/// libnds's `bgHide` is also `static inline`; inline its body here.
///
/// # Safety
/// Touches MMIO; must run on the DS.
pub unsafe fn hide_bg(engine: BgEngine, layer: c_int) {
    let reg = match engine {
        BgEngine::Main => REG_DISPCNT,
        BgEngine::Sub => REG_DISPCNT_SUB,
    };
    unsafe {
        let cur = core::ptr::read_volatile(reg);
        write_volatile(reg, cur & !(1u32 << (DISPLAY_BG_BIT_SHIFT + layer as u32)));
    }
}

/// Which 2D engine an MMIO write targets.
#[derive(Clone, Copy)]
pub enum BgEngine {
    Main,
    Sub,
}

/// Set the main-engine display mode to `MODE_5_2D` so the extended BG2/BG3
/// layers (where the 16bpp bitmap lives) are available. The existing video
/// crate set MODE_0; mode 5 still gives us BG0/BG1 as text (so the console
/// keeps working and the tile BG keeps fitting on BG1).
///
/// # Safety
/// Touches `REG_DISPCNT`; must run on the DS.
pub unsafe fn set_main_video_mode_5() {
    unsafe { write_volatile(REG_DISPCNT, MODE_5_2D) };
}

/// Map VRAM bank B (128 KiB) to the main engine's BG memory at 0x06020000,
/// so the 16bpp bitmap BG has a dedicated bank that doesn't collide with
/// VRAM_A (which the console + tile BG share).
///
/// # Safety
/// Touches MMIO; must run on the DS.
pub unsafe fn map_vram_b_to_main_bg_slot1() {
    unsafe { write_volatile(VRAM_B_CR, VRAM_ENABLE | VRAM_B_MAIN_BG_0X06020000) };
}
