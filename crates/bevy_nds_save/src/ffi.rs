//! Raw C bindings for the writable-filesystem layer.
//!
//! `fatInitDefault` lives in `<fat.h>`; everything else is plain newlib stdio
//! / POSIX (`<stdio.h>`, `<unistd.h>`, `<sys/stat.h>`). BlocksDS's libfat
//! plumbs FAT access through stdio so paths like `fat:/foo.sav` and
//! `sd:/foo.sav` work with the ordinary `fopen` family.

use core::ffi::{c_char, c_int, c_long, c_void};

/// Opaque `FILE` handle returned by `fopen`.
#[repr(C)]
pub struct CFile {
    _private: [u8; 0],
}

/// `lseek` whence constants. POSIX/newlib values; included so we can use
/// `fseek(.., SEEK_END)` to determine file size before reading.
pub const SEEK_SET: c_int = 0;
pub const SEEK_END: c_int = 2;

unsafe extern "C" {
    /// Mount the default writable device (DLDI flashcart, DSi SD). Returns
    /// `true` on success. See `<fat.h>`.
    pub fn fatInitDefault() -> bool;

    /// `FILE *fopen(const char *path, const char *mode)`. See `<stdio.h>`.
    pub fn fopen(path: *const c_char, mode: *const c_char) -> *mut CFile;
    /// `size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream)`.
    pub fn fread(ptr: *mut c_void, size: usize, nmemb: usize, stream: *mut CFile) -> usize;
    /// `size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream)`.
    pub fn fwrite(ptr: *const c_void, size: usize, nmemb: usize, stream: *mut CFile) -> usize;
    /// `int fclose(FILE *stream)`. Returns 0 on success.
    pub fn fclose(stream: *mut CFile) -> c_int;
    /// `int fseek(FILE *stream, long offset, int whence)`. Returns 0 on success.
    pub fn fseek(stream: *mut CFile, offset: c_long, whence: c_int) -> c_int;
    /// `long ftell(FILE *stream)`. Returns the current offset, or -1 on error.
    pub fn ftell(stream: *mut CFile) -> c_long;

    /// `int access(const char *pathname, int mode)`. We only use `F_OK`
    /// (`mode == 0`): returns 0 if the path exists, -1 otherwise.
    /// See `<unistd.h>`.
    pub fn access(pathname: *const c_char, mode: c_int) -> c_int;
    /// `int unlink(const char *pathname)`. Returns 0 on success.
    /// See `<unistd.h>`.
    pub fn unlink(pathname: *const c_char) -> c_int;
    /// `int mkdir(const char *pathname, mode_t mode)`. Returns 0 on success
    /// or -1 with `errno = EEXIST` if the directory already exists.
    /// `mode_t` is `unsigned short` on this newlib; we pass it through `c_int`
    /// since the function actually takes a `mode_t` which is wider on the
    /// register convention. See `<sys/stat.h>`.
    pub fn mkdir(pathname: *const c_char, mode: c_int) -> c_int;
}
