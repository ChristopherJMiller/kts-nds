//! `bevy_nds_sprite` — 2D hardware sprites (OAM) for [`bevy_nds`].
//!
//! The DS has a dedicated **OAM** ("sprite") engine on each 2D engine — up to
//! 128 movable image objects per screen, drawn *over* the BG layers (text /
//! tile-map). This crate wraps the libnds OAM API as ordinary Bevy components
//! and a [`SpritePlugin`].
//!
//! ## Asset model
//!
//! Sprites are baked from `assets/sprites/**/*.png` into `.sprite` blobs under
//! `nitro:/sprites/` by the host-side `png2sprite` crate. `build.rs` also
//! generates a Rust constants module (`sprites::*`) of NitroFS paths the game
//! `include!`s, so spawning a sprite looks like:
//!
//! ```ignore
//! commands.spawn(Sprite { image: sprites::CURSOR, x: 16, y: 8 });
//! ```
//!
//! At runtime the plugin maintains a [`SpriteAssets`] registry, capped at 16
//! distinct images (one per sub-engine sprite palette bank). The first time
//! any [`Sprite`] is seen carrying a given `image` path, the plugin reads the
//! `.sprite` file from NitroFS, claims a palette bank, allocates the matching
//! gfx VRAM, and caches the result. Subsequent sprites that reuse the same
//! `image` share the cached entry — at no extra VRAM cost.
//!
//! Supported sizes are the square OAM sizes: **8×8, 16×16, 32×32, 64×64**.
//! Rectangular sizes (8×16, 16×8, …) are rejected at load time.
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

use alloc::vec::Vec;
use core::ffi::{c_int, c_void};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

mod asset;
mod ffi;
mod slots;

pub use slots::{MAX_SPRITES, SpriteSlots};

/// Maximum number of distinct sprite images loaded simultaneously. Bounded by
/// the 16 4bpp palette banks on the sub-engine sprite palette — each loaded
/// image claims its own bank.
pub const MAX_SPRITE_IMAGES: usize = 16;

/// One on-screen hardware sprite. Position is in pixels (top-left corner;
/// 0..=255 horizontally, 0..=191 vertically — values outside this range are
/// clipped by the hardware). `image` is a NUL-terminated NitroFS path —
/// typically one of the `sprites::*` constants generated from
/// `assets/sprites/**/*.png` at build time.
#[derive(Component, Clone, Copy, Debug)]
pub struct Sprite {
    pub x: i16,
    pub y: i16,
    /// NUL-terminated NitroFS path to the baked `.sprite` asset. Use the
    /// constants in the game's generated `sprites` module.
    pub image: &'static [u8],
}

impl Sprite {
    /// Convenience constructor — `Sprite::new(sprites::CURSOR).at(16, 8)`.
    pub const fn new(image: &'static [u8]) -> Self {
        Self { x: 0, y: 0, image }
    }

    /// Builder-style: set the screen position in pixels.
    pub const fn at(mut self, x: i16, y: i16) -> Self {
        self.x = x;
        self.y = y;
        self
    }
}

/// Registry of sprite images that have been loaded into hardware VRAM and
/// palette banks. Populated lazily as new [`Sprite`] entities are seen.
///
/// The registry is keyed on the **pointer identity** of `Sprite.image`: two
/// sprites referencing the same `&'static [u8]` constant share an entry, but
/// two constants with the same bytes do not. This works because the build
/// pipeline emits one `&'static [u8]` per asset.
#[derive(Resource, Default)]
pub struct SpriteAssets {
    entries: Vec<SpriteEntry>,
}

impl SpriteAssets {
    /// Look up an already-loaded entry by image path.
    fn find(&self, image: &[u8]) -> Option<&SpriteEntry> {
        let key_ptr = image.as_ptr();
        let key_len = image.len();
        self.entries
            .iter()
            .find(|e| e.path_ptr == key_ptr && e.path_len == key_len)
    }

    /// How many distinct images are currently loaded.
    pub fn loaded_count(&self) -> usize {
        self.entries.len()
    }
}

/// One slot in [`SpriteAssets`]: the hardware resources backing a single
/// loaded sprite image.
struct SpriteEntry {
    /// Identity of the source path (matched by pointer + length).
    path_ptr: *const u8,
    path_len: usize,
    /// VRAM offset returned by `oamAllocateGfx`.
    gfx_ptr: *mut u16,
    /// Palette bank (0..15) this image's 16-colour palette lives in.
    palette_bank: u8,
    /// OAM size code (`ffi::sprite_size::_*`).
    size_code: c_int,
}

// SAFETY: the DS is single-core; the raw pointers in SpriteEntry only ever
// point into MMIO/VRAM, never into user-managed memory, and we only touch
// them from systems running on the ARM9.
unsafe impl Send for SpriteEntry {}
unsafe impl Sync for SpriteEntry {}

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
            .init_resource::<SpriteAssets>()
            .add_systems(Startup, init_sprite_engine)
            .add_systems(PreUpdate, ensure_sprites_loaded)
            .add_systems(Last, (submit_sprites, finalize_oam).chain());
    }
}

/// One-time sub-engine OAM bring-up: map VRAM_I to sub sprite memory and
/// initialise the OAM shadow buffer. No asset loading — that is lazy and
/// keyed off each [`Sprite`]'s `image` path.
fn init_sprite_engine() {
    unsafe {
        ffi::map_vram_i_to_sub_sprite();
        ffi::oamInit(
            &raw mut ffi::oamSub,
            ffi::SPRITE_MAPPING_1D_32,
            false, // extPalette
        );
    }
}

/// PreUpdate: load any sprite image we haven't seen yet. Linear scan over all
/// live [`Sprite`] entities; with up to 128 sprites and ≤16 cached entries,
/// each frame this is a few thousand pointer comparisons (cheap on the ARM9
/// even without the cache).
fn ensure_sprites_loaded(mut assets: ResMut<SpriteAssets>, sprites: Query<&Sprite>) {
    for sprite in &sprites {
        if assets.find(sprite.image).is_some() {
            continue;
        }
        if assets.entries.len() >= MAX_SPRITE_IMAGES {
            // No palette banks left — skip silently. The sprite will simply
            // not render until another image is dropped (not yet supported).
            continue;
        }
        let Some(entry) = load_into_hardware(sprite.image, assets.entries.len() as u8) else {
            // Asset missing/invalid/unsupported size. The sprite using it
            // will be hidden; retrying every frame keeps the cost trivial.
            continue;
        };
        assets.entries.push(entry);
    }
}

/// Read a `.sprite` blob from NitroFS, allocate gfx VRAM, copy the palette
/// into the assigned bank, and copy the tile bytes into VRAM. Returns the
/// completed [`SpriteEntry`], or `None` if anything went wrong.
fn load_into_hardware(image: &'static [u8], palette_bank: u8) -> Option<SpriteEntry> {
    let loaded = asset::load(image)?;
    let size_code = size_code_for(loaded.width, loaded.height)?;

    let gfx_ptr = unsafe {
        let gfx = ffi::oamAllocateGfx(
            &raw mut ffi::oamSub,
            size_code,
            ffi::sprite_color_format::_16COLOR,
        );
        if gfx.is_null() {
            return None;
        }
        // Tile bytes are paired into 16-bit writes (VRAM is halfword-aligned).
        let halfwords = loaded.gfx.len() / 2;
        for i in 0..halfwords {
            let lo = loaded.gfx[i * 2] as u16;
            let hi = loaded.gfx[i * 2 + 1] as u16;
            core::ptr::write_volatile(gfx.add(i), lo | (hi << 8));
        }
        gfx
    };

    // Each loaded image owns one 16-entry palette bank on the sub engine.
    // Write up to 16 entries; ignore anything grit emitted past that (it
    // shouldn't, for 4bpp, but we don't want to spill into adjacent banks).
    unsafe {
        let base = ffi::SPRITE_PALETTE_SUB.add(palette_bank as usize * 16);
        let n = loaded.palette.len().min(16);
        for i in 0..n {
            core::ptr::write_volatile(base.add(i), loaded.palette[i]);
        }
    }

    Some(SpriteEntry {
        path_ptr: image.as_ptr(),
        path_len: image.len(),
        gfx_ptr,
        palette_bank,
        size_code,
    })
}

/// Map (width, height) → OAM size code. Only the square sizes are supported
/// for now; everything else returns `None` and the sprite is hidden.
fn size_code_for(width: u16, height: u16) -> Option<c_int> {
    match (width, height) {
        (8, 8) => Some(ffi::sprite_size::_8X8),
        (16, 16) => Some(ffi::sprite_size::_16X16),
        (32, 32) => Some(ffi::sprite_size::_32X32),
        (64, 64) => Some(ffi::sprite_size::_64X64),
        _ => None,
    }
}

/// Push every live [`Sprite`] into the next OAM slot. After all sprites are
/// written, hide any slots up to last frame's high-water mark. Cheap on the
/// 33 MHz ARM9: position + entry lookup per sprite, no per-frame allocation.
fn submit_sprites(
    assets: Res<SpriteAssets>,
    sprites: Query<&Sprite>,
    mut high_water: ResMut<OamHighWater>,
) {
    let mut next: u8 = 0;
    for sprite in &sprites {
        if (next as usize) >= MAX_SPRITES {
            break;
        }
        // Sprites whose image hasn't loaded (yet) are silently skipped.
        let Some(entry) = assets.find(sprite.image) else {
            continue;
        };
        unsafe {
            ffi::oamSet(
                &raw mut ffi::oamSub,
                next as i32,
                sprite.x as i32,
                sprite.y as i32,
                0, // priority (0 = top)
                entry.palette_bank as i32,
                entry.size_code,
                ffi::sprite_color_format::_16COLOR,
                entry.gfx_ptr as *const c_void,
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
    // Hide slots that were live last frame but are vacant this frame. We pick
    // an arbitrary size — the entry is hidden, so the parameters don't render.
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
    pub use crate::{Sprite, SpriteAssets, SpritePlugin};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_code_for_recognises_square_sizes() {
        assert!(size_code_for(8, 8).is_some());
        assert!(size_code_for(16, 16).is_some());
        assert!(size_code_for(32, 32).is_some());
        assert!(size_code_for(64, 64).is_some());
    }

    #[test]
    fn size_code_for_rejects_rectangles_and_unknowns() {
        assert!(size_code_for(16, 8).is_none());
        assert!(size_code_for(24, 24).is_none());
        assert!(size_code_for(0, 0).is_none());
    }
}
