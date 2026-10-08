//! Player controller state machine (#24).
//!
//! The real controller, promoted out of the Spike C harness: a `Stowed ↔
//! Deployed` state machine that wires the stylus + cluster verbs into one
//! moment-to-moment loop (pillars 1 & 2). All input flows through the #21 action
//! layer ([`crate::control`] + the [`Handedness`] resource), never raw buttons.
//!
//! - **Stowed:** stylus = virtual-stick locomotion (Spike A); cluster = `Jump`
//!   (single = hop / double-tap = `Dash`), `Roll` (i-frame dodge), and the
//!   camera verbs (▲ top-down toggle / ◄ orbit-set — driven by the camera director).
//! - **Deployed:** stylus = draw (Spike B, in `main`); cluster = directional
//!   dodge-steps + double-tap roll. Jump/dash are disabled — the pen is out.
//!
//! A real jump/height model lives here too: the avatar carries a [`Height`] (the
//! world "up" axis, separate from the ground `WorldPos`) integrated under
//! gravity, rendered as a screen-Y lift with a ground [`Shadow`]. The vertical
//! read is provisional — it lands better once a side-ish corridor camera exists
//! (#23). Movement is tuned per [`Locomotion`] preset (Arena / Corridor); #27
//! will pick the preset per space.
//!
//! Both axes are resolved against the zone's static geometry by
//! `bevy_nds_collide` (#12): [`move_player`] pushes the horizontal move out of
//! boxes, ramps and columns, and settles [`Height`] onto whatever surface ends
//! up underfoot — the avatar walks up a ramp, steps a kerb, and falls off a
//! ledge through one rule set. The body's dimensions live on [`Locomotion`].

use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_collide::{Body, ground_height, resolve_move, settle_height};
use bevy_nds_math::stick::{StickConfig, smooth as vel_smooth, stick_vector};

use bevy_nds_scene::CameraMode;

use crate::control::{self, Action};
use crate::{Avatar, WorldPos};

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
///
/// This is the **Height frame** `bevy_nds_collide` works in: `0` is the floor
/// and `z` is the height of the avatar's feet above it, which is why
/// `crate::collide::harvest` converts a collider's span out of the render frame
/// by subtracting `GROUND_Y`.
#[derive(Component, Default)]
pub struct Height {
    pub z: Fx32,
    pub vz: Fx32,
    pub grounded: bool,
    /// The support height under the avatar this frame — the floor (`0`) over
    /// open ground, a box top or a point on a ramp over geometry. Read by
    /// [`sync_shadow`] so the contact shadow rides the surface it belongs to.
    pub ground: Fx32,
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

    /// True during a non-invulnerable evasive burst — the stowed dash (a
    /// deployed burst is always the invuln roll). The lunge that finishes a
    /// *breakable* enemy: retract + ram = the expedient destroy exit (#26).
    pub fn is_dashing(&self) -> bool {
        self.burst > 0 && !self.invuln
    }
}

/// Avatar hit points. A hit while deployed **chips health** rather than forcing
/// a retract (OQ-2 resolved dodge-while-draw as fair on its own — the knockout
/// crutch was making a fresh deploy a coin-flip); only running *out* of health
/// knocks the device offline. `START` restores it. (#26 feel pass, 2026-07-11.)
#[derive(Resource)]
pub struct Health {
    pub hp: u8,
    pub max: u8,
}

/// Starting / full hit points.
pub const MAX_HP: u8 = 5;

impl Default for Health {
    fn default() -> Self {
        Self {
            hp: MAX_HP,
            max: MAX_HP,
        }
    }
}

impl Health {
    /// True once health is spent — the device gets knocked offline (the fail
    /// beat), and the avatar stays down until `START` re-arms.
    pub fn is_downed(&self) -> bool {
        self.hp == 0
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
    /// Collision radius of the avatar's body. `0.18` + a landmark's `0.08` half
    /// extent is exactly the `0.26` the pre-#12 circle push-out enforced, so the
    /// shipped landmark feel is preserved to the raw fixed-point unit.
    pub radius: Fx32,
    /// Body height — the ceiling for "is that an overhang I walk under?".
    pub height: Fx32,
    /// The tallest lip the avatar walks up instead of being stopped by.
    pub step: Fx32,
}

/// Avatar body dimensions (#12, provisional pending a `playtest-log` verdict).
/// **Identical in both presets on purpose**: this slice changes no corridor
/// feel, and `Locomotion::for_camera` remains the per-space tuning seam if a
/// later pass wants to differ them.
const BODY_RADIUS: f32 = 0.18;
const BODY_HEIGHT: f32 = 0.32;
const BODY_STEP: f32 = 0.10;

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
            radius: Fx32::from_f32(BODY_RADIUS),
            height: Fx32::from_f32(BODY_HEIGHT),
            step: Fx32::from_f32(BODY_STEP),
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
            radius: Fx32::from_f32(BODY_RADIUS),
            height: Fx32::from_f32(BODY_HEIGHT),
            step: Fx32::from_f32(BODY_STEP),
        }
    }

    /// Pick the movement preset implied by a space's authored camera (#27): a
    /// side-on [`CameraMode::Rail2_5D`] is a traversal corridor (tighter feel);
    /// everything else is an open arena. Lets a space transition swap the feel
    /// without a separate authored field — the camera mode already encodes the
    /// space's role (arena tension vs corridor rest).
    pub fn for_camera(mode: CameraMode) -> Self {
        match mode {
            CameraMode::Rail2_5D { .. } => Self::corridor(),
            _ => Self::arena(),
        }
    }
}

impl Default for Locomotion {
    fn default() -> Self {
        Self::arena()
    }
}

// --- Systems -----------------------------------------------------------------

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
/// deployed dodge / evasive burst), resolve it against the zone's static
/// blocking geometry (#12) and the zone bounds, then settle the jump/height
/// model onto whatever surface ended up underfoot.
pub fn move_player(
    time: Res<Time>,
    touches: Res<Touches>,
    input: Res<ButtonInput<DsButton>>,
    handed: Res<Handedness>,
    state: Res<PlayerState>,
    loco: Res<Locomotion>,
    zone: Res<crate::transition::Zone>,
    radial: Res<crate::radial::Radial>,
    mut stick: ResMut<StickState>,
    mut motion: ResMut<Motion>,
    colliders: Res<crate::collide::Colliders>,
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
        stowed_step(
            &touches,
            &input,
            *handed,
            radial.open,
            &mut stick,
            &mut motion,
            &mut height,
            &loco,
            dt,
        )
    };

    if delta != FxVec2::ZERO {
        motion.last_dir = delta.normalize_or_zero();
    }

    // Horizontal move, then the vertical settle — in that order, because what
    // is underfoot depends on where the move ended up.
    //
    // `prev` is the height the avatar starts the frame at (its feet, in the
    // Height frame the collide crate uses); `grounded` is read *after* input, so
    // a jump armed this frame has already cleared it and nothing steps mid-hop.
    let prev = height.z;
    let grounded = height.grounded;
    let body = Body {
        radius: loco.radius,
        height: loco.height,
        step: loco.step,
    };
    // `WorldPos.y` is the world depth (Z) axis; `Zone.bounds` is
    // `[min_x, min_z, max_x, max_z]`. The depth band is tight for a 2.5D
    // corridor, so the avatar can't walk into the rail camera.
    let [min_x, min_z, max_x, max_z] = zone.bounds;
    let bounds = [
        Fx32::from_f32(min_x),
        Fx32::from_f32(min_z),
        Fx32::from_f32(max_x),
        Fx32::from_f32(max_z),
    ];
    // Grounded, a lip up to `step` tall is walked over; airborne, nothing is.
    let step_allow = if grounded { loco.step } else { Fx32::ZERO };
    let np = resolve_move(
        &colliders.0,
        pos.0,
        pos.0 + delta,
        &body,
        prev,
        step_allow,
        bounds,
    );
    // The same split on the vertical axis: grounded you may rise onto a surface
    // up to `step` above your feet, airborne only onto one you are over (the
    // crate's own exception — landing up-slope) and only while descending, so
    // there is no snap-up onto a pillar near the jump apex.
    // `prev` goes in twice on purpose: as the feet (which decide whether a
    // surface is a step *up*, and so needs the bare footprint rather than the
    // forgiving skirt) and as the airborne ceiling.
    let ceiling = if grounded { prev + loco.step } else { prev };
    let ground = ground_height(&colliders.0, np, &body, prev, ceiling, Fx32::ZERO);
    let vz = height.vz - loco.gravity * dt;
    let z = prev + vz * dt;
    let (z, vz, g) = settle_height(z, vz, prev, grounded, ground, loco.step);
    height.z = z;
    height.vz = vz;
    height.grounded = g;
    height.ground = ground;
    pos.0 = np;
}

/// Stowed locomotion: the Spike A virtual stick, plus the `Jump`/`Dash`/`Roll`
/// cluster verbs. May arm an evasive burst (returning its first-frame delta).
fn stowed_step(
    touches: &Touches,
    input: &ButtonInput<DsButton>,
    handed: Handedness,
    radial_open: bool,
    stick: &mut StickState,
    motion: &mut Motion,
    height: &mut Height,
    loco: &Locomotion,
    dt: Fx32,
) -> FxVec2 {
    // The radial wheel gates the pen out of locomotion (#25): while the shoulder
    // is held, the same drag is picking a spoke, not moving the avatar. Suppress
    // locomotion and drop the stick so it re-anchors cleanly on release — the
    // same borrow-the-stylus pattern OrbitSet uses below.
    // OrbitSet borrows the stylus to aim the camera while cluster ◄ is held
    // (#23): suppress locomotion so the same drag doesn't also move the avatar.
    if radial_open || control::pressed(Action::CamOrbit, handed, input) {
        stick.active = false;
        return FxVec2::ZERO;
    }
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
            return arm_burst(
                motion,
                heading,
                loco.dash_speed,
                loco.roll_frames,
                false,
                dt,
            );
        }
        motion.jump_tap = DOUBLE_TAP_WINDOW;
        if height.grounded {
            height.vz = loco.jump_impulse;
            height.grounded = false;
        }
    }
    // Camera verbs are live (#23): ▲ CamTopDown toggles top-down, ◄ CamOrbit
    // orbits the camera — both handled by the camera director, not here (the
    // ◄ hold is gated above so it doesn't double as locomotion).

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
    // Cluster direction → world heading. `WorldPos.y` is the depth axis and
    // world +Z points *toward* the camera, so "forward/away" is −y — matching
    // the stowed stick (drag up-screen → away; see `stowed_locomotion`). Up =
    // away, Down = toward the camera, so the dodge reads the same deployed as
    // stowed.
    let dirs = [
        (Cluster::Left, FxVec2::new(Fx32::NEG_ONE, Fx32::ZERO)),
        (Cluster::Right, FxVec2::new(Fx32::ONE, Fx32::ZERO)),
        (Cluster::Up, FxVec2::new(Fx32::ZERO, Fx32::NEG_ONE)),
        (Cluster::Down, FxVec2::new(Fx32::ZERO, Fx32::ONE)),
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

fn stowed_locomotion(
    touches: &Touches,
    stick: &mut StickState,
    loco: &Locomotion,
    dt: Fx32,
) -> FxVec2 {
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
        // Y-up world: the ground depth axis (WorldPos.y → world +Z) points
        // *toward* the camera, so dragging the pen up the screen (raw.y < 0)
        // moves the avatar away into the scene — pass raw.y through directly.
        stick_vector(FxVec2::new(raw.x, raw.y), &cfg)
    } else {
        stick.active = false;
        FxVec2::ZERO
    };
    stick.vel = vel_smooth(stick.vel, target, cfg.smoothing);
    stick.vel * (loco.stow_speed * dt)
}

/// Keep the ground [`Shadow`] under the avatar: its ground `WorldPos`, and its
/// [`Height::z`] set to the **support** height rather than the avatar's own, so
/// the shadow rides box and ramp tops (#12) while a jump's lift still reads as a
/// visible gap above it.
pub fn sync_shadow(
    avatar: Query<(&WorldPos, &Height), (With<Avatar>, Without<Shadow>)>,
    mut shadow: Query<(&mut WorldPos, &mut Height), With<Shadow>>,
) {
    let (Some((a, ah)), Some((mut s, mut sh))) = (avatar.iter().next(), shadow.iter_mut().next())
    else {
        return;
    };
    s.0 = a.0;
    sh.z = ah.ground;
}
