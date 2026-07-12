//! `bevy_nds_bg` — 2D background layers (BG) for [`bevy_nds`].
//!
//! The DS 2D engine has four background "layers" per screen (BG0..BG3) plus
//! the OAM ("sprite") plane on top. `bevy_nds_video` already owns **BG0** on
//! both engines: it's the text console that `bevy_nds_text` draws into. This
//! crate owns the other layers:
//!
//! - **BG1**: 4bpp **tile** background, 32×32 tilemap = 256×256 pixels =
//!   exactly one screen-fill. Hardware-scrollable.
//! - **BG3**: 16bpp **bitmap** background (extended-mode `BgType_Bmp16`),
//!   256×256 pixels of RGB15 + alpha-bit. Main engine only — a full 256×256
//!   framebuffer is 128 KiB and the sub engine has no spare bank that size.
//! - **BG2 (sub only)**: the live 128×128 [`PaintCanvas`] (issue #35) — a
//!   32 KiB 16bpp bitmap that fits VRAM_C's free low region beside the text
//!   console, hardware-magnified 2× to fill the tactical screen. Opt-in via
//!   [`Backgrounds::enable_paint_canvas`]; repaint it each frame.
//!
//! Game code asks for backgrounds through the [`Backgrounds`] resource. It is
//! resource-shaped rather than component-shaped because backgrounds are fixed
//! hardware slots, not entities — there are at most four ([top, bottom] ×
//! [tile, bitmap]) and you don't spawn new ones at runtime.
//!
//! ## Assets and lazy loading
//!
//! Tile backgrounds are baked from `assets/backgrounds/tiled/**/*.png` into
//! `.bg` blobs (tileset + map + palette) by `png2bg`; bitmap backgrounds from
//! `assets/backgrounds/bitmap/**/*.png` into `.bbg` blobs (RGB15 pixels).
//! `build.rs` emits a Rust constants module (`backgrounds::tiled::*`,
//! `backgrounds::bitmap::*`) the game `include!`s, so paths aren't
//! stringly-typed:
//!
//! ```ignore
//! use bevy_nds::prelude::*;
//! commands.run_system_once(move |mut bgs: ResMut<Backgrounds>| {
//!     bgs.set_tile(DsScreen::Top, backgrounds::tiled::FOREST);
//!     bgs.set_bitmap(DsScreen::Top, backgrounds::bitmap::PHOTO);
//! });
//! ```
//!
//! The asset is loaded from NitroFS on the **next frame** — the resource
//! merely flags the slot as pending. This matches the sprite crate's lazy
//! behaviour and keeps the runtime free of long-running work during
//! game-state changes.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use core::ffi::c_int;
use core::ptr::write_volatile;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds_video::DsScreen;

mod asset;
mod ffi;

/// Internal: pick the libnds main-engine vs sub-engine `bgInit*` variant.
fn bg_init_hidden(
    screen: DsScreen,
    layer: c_int,
    ty: c_int,
    size: c_int,
    map_base: c_int,
    tile_base: c_int,
) -> c_int {
    unsafe {
        match screen {
            DsScreen::Top => ffi::bgInitHidden(layer, ty, size, map_base, tile_base),
            DsScreen::Bottom => ffi::bgInitHiddenSub(layer, ty, size, map_base, tile_base),
        }
    }
}

/// Map a `DsScreen` to the corresponding 2D engine for the MMIO helpers.
fn engine_for(screen: DsScreen) -> ffi::BgEngine {
    match screen {
        DsScreen::Top => ffi::BgEngine::Main,
        DsScreen::Bottom => ffi::BgEngine::Sub,
    }
}

/// One BG slot's state machine.
#[derive(Default)]
enum TileSlot {
    /// No background here.
    #[default]
    Empty,
    /// User has requested an image; not yet loaded into VRAM.
    Pending {
        image: &'static [u8],
        scroll: (i16, i16),
    },
    /// VRAM is populated and the layer is shown.
    Loaded {
        scroll: (i16, i16),
        /// libnds bg id; needed to drive `bgSetScrollf`.
        bg_id: c_int,
        /// Last scroll value flushed to hardware (so we only rewrite the
        /// scroll register on actual changes — cheap, but keeps the hot path
        /// MMIO-quiet).
        flushed_scroll: (i16, i16),
    },
}

#[derive(Default)]
enum BitmapSlot {
    #[default]
    Empty,
    Pending {
        image: &'static [u8],
    },
    Loaded,
}

#[derive(Default)]
struct EngineSlots {
    tile: TileSlot,
    bitmap: BitmapSlot,
}

/// Width/height of the sub-engine **paint canvas** bitmap ([`PaintCanvas`]) —
/// the live, per-frame-mutable framebuffer the game paints the capture stroke
/// into (issue #35). It is a 128×128 16bpp bitmap (32 KiB) that the hardware
/// magnifies 2× (see [`ffi::bgSetScale`]) to fill the 256×192 screen, so only
/// the top-left [`PAINT_VISIBLE_H`] rows are on-screen. Half touch-resolution;
/// the *capture math* stays full-resolution on the raw touch stream — the
/// canvas is purely visual.
const PAINT_W: usize = 128;
const PAINT_H: usize = 128;
/// The rows of the 128-tall canvas that are actually visible after the 2×
/// upscale onto the 192px-tall screen (192 / 2 = 96).
const PAINT_VISIBLE_H: usize = 96;

/// State of the sub-engine paint canvas. Opt-in (via
/// [`Backgrounds::enable_paint_canvas`]) so crate consumers that only want tile
/// backgrounds don't pay for a sub bitmap BG or the mode-5 switch.
#[derive(Default)]
enum PaintSlot {
    #[default]
    Empty,
    /// Requested; the plugin initialises the BG on the next frame.
    Pending,
    /// Live: `gfx` is the framebuffer's VRAM address (kept as `usize` so
    /// [`Backgrounds`] stays `Send`/`Sync` without a raw pointer field — the
    /// DS is single-core and reconstructs the slice on demand).
    Ready { gfx: usize },
}

/// Game-facing handle to the BG layers. Insert + populate from a system; the
/// plugin's per-frame work loads pending slots and syncs scroll.
///
/// All setters are cheap: they only mutate this resource, never touch
/// hardware. The plugin's PreUpdate system does the actual VRAM uploads, and
/// its Last system flushes scroll. This means the API stays fast to call
/// from Startup / event handlers / Update systems without surprising costs.
#[derive(Resource, Default)]
pub struct Backgrounds {
    main: EngineSlots,
    sub: EngineSlots,
    /// The sub-engine live paint canvas (issue #35); `Empty` unless a game opts
    /// in with [`enable_paint_canvas`](Self::enable_paint_canvas).
    paint: PaintSlot,
}

impl Backgrounds {
    /// Place a 4bpp tile background on BG1 of `screen`. The asset is loaded
    /// from NitroFS on the next frame; if the path is invalid, the load
    /// silently fails and the layer stays hidden. Replaces any previous tile
    /// background on this screen.
    pub fn set_tile(&mut self, screen: DsScreen, image: &'static [u8]) {
        let slots = self.slots_mut(screen);
        let scroll = match &slots.tile {
            TileSlot::Loaded { scroll, .. } | TileSlot::Pending { scroll, .. } => *scroll,
            TileSlot::Empty => (0, 0),
        };
        slots.tile = TileSlot::Pending { image, scroll };
    }

    /// Set the hardware scroll on the tile background of `screen` (no-op if
    /// no tile background is loaded / pending there). x and y wrap around the
    /// 256×256 tilemap.
    pub fn set_tile_scroll(&mut self, screen: DsScreen, x: i16, y: i16) {
        let slots = self.slots_mut(screen);
        match &mut slots.tile {
            TileSlot::Empty => {}
            TileSlot::Pending { scroll, .. } => *scroll = (x, y),
            TileSlot::Loaded { scroll, .. } => *scroll = (x, y),
        }
    }

    /// Remove the tile background from `screen`. Hides the layer on the next
    /// frame.
    pub fn clear_tile(&mut self, screen: DsScreen) {
        self.slots_mut(screen).tile = TileSlot::Empty;
    }

    /// Place a 16bpp direct-color bitmap background on BG3 of `screen`. Only
    /// the main engine ([`DsScreen::Top`]) is supported today; calls for the
    /// sub screen are silently ignored.
    pub fn set_bitmap(&mut self, screen: DsScreen, image: &'static [u8]) {
        // Sub engine has no spare VRAM bank for a 128 KiB bitmap, so the
        // bottom-screen bitmap path is a no-op rather than a panic. Games
        // that need it can plumb it later.
        if matches!(screen, DsScreen::Bottom) {
            return;
        }
        self.slots_mut(screen).bitmap = BitmapSlot::Pending { image };
    }

    /// Remove the bitmap background from `screen`.
    pub fn clear_bitmap(&mut self, screen: DsScreen) {
        self.slots_mut(screen).bitmap = BitmapSlot::Empty;
    }

    /// Opt into the sub-engine **paint canvas** (issue #35): a live 128×128
    /// 16bpp bitmap on the bottom screen's BG2, hardware-magnified 2× to fill
    /// the tactical surface, that the game repaints each frame (the capture
    /// stroke). Idempotent — a no-op once enabled. The BG is initialised on the
    /// next frame; poll [`paint`](Self::paint) for the drawable handle.
    ///
    /// It shares the sub engine with the text console (BG0, which stays a text
    /// layer under mode 5) and the sprite blips (OAM, always on top), and lives
    /// in VRAM_C's free region between the console's font tiles and map. Do not
    /// combine it with a sub-screen *tile* background — they'd contend for the
    /// same VRAM.
    pub fn enable_paint_canvas(&mut self) {
        if matches!(self.paint, PaintSlot::Empty) {
            self.paint = PaintSlot::Pending;
        }
    }

    /// Borrow the paint canvas for this frame, or `None` if it isn't enabled /
    /// initialised yet. Write to it with [`PaintCanvas::clear`] /
    /// [`PaintCanvas::plot`]; changes are visible immediately (the framebuffer
    /// is VRAM). See [`enable_paint_canvas`](Self::enable_paint_canvas).
    pub fn paint(&mut self) -> Option<PaintCanvas<'_>> {
        match self.paint {
            PaintSlot::Ready { gfx } => {
                // SAFETY: `gfx` is the BG's framebuffer base returned by
                // `bgGetGfxPtr` at init; it owns a contiguous PAINT_W×PAINT_H
                // u16 region of VRAM. The DS is single-core and nothing else
                // writes this BG, so a unique &mut slice for the frame is sound.
                let fb =
                    unsafe { core::slice::from_raw_parts_mut(gfx as *mut u16, PAINT_W * PAINT_H) };
                Some(PaintCanvas { fb })
            }
            _ => None,
        }
    }

    fn slots_mut(&mut self, screen: DsScreen) -> &mut EngineSlots {
        match screen {
            DsScreen::Top => &mut self.main,
            DsScreen::Bottom => &mut self.sub,
        }
    }

    #[cfg(test)]
    fn slots(&self, screen: DsScreen) -> &EngineSlots {
        match screen {
            DsScreen::Top => &self.main,
            DsScreen::Bottom => &self.sub,
        }
    }
}

/// A borrowed, writable view of the sub-engine paint canvas framebuffer for one
/// frame (issue #35). Obtained from [`Backgrounds::paint`]. Coordinates are in
/// **canvas space** (0..[`WIDTH`](Self::WIDTH), 0..[`HEIGHT`](Self::HEIGHT)) —
/// half touch-screen resolution; map a touch pixel to canvas space by halving.
/// Pixels are 16bpp `aBBBBBGGGGGRRRRR` (bit 15 = opaque); `0` is transparent.
///
/// The anti-aliased stroke's per-pixel coverage is baked into the *colour* the
/// caller passes (blend the line colour toward the backdrop by coverage), since
/// the hardware bitmap has only a 1-bit alpha — coverage-as-brightness is the
/// anti-aliasing.
pub struct PaintCanvas<'a> {
    fb: &'a mut [u16],
}

impl PaintCanvas<'_> {
    /// Canvas width in pixels (half the 256px screen width).
    pub const WIDTH: usize = PAINT_W;
    /// Canvas height in pixels (only [`VISIBLE_HEIGHT`](Self::VISIBLE_HEIGHT)
    /// rows are on-screen after the 2× upscale).
    pub const HEIGHT: usize = PAINT_H;
    /// On-screen rows after the 2× upscale onto the 192px-tall screen.
    pub const VISIBLE_HEIGHT: usize = PAINT_VISIBLE_H;

    /// Clear the whole canvas to transparent.
    pub fn clear(&mut self) {
        self.fb.fill(0);
    }

    /// Set the pixel at canvas `(x, y)` to `color`. Off-canvas coordinates are
    /// ignored (the rasterizer may emit brush pixels past the edge).
    pub fn plot(&mut self, x: i32, y: i32, color: u16) {
        if x >= 0 && (x as usize) < PAINT_W && y >= 0 && (y as usize) < PAINT_H {
            self.fb[y as usize * PAINT_W + x as usize] = color;
        }
    }

    /// Blit a coverage buffer (`WIDTH` × `VISIBLE_HEIGHT`, row-major, one byte
    /// per pixel) into the visible region of the canvas, mapping each coverage
    /// byte through a 256-entry colour `palette` (coverage → 16bpp colour). This
    /// writes **every** visible pixel, so it doubles as the clear — no separate
    /// [`clear`](Self::clear) needed.
    ///
    /// The fast path for painting a whole frame's stroke at once: the game
    /// rasterizes into a CPU coverage buffer (max-combining overlaps) and hands
    /// it here in one call. The palette is a plain lookup (no per-pixel closure
    /// crossing a crate boundary), so this whole loop stays in this opt-3 crate.
    /// Panics if `cov` is shorter than `WIDTH × VISIBLE_HEIGHT`.
    pub fn blit_coverage(&mut self, cov: &[u8], palette: &[u16; 256]) {
        self.blit_coverage_rect(cov, palette, 0, 0, PAINT_W, PAINT_VISIBLE_H);
    }

    /// Blit only the rectangle `[x0, x1) × [y0, y1)` of the coverage buffer (as
    /// [`blit_coverage`](Self::blit_coverage)). The fast path for **incremental**
    /// painting: after stamping just the newest stroke segment, push only its
    /// bounding box to VRAM — the whole-frame blit is too much when the game is
    /// already at the vblank budget. Bounds are clamped to the visible canvas.
    pub fn blit_coverage_rect(
        &mut self,
        cov: &[u8],
        palette: &[u16; 256],
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
    ) {
        let x1 = x1.min(PAINT_W);
        let y1 = y1.min(PAINT_VISIBLE_H);
        for y in y0..y1 {
            let row = y * PAINT_W;
            for x in x0..x1 {
                self.fb[row + x] = palette[cov[row + x] as usize];
            }
        }
    }
}

/// VRAM map_base / tile_base assignments. The console already owns part of
/// VRAM_A on the main engine and part of VRAM_C on the sub engine, so these
/// values are picked to avoid both regions. Units are 2 KiB for `map_base`
/// and 16 KiB for `tile_base`. See `<nds/arm9/background.h>` (`BG_TILE_RAM`,
/// `BG_MAP_RAM`).
///
/// Main BG0 console: tile_base=3 (48–64 KiB), map_base=22 (44–46 KiB).
/// Sub  BG0 console: tile_base=0 (0–16 KiB),  map_base=31 (62–64 KiB).
mod vram {
    use core::ffi::c_int;
    /// Main BG1 tile gfx slot: 0–16 KiB of VRAM_A, before the console's tiles.
    pub const MAIN_TILE_GFX: c_int = 0;
    /// Main BG1 tile map slot: 32–34 KiB, in the gap before the console map.
    pub const MAIN_TILE_MAP: c_int = 16;
    /// Sub BG1 tile gfx slot: 16–32 KiB of VRAM_C (sub console tiles in 0–16).
    pub const SUB_TILE_GFX: c_int = 1;
    /// Sub BG1 tile map slot: 32–34 KiB (sub console map at 62 KiB).
    pub const SUB_TILE_MAP: c_int = 16;
    /// Main BG3 bitmap base: VRAM_B mapped at 0x06020000 = 8 × 16 KiB into
    /// main BG memory.
    pub const MAIN_BITMAP_BASE: c_int = 8;
    /// Sub BG2 **paint-canvas** bitmap base: 0 × 16 KiB = 0–32 KiB into VRAM_C.
    /// The sub text console (`consoleDemoInit`) lives in VRAM_C's *upper* region
    /// — tilemap at map-base 22 (44 KiB), font tiles at tile-base 3 (48–64 KiB)
    /// — so the low 32 KiB is free for the 128×128 16bpp canvas (verified: a
    /// full-canvas clear each frame leaves the console text intact). Do **not**
    /// combine with a sub-screen tile background (it would contend here).
    pub const SUB_PAINT_BASE: c_int = 0;
}

/// Which palette bank our tile BG uses. Bank 0 is reserved for the text
/// console (libnds default font), so we shift to bank 1 to coexist.
const PALETTE_BANK_TILE: u8 = 1;

/// Plugin entry point.
///
/// Add it alongside `DsPlugins`. The plugin:
///
/// - Switches the main engine into MODE 5 so the extended BG3 (where the
///   bitmap lives) is available, and maps VRAM_B into main BG memory at
///   0x06020000 so the bitmap framebuffer has somewhere to sit.
/// - Inserts the [`Backgrounds`] resource.
/// - Each PreUpdate frame, loads any `Pending` slots into VRAM.
/// - Each Last frame, flushes scroll changes and calls `bgUpdate()` so the
///   layer-control + scroll registers latch this frame's state.
pub struct BackgroundPlugin;

impl Plugin for BackgroundPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Backgrounds>()
            .add_systems(PreStartup, init_video_mode_5)
            .add_systems(PreUpdate, ensure_loaded)
            .add_systems(Last, flush_scroll_and_update);
    }
}

/// PreStartup: bump video mode + map VRAM_B. The video crate's
/// `init_screens` already mapped VRAM_A and set MODE_0; we overwrite the mode
/// with MODE_5 (which still gives BG0/BG1 as text, so the console keeps
/// working) and additionally map VRAM_B at 0x06020000.
fn init_video_mode_5() {
    unsafe {
        ffi::set_main_video_mode_5();
        ffi::map_vram_b_to_main_bg_slot1();
    }
}

/// PreUpdate: process pending tile / bitmap slots on both engines.
fn ensure_loaded(mut bgs: ResMut<Backgrounds>) {
    for screen in [DsScreen::Top, DsScreen::Bottom] {
        process_tile_slot(&mut bgs, screen);
        process_bitmap_slot(&mut bgs, screen);
    }
    process_paint_slot(&mut bgs);
}

/// PreUpdate: bring up the sub-engine paint canvas if it was requested.
fn process_paint_slot(bgs: &mut Backgrounds) {
    if !matches!(bgs.paint, PaintSlot::Pending) {
        return;
    }
    bgs.paint = match init_paint_canvas() {
        Some(gfx) => PaintSlot::Ready { gfx },
        // Init can't really fail (no asset), but stay defensive.
        None => PaintSlot::Empty,
    };
}

/// Initialise the sub-engine BG2 paint canvas: switch the sub engine to mode 5
/// (preserving the console on BG0), init a 128×128 16bpp bitmap in VRAM_C's
/// free region, clear it, set the 2× top-left-anchored magnification, and show
/// it. Returns the framebuffer's VRAM address. The `Last` `bgUpdate()` latches
/// the show + affine matrix into hardware.
fn init_paint_canvas() -> Option<usize> {
    unsafe {
        ffi::set_sub_video_mode_5();
        let bg_id = ffi::bgInitHiddenSub(
            2, // BG2 (extended-rotation, so it can be magnified)
            ffi::BG_TYPE_BMP16,
            ffi::BG_SIZE_B16_128X128,
            vram::SUB_PAINT_BASE,
            0, // tileBase unused for bitmaps
        );
        let gfx = ffi::bgGetGfxPtr(bg_id);
        for i in 0..(PAINT_W * PAINT_H) {
            write_volatile(gfx.add(i), 0); // transparent
        }
        // 0.5 in 24.8 → magnify 2×; anchor + scroll at the origin so screen
        // (0,0) samples canvas (0,0). The visible 256×192 maps to canvas
        // 128×96 (top-left).
        ffi::bgSetScale(bg_id, 128, 128);
        ffi::bgSetCenterf(bg_id, 0, 0);
        ffi::bgSetScrollf(bg_id, 0, 0);
        ffi::show_bg(ffi::BgEngine::Sub, 2);
        Some(gfx as usize)
    }
}

fn process_tile_slot(bgs: &mut Backgrounds, screen: DsScreen) {
    let slot = &mut bgs.slots_mut(screen).tile;
    let TileSlot::Pending { image, scroll } = *slot else {
        return;
    };
    *slot = match load_tile(screen, image) {
        Some(bg_id) => TileSlot::Loaded {
            bg_id,
            scroll,
            flushed_scroll: (i16::MIN, i16::MIN),
        },
        // Asset missing/invalid: drop back to Empty so the next setter can
        // retry. We don't loop forever on a broken asset.
        None => TileSlot::Empty,
    };
}

fn process_bitmap_slot(bgs: &mut Backgrounds, screen: DsScreen) {
    // Sub-engine bitmap is unsupported; setters already filter it, but be
    // defensive in case state was constructed by another code path.
    if matches!(screen, DsScreen::Bottom) {
        return;
    }
    let slot = &mut bgs.slots_mut(screen).bitmap;
    let BitmapSlot::Pending { image } = *slot else {
        return;
    };
    *slot = match load_bitmap(screen, image) {
        Some(_bg_id) => BitmapSlot::Loaded,
        None => BitmapSlot::Empty,
    };
}

/// Initialise BG1 (tile) on `screen`, copy gfx/map/palette to VRAM, and show
/// the layer. Returns `Some(bg_id)` on success, `None` if the asset can't be
/// loaded.
fn load_tile(screen: DsScreen, image: &'static [u8]) -> Option<c_int> {
    let loaded = asset::load_tile(image)?;

    let (map_base, tile_base) = match screen {
        DsScreen::Top => (vram::MAIN_TILE_MAP, vram::MAIN_TILE_GFX),
        DsScreen::Bottom => (vram::SUB_TILE_MAP, vram::SUB_TILE_GFX),
    };
    let bg_id = bg_init_hidden(
        screen,
        1, // BG1
        ffi::BG_TYPE_TEXT_4BPP,
        ffi::BG_SIZE_T_256X256,
        map_base,
        tile_base,
    );

    unsafe {
        // Tile gfx: copied byte-by-byte so we don't worry about odd-length
        // assets. VRAM is half-word addressable; the libnds pointer is *u16
        // so we pack pairs.
        let gfx_ptr = ffi::bgGetGfxPtr(bg_id);
        let halfwords = loaded.gfx.len() / 2;
        for i in 0..halfwords {
            let lo = loaded.gfx[i * 2] as u16;
            let hi = loaded.gfx[i * 2 + 1] as u16;
            write_volatile(gfx_ptr.add(i), lo | (hi << 8));
        }

        // Tilemap entries are 16 bits each: tile index (0..9), h/v flip
        // (10..11), palette bank (12..15). grit emits everything in bank 0;
        // we rewrite the bank field to PALETTE_BANK_TILE so our 16 palette
        // colours don't collide with the text console (which lives on BG0 +
        // bank 0).
        let map_ptr = ffi::bgGetMapPtr(bg_id);
        let entries = loaded.map.len() / 2;
        for i in 0..entries {
            let lo = loaded.map[i * 2] as u16;
            let hi = loaded.map[i * 2 + 1] as u16;
            let entry = ((lo | (hi << 8)) & 0x0FFF) | ((PALETTE_BANK_TILE as u16) << 12);
            write_volatile(map_ptr.add(i), entry);
        }

        // Write our 16-colour palette into bank PALETTE_BANK_TILE (offset
        // bank × 16 in the 256-entry BG palette). The text console keeps
        // bank 0 for its own font (transparent + foreground); 3D doesn't
        // touch the BG palette at all.
        let pal_base = match screen {
            DsScreen::Top => ffi::BG_PALETTE,
            DsScreen::Bottom => ffi::BG_PALETTE_SUB,
        };
        let bank_off = PALETTE_BANK_TILE as usize * 16;
        let n = loaded.palette.len().min(16);
        for i in 0..n {
            write_volatile(pal_base.add(bank_off + i), loaded.palette[i]);
        }

        ffi::show_bg(engine_for(screen), 1);
    }
    Some(bg_id)
}

/// Initialise BG3 (bitmap) on the main engine, copy pixels to VRAM, and show
/// the layer. Sub engine isn't supported; callers must filter [`DsScreen`]
/// first.
fn load_bitmap(screen: DsScreen, image: &'static [u8]) -> Option<c_int> {
    debug_assert!(matches!(screen, DsScreen::Top));
    let loaded = asset::load_bitmap(image)?;

    let bg_id = bg_init_hidden(
        screen,
        3, // BG3
        ffi::BG_TYPE_BMP16,
        ffi::BG_SIZE_B16_256X256,
        vram::MAIN_BITMAP_BASE,
        0,
    );
    unsafe {
        let gfx_ptr = ffi::bgGetGfxPtr(bg_id);
        // Copy row-by-row: PNG width is required to be 256 (see png2bg), but
        // height may be shorter (typically 192). Pixels outside the painted
        // region keep whatever was in VRAM — fine in practice because the
        // top 192 rows are the only ones visible.
        let row_pixels = loaded.width as usize;
        for (row_idx, row) in loaded.pixels.chunks_exact(row_pixels).enumerate() {
            // The hardware row stride is 256 pixels regardless of PNG width.
            let dst_row_start = row_idx * 256;
            for (i, &px) in row.iter().enumerate() {
                write_volatile(gfx_ptr.add(dst_row_start + i), px);
            }
        }
        ffi::show_bg(engine_for(screen), 3);
    }
    Some(bg_id)
}

/// Last: flush any pending scroll changes, then call `bgUpdate()` once so
/// libnds latches the layer-control + scroll registers for this frame.
fn flush_scroll_and_update(mut bgs: ResMut<Backgrounds>) {
    for screen in [DsScreen::Top, DsScreen::Bottom] {
        if let TileSlot::Loaded {
            bg_id,
            scroll,
            flushed_scroll,
            ..
        } = &mut bgs.slots_mut(screen).tile
        {
            if scroll != flushed_scroll {
                // bgSetScroll(id, x, y) inlines to bgSetScrollf(id, x<<8, y<<8).
                unsafe {
                    ffi::bgSetScrollf(*bg_id, (scroll.0 as i32) << 8, (scroll.1 as i32) << 8);
                }
                *flushed_scroll = *scroll;
            }
        }
    }
    // Always update: the load path uses bgInitHidden + bgShow, and bgShow's
    // bit flip is what bgUpdate latches. Cheap (writes a few registers).
    unsafe { ffi::bgUpdate() };
}

/// Common imports for games using the BG backend.
pub mod prelude {
    pub use crate::{BackgroundPlugin, Backgrounds, PaintCanvas};
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_nds_video::DsScreen;

    // The state-machine transitions are pure; verify them without touching
    // the FFI. We can't construct `Loaded` from a test (it'd require a real
    // bg_id from libnds), but Empty / Pending transitions cover the
    // user-facing API surface.

    #[test]
    fn set_tile_on_empty_slot_starts_at_zero_scroll() {
        // set_tile_scroll on Empty is intentionally a no-op (no slot to
        // associate scroll with yet), so set_tile lands on a fresh Pending
        // with scroll (0, 0) even though we asked for (4, 8) first.
        let mut bgs = Backgrounds::default();
        bgs.set_tile_scroll(DsScreen::Top, 4, 8);
        bgs.set_tile(DsScreen::Top, b"nitro:/x.bg\0");
        match &bgs.slots(DsScreen::Top).tile {
            TileSlot::Pending { scroll, .. } => assert_eq!(*scroll, (0, 0)),
            _ => panic!("expected Pending after set_tile"),
        }
    }

    #[test]
    fn set_tile_scroll_on_pending_slot_sticks() {
        let mut bgs = Backgrounds::default();
        bgs.set_tile(DsScreen::Top, b"nitro:/x.bg\0");
        bgs.set_tile_scroll(DsScreen::Top, 12, -3);
        match &bgs.slots(DsScreen::Top).tile {
            TileSlot::Pending { scroll, .. } => assert_eq!(*scroll, (12, -3)),
            _ => panic!("expected Pending with updated scroll"),
        }
    }

    #[test]
    fn clear_tile_resets_slot() {
        let mut bgs = Backgrounds::default();
        bgs.set_tile(DsScreen::Top, b"nitro:/x.bg\0");
        bgs.clear_tile(DsScreen::Top);
        assert!(matches!(bgs.slots(DsScreen::Top).tile, TileSlot::Empty));
    }

    #[test]
    fn set_bitmap_top_makes_pending_bottom_is_noop() {
        let mut bgs = Backgrounds::default();
        bgs.set_bitmap(DsScreen::Top, b"nitro:/x.bbg\0");
        bgs.set_bitmap(DsScreen::Bottom, b"nitro:/y.bbg\0");
        assert!(matches!(
            bgs.slots(DsScreen::Top).bitmap,
            BitmapSlot::Pending { .. }
        ));
        assert!(matches!(
            bgs.slots(DsScreen::Bottom).bitmap,
            BitmapSlot::Empty
        ));
    }

    #[test]
    fn enable_paint_canvas_is_pending_and_idempotent() {
        let mut bgs = Backgrounds::default();
        assert!(matches!(bgs.paint, PaintSlot::Empty));
        assert!(bgs.paint().is_none()); // not ready until the plugin inits it
        bgs.enable_paint_canvas();
        assert!(matches!(bgs.paint, PaintSlot::Pending));
        // Second call doesn't reset an already-requested/ready canvas.
        bgs.enable_paint_canvas();
        assert!(matches!(bgs.paint, PaintSlot::Pending));
    }

    #[test]
    fn paint_canvas_dims_match_half_res_layout() {
        assert_eq!(PaintCanvas::WIDTH, 128);
        assert_eq!(PaintCanvas::HEIGHT, 128);
        assert_eq!(PaintCanvas::VISIBLE_HEIGHT, 96); // 192px screen / 2× upscale
    }

    #[test]
    fn replacing_a_pending_tile_image_preserves_scroll() {
        let mut bgs = Backgrounds::default();
        bgs.set_tile(DsScreen::Top, b"nitro:/a.bg\0");
        bgs.set_tile_scroll(DsScreen::Top, 9, 4);
        bgs.set_tile(DsScreen::Top, b"nitro:/b.bg\0");
        match &bgs.slots(DsScreen::Top).tile {
            TileSlot::Pending { image, scroll } => {
                assert_eq!(*image, b"nitro:/b.bg\0".as_slice());
                assert_eq!(*scroll, (9, 4));
            }
            _ => panic!("expected Pending"),
        }
    }
}
