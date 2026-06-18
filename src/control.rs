//! Logical control actions for Kill the Serpent (#21), mapped onto physical DS
//! inputs through the handedness-aware mirror in `bevy_nds_input`.
//!
//! The control model is locked in issue #17: the stylus is locomotion/draw, the
//! four-button cluster carries the action verbs (mirrored d-pad↔ABXY by
//! handedness), and the two shoulders carry the device radial + a reserve. This
//! module is the game-specific *policy* — which verb sits on which slot; the
//! reusable handedness *mechanism* (and its host tests) lives in the library.
//!
//! Consumed by the player-controller state machine (#24).
#![allow(dead_code)] // bindings declared here; wired up by the #24 controller

use bevy_nds::prelude::*;

/// A logical player action. `Move` is the stylus (locomotion while stowed, draw
/// while deployed); `Dash` is *derived* from a double-tap of `Jump` and has no
/// button of its own. The rest sit on a cluster direction or a shoulder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Move,
    Jump,
    Dash,
    Roll,
    CamTopDown,
    CamOrbit,
    DeviceRadial,
    LockOn,
}

/// Where an [`Action`] lives physically, *before* handedness is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// The stylus / touch screen (hand-agnostic — never mirrored).
    Stylus,
    /// A face-cluster direction (mirrors d-pad↔ABXY with handedness).
    Cluster(Cluster),
    /// A shoulder role (mirrors L↔R with handedness).
    Shoulder(Shoulder),
    /// Derived input with no button of its own (e.g. `Dash` = double-tap `Jump`).
    Derived,
}

impl Action {
    /// The control-model binding for this action (issue #17, `## Locked`).
    pub const fn binding(self) -> Binding {
        match self {
            Action::Move => Binding::Stylus,
            Action::Jump => Binding::Cluster(Cluster::Right),
            Action::Roll => Binding::Cluster(Cluster::Down),
            Action::CamTopDown => Binding::Cluster(Cluster::Up),
            Action::CamOrbit => Binding::Cluster(Cluster::Left),
            Action::DeviceRadial => Binding::Shoulder(Shoulder::Primary),
            Action::LockOn => Binding::Shoulder(Shoulder::Secondary),
            Action::Dash => Binding::Derived,
        }
    }

    /// The physical button this action reads for `handedness` — or `None` for
    /// the stylus / derived actions (which aren't a single button: read `Move`
    /// via `Touches`, `Dash` via the double-tap detector in the controller).
    pub fn button(self, handedness: Handedness) -> Option<DsButton> {
        match self.binding() {
            Binding::Cluster(c) => Some(c.button(handedness)),
            Binding::Shoulder(s) => Some(s.button(handedness)),
            Binding::Stylus | Binding::Derived => None,
        }
    }
}

/// Is `action` currently held, for the active handedness? Stylus / derived
/// actions report `false` here (read them via `Touches` / the dash detector).
pub fn pressed(action: Action, handedness: Handedness, buttons: &ButtonInput<DsButton>) -> bool {
    action.button(handedness).is_some_and(|b| buttons.pressed(b))
}

/// Did `action` go down this frame?
pub fn just_pressed(
    action: Action,
    handedness: Handedness,
    buttons: &ButtonInput<DsButton>,
) -> bool {
    action
        .button(handedness)
        .is_some_and(|b| buttons.just_pressed(b))
}
