//! Host-side PNG → background-asset baker, wrapping BlocksDS's `grit`.
//!
//! Two flavours, distinguished by the immediate subdirectory under
//! `assets/backgrounds/`:
//!
//! - `tiled/<name>.png`  → 4bpp **tile** background (`.bg` blob)
//! - `bitmap/<name>.png` → 16bpp direct-color **bitmap** background (`.bbg`)
//!
//! The shape mirrors [`png2sprite`]: `build.rs` calls [`build_dir`] over the
//! `assets/backgrounds/` tree, the per-PNG blobs land under
//! `build/nitrofs/backgrounds/` (so `just rom` packs them into NitroFS), and a
//! generated Rust constants module ([`emit_rust_consts`]) is `include!`d by
//! the game so paths aren't stringly-typed.
//!
//! Tile output uses `grit -gt -gB4 -m -mR4 -mLs`, which emits a 4bpp tileset
//! + a 16-bit-entry tilemap + a 16-colour palette (the standard text-mode-4bpp
//! background layout in libnds's `<nds/arm9/background.h>`). The MVP pins the
//! tilemap to 32×32 (= 256×256 pixels = one screen-fill) and rejects PNGs
//! whose dimensions are not multiples of 8.
//!
//! Bitmap output uses `grit -gb -gB16`, producing the raw RGB15 pixel buffer
//! the extended-mode bitmap BG (`BgType_Bmp16`) expects. The PNG must be 256
//! pixels wide so each scanline lands aligned in the 256×256 VRAM region the
//! runtime allocates.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// ASCII `"BTB1"` — magic prefix of a baked `.bg` (tile background) file.
pub const TILE_MAGIC: u32 = u32::from_le_bytes(*b"BTB1");
/// ASCII `"BBB1"` — magic prefix of a baked `.bbg` (bitmap background) file.
pub const BITMAP_MAGIC: u32 = u32::from_le_bytes(*b"BBB1");

/// Subdirectory under the NitroFS root holding baked backgrounds, so they
/// can't collide with `sprites/`, `*.dl`, `soundbank.bin`, etc.
pub const NITROFS_SUBDIR: &str = "backgrounds";

/// Immediate subdirectory of `assets/backgrounds/` holding tile-mode PNGs.
pub const TILED_SUBDIR: &str = "tiled";
/// Immediate subdirectory of `assets/backgrounds/` holding bitmap-mode PNGs.
pub const BITMAP_SUBDIR: &str = "bitmap";

/// Pixel width of the tile-BG MVP. 32 tiles × 8 px = 256 px (one screen-fill).
pub const TILE_MAP_WIDTH_PX: u16 = 256;
/// Pixel height of the tile-BG MVP. 32 tiles × 8 px = 256 px.
pub const TILE_MAP_HEIGHT_PX: u16 = 256;
/// Pixel width required for bitmap-BG PNGs. Matches the 256-pixel hardware
/// scanline so per-row copies into VRAM stay aligned.
pub const BITMAP_REQUIRED_WIDTH: u16 = 256;

/// Which flavour of background a given PNG bakes to. Inferred from the
/// `tiled/` vs `bitmap/` subdirectory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Tile,
    Bitmap,
}

impl Kind {
    /// Output file extension produced for blobs of this kind.
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Tile => "bg",
            Kind::Bitmap => "bbg",
        }
    }
}

/// A baked 4bpp tile background, ready to encode.
#[derive(Clone, Debug)]
pub struct TileBg {
    /// 16-entry RGB15 palette.
    pub palette: Vec<u16>,
    /// 4bpp tile gfx as grit emits it (one byte per pair of pixels).
    pub gfx: Vec<u8>,
    /// 16-bit map entries as grit emits them (tile index + flip + palette
    /// bank). 32 × 32 = 1024 entries = 2048 bytes for the MVP.
    pub map: Vec<u8>,
}

/// A baked 16bpp bitmap background, ready to encode.
#[derive(Clone, Debug)]
pub struct BitmapBg {
    /// PNG width in pixels (must equal [`BITMAP_REQUIRED_WIDTH`]).
    pub width: u16,
    /// PNG height in pixels (≤ 256; the runtime copies into a 256-tall slot).
    pub height: u16,
    /// RGB15 pixels in row-major order. The MSB is the "alpha" bit the DS
    /// extended-bitmap mode treats as opaque, set by grit.
    pub pixels: Vec<u16>,
}

/// Descriptor for one PNG → background asset, returned by [`build_dir`] /
/// [`discover`]. The build script uses it for cargo rerun tracking and to
/// feed [`emit_rust_consts`].
#[derive(Clone, Debug)]
pub struct Baked {
    /// Absolute source path (the input PNG).
    pub input: PathBuf,
    /// What we baked it as.
    pub kind: Kind,
    /// Output path components below the NitroFS `backgrounds/` root, e.g.
    /// `["tiled", "forest.bg"]`.
    pub rel: PathBuf,
    /// Constants-module path, e.g. `["tiled", "FOREST"]`. The last element is
    /// the upper-cased asset name; earlier elements name nested modules.
    pub const_path: Vec<String>,
    /// NitroFS path the constant resolves to, e.g.
    /// `"nitro:/backgrounds/tiled/forest.bg"`.
    pub nitrofs_path: String,
}

/// What [`build_dir`] returns: the set of baked backgrounds, for both cargo
/// rerun tracking and constants emission.
#[derive(Default, Debug)]
pub struct Built {
    pub items: Vec<Baked>,
}

impl Built {
    pub fn inputs(&self) -> impl Iterator<Item = &Path> {
        self.items.iter().map(|b| b.input.as_path())
    }
}

/// Locate the `grit` binary that ships with BlocksDS. Honours `$GRIT` first,
/// then falls back to `$BLOCKSDS/tools/grit/grit`, then `$PATH`. Same
/// resolution order as `png2sprite::find_grit`.
pub fn find_grit() -> Option<PathBuf> {
    if let Ok(p) = env::var("GRIT") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(b) = env::var("BLOCKSDS") {
        let path = PathBuf::from(b).join("tools/grit/grit");
        if path.is_file() {
            return Some(path);
        }
    }
    which("grit")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Bake a tile-BG PNG into a [`TileBg`] in memory using `grit`. `work` is a
/// scratch directory grit writes its intermediate `.img.bin` / `.map.bin` /
/// `.pal.bin` files into.
pub fn bake_tile(grit: &Path, png: &Path, work: &Path) -> Result<TileBg, String> {
    fs::create_dir_all(work).map_err(|e| format!("mkdir {}: {e}", work.display()))?;
    let stem = png
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("bad PNG name {}", png.display()))?;
    let out_base = work.join(stem);

    let status = Command::new(grit)
        .arg(png)
        .args([
            "-gt",   // tile output
            "-gB4",  // 4 bpp
            "-gT0",  // transparent palette index 0
            "-mR4",  // map: reduce tiles + flip (the standard text-BG layout)
            "-mLs",  // map: 16-bit entries (tile + flip + palette bank)
            "-p",    // include palette
            "-pu16", // u16 palette entries
            "-ftb",  // binary output
            "-fh!",  // no C header
        ])
        .arg(format!("-o{}", out_base.display()))
        .status()
        .map_err(|e| format!("spawn grit: {e}"))?;
    if !status.success() {
        return Err(format!("grit failed on {}", png.display()));
    }

    let img_path = work.join(format!("{stem}.img.bin"));
    let map_path = work.join(format!("{stem}.map.bin"));
    let pal_path = work.join(format!("{stem}.pal.bin"));
    let gfx = fs::read(&img_path).map_err(|e| format!("read {}: {e}", img_path.display()))?;
    let map = fs::read(&map_path).map_err(|e| format!("read {}: {e}", map_path.display()))?;
    let pal_bytes = fs::read(&pal_path).map_err(|e| format!("read {}: {e}", pal_path.display()))?;
    if pal_bytes.len() % 2 != 0 {
        return Err(format!(
            "palette length {} not a multiple of 2 (u16)",
            pal_bytes.len()
        ));
    }
    let palette: Vec<u16> = pal_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(TileBg { palette, gfx, map })
}

/// Bake a bitmap-BG PNG into a [`BitmapBg`] in memory. grit handles the PNG
/// decode + RGB15 conversion; we just dimension-check and forward.
pub fn bake_bitmap(grit: &Path, png: &Path, work: &Path) -> Result<BitmapBg, String> {
    fs::create_dir_all(work).map_err(|e| format!("mkdir {}: {e}", work.display()))?;
    let stem = png
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("bad PNG name {}", png.display()))?;
    let out_base = work.join(stem);

    let (width, height) = png_dimensions(png)?;
    if width != BITMAP_REQUIRED_WIDTH {
        return Err(format!(
            "{} is {}px wide; bitmap backgrounds require {}",
            png.display(),
            width,
            BITMAP_REQUIRED_WIDTH
        ));
    }
    if height == 0 || height > 256 {
        return Err(format!(
            "{} is {}px tall; bitmap backgrounds need 1..=256",
            png.display(),
            height
        ));
    }

    let status = Command::new(grit)
        .arg(png)
        .args([
            "-gb",   // bitmap output (no tiling)
            "-gB16", // 16 bpp (RGB15 + alpha bit, as the DS extended-bitmap BG wants)
            "-ftb",  // binary output
            "-fh!",  // no C header
        ])
        .arg(format!("-o{}", out_base.display()))
        .status()
        .map_err(|e| format!("spawn grit: {e}"))?;
    if !status.success() {
        return Err(format!("grit failed on {}", png.display()));
    }

    let img_path = work.join(format!("{stem}.img.bin"));
    let bytes = fs::read(&img_path).map_err(|e| format!("read {}: {e}", img_path.display()))?;
    let expected = (width as usize) * (height as usize) * 2;
    if bytes.len() != expected {
        return Err(format!(
            "{} gfx is {} bytes, expected {} for {}x{} 16bpp",
            png.display(),
            bytes.len(),
            expected,
            width,
            height,
        ));
    }
    let pixels: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(BitmapBg {
        width,
        height,
        pixels,
    })
}

/// Read width/height from a PNG file's IHDR chunk. Just enough of the spec to
/// validate input dimensions; we don't decode the pixels (grit does that).
fn png_dimensions(png: &Path) -> Result<(u16, u16), String> {
    let bytes = fs::read(png).map_err(|e| format!("read {}: {e}", png.display()))?;
    // PNG signature (8) + length (4) + "IHDR" (4) = 16; width @ 16..20, height @ 20..24.
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err(format!("{} is not a PNG", png.display()));
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w > u16::MAX as u32 || h > u16::MAX as u32 {
        return Err(format!("{} is too big ({}x{})", png.display(), w, h));
    }
    Ok((w as u16, h as u16))
}

/// Serialise a [`TileBg`] to the on-disk `.bg` format. Little-endian
/// throughout:
///
/// | offset | type      | field                              |
/// |--------|-----------|------------------------------------|
/// | 0      | `u32`     | magic [`TILE_MAGIC`] (`"BTB1"`)    |
/// | 4      | `u32`     | palette entry count (P)            |
/// | 8      | `u32`     | gfx byte count (G)                 |
/// | 12     | `u32`     | map byte count (M)                 |
/// | 16     | `u16` × P | palette (RGB15)                    |
/// | …      | `u8` × G  | gfx (4bpp tiles)                   |
/// | …      | `u8` × M  | map (16-bit entries, little-endian)|
pub fn encode_tile(bg: &TileBg) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + bg.palette.len() * 2 + bg.gfx.len() + bg.map.len());
    out.extend_from_slice(&TILE_MAGIC.to_le_bytes());
    out.extend_from_slice(&(bg.palette.len() as u32).to_le_bytes());
    out.extend_from_slice(&(bg.gfx.len() as u32).to_le_bytes());
    out.extend_from_slice(&(bg.map.len() as u32).to_le_bytes());
    for &p in &bg.palette {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out.extend_from_slice(&bg.gfx);
    out.extend_from_slice(&bg.map);
    out
}

/// Serialise a [`BitmapBg`] to the on-disk `.bbg` format. Little-endian:
///
/// | offset | type      | field                              |
/// |--------|-----------|------------------------------------|
/// | 0      | `u32`     | magic [`BITMAP_MAGIC`] (`"BBB1"`)  |
/// | 4      | `u16`     | width (px)                         |
/// | 6      | `u16`     | height (px)                        |
/// | 8      | `u32`     | pixel count (= width × height)     |
/// | 12     | `u16` × N | pixels (RGB15 + alpha bit)         |
pub fn encode_bitmap(bg: &BitmapBg) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + bg.pixels.len() * 2);
    out.extend_from_slice(&BITMAP_MAGIC.to_le_bytes());
    out.extend_from_slice(&bg.width.to_le_bytes());
    out.extend_from_slice(&bg.height.to_le_bytes());
    out.extend_from_slice(&(bg.pixels.len() as u32).to_le_bytes());
    for &p in &bg.pixels {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out
}

/// Convert a PNG file stem to the constant name `wav2bank::sample_const_name`
/// would yield: upper-cased, non-alphanumerics turned to `_`.
pub fn const_name(stem: &str) -> String {
    let mut s = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_uppercase());
        } else {
            s.push('_');
        }
    }
    s
}

/// Lower-case + non-alphanumeric → `_`, used for nested module names that
/// mirror subdirectories. Rust identifiers must start with a letter; we
/// prepend `_` if the first character is a digit.
fn module_name(component: &str) -> String {
    let mut s = String::new();
    for c in component.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else {
            s.push('_');
        }
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

/// Walk `src` recursively, returning a [`Baked`] descriptor for every PNG
/// found under `tiled/` or `bitmap/`. Does **not** invoke `grit` — used both
/// by [`build_dir`] (to drive the bake loop) and by [`predict_dir`] (so the
/// constants module can be emitted even when grit isn't available).
pub fn discover(src: &Path) -> Result<Vec<Baked>, String> {
    let mut out = Vec::new();
    if !src.is_dir() {
        return Ok(out);
    }
    for (subdir, kind) in [(TILED_SUBDIR, Kind::Tile), (BITMAP_SUBDIR, Kind::Bitmap)] {
        let root = src.join(subdir);
        if !root.is_dir() {
            continue;
        }
        let mut rel_components = vec![subdir.to_string()];
        discover_into(&root, kind, &mut rel_components, &mut out)?;
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn discover_into(
    dir: &Path,
    kind: Kind,
    rel_components: &mut Vec<String>,
    out: &mut Vec<Baked>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ty = entry
            .file_type()
            .map_err(|e| format!("file_type {}: {e}", path.display()))?;
        if ty.is_dir() {
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| format!("bad dir name {}", path.display()))?
                .to_string();
            rel_components.push(name);
            discover_into(&path, kind, rel_components, out)?;
            rel_components.pop();
            continue;
        }
        if path.extension().and_then(OsStr::to_str) != Some("png") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| format!("bad PNG name {}", path.display()))?
            .to_string();

        let mut rel: PathBuf = rel_components.iter().collect();
        rel.push(format!("{stem}.{}", kind.extension()));

        let mut nitrofs = String::from("nitro:/");
        nitrofs.push_str(NITROFS_SUBDIR);
        nitrofs.push('/');
        for comp in rel_components.iter() {
            nitrofs.push_str(comp);
            nitrofs.push('/');
        }
        nitrofs.push_str(&stem);
        nitrofs.push('.');
        nitrofs.push_str(kind.extension());

        let mut const_path: Vec<String> = rel_components.iter().map(|c| module_name(c)).collect();
        const_path.push(const_name(&stem));

        out.push(Baked {
            input: path,
            kind,
            rel,
            const_path,
            nitrofs_path: nitrofs,
        });
    }
    Ok(())
}

/// Bake every PNG under `src` (split by `tiled/` vs `bitmap/`) into the
/// matching blob under `dst/<rel>.{bg,bbg}`. Returns a [`Built`] descriptor.
pub fn build_dir(src: &Path, dst: &Path, grit: &Path, work: &Path) -> Result<Built, String> {
    fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    let items = discover(src)?;

    for baked in &items {
        let out_path = dst.join(&baked.rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let mut work_sub = work.to_path_buf();
        for comp in baked.rel.parent().into_iter().flatten() {
            work_sub.push(comp);
        }
        let bytes = match baked.kind {
            Kind::Tile => encode_tile(&bake_tile(grit, &baked.input, &work_sub)?),
            Kind::Bitmap => encode_bitmap(&bake_bitmap(grit, &baked.input, &work_sub)?),
        };
        fs::write(&out_path, &bytes).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    }
    Ok(Built { items })
}

/// Like [`discover`], for the build.rs "grit missing" path so the game still
/// compiles — `bevy_nds_bg` will simply fail to load any background at
/// runtime.
pub fn predict_dir(src: &Path) -> Result<Vec<Baked>, String> {
    discover(src)
}

/// Emit a Rust source string declaring one `pub const NAME: &[u8]` per baked
/// background, with subdirectories rendered as nested `pub mod` blocks. Each
/// constant is a NUL-terminated byte literal suitable for handing to
/// `bevy_nds_nitrofs::read_file`.
pub fn emit_rust_consts(items: &[Baked]) -> String {
    let mut root = Node::default();
    for item in items {
        root.insert(&item.const_path, &item.nitrofs_path);
    }
    let mut s = String::new();
    s.push_str("// @generated by png2bg from assets/backgrounds/{tiled,bitmap}/**/*.png.\n");
    s.push_str("// Each constant is a NUL-terminated NitroFS path you can pass to\n");
    s.push_str("// `Backgrounds::set_tile` (under `tiled::`) or `Backgrounds::set_bitmap`\n");
    s.push_str("// (under `bitmap::`).\n");
    root.render(&mut s, 0);
    s
}

#[derive(Default)]
struct Node {
    leaves: Vec<(String, String)>,
    children: std::collections::BTreeMap<String, Node>,
}

impl Node {
    fn insert(&mut self, path: &[String], nitrofs_path: &str) {
        match path {
            [] => unreachable!("const_path is always non-empty"),
            [leaf] => self.leaves.push((leaf.clone(), nitrofs_path.to_string())),
            [head, rest @ ..] => self
                .children
                .entry(head.clone())
                .or_default()
                .insert(rest, nitrofs_path),
        }
    }

    fn render(&self, out: &mut String, depth: usize) {
        let pad = "    ".repeat(depth);
        for (name, path) in &self.leaves {
            out.push_str(&pad);
            out.push_str("pub const ");
            out.push_str(name);
            out.push_str(": &[u8] = b\"");
            out.push_str(path);
            out.push_str("\\0\";\n");
        }
        for (mod_name, child) in &self.children {
            out.push_str(&pad);
            out.push_str("pub mod ");
            out.push_str(mod_name);
            out.push_str(" {\n");
            child.render(out, depth + 1);
            out.push_str(&pad);
            out.push_str("}\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_header_layout_is_stable() {
        let bg = TileBg {
            palette: vec![0x0001, 0x0002],
            gfx: vec![0xAA, 0xBB],
            map: vec![0xCC, 0xDD, 0xEE, 0xFF],
        };
        let bytes = encode_tile(&bg);
        assert_eq!(&bytes[0..4], b"BTB1");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 0x0001);
        assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 0x0002);
        assert_eq!(&bytes[20..22], &[0xAA, 0xBB]);
        assert_eq!(&bytes[22..26], &[0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(bytes.len(), 16 + 4 + 2 + 4);
    }

    #[test]
    fn bitmap_header_layout_is_stable() {
        let bg = BitmapBg {
            width: 256,
            height: 2,
            pixels: vec![0x7FFF, 0x0001, 0x8000, 0xABCD],
        };
        let bytes = encode_bitmap(&bg);
        assert_eq!(&bytes[0..4], b"BBB1");
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 256);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 2);
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes([bytes[12], bytes[13]]), 0x7FFF);
        assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 0xABCD);
        assert_eq!(bytes.len(), 12 + 4 * 2);
    }

    #[test]
    fn const_name_upper_cases_and_sanitises() {
        assert_eq!(const_name("forest"), "FOREST");
        assert_eq!(const_name("hill-01"), "HILL_01");
        assert_eq!(const_name("photo.day"), "PHOTO_DAY");
    }

    #[test]
    fn module_name_is_lower_snake_safe_for_identifiers() {
        assert_eq!(module_name("tiled"), "tiled");
        assert_eq!(module_name("Bitmap"), "bitmap");
        assert_eq!(module_name("2d"), "_2d");
    }

    #[test]
    fn kind_extension_picks_per_flavour() {
        assert_eq!(Kind::Tile.extension(), "bg");
        assert_eq!(Kind::Bitmap.extension(), "bbg");
    }

    #[test]
    fn emit_rust_consts_separates_tiled_and_bitmap() {
        let items = vec![
            Baked {
                input: PathBuf::from("/dev/null"),
                kind: Kind::Tile,
                rel: PathBuf::from("tiled/forest.bg"),
                const_path: vec!["tiled".into(), "FOREST".into()],
                nitrofs_path: "nitro:/backgrounds/tiled/forest.bg".into(),
            },
            Baked {
                input: PathBuf::from("/dev/null"),
                kind: Kind::Bitmap,
                rel: PathBuf::from("bitmap/photo.bbg"),
                const_path: vec!["bitmap".into(), "PHOTO".into()],
                nitrofs_path: "nitro:/backgrounds/bitmap/photo.bbg".into(),
            },
        ];
        let src = emit_rust_consts(&items);
        assert!(src.contains("pub mod tiled {"));
        assert!(src.contains("pub mod bitmap {"));
        assert!(
            src.contains("pub const FOREST: &[u8] = b\"nitro:/backgrounds/tiled/forest.bg\\0\";")
        );
        assert!(
            src.contains("pub const PHOTO: &[u8] = b\"nitro:/backgrounds/bitmap/photo.bbg\\0\";")
        );
    }

    #[test]
    fn discover_routes_subdirs_to_the_right_kind() {
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("tiled")).unwrap();
        std::fs::create_dir_all(tmp.join("bitmap")).unwrap();
        std::fs::write(tmp.join("tiled/forest.png"), b"").unwrap();
        std::fs::write(tmp.join("bitmap/photo.png"), b"").unwrap();
        // PNGs at the root are ignored — kind is ambiguous, so the discovery
        // requires the user to commit one way or the other.
        std::fs::write(tmp.join("loose.png"), b"").unwrap();

        let items = discover(&tmp).unwrap();
        let pairs: Vec<_> = items
            .iter()
            .map(|b| (b.kind, b.nitrofs_path.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (Kind::Bitmap, "nitro:/backgrounds/bitmap/photo.bbg"),
                (Kind::Tile, "nitro:/backgrounds/tiled/forest.bg"),
            ]
        );
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("png2bg-test-{nonce}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
