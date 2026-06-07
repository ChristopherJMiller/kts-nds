//! A real-time clock that powers Bevy's standard [`Time`] resource.
//!
//! On desktop, `bevy_time` reads a wall clock. The DS has no `std` clock, but it
//! does have a free-running hardware timer at the bus clock (~33.51 MHz). We
//! start it once and, each frame, advance virtual time by the real number of
//! ticks elapsed since the previous frame. Game code can then use the ordinary
//! `Res<Time>` API (`elapsed_secs`, `delta_secs`, ...) and it reflects true
//! wall-clock time — including any frames that ran long.

use core::time::Duration;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_time::Time;

use crate::ffi;

/// DS bus clock in Hz (see `BUS_CLOCK` in libnds `timers.h`).
const BUS_CLOCK: u64 = 33_513_982;

/// Last hardware-timer reading, used to compute per-frame deltas. The timer is
/// 32 bits and wraps about every 128 s, which `wrapping_sub` handles.
#[derive(Resource)]
struct HardwareClock {
    last_ticks: u32,
}

fn start_clock(mut commands: Commands) {
    let last_ticks = unsafe {
        ffi::cpuStartTiming(0);
        ffi::cpuGetTiming()
    };
    commands.insert_resource(HardwareClock { last_ticks });
}

fn advance_time(mut time: ResMut<Time>, mut clock: ResMut<HardwareClock>) {
    let now = unsafe { ffi::cpuGetTiming() };
    let delta_ticks = now.wrapping_sub(clock.last_ticks);
    clock.last_ticks = now;

    let nanos = delta_ticks as u64 * 1_000_000_000 / BUS_CLOCK;
    time.advance_by(Duration::from_nanos(nanos));
}

/// Inserts a [`Time`] resource and advances it by real elapsed time each frame.
pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Time>()
            .add_systems(PreStartup, start_clock)
            .add_systems(First, advance_time);
    }
}
