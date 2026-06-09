//! `bevy_nds` — run Bevy's `no_std` ECS on the Nintendo DS.
//!
//! This is the **umbrella crate**: it re-exports the platform-layer subcrates
//! and bundles them as a single [`DsPlugins`] plugin group. Each capability
//! lives in its own crate so games can opt in to just what they need
//! (e.g. drop [`bevy_nds_text`] for a sprite-only game) or so the test suite
//! can run pure-logic tests on the host without the bare-metal items getting
//! in the way.
//!
//! | DS hardware            | This crate exposes                         | Subcrate / plugin                                                  |
//! | ---------------------- | ------------------------------------------ | ------------------------------------------------------------------ |
//! | Top / bottom LCDs      | [`DsScreen`] component + [`Consoles`] resource | [`bevy_nds_video::VideoPlugin`]                                |
//! | Buttons                | `ButtonInput<`[`DsButton`]`>` resource     | [`bevy_nds_input::InputPlugin`]                                    |
//! | Touch screen           | `Touches` resource + `TouchInput` events   | [`bevy_nds_input::InputPlugin`]                                    |
//! | Touch gestures         | [`Gestures`] resource + [`GestureEvent`] events | [`bevy_nds_gesture::GesturePlugin`]                           |
//! | Vertical-blank @ 60 Hz | the [`run`] loop + `Time` resource         | [`bevy_nds_runtime::run`] + [`bevy_nds_time::TimePlugin`]          |
//! | —                      | smoothed [`Fps`] resource                  | [`bevy_nds_diagnostics::DiagnosticsPlugin`]                        |
//! | Tiled text background  | [`Glyph`] / [`DsText`] + [`TilePos`]       | [`bevy_nds_text::TextRenderPlugin`]                                |
//! | ROM filesystem         | [`NitroFs`] resource + [`read_file`]       | [`bevy_nds_nitrofs::NitroFsPlugin`]                                |
//! | Math coprocessor       | [`Fx32`] / [`FxVec3`] + [`bevy_nds_math::hw`] divide/sqrt | [`bevy_nds_math`]                            |
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
//!
//! [`read_file`]: bevy_nds_nitrofs::read_file

#![no_std]

use bevy_app::{PluginGroup, PluginGroupBuilder};

// Re-export the platform subcrates' public surface so games can import
// everything from `bevy_nds::*` (or, preferably, `bevy_nds::prelude::*`).
pub use bevy_nds_diagnostics::{DiagnosticsPlugin, Fps};
pub use bevy_nds_gesture::{
    Gesture, GestureEvent, GesturePlugin, GestureRecognizer, Gestures, SwipeDir,
};
pub use bevy_nds_input::{DsButton, InputPlugin};
pub use bevy_nds_math::{Fx32, FxVec2, FxVec3};
pub use bevy_nds_nitrofs::{NitroFs, NitroFsPlugin, flush_dcache, init_nitrofs, read_file};
pub use bevy_nds_runtime::run;
pub use bevy_nds_text::{DsText, Glyph, TextRenderPlugin, TilePos};
pub use bevy_nds_time::TimePlugin;
pub use bevy_nds_video::{ConsoleHandle, Consoles, DsScreen, PrintConsole, VideoPlugin};

/// Bundles every DS platform plugin. Add this to your [`App`] to wire up the
/// screens, input, time, text rendering and the ROM filesystem, then call
/// [`run`].
pub struct DsPlugins;

impl PluginGroup for DsPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(VideoPlugin)
            .add(NitroFsPlugin)
            .add(TimePlugin)
            .add(DiagnosticsPlugin)
            .add(InputPlugin)
            .add(GesturePlugin)
            .add(TextRenderPlugin)
    }
}

/// Common imports for games built on `bevy_nds`.
pub mod prelude {
    pub use crate::{
        DsButton, DsPlugins, DsScreen, DsText, Fps, Fx32, FxVec2, FxVec3, Gesture, GestureEvent,
        Gestures, Glyph, NitroFs, SwipeDir, TilePos, run,
    };
    pub use bevy_input::ButtonInput;
    pub use bevy_input::touch::{TouchInput, TouchPhase, Touches};
    pub use bevy_time::Time;
}
