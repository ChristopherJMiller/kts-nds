//! The device + item radial wheel (issue #25) — the deploy/retract affordance
//! that replaces the shoulder-toggle stand-in the controller shipped with.
//!
//! Locked model (#25 / #17): **hold the non-stylus shoulder → the pen is gated
//! out of locomotion into radial-select** (its position at the press becomes the
//! wheel origin, reusing the virtual-stick's relative-origin idiom); a quick
//! directional drag/flick picks the nearest of **5 spokes** (a point-up pentagon,
//! so the top spoke is always the capture **device**); **release commits**. No
//! drag past the deadzone = **cancel**; a quick shoulder **tap** (no hold, no
//! drag) = **instant retract** (the panic bail).
//!
//! The shoulder-hold *modality* is what keeps this from colliding with
//! movement/draw — you never move and select in the same instant — so the pen
//! serves both locomotion and radial-select rather than adding a new verb
//! (pillars 1 + 3). Item spokes (the other four) are a **stubbed seam** here
//! until the item economy (#30); this first cut is device-only.
//!
//! The spoke geometry is the pure, host-tested [`bevy_nds_math::radial`] helper
//! (sibling of the virtual stick); this module is the game-specific half — the
//! [`Spoke`]→verb mapping and the state machine ([`drive_radial`]) that reads
//! `Touches` + the handedness-mirrored shoulder and drives [`PlayerState`]
//! (+ clears the in-flight [`Stroke`]).

use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_math::{Fx32, FxVec2, radial::nearest_spoke};

use crate::Stroke;
use crate::player::PlayerState;

/// Minimum stylus drag (screen px) from the wheel origin before a spoke
/// registers. A touch above the stow stick's 8 px deadzone, so opening the wheel
/// and committing a spoke is a deliberate flick, not stick jitter.
const RADIAL_DEADZONE: f32 = 12.0;

/// A shoulder press released within this many frames **with no drag** is a *tap*
/// (instant retract), not a held-open-then-cancelled wheel. ~⅙ s at 60 Hz.
const TAP_FRAMES: u8 = 10;

/// A radial spoke. Five spokes, a **point-up regular pentagon**: [`Spoke::Device`]
/// is the top vertex (guaranteeing the locked "device = up", #17/#25); the other
/// four are `Item(1..=4)` clockwise from the top — stubbed until the item economy
/// (#30).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoke {
    /// Top spoke — throw out the capture device (deploy).
    Device,
    /// Item slots 1..=4, clockwise from the top. Stubbed (#30).
    Item(u8),
}

impl Spoke {
    /// Map a [`bevy_nds_math::radial`] spoke index (0 = up) onto a game verb.
    fn from_index(i: u8) -> Self {
        match i {
            0 => Spoke::Device,
            n => Spoke::Item(n),
        }
    }

    /// The wheel index of this spoke (0 = device/up) — the inverse of
    /// [`Spoke::from_index`], for matching a spoke against the drawn wheel.
    pub fn index(self) -> u8 {
        match self {
            Spoke::Device => 0,
            Spoke::Item(n) => n,
        }
    }

    /// Short HUD label for the previewed spoke.
    pub fn label(self) -> &'static str {
        match self {
            Spoke::Device => "device",
            Spoke::Item(1) => "item 1",
            Spoke::Item(2) => "item 2",
            Spoke::Item(3) => "item 3",
            _ => "item 4",
        }
    }
}

/// Map a drag `offset` (stylus position minus the wheel origin, screen px) to the
/// selected [`Spoke`], or `None` inside the deadzone (a release there cancels).
/// The geometry is [`bevy_nds_math::radial::nearest_spoke`]; this only maps its
/// index onto the game's device/item verbs.
fn select_spoke(offset: FxVec2, deadzone: Fx32) -> Option<Spoke> {
    nearest_spoke(offset, deadzone).map(Spoke::from_index)
}

/// Radial-wheel runtime state — the shoulder-hold gesture in flight.
#[derive(Resource, Default)]
pub struct Radial {
    /// Was the shoulder held last frame? (Edge-detects the release that commits.)
    holding: bool,
    /// Frames the shoulder has been held this press — tap-vs-hold classification.
    held_frames: u8,
    /// Wheel origin: the stylus position when the wheel opened, or the first
    /// touch seen while holding. `None` until a touch anchors it.
    pub origin: Option<FxVec2>,
    /// Was the stylus down last frame? Edge-detects the *stylus* release, which
    /// commits the selected spoke without waiting for the shoulder (#25 nip:
    /// either release activates — whichever comes first).
    had_touch: bool,
    /// Already resolved this hold (via a stylus release) — latched so the later
    /// shoulder release doesn't re-fire; cleared when the shoulder lets go.
    committed: bool,
    /// The spoke currently under the pen — previewed live, committed on release.
    /// `None` = inside the deadzone (releasing here cancels).
    pub preview: Option<Spoke>,
    /// True while the wheel is up (shoulder held, not yet resolved). Locomotion /
    /// draw read this to gate the pen out of the virtual stick and the capture
    /// stroke (no move-or-draw while selecting), and the overlay draws while set.
    pub open: bool,
}

/// Drive the radial wheel: hold the (handedness-mirrored) primary shoulder to
/// open it and quick-drag the pen to a spoke. **Either release commits** —
/// lifting the stylus off a spoke activates it, or releasing the shoulder does
/// (whichever comes first, #25 nip). Device spoke = deploy; a bare shoulder tap
/// = instant retract; a drag-less hold released = cancel. Replaces the
/// shoulder-toggle stand-in (`player::transition_state`).
pub fn drive_radial(
    input: Res<ButtonInput<DsButton>>,
    handed: Res<Handedness>,
    touches: Res<Touches>,
    mut radial: ResMut<Radial>,
    mut state: ResMut<PlayerState>,
    mut stroke: ResMut<Stroke>,
) {
    let shoulder = Shoulder::Primary.button(*handed);
    let held = input.pressed(shoulder);
    let touch = touches
        .iter()
        .next()
        .map(|t| FxVec2::from_f32(t.position().x, t.position().y));

    if !held {
        // Shoulder released: resolve the gesture, unless a stylus release already
        // committed it this hold. Then reset for the next press.
        if radial.holding && !radial.committed {
            commit_shoulder(&radial, &mut state, &mut stroke);
        }
        *radial = Radial::default();
        return;
    }

    radial.holding = true;
    // Already resolved this hold (a stylus release) — keep the wheel closed until
    // the shoulder lets go, so a second lift/press can't re-fire.
    if radial.committed {
        radial.open = false;
        radial.preview = None;
        radial.had_touch = touch.is_some();
        return;
    }

    radial.held_frames = radial.held_frames.saturating_add(1);
    let lifted = radial.had_touch && touch.is_none();
    if let Some(cur) = touch {
        // Anchor the origin to the pen the first time one is down, then preview
        // the spoke the current drag points at.
        let origin = *radial.origin.get_or_insert(cur);
        radial.preview = select_spoke(cur - origin, Fx32::from_f32(RADIAL_DEADZONE));
    }

    // Stylus release on a spoke commits it immediately (no wait for the
    // shoulder). `preview` still holds last frame's spoke — `touch` is `None`
    // now. A lift at centre (no spoke) just keeps the wheel open.
    if lifted {
        if let Some(spoke) = radial.preview {
            commit_spoke(spoke, &mut state, &mut stroke);
            radial.committed = true;
            radial.open = false;
            radial.preview = None;
            radial.had_touch = false;
            return;
        }
        radial.preview = None;
    }

    radial.open = true;
    radial.had_touch = touch.is_some();
}

/// Resolve a **shoulder** release into a state change:
/// - quick tap (short hold) → **instant retract** (panic bail),
/// - held open + dragged to a spoke → commit it,
/// - held open, no drag → **cancel** (no state change).
fn commit_shoulder(radial: &Radial, state: &mut PlayerState, stroke: &mut Stroke) {
    // The tap is classified on the *shoulder's* hold time, **not** on whether the
    // pen moved: a bail while drawing must fire even though the drawing pen has
    // drifted past the deadzone (which would otherwise read as a spoke). A quick
    // tap always retracts; you have to hold the wheel open to select from it.
    if radial.held_frames <= TAP_FRAMES {
        if *state != PlayerState::Stowed {
            *state = PlayerState::Stowed;
            stroke.0.clear();
        }
        return;
    }
    // Held open long enough to be a deliberate wheel — the drag picks the spoke.
    // No drag past the deadzone (`preview` is `None`) = cancel.
    if let Some(spoke) = radial.preview {
        commit_spoke(spoke, state, stroke);
    }
}

/// Apply a selected spoke: device = deploy (items are a stubbed seam, #30).
/// Shared by both commit paths (shoulder release and stylus release).
fn commit_spoke(spoke: Spoke, state: &mut PlayerState, stroke: &mut Stroke) {
    match spoke {
        Spoke::Device => {
            // "Throw it out" — ensure deployed. A fresh stroke starts clean.
            if *state != PlayerState::Deployed {
                *state = PlayerState::Deployed;
                stroke.0.clear();
            }
        }
        // Item spokes are a stubbed seam until the item economy (#30): no-op.
        Spoke::Item(_) => {}
    }
}
