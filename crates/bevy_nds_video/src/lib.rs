//! The two physical DS screens, modelled for the ECS.
//!
//! Each LCD is driven by a separate 2D engine (the "main" engine outputs to the
//! top screen, the "sub" engine to the bottom). We bring up a libnds text
//! console on each and expose them as a [`Consoles`] resource. Renderable
//! entities carry a [`DsScreen`] component selecting which screen they live on.

#![cfg_attr(not(test), no_std)]

use core::ffi::c_int;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

/// Opaque storage for a libnds `PrintConsole`. The real struct is ~70 bytes;
/// we over-allocate (and over-align) so libnds can initialise it in place.
#[repr(C, align(8))]
pub struct PrintConsole {
    _opaque: [u8; 256],
}

impl PrintConsole {
    /// Zeroed storage suitable for handing to [`consoleInit`].
    const fn zeroed() -> Self {
        Self { _opaque: [0; 256] }
    }
}

// libnds background type / size enums (see <nds/arm9/background.h>). These are
// passed to `consoleInit` to describe the tiled text layer.
const BG_TYPE_TEXT_4BPP: c_int = 1; // BgType_Text4bpp
// Written as in the libnds header: BgSize_T_256x256 = (0 << 14) | (1 << 16).
#[allow(clippy::identity_op)]
const BG_SIZE_T_256X256: c_int = (0 << 14) | (1 << 16);

// Memory-mapped display/VRAM registers (see <nds/arm9/video.h>). We poke these
// directly to bring up the *main* engine for the top screen, since libnds only
// ships a one-call helper (`consoleDemoInit`) for the *sub* engine.
const REG_DISPCNT: *mut u32 = 0x0400_0000 as *mut u32;
const VRAM_A_CR: *mut u8 = 0x0400_0240 as *mut u8;
const MODE_0_2D: u32 = 1 << 16; // DISPLAY_VIDEO_MODE(0) | DISPLAY_MODE_NORMAL
const VRAM_ENABLE: u8 = 1 << 7;
const VRAM_A_MAIN_BG: u8 = 1; // map VRAM bank A to main-engine BG memory

// Master-brightness registers (`<nds/arm9/video.h>`): a final per-engine fade
// applied to the *composited* output (including the 3D layer on the main
// engine), so a single write fades the whole screen to black/white. The main
// engine's lives at 0x0400_006C, the sub engine's 0x1000 above it.
const REG_MASTER_BRIGHT: *mut u16 = 0x0400_006C as *mut u16;
const REG_MASTER_BRIGHT_SUB: *mut u16 = 0x0400_106C as *mut u16;

#[allow(non_snake_case)]
unsafe extern "C" {
    /// Initialise a simple text console on the sub (bottom) screen and select
    /// it. Returns a pointer to the default console it set up.
    /// See `<nds/arm9/console.h>`.
    fn consoleDemoInit() -> *mut PrintConsole;
    /// Initialise a text console on the given background of the main or sub
    /// engine. `main_display` selects the engine; `load_graphics` loads the
    /// default font. See `<nds/arm9/console.h>`.
    fn consoleInit(
        console: *mut PrintConsole,
        layer: c_int,
        bg_type: c_int,
        bg_size: c_int,
        map_base: c_int,
        tile_base: c_int,
        main_display: bool,
        load_graphics: bool,
    ) -> *mut PrintConsole;
}

/// Which physical screen an entity is rendered on.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum DsScreen {
    /// The top LCD, driven by the main 2D engine.
    Top,
    /// The bottom LCD, driven by the sub 2D engine.
    Bottom,
}

/// A pointer to a libnds console. Wrapped so it can live in a `Resource`; the
/// DS is single-core and we only touch consoles from systems, so sharing the
/// raw pointer across the (single) thread is sound.
#[derive(Clone, Copy)]
pub struct ConsoleHandle(*mut PrintConsole);

impl ConsoleHandle {
    /// The raw libnds pointer, for crates that drive the console directly
    /// (e.g. `bevy_nds_text`).
    pub fn as_ptr(self) -> *mut PrintConsole {
        self.0
    }
}

// SAFETY: the DS runs systems on a single core; there is no real concurrency.
unsafe impl Send for ConsoleHandle {}
unsafe impl Sync for ConsoleHandle {}

/// Handles to the initialised top/bottom consoles.
#[derive(Resource, Clone, Copy)]
pub struct Consoles {
    top: ConsoleHandle,
    bottom: ConsoleHandle,
}

impl Consoles {
    /// The console backing the given screen.
    pub fn handle(&self, screen: DsScreen) -> ConsoleHandle {
        match screen {
            DsScreen::Top => self.top,
            DsScreen::Bottom => self.bottom,
        }
    }
}

/// Backing storage for the top console. libnds initialises it in place during
/// [`init_screens`]; it must outlive the program, hence `static`.
static mut TOP_CONSOLE: PrintConsole = PrintConsole::zeroed();

/// Brings up a text console on both screens and inserts the [`Consoles`]
/// resource. Runs once, before the first frame is rendered.
fn init_screens(mut commands: Commands) {
    let (top, bottom) = unsafe {
        // The sub engine + bottom console: libnds has a one-call helper that
        // also configures the sub video mode and VRAM bank C.
        let bottom = consoleDemoInit();

        // The main engine + top console: configure video mode 0 and map VRAM
        // bank A to main-engine background memory, then init the console on it.
        core::ptr::write_volatile(REG_DISPCNT, MODE_0_2D);
        core::ptr::write_volatile(VRAM_A_CR, VRAM_ENABLE | VRAM_A_MAIN_BG);
        let top = consoleInit(
            &raw mut TOP_CONSOLE,
            0,                 // background layer 0
            BG_TYPE_TEXT_4BPP, // 4bpp tiled text
            BG_SIZE_T_256X256, // 256x256 text background
            22,                // map base (matches the demo console)
            3,                 // tile base
            true,              // main_display -> top screen
            true,              // load the default font
        );
        (top, bottom)
    };

    commands.insert_resource(Consoles {
        top: ConsoleHandle(top),
        bottom: ConsoleHandle(bottom),
    });
}

/// Per-screen master-brightness fade. Each field is a level in `-16..=16`:
/// `0` = normal, **negative** fades toward black, **positive** toward white;
/// magnitude `16` is fully black / white. Set it from a system (e.g. a scene
/// transition) and [`apply_brightness`] writes the hardware registers each
/// frame. The main engine drives the top (3D) screen, the sub engine the bottom.
#[derive(Resource, Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct MasterBright {
    pub top: i8,
    pub bottom: i8,
}

/// Encode a fade level (`-16..=16`) into a `MASTER_BRIGHT` register value:
/// bits 0..4 = factor (`0..16`), bits 14..15 = mode (`1` = fade up/white,
/// `2` = fade down/black; `0` = disabled). Pure, so it host-tests directly.
pub fn master_bright_bits(level: i8) -> u16 {
    let level = level.clamp(-16, 16);
    match level {
        0 => 0,
        _ if level < 0 => (2u16 << 14) | (-(level as i16)) as u16,
        _ => (1u16 << 14) | level as u16,
    }
}

/// Push [`MasterBright`] to the hardware `MASTER_BRIGHT` registers (main + sub).
/// Runs in [`Last`] so it reflects whatever a transition wrote during `Update`,
/// and only when the resource changed (the registers latch, so re-writing an
/// unchanged value is wasted MMIO).
fn apply_brightness(bright: Res<MasterBright>) {
    if !bright.is_changed() {
        return;
    }
    let (top, bottom) = (
        master_bright_bits(bright.top),
        master_bright_bits(bright.bottom),
    );
    // SAFETY: fixed MMIO addresses for the two 2D engines' brightness latches;
    // the DS is single-core and these are plain register writes.
    #[cfg(target_vendor = "nintendo")]
    unsafe {
        core::ptr::write_volatile(REG_MASTER_BRIGHT, top);
        core::ptr::write_volatile(REG_MASTER_BRIGHT_SUB, bottom);
    }
    #[cfg(not(target_vendor = "nintendo"))]
    let _ = (top, bottom);
}

/// Initialises the DS video hardware and exposes the screens to the ECS.
pub struct VideoPlugin;

impl Plugin for VideoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MasterBright>()
            .add_systems(PreStartup, init_screens)
            .add_systems(Last, apply_brightness);
    }
}
