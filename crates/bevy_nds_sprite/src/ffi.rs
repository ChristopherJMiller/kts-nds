//! Hand-written FFI to libnds's OAM ("sprite") API, in the same style as the
//! rest of the workspace: no bindgen, minimal surface, symbols resolved
//! against `libnds9.a` at final link. Cited against `<nds/arm9/sprite.h>` and
//! `<nds/arm9/video.h>`.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use core::ffi::{c_int, c_void};
use core::ptr::write_volatile;

// --- VRAM bank control (see <nds/arm9/video.h>) ------------------------------

/// VRAM bank I control register (8-bit).
pub const VRAM_I_CR: *mut u8 = 0x0400_0249 as *mut u8;
/// `VRAM_ENABLE = BIT(7)`.
pub const VRAM_ENABLE: u8 = 1 << 7;
/// Map VRAM bank I (16 KiB) to the sub engine's sprite VRAM
/// (`VRAM_I_SUB_SPRITE = 3`). See the libnds `VRAM_I_CR_*` constants.
pub const VRAM_I_SUB_SPRITE: u8 = 3;

/// Sub-engine sprite palette (256 × u16, fixed VRAM address). See libnds
/// `SPRITE_PALETTE_SUB`.
pub const SPRITE_PALETTE_SUB: *mut u16 = 0x0500_0600 as *mut u16;
/// Main-engine sprite palette (256 × u16). See libnds `SPRITE_PALETTE`.
pub const SPRITE_PALETTE: *mut u16 = 0x0500_0200 as *mut u16;

// --- Sprite-attribute encodings (see <nds/arm9/sprite.h>) --------------------

/// `SpriteSize` discriminants (packed: width/height + size class for the
/// tile-VRAM allocator). Only the variants we use are declared.
pub mod sprite_size {
    use core::ffi::c_int;

    // Each discriminant is `(SIZE_CODE << 14) | (SHAPE_CODE << 12) | TILE_COUNT`
    // (see `SpriteSize_*` in <nds/arm9/sprite.h>).
    /// 8x8 sprite (1 tile).
    pub const _8X8: c_int = (0 << 14) | (0 << 12) | ((8 * 8) >> 5);
    /// 16x16 sprite (4 tiles).
    pub const _16X16: c_int = (1 << 14) | (0 << 12) | ((16 * 16) >> 5);
    /// 32x32 sprite (16 tiles).
    pub const _32X32: c_int = (2 << 14) | (0 << 12) | ((32 * 32) >> 5);
    /// 64x64 sprite (64 tiles).
    pub const _64X64: c_int = (3 << 14) | (0 << 12) | ((64 * 64) >> 5);
}

/// `SpriteColorFormat` discriminants.
pub mod sprite_color_format {
    use core::ffi::c_int;
    /// 16 colours per palette, 4bpp tiles.
    pub const _16COLOR: c_int = 2;
    /// 256 colours, 8bpp tiles.
    pub const _256COLOR: c_int = 1;
}

/// `SpriteMapping_1D_32`: 1D tile mapping with 32-byte stride (smallest /
/// densest packing, what `grit` produces by default).
pub const SPRITE_MAPPING_1D_32: c_int = 0x0010_0000 | 0;

/// Opaque OAM state — libnds defines this as `OamState`. We only ever hand
/// the address of `oamSub` / `oamMain` to libnds; we never inspect the struct.
#[repr(C)]
pub struct OamState {
    _opaque: [u8; 1],
}

unsafe extern "C" {
    /// `oamSub` / `oamMain`: the two libnds OAM-state singletons (sub and main
    /// engine). We pass their addresses to every OAM call.
    pub static mut oamSub: OamState;
    pub static mut oamMain: OamState;

    /// `oamInit(oam, mapping, extPalette)` — set up the OAM shadow buffer and
    /// configure the engine's sprite mapping mode. Must run before any other
    /// `oam*` call against `oam`. See `<nds/arm9/sprite.h>`.
    pub fn oamInit(oam: *mut OamState, mapping: c_int, extPalette: bool);

    /// `oamAllocateGfx(oam, size, format)` returns a tile-VRAM offset (typed
    /// as `u16*`) sized for the given sprite. The bytes at that offset will
    /// be DMA'd into the sprite engine on the next `oamUpdate`.
    pub fn oamAllocateGfx(oam: *mut OamState, size: c_int, format: c_int) -> *mut u16;

    /// Free a previously [`oamAllocateGfx`]-ed buffer.
    pub fn oamFreeGfx(oam: *mut OamState, gfx: *const c_void);

    /// `oamSet(...)` — configure OAM entry `id` for the next frame. Lots of
    /// parameters; see `<nds/arm9/sprite.h>`. The values are written into the
    /// OAM *shadow* buffer; `oamUpdate` flushes it to hardware.
    pub fn oamSet(
        oam: *mut OamState,
        id: c_int,
        x: c_int,
        y: c_int,
        priority: c_int,
        palette_alpha: c_int,
        size: c_int,
        format: c_int,
        gfx_offset: *const c_void,
        affine_index: c_int,
        size_double: bool,
        hide: bool,
        hflip: bool,
        vflip: bool,
        mosaic: bool,
    );

    /// Flush the OAM shadow buffer to the real OAM. Call once per frame after
    /// updating all sprites.
    pub fn oamUpdate(oam: *mut OamState);
}

/// Convenience: enable VRAM bank I and route it to the sub engine's sprite
/// VRAM. Idempotent and only writes one byte.
///
/// # Safety
/// Touches MMIO; must run on the DS.
pub unsafe fn map_vram_i_to_sub_sprite() {
    unsafe { write_volatile(VRAM_I_CR, VRAM_ENABLE | VRAM_I_SUB_SPRITE) };
}
