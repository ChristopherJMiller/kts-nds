//! Hand-written FFI to maxmod's ARM9 API, in the style of `bevy_nds`'s own
//! `ffi.rs`: no bindgen, minimal surface, symbols resolved against `libmm9.a`
//! at final link (the game crate's `build.rs` adds `-lmm9`).
//!
//! Maxmod runs the actual mixer on the **ARM7** core; these ARM9 calls hand
//! commands across the FIFO/IPC. On the DS the ARM7 services playback, so there
//! is no per-frame `mmFrame` to call from the ARM9 side. The soundbank these
//! IDs index is produced by the `wav2bank` crate.
//!
//! Declarations cite the libnds / maxmod headers they mirror.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

use core::ffi::{c_char, c_int};

// maxmod scalar types (see `<mm_types.h>`).
/// Generic unsigned 32-bit maxmod value.
pub type mm_word = u32;
/// Sound-effect handle returned by [`mmEffect`]; `0` is invalid.
pub type mm_sfxhand = u16;
/// Generic unsigned 8-bit maxmod value (e.g. panning).
pub type mm_byte = u8;
/// Boolean (non-zero = true).
pub type mm_bool = u8;

// Module looping modes for [`mmStart`] (see `mm_pmode` in `<mm_types.h>`).
/// Loop the module forever, until stopped manually.
pub const MM_PLAY_LOOP: c_int = 0;
/// Play the module once, stopping after the last pattern.
pub const MM_PLAY_ONCE: c_int = 1;

/// `mm_sfxhand` value representing "no effect" (see `MM_SFXHAND_INVALID`).
pub const MM_SFXHAND_INVALID: mm_sfxhand = 0;

unsafe extern "C" {
    // --- libnds sound power (see `<nds/arm9/sound.h>`) ------------------------
    /// Power up the sound hardware. Must be called before playback is audible.
    pub fn soundEnable();
    /// Power down the sound hardware.
    pub fn soundDisable();

    // --- ROM filesystem (see `<filesystem.h>`) -------------------------------
    /// Mount the ROM filesystem (NitroFS) so the soundbank can be read from
    /// `nitro:/`. Safe to call again if another plugin already mounted it.
    pub fn nitroFSInit(basepath: *const c_char) -> mm_bool;

    // --- maxmod system (see `<maxmod9.h>`) -----------------------------------
    /// Initialise maxmod with the default channel layout, loading the soundbank
    /// from the given file (e.g. `"nitro:/soundbank.bin"`). Returns success.
    pub fn mmInitDefault(soundbank_file: *const c_char) -> mm_bool;

    // --- modules / music (see `<maxmod9.h>`) ---------------------------------
    /// Load a module from the soundbank into memory; required before [`mmStart`].
    pub fn mmLoad(module_id: mm_word) -> mm_word;
    /// Unload a previously [`mmLoad`]ed module.
    pub fn mmUnload(module_id: mm_word) -> mm_word;
    /// Begin playing a loaded module in [`MM_PLAY_LOOP`] or [`MM_PLAY_ONCE`].
    pub fn mmStart(module_id: mm_word, mode: c_int);
    /// Stop the active module (restart from the beginning with [`mmStart`]).
    pub fn mmStop();
    /// Pause the active module; resume with [`mmResume`].
    pub fn mmPause();
    /// Resume a paused module.
    pub fn mmResume();
    /// Set the active module's volume, `0` (silent) ..= `1024` (normal).
    pub fn mmSetModuleVolume(volume: mm_word);
    /// Non-zero while a module is actively playing.
    pub fn mmActive() -> mm_bool;

    // --- sound effects (see `<maxmod9.h>`) -----------------------------------
    /// Load a sample effect from the soundbank; required before [`mmEffect`].
    pub fn mmLoadEffect(sample_id: mm_word) -> mm_word;
    /// Unload a previously [`mmLoadEffect`]ed sample.
    pub fn mmUnloadEffect(sample_id: mm_word) -> mm_word;
    /// Play a loaded sample effect, returning a handle to control it.
    pub fn mmEffect(sample_id: mm_word) -> mm_sfxhand;
    /// Set an effect's volume, `0` (silent) ..= `255` (normal).
    pub fn mmEffectVolume(handle: mm_sfxhand, volume: mm_word);
    /// Set an effect's panning, `0` (left) ..= `255` (right).
    pub fn mmEffectPanning(handle: mm_sfxhand, panning: mm_byte);
    /// Set an effect's playback rate (frequency scaling, 6.10 fixed point).
    pub fn mmEffectRate(handle: mm_sfxhand, rate: mm_word);
    /// Release an effect: let it finish, after which the handle becomes invalid.
    pub fn mmEffectRelease(handle: mm_sfxhand);
    /// Cancel (immediately stop) a still-held effect.
    pub fn mmEffectCancel(handle: mm_sfxhand);
    /// Cancel every playing effect.
    pub fn mmEffectCancelAll();
}
