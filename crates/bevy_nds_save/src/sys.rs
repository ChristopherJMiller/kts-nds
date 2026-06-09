//! On-target (`target_vendor = "nintendo"`) implementations of the blocking
//! filesystem operations. Pulled out of `lib.rs` so the FFI and the byte-by-
//! byte newlib glue stay out of the way of the public Rust surface.

use alloc::ffi::CString;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_void;

use crate::ffi;

/// CString-ify a Rust string for handing to libc. Returns `None` if it would
/// contain an interior NUL byte — the slot-name validator in `lib.rs` already
/// rejects those, but paths constructed in unusual ways might still slip
/// through.
fn cstr(s: &str) -> Option<CString> {
    CString::new(s).ok()
}

pub fn mount_and_prepare(base_dir: &str) -> bool {
    // SAFETY: `fatInitDefault()` takes no arguments and returns a bool. It is
    // idempotent — calling it after a previous successful mount is harmless,
    // which matters because the user might add the plugin twice.
    if !unsafe { ffi::fatInitDefault() } {
        return false;
    }

    // Strip a trailing `/` so `mkdir` sees a normalized directory name.
    let dir = base_dir.trim_end_matches('/');
    let Some(c_dir) = cstr(dir) else {
        return false;
    };
    // SAFETY: `c_dir` lives until end of scope, the C side does not retain
    // the pointer. Mode 0o755 = rwxr-xr-x. EEXIST is fine — we only need the
    // directory to *be* there afterwards, not for *us* to have created it.
    let rc = unsafe { ffi::mkdir(c_dir.as_ptr(), 0o755) };
    if rc != 0 {
        // mkdir failed — could be EEXIST (fine) or something fatal. Probe
        // with access(); if the dir is present, we're good.
        // SAFETY: same CString lifetime guarantees as above. Mode 0 = F_OK.
        if unsafe { ffi::access(c_dir.as_ptr(), 0) } != 0 {
            return false;
        }
    }
    true
}

pub fn read_blocking(path: &str) -> Option<Vec<u8>> {
    let c_path = cstr(path)?;
    // Open binary for reading.
    let mode = c"rb";
    // SAFETY: both pointers live through the call. A null return means the
    // file couldn't be opened (most commonly: doesn't exist), and we hand
    // that back as `None` without touching the file pointer.
    let f = unsafe { ffi::fopen(c_path.as_ptr(), mode.as_ptr()) };
    if f.is_null() {
        return None;
    }

    // Sized in a single allocation: seek to end, ftell to learn the length,
    // rewind, read once. The two-pass alternative (read in chunks into a
    // growing Vec) means more allocator traffic on a 4 MB system.
    // SAFETY: `f` is a valid FILE* until we fclose() it below.
    let len = unsafe {
        if ffi::fseek(f, 0, ffi::SEEK_END) != 0 {
            ffi::fclose(f);
            return None;
        }
        let n = ffi::ftell(f);
        if n < 0 {
            ffi::fclose(f);
            return None;
        }
        if ffi::fseek(f, 0, ffi::SEEK_SET) != 0 {
            ffi::fclose(f);
            return None;
        }
        n as usize
    };

    let mut buf: Vec<u8> = vec![0u8; len];
    // SAFETY: buf.as_mut_ptr() is valid for `len` bytes; we wrote `len` to
    // the Vec via vec![] above. A short read means EOF or I/O error; treat
    // that as failure.
    let read = unsafe { ffi::fread(buf.as_mut_ptr() as *mut c_void, 1, len, f) };
    // SAFETY: regardless of read success, the FILE* must be closed.
    let close_ok = unsafe { ffi::fclose(f) } == 0;
    if read != len || !close_ok {
        return None;
    }
    Some(buf)
}

pub fn write_blocking(path: &str, data: &[u8]) -> bool {
    let Some(c_path) = cstr(path) else {
        return false;
    };
    let mode = c"wb";
    // SAFETY: both pointers live through the call.
    let f = unsafe { ffi::fopen(c_path.as_ptr(), mode.as_ptr()) };
    if f.is_null() {
        return false;
    }

    // SAFETY: `data.as_ptr()` is valid for `data.len()` bytes; the FILE* is
    // owned until fclose(). A short write means the underlying card filled
    // or errored — treat as failure, but still close the handle.
    let written = unsafe { ffi::fwrite(data.as_ptr() as *const c_void, 1, data.len(), f) };
    let close_ok = unsafe { ffi::fclose(f) } == 0;
    written == data.len() && close_ok
}

pub fn exists_blocking(path: &str) -> bool {
    let Some(c_path) = cstr(path) else {
        return false;
    };
    // SAFETY: c_path lives through the call. Mode 0 = F_OK (existence check).
    unsafe { ffi::access(c_path.as_ptr(), 0) == 0 }
}

pub fn delete_blocking(path: &str) -> bool {
    let Some(c_path) = cstr(path) else {
        return false;
    };
    // SAFETY: c_path lives through the call. Treat "already absent" as
    // success — the post-condition the caller cares about (file gone) holds.
    if unsafe { ffi::unlink(c_path.as_ptr()) } == 0 {
        return true;
    }
    !exists_blocking(path)
}
