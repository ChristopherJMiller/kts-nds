//! Wall-clock [`WallClock`] resource sourced from the Nintendo DS real-time clock.
//!
//! [`bevy_nds_time`] already drives Bevy's monotonic [`Time`] resource off the
//! hardware bus-clock timer (frame deltas). This crate adds the orthogonal
//! *wall-clock* axis: calendar year/month/day + hour/min/sec from the DS RTC,
//! exposed as a plain [`WallClock`] resource and refreshed once per frame in
//! [`First`].
//!
//! BlocksDS wires the DS RTC behind newlib's standard `<time.h>`, so the FFI
//! surface is a single `time(NULL)` call returning a Unix timestamp. We then
//! decompose it in pure Rust (Howard Hinnant's civil-from-days algorithm), so
//! the conversion is host-testable and we avoid pulling in newlib's `localtime`
//! / `strftime` (which depend on a configured timezone the DS does not have —
//! the RTC reports whatever local time the user set in System Settings).
//!
//! ```ignore
//! use bevy_ecs::prelude::*;
//! use bevy_nds_rtc::WallClock;
//!
//! fn show_clock(clock: Res<WallClock>) {
//!     // 14:35:12 etc.
//!     let _ = (clock.hour, clock.minute, clock.second);
//! }
//! ```
//!
//! [`Time`]: bevy_time::Time

#![cfg_attr(not(test), no_std)]

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

// The DS RTC is read via newlib's standard `<time.h>`. On BlocksDS newlib
// (`sys/_types.h`), `time_t` is `__int64_t`, so `time` takes/returns a 64-bit
// signed second count. Passing a null pointer is the standard "I just want the
// return value" idiom. See `<time.h>` and the SDK example
// `examples/time/rtc_set_get/arm9/source/main.c`.
#[cfg(target_vendor = "nintendo")]
unsafe extern "C" {
    fn time(tloc: *mut i64) -> i64;
}

#[cfg(target_vendor = "nintendo")]
fn read_rtc_secs() -> i64 {
    unsafe { time(core::ptr::null_mut()) }
}

#[cfg(not(target_vendor = "nintendo"))]
fn read_rtc_secs() -> i64 {
    // Host tests never go through the system; the pure decomposition is what
    // we want to exercise. Returning 0 keeps `Default` and the refresh system
    // deterministic if someone wires them up under a host harness anyway.
    0
}

/// Broken-down wall-clock time read from the DS RTC. Refreshed once per frame
/// in [`First`] by [`RtcPlugin`].
///
/// Treat this as best-effort: it reflects whatever local time the user set in
/// the console's System Settings (the DS RTC has no timezone). [`unix_secs`]
/// is the same value reinterpreted as a Unix timestamp, suitable for diffing,
/// save timestamps, or seeding RNG.
///
/// [`unix_secs`]: WallClock::unix_secs
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallClock {
    /// Seconds since 1970-01-01T00:00:00 (as reported by the RTC; no TZ).
    pub unix_secs: i64,
    /// Full Gregorian year, e.g. `2026`.
    pub year: i32,
    /// Month of year, `1..=12`.
    pub month: u8,
    /// Day of month, `1..=31`.
    pub day: u8,
    /// Hour of day, `0..=23`.
    pub hour: u8,
    /// Minute of hour, `0..=59`.
    pub minute: u8,
    /// Second of minute, `0..=59` (no leap seconds — Unix time convention).
    pub second: u8,
    /// Day of week, `0 = Sunday .. 6 = Saturday` (libc / POSIX convention).
    pub weekday: u8,
}

impl Default for WallClock {
    fn default() -> Self {
        Self::from_unix(0)
    }
}

impl WallClock {
    /// Decompose a Unix timestamp into civil (proleptic Gregorian) wall-clock
    /// fields. Pure logic so it can be unit-tested on the host.
    ///
    /// Uses Howard Hinnant's `civil_from_days` algorithm (public domain,
    /// described in <http://howardhinnant.github.io/date_algorithms.html>),
    /// which is exact for the whole representable range of `i64` seconds and
    /// avoids any reliance on a libc that doesn't ship a timezone database.
    pub fn from_unix(secs: i64) -> Self {
        // Floor-divide so pre-epoch timestamps land in the right day.
        let day_count = secs.div_euclid(86_400);
        let sec_of_day = secs.rem_euclid(86_400) as u32;
        let hour = (sec_of_day / 3_600) as u8;
        let minute = ((sec_of_day / 60) % 60) as u8;
        let second = (sec_of_day % 60) as u8;
        // 1970-01-01 was a Thursday; Sunday = 0.
        let weekday = (day_count + 4).rem_euclid(7) as u8;

        // Shift the epoch from 1970-01-01 to 0000-03-01 so leap years land
        // cleanly at the end of each 400-year era.
        let z = day_count + 719_468;
        let era = z.div_euclid(146_097);
        let doe = (z - era * 146_097) as u32; // day of era, [0, 146096]
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
        let mp = (5 * doy + 2) / 153; // shifted month, [0, 11]
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8; // [1, 12]
        let year = (y + if month <= 2 { 1 } else { 0 }) as i32;

        Self {
            unix_secs: secs,
            year,
            month,
            day,
            hour,
            minute,
            second,
            weekday,
        }
    }
}

fn refresh_wall_clock(mut clock: ResMut<WallClock>) {
    *clock = WallClock::from_unix(read_rtc_secs());
}

/// Inserts a [`WallClock`] resource and refreshes it from the DS RTC each
/// frame in [`First`]. Sibling to [`bevy_nds_time::TimePlugin`], which drives
/// the monotonic [`Time`] resource — the two are independent.
///
/// [`Time`]: bevy_time::Time
pub struct RtcPlugin;

impl Plugin for RtcPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WallClock>()
            .add_systems(First, refresh_wall_clock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970_01_01_thursday() {
        let c = WallClock::from_unix(0);
        assert_eq!(
            (
                c.year, c.month, c.day, c.hour, c.minute, c.second, c.weekday
            ),
            (1970, 1, 1, 0, 0, 0, 4),
        );
    }

    #[test]
    fn decomposes_y2k() {
        // 2000-01-01T00:00:00Z — verifiable against `date -u -d @946684800`.
        let c = WallClock::from_unix(946_684_800);
        assert_eq!(
            (
                c.year, c.month, c.day, c.hour, c.minute, c.second, c.weekday
            ),
            (2000, 1, 1, 0, 0, 0, 6), // Saturday
        );
    }

    #[test]
    fn decomposes_known_modern_timestamp() {
        // 2026-06-08T01:54:54Z (Monday) — `date -u -d @1780883694`.
        let c = WallClock::from_unix(1_780_883_694);
        assert_eq!(
            (
                c.year, c.month, c.day, c.hour, c.minute, c.second, c.weekday
            ),
            (2026, 6, 8, 1, 54, 54, 1),
        );
    }

    #[test]
    fn last_second_of_day_and_first_second_of_next() {
        let end = WallClock::from_unix(86_399);
        assert_eq!(
            (
                end.year, end.month, end.day, end.hour, end.minute, end.second
            ),
            (1970, 1, 1, 23, 59, 59),
        );
        let next = WallClock::from_unix(86_400);
        assert_eq!(
            (
                next.year,
                next.month,
                next.day,
                next.hour,
                next.minute,
                next.second
            ),
            (1970, 1, 2, 0, 0, 0),
        );
    }

    #[test]
    fn leap_day_2024_02_29() {
        // 2024-02-29T12:00:00Z.
        let c = WallClock::from_unix(1_709_208_000);
        assert_eq!((c.year, c.month, c.day, c.hour), (2024, 2, 29, 12));
    }

    #[test]
    fn pre_epoch_pulls_day_back_correctly() {
        // -1 second is 1969-12-31T23:59:59 — a Wednesday (weekday 3).
        let c = WallClock::from_unix(-1);
        assert_eq!(
            (
                c.year, c.month, c.day, c.hour, c.minute, c.second, c.weekday
            ),
            (1969, 12, 31, 23, 59, 59, 3),
        );
    }

    #[test]
    fn default_is_unix_epoch() {
        assert_eq!(WallClock::default(), WallClock::from_unix(0));
    }
}
