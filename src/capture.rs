//! The enemy capture model (issue #26) — promoted out of the Spike-C prototype
//! in `main`.
//!
//! Enemies are **circle-vulnerable**: a drawn loop only captures one when it
//! encloses the enemy's whole footprint ([`VulnerabilityShape::Circle`], tested
//! via [`bevy_nds_loop::encloses_circle`]), not merely its centre. Capture
//! progress is **per enemy** ([`Capture`]) so it persists across stow/deploy —
//! which is what makes the two-exit resolution work:
//!
//! - **Liberate** — keep drawing to full (`progress >= 1.0`) while deployed:
//!   stay exposed and precise. The canonical, rewarded outcome (the machine is
//!   hacked free of the Serpent).
//! - **Destroy** — past the [`DESTROY_THRESHOLD`] the enemy is *breakable*;
//!   retract the pen and **dash into it** ([`Motion::is_dashing`]) to finish it
//!   fast — the expedient, costed bail when the pressure's too high.
//!
//! Both exits latch [`Capture::resolved`] and fire a [`CaptureResolved`] event —
//! the seam the rest of the game (recruit economy #30, ranking #32, VFX) hooks
//! without touching the capture mechanic. Circle is the only shape for now; the
//! full shape-vulnerability matrix (line/triangle/square) is #29.

use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_loop::{encloses_circle, find_closed_loop_within, smooth as path_smooth};
use bevy_nds_math::{Fx32, FxVec2};

use crate::player::{Health, Motion, PlayerState};
use crate::{
    Avatar, CLOSE_TOL, CONTACT_COOLDOWN, CONTACT_DIST, Device, Enemy, MAP_SCALE, MAX_POINTS,
    MIN_SPACING, Stroke, WorldPos, knock_device_offline, world_to_map,
};

/// Capture progress added per fully-enclosing loop — two clean loops (`>= 1.0`)
/// liberate. (OQ-3 tuning, #26.)
pub const CAPTURE_PER_LOOP: f32 = 0.5;

/// Fraction of progress at which an enemy becomes *breakable* — a dash into it
/// destroys it. Below this, only drawing to full (liberate) resolves a capture.
/// One clean loop arms the destroy exit; a second liberates. (OQ-3 tuning, #26.)
pub const DESTROY_THRESHOLD: f32 = 0.5;

/// Enemy footprint radius, world units — the circle a loop must fully enclose.
/// Sized just under the body so the loop has to clearly surround it.
const CAPTURE_RADIUS: f32 = 0.18;

/// How close a dashing avatar must come to a breakable enemy to destroy it.
/// A touch more generous than body-contact so the lunge reads as a hit.
const DASH_KILL_DIST: f32 = 0.34;

/// What a capture region tests against. `Circle` is the only shape for now;
/// `Line` / `Triangle` / `Square` join later as the shape matrix (#29).
#[derive(Component, Clone, Copy)]
pub enum VulnerabilityShape {
    /// Circle-vulnerable: the loop must enclose the enemy's whole footprint of
    /// this radius (world units).
    Circle { radius: Fx32 },
}

impl VulnerabilityShape {
    /// Default circle-vulnerable footprint.
    pub fn circle() -> Self {
        Self::Circle {
            radius: Fx32::from_f32(CAPTURE_RADIUS),
        }
    }
}

/// How a capture ended (issue #26). `Liberated` is the canonical, rewarded path;
/// `Destroyed` is the expedient dash-kill.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Drawn to full — the machine is hacked free of the Serpent.
    Liberated,
    /// Dashed into while breakable — the costed shortcut.
    Destroyed,
}

/// Per-enemy capture state. `progress` accrues from enclosing loops; at
/// [`DESTROY_THRESHOLD`] the enemy is breakable; at `1.0` it liberates.
/// `resolved` latches the outcome so systems stop acting on a finished enemy.
#[derive(Component, Default)]
pub struct Capture {
    pub progress: f32,
    pub resolved: Option<CaptureOutcome>,
}

impl Capture {
    /// Past the destroy threshold and not yet resolved → a dash will destroy it.
    pub fn is_breakable(&self) -> bool {
        self.resolved.is_none() && self.progress >= DESTROY_THRESHOLD
    }

    /// Resolved either way → inert (hidden, no longer a threat or a target).
    pub fn is_resolved(&self) -> bool {
        self.resolved.is_some()
    }
}

/// Fired once when an enemy's capture resolves — the outcome seam (#26): the
/// edge-triggered hook for systems that don't own the mechanic (VFX, sound, and
/// the deferred recruit economy #30 / ranking #32). Carries how it went; enemy
/// identity can join it when a consumer needs it.
#[derive(Event)]
pub struct CaptureResolved {
    pub outcome: CaptureOutcome,
}

/// Running count of how each capture resolved — the first (minimal) consumer of
/// [`CaptureResolved`], and the seed of scoring/ranking (#32). Shown on the HUD
/// so the two exits are legible while playtesting.
#[derive(Resource, Default)]
pub struct CaptureTally {
    pub liberated: u32,
    pub destroyed: u32,
}

/// Drain [`CaptureResolved`] into the [`CaptureTally`].
pub fn tally_captures(mut events: EventReader<CaptureResolved>, mut tally: ResMut<CaptureTally>) {
    for ev in events.read() {
        match ev.outcome {
            CaptureOutcome::Liberated => tally.liberated += 1,
            CaptureOutcome::Destroyed => tally.destroyed += 1,
        }
    }
}

/// While deployed, gather the stylus path and, on closure, add progress to every
/// enemy whose vulnerability footprint the loop **fully** encloses (#26). Full
/// progress liberates and fires [`CaptureResolved`].
pub fn draw_capture(
    touches: Res<Touches>,
    state: Res<PlayerState>,
    mut stroke: ResMut<Stroke>,
    mut resolved: EventWriter<CaptureResolved>,
    mut enemies: Query<(&WorldPos, &VulnerabilityShape, &mut Capture)>,
) {
    if !state.is_deployed() {
        stroke.0.clear();
        return;
    }
    let Some(touch) = touches.iter().next() else {
        stroke.0.clear();
        return;
    };

    let p = touch.position();
    let cur = FxVec2::from_f32(p.x, p.y);
    let push = stroke
        .0
        .last()
        .is_none_or(|&last| (cur - last).length() >= Fx32::from_f32(MIN_SPACING));
    if push {
        stroke.0.push(cur);
        if stroke.0.len() > MAX_POINTS {
            stroke.0.remove(0);
        }
    }
    if stroke.0.len() < 4 {
        return;
    }

    let path = path_smooth(&stroke.0);
    let Some(poly) = find_closed_loop_within(&path, Fx32::from_f32(CLOSE_TOL)) else {
        return;
    };

    let scale = Fx32::from_f32(MAP_SCALE);
    for (pos, shape, mut cap) in &mut enemies {
        if cap.is_resolved() {
            continue;
        }
        let VulnerabilityShape::Circle { radius } = *shape;
        let (mx, my) = world_to_map(pos.0);
        let center = FxVec2::from_f32(mx as f32, my as f32);
        // The loop lives in map pixels; scale the world-unit footprint to match.
        if encloses_circle(&poly, center, radius * scale) {
            cap.progress += CAPTURE_PER_LOOP;
            if cap.progress >= 1.0 {
                cap.resolved = Some(CaptureOutcome::Liberated);
                resolved.write(CaptureResolved {
                    outcome: CaptureOutcome::Liberated,
                });
            }
        }
    }
    stroke.0.clear();
}

/// Dash into a *breakable* enemy to destroy it — the expedient exit (#26). The
/// stowed dash lunge (retract + ram) is the only non-invuln burst, so this fires
/// only while dashing and only against enemies already past the threshold.
pub fn dash_destroy(
    motion: Res<Motion>,
    mut resolved: EventWriter<CaptureResolved>,
    avatar: Query<&WorldPos, With<Avatar>>,
    mut enemies: Query<(&WorldPos, &mut Capture)>,
) {
    if !motion.is_dashing() {
        return;
    }
    let Some(a) = avatar.iter().next().map(|w| w.0) else {
        return;
    };
    let reach = Fx32::from_f32(DASH_KILL_DIST);
    for (pos, mut cap) in &mut enemies {
        if cap.is_breakable() && (a - pos.0).length() < reach {
            cap.resolved = Some(CaptureOutcome::Destroyed);
            resolved.write(CaptureResolved {
                outcome: CaptureOutcome::Destroyed,
            });
        }
    }
}

/// Apply one hit to the avatar: chip a hit point. Progress is **not** lost and
/// the device stays deployed — a hit is attrition, not an ejection — *unless*
/// this empties health, in which case the device is knocked offline (the fail
/// beat). (#26 feel pass: the per-hit forced retract made a fresh deploy a
/// coin-flip; OQ-2 already resolved dodge-while-draw as fair without it.)
pub fn damage(health: &mut Health, state: &mut PlayerState, stroke: &mut Stroke) {
    health.hp = health.hp.saturating_sub(1);
    if health.is_downed() {
        knock_device_offline(state, stroke);
    }
}

/// Enemy body contact while deployed costs a hit point (unless you're mid-roll
/// i-frames or within the post-hit cooldown). The core pressure of
/// capture-while-dodging (#26).
pub fn enemy_contact(
    motion: Res<Motion>,
    mut state: ResMut<PlayerState>,
    mut device: ResMut<Device>,
    mut stroke: ResMut<Stroke>,
    mut health: ResMut<Health>,
    avatar: Query<&WorldPos, With<Avatar>>,
    enemies: Query<(&WorldPos, &Capture), With<Enemy>>,
) {
    if device.hit_cd > 0 {
        device.hit_cd -= 1;
    }
    let Some(a) = avatar.iter().next().map(|w| w.0) else {
        return;
    };
    if !state.is_deployed() || motion.invulnerable() || device.hit_cd > 0 {
        return;
    }
    let contact = Fx32::from_f32(CONTACT_DIST);
    for (pos, cap) in &enemies {
        if cap.is_resolved() {
            continue;
        }
        if (a - pos.0).length() < contact {
            device.hit_cd = CONTACT_COOLDOWN;
            damage(&mut health, &mut state, &mut stroke);
            return;
        }
    }
}
