//! Lightweight runtime diagnostics, surfaced as ECS resources.
//!
//! Two things live here:
//!
//! - `Fps`, a smoothed frames-per-second estimate derived from the real
//!   per-frame delta provided by [`bevy_nds_time`](https://docs.rs/bevy_nds_time).
//! - `PerfBlob`, a fixed-layout record in main RAM that mirrors a short ring
//!   buffer of recent per-frame microsecond samples. The DS has no way to
//!   stream telemetry out by itself, but an emulator's gdbstub (e.g. desmume
//!   built with `gdb-stub=true` and launched with `--arm9gdb=PORT`) can read
//!   ARM9 memory. A host-side tool scans main RAM for the magic header
//!   (`b"BVDS"`), pulls the ring, and computes min/avg/p95. This is the
//!   no-FPU, no-Wi-Fi DS analogue of "print to stderr from inside a test".

#![cfg_attr(not(test), no_std)]

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_time::Time;

/// Smoothed frames-per-second estimate. `0.0` until the first delta arrives.
#[derive(Resource, Default, Clone, Copy)]
pub struct Fps(pub f32);

/// Exponential-smoothing factor (weight given to the newest sample).
const SMOOTHING: f32 = 0.1;

/// Fold a new per-frame delta into the smoothed FPS estimate. The first sample
/// (when `prev == 0.0`) seeds the average; non-positive deltas leave it
/// unchanged. Pure helper so the smoothing maths is unit-testable off-target.
fn smooth_fps(prev: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        return prev;
    }
    let instant = 1.0 / dt;
    if prev == 0.0 {
        instant
    } else {
        prev * (1.0 - SMOOTHING) + instant * SMOOTHING
    }
}

// --- PerfBlob (host-readable frame-time ring) --------------------------------

/// Number of frame-time samples held in [`PerfBlob`]. 256 covers ~4s at 60 Hz —
/// plenty for the short windows `just preview` exercises and small enough that
/// a host gdbstub read finishes in well under a frame.
pub const PERF_RING_LEN: usize = 256;

/// Bytewise magic the host scanner looks for in main RAM. Chosen to be unlikely
/// to appear by accident in code or data and easy to spot in a hex dump.
pub const PERF_MAGIC: [u8; 4] = *b"BVDS";

/// On-wire layout version. Bump if any field changes size, position, or
/// meaning; the host tool refuses to decode a blob whose version it doesn't
/// recognise.
pub const PERF_VERSION: u32 = 1;

/// Host-readable record of recent per-frame microsecond samples, with a
/// fixed-layout header so an off-board tool (gdb-remote / memory dump) can
/// locate and decode it without symbol info.
///
/// Layout is `#[repr(C)]`; treat the field order as part of the on-wire ABI.
/// `head` and `written` cooperate to disambiguate "ring not yet full" from "ring
/// already wrapped":
/// - while `written < PERF_RING_LEN`, samples `ring_us[0..written]` are valid;
/// - once `written >= PERF_RING_LEN`, the whole ring is valid, with the oldest
///   sample at `ring_us[head]` and the newest at `ring_us[(head + PERF_RING_LEN
///   - 1) % PERF_RING_LEN]`.
#[repr(C)]
pub struct PerfBlob {
    /// `PERF_MAGIC` (`b"BVDS"`). Lets a host scanner find this struct in main
    /// RAM without symbols.
    pub magic: [u8; 4],
    /// `PERF_VERSION`. Layout/version gate for the host decoder.
    pub version: u32,
    /// Index of the next ring slot to write — always in `0..PERF_RING_LEN`.
    pub head: u32,
    /// Length of the ring (`PERF_RING_LEN`). Stored so the host doesn't have to
    /// be recompiled when the ROM grows the buffer.
    pub ring_len: u32,
    /// Total number of samples ever pushed (saturates at `u64::MAX`). The host
    /// uses this to tell "ring still warming up" from "ring already wrapped".
    pub written: u64,
    /// Per-frame deltas in microseconds. Zero means "no sample yet".
    pub ring_us: [u32; PERF_RING_LEN],
}

impl PerfBlob {
    /// Empty ring with the magic + version stamped. `const` so it can seed a
    /// `static` directly.
    pub const fn new() -> Self {
        Self {
            magic: PERF_MAGIC,
            version: PERF_VERSION,
            head: 0,
            ring_len: PERF_RING_LEN as u32,
            written: 0,
            ring_us: [0; PERF_RING_LEN],
        }
    }

    /// Append a single microsecond sample. Zero deltas (which would show up as
    /// "no sample yet" in the host decoder) are dropped to keep the on-wire
    /// semantics clean.
    pub fn push_us(&mut self, us: u32) {
        if us == 0 {
            return;
        }
        let head = self.head as usize;
        self.ring_us[head] = us;
        self.head = ((head + 1) % PERF_RING_LEN) as u32;
        self.written = self.written.saturating_add(1);
    }
}

impl Default for PerfBlob {
    fn default() -> Self {
        Self::new()
    }
}

/// The actual blob that lives in main RAM at link time. `#[unsafe(no_mangle)]`
/// keeps the symbol name stable so a host dev can also locate it via the ELF
/// symbol table; the host tool included in this workspace doesn't need that and
/// scans for the magic header instead. Only compiled on the DS target to keep
/// host unit tests of dependent crates free of mutable-static collisions.
#[cfg(target_vendor = "nintendo")]
#[unsafe(no_mangle)]
pub static mut PERF_BLOB: PerfBlob = PerfBlob::new();

/// Push `us` into `PERF_BLOB`. Inert on the host so dependent crates can be
/// unit-tested without dragging a static-mut into the test binary.
#[inline]
fn record_perf_sample(us: u32) {
    #[cfg(target_vendor = "nintendo")]
    // SAFETY: the DS is single-core and only `update_fps` touches `PERF_BLOB`,
    // so there is no concurrent access from Rust code. A host gdbstub reader
    // may observe a partial update; that's tolerated by the on-wire protocol
    // (the reader re-reads `head` + `written` to detect a torn sample).
    unsafe {
        let blob = &mut *(&raw mut PERF_BLOB);
        blob.push_us(us);
    }
    #[cfg(not(target_vendor = "nintendo"))]
    let _ = us;
}

fn update_fps(time: Res<Time>, mut fps: ResMut<Fps>) {
    let dt = time.delta_secs();
    fps.0 = smooth_fps(fps.0, dt);
    // u128 → u32 cast is safe in practice: a 1 s delta is 1_000_000 µs, far
    // below u32::MAX. A pathologically long stall would clip; that's fine.
    record_perf_sample(time.delta().as_micros() as u32);
}

/// Maintains the [`Fps`] resource each frame and (on the DS) the host-readable
/// [`PerfBlob`] frame-time ring.
pub struct DiagnosticsPlugin;

impl Plugin for DiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Fps>()
            .add_systems(PreUpdate, update_fps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_seeds_with_instantaneous_rate() {
        // 1/60 s frame -> 60 fps on the very first delta.
        assert!((smooth_fps(0.0, 1.0 / 60.0) - 60.0).abs() < 0.01);
    }

    #[test]
    fn non_positive_delta_leaves_estimate_unchanged() {
        assert_eq!(smooth_fps(42.0, 0.0), 42.0);
        assert_eq!(smooth_fps(42.0, -1.0), 42.0);
    }

    #[test]
    fn blends_towards_the_new_sample() {
        // From a steady 60, a slower (30 fps) frame nudges the estimate down,
        // but only by the smoothing weight, so it stays well above 30.
        let next = smooth_fps(60.0, 1.0 / 30.0);
        let expected = 60.0 * (1.0 - SMOOTHING) + 30.0 * SMOOTHING;
        assert!((next - expected).abs() < 0.001);
        assert!(next < 60.0 && next > 30.0);
    }

    #[test]
    fn steady_rate_converges_to_that_rate() {
        let mut fps = 0.0;
        for _ in 0..200 {
            fps = smooth_fps(fps, 1.0 / 60.0);
        }
        assert!((fps - 60.0).abs() < 0.01);
    }

    #[test]
    fn perf_blob_seeds_with_magic_and_version() {
        let b = PerfBlob::new();
        assert_eq!(b.magic, PERF_MAGIC);
        assert_eq!(b.version, PERF_VERSION);
        assert_eq!(b.head, 0);
        assert_eq!(b.written, 0);
        assert_eq!(b.ring_len as usize, PERF_RING_LEN);
        assert!(b.ring_us.iter().all(|&x| x == 0));
    }

    #[test]
    fn perf_blob_push_advances_head_and_written() {
        let mut b = PerfBlob::new();
        b.push_us(16_667);
        b.push_us(16_700);
        b.push_us(33_000);
        assert_eq!(b.ring_us[0], 16_667);
        assert_eq!(b.ring_us[1], 16_700);
        assert_eq!(b.ring_us[2], 33_000);
        assert_eq!(b.head, 3);
        assert_eq!(b.written, 3);
    }

    #[test]
    fn perf_blob_drops_zero_samples() {
        let mut b = PerfBlob::new();
        b.push_us(16_667);
        b.push_us(0);
        b.push_us(16_700);
        assert_eq!(b.ring_us[0], 16_667);
        assert_eq!(b.ring_us[1], 16_700);
        assert_eq!(b.head, 2);
        assert_eq!(b.written, 2);
    }

    #[test]
    fn perf_blob_wraps_after_ring_len_samples() {
        let mut b = PerfBlob::new();
        // First PERF_RING_LEN samples fill the ring; head wraps back to 0.
        for i in 0..PERF_RING_LEN as u32 {
            b.push_us(i + 1);
        }
        assert_eq!(b.head, 0);
        assert_eq!(b.written, PERF_RING_LEN as u64);
        assert_eq!(b.ring_us[0], 1);
        assert_eq!(b.ring_us[PERF_RING_LEN - 1], PERF_RING_LEN as u32);

        // One more sample overwrites the oldest slot; head moves to 1.
        b.push_us(9999);
        assert_eq!(b.head, 1);
        assert_eq!(b.written, PERF_RING_LEN as u64 + 1);
        assert_eq!(b.ring_us[0], 9999);
        assert_eq!(b.ring_us[1], 2);
    }
}
