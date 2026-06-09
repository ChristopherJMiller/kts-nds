//! Save data on the DS's writable FAT/SD storage.
//!
//! Mounts the writable filesystem once in [`PreStartup`] via
//! `fatInitDefault()` (libnds), reports availability via [`StorageStatus`],
//! and exposes a small slot-keyed [`SaveStorage`] API for reading and writing
//! game saves. Files live at `<base_dir>/<slot>.sav` (default `base_dir`
//! is `fat:/bevy-ds/`); slot names are validated to forbid path traversal.
//!
//! ## Blocking vs. async
//!
//! Both flavours are exposed and they share the same underlying stdio path:
//!
//! - [`SaveStorage::read`] / [`SaveStorage::write`] / [`SaveStorage::exists`]
//!   / [`SaveStorage::delete`] do the work synchronously on the calling
//!   thread. Fine for startup loads and tiny saves (settings, a counter); a
//!   large write will stall vblank.
//! - [`SaveStorage::read_async`] / [`SaveStorage::write_async`] return a
//!   [`Task`] from `bevy_nds_cothread`. The work runs on a cooperative
//!   thread that yields to the frame loop, so a multi-kilobyte save no
//!   longer drops the frame rate. The caller holds the [`Task`] and polls
//!   it each frame for completion (dropping an unfinished task blocks until
//!   it joins). [`bevy_nds_cothread::CothreadPlugin`] must be in the [`App`].
//!
//! ## Availability
//!
//! Writable FAT is **not guaranteed** — it depends on the flashcart's DLDI
//! driver or DSi SD card. When the mount fails, [`StorageStatus::Unavailable`]
//! is inserted and every [`SaveStorage`] call returns `None` / `false`. Gate
//! save-related systems on `StorageStatus::is_ready()` (or just check the
//! return value) and degrade gracefully.
//!
//! ```ignore
//! use bevy_ecs::prelude::*;
//! use bevy_nds_save::SaveStorage;
//!
//! fn load_settings(save: Res<SaveStorage>, mut settings: ResMut<Settings>) {
//!     if let Some(bytes) = save.read("settings") {
//!         *settings = Settings::from_bytes(&bytes);
//!     }
//! }
//! ```

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_nds_cothread::Task;

#[cfg(target_vendor = "nintendo")]
mod ffi;
#[cfg(target_vendor = "nintendo")]
mod sys;

/// Default mount + per-game directory used when [`SavePlugin::base_dir`] is
/// `None`. Must already end in `/`.
pub const DEFAULT_BASE_DIR: &str = "fat:/bevy-ds/";

/// Whether [`fatInitDefault()`](https://blocksds.skylyrac.net/) succeeded and
/// the slot directory exists.
///
/// A `Ready` status does not guarantee any specific slot exists — only that
/// reads/writes can be attempted.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageStatus {
    /// FAT mount failed (no flashcart, broken DLDI, no SD) or the base
    /// directory could not be created. All [`SaveStorage`] calls will fail.
    Unavailable,
    /// Writable FS mounted and base directory exists.
    Ready,
}

impl StorageStatus {
    /// Convenience for run conditions.
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Per-slot save data on the writable filesystem. See the [crate-level docs]
/// for the blocking vs. async story and the slot-name rules.
///
/// [crate-level docs]: crate
#[derive(Resource, Clone)]
pub struct SaveStorage {
    base_dir: String,
    status: StorageStatus,
}

impl SaveStorage {
    /// `Ready` if and only if the FAT mount and base-dir mkdir both
    /// succeeded.
    pub fn status(&self) -> StorageStatus {
        self.status
    }

    /// Full filesystem path for a slot, or `None` if the slot name is
    /// invalid. Pure helper — exposed for tests and the occasional consumer
    /// that wants to interop with raw libc stdio.
    pub fn path_for(&self, slot: &str) -> Option<String> {
        slot_path(&self.base_dir, slot)
    }

    /// Read `<slot>.sav` into memory. Returns `None` if the slot doesn't
    /// exist, the name is invalid, storage is unavailable, or any I/O error
    /// occurs.
    ///
    /// Blocks the calling thread; prefer [`read_async`](Self::read_async) for
    /// anything larger than a couple of kilobytes.
    pub fn read(&self, slot: &str) -> Option<Vec<u8>> {
        if !self.status.is_ready() {
            return None;
        }
        let path = self.path_for(slot)?;
        read_blocking(&path)
    }

    /// Write `data` to `<slot>.sav`, atomically replacing any existing
    /// contents. Returns `true` on success.
    ///
    /// Blocks the calling thread; prefer [`write_async`](Self::write_async)
    /// for anything larger than a couple of kilobytes.
    pub fn write(&self, slot: &str, data: &[u8]) -> bool {
        if !self.status.is_ready() {
            return false;
        }
        let Some(path) = self.path_for(slot) else {
            return false;
        };
        write_blocking(&path, data)
    }

    /// `true` if `<slot>.sav` exists and the name is valid.
    pub fn exists(&self, slot: &str) -> bool {
        if !self.status.is_ready() {
            return false;
        }
        let Some(path) = self.path_for(slot) else {
            return false;
        };
        exists_blocking(&path)
    }

    /// Remove `<slot>.sav` if it exists. Returns `true` if the file was
    /// removed (or never existed) and `false` on I/O error.
    pub fn delete(&self, slot: &str) -> bool {
        if !self.status.is_ready() {
            return false;
        }
        let Some(path) = self.path_for(slot) else {
            return false;
        };
        delete_blocking(&path)
    }

    /// Non-blocking [`read`](Self::read) — the work runs on a cothread so the
    /// frame loop keeps ticking. Hold the returned [`Task`] across frames and
    /// poll it for completion.
    pub fn read_async(&self, slot: &str) -> Task<Option<Vec<u8>>> {
        let path = if self.status.is_ready() {
            self.path_for(slot)
        } else {
            None
        };
        bevy_nds_cothread::spawn(move || path.and_then(|p| read_blocking(&p)))
    }

    /// Non-blocking [`write`](Self::write) — takes ownership of `data` so it
    /// can move into the cothread. Hold the returned [`Task`] and poll it
    /// for the success bool.
    pub fn write_async(&self, slot: &str, data: Vec<u8>) -> Task<bool> {
        let path = if self.status.is_ready() {
            self.path_for(slot)
        } else {
            None
        };
        bevy_nds_cothread::spawn(move || match path {
            Some(p) => write_blocking(&p, &data),
            None => false,
        })
    }
}

/// Mounts the writable filesystem in [`PreStartup`] and inserts both
/// [`SaveStorage`] and [`StorageStatus`].
pub struct SavePlugin {
    /// Directory under the writable mount where slot files live, including
    /// the trailing `/`. `None` falls back to [`DEFAULT_BASE_DIR`].
    ///
    /// Must be a single level under the mount point (e.g.
    /// `"fat:/my-game/"`); only the leaf directory is `mkdir`'d at startup,
    /// so multi-level paths fail unless every parent already exists.
    pub base_dir: Option<String>,
}

impl Default for SavePlugin {
    fn default() -> Self {
        Self { base_dir: None }
    }
}

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        let base_dir = self
            .base_dir
            .clone()
            .unwrap_or_else(|| DEFAULT_BASE_DIR.to_string());
        app.insert_resource(PendingBaseDir(base_dir))
            .add_systems(PreStartup, init_storage);
    }
}

/// Carries the chosen `base_dir` from the plugin builder into the
/// `init_storage` system without leaking implementation detail into the
/// public surface.
#[derive(Resource)]
struct PendingBaseDir(String);

fn init_storage(mut commands: Commands, base: Res<PendingBaseDir>) {
    let base_dir = base.0.clone();
    let status = if mount_and_prepare(&base_dir) {
        StorageStatus::Ready
    } else {
        StorageStatus::Unavailable
    };
    commands.insert_resource(SaveStorage {
        base_dir,
        status,
    });
    commands.insert_resource(status);
    commands.remove_resource::<PendingBaseDir>();
}

// --- Pure path helpers (host-tested) -----------------------------------------

/// Build `<base_dir>/<slot>.sav`, validating the slot name. Pure so host
/// tests can exercise the joining and the safety check.
fn slot_path(base_dir: &str, slot: &str) -> Option<String> {
    if !is_valid_slot(slot) {
        return None;
    }
    let mut out = String::with_capacity(base_dir.len() + slot.len() + 5);
    out.push_str(base_dir);
    if !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(slot);
    out.push_str(".sav");
    Some(out)
}

/// Reject slot names that could escape `base_dir` (`/`, `\`, `..`), embed
/// NULs (would truncate the C string passed to libc), or are empty/`.`.
fn is_valid_slot(slot: &str) -> bool {
    if slot.is_empty() || slot == "." || slot == ".." {
        return false;
    }
    !slot
        .as_bytes()
        .iter()
        .any(|&b| b == b'/' || b == b'\\' || b == 0)
}

// --- Backend dispatch --------------------------------------------------------

#[cfg(target_vendor = "nintendo")]
fn mount_and_prepare(base_dir: &str) -> bool {
    sys::mount_and_prepare(base_dir)
}

#[cfg(target_vendor = "nintendo")]
fn read_blocking(path: &str) -> Option<Vec<u8>> {
    sys::read_blocking(path)
}

#[cfg(target_vendor = "nintendo")]
fn write_blocking(path: &str, data: &[u8]) -> bool {
    sys::write_blocking(path, data)
}

#[cfg(target_vendor = "nintendo")]
fn exists_blocking(path: &str) -> bool {
    sys::exists_blocking(path)
}

#[cfg(target_vendor = "nintendo")]
fn delete_blocking(path: &str) -> bool {
    sys::delete_blocking(path)
}

// Host stubs — never reached from tests, since tests only touch pure helpers.
#[cfg(not(target_vendor = "nintendo"))]
fn mount_and_prepare(_base_dir: &str) -> bool {
    false
}
#[cfg(not(target_vendor = "nintendo"))]
fn read_blocking(_path: &str) -> Option<Vec<u8>> {
    None
}
#[cfg(not(target_vendor = "nintendo"))]
fn write_blocking(_path: &str, _data: &[u8]) -> bool {
    false
}
#[cfg(not(target_vendor = "nintendo"))]
fn exists_blocking(_path: &str) -> bool {
    false
}
#[cfg(not(target_vendor = "nintendo"))]
fn delete_blocking(_path: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_path_joins_with_extension() {
        assert_eq!(
            slot_path("fat:/bevy-ds/", "save_0").as_deref(),
            Some("fat:/bevy-ds/save_0.sav"),
        );
    }

    #[test]
    fn slot_path_inserts_missing_separator() {
        assert_eq!(
            slot_path("fat:/bevy-ds", "save_0").as_deref(),
            Some("fat:/bevy-ds/save_0.sav"),
        );
    }

    #[test]
    fn slot_path_rejects_traversal() {
        assert_eq!(slot_path("fat:/bevy-ds/", "../escape"), None);
        assert_eq!(slot_path("fat:/bevy-ds/", "sub/dir"), None);
        assert_eq!(slot_path("fat:/bevy-ds/", "back\\slash"), None);
        assert_eq!(slot_path("fat:/bevy-ds/", ".."), None);
        assert_eq!(slot_path("fat:/bevy-ds/", "."), None);
    }

    #[test]
    fn slot_path_rejects_empty_and_nul() {
        assert_eq!(slot_path("fat:/bevy-ds/", ""), None);
        assert_eq!(slot_path("fat:/bevy-ds/", "with\0nul"), None);
    }

    #[test]
    fn slot_path_accepts_alphanumeric_and_punct() {
        for name in ["a", "save_0", "high.scores", "level-3", "ABC", "1234"] {
            assert!(slot_path("fat:/bevy-ds/", name).is_some(), "{name}");
        }
    }

    #[test]
    fn storage_status_is_ready_only_for_ready() {
        assert!(StorageStatus::Ready.is_ready());
        assert!(!StorageStatus::Unavailable.is_ready());
    }
}
