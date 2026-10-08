//! Pure Rust, no external tool: the DS texture layout is linear (row-major), so
//! there is nothing for `grit` to tile, and owning the encoder keeps the
//! colour-count rule (authoring contract v1) host-tested.

use std::collections::HashMap;

/// The DS texture formats this pipeline emits. The discriminant is the
/// `TEXIMAGE_PARAM` format code (bits 26–28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TexFormat {
    Pal4 = 2,
    Pal16 = 3,
    Pal256 = 4,
}

impl TexFormat {
    pub fn capacity(self) -> usize {
        match self {
            Self::Pal4 => 4,
            Self::Pal16 => 16,
            Self::Pal256 => 256,
        }
    }

    pub fn bits_per_texel(self) -> usize {
        match self {
            Self::Pal4 => 2,
            Self::Pal16 => 4,
            Self::Pal256 => 8,
        }
    }
}

/// Allowed side lengths (authoring contract v1, #66).
pub const SIZES: [u32; 6] = [8, 16, 32, 64, 128, 256];

/// Magic identifying a `.tex` blob: ASCII `"DST1"`.
pub const TEX_MAGIC: u32 = u32::from_le_bytes(*b"DST1");

/// A baked DS paletted texture.
#[derive(Clone, Debug, PartialEq)]
pub struct DsTexture {
    pub width: u16,
    pub height: u16,
    pub format: TexFormat,
    /// Palette entry 0 is transparent (the source had alpha-0 pixels).
    pub transparent0: bool,
    /// RGB15 entries actually used, ≤ the format's capacity.
    pub palette: Vec<u16>,
    /// Row-major texel indices, first pixel in the lowest bits of each byte.
    pub texels: Vec<u8>,
}

impl DsTexture {
    pub fn texel_bytes(&self) -> u32 {
        self.texels.len() as u32
    }

    /// Palette VRAM cost, rounded up to the 16-byte palette alignment.
    pub fn palette_bytes(&self) -> u32 {
        (self.palette.len() as u32 * 2 + 15) & !15
    }

    /// Serialise to the runtime NitroFS `.tex` format. All little-endian:
    ///
    /// | offset | type      | field                                        |
    /// |--------|-----------|----------------------------------------------|
    /// | 0      | `u32`     | magic [`TEX_MAGIC`]                          |
    /// | 4      | `u16`     | width                                        |
    /// | 6      | `u16`     | height                                       |
    /// | 8      | `u8`      | format ([`TexFormat`] / `TEXIMAGE_PARAM` code) |
    /// | 9      | `u8`      | flags (bit 0: palette entry 0 transparent)   |
    /// | 10     | `u16`     | palette entry count P                        |
    /// | 12     | `u32`     | texel byte count T                           |
    /// | 16     | `u16` × P | palette (RGB15), zero-padded to 4 bytes      |
    /// | …      | `u8` × T  | texels                                       |
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.palette.len() * 2 + 2 + self.texels.len());
        out.extend_from_slice(&TEX_MAGIC.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(self.format as u8);
        out.push(self.transparent0 as u8);
        out.extend_from_slice(&(self.palette.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.texels.len() as u32).to_le_bytes());
        for c in &self.palette {
            out.extend_from_slice(&c.to_le_bytes());
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(&self.texels);
        out
    }
}

/// RGB888 → DS RGB15 (`r | g << 5 | b << 10`, the top five bits of each).
pub fn rgb15(r: u8, g: u8, b: u8) -> u16 {
    (r as u16 >> 3) | ((g as u16 >> 3) << 5) | ((b as u16 >> 3) << 10)
}

/// Encode RGBA8 pixels (row-major) as the smallest paletted format that holds
/// them. Alpha-0 pixels become palette index 0; any other alpha is opaque.
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<DsTexture, String> {
    if !SIZES.contains(&width) || !SIZES.contains(&height) {
        return Err(format!(
            "texture is {width}×{height}; each side must be a power of two from 8 to 256 (authoring contract v1, #66)"
        ));
    }
    let n = (width * height) as usize;
    if rgba.len() != n * 4 {
        return Err(format!("expected {} RGBA bytes, got {}", n * 4, rgba.len()));
    }

    let transparent0 = rgba.chunks_exact(4).any(|p| p[3] == 0);
    let mut palette: Vec<u16> = Vec::new();
    if transparent0 {
        palette.push(0);
    }
    let mut lookup: HashMap<u16, usize> = HashMap::new();
    for p in rgba.chunks_exact(4).filter(|p| p[3] != 0) {
        let c = rgb15(p[0], p[1], p[2]);
        lookup.entry(c).or_insert_with(|| {
            palette.push(c);
            palette.len() - 1
        });
    }

    let format = [TexFormat::Pal4, TexFormat::Pal16, TexFormat::Pal256]
        .into_iter()
        .find(|f| palette.len() <= f.capacity())
        .ok_or_else(|| {
            format!(
                "texture uses {} palette entries after 15-bit conversion{}; the most a DS texture holds is 256 (authoring contract v1, #66)",
                palette.len(),
                if transparent0 { " (incl. one for transparency)" } else { "" }
            )
        })?;

    let indices: Vec<u8> = rgba
        .chunks_exact(4)
        .map(|p| if p[3] == 0 { 0 } else { lookup[&rgb15(p[0], p[1], p[2])] as u8 })
        .collect();

    let bits = format.bits_per_texel();
    let per = 8 / bits;
    let mut texels = vec![0u8; n / per];
    for (i, &ix) in indices.iter().enumerate() {
        texels[i / per] |= ix << ((i % per) * bits);
    }

    Ok(DsTexture {
        width: width as u16,
        height: height as u16,
        format,
        transparent0,
        palette,
        texels,
    })
}

/// Decode any 8- or 16-bit PNG (indexed, grey, RGB, with or without alpha) to
/// RGBA8.
pub fn decode_png_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| format!("not a readable PNG: {e}"))?;
    let size = reader.output_buffer_size().ok_or("PNG is too large to decode")?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("PNG decode failed: {e}"))?;
    let n = (info.width * info.height) as usize;
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..n * 4].to_vec(),
        png::ColorType::Rgb => buf[..n * 3].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..n * 2].chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf[..n].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("PNG is still indexed after expansion".into()),
    };
    Ok((info.width, info.height, rgba))
}

/// Decode a PNG and encode it as a DS texture.
pub fn encode_png(bytes: &[u8]) -> Result<DsTexture, String> {
    let (w, h, rgba) = decode_png_rgba(bytes)?;
    encode_rgba(w, h, &rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RGBA for a `w`×`h` image filled by `f(x, y)`.
    fn img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| f(x, y)).collect()
    }

    /// `n` distinct opaque colours spread over an 8×8 (or 16×16 for n > 64) image.
    fn colours(n: u32) -> (u32, Vec<u8>) {
        let side = if n > 64 { 16 } else { 8 };
        let rgba = img(side, side, |x, y| {
            let i = (y * side + x) % n;
            [((i & 31) << 3) as u8, (((i >> 5) & 31) << 3) as u8, 0, 255]
        });
        (side, rgba)
    }

    fn png(w: u32, h: u32, color: ::png::ColorType, data: &[u8], palette: Option<&[u8]>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = ::png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(::png::BitDepth::Eight);
            if let Some(p) = palette {
                enc.set_palette(p.to_vec());
            }
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(data).unwrap();
            wr.finish().unwrap();
        }
        out
    }

    #[test]
    fn rgb15_takes_top_five_bits() {
        assert_eq!(rgb15(255, 0, 0), 0x001F);
        assert_eq!(rgb15(0, 255, 0), 0x03E0);
        assert_eq!(rgb15(0, 0, 255), 0x7C00);
        assert_eq!(rgb15(7, 7, 7), 0);
    }

    #[test]
    fn format_follows_colour_count() {
        let fmt = |n| {
            let (s, rgba) = colours(n);
            encode_rgba(s, s, &rgba).unwrap().format
        };
        assert_eq!(fmt(2), TexFormat::Pal4);
        assert_eq!(fmt(4), TexFormat::Pal4);
        assert_eq!(fmt(5), TexFormat::Pal16);
        assert_eq!(fmt(16), TexFormat::Pal16);
        assert_eq!(fmt(17), TexFormat::Pal256);
        assert_eq!(fmt(256), TexFormat::Pal256);
    }

    #[test]
    fn transparency_takes_palette_slot_zero() {
        // Four opaque colours + transparent = 5 entries → no longer fits 2bpp.
        let rgba = img(8, 8, |x, _| match x {
            0 => [0, 0, 0, 0],
            n => [(n as u8 % 4) * 64, 0, 0, 255],
        });
        let t = encode_rgba(8, 8, &rgba).unwrap();
        assert!(t.transparent0);
        assert_eq!(t.palette[0], 0);
        assert_eq!(t.format, TexFormat::Pal16);
        // Pixel (0,0) is transparent → index 0 in the low nibble of byte 0.
        assert_eq!(t.texels[0] & 0x0F, 0);
    }

    #[test]
    fn too_many_colours_is_an_error() {
        // 32×16 with a distinct 15-bit colour per pixel: 512 entries.
        let rgba = img(32, 16, |x, y| [(x * 8) as u8, (y * 16) as u8, 0, 255]);
        let err = encode_rgba(32, 16, &rgba).unwrap_err();
        assert!(err.contains("512") && err.contains("256"), "{err}");
    }

    #[test]
    fn colours_merge_after_15_bit_conversion() {
        let rgba = img(8, 8, |x, _| if x % 2 == 0 { [0, 0, 0, 255] } else { [7, 7, 7, 255] });
        let t = encode_rgba(8, 8, &rgba).unwrap();
        assert_eq!(t.palette, vec![0]);
        assert_eq!(t.format, TexFormat::Pal4);
    }

    #[test]
    fn texels_pack_low_bits_first() {
        // Row 0: A, B, A, A, ... → palette [A, B]; 2bpp byte 0 = 0 | 1<<2 = 0x04.
        let rgba = img(8, 8, |x, y| if (x, y) == (1, 0) { [0, 0, 255, 255] } else { [255, 0, 0, 255] });
        let t = encode_rgba(8, 8, &rgba).unwrap();
        assert_eq!(t.format, TexFormat::Pal4);
        assert_eq!(t.palette, vec![0x001F, 0x7C00]);
        assert_eq!(t.texels.len(), 16);
        assert_eq!(t.texels[0], 0x04);
        assert!(t.texels[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn rejects_bad_sizes() {
        for (w, h) in [(12, 8), (512, 8), (4, 8), (8, 0)] {
            let rgba = vec![255u8; (w * h * 4) as usize];
            let err = encode_rgba(w, h, &rgba).unwrap_err();
            assert!(err.contains("power of two"), "{w}x{h}: {err}");
        }
    }

    #[test]
    fn png_colour_types_decode_alike() {
        let rgba = img(8, 8, |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        let want = encode_rgba(8, 8, &rgba).unwrap();

        let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        assert_eq!(encode_png(&png(8, 8, ::png::ColorType::Rgb, &rgb, None)).unwrap(), want);
        assert_eq!(encode_png(&png(8, 8, ::png::ColorType::Rgba, &rgba, None)).unwrap(), want);

        let idx: Vec<u8> = rgba.chunks(4).map(|p| if p[0] == 255 { 0 } else { 1 }).collect();
        let pal = [255, 0, 0, 0, 0, 255];
        assert_eq!(encode_png(&png(8, 8, ::png::ColorType::Indexed, &idx, Some(&pal))).unwrap(), want);

        let grey = img(8, 8, |x, _| if x < 4 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let g: Vec<u8> = grey.chunks(4).map(|p| p[0]).collect();
        assert_eq!(
            encode_png(&png(8, 8, ::png::ColorType::Grayscale, &g, None)).unwrap(),
            encode_rgba(8, 8, &grey).unwrap()
        );
    }

    #[test]
    fn blob_layout() {
        let t = encode_rgba(8, 8, &img(8, 8, |_, _| [255, 0, 0, 255])).unwrap();
        let b = t.to_le_bytes();
        assert_eq!(&b[0..4], b"DST1");
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 8);
        assert_eq!(u16::from_le_bytes([b[6], b[7]]), 8);
        assert_eq!(b[8], TexFormat::Pal4 as u8);
        assert_eq!(b[9], 0); // no transparency
        assert_eq!(u16::from_le_bytes([b[10], b[11]]), 1); // one palette entry
        assert_eq!(u32::from_le_bytes([b[12], b[13], b[14], b[15]]), 16);
        assert_eq!(&b[16..20], &[0x1F, 0x00, 0, 0]); // palette, padded to 4
        assert_eq!(b.len(), 20 + 16);
        assert_eq!(t.palette_bytes(), 16); // 2 bytes rounded up to 16
        assert_eq!(t.texel_bytes(), 16);
    }
}
