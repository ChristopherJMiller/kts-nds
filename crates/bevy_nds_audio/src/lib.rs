//! `bevy_nds_audio` — maxmod-backed audio for [`bevy_nds`].
//!
//! Sound on the Nintendo DS is mixed on the **ARM7** core by maxmod, driven over
//! the FIFO/IPC from the ARM9. This crate is an additive audio backend for
//! `bevy_nds`: it wraps the maxmod ARM9 API and exposes it as ordinary Bevy
//! resources and events, so games play looping music and fire one-shot sound
//! effects without ever touching FFI. (Audio is the project's first second-core
//! dependency: the ROM must embed the maxmod ARM7 core — `just rom` selects
//! `arm7_maxmod.elf`.)
//!
//! The soundbank is baked host-side by the `wav2bank` crate into
//! `nitro:/soundbank.bin` and loaded at runtime; game code refers to sounds by
//! the numeric IDs `wav2bank` generates (`SFX_*`).
//!
//! ```ignore
//! app.add_plugins(AudioPlugin);
//! // Looping background music (declarative — set it and forget it):
//! fn start(mut music: ResMut<Music>) { music.play(SoundId(SFX_PIANO_LOOP)); }
//! // One-shot effect (imperative — fire an event):
//! fn click(mut sfx: EventWriter<PlaySfx>) { sfx.write(PlaySfx::new(SoundId(SFX_BLIP_SELECT))); }
//! ```

#![no_std]

extern crate alloc;

use core::ffi::c_char;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;

mod ffi;
mod sfx;

pub use sfx::{LoadedEffects, effect_volume, module_volume, panning};

/// Path the soundbank is mounted from at runtime (packed into NitroFS by
/// `just rom`).
const SOUNDBANK_PATH: &[u8] = b"nitro:/soundbank.bin\0";

/// A sound in the soundbank, identified by the numeric ID `wav2bank` generates
/// (e.g. `SoundId(SFX_BLIP_SELECT)`). The same id space covers samples (effects)
/// and modules (songs); which one an id is depends on the soundbank.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct SoundId(pub u32);

/// Whether the audio system initialised. When `ready` is `false` the soundbank
/// failed to mount (usually because NitroFS is unavailable — a loader that
/// doesn't supply `argv[0]`; emulators always work) and all playback is a no-op.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct Audio {
    pub ready: bool,
}

/// Declarative control of the single looping background-music track.
///
/// Set the desired track with [`Music::play`] (or clear it with [`Music::stop`])
/// and the backend reconciles the hardware to match — so it is safe to call from
/// `Startup` without worrying about event timing. Volume is `0.0..=1.0`.
#[derive(Resource, Debug)]
pub struct Music {
    /// The track the game wants playing (looped), or `None` for silence.
    desired: Option<SoundId>,
    /// Desired volume, `0.0..=1.0`.
    volume: f32,
    /// What is actually playing: the track and its maxmod effect handle.
    playing: Option<(SoundId, ffi::mm_sfxhand)>,
    /// The volume last pushed to hardware, to avoid redundant FFI each frame.
    applied_volume: f32,
}

impl Default for Music {
    fn default() -> Self {
        Self {
            desired: None,
            volume: 1.0,
            playing: None,
            applied_volume: -1.0,
        }
    }
}

impl Music {
    /// Request `sound` to play, looping. Replaces any current track.
    pub fn play(&mut self, sound: SoundId) {
        self.desired = Some(sound);
    }

    /// Stop the music.
    pub fn stop(&mut self) {
        self.desired = None;
    }

    /// Set the music volume, `0.0` (silent) ..= `1.0` (full).
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
    }

    /// The current music volume setting.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Whether a track is currently playing on the hardware.
    pub fn is_playing(&self) -> bool {
        self.playing.is_some()
    }
}

/// Fire a one-shot sound effect. Write this event from gameplay systems
/// (`EventWriter<PlaySfx>`); the backend plays it the same frame.
#[derive(Event, Clone, Copy, Debug)]
pub struct PlaySfx {
    /// Which effect to play.
    pub sound: SoundId,
    /// Volume, `0.0..=1.0`.
    pub volume: f32,
    /// Stereo position, `0.0` (left) ..= `1.0` (right); `0.5` is centred.
    pub panning: f32,
}

impl PlaySfx {
    /// A centred effect at full volume.
    pub fn new(sound: SoundId) -> Self {
        Self {
            sound,
            volume: 1.0,
            panning: 0.5,
        }
    }

    /// Set the volume (`0.0..=1.0`).
    pub fn with_volume(mut self, volume: f32) -> Self {
        self.volume = volume;
        self
    }

    /// Set the stereo position (`0.0` left ..= `1.0` right).
    pub fn with_panning(mut self, panning: f32) -> Self {
        self.panning = panning;
        self
    }
}

/// Internal backend state: which effects have been loaded into maxmod.
#[derive(Resource, Default)]
struct AudioBackend {
    loaded: LoadedEffects,
}

/// Maxmod audio: a looping [`Music`] track plus one-shot [`PlaySfx`] effects.
///
/// Mounts the soundbank from NitroFS in `PreStartup` and processes playback in
/// `Update`. Add it via `DsPlugins` or directly. Requires the ROM to carry the
/// maxmod ARM7 core (`arm7_maxmod.elf`) and to be linked against `-lmm9`.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Audio>()
            .init_resource::<Music>()
            .init_resource::<AudioBackend>()
            .add_event::<PlaySfx>()
            .add_systems(PreStartup, init_audio)
            .add_systems(Update, (drive_music, play_sfx_events));
    }
}

/// Mount the soundbank and power up the sound hardware.
fn init_audio(mut audio: ResMut<Audio>) {
    // SAFETY: one-shot hardware bring-up on the single ARM9 core. `nitroFSInit`
    // is idempotent, so it is safe even if another plugin already mounted the
    // filesystem; `mmInitDefault` reads the NUL-terminated soundbank path.
    let ready = unsafe {
        ffi::nitroFSInit(core::ptr::null());
        let ok = ffi::mmInitDefault(SOUNDBANK_PATH.as_ptr() as *const c_char);
        ffi::soundEnable();
        ok != 0
    };
    audio.ready = ready;
}

/// Reconcile the maxmod state to the desired [`Music`] each frame. Music is a
/// looped sample (the soundbank bakes the loop point), kept alive by retaining
/// its effect handle; stopping cancels it.
fn drive_music(audio: Res<Audio>, mut music: ResMut<Music>, mut backend: ResMut<AudioBackend>) {
    if !audio.ready {
        return;
    }

    match (music.desired, music.playing) {
        // Already playing the right track: push a volume change if one happened.
        (Some(want), Some((cur, handle))) if want == cur => {
            if music.volume != music.applied_volume {
                // SAFETY: `handle` is a live effect handle from `mmEffect`.
                unsafe { ffi::mmEffectVolume(handle, effect_volume(music.volume)) };
                music.applied_volume = music.volume;
            }
        }
        // A new (or changed) track: cancel the old one and start the new.
        (Some(want), _) => {
            if let Some((_, handle)) = music.playing {
                // SAFETY: cancelling a live, unreleased effect handle.
                unsafe { ffi::mmEffectCancel(handle) };
            }
            if backend.loaded.needs_load(want.0) {
                // SAFETY: load the sample before first playing it.
                unsafe { ffi::mmLoadEffect(want.0) };
            }
            // SAFETY: the effect is loaded; the returned handle controls it.
            let handle = unsafe { ffi::mmEffect(want.0) };
            let vol = music.volume;
            unsafe { ffi::mmEffectVolume(handle, effect_volume(vol)) };
            music.playing = Some((want, handle));
            music.applied_volume = vol;
        }
        // Asked to stop: cancel whatever is playing.
        (None, Some((_, handle))) => {
            // SAFETY: cancelling a live, unreleased effect handle.
            unsafe { ffi::mmEffectCancel(handle) };
            music.playing = None;
        }
        (None, None) => {}
    }
}

/// Play each queued [`PlaySfx`] as a one-shot effect.
fn play_sfx_events(
    audio: Res<Audio>,
    mut backend: ResMut<AudioBackend>,
    mut events: EventReader<PlaySfx>,
) {
    if !audio.ready {
        events.clear();
        return;
    }
    for event in events.read() {
        let id = event.sound.0;
        if backend.loaded.needs_load(id) {
            // SAFETY: load the sample before first playing it.
            unsafe { ffi::mmLoadEffect(id) };
        }
        // SAFETY: the effect is loaded; configure the returned handle and let it
        // play to completion (one-shot, so we don't retain the handle).
        unsafe {
            let handle = ffi::mmEffect(id);
            ffi::mmEffectVolume(handle, effect_volume(event.volume));
            ffi::mmEffectPanning(handle, panning(event.panning));
        }
    }
}

/// Common imports for games using the audio backend.
pub mod prelude {
    pub use crate::{Audio, AudioPlugin, Music, PlaySfx, SoundId};
}
