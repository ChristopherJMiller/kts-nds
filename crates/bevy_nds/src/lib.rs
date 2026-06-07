//! `bevy_nds` — run Bevy's `no_std` ECS on the Nintendo DS.
//!
//! This crate is the reusable integration layer between Bevy and the DS. It
//! provides the bare-metal runtime (allocator, panic handler, critical section),
//! a vblank-driven [`run`] loop, and a set of Bevy plugins that map the DS
//! hardware onto ECS concepts:
//!
//! | DS hardware            | This crate exposes                         |
//! | ---------------------- | ------------------------------------------ |
//! | Top / bottom LCDs      | [`DsScreen`] component + `Consoles` resource (via [`VideoPlugin`]) |
//! | Buttons                | `ButtonInput<`[`DsButton`]`>` resource (via [`InputPlugin`]) |
//! | Vertical-blank @ 60 Hz | the [`run`] loop + `Time` resource (via [`TimePlugin`]) |
//! | Tiled text background   | [`Glyph`] / [`DsText`] + [`TilePos`] drawn by [`RenderPlugin`] |
//!
//! Games depend on this crate, add [`DsPlugins`] to their `App`, and call
//! [`run`] — they never touch FFI directly.
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//!
//! use bevy_app::prelude::*;
//! use bevy_nds::prelude::*;
//!
//! #[unsafe(no_mangle)]
//! pub extern "C" fn main() -> core::ffi::c_int {
//!     let mut app = App::new();
//!     app.add_plugins(DsPlugins);
//!     bevy_nds::run(app)
//! }
//! ```

#![no_std]

extern crate alloc;

mod diagnostics;
mod ffi;
mod input;
mod render;
mod runner;
mod runtime;
mod screen;
mod time;

pub use diagnostics::{DiagnosticsPlugin, Fps};
pub use input::{DsButton, InputPlugin};
pub use render::{DsText, Glyph, RenderPlugin, TilePos};
pub use runner::{DsPlugins, run};
pub use screen::{Consoles, DsScreen, VideoPlugin};
pub use time::TimePlugin;

/// Common imports for games built on `bevy_nds`.
pub mod prelude {
    pub use crate::diagnostics::Fps;
    pub use crate::input::DsButton;
    pub use crate::render::{DsText, Glyph, TilePos};
    pub use crate::runner::{DsPlugins, run};
    pub use crate::screen::DsScreen;
    pub use bevy_input::ButtonInput;
    pub use bevy_time::Time;
}
