//! The game: a Bevy app that runs on the Nintendo DS.
//!
//! Everything here is ordinary Bevy — components, systems, resources. The DS
//! itself is handled entirely by the [`bevy_nds`] library via [`DsPlugins`]
//! (the platform layer), [`bevy_nds_3d`] via [`Ds3dPlugin`] (the hardware 3D
//! backend) and [`bevy_nds_audio`] via [`AudioPlugin`] (maxmod sound): this
//! file contains no FFI, no allocator and no panic handler.
//!
//! The demo is a tiny "tile-grid exploration": a hardware-rendered, hardware-lit
//! Utah teapot sits on the bottom screen (the 3D engine's permanent home) and
//! moves one cell at a time around a small map. The top screen shows the map
//! as ASCII (a placeholder for the upcoming sprite plugin) with the player's
//! `@` and a stationary companion `O` drawn over the walkable floor `.` and
//! walls `#`. D-pad snaps the player between adjacent walkable cells; ABXY
//! still tumble the player teapot so the hardware lighting plays across the
//! surface. Looping piano music plays from the baked soundbank (START toggles
//! it), and tapping a teapot fires a click SFX.

#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::Write;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds::prelude::*;
use bevy_nds_3d::prelude::*;
use bevy_nds_audio::prelude::*;
use bevy_nds_sprite::prelude::*;

/// Numeric sound IDs generated at build time by `wav2bank` from the soundbank
/// header (e.g. `SFX_PIANO_LOOP`, `SFX_BLIP_SELECT`), so game code never hard-codes
/// indices. Written to `$OUT_DIR/sounds.rs` by `build.rs`.
mod sounds {
    #![allow(dead_code)] // mmutil also emits MSL_* bank-metadata counts we don't use.
    include!(concat!(env!("OUT_DIR"), "/sounds.rs"));
}

/// NitroFS paths for every baked sprite under `assets/sprites/**/*.png`,
/// generated at build time by `png2sprite` (e.g. `sprites::SPRITE`,
/// `sprites::ui::CURSOR`). Pass one to `Sprite::image` instead of hard-coding
/// the `nitro:/...` path. Written to `$OUT_DIR/sprites.rs` by `build.rs`.
mod sprites {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/sprites.rs"));
}

/// Program entry point, called by the BlocksDS crt0.
#[unsafe(no_mangle)]
pub extern "C" fn main() -> core::ffi::c_int {
    let mut app = App::new();
    app.add_plugins(DsPlugins)
        .add_plugins(Ds3dPlugin)
        .add_plugins(AudioPlugin)
        .add_plugins(SpritePlugin)
        .add_plugins(GamePlugin);
    bevy_nds::run(app)
}

/// The actual game, as a self-contained Bevy plugin.
struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        // The 3D engine lives permanently on the bottom screen; the top screen
        // (the sub engine's text console) is the map + HUD.
        app.insert_resource(Display3d {
            screen: DsScreen::Bottom,
        })
        .insert_resource(Map::new())
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                step_player,
                spin_companion,
                sync_map_to_world,
                sync_marker_glyph,
                sync_marker_sprite,
                update_hud,
                update_touch_hud,
                update_pick_hud,
                update_gesture_hud,
                poke_picked,
                toggle_music,
                update_audio_hud,
            ),
        );
    }
}

// --- Map ---------------------------------------------------------------------

/// Map width in cells.
const MAP_W: usize = 16;
/// Map height in cells.
const MAP_H: usize = 8;
/// World-units per map cell. The map's full extent (`MAP_W * CELL`, `MAP_H * CELL`)
/// is sized to fit inside the camera frustum at z=0.
const CELL: f32 = 0.2;

/// World position of the centre of cell `(0, 0)`. The map is centred on the
/// origin, so cell `(MAP_W-1, MAP_H-1)` lands at the negative of this on each
/// axis.
const MAP_ORIGIN: Vec3 = Vec3::new(
    -CELL * (MAP_W as f32 - 1.0) * 0.5,
    CELL * (MAP_H as f32 - 1.0) * 0.5,
    0.0,
);

/// Where the map is drawn on the text console. Centres the 16x8 cell display
/// horizontally and leaves a title row above it.
const MAP_TILE_COL: i16 = (32 - MAP_W as i16) / 2;
const MAP_TILE_ROW: i16 = 2;

/// The level: walkable floors (`.`) and walls (`#`). Row 0 is the top.
const MAP_DATA: [&[u8; MAP_W]; MAP_H] = [
    b"################",
    b"#..............#",
    b"#.####.##.####.#",
    b"#..............#",
    b"#.####.##.####.#",
    b"#..............#",
    b"#.####.##.####.#",
    b"################",
];

/// The map. Holds the static tile layout; entity positions are separate
/// (carried by [`MapPos`] components) so multiple things can share a cell.
#[derive(Resource)]
struct Map {
    tiles: [[u8; MAP_W]; MAP_H],
}

impl Map {
    fn new() -> Self {
        let mut tiles = [[b' '; MAP_W]; MAP_H];
        for (row, src) in MAP_DATA.iter().enumerate() {
            tiles[row] = **src;
        }
        Self { tiles }
    }

    /// Is `(col, row)` inside the map and a floor cell?
    fn walkable(&self, col: i16, row: i16) -> bool {
        if col < 0 || row < 0 || col >= MAP_W as i16 || row >= MAP_H as i16 {
            return false;
        }
        self.tiles[row as usize][col as usize] == b'.'
    }
}

/// A cell coordinate on the [`Map`]. Carried by anything that occupies a tile
/// (the player teapot, the companion). A separate system ([`sync_map_to_world`])
/// keeps [`Transform3d::translation`] in step with this each frame.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
struct MapPos {
    col: i16,
    row: i16,
}

impl MapPos {
    fn to_world(self) -> Vec3 {
        Vec3::new(
            MAP_ORIGIN.x + CELL * self.col as f32,
            MAP_ORIGIN.y - CELL * self.row as f32,
            0.0,
        )
    }
}

// --- Components --------------------------------------------------------------

/// The player-controlled teapot.
#[derive(Component)]
struct Player;

/// A second, stationary teapot that simply spins in place. It shares the
/// player's geometry but has its own [`Transform3d`], so every frame the
/// renderer composes and uploads two independent model matrices.
#[derive(Component)]
struct Companion;

/// Marker for the player's on-map `Glyph` overlay (the moving `@`). A separate
/// entity so we can update just its `TilePos` when the player walks, without
/// recomposing any text.
#[derive(Component)]
struct PlayerMarker;

/// The live status line on the text console.
#[derive(Component)]
struct Hud;

/// A second status line that echoes the touch-screen position.
#[derive(Component)]
struct TouchHud;

/// A status line naming which teapot the pen is currently over (via picking).
#[derive(Component)]
struct PickHud;

/// A status line showing the most recent touch gesture.
#[derive(Component)]
struct GestureHud;

/// A status line reflecting the music state (playing/muted).
#[derive(Component)]
struct AudioHud;

// --- Setup -------------------------------------------------------------------

fn setup(mut commands: Commands, nitrofs: Res<NitroFs>, mut music: ResMut<Music>) {
    // Load the teapot model: prefer NitroFS (so large models stay out of main
    // RAM and can be swapped without relinking), fall back to the copy baked
    // into the binary by `include_obj!`. Both paths produce byte-identical
    // geometry. The model is authored on the XY plane (pivot at its base);
    // both paths recentre it so it rotates about its visual middle.
    let loaded = nitrofs
        .ready
        .then(|| DsMesh::load(b"nitro:/teapot.dl\0"))
        .flatten();
    let from_nitrofs = loaded.is_some();
    let teapot = loaded.unwrap_or_else(|| include_obj!("assets/teapot.obj", center));
    // The companion shares the same geometry (cheap Cow clone of the display list).
    let companion = teapot.clone();

    // Player teapot — at a known floor cell on the upper-left of the map.
    let player_start = MapPos { col: 2, row: 1 };
    commands.spawn((
        Player,
        player_start,
        teapot,
        DsMaterial {
            diffuse: [120, 170, 215],
            ambient: [28, 36, 56],
        },
        Transform3d {
            translation: player_start.to_world(),
            rotation: Vec3::new(-1.3, 0.5, 0.0),
            scale: Vec3::splat(0.18),
        },
    ));

    // Companion teapot — fixed on the right-hand side, spinning. Proves out
    // multiple transformed meshes per frame (per-object CPU matrix compose +
    // frustum culling) without the player having to move into it.
    let companion_pos = MapPos { col: 13, row: 5 };
    commands.spawn((
        Companion,
        companion_pos,
        companion,
        DsMaterial {
            diffuse: [215, 150, 90],
            ambient: [48, 34, 20],
        },
        Transform3d {
            translation: companion_pos.to_world(),
            rotation: Vec3::new(-1.3, 0.0, 0.0),
            scale: Vec3::splat(0.14),
        },
    ));

    // Top screen: title, map rows (composed each frame from `Map` + entities),
    // and HUD lines.
    let source = if from_nitrofs {
        "bevy-ds map demo  (nitrofs)"
    } else {
        "bevy-ds map demo  (baked-in)"
    };
    commands.spawn((DsScreen::Bottom, TilePos::new(2, 0), DsText::new(source)));

    // Static map rows: one `DsText` per row, written once. Composition cost
    // disappears for unchanged frames; the text-renderer's per-cell diff only
    // ever fires on the cells where the moving `@`/`O` glyphs overlap. The
    // game crate runs at `opt-level = 0`, so recomposing 8 × 16 characters
    // every frame here noticeably halved fps before this change.
    let mut row_buf = alloc::string::String::with_capacity(MAP_W);
    for row in 0..MAP_H {
        row_buf.clear();
        for &byte in MAP_DATA[row].iter() {
            row_buf.push(byte as char);
        }
        commands.spawn((
            DsScreen::Bottom,
            TilePos::new(MAP_TILE_COL, MAP_TILE_ROW + row as i16),
            DsText::new(row_buf.as_str()),
        ));
    }

    // Player marker — a tile-console `Glyph` (`@`) that the text renderer
    // overlays on the static map, PLUS a 16x16 hardware sprite drawn on top
    // of the same cell by `bevy_nds_sprite`. The sprite proves the OAM
    // pipeline; the glyph remains as a fallback if the sprite engine isn't
    // up. Both follow the player's `MapPos` via `sync_marker_*`.
    let start_tile = cell_to_tile(player_start);
    commands.spawn((
        PlayerMarker,
        DsScreen::Bottom,
        start_tile,
        Glyph(b'@'),
        Sprite::new(sprites::SPRITE).at(start_tile.x * 8, start_tile.y * 8),
    ));

    // Companion marker (`O`). It doesn't move, so its `TilePos` is static.
    commands.spawn((DsScreen::Bottom, cell_to_tile(companion_pos), Glyph(b'O')));

    // HUD lines, below the map.
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, MAP_TILE_ROW + MAP_H as i16 + 1),
        Hud,
        DsText::new(""),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, MAP_TILE_ROW + MAP_H as i16 + 2),
        TouchHud,
        DsText::new("touch: --"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, MAP_TILE_ROW + MAP_H as i16 + 3),
        PickHud,
        DsText::new("picked: none"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, MAP_TILE_ROW + MAP_H as i16 + 4),
        GestureHud,
        DsText::new("gesture: --"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, MAP_TILE_ROW + MAP_H as i16 + 5),
        AudioHud,
        DsText::new("music: --"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 22),
        DsText::new("D-pad: walk   ABXY: tumble"),
    ));
    commands.spawn((
        DsScreen::Bottom,
        TilePos::new(2, 23),
        DsText::new("tap teapot for SFX   START: mute"),
    ));

    // Kick off the looping piano. `Music` is declarative — the audio backend
    // reconciles the hardware to it each frame.
    music.play(SoundId(sounds::SFX_PIANO_LOOP));
}

// --- Movement ----------------------------------------------------------------

/// Move the player one cell on D-pad press (not hold), if the target is
/// walkable. Discrete cell-snapped movement: the world position follows in
/// [`sync_map_to_world`].
fn step_player(
    input: Res<ButtonInput<DsButton>>,
    map: Res<Map>,
    mut query: Query<&mut MapPos, With<Player>>,
) {
    let (dx, dy) = if input.just_pressed(DsButton::Left) {
        (-1, 0)
    } else if input.just_pressed(DsButton::Right) {
        (1, 0)
    } else if input.just_pressed(DsButton::Up) {
        (0, -1)
    } else if input.just_pressed(DsButton::Down) {
        (0, 1)
    } else {
        return;
    };
    for mut pos in &mut query {
        let target_col = pos.col + dx;
        let target_row = pos.row + dy;
        if map.walkable(target_col, target_row) {
            pos.col = target_col;
            pos.row = target_row;
        }
    }
}

/// Tumble the player with the face buttons so the hardware lighting is visible:
/// Y/A yaw left/right, X/B pitch up/down.
fn tumble_player(input: &ButtonInput<DsButton>, transform: &mut Transform3d) {
    const SPEED: f32 = 0.06;
    if input.pressed(DsButton::A) {
        transform.rotation.y += SPEED;
    }
    if input.pressed(DsButton::Y) {
        transform.rotation.y -= SPEED;
    }
    if input.pressed(DsButton::X) {
        transform.rotation.x -= SPEED;
    }
    if input.pressed(DsButton::B) {
        transform.rotation.x += SPEED;
    }
}

/// Slowly spin the companion in place, and apply the face-button tumble to the
/// player. Both are folded into the same `MapPos -> world` sync below so any
/// rotation here lands in the same frame's transform.
fn spin_companion(time: Res<Time>, mut query: Query<&mut Transform3d, With<Companion>>) {
    let dt = time.delta_secs();
    for mut transform in &mut query {
        transform.rotation.y += dt;
    }
}

/// Drive `Transform3d.translation` from `MapPos` each frame. Cheap (a handful
/// of entities) and keeps map position the source of truth.
fn sync_map_to_world(
    input: Res<ButtonInput<DsButton>>,
    mut query: Query<(&MapPos, &mut Transform3d, Option<&Player>)>,
) {
    for (pos, mut transform, is_player) in &mut query {
        transform.translation = pos.to_world();
        if is_player.is_some() {
            tumble_player(&input, &mut transform);
        }
    }
}

// --- Map display -------------------------------------------------------------

/// Convert a map cell to the tile position of its glyph on the text console.
fn cell_to_tile(pos: MapPos) -> TilePos {
    TilePos::new(MAP_TILE_COL + pos.col, MAP_TILE_ROW + pos.row)
}

/// Keep the player's `@` glyph aligned with its current cell. Triggered by
/// `Changed<MapPos>`, so it only does work on the frames the player actually
/// walks — no per-frame map recomposition.
fn sync_marker_glyph(
    player: Query<&MapPos, (With<Player>, Changed<MapPos>)>,
    mut marker: Query<&mut TilePos, With<PlayerMarker>>,
) {
    let Some(pos) = player.iter().next() else {
        return;
    };
    for mut tile in &mut marker {
        *tile = cell_to_tile(*pos);
    }
}

/// Keep the hardware sprite covering the player's `@` glyph aligned with its
/// current cell. Same `Changed<MapPos>` gating as the glyph: the sprite only
/// moves on the frame the player walks.
fn sync_marker_sprite(
    player: Query<&MapPos, (With<Player>, Changed<MapPos>)>,
    mut marker: Query<&mut Sprite, With<PlayerMarker>>,
) {
    let Some(pos) = player.iter().next() else {
        return;
    };
    let tile = cell_to_tile(*pos);
    for mut sprite in &mut marker {
        sprite.x = tile.x * 8;
        sprite.y = tile.y * 8;
    }
}

// --- HUDs --------------------------------------------------------------------

/// Echo the touch-screen state to its HUD line.
fn update_touch_hud(touches: Res<Touches>, mut query: Query<&mut DsText, With<TouchHud>>) {
    for mut text in &mut query {
        text.0.clear();
        if let Some(touch) = touches.iter().next() {
            let pos = touch.position();
            let _ = write!(text.0, "touch: {:>3},{:>3}", pos.x as i32, pos.y as i32);
        } else {
            let _ = write!(text.0, "touch: --");
        }
    }
}

/// Name the entity the pen is over, by checking it against the teapot markers.
fn pick_name(pick: &TouchPick, player: Entity, companion: Entity) -> &'static str {
    match pick.entity {
        Some(e) if e == player => "player",
        Some(e) if e == companion => "companion",
        Some(_) => "?",
        None => "none",
    }
}

/// Report which teapot the pen is hovering over, using the engine's hardware
/// [`TouchPick`] result.
fn update_pick_hud(
    pick: Res<TouchPick>,
    player: Single<Entity, With<Player>>,
    companion: Single<Entity, With<Companion>>,
    mut query: Query<&mut DsText, With<PickHud>>,
) {
    let name = pick_name(&pick, *player, *companion);
    for mut text in &mut query {
        text.0.clear();
        let _ = write!(text.0, "picked: {name}");
    }
}

/// Tapping a teapot tumbles it and fires a click SFX. Gated on the
/// [`Gesture::Tap`] event (a quick press-and-release in place) so dragging /
/// swiping across the teapots doesn't trigger it. The tap is emitted on
/// pen-up, which reaches `Update` before [`TouchPick`] is cleared in `Last`,
/// so `pick.entity` still holds whatever teapot was under the pen during the
/// press.
fn poke_picked(
    pick: Res<TouchPick>,
    mut gestures: EventReader<GestureEvent>,
    mut sfx: EventWriter<PlaySfx>,
    mut query: Query<&mut Transform3d>,
) {
    let tapped = gestures
        .read()
        .any(|GestureEvent(g)| matches!(g, Gesture::Tap(_)));
    if !tapped {
        return;
    }
    if let Some(entity) = pick.entity
        && let Ok(mut transform) = query.get_mut(entity)
    {
        transform.rotation.y += core::f32::consts::FRAC_PI_2;
        sfx.write(PlaySfx::new(SoundId(sounds::SFX_BLIP_SELECT)));
    }
}

/// Toggle the background music on and off with START. Demonstrates the
/// declarative [`Music`] resource: the game sets the desired track and the
/// backend reconciles the hardware.
fn toggle_music(input: Res<ButtonInput<DsButton>>, mut music: ResMut<Music>) {
    if input.just_pressed(DsButton::Start) {
        if music.is_playing() {
            music.stop();
        } else {
            music.play(SoundId(sounds::SFX_PIANO_LOOP));
        }
    }
}

/// Reflect the music state on its HUD line.
fn update_audio_hud(
    audio: Res<Audio>,
    music: Res<Music>,
    mut query: Query<&mut DsText, With<AudioHud>>,
) {
    let state = if !audio.ready {
        "unavailable"
    } else if music.is_playing() {
        "playing"
    } else {
        "muted"
    };
    for mut text in &mut query {
        text.0.clear();
        let _ = write!(text.0, "music: {state}");
    }
}

/// Show the latest touch gesture on its HUD line.
fn update_gesture_hud(
    mut events: EventReader<GestureEvent>,
    mut query: Query<&mut DsText, With<GestureHud>>,
) {
    let Some(GestureEvent(gesture)) = events.read().last() else {
        return;
    };
    let label = match gesture {
        Gesture::Tap(_) => "tap",
        Gesture::LongPress(_) => "long press",
        Gesture::Swipe { direction, .. } => match direction {
            SwipeDir::Up => "swipe up",
            SwipeDir::Down => "swipe down",
            SwipeDir::Left => "swipe left",
            SwipeDir::Right => "swipe right",
        },
        Gesture::DragStart(_) => "drag start",
        Gesture::Drag { .. } => "drag",
        Gesture::DragEnd(_) => "drag end",
    };
    for mut text in &mut query {
        text.0.clear();
        let _ = write!(text.0, "gesture: {label}");
    }
}

/// Refresh the HUD from `Time`, `Fps`, and the player's map cell.
fn update_hud(
    time: Res<Time>,
    fps: Res<Fps>,
    player: Query<&MapPos, With<Player>>,
    mut query: Query<&mut DsText, With<Hud>>,
) {
    let secs = time.elapsed_secs() as u32;
    let fps = fps.0;
    let (pc, pr) = match player.iter().next() {
        Some(p) => (p.col, p.row),
        None => (0, 0),
    };
    for mut text in &mut query {
        text.0.clear();
        let _ = write!(text.0, "t={secs:>4}s fps={fps:>2.0} cell=({pc:>2},{pr:>2})");
    }
}
