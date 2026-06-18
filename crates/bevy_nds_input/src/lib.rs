//! Nintendo DS buttons & touch, surfaced through Bevy's standard input
//! abstractions.
//!
//! Rather than inventing a bespoke input resource, we reuse [`ButtonInput`]
//! (the same type Bevy uses for keyboards, mice and gamepads) for buttons, and
//! Bevy's standard [`Touches`] / [`TouchInput`] pipeline for the touch screen.
//! Game code reads `Res<ButtonInput<DsButton>>` and `Res<Touches>` and gets the
//! usual `pressed` / `just_pressed` / `iter()` API for free.

#![cfg_attr(not(test), no_std)]

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_input::ButtonInput;
use bevy_input::touch::{TouchInput, TouchPhase, Touches, touch_screen_input_system};
use bevy_math::Vec2;

// libnds key bit masks (see <nds/input.h>).
const KEY_A: u32 = 1 << 0;
const KEY_B: u32 = 1 << 1;
const KEY_SELECT: u32 = 1 << 2;
const KEY_START: u32 = 1 << 3;
const KEY_RIGHT: u32 = 1 << 4;
const KEY_LEFT: u32 = 1 << 5;
const KEY_UP: u32 = 1 << 6;
const KEY_DOWN: u32 = 1 << 7;
const KEY_R: u32 = 1 << 8;
const KEY_L: u32 = 1 << 9;
const KEY_X: u32 = 1 << 10;
const KEY_Y: u32 = 1 << 11;
/// Touchscreen pen-down. Set in `keysHeld()` while the screen is being pressed;
/// `touchRead` only returns useful data when this bit is set.
const KEY_TOUCH: u32 = 1 << 12;

/// Touch-screen reading, calibrated by libnds from the firmware (see
/// `<nds/touch.h>`). Only `px` / `py` (pixel coordinates, 0..=255 by 0..=191)
/// are meaningful for normal use; the raw and resistance fields are kept to
/// match the C struct layout exactly so `touchRead` writes the right offsets.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
#[allow(non_camel_case_types)]
struct touchPosition {
    rawx: u16,
    rawy: u16,
    px: u16,
    py: u16,
    z1: u16,
    z2: u16,
}

unsafe extern "C" {
    /// Latch the current button state; call once per frame before reading keys.
    fn scanKeys();
    /// Buttons currently held down (bitfield of `KEY_*`).
    fn keysHeld() -> u32;
    /// Read the calibrated touch-screen position into `pos`. Only produces
    /// useful data when `keysHeld()` reports `KEY_TOUCH`. See `<nds/touch.h>`.
    fn touchRead(pos: *mut touchPosition) -> u32;
}

/// A button on the Nintendo DS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DsButton {
    A,
    B,
    X,
    Y,
    L,
    R,
    Start,
    Select,
    Up,
    Down,
    Left,
    Right,
}

/// Which hand holds the stylus. The stylus is the precision instrument and
/// always sits in the dominant hand (pillar 1); the **other** hand works the
/// face cluster + a shoulder, so the cluster mirrors between the d-pad
/// (right-handed) and the ABXY diamond (left-handed), and the two shoulders
/// swap. See the control model in issue #17. Set this resource from the game's
/// handedness setting; defaults to [`Handedness::Right`].
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Handedness {
    /// Stylus in the right hand → the **left** hand works the d-pad cluster and
    /// the L shoulder.
    #[default]
    Right,
    /// Stylus in the left hand → the **right** hand works the ABXY cluster and
    /// the R shoulder.
    Left,
}

/// A logical direction on the four-button face *cluster* (the diamond), free of
/// any specific physical button. Resolve it to a [`DsButton`] for the current
/// [`Handedness`] with [`Cluster::button`]: right-handed it is the d-pad,
/// left-handed it mirrors onto the ABXY diamond by position (Up↔X, Down↔B,
/// Left↔Y, Right↔A). This is how a binding stays hand-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cluster {
    Up,
    Down,
    Left,
    Right,
}

/// One of the two shoulder buttons, named by *role* rather than side so it
/// mirrors with handedness. [`Shoulder::Primary`] is the shoulder under the
/// non-stylus hand (the capture-device / radial home — L when right-handed, R
/// when left-handed); [`Shoulder::Secondary`] is the other (the reserve).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shoulder {
    Primary,
    Secondary,
}

impl Cluster {
    /// The physical [`DsButton`] this cluster direction maps to for `handedness`.
    pub fn button(self, handedness: Handedness) -> DsButton {
        match handedness {
            // Right-handed: the cluster is the d-pad.
            Handedness::Right => match self {
                Cluster::Up => DsButton::Up,
                Cluster::Down => DsButton::Down,
                Cluster::Left => DsButton::Left,
                Cluster::Right => DsButton::Right,
            },
            // Left-handed: mirror onto the ABXY diamond by screen position
            // (X top, B bottom, Y left, A right).
            Handedness::Left => match self {
                Cluster::Up => DsButton::X,
                Cluster::Down => DsButton::B,
                Cluster::Left => DsButton::Y,
                Cluster::Right => DsButton::A,
            },
        }
    }
}

impl Shoulder {
    /// The physical [`DsButton`] this shoulder role maps to for `handedness`.
    /// The non-stylus hand's shoulder is `Primary`, so the two swap L↔R.
    pub fn button(self, handedness: Handedness) -> DsButton {
        match (self, handedness) {
            (Shoulder::Primary, Handedness::Right) => DsButton::L,
            (Shoulder::Secondary, Handedness::Right) => DsButton::R,
            (Shoulder::Primary, Handedness::Left) => DsButton::R,
            (Shoulder::Secondary, Handedness::Left) => DsButton::L,
        }
    }
}

impl DsButton {
    /// Every button paired with its libnds key mask.
    const ALL: [(DsButton, u32); 12] = [
        (DsButton::A, KEY_A),
        (DsButton::B, KEY_B),
        (DsButton::X, KEY_X),
        (DsButton::Y, KEY_Y),
        (DsButton::L, KEY_L),
        (DsButton::R, KEY_R),
        (DsButton::Start, KEY_START),
        (DsButton::Select, KEY_SELECT),
        (DsButton::Up, KEY_UP),
        (DsButton::Down, KEY_DOWN),
        (DsButton::Left, KEY_LEFT),
        (DsButton::Right, KEY_RIGHT),
    ];
}

/// Latches the hardware key state into the [`ButtonInput`] resource each frame,
/// driving its pressed / just-pressed / just-released bookkeeping.
fn read_keys(mut buttons: ResMut<ButtonInput<DsButton>>) {
    // Clear last frame's "just" transitions, then re-derive press state.
    buttons.clear();

    let held = unsafe {
        scanKeys();
        keysHeld()
    };

    for (button, mask) in DsButton::ALL {
        if held & mask != 0 {
            buttons.press(button);
        } else {
            buttons.release(button);
        }
    }
}

/// Exposes the DS buttons + touch screen through Bevy's standard input
/// resources: `ButtonInput<DsButton>` and `Touches`.
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ButtonInput<DsButton>>()
            .init_resource::<Touches>()
            .init_resource::<Handedness>()
            .add_event::<TouchInput>()
            // `scanKeys` (in `read_keys`) must latch the hardware before
            // `read_touch` inspects `KEY_TOUCH`, and our raw `TouchInput` events
            // must be written before Bevy folds them into `Touches`.
            .add_systems(
                PreUpdate,
                (read_keys, read_touch, touch_screen_input_system).chain(),
            );
    }
}

/// libnds treats the touch screen as a single pointer, so we model it as one
/// Bevy touch with a fixed id.
const TOUCH_ID: u64 = 0;

/// Translate the previous and current pen state into the touch event (if any) to
/// emit, plus the position to remember for next frame.
///
/// This is the pure, host-testable half of [`read_touch`]: `prev` is the last
/// position while the pen was down (or `None` if it was up), `current` is this
/// frame's reading (or `None` if the pen is up now). A press emits
/// [`TouchPhase::Started`], a real move emits [`TouchPhase::Moved`], and a
/// release emits [`TouchPhase::Ended`] at the *last* known position (the
/// hardware reports nothing once the pen leaves the screen).
fn diff_touch(
    prev: Option<Vec2>,
    current: Option<Vec2>,
) -> (Option<(TouchPhase, Vec2)>, Option<Vec2>) {
    match (prev, current) {
        (None, Some(pos)) => (Some((TouchPhase::Started, pos)), Some(pos)),
        (Some(prev_pos), Some(pos)) if pos != prev_pos => {
            (Some((TouchPhase::Moved, pos)), Some(pos))
        }
        (Some(_), Some(pos)) => (None, Some(pos)),
        (Some(prev_pos), None) => (Some((TouchPhase::Ended, prev_pos)), None),
        (None, None) => (None, None),
    }
}

/// Reads the touch screen each frame and feeds Bevy's standard touch pipeline by
/// writing [`TouchInput`] events; [`touch_screen_input_system`] turns those into
/// the [`Touches`] resource that game code reads. The previous pen position is
/// kept in a `Local` so we can derive started / moved / ended transitions.
fn read_touch(mut prev: Local<Option<Vec2>>, mut events: EventWriter<TouchInput>) {
    // `scanKeys` was already called this frame by `read_keys` (the systems are
    // chained), so the held-key state is current.
    let current = (unsafe { keysHeld() } & KEY_TOUCH != 0).then(|| {
        let mut pos = touchPosition::default();
        // SAFETY: `pos` is a valid, writable `touchPosition`; libnds only fills
        // in calibrated data because `KEY_TOUCH` is held.
        unsafe { touchRead(&mut pos) };
        Vec2::new(pos.px as f32, pos.py as f32)
    });

    let (event, next) = diff_touch(*prev, current);
    *prev = next;

    if let Some((phase, position)) = event {
        events.write(TouchInput {
            phase,
            position,
            window: Entity::PLACEHOLDER,
            force: None,
            id: TOUCH_ID,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_button_is_mapped_exactly_once() {
        // All 12 variants appear, with no duplicate buttons.
        assert_eq!(DsButton::ALL.len(), 12);
        for i in 0..DsButton::ALL.len() {
            for j in (i + 1)..DsButton::ALL.len() {
                assert_ne!(DsButton::ALL[i].0, DsButton::ALL[j].0);
            }
        }
    }

    #[test]
    fn key_masks_are_single_distinct_bits() {
        let mut seen = 0u32;
        for (_, mask) in DsButton::ALL {
            assert!(mask != 0, "mask must be non-zero");
            assert_eq!(mask & (mask - 1), 0, "mask must be a single bit");
            assert_eq!(seen & mask, 0, "masks must be disjoint");
            seen |= mask;
        }
    }

    #[test]
    fn handedness_defaults_to_right() {
        assert_eq!(Handedness::default(), Handedness::Right);
    }

    #[test]
    fn cluster_is_dpad_when_right_handed() {
        let h = Handedness::Right;
        assert_eq!(Cluster::Up.button(h), DsButton::Up);
        assert_eq!(Cluster::Down.button(h), DsButton::Down);
        assert_eq!(Cluster::Left.button(h), DsButton::Left);
        assert_eq!(Cluster::Right.button(h), DsButton::Right);
    }

    #[test]
    fn cluster_mirrors_to_abxy_diamond_when_left_handed() {
        // By screen position: X top, B bottom, Y left, A right.
        let h = Handedness::Left;
        assert_eq!(Cluster::Up.button(h), DsButton::X);
        assert_eq!(Cluster::Down.button(h), DsButton::B);
        assert_eq!(Cluster::Left.button(h), DsButton::Y);
        assert_eq!(Cluster::Right.button(h), DsButton::A);
    }

    #[test]
    fn shoulders_swap_with_handedness() {
        // Primary = non-stylus hand's shoulder.
        assert_eq!(Shoulder::Primary.button(Handedness::Right), DsButton::L);
        assert_eq!(Shoulder::Secondary.button(Handedness::Right), DsButton::R);
        assert_eq!(Shoulder::Primary.button(Handedness::Left), DsButton::R);
        assert_eq!(Shoulder::Secondary.button(Handedness::Left), DsButton::L);
    }

    #[test]
    fn mirror_is_a_bijection_per_handedness() {
        // The four cluster directions must map to four distinct buttons (no two
        // logical directions collide) under each handedness.
        for h in [Handedness::Right, Handedness::Left] {
            let mapped = [
                Cluster::Up.button(h),
                Cluster::Down.button(h),
                Cluster::Left.button(h),
                Cluster::Right.button(h),
            ];
            for i in 0..4 {
                for j in (i + 1)..4 {
                    assert_ne!(mapped[i], mapped[j], "collision under {h:?}");
                }
            }
        }
    }

    #[test]
    fn directional_masks_match_libnds() {
        let mask = |b: DsButton| DsButton::ALL.iter().find(|(x, _)| *x == b).unwrap().1;
        assert_eq!(mask(DsButton::Left), KEY_LEFT);
        assert_eq!(mask(DsButton::Right), KEY_RIGHT);
        assert_eq!(mask(DsButton::Up), KEY_UP);
        assert_eq!(mask(DsButton::Down), KEY_DOWN);
    }

    #[test]
    fn touch_down_from_idle_starts() {
        let here = Vec2::new(40.0, 90.0);
        let (event, next) = diff_touch(None, Some(here));
        assert_eq!(event, Some((TouchPhase::Started, here)));
        assert_eq!(next, Some(here));
    }

    #[test]
    fn touch_move_while_held_reports_new_position() {
        let from = Vec2::new(40.0, 90.0);
        let to = Vec2::new(41.0, 92.0);
        let (event, next) = diff_touch(Some(from), Some(to));
        assert_eq!(event, Some((TouchPhase::Moved, to)));
        assert_eq!(next, Some(to));
    }

    #[test]
    fn touch_held_still_emits_nothing() {
        let here = Vec2::new(40.0, 90.0);
        let (event, next) = diff_touch(Some(here), Some(here));
        assert_eq!(event, None);
        assert_eq!(next, Some(here));
    }

    #[test]
    fn touch_release_ends_at_last_position() {
        let last = Vec2::new(40.0, 90.0);
        let (event, next) = diff_touch(Some(last), None);
        assert_eq!(event, Some((TouchPhase::Ended, last)));
        assert_eq!(next, None);
    }

    #[test]
    fn touch_idle_stays_idle() {
        let (event, next) = diff_touch(None, None);
        assert_eq!(event, None);
        assert_eq!(next, None);
    }
}
