//! Pure audio bookkeeping, kept separate from FFI so it can be unit tested on
//! the host (the repo convention: split the testable computation out of the
//! hardware call). Covers the volume/panning quantisation maxmod expects and a
//! small "which effects have been loaded" set.

use alloc::collections::BTreeSet;

/// Convert a normalised volume `0.0..=1.0` to maxmod's effect range `0..=255`.
/// Out-of-range inputs are clamped.
pub fn effect_volume(v: f32) -> u32 {
    quantise(v, 255)
}

/// Convert a normalised volume `0.0..=1.0` to maxmod's module range `0..=1024`.
/// Out-of-range inputs are clamped.
pub fn module_volume(v: f32) -> u32 {
    quantise(v, 1024)
}

/// Convert a normalised pan `0.0` (left) ..= `1.0` (right) to maxmod's `0..=255`
/// (so `0.5` maps to centre, `128`). Out-of-range inputs are clamped.
pub fn panning(p: f32) -> u8 {
    quantise(p, 255) as u8
}

/// Clamp `v` to `0.0..=1.0` and scale to `0..=max`, rounding to nearest.
fn quantise(v: f32, max: u32) -> u32 {
    let clamped = if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    };
    // `+ 0.5` rounds to nearest; the result is in `0..=max` after clamping.
    (clamped * max as f32 + 0.5) as u32
}

/// Tracks which soundbank effects have already been loaded, so an effect is
/// loaded exactly once before it is first played (loading twice wastes RAM).
#[derive(Default)]
pub struct LoadedEffects {
    loaded: BTreeSet<u32>,
}

impl LoadedEffects {
    /// Note that effect `id` is about to be played. Returns `true` the first
    /// time an id is seen (meaning "load it now"), `false` thereafter.
    pub fn needs_load(&mut self, id: u32) -> bool {
        self.loaded.insert(id)
    }

    /// Whether effect `id` has been marked loaded.
    pub fn is_loaded(&self, id: u32) -> bool {
        self.loaded.contains(&id)
    }

    /// Forget all loaded effects (e.g. after re-initialising the soundbank).
    pub fn clear(&mut self) {
        self.loaded.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_volume_spans_the_range() {
        assert_eq!(effect_volume(0.0), 0);
        assert_eq!(effect_volume(1.0), 255);
        assert_eq!(effect_volume(0.5), 128); // 127.5 rounds up
    }

    #[test]
    fn module_volume_spans_the_range() {
        assert_eq!(module_volume(0.0), 0);
        assert_eq!(module_volume(1.0), 1024);
        assert_eq!(module_volume(0.5), 512);
    }

    #[test]
    fn panning_centres_at_half() {
        assert_eq!(panning(0.0), 0);
        assert_eq!(panning(1.0), 255);
        assert_eq!(panning(0.5), 128);
    }

    #[test]
    fn out_of_range_is_clamped() {
        assert_eq!(effect_volume(-1.0), 0);
        assert_eq!(effect_volume(2.0), 255);
        assert_eq!(module_volume(-0.5), 0);
        assert_eq!(module_volume(9.0), 1024);
        assert_eq!(panning(-3.0), 0);
        assert_eq!(panning(4.0), 255);
    }

    #[test]
    fn effects_load_exactly_once() {
        let mut loaded = LoadedEffects::default();
        assert!(loaded.needs_load(7), "first sight should request a load");
        assert!(!loaded.needs_load(7), "second sight should not");
        assert!(loaded.is_loaded(7));
        assert!(!loaded.is_loaded(8));
        assert!(loaded.needs_load(8));
    }

    #[test]
    fn clear_forgets_loaded_effects() {
        let mut loaded = LoadedEffects::default();
        loaded.needs_load(1);
        loaded.clear();
        assert!(!loaded.is_loaded(1));
        assert!(loaded.needs_load(1), "after clear, load is requested again");
    }
}
