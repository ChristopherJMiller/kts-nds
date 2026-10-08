//! The enemy capture model (issue #26) — promoted out of the Spike-C prototype
//! in `main` — and the **policy half** of the shape-vulnerability matrix (#29).
//!
//! Every enemy carries a [`VulnerabilityShape`]: the one [`CaptureShape`] it
//! answers to (from the matrix, `kts_schema::EnemyKind::required_shape`) and the
//! world-unit radius of its footprint. Two gestures resolve against it:
//!
//! - a **closed loop**, classified by [`bevy_nds_loop::classify_loop`] into
//!   circle / triangle / square, which must *fully enclose* the footprint
//!   ([`bevy_nds_loop::encloses_circle`]) — not merely cover its centre;
//! - an open **slash**, classified on pen-up by
//!   [`bevy_nds_loop::classify_open`], which must *cut through* the footprint
//!   with both ends clear of it ([`bevy_nds_loop::crosses_circle`]).
//!
//! The drawn shape then has to match: [`kts_schema::accepts`] is **exact** for
//! Milestone 2 (no strength hierarchy — open on #29). A wrong-shape attempt
//! scores nothing but is never silent — it sets [`Tell::wrong_shape`], which
//! flashes the enemy's map blip.
//!
//! This module holds **policy only**. All stroke geometry lives in
//! `bevy_nds_loop::shape` (pure, host-tested) and the kind→shape table lives in
//! `kts_schema` (declared once). Classification runs on a closure or a pen-up
//! frame, never per frame.
//!
//! Capture progress is **per enemy** ([`Capture`]) so it persists across
//! stow/deploy — which is what makes the two-exit resolution work:
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
//! without touching the capture mechanic.

use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_loop::{
    LoopShape, classify_loop, classify_open, crosses_circle, encloses_circle,
    find_closed_loop_within, smooth as path_smooth,
};
use bevy_nds_math::{Fx32, FxVec2};
use bevy_nds_sprite::prelude::Sprite;
use kts_schema::{CaptureShape, EnemyKind, accepts};

use crate::player::{Health, Motion, PlayerState};
use crate::{
    Avatar, CLOSE_TOL, CONTACT_COOLDOWN, CONTACT_DIST, Device, Enemy, MAP_SCALE, MAX_POINTS,
    MIN_SPACING, Stroke, WorldPos, knock_device_offline, sprites, world_to_map,
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

/// Frames an enemy's map blip flashes after a **wrong-shape** attempt (#29).
/// Long enough to read at 60 Hz, short enough not to hide the shape tell.
pub const TELL_FRAMES: u8 = 14;

/// Frames the HUD keeps showing what the last resolved stroke was read as —
/// the instrumentation the loop-quality question (#29 / #32) needs.
pub const LAST_SHAPE_TTL: u8 = 90;

/// Per-enemy capture geometry: **which** shape captures it and **how big** its
/// footprint is.
///
/// `required` comes from the matrix (`EnemyKind::required_shape`, declared once
/// in `kts_schema`); `radius` is the map-space footprint in **world units**,
/// scaled by [`MAP_SCALE`] at the test site. One radius for all four kinds this
/// slice — per-kind radii stay deliberately un-introduced (#26 OQ-3 / #29), so
/// no tuning knob is entangled with the new shape axis before either has a
/// playtest verdict.
#[derive(Component, Clone, Copy)]
pub struct VulnerabilityShape {
    pub required: CaptureShape,
    pub radius: Fx32,
}

impl VulnerabilityShape {
    /// The footprint an enemy of this kind gets. **The one site** that consults
    /// the #29 matrix; the table itself lives in `kts_schema` so the game, the
    /// baker and the editor cannot disagree about it.
    pub fn for_kind(kind: EnemyKind) -> Self {
        Self {
            required: kind.required_shape(),
            radius: Fx32::from_f32(CAPTURE_RADIUS),
        }
    }

    /// The circle-vulnerable footprint — i.e. [`EnemyKind::Basic`]'s. Kept for
    /// any caller that wants "the default enemy" without naming a kind (nothing
    /// does today; every enemy comes from an authored kind through
    /// [`Self::for_kind`]).
    #[allow(dead_code)] // no caller since spawn started reading the authored kind
    pub fn circle() -> Self {
        Self::for_kind(EnemyKind::Basic)
    }

    /// The tactical-map blip that advertises this enemy's required shape. The
    /// **shape-based** half of the #27 accessibility lock (colour only
    /// reinforces): the tell is on the screen the pen acts on.
    pub fn blip(&self) -> &'static [u8] {
        match self.required {
            CaptureShape::Circle => sprites::BLIP,
            CaptureShape::Line => sprites::BLIP_LINE,
            CaptureShape::Triangle => sprites::BLIP_TRI,
            CaptureShape::Square => sprites::BLIP_SQ,
        }
    }

    /// The one-character HUD stand-in for the required shape.
    pub fn glyph(&self) -> char {
        shape_glyph(self.required)
    }
}

/// A [`CaptureShape`] as one console character: `O` circle, `-` line,
/// `A` triangle, `#` square. The 32-column grid has no room for the words, and
/// these read at a glance next to a blip of the same silhouette.
pub fn shape_glyph(shape: CaptureShape) -> char {
    match shape {
        CaptureShape::Circle => 'O',
        CaptureShape::Line => '-',
        CaptureShape::Triangle => 'A',
        CaptureShape::Square => '#',
    }
}

/// Per-enemy feedback state — why a drawn stroke did nothing (#29).
///
/// [`update_enemy_tell`] is **the single writer** of an enemy's `Sprite.image`
/// (and, once the items work lands, of its `DsMaterial.diffuse`). Later feedback
/// — afflictions, hit flashes — **adds a field here and extends the precedence
/// in `update_enemy_tell`**; it must never write those components from a second
/// system, or two systems fight over one sprite every frame.
#[derive(Component, Default)]
pub struct Tell {
    /// Frames left of the wrong-shape flash.
    pub wrong_shape: u8,
}

impl Tell {
    /// Age the tell by one frame. Called once per frame by [`update_enemy_tell`].
    pub fn tick(&mut self) {
        self.wrong_shape = self.wrong_shape.saturating_sub(1);
    }

    /// Is a wrong-shape flash showing right now?
    pub fn flashing(&self) -> bool {
        self.wrong_shape > 0
    }
}

/// What the last resolved stroke was read as — HUD instrumentation for the
/// loop-quality question (#29 / #32).
///
/// `ttl > 0` with `drawn == None` means the stroke closed but classified as
/// **nothing** (below the scribble floor) — the HUD says `drew ?`. Quality is
/// shown, never spent: whether it should scale capture progress is still open.
#[derive(Resource, Default)]
pub struct LastShape {
    pub drawn: Option<CaptureShape>,
    pub quality: Fx32,
    pub ttl: u8,
}

impl LastShape {
    /// Record a recognised stroke.
    pub fn set(&mut self, drawn: CaptureShape, quality: Fx32) {
        self.drawn = Some(drawn);
        self.quality = quality;
        self.ttl = LAST_SHAPE_TTL;
    }

    /// Record a closed stroke that was not a shape at all.
    pub fn set_scribble(&mut self) {
        self.drawn = None;
        self.quality = Fx32::ZERO;
        self.ttl = LAST_SHAPE_TTL;
    }
}

/// The drawn-shape vocabulary a closed loop maps onto. `bevy_nds_loop` is
/// game-agnostic and has no `Line` (a line cannot be a closed polygon); the
/// slash arrives through [`classify_open`] instead.
fn drawn_shape(shape: LoopShape) -> CaptureShape {
    match shape {
        LoopShape::Circle => CaptureShape::Circle,
        LoopShape::Triangle => CaptureShape::Triangle,
        LoopShape::Square => CaptureShape::Square,
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
    /// How cleanly the resolving stroke was drawn, `0..=1` (#29). **Carried,
    /// not spent**: nothing scales off it this slice — whether loop quality
    /// should pay out (and how) is open on #29 / #32, and this is the seam a
    /// consumer would read. `ZERO` for a dash, where quality is n/a.
    #[allow(dead_code)] // the unspent #29 seam; owners are #30 (items) / #32 (ranking)
    pub quality: Fx32,
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

/// While deployed, gather the stylus path and resolve it against the shape
/// matrix (#26 / #29). Two resolutions over the same buffer:
///
/// - **on closure** — the loop is classified (circle / triangle / square) and
///   applied to every enemy whose footprint it fully encloses;
/// - **on pen-up** — an unclosed stroke that reads as a straight slash is
///   applied to every enemy it cuts through.
///
/// Either way the drawn shape must [`accepts`] the enemy's required one; a
/// wrong shape scores nothing and lights [`Tell::wrong_shape`]. Full progress
/// liberates and fires [`CaptureResolved`].
///
/// **One pen-down resolves at most once.** A closure empties the point buffer
/// but does not end the pen-down, so the tail drawn past the crossing refills
/// it; `Stroke`'s closure latch (`spend_closure` / `closure_spent`) is what
/// stops that tail being read again as a slash on release. Without it, a tail
/// straight enough to pass [`classify_open`] resolves a Line against whatever
/// it cuts and overwrites the HUD readout with `drew -` — on exactly the square
/// gesture the readout exists to measure. Not yet observed on hardware; the
/// path is there by construction (brief §9.12 is the check).
pub fn draw_capture(
    touches: Res<Touches>,
    state: Res<PlayerState>,
    radial: Res<crate::radial::Radial>,
    mut stroke: ResMut<Stroke>,
    mut last: ResMut<LastShape>,
    mut resolved: EventWriter<CaptureResolved>,
    mut enemies: Query<(&WorldPos, &VulnerabilityShape, &mut Capture, &mut Tell)>,
) {
    // While the radial wheel is open the pen is selecting a spoke, not drawing
    // (#25): the shoulder-hold gates it out of the capture stroke, the deployed
    // twin of the locomotion gate in `stowed_step`. Stowing or opening the wheel
    // therefore cancels an in-flight slash exactly as it cancels a loop.
    if !state.is_deployed() || radial.open {
        stroke.clear();
        return;
    }
    let Some(touch) = touches.iter().next() else {
        // Pen-up: the frame a **slash** resolves (#29). It runs exactly once —
        // `Touches::iter()` yields nothing from here on and the buffer is
        // cleared below, so a held-down swipe can never award 60×/s.
        //
        // …and only if this pen-down has not *already* resolved. A closure
        // empties the buffer while the pen is still down, so the overshoot tail
        // the player draws past the crossing refills it; classifying that tail
        // would resolve one gesture twice and overwrite `LastShape` (the square
        // the HUD is there to report) with a spurious `drew -`.
        if !stroke.closure_spent() && stroke.0.len() >= 4 {
            let path = path_smooth(&stroke.0);
            if let Some(quality) = classify_open(&path) {
                last.set(CaptureShape::Line, quality);
                let scale = Fx32::from_f32(MAP_SCALE);
                for (pos, shape, mut cap, mut tell) in &mut enemies {
                    if cap.is_resolved() {
                        continue;
                    }
                    let (mx, my) = world_to_map(pos.0);
                    let center = FxVec2::from_f32(mx as f32, my as f32);
                    // The stroke lives in map pixels; scale the world-unit
                    // footprint to match (same conversion as the loop path).
                    if !crosses_circle(&path, center, shape.radius * scale) {
                        continue;
                    }
                    if accepts(shape.required, CaptureShape::Line) {
                        cap.progress += CAPTURE_PER_LOOP;
                        if cap.progress >= 1.0 {
                            cap.resolved = Some(CaptureOutcome::Liberated);
                            resolved.write(CaptureResolved {
                                outcome: CaptureOutcome::Liberated,
                                quality,
                            });
                        }
                    } else {
                        // Cut, but not what this machine answers to — say so.
                        tell.wrong_shape = TELL_FRAMES;
                    }
                }
            }
        }
        stroke.clear();
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

    // What did that loop actually draw? Below the scribble floor it drew
    // nothing — the stroke is spent either way, so the HUD says `drew ?`
    // instead of leaving the player wondering if detection broke.
    let Some(fit) = classify_loop(&poly) else {
        last.set_scribble();
        stroke.spend_closure();
        return;
    };
    let drawn = drawn_shape(fit.shape);
    last.set(drawn, fit.quality);

    let scale = Fx32::from_f32(MAP_SCALE);
    for (pos, shape, mut cap, mut tell) in &mut enemies {
        if cap.is_resolved() {
            continue;
        }
        let (mx, my) = world_to_map(pos.0);
        let center = FxVec2::from_f32(mx as f32, my as f32);
        // The loop lives in map pixels; scale the world-unit footprint to match.
        if !encloses_circle(&poly, center, shape.radius * scale) {
            continue;
        }
        if accepts(shape.required, drawn) {
            cap.progress += CAPTURE_PER_LOOP;
            if cap.progress >= 1.0 {
                cap.resolved = Some(CaptureOutcome::Liberated);
                resolved.write(CaptureResolved {
                    outcome: CaptureOutcome::Liberated,
                    quality: fit.quality,
                });
            }
        } else {
            // Enclosed, but the wrong gesture — zero progress, never silent.
            tell.wrong_shape = TELL_FRAMES;
        }
    }
    stroke.spend_closure();
}

/// The **single writer** of an enemy's map blip (#29).
///
/// Base image is the enemy's required-shape blip ([`VulnerabilityShape::blip`]);
/// a live [`Tell`] overrides it with `BLIP_HIT` for [`TELL_FRAMES`]. Later
/// feedback layers extend this precedence — nothing else may assign
/// `Sprite.image` on an enemy.
///
/// Also ages [`LastShape`], which is the other thing the pen's feedback owns.
/// No `Commands` and no archetype moves: one `u8` per enemy per frame.
pub fn update_enemy_tell(
    mut last: ResMut<LastShape>,
    mut q: Query<(&VulnerabilityShape, &mut Tell, &mut Sprite), With<Enemy>>,
) {
    last.ttl = last.ttl.saturating_sub(1);
    for (shape, mut tell, mut sprite) in &mut q {
        let want = if tell.flashing() {
            sprites::BLIP_HIT
        } else {
            shape.blip()
        };
        // `SpriteAssets` keys on the *pointer identity* of the path constant, so
        // always assign the `sprites::*` constant itself, and only when it
        // actually changes (an assignment would otherwise mark the component
        // changed every frame for nothing).
        if !core::ptr::eq(sprite.image.as_ptr(), want.as_ptr()) {
            sprite.image = want;
        }
        tell.tick();
    }
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
                // Quality is n/a for a dash — nothing was drawn to score.
                quality: Fx32::ZERO,
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
