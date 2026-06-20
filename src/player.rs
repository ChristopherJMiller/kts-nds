//! Player controller state machine (#24).
//!
//! The real controller, promoted out of the Spike C harness: a `Stowed ↔
//! Deployed` state machine that wires the stylus + cluster verbs into one
//! moment-to-moment loop (pillars 1 & 2). All input flows through the #21 action
//! layer ([`crate::control`] + the [`Handedness`] resource), never raw buttons.
//!
//! - **Stowed:** stylus = virtual-stick locomotion (Spike A); cluster = `Jump`
//!   (single = hop / double-tap = `Dash`), `Roll` (i-frame dodge), and the
//!   camera toggles (bound but inert until #23).
//! - **Deployed:** stylus = draw (Spike B, in `main`); cluster = directional
//!   dodge-steps + double-tap roll. Jump/dash are disabled — the pen is out.
//!
//! A real jump/height model lives here too: the avatar carries a [`Height`] (the
//! world "up" axis, separate from the ground `WorldPos`) integrated under
//! gravity, rendered as a screen-Y lift with a ground [`Shadow`]. The vertical
//! read is provisional — it lands better once a side-ish corridor camera exists
//! (#23). Movement is tuned per [`Locomotion`] preset (Arena / Corridor); #27
//! will pick the preset per space.

use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_math::stick::{StickConfig, smooth as vel_smooth, stick_vector};

use crate::control::{self, Action};
use crate::{ARENA_HALF, Avatar, LANDMARK_COLLIDE, LANDMARKS, Stroke, WorldPos};

// --- Stylus conditioning (Spike A defaults, locked 2026-06-14) ---------------

const STOW_DEADZONE: f32 = 8.0;
const STOW_MAX_RADIUS: f32 = 70.0;
const STOW_SMOOTH: f32 = 0.5;
/// Frames a tap stays "armed" for a double-tap (roll / dash trigger).
const DOUBLE_TAP_WINDOW: u8 = 12;

// --- Controller state --------------------------------------------------------

/// The controller's top-level state. `Deployed` means the capture device is out
/// (stylus draws, cluster dodges); `Stowed` is free traversal.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    #[default]
    Stowed,
    Deployed,
}

impl PlayerState {
    pub fn is_deployed(self) -> bool {
        self == PlayerState::Deployed
    }
}

/// Avatar height above the ground plane — the jump axis, separate from the
/// `WorldPos` ground XY. Integrated under gravity each frame.
#[derive(Component, Default)]
pub struct Height {
    pub z: Fx32,
    pub vz: Fx32,
    pub grounded: bool,
}

/// Marker for the flat ground shadow that tracks the avatar's ground position
/// (so a jump's screen-Y lift reads as height, not ground movement).
#[derive(Component)]
pub struct Shadow;

/// Virtual-stick bookkeeping for stowed stylus locomotion (Spike A).
#[derive(Resource, Default)]
pub struct StickState {
    origin: FxVec2,
    vel: FxVec2,
    active: bool,
}

/// Transient roll / dash / double-tap state.
#[derive(Resource)]
pub struct Motion {
    /// Frames left in an evasive burst (roll or dash); also the i-frame window
    /// when `invuln`.
    burst: u8,
    burst_dir: FxVec2,
    burst_speed: Fx32,
    /// Whether the active burst grants invulnerability (roll yes, dash no).
    invuln: bool,
    /// Deployed per-direction double-tap windows, indexed [Left, Right, Up, Down].
    step_tap: [u8; 4],
    /// Stowed `Jump` double-tap window (a second press inside it dashes).
    jump_tap: u8,
    /// Last non-zero horizontal heading, for direction-less rolls / dashes.
    last_dir: FxVec2,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            burst: 0,
            burst_dir: FxVec2::ZERO,
            burst_speed: Fx32::ZERO,
            invuln: false,
            step_tap: [0; 4],
            jump_tap: 0,
            last_dir: FxVec2::new(Fx32::ZERO, Fx32::NEG_ONE),
        }
    }
}

impl Motion {
    /// True while an evasive roll's i-frames are active (read by hit checks).
    pub fn invulnerable(&self) -> bool {
        self.burst > 0 && self.invuln
    }
}

/// Which authored space the avatar is in — selects the movement feel. No space
/// system yet (#27), so this is toggled by a debug key for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceKind {
    Arena,
    Corridor,
}

/// Movement tuning. Two presets: open `Arena` (default) and tighter `Corridor`
/// (2.5D platforming). Values are provisional until #23/#27 give real spaces.
#[derive(Resource, Clone, Copy)]
pub struct Locomotion {
    pub kind: SpaceKind,
    pub stow_speed: Fx32,
    pub dodge_speed: Fx32,
    pub roll_speed: Fx32,
    pub dash_speed: Fx32,
    pub roll_frames: u8,
    pub jump_impulse: Fx32,
    pub gravity: Fx32,
}

impl Locomotion {
    fn arena() -> Self {
        Self {
            kind: SpaceKind::Arena,
            stow_speed: Fx32::from_f32(1.6),
            dodge_speed: Fx32::from_f32(0.4),
            roll_speed: Fx32::from_f32(3.8),
            dash_speed: Fx32::from_f32(3.4),
            roll_frames: 10,
            jump_impulse: Fx32::from_f32(2.2),
            gravity: Fx32::from_f32(9.0),
        }
    }

    fn corridor() -> Self {
        Self {
            kind: SpaceKind::Corridor,
            stow_speed: Fx32::from_f32(1.25),
            dodge_speed: Fx32::from_f32(0.32),
            roll_speed: Fx32::from_f32(3.2),
            dash_speed: Fx32::from_f32(2.9),
            roll_frames: 9,
            jump_impulse: Fx32::from_f32(2.0),
            gravity: Fx32::from_f32(10.0),
        }
    }
}

impl Default for Locomotion {
    fn default() -> Self {
        Self::arena()
    }
}

// --- Systems -----------------------------------------------------------------

/// Toggle the capture device (`Action::DeviceRadial` — the shoulder, a stand-in
/// for the #25 radial). Any transition drops the in-flight stroke.
pub fn transition_state(
    input: Res<ButtonInput<DsButton>>,
    handed: Res<Handedness>,
    mut state: ResMut<PlayerState>,
    mut stroke: ResMut<Stroke>,
) {
    if control::just_pressed(Action::DeviceRadial, *handed, &input) {
        *state = match *state {
            PlayerState::Stowed => PlayerState::Deployed,
            PlayerState::Deployed => PlayerState::Stowed,
        };
        stroke.0.clear();
    }
}

/// Debug: cycle the movement tuning preset (Arena ↔ Corridor) on `Select`,
/// until #27 assigns it per-space.
pub fn toggle_tuning(input: Res<ButtonInput<DsButton>>, mut loco: ResMut<Locomotion>) {
    if input.just_pressed(DsButton::Select) {
        *loco = match loco.kind {
            SpaceKind::Arena => Locomotion::corridor(),
            SpaceKind::Corridor => Locomotion::arena(),
        };
    }
}

/// The core controller: produce this frame's horizontal move (stowed stylus /
/// deployed dodge / evasive burst), integrate the jump/height model, then apply
/// the result to the avatar's [`WorldPos`] + [`Height`] with arena clamp and
/// landmark push-out (the same collision the spike used).
pub fn move_player(
    time: Res<Time>,
    touches: Res<Touches>,
    input: Res<ButtonInput<DsButton>>,
    handed: Res<Handedness>,
    state: Res<PlayerState>,
    loco: Res<Locomotion>,
    mut stick: ResMut<StickState>,
    mut motion: ResMut<Motion>,
    mut q: Query<(&mut WorldPos, &mut Height), With<Avatar>>,
) {
    let dt = Fx32::from_f32(time.delta_secs());
    let Some((mut pos, mut height)) = q.iter_mut().next() else {
        return;
    };

    // Age the double-tap windows.
    for t in &mut motion.step_tap {
        *t = t.saturating_sub(1);
    }
    motion.jump_tap = motion.jump_tap.saturating_sub(1);

    // Horizontal delta. An in-progress burst (roll/dash) overrides input.
    let delta = if motion.burst > 0 {
        motion.burst -= 1;
        motion.burst_dir * (motion.burst_speed * dt)
    } else if state.is_deployed() {
        deployed_step(&input, *handed, &mut motion, &loco, dt)
    } else {
        stowed_step(&touches, &input, *handed, &mut stick, &mut motion, &mut height, &loco, dt)
    };

    if delta != FxVec2::ZERO {
        motion.last_dir = delta.normalize_or_zero();
    }

    // Gravity integration (the jump arc). Stays grounded at z = 0.
    height.vz = height.vz - loco.gravity * dt;
    height.z = height.z + height.vz * dt;
    if height.z <= Fx32::ZERO {
        height.z = Fx32::ZERO;
        height.vz = Fx32::ZERO;
        height.grounded = true;
    } else {
        height.grounded = false;
    }

    // Apply horizontal move: clamp to the arena, push out of landmark obstacles.
    let bound = Fx32::from_f32(ARENA_HALF);
    let mut np = pos.0 + delta;
    np.x = np.x.clamp(-bound, bound);
    np.y = np.y.clamp(-bound, bound);
    let min = Fx32::from_f32(LANDMARK_COLLIDE);
    for &(lx, ly) in &LANDMARKS {
        let c = FxVec2::from_f32(lx, ly);
        let sep = np - c;
        let d = sep.length();
        if d > Fx32::ZERO && d < min {
            np = c + sep.normalize_or_zero() * min;
        }
    }
    pos.0 = np;
}

/// Stowed locomotion: the Spike A virtual stick, plus the `Jump`/`Dash`/`Roll`
/// cluster verbs. May arm an evasive burst (returning its first-frame delta).
fn stowed_step(
    touches: &Touches,
    input: &ButtonInput<DsButton>,
    handed: Handedness,
    stick: &mut StickState,
    motion: &mut Motion,
    height: &mut Height,
    loco: &Locomotion,
    dt: Fx32,
) -> FxVec2 {
    let delta = stowed_locomotion(touches, stick, loco, dt);
    let heading = if delta != FxVec2::ZERO {
        delta.normalize_or_zero()
    } else {
        motion.last_dir
    };

    // Roll (cluster ▼): an i-frame dodge along the current heading.
    if control::just_pressed(Action::Roll, handed, input) {
        return arm_burst(motion, heading, loco.roll_speed, loco.roll_frames, true, dt);
    }

    // Jump (cluster ►): single press hops; a second within the window dashes.
    if control::just_pressed(Action::Jump, handed, input) {
        if motion.jump_tap > 0 {
            motion.jump_tap = 0;
            return arm_burst(motion, heading, loco.dash_speed, loco.roll_frames, false, dt);
        }
        motion.jump_tap = DOUBLE_TAP_WINDOW;
        if height.grounded {
            height.vz = loco.jump_impulse;
            height.grounded = false;
        }
    }
    // Camera toggles (▲ CamTopDown / ◄ CamOrbit) are bound but inert until #23.

    delta
}

/// Deployed evasive movement: directional dodge-steps (held) + double-tap roll,
/// all on the cluster (mirrored by handedness). The Spike-C-proven model.
fn deployed_step(
    input: &ButtonInput<DsButton>,
    handed: Handedness,
    motion: &mut Motion,
    loco: &Locomotion,
    dt: Fx32,
) -> FxVec2 {
    // Cluster direction → world heading.
    let dirs = [
        (Cluster::Left, FxVec2::new(Fx32::NEG_ONE, Fx32::ZERO)),
        (Cluster::Right, FxVec2::new(Fx32::ONE, Fx32::ZERO)),
        (Cluster::Up, FxVec2::new(Fx32::ZERO, Fx32::ONE)),
        (Cluster::Down, FxVec2::new(Fx32::ZERO, Fx32::NEG_ONE)),
    ];

    // Double-tap a direction → roll that way (i-frames).
    for (i, (cluster, vec)) in dirs.iter().enumerate() {
        if input.just_pressed(cluster.button(handed)) {
            if motion.step_tap[i] > 0 {
                motion.step_tap[i] = 0;
                return arm_burst(motion, *vec, loco.roll_speed, loco.roll_frames, true, dt);
            }
            motion.step_tap[i] = DOUBLE_TAP_WINDOW;
        }
    }

    // Held steps at the (slow) deployed speed.
    let mut dir = FxVec2::ZERO;
    for (cluster, vec) in &dirs {
        if input.pressed(cluster.button(handed)) {
            dir = dir + *vec;
        }
    }
    dir.normalize_or_zero() * (loco.dodge_speed * dt)
}

/// Arm an evasive burst (roll or dash) and return its first-frame delta. The
/// burst continues for `frames` more frames in `move_player`.
fn arm_burst(
    motion: &mut Motion,
    dir: FxVec2,
    speed: Fx32,
    frames: u8,
    invuln: bool,
    dt: Fx32,
) -> FxVec2 {
    motion.burst = frames;
    motion.burst_dir = dir;
    motion.burst_speed = speed;
    motion.invuln = invuln;
    dir * (speed * dt)
}

fn stowed_locomotion(touches: &Touches, stick: &mut StickState, loco: &Locomotion, dt: Fx32) -> FxVec2 {
    let cfg = StickConfig {
        deadzone: Fx32::from_f32(STOW_DEADZONE),
        max_radius: Fx32::from_f32(STOW_MAX_RADIUS),
        smoothing: Fx32::from_f32(STOW_SMOOTH),
    };
    let target = if let Some(touch) = touches.iter().next() {
        let p = touch.position();
        let cur = FxVec2::from_f32(p.x, p.y);
        if !stick.active {
            stick.origin = cur;
            stick.active = true;
        }
        let raw = cur - stick.origin;
        stick_vector(FxVec2::new(raw.x, -raw.y), &cfg)
    } else {
        stick.active = false;
        FxVec2::ZERO
    };
    stick.vel = vel_smooth(stick.vel, target, cfg.smoothing);
    stick.vel * (loco.stow_speed * dt)
}

/// Keep the ground [`Shadow`] under the avatar (it ignores [`Height`], so the
/// jump lift reads against it). Mirrors the avatar's ground `WorldPos`.
pub fn sync_shadow(
    avatar: Query<&WorldPos, (With<Avatar>, Without<Shadow>)>,
    mut shadow: Query<&mut WorldPos, With<Shadow>>,
) {
    let (Some(a), Some(mut s)) = (avatar.iter().next(), shadow.iter_mut().next()) else {
        return;
    };
    s.0 = a.0;
}
