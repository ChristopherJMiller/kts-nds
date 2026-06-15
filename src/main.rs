//! **Spike B — loop-draw capture + enclosure detection** (Milestone 1, issue #19).
//!
//! A throwaway feel-spike, not production code. It proves the core capture verb
//! in isolation (no 3D, no dodging): **does loop-drawing read clearly and feel
//! satisfying** at 60 Hz touch sampling? You drag the stylus into a loop around
//! the enemy "blips" on the bottom screen; whatever the loop encloses takes a
//! capture hit, and two hits captures it.
//!
//! The feel-critical geometry — path smoothing, self-intersection loop closure,
//! point-in-polygon enclosure — is the pure, host-tested [`bevy_nds_loop`] crate
//! (the keeper that grows into epic #22). This file is the ROM-side harness: it
//! captures the touch path, feeds the crate, and visualizes the stroke (a trail
//! of dot sprites) and the blips (sprites that change as they're captured).
//!
//! Layout: the bottom LCD is the touch/draw surface — sprites (blips + the
//! drawn trail) on the sub engine, over a black canvas. The top LCD is a text
//! HUD. No 3D core this time, so there's no engine-swap dance.
//!
//! Controls: **drag** on the bottom screen to draw a loop. **START** resets the
//! blips for another pass.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_loop::{densify, enclosed, find_closed_loop_within, smooth};
use bevy_nds_math::{Fx32, FxVec2};
use bevy_nds_sprite::prelude::*;

/// NitroFS paths for baked sprites (`sprites::BLIP`, `sprites::BLIP_HIT`,
/// `sprites::DOT`), generated at build time by `png2sprite`.
mod sprites {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/sprites.rs"));
}

// --- Tunables ----------------------------------------------------------------

/// Minimum stylus travel (px) between captured path points — resamples the raw
/// ~60 Hz stream down to an even, jitter-tolerant polyline. Kept below the 8 px
/// dot size so the trail reads as a continuous line, not a dotted one.
const MIN_SPACING: f32 = 4.0;
/// Max control points retained in the live stroke (oldest dropped). Bounds the
/// per-frame closure scan; a 4 px-spaced stroke of this many covers ~400 px.
const MAX_POINTS: usize = 100;
/// Sprites in the trail pool (≤ 128 OAM minus the blips). The trail is
/// `densify`-resampled to this many points, so it reads as a continuous line.
const DOT_POOL: usize = 110;
/// Spacing (px) between rendered trail dots after densify (≤ 8 px dot size so
/// they overlap into a line).
const TRAIL_STEP: f32 = 4.0;
/// Default loop-closure proximity tolerance (px). 0 = exact self-crossing only;
/// higher = laxer (the stroke closes when it returns *near* its trail). Live-
/// tunable with L/R. Feel pass (2026-06-14) landed on 2: exact crossing plus a
/// small near-miss grace.
const DEFAULT_CLOSE_TOL: f32 = 2.0;
/// Enclosures needed to fully capture a blip (so progress visibly accrues).
const CAPTURE_HITS: u8 = 2;

/// Enemy blip centres on the bottom screen (256×192 px). Clustered so several
/// can be caught in one loop, plus loners for single captures.
const BLIPS: [(i16, i16); 6] = [
    (104, 64),
    (128, 56),
    (118, 86),
    (196, 120),
    (60, 132),
    (200, 58),
];

// --- Resources / components --------------------------------------------------

/// The in-progress stroke: resampled touch points (pixels) + whether the pen is
/// currently down (so pen-up can finalize/clear it).
#[derive(Resource, Default)]
struct Stroke {
    points: Vec<FxVec2>,
    active: bool,
}

/// Tally + last-loop feedback for the HUD.
#[derive(Resource, Default)]
struct Score {
    captured: u32,
    last_enclosed: usize,
}

/// Live loop-closure proximity tolerance (px), tuned with L/R during the feel
/// pass. See [`DEFAULT_CLOSE_TOL`].
#[derive(Resource)]
struct CloseTol(Fx32);

impl Default for CloseTol {
    fn default() -> Self {
        Self(Fx32::from_f32(DEFAULT_CLOSE_TOL))
    }
}

/// An enemy blip and its capture progress.
#[derive(Component)]
struct Blip {
    hits: u8,
    captured: bool,
}

/// A blip's centre in screen pixels (fixed-point, for the enclosure test).
#[derive(Component)]
struct BlipPos(FxVec2);

/// One sprite in the path-trail pool.
#[derive(Component)]
struct PathDot;

/// The top-screen HUD line.
#[derive(Component)]
struct InfoHud;

/// Program entry point, called by the BlocksDS crt0.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins)
        .add_plugins(SpritePlugin)
        .add_plugins(SpikePlugin);
    bevy_nds::run(app)
}

struct SpikePlugin;

impl Plugin for SpikePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Stroke>()
            .init_resource::<Score>()
            .init_resource::<CloseTol>()
            .add_systems(Startup, setup)
            .add_systems(
                Update,
                (
                    reset_blips,
                    adjust_closure,
                    capture_input,
                    update_dots,
                    update_blip_sprites,
                    update_hud,
                )
                    .chain(),
            );
    }
}

// --- Setup -------------------------------------------------------------------

fn setup(mut commands: Commands) {
    // Enemy blips (sub-engine sprites, centred on their BLIPS coordinate).
    for &(cx, cy) in &BLIPS {
        commands.spawn((
            Blip {
                hits: 0,
                captured: false,
            },
            BlipPos(FxVec2::from_f32(cx as f32, cy as f32)),
            Sprite::new(sprites::BLIP).at(cx - 8, cy - 8),
        ));
    }

    // Path-trail pool — parked off-screen until the stroke uses them.
    for _ in 0..DOT_POOL {
        commands.spawn((PathDot, Sprite::new(sprites::DOT).at(0, 200)));
    }

    // Top-screen HUD.
    let t = DsScreen::Top;
    commands.spawn((t, TilePos::new(2, 1), DsText::new("Spike B: loop-draw capture")));
    commands.spawn((t, TilePos::new(2, 3), DsText::new("Draw a loop around the red")));
    commands.spawn((t, TilePos::new(2, 4), DsText::new("nodes on the bottom screen.")));
    commands.spawn((t, TilePos::new(2, 5), DsText::new("2 loops captures a node.")));
    commands.spawn((t, TilePos::new(2, 8), InfoHud, DsText::new("")));
    commands.spawn((t, TilePos::new(2, 21), DsText::new("L/R: closure laxness")));
    commands.spawn((t, TilePos::new(2, 22), DsText::new("START: reset nodes")));
}

// --- Capture loop ------------------------------------------------------------

/// Capture the stylus path, close the loop (self-crossing or a lax near-miss),
/// test which blips it encloses, and accrue capture hits. The heart of the spike.
fn capture_input(
    touches: Res<Touches>,
    tol: Res<CloseTol>,
    mut stroke: ResMut<Stroke>,
    mut score: ResMut<Score>,
    mut blips: Query<(&BlipPos, &mut Blip)>,
) {
    let min_spacing = Fx32::from_f32(MIN_SPACING);

    let Some(touch) = touches.iter().next() else {
        // Pen up: drop the stroke.
        stroke.active = false;
        stroke.points.clear();
        return;
    };

    let p = touch.position();
    let cur = FxVec2::from_f32(p.x, p.y);
    stroke.active = true;

    // Resample: only keep a point once the pen has moved far enough.
    let push = stroke
        .points
        .last()
        .is_none_or(|&last| (cur - last).length() >= min_spacing);
    if push {
        stroke.points.push(cur);
        if stroke.points.len() > MAX_POINTS {
            stroke.points.remove(0);
        }
    }

    if stroke.points.len() < 4 {
        return;
    }

    // Smooth, then look for a closure (exact self-crossing, or a near-miss
    // within the live tolerance).
    let path = smooth(&stroke.points);
    let Some(poly) = find_closed_loop_within(&path, tol.0) else {
        return;
    };

    // Which un-captured blips are inside? Accrue a hit on each.
    let mut live: Vec<(FxVec2, Mut<Blip>)> = blips
        .iter_mut()
        .filter(|(_, b)| !b.captured)
        .map(|(pos, b)| (pos.0, b))
        .collect();
    let centres: Vec<FxVec2> = live.iter().map(|(c, _)| *c).collect();
    let inside = enclosed(&poly, &centres);

    score.last_enclosed = inside.len();
    for i in inside {
        let blip = &mut live[i].1;
        blip.hits += 1;
        if blip.hits >= CAPTURE_HITS {
            blip.captured = true;
            score.captured += 1;
        }
    }

    // Consume the loop so the next one starts fresh.
    stroke.points.clear();
}

/// START re-arms every blip for another pass.
fn reset_blips(
    input: Res<ButtonInput<DsButton>>,
    mut score: ResMut<Score>,
    mut blips: Query<&mut Blip>,
) {
    if !input.just_pressed(DsButton::Start) {
        return;
    }
    for mut blip in &mut blips {
        blip.hits = 0;
        blip.captured = false;
    }
    *score = Score::default();
}

/// Live-tune the loop-closure laxness with L/R (0 = exact self-crossing only).
fn adjust_closure(input: Res<ButtonInput<DsButton>>, mut tol: ResMut<CloseTol>) {
    let step = Fx32::from_int(2);
    if input.just_pressed(DsButton::L) {
        tol.0 = (tol.0 - step).max(Fx32::ZERO);
    }
    if input.just_pressed(DsButton::R) {
        tol.0 = (tol.0 + step).min(Fx32::from_int(32));
    }
}

// --- Rendering ---------------------------------------------------------------

/// Render the stroke as a continuous line: smooth it, then `densify` to evenly
/// spaced points (filling fast-drag gaps) and lay the pooled dot sprites along
/// them; park the rest off-screen. The dots are identical, so iteration order
/// doesn't matter — only the set of positions does.
fn update_dots(stroke: Res<Stroke>, mut dots: Query<&mut Sprite, With<PathDot>>) {
    let line = densify(
        &smooth(&stroke.points),
        Fx32::from_f32(TRAIL_STEP),
        DOT_POOL,
    );
    for (i, mut sprite) in dots.iter_mut().enumerate() {
        if let Some(p) = line.get(i) {
            sprite.x = p.x.to_f32() as i16 - 4; // 8×8 dot, centre on the point
            sprite.y = p.y.to_f32() as i16 - 4;
        } else {
            sprite.x = 0;
            sprite.y = 200; // off the 192-px screen
        }
    }
}

/// Reflect each blip's capture state in its sprite: red → yellow at one hit,
/// parked off-screen once captured.
fn update_blip_sprites(mut blips: Query<(&Blip, &BlipPos, &mut Sprite), Without<PathDot>>) {
    for (blip, pos, mut sprite) in &mut blips {
        if blip.captured {
            sprite.x = 248;
            sprite.y = 200;
            continue;
        }
        sprite.image = if blip.hits >= 1 {
            sprites::BLIP_HIT
        } else {
            sprites::BLIP
        };
        sprite.x = pos.0.x.to_f32() as i16 - 8; // 16×16 blip
        sprite.y = pos.0.y.to_f32() as i16 - 8;
    }
}

fn update_hud(
    stroke: Res<Stroke>,
    score: Res<Score>,
    tol: Res<CloseTol>,
    mut hud: Query<&mut DsText, With<InfoHud>>,
) {
    for mut text in &mut hud {
        text.0.clear();
        let _ = write!(
            text.0,
            "cap {}/{} loop:{} pts:{} lax:{:.0}",
            score.captured,
            BLIPS.len(),
            score.last_enclosed,
            stroke.points.len(),
            tol.0.to_f32(),
        );
    }
}
