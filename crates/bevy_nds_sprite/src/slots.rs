//! Pure OAM-slot bookkeeping, split out of the FFI so it can be unit-tested
//! on the host. The DS sprite engine has 128 OAM entries per engine; this
//! resource is a simple bitmask tracking which are free.

use bevy_ecs::prelude::Resource;

/// Number of hardware OAM entries on a DS 2D engine.
pub const MAX_SPRITES: usize = 128;

/// Tracks which OAM slots are currently in use. Backed by a 128-bit bitmask
/// (two `u64`s); `allocate` is a one-shot scan for the lowest free slot.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteSlots {
    used: [u64; 2],
}

impl SpriteSlots {
    /// Reserve and return the lowest free slot id, or `None` if all 128 are
    /// taken.
    pub fn allocate(&mut self) -> Option<u8> {
        for (word, bits) in self.used.iter_mut().enumerate() {
            let free = !*bits;
            if free != 0 {
                let bit = free.trailing_zeros() as u8;
                *bits |= 1 << bit;
                return Some(word as u8 * 64 + bit);
            }
        }
        None
    }

    /// Mark `id` as free. No-op for invalid ids or already-free slots.
    pub fn release(&mut self, id: u8) {
        if (id as usize) >= MAX_SPRITES {
            return;
        }
        let word = (id / 64) as usize;
        let bit = id % 64;
        self.used[word] &= !(1u64 << bit);
    }

    /// Whether `id` is currently allocated.
    pub fn is_used(&self, id: u8) -> bool {
        if (id as usize) >= MAX_SPRITES {
            return false;
        }
        let word = (id / 64) as usize;
        let bit = id % 64;
        self.used[word] & (1u64 << bit) != 0
    }

    /// Number of slots currently allocated.
    pub fn count(&self) -> u32 {
        self.used[0].count_ones() + self.used[1].count_ones()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_starts_from_zero_and_climbs() {
        let mut s = SpriteSlots::default();
        assert_eq!(s.allocate(), Some(0));
        assert_eq!(s.allocate(), Some(1));
        assert_eq!(s.allocate(), Some(2));
        assert_eq!(s.count(), 3);
    }

    #[test]
    fn release_returns_the_lowest_slot_next() {
        let mut s = SpriteSlots::default();
        for _ in 0..5 {
            s.allocate();
        }
        s.release(2);
        // The next allocation should reuse slot 2 (lowest free).
        assert_eq!(s.allocate(), Some(2));
    }

    #[test]
    fn allocate_runs_out_at_max_sprites() {
        let mut s = SpriteSlots::default();
        for i in 0..MAX_SPRITES {
            assert_eq!(s.allocate(), Some(i as u8));
        }
        assert_eq!(s.count() as usize, MAX_SPRITES);
        assert_eq!(s.allocate(), None);
    }

    #[test]
    fn allocation_crosses_word_boundary() {
        let mut s = SpriteSlots::default();
        // Fill the first u64.
        for _ in 0..64 {
            s.allocate();
        }
        // The next allocation must come from the second u64.
        assert_eq!(s.allocate(), Some(64));
        assert!(s.is_used(64));
        assert!(!s.is_used(65));
    }

    #[test]
    fn release_of_unknown_or_oob_is_a_noop() {
        let mut s = SpriteSlots::default();
        s.release(0); // never allocated
        s.release(200); // out of range
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn is_used_tracks_allocations() {
        let mut s = SpriteSlots::default();
        let a = s.allocate().unwrap();
        let b = s.allocate().unwrap();
        assert!(s.is_used(a));
        assert!(s.is_used(b));
        s.release(a);
        assert!(!s.is_used(a));
        assert!(s.is_used(b));
    }

    #[test]
    fn is_used_rejects_out_of_range() {
        let s = SpriteSlots::default();
        assert!(!s.is_used(200));
    }
}
