//! Tile-console rendering, expressed as an ECS extraction step.
//!
//! Full Bevy renders entities to the GPU via wgpu; that stack cannot run on the
//! DS. We keep the *shape* of that model — entities describe what to draw, a
//! system extracts them to the display each frame — but the "GPU" is the DS
//! text console (a tiled background) and the draw call is libnds `printf`.
//!
//! Drawables carry a [`TilePos`] (grid coordinate) and a [`DsScreen`], plus one
//! of:
//! - [`Glyph`] — a single character ("text sprite"), or
//! - [`DsText`] — a run of text.
//!
//! ## Double-buffered, diffed output
//!
//! Naively clearing and reprinting the whole console every frame makes the
//! screen flicker (there is a visible blank moment between the clear and the
//! refill) and is slow. Instead we keep, per screen, two grids — a `front`
//! buffer (what is currently on the display) and a `back` buffer (what we are
//! composing this frame) — both statically sized. Each frame we compose into
//! `back`, then write *only the cells that differ* to the live tilemap and copy
//! them into `front`. The display is never blanked, so there is no flicker, and
//! a typical frame only touches a handful of tiles.

extern crate alloc;

use alloc::string::String;
use core::ffi::{c_char, c_uint};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

use crate::ffi;
use crate::screen::{Consoles, DsScreen};

/// Console grid dimensions (libnds default font is 32x24 tiles).
const COLS: usize = 32;
const ROWS: usize = 24;
const CELLS: usize = COLS * ROWS;
/// Blank cell value (ASCII space).
const BLANK: u8 = b' ';

/// A position on the 32x24 tile grid (0-based, origin top-left).
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub struct TilePos {
    pub x: i16,
    pub y: i16,
}

impl TilePos {
    pub const fn new(x: i16, y: i16) -> Self {
        Self { x, y }
    }
}

/// A single character drawn at a [`TilePos`] — the DS analogue of a text sprite.
#[derive(Component, Clone, Copy)]
pub struct Glyph(pub u8);

/// A run of text drawn starting at a [`TilePos`].
#[derive(Component, Clone)]
pub struct DsText(pub String);

impl DsText {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}

/// Front/back shadow grids for one screen. `front` mirrors the live tilemap;
/// `back` is composed fresh each frame and then diffed against `front`.
struct Grid {
    front: [u8; CELLS],
    back: [u8; CELLS],
}

impl Grid {
    const fn new() -> Self {
        Self {
            front: [BLANK; CELLS],
            back: [BLANK; CELLS],
        }
    }

    /// Reset the composition buffer to all-blank for a new frame.
    fn clear_back(&mut self) {
        self.back = [BLANK; CELLS];
    }

    /// Stamp a single byte into the back buffer (ignored if off-grid).
    fn put(&mut self, x: i16, y: i16, byte: u8) {
        if (0..COLS as i16).contains(&x) && (0..ROWS as i16).contains(&y) {
            self.back[y as usize * COLS + x as usize] = byte;
        }
    }

    /// Stamp a run of bytes starting at (x, y), clipped to the row.
    fn put_str(&mut self, x: i16, y: i16, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.put(x + i as i16, y, b);
        }
    }

    /// Write every cell that changed since last frame to `console`, batching
    /// consecutive changes on a row into a single positioned print, then copy
    /// `back` into `front`.
    ///
    /// # Safety
    /// `console` must be a valid libnds console pointer.
    unsafe fn flush(&mut self, console: *mut ffi::PrintConsole) {
        unsafe { ffi::consoleSelect(console) };

        let mut i = 0;
        while i < CELLS {
            if self.back[i] == self.front[i] {
                i += 1;
                continue;
            }

            // Start of a changed run; gather it (bounded to the current row).
            let row = i / COLS;
            let col = i % COLS;
            let mut run = [0u8; COLS + 1];
            let mut len = 0;
            while i < CELLS && i / COLS == row && self.back[i] != self.front[i] {
                run[len] = self.back[i];
                self.front[i] = self.back[i];
                len += 1;
                i += 1;
            }
            run[len] = 0; // NUL-terminate for %s.

            // ANSI cursor move to 1-based (row, col), then print the run.
            unsafe {
                ffi::printf(
                    c"\x1b[%u;%uH%s".as_ptr(),
                    (row + 1) as c_uint,
                    (col + 1) as c_uint,
                    run.as_ptr() as *const c_char,
                );
            }
        }
    }
}

/// Per-screen shadow grids. Plain arrays, so trivially `Send`/`Sync`.
#[derive(Resource)]
struct Buffers {
    top: Grid,
    bottom: Grid,
}

impl Buffers {
    fn screen(&mut self, screen: DsScreen) -> &mut Grid {
        match screen {
            DsScreen::Top => &mut self.top,
            DsScreen::Bottom => &mut self.bottom,
        }
    }
}

/// Insert the (blank) shadow buffers and clear both consoles once, so the
/// `front` buffers match the now-blank display.
fn setup_buffers(mut commands: Commands, consoles: Res<Consoles>) {
    unsafe {
        ffi::consoleSelect(consoles.handle(DsScreen::Top));
        ffi::consoleClear();
        ffi::consoleSelect(consoles.handle(DsScreen::Bottom));
        ffi::consoleClear();
    }
    commands.insert_resource(Buffers {
        top: Grid::new(),
        bottom: Grid::new(),
    });
}

/// Compose every drawable into the back buffers, then flush the per-cell diff
/// to both consoles. Runs in `Last`, after game systems have updated state.
fn render(
    consoles: Res<Consoles>,
    mut buffers: ResMut<Buffers>,
    glyphs: Query<(&DsScreen, &TilePos, &Glyph)>,
    texts: Query<(&DsScreen, &TilePos, &DsText)>,
) {
    buffers.top.clear_back();
    buffers.bottom.clear_back();

    for (screen, pos, glyph) in &glyphs {
        buffers.screen(*screen).put(pos.x, pos.y, glyph.0);
    }
    for (screen, pos, text) in &texts {
        buffers
            .screen(*screen)
            .put_str(pos.x, pos.y, text.0.as_bytes());
    }

    unsafe {
        buffers.top.flush(consoles.handle(DsScreen::Top));
        buffers.bottom.flush(consoles.handle(DsScreen::Bottom));
    }
}

/// Draws [`Glyph`] / [`DsText`] entities to the DS text consoles each frame.
pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_buffers)
            .add_systems(Last, render);
    }
}
