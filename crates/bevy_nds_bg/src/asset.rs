//! Runtime parsers for the `.bg` (tile) and `.bbg` (bitmap) blobs produced
//! host-side by `png2bg`. Pure logic, no FFI — host-tested.
//!
//! The on-disk layouts are the inverse of `png2bg::encode_tile` /
//! `png2bg::encode_bitmap`. Tile layout:
//!
//! | offset | type      | field                              |
//! |--------|-----------|------------------------------------|
//! | 0      | `u32`     | magic `"BTB1"`                     |
//! | 4      | `u32`     | palette entry count (P)            |
//! | 8      | `u32`     | gfx byte count (G)                 |
//! | 12     | `u32`     | map byte count (M)                 |
//! | 16     | `u16` × P | palette (RGB15)                    |
//! | …      | `u8` × G  | gfx (4bpp tiles)                   |
//! | …      | `u8` × M  | map (16-bit entries, little-endian)|
//!
//! Bitmap layout:
//!
//! | offset | type      | field                              |
//! |--------|-----------|------------------------------------|
//! | 0      | `u32`     | magic `"BBB1"`                     |
//! | 4      | `u16`     | width (px)                         |
//! | 6      | `u16`     | height (px)                        |
//! | 8      | `u32`     | pixel count (= width × height)     |
//! | 12     | `u16` × N | pixels (RGB15 + alpha bit)         |

extern crate alloc;

use alloc::vec::Vec;

/// ASCII `"BTB1"`, matches `png2bg::TILE_MAGIC`.
const TILE_MAGIC: u32 = u32::from_le_bytes(*b"BTB1");
const TILE_HEADER_LEN: usize = 16;
/// ASCII `"BBB1"`, matches `png2bg::BITMAP_MAGIC`.
const BITMAP_MAGIC: u32 = u32::from_le_bytes(*b"BBB1");
const BITMAP_HEADER_LEN: usize = 12;

/// A baked tile background loaded from NitroFS: palette + gfx + map, all owned
/// so the source buffer can be dropped.
pub struct LoadedTile {
    pub palette: Vec<u16>,
    pub gfx: Vec<u8>,
    pub map: Vec<u8>,
}

/// A baked bitmap background loaded from NitroFS. Height is implicit in
/// `pixels.len() / width`; only `width` and `pixels` are exposed because they
/// are the only inputs the runtime row-copy needs.
pub struct LoadedBitmap {
    pub width: u16,
    pub pixels: Vec<u16>,
}

/// Try to load and parse a tile `.bg` blob at `path` (NUL-terminated NitroFS
/// path). Returns `None` if the filesystem isn't mounted, the file is missing,
/// the magic is wrong, or the declared sizes don't fit.
pub fn load_tile(path: &[u8]) -> Option<LoadedTile> {
    let bytes = bevy_nds_nitrofs::read_file(path)?;
    parse_tile(&bytes)
}

/// Try to load and parse a bitmap `.bbg` blob at `path`.
pub fn load_bitmap(path: &[u8]) -> Option<LoadedBitmap> {
    let bytes = bevy_nds_nitrofs::read_file(path)?;
    parse_bitmap(&bytes)
}

/// Pure tile parser. Split out from [`load_tile`] so it can be host-tested
/// without the NitroFS FFI.
pub fn parse_tile(bytes: &[u8]) -> Option<LoadedTile> {
    if bytes.len() < TILE_HEADER_LEN {
        return None;
    }
    if read_u32(bytes, 0) != TILE_MAGIC {
        return None;
    }
    let pal_count = read_u32(bytes, 4) as usize;
    let gfx_count = read_u32(bytes, 8) as usize;
    let map_count = read_u32(bytes, 12) as usize;

    let pal_off = TILE_HEADER_LEN;
    let pal_end = pal_off.checked_add(pal_count.checked_mul(2)?)?;
    let gfx_off = pal_end;
    let gfx_end = gfx_off.checked_add(gfx_count)?;
    let map_off = gfx_end;
    let map_end = map_off.checked_add(map_count)?;
    if map_end > bytes.len() {
        return None;
    }

    let mut palette = Vec::with_capacity(pal_count);
    for i in 0..pal_count {
        palette.push(read_u16(bytes, pal_off + i * 2));
    }
    let gfx = bytes[gfx_off..gfx_end].to_vec();
    let map = bytes[map_off..map_end].to_vec();
    Some(LoadedTile { palette, gfx, map })
}

/// Pure bitmap parser.
pub fn parse_bitmap(bytes: &[u8]) -> Option<LoadedBitmap> {
    if bytes.len() < BITMAP_HEADER_LEN {
        return None;
    }
    if read_u32(bytes, 0) != BITMAP_MAGIC {
        return None;
    }
    let width = read_u16(bytes, 4);
    let height = read_u16(bytes, 6);
    let pixel_count = read_u32(bytes, 8) as usize;

    let off = BITMAP_HEADER_LEN;
    let end = off.checked_add(pixel_count.checked_mul(2)?)?;
    if end > bytes.len() {
        return None;
    }
    if pixel_count != (width as usize) * (height as usize) {
        return None;
    }

    let mut pixels = Vec::with_capacity(pixel_count);
    for i in 0..pixel_count {
        pixels.push(read_u16(bytes, off + i * 2));
    }
    Some(LoadedBitmap { width, pixels })
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

    fn tile_blob(palette: &[u16], gfx: &[u8], map: &[u8]) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(TILE_HEADER_LEN + palette.len() * 2 + gfx.len() + map.len());
        out.extend_from_slice(&TILE_MAGIC.to_le_bytes());
        out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
        out.extend_from_slice(&(gfx.len() as u32).to_le_bytes());
        out.extend_from_slice(&(map.len() as u32).to_le_bytes());
        for &p in palette {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out.extend_from_slice(gfx);
        out.extend_from_slice(map);
        out
    }

    fn bitmap_blob(width: u16, height: u16, pixels: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(BITMAP_HEADER_LEN + pixels.len() * 2);
        out.extend_from_slice(&BITMAP_MAGIC.to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
        for &p in pixels {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out
    }

    #[test]
    fn parse_tile_round_trips() {
        let pal = [0u16, 0x03FF, 0x001F];
        let gfx = [0xAAu8, 0xBB, 0xCC];
        let map = [0xCCu8, 0xDD, 0xEE, 0xFF];
        let blob = tile_blob(&pal, &gfx, &map);
        let parsed = parse_tile(&blob).unwrap();
        assert_eq!(parsed.palette, pal);
        assert_eq!(parsed.gfx, gfx);
        assert_eq!(parsed.map, map);
    }

    #[test]
    fn parse_tile_rejects_short_header() {
        assert!(parse_tile(&[]).is_none());
        assert!(parse_tile(&[0u8; 4]).is_none());
    }

    #[test]
    fn parse_tile_rejects_bad_magic() {
        let mut blob = tile_blob(&[0u16; 4], &[0u8; 8], &[0u8; 16]);
        blob[0] ^= 0xFF;
        assert!(parse_tile(&blob).is_none());
    }

    #[test]
    fn parse_tile_rejects_truncated_body() {
        let blob = tile_blob(&[0u16; 4], &[0u8; 8], &[0u8; 16]);
        // Drop the last map byte — declared map_count no longer fits.
        let trunc = &blob[..blob.len() - 1];
        assert!(parse_tile(trunc).is_none());
    }

    #[test]
    fn parse_bitmap_round_trips() {
        let pixels = [0x7FFFu16, 0x0001, 0x8000, 0xABCD];
        let blob = bitmap_blob(2, 2, &pixels);
        let parsed = parse_bitmap(&blob).unwrap();
        assert_eq!(parsed.width, 2);
        assert_eq!(parsed.pixels, pixels);
    }

    #[test]
    fn parse_bitmap_rejects_mismatched_pixel_count() {
        // Claim width × height = 4 but ship 5 pixels — the header is the
        // source of truth and the runtime should refuse the asset.
        let pixels = [0u16; 5];
        let blob = bitmap_blob(2, 2, &pixels);
        // Hand-rewrite the pixel count to mismatch dimensions.
        let mut blob = blob;
        blob[8..12].copy_from_slice(&5u32.to_le_bytes());
        assert!(parse_bitmap(&blob).is_none());
    }

    #[test]
    fn parse_bitmap_rejects_bad_magic() {
        let mut blob = bitmap_blob(2, 2, &[0u16; 4]);
        blob[0] ^= 0xFF;
        assert!(parse_bitmap(&blob).is_none());
    }
}
