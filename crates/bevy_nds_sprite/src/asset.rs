//! Runtime parser for the `.sprite` blob produced host-side by `png2sprite`.
//!
//! The on-disk layout (little-endian) is the inverse of `png2sprite::encode`:
//!
//! | offset | type      | field                              |
//! |--------|-----------|------------------------------------|
//! | 0      | `u32`     | magic `"BSP1"`                     |
//! | 4      | `u16`     | width (pixels)                     |
//! | 6      | `u16`     | height (pixels)                    |
//! | 8      | `u32`     | palette entry count                |
//! | 12     | `u32`     | gfx byte count                     |
//! | 16     | `u16` × P | palette (RGB15)                    |
//! | 16+2P  | `u8` × G  | gfx (4bpp tiles, 1D-32 order)      |
//!
//! Pure parsing: no FFI, no hardware. Host-tested.

extern crate alloc;

use alloc::vec::Vec;

/// ASCII `"BSP1"`, matches `png2sprite::ASSET_MAGIC`.
const MAGIC: u32 = u32::from_le_bytes(*b"BSP1");
const HEADER_LEN: usize = 16;

/// A baked sprite read from NitroFS: dimensions + palette + tile gfx, all
/// owned so the underlying file buffer can be dropped.
pub struct LoadedSprite {
    pub width: u16,
    pub height: u16,
    pub palette: Vec<u16>,
    pub gfx: Vec<u8>,
}

/// Try to load and parse `path` (a NUL-terminated NitroFS path). Returns
/// `None` if the filesystem isn't mounted, the file is missing, the magic is
/// wrong, or the declared sizes don't fit.
pub fn load(path: &[u8]) -> Option<LoadedSprite> {
    let bytes = bevy_nds_nitrofs::read_file(path)?;
    parse(&bytes)
}

/// Pure parser. Split out from [`load`] so it can be host-tested without the
/// NitroFS FFI.
pub fn parse(bytes: &[u8]) -> Option<LoadedSprite> {
    if bytes.len() < HEADER_LEN {
        return None;
    }
    let magic = read_u32(bytes, 0);
    if magic != MAGIC {
        return None;
    }
    let width = read_u16(bytes, 4);
    let height = read_u16(bytes, 6);
    let pal_count = read_u32(bytes, 8) as usize;
    let gfx_count = read_u32(bytes, 12) as usize;

    let pal_off = HEADER_LEN;
    let pal_end = pal_off.checked_add(pal_count.checked_mul(2)?)?;
    let gfx_off = pal_end;
    let gfx_end = gfx_off.checked_add(gfx_count)?;
    if gfx_end > bytes.len() {
        return None;
    }

    let mut palette = Vec::with_capacity(pal_count);
    for i in 0..pal_count {
        palette.push(read_u16(bytes, pal_off + i * 2));
    }
    let gfx = bytes[gfx_off..gfx_end].to_vec();
    Some(LoadedSprite {
        width,
        height,
        palette,
        gfx,
    })
}

fn read_u16(bytes: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([bytes[off], bytes[off + 1]])
}

fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_blob(palette: &[u16], gfx: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + palette.len() * 2 + gfx.len());
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes()); // width
        out.extend_from_slice(&16u16.to_le_bytes()); // height
        out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
        out.extend_from_slice(&(gfx.len() as u32).to_le_bytes());
        for &p in palette {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out.extend_from_slice(gfx);
        out
    }

    #[test]
    fn parses_a_round_trip() {
        let pal = [0u16, 0x03FF, 0x001F];
        let gfx = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let blob = build_blob(&pal, &gfx);
        let parsed = parse(&blob).unwrap();
        assert_eq!(parsed.width, 16);
        assert_eq!(parsed.height, 16);
        assert_eq!(parsed.palette, pal);
        assert_eq!(parsed.gfx, gfx);
    }

    #[test]
    fn rejects_short_header() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0u8; 4]).is_none());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut blob = build_blob(&[0u16; 16], &[0u8; 128]);
        blob[0] ^= 0xFF;
        assert!(parse(&blob).is_none());
    }

    #[test]
    fn rejects_truncated_body() {
        let blob = build_blob(&[0u16; 16], &[0u8; 128]);
        // Drop the last gfx byte; the declared gfx_count no longer fits.
        let trunc = &blob[..blob.len() - 1];
        assert!(parse(trunc).is_none());
    }
}
