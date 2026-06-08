//! `bevy_nds_sprite` — 2D hardware sprites (OAM) for [`bevy_nds`].
//!
//! The DS has a dedicated **OAM** ("sprite") engine on each 2D engine — up to
//! 128 movable image objects per screen, drawn *over* the BG layers (text /
//! tile-map). This crate wraps the libnds OAM API as ordinary Bevy components
//! and a [`SpritePlugin`].
//!
//! ## Scope (MVP)
//!
//! - One pre-loaded 16x16 4bpp sprite image and a 16-colour palette, both
//!   embedded in this crate as bytes. Replace with a `grit`-baked asset once
//!   the `png2sprite` host crate lands.
//! - Up to 128 [`Sprite`] entities on the **sub engine** (typically the screen
//!   that *isn't* showing the 3D output, i.e. the top LCD when `Display3d` is
//!   `Bottom`).
//! - Each frame, the live [`Sprite`] entities are flushed into OAM slots
//!   0..n; a small high-water watermark tells the plugin which trailing slots
//!   to hide when sprite count drops.
//!
//! ```ignore
//! app.add_plugins(SpritePlugin);
//! commands.spawn(Sprite::at(120, 90));
//! ```
//!
//! ## Engine ownership
//!
//! `SpritePlugin` only owns the **sub engine** OAM today (the engine the text
//! console runs on). When the 3D backend swaps screens via `Display3d`, the
//! sub engine moves between LCDs together with the text console, so sprites
//! and text always land on the same physical screen. A future revision will
//! let games opt into main-engine OAM too.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use core::ffi::c_void;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

mod ffi;
mod slots;

pub use slots::{MAX_SPRITES, SpriteSlots};

/// One on-screen hardware sprite. Position is in pixels (top-left corner;
/// 0..=255 horizontally, 0..=191 vertically — values outside this range are
/// clipped by the hardware). All other OAM attributes are fixed at the MVP
/// defaults (size 16x16, 4bpp 16-colour, palette 0, priority 0, no flip /
/// affine / mosaic).
#[derive(Component, Clone, Copy, Debug)]
pub struct Sprite {
    pub x: i16,
    pub y: i16,
}

impl Sprite {
    pub const fn at(x: i16, y: i16) -> Self {
        Self { x, y }
    }
}

/// Internal: the embedded sprite gfx, allocated once in [`init_sprite_engine`].
#[derive(Resource, Clone, Copy)]
struct SpriteGfx {
    gfx_ptr: *mut u16,
}

// SAFETY: the DS is single-core; we only touch the pointer from systems.
unsafe impl Send for SpriteGfx {}
unsafe impl Sync for SpriteGfx {}

/// Internal: the highest OAM slot id we wrote to last frame, so the next
/// frame can hide any trailing slots that no longer have a live sprite.
#[derive(Resource, Default, Clone, Copy)]
struct OamHighWater(u8);

/// Drives the DS sub-engine OAM each frame from [`Sprite`] entities. Add it
/// alongside `DsPlugins`.
pub struct SpritePlugin;

impl Plugin for SpritePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OamHighWater>()
            .add_systems(Startup, init_sprite_engine)
            .add_systems(Last, (submit_sprites, finalize_oam).chain());
    }
}

/// One-time sub-engine OAM bring-up: map VRAM_I to sub sprite memory, load
/// the embedded palette, allocate gfx VRAM for the one sprite image we ship
/// today, and copy the tile bytes into it. Runs in [`Startup`], after
/// `bevy_nds`'s `PreStartup` video bring-up has set up the consoles.
fn init_sprite_engine(mut commands: Commands) {
    let (gfx_bytes, palette) = embedded::cursor();

    let gfx_ptr = unsafe {
        ffi::map_vram_i_to_sub_sprite();
        ffi::oamInit(
            &raw mut ffi::oamSub,
            ffi::SPRITE_MAPPING_1D_32,
            false, // extPalette
        );

        // Load the 16-colour palette into sub-engine sprite palette bank 0.
        let pal = ffi::SPRITE_PALETTE_SUB;
        for (i, &entry) in palette.iter().enumerate() {
            core::ptr::write_volatile(pal.add(i), entry);
        }

        // Allocate VRAM for one 16x16 4bpp sprite (4 tiles × 32 bytes = 128 B).
        let gfx = ffi::oamAllocateGfx(
            &raw mut ffi::oamSub,
            ffi::sprite_size::_16X16,
            ffi::sprite_color_format::_16COLOR,
        );

        // Copy tile bytes into the allocated VRAM. `gfx` is `u16*` (VRAM is
        // 16-bit-aligned), so pair adjacent bytes into one halfword write.
        let dst = gfx;
        let halfwords = gfx_bytes.len() / 2;
        for i in 0..halfwords {
            let lo = gfx_bytes[i * 2] as u16;
            let hi = gfx_bytes[i * 2 + 1] as u16;
            core::ptr::write_volatile(dst.add(i), lo | (hi << 8));
        }
        gfx
    };

    commands.insert_resource(SpriteGfx { gfx_ptr });
}

/// Push every live [`Sprite`] into the next OAM slot. After all sprites are
/// written, hide any slots up to last frame's high-water mark. Cheap on the
/// 33 MHz ARM9: position + gfx pointer per sprite, no per-frame allocation.
fn submit_sprites(
    gfx: Res<SpriteGfx>,
    sprites: Query<&Sprite>,
    mut high_water: ResMut<OamHighWater>,
) {
    let mut next: u8 = 0;
    for sprite in &sprites {
        if (next as usize) >= MAX_SPRITES {
            break;
        }
        unsafe {
            ffi::oamSet(
                &raw mut ffi::oamSub,
                next as i32,
                sprite.x as i32,
                sprite.y as i32,
                0, // priority (0 = top)
                0, // palette index
                ffi::sprite_size::_16X16,
                ffi::sprite_color_format::_16COLOR,
                gfx.gfx_ptr as *const c_void,
                -1,    // affine_index (none)
                false, // size_double
                false, // hide
                false, // hflip
                false, // vflip
                false, // mosaic
            );
        }
        next += 1;
    }
    // Hide slots that were live last frame but are vacant this frame.
    let mut hide = next;
    while hide < high_water.0 {
        unsafe {
            ffi::oamSet(
                &raw mut ffi::oamSub,
                hide as i32,
                0,
                0,
                0,
                0,
                ffi::sprite_size::_16X16,
                ffi::sprite_color_format::_16COLOR,
                core::ptr::null(),
                -1,
                false,
                true, // hide
                false,
                false,
                false,
            );
        }
        hide += 1;
    }
    high_water.0 = next;
}

/// Flush the OAM shadow buffer to hardware. Must run after [`submit_sprites`].
fn finalize_oam() {
    unsafe { ffi::oamUpdate(&raw mut ffi::oamSub) };
}

/// Common imports for games using the sprite backend.
pub mod prelude {
    pub use crate::{Sprite, SpritePlugin};
}

/// Embedded MVP sprite asset: a 16x16 square with a red border on yellow
/// fill, 4bpp + 16-colour palette. Replace with a `grit`-baked NitroFS asset
/// once `png2sprite` lands.
mod embedded {
    /// Palette: 16 RGB15 entries (libnds `RGB15(r, g, b)` layout). Colour 0
    /// is transparent (the sprite engine treats palette index 0 as
    /// see-through). Colour 1 is bright yellow, colour 2 is red.
    const PALETTE: [u16; 16] = [
        0,      // 0: transparent
        0x03FF, // 1: yellow (R=31, G=31, B=0)
        0x001F, // 2: red    (R=31, G=0,  B=0)
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    /// 16x16 sprite encoded as 4 tiles (top-left, top-right, bottom-left,
    /// bottom-right) in 1D-32 tile order, each tile being 8x8 px at 4bpp
    /// (= 32 bytes). Each *byte* holds two 4-bit pixels, low nibble = left.
    ///
    /// Yellow fill (palette 1) bordered by red (palette 2) — visible against
    /// the dark blue 3D background.
    pub fn cursor() -> (&'static [u8], &'static [u16; 16]) {
        // For a row of 8 pixels at 4bpp, the byte order is:
        //   byte 0 = pixel 1 (high nibble) | pixel 0 (low nibble)
        //   byte 1 = pixel 3 | pixel 2
        //   byte 2 = pixel 5 | pixel 4
        //   byte 3 = pixel 7 | pixel 6
        const TILE_TL: [u8; 32] = [
            // row 0 (top sprite edge): all border (2,2,2,2,2,2,2,2)
            0x22, 0x22, 0x22, 0x22,
            // rows 1..7: left col is border, rest fill (2,1,1,1,1,1,1,1)
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
        ];
        const TILE_TR: [u8; 32] = [
            // row 0: all border
            0x22, 0x22, 0x22, 0x22,
            // rows 1..7: fill with right col border (1,1,1,1,1,1,1,2)
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
        ];
        const TILE_BL: [u8; 32] = [
            // rows 0..6: left col border, rest fill
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            0x12, 0x11, 0x11, 0x11,
            // row 7 (bottom sprite edge): all border
            0x22, 0x22, 0x22, 0x22,
        ];
        const TILE_BR: [u8; 32] = [
            // rows 0..6: fill with right col border
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            0x11, 0x11, 0x11, 0x21,
            // row 7: all border
            0x22, 0x22, 0x22, 0x22,
        ];
        // Concatenate the 4 tiles into the 1D-32 byte order libnds expects.
        static GFX: [u8; 128] = {
            let mut out = [0u8; 128];
            let mut i = 0;
            while i < 32 {
                out[i] = TILE_TL[i];
                out[32 + i] = TILE_TR[i];
                out[64 + i] = TILE_BL[i];
                out[96 + i] = TILE_BR[i];
                i += 1;
            }
            out
        };
        (&GFX, &PALETTE)
    }
}
