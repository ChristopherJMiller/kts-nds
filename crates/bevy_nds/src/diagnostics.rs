//! Lightweight runtime diagnostics, surfaced as ECS resources.
//!
//! Right now this is just a smoothed frames-per-second counter derived from the
//! real per-frame delta provided by [`crate::time`]. Games read `Res<Fps>` and
//! display it however they like.

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_time::Time;

/// Smoothed frames-per-second estimate. `0.0` until the first delta arrives.
#[derive(Resource, Default, Clone, Copy)]
pub struct Fps(pub f32);

/// Exponential-smoothing factor (weight given to the newest sample).
const SMOOTHING: f32 = 0.1;

fn update_fps(time: Res<Time>, mut fps: ResMut<Fps>) {
    let dt = time.delta_secs();
    if dt > 0.0 {
        let instant = 1.0 / dt;
        fps.0 = if fps.0 == 0.0 {
            instant
        } else {
            fps.0 * (1.0 - SMOOTHING) + instant * SMOOTHING
        };
    }
}

/// Maintains the [`Fps`] resource each frame.
pub struct DiagnosticsPlugin;

impl Plugin for DiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Fps>()
            .add_systems(PreUpdate, update_fps);
    }
}
