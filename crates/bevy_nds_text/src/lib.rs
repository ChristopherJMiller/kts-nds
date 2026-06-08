//! Tile-console text rendering, expressed as an ECS extraction step.
//!
//! Full Bevy renders entities to the GPU via wgpu; that stack cannot run on the
//! DS. We keep the *shape* of that model — entities describe what to draw, a
//! system extracts them to the display each frame — but the "GPU" is the DS
//! text console (a tiled background) and the draw call is libnds `printf`.
//!
//! Drawables carry a [`TilePos`] (grid coordinate) and a [`DsScreen`], plus one
//! of:
//! - [`DsText`] — a run of text (rendered first), or
//! - [`Glyph`] — a single character "sticker" rendered *on top of* any text in
//!   the same cell. Use this to move a marker around without recomposing the
//!   underlying text every frame (e.g. a player `@` over a static map row).
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

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use core::ffi::{c_char, c_uint};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds_video::{Consoles, DsScreen, PrintConsole};

#[allow(non_snake_case)]
unsafe extern "C" {
    /// Make `console` the target of subsequent console output (`printf`, etc.).
    /// See `<nds/arm9/console.h>`.
    fn consoleSelect(console: *mut PrintConsole) -> *mut PrintConsole;
    /// Clear the active console.
    fn consoleClear();
    /// printf to the active console (libnds redirects stdout to the console).
    fn printf(fmt: *const c_char, ...) -> core::ffi::c_int;
}

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

    /// Diff `back` against `front`, emitting each changed run and advancing
    /// `front` to match. For every maximal run of cells that changed on a single
    /// row, `emit(row, col, &bytes)` is called with the run's new bytes (0-based
    /// grid coordinates). This is the pure core of [`flush`] — it performs no
    /// I/O — which keeps the anti-flicker diff logic unit-testable.
    fn diff_runs(&mut self, mut emit: impl FnMut(usize, usize, &[u8])) {
        let mut i = 0;
        while i < CELLS {
            if self.back[i] == self.front[i] {
                i += 1;
                continue;
            }

            // Start of a changed run; gather it (bounded to the current row).
            let row = i / COLS;
            let col = i % COLS;
            let mut run = [0u8; COLS];
            let mut len = 0;
            while i < CELLS && i / COLS == row && self.back[i] != self.front[i] {
                run[len] = self.back[i];
                self.front[i] = self.back[i];
                len += 1;
                i += 1;
            }

            emit(row, col, &run[..len]);
        }
    }

    /// Write every cell that changed since last frame to `console`, batching
    /// consecutive changes on a row into a single positioned print, then copy
    /// `back` into `front`.
    ///
    /// # Safety
    /// `console` must be a valid libnds console pointer.
    unsafe fn flush(&mut self, console: *mut PrintConsole) {
        unsafe { consoleSelect(console) };

        self.diff_runs(|row, col, bytes| {
            // Copy into a NUL-terminated buffer for `%s`.
            let mut run = [0u8; COLS + 1];
            run[..bytes.len()].copy_from_slice(bytes);
            run[bytes.len()] = 0;

            // ANSI cursor move, then print the run. NB: libnds's ANSI parser
            // treats the row/col params in `ESC[r;cH` as *zero-based*, not the
            // standard 1-based that VT100/xterm use (see libnds console.c's
            // case 'H' — it assigns `cursorY = params[0]` with no -1). Pass
            // raw 0-based coords so the cursor lands where the diff intended.
            unsafe {
                printf(
                    c"\x1b[%u;%uH%s".as_ptr(),
                    row as c_uint,
                    col as c_uint,
                    run.as_ptr() as *const c_char,
                );
            }
        });
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
        consoleSelect(consoles.handle(DsScreen::Top).as_ptr());
        consoleClear();
        consoleSelect(consoles.handle(DsScreen::Bottom).as_ptr());
        consoleClear();
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

    // Text first, glyphs on top: lets games park static `DsText` rows (a map,
    // a HUD frame) and use cheap single-cell `Glyph` entities for the moving
    // pieces, without recomposing the underlying text each frame.
    for (screen, pos, text) in &texts {
        buffers
            .screen(*screen)
            .put_str(pos.x, pos.y, text.0.as_bytes());
    }
    for (screen, pos, glyph) in &glyphs {
        buffers.screen(*screen).put(pos.x, pos.y, glyph.0);
    }

    unsafe {
        buffers.top.flush(consoles.handle(DsScreen::Top).as_ptr());
        buffers
            .bottom
            .flush(consoles.handle(DsScreen::Bottom).as_ptr());
    }
}

/// Draws [`Glyph`] / [`DsText`] entities to the DS text consoles each frame.
pub struct TextRenderPlugin;

impl Plugin for TextRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_buffers)
            .add_systems(Last, render);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Collect the changed runs a flush would emit, as (row, col, bytes).
    fn runs(grid: &mut Grid) -> Vec<(usize, usize, Vec<u8>)> {
        let mut out = Vec::new();
        grid.diff_runs(|row, col, bytes| out.push((row, col, bytes.to_vec())));
        out
    }

    #[test]
    fn put_writes_on_grid_cell() {
        let mut g = Grid::new();
        g.put(3, 2, b'A');
        assert_eq!(g.back[2 * COLS + 3], b'A');
    }

    #[test]
    fn put_ignores_off_grid_coordinates() {
        let mut g = Grid::new();
        // Negative, and past each edge — none should write or panic.
        g.put(-1, 0, b'X');
        g.put(0, -1, b'X');
        g.put(COLS as i16, 0, b'X');
        g.put(0, ROWS as i16, b'X');
        assert!(g.back.iter().all(|&c| c == BLANK));
    }

    #[test]
    fn put_str_clips_at_row_end() {
        let mut g = Grid::new();
        // Starts two cells before the right edge: only "AB" should land.
        g.put_str(COLS as i16 - 2, 1, b"ABCD");
        assert_eq!(g.back[1 * COLS + (COLS - 2)], b'A');
        assert_eq!(g.back[1 * COLS + (COLS - 1)], b'B');
        // It must not wrap onto the next row.
        assert!(g.back[2 * COLS..].iter().all(|&c| c == BLANK));
    }

    #[test]
    fn diff_emits_one_run_for_contiguous_changes() {
        let mut g = Grid::new();
        g.put_str(4, 2, b"Hello");
        assert_eq!(runs(&mut g), [(2, 4, b"Hello".to_vec())]);
    }

    #[test]
    fn diff_splits_runs_on_unchanged_cells() {
        let mut g = Grid::new();
        g.put(0, 0, b'A');
        g.put(2, 0, b'B'); // cell 1 stays blank, breaking the run
        assert_eq!(runs(&mut g), [(0, 0, b"A".to_vec()), (0, 2, b"B".to_vec())]);
    }

    #[test]
    fn diff_does_not_merge_across_rows() {
        let mut g = Grid::new();
        // Fill the whole first row and the first cell of the second.
        for x in 0..COLS as i16 {
            g.put(x, 0, b'#');
        }
        g.put(0, 1, b'#');
        let r = runs(&mut g);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].0, r[0].1, r[0].2.len()), (0, 0, COLS));
        assert_eq!(r[1], (1, 0, b"#".to_vec()));
    }

    #[test]
    fn flush_advances_front_and_is_idempotent() {
        let mut g = Grid::new();
        g.put_str(1, 1, b"hi");
        let first = runs(&mut g);
        assert_eq!(first, [(1, 1, b"hi".to_vec())]);
        // front now mirrors back; a second diff with no recomposition is empty.
        assert!(runs(&mut g).is_empty());
        // front must actually hold the drawn bytes.
        assert_eq!(&g.front[1 * COLS + 1..1 * COLS + 3], b"hi");
    }

    #[test]
    fn diff_detects_a_cleared_cell() {
        let mut g = Grid::new();
        g.put(5, 5, b'@');
        let _ = runs(&mut g); // commit '@' into front
        g.clear_back(); // entity moved away: cell goes back to blank
        assert_eq!(runs(&mut g), [(5, 5, alloc::vec![BLANK])]);
    }
}
