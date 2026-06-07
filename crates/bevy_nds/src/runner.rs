//! The application runner: a vblank-paced main loop that owns the program.
//!
//! Bevy's desktop runner is driven by `winit`'s event loop; ours is driven by
//! the DS vertical-blank interrupt. Installing it via [`App::set_runner`] means
//! the rest of the program is ordinary Bevy: the loop just paces frames and
//! calls `App::update`, which runs every schedule (`First` .. `Last`).

use bevy_app::{App, AppExit, PluginGroup, PluginGroupBuilder};

use crate::diagnostics::DiagnosticsPlugin;
use crate::ffi;
use crate::input::InputPlugin;
use crate::render::RenderPlugin;
use crate::screen::VideoPlugin;
use crate::time::TimePlugin;

/// The DS frame loop. Never returns — there is nothing to exit *to*.
fn ds_runner(mut app: App) -> AppExit {
    // Finish plugin setup, then run forever, one `update` per display refresh.
    app.finish();
    app.cleanup();
    loop {
        unsafe { ffi::swiWaitForVBlank() };
        app.update();
    }
}

/// Bundles every DS integration plugin. Add this to your [`App`] to wire up the
/// screens, input, time and rendering, then call [`run`].
pub struct DsPlugins;

impl PluginGroup for DsPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(VideoPlugin)
            .add(TimePlugin)
            .add(DiagnosticsPlugin)
            .add(InputPlugin)
            .add(RenderPlugin)
    }
}

/// Installs the DS runner and starts the frame loop. Does not return.
pub fn run(mut app: App) -> ! {
    app.set_runner(ds_runner);
    app.run();
    // The runner loops forever, so control never reaches here.
    unreachable!("the DS runner never returns")
}
