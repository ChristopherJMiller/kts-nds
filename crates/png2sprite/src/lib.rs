//! Host-side PNG → `.sprite` baker, wrapping BlocksDS's `grit`.
//!
//! Mirrors the `obj2dl` / `wav2bank` shape: a `build.rs` calls [`build_dir`]
//! over `assets/sprites/**/*.png` (recursive) and the results land under
//! `build/nitrofs/sprites/` so `just rom` can pack them into the ROM
//! filesystem. `bevy_nds_sprite` reads the resulting `.sprite` blobs at runtime
//! via the `SpriteAssets` registry.
//!
//! Each PNG is fed through `grit` with these flags (see grit's `--help`):
//!
//! - `-gt`     tile output (the OAM layout)
//! - `-gB4`    4 bits per pixel (16-colour palette per sprite)
//! - `-gT 0`   transparent colour is palette index 0
//! - `-p`      include palette
//! - `-ftb`    binary (`.bin`) output
//! - `-fh!`    no C header
//!
//! grit produces `<name>.img.bin` (gfx) and `<name>.pal.bin` (palette). We
//! repack them with a small header into a single `<name>.sprite` file so the
//! runtime only has one path to load.
//!
//! Alongside the binaries, the host crate emits a Rust module of NitroFS
//! paths (via [`emit_rust_consts`]) that the game `include!`s — analogous to
//! the `sounds.rs` `wav2bank` emits — so game code refers to sprites by
//! name (`sprites::CURSOR`) instead of stringly-typed paths.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// ASCII `"BSP1"` — magic prefix of a baked `.sprite` file.
pub const ASSET_MAGIC: u32 = u32::from_le_bytes(*b"BSP1");

/// Subdirectory under the NitroFS root that holds baked sprites. Both the
/// on-disk layout (`build/nitrofs/sprites/...`) and the generated constants
/// (`nitro:/sprites/...`) live here, so they cannot collide with `teapot.dl`,
/// `soundbank.bin`, etc.
pub const NITROFS_SUBDIR: &str = "sprites";

/// Bake options for a single PNG.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Sprite width in pixels (must match the PNG width).
    pub width: u16,
    /// Sprite height in pixels (must match the PNG height).
    pub height: u16,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 16,
            height: 16,
        }
    }
}

/// A baked sprite: header + palette + gfx, ready to write to disk.
#[derive(Clone, Debug)]
pub struct Sprite {
    pub width: u16,
    pub height: u16,
    /// 16-entry RGB15 palette (the first entry is transparent).
    pub palette: Vec<u16>,
    /// 4bpp tile gfx in 1D-32 tile order, as grit emits it.
    pub gfx: Vec<u8>,
}

/// One PNG's bake metadata, returned by [`build_dir`] / [`predict_dir`] so
/// the build script can both register `cargo:rerun-if-changed` deps and feed
/// the constants emitter.
#[derive(Clone, Debug)]
pub struct Baked {
    /// Absolute source path (the input PNG).
    pub input: PathBuf,
    /// Output `.sprite` path components below `dst`, e.g. `["ui", "cursor.sprite"]`.
    pub rel: PathBuf,
    /// Constants-module path, e.g. `["ui", "CURSOR"]`. The last element is the
    /// upper-cased asset name; everything before names a nested module.
    pub const_path: Vec<String>,
    /// NitroFS path the constant resolves to, e.g. `"nitro:/sprites/ui/cursor.sprite"`.
    pub nitrofs_path: String,
}

/// What [`build_dir`] returns: the set of baked sprites, for both
/// cargo rerun tracking and constants emission.
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
/// then falls back to `$BLOCKSDS/tools/grit/grit`, then `$PATH`.
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

/// Bake one PNG into a [`Sprite`] in memory using `grit`. `work` is a scratch
/// directory grit writes its intermediate `.img.bin` / `.pal.bin` files into.
pub fn bake(grit: &Path, png: &Path, work: &Path, opts: &Options) -> Result<Sprite, String> {
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
    let pal_path = work.join(format!("{stem}.pal.bin"));
    let gfx = fs::read(&img_path).map_err(|e| format!("read {}: {e}", img_path.display()))?;
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

    // Sanity: 4bpp 16x16 should be 128 bytes of gfx + 16 palette entries. We
    // don't hard-fail on mismatches (grit may emit a longer palette for
    // colour-quantisation reasons), but we do warn.
    let expected_gfx = (opts.width as usize) * (opts.height as usize) / 2;
    if gfx.len() != expected_gfx {
        // Print on stderr so cargo:warning picks it up from build.rs.
        eprintln!(
            "png2sprite: warning: {} gfx is {} bytes, expected {} for {}x{} 4bpp",
            png.display(),
            gfx.len(),
            expected_gfx,
            opts.width,
            opts.height,
        );
    }

    Ok(Sprite {
        width: opts.width,
        height: opts.height,
        palette,
        gfx,
    })
}

/// Serialise a [`Sprite`] to the on-disk `.sprite` format. Little-endian
/// throughout:
///
/// | offset | type      | field                              |
/// |--------|-----------|------------------------------------|
/// | 0      | `u32`     | magic [`ASSET_MAGIC`] (`"BSP1"`)   |
/// | 4      | `u16`     | width (pixels)                     |
/// | 6      | `u16`     | height (pixels)                    |
/// | 8      | `u32`     | palette entry count                |
/// | 12     | `u32`     | gfx byte count                     |
/// | 16     | `u16` × P | palette (RGB15)                    |
/// | 16+2P  | `u8` × G  | gfx (4bpp tiles, 1D-32 order)      |
pub fn encode(sprite: &Sprite) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + sprite.palette.len() * 2 + sprite.gfx.len());
    out.extend_from_slice(&ASSET_MAGIC.to_le_bytes());
    out.extend_from_slice(&sprite.width.to_le_bytes());
    out.extend_from_slice(&sprite.height.to_le_bytes());
    out.extend_from_slice(&(sprite.palette.len() as u32).to_le_bytes());
    out.extend_from_slice(&(sprite.gfx.len() as u32).to_le_bytes());
    for &p in &sprite.palette {
        out.extend_from_slice(&p.to_le_bytes());
    }
    out.extend_from_slice(&sprite.gfx);
    out
}

/// Convert a PNG file stem to the constant name `wav2bank::sample_const_name`
/// would yield: upper-cased, non-alphanumerics turned to `_`. No prefix —
/// the enclosing module already says these are sprites.
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

/// Lower-case + non-alphanumeric → `_`, used for the nested module names that
/// mirror subdirectories under `assets/sprites/`. Rust identifiers must start
/// with a letter; we prepend `_` if the first character is a digit.
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
/// found. Does **not** invoke `grit` — used both by [`build_dir`] (to drive
/// the bake loop) and by [`predict_dir`] (to emit the constants module when
/// grit isn't available).
pub fn discover(src: &Path) -> Result<Vec<Baked>, String> {
    let mut out = Vec::new();
    if !src.is_dir() {
        return Ok(out);
    }
    discover_into(src, &mut Vec::new(), &mut out)?;
    // Stable order so generated constants don't churn between builds.
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn discover_into(
    dir: &Path,
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
            discover_into(&path, rel_components, out)?;
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

        // On-disk relative output (`build/nitrofs/sprites/<rel>.sprite`).
        let mut rel: PathBuf = rel_components.iter().collect();
        rel.push(format!("{stem}.sprite"));

        // NitroFS path the runtime opens. Always uses `/` separators.
        let mut nitrofs = String::from("nitro:/");
        nitrofs.push_str(NITROFS_SUBDIR);
        nitrofs.push('/');
        for comp in rel_components.iter() {
            nitrofs.push_str(comp);
            nitrofs.push('/');
        }
        nitrofs.push_str(&stem);
        nitrofs.push_str(".sprite");

        // Constants-module path: nested modules per subdirectory, leaf is
        // the upper-cased stem.
        let mut const_path: Vec<String> = rel_components.iter().map(|c| module_name(c)).collect();
        const_path.push(const_name(&stem));

        out.push(Baked {
            input: path,
            rel,
            const_path,
            nitrofs_path: nitrofs,
        });
    }
    Ok(())
}

/// Bake every `*.png` under `src` (recursive) into `dst/<rel>.sprite`. Returns
/// the [`Built`] descriptor for cargo rerun tracking and constants emission.
pub fn build_dir(
    src: &Path,
    dst: &Path,
    grit: &Path,
    work: &Path,
    opts: &Options,
) -> Result<Built, String> {
    fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    let items = discover(src)?;

    for baked in &items {
        let out_path = dst.join(&baked.rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        // Each PNG gets its own scratch subdir so grit's `<stem>.img.bin` /
        // `<stem>.pal.bin` outputs don't collide across nested folders that
        // happen to use the same stem (e.g. `ui/cursor.png` and `cursor.png`).
        let mut work_sub = work.to_path_buf();
        for comp in baked.rel.parent().into_iter().flatten() {
            work_sub.push(comp);
        }
        let sprite = bake(grit, &baked.input, &work_sub, opts)?;
        let bytes = encode(&sprite);
        fs::write(&out_path, &bytes).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    }

    Ok(Built { items })
}

/// Like [`discover`], but exposed for the build.rs "grit missing" path so the
/// game can still compile — `bevy_nds_sprite` will simply fail to load any
/// sprite at runtime.
pub fn predict_dir(src: &Path) -> Result<Vec<Baked>, String> {
    discover(src)
}

/// Emit a Rust source string that declares one `pub const NAME: &[u8]` per
/// baked sprite, with subdirectories rendered as nested `pub mod` blocks.
/// Each constant is a NUL-terminated byte literal suitable for handing to
/// `bevy_nds_nitrofs::read_file`.
pub fn emit_rust_consts(items: &[Baked]) -> String {
    let mut root = Node::default();
    for item in items {
        root.insert(&item.const_path, &item.nitrofs_path);
    }
    let mut s = String::new();
    s.push_str("// @generated by png2sprite from assets/sprites/**/*.png.\n");
    s.push_str("// Each constant is a NUL-terminated NitroFS path you can pass\n");
    s.push_str("// to `bevy_nds_nitrofs::read_file` or stash in a `Sprite.image`.\n");
    root.render(&mut s, 0);
    s
}

/// In-memory tree used by [`emit_rust_consts`] to group leaf constants by
/// their parent module path.
#[derive(Default)]
struct Node {
    /// Direct leaf constants at this module (NAME → nitrofs path).
    leaves: Vec<(String, String)>,
    /// Nested children, keyed by module name.
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

    /// The header encoding matches the on-disk layout exactly (so the runtime
    /// loader's offsets stay in sync with this writer).
    #[test]
    fn header_layout_is_stable() {
        let sprite = Sprite {
            width: 16,
            height: 16,
            palette: vec![0x0001, 0x0002, 0x0003],
            gfx: vec![0xAA, 0xBB, 0xCC, 0xDD],
        };
        let bytes = encode(&sprite);
        // magic
        assert_eq!(&bytes[0..4], b"BSP1");
        // width / height
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 16);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 16);
        // palette count, gfx count
        assert_eq!(
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            3
        );
        assert_eq!(
            u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            4
        );
        // palette entries
        assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 0x0001);
        assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 0x0002);
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 0x0003);
        // gfx bytes
        assert_eq!(&bytes[22..26], &[0xAA, 0xBB, 0xCC, 0xDD]);
        // total length
        assert_eq!(bytes.len(), 16 + 6 + 4);
    }

    #[test]
    fn const_name_upper_cases_and_sanitises() {
        assert_eq!(const_name("cursor"), "CURSOR");
        assert_eq!(const_name("hit-01"), "HIT_01");
        assert_eq!(const_name("player.run"), "PLAYER_RUN");
    }

    #[test]
    fn module_name_is_lower_snake_safe_for_identifiers() {
        assert_eq!(module_name("ui"), "ui");
        assert_eq!(module_name("Ui-Bits"), "ui_bits");
        assert_eq!(module_name("2d"), "_2d");
    }

    #[test]
    fn emit_rust_consts_groups_by_module() {
        let items = vec![
            Baked {
                input: PathBuf::from("/dev/null"),
                rel: PathBuf::from("cursor.sprite"),
                const_path: vec!["CURSOR".into()],
                nitrofs_path: "nitro:/sprites/cursor.sprite".into(),
            },
            Baked {
                input: PathBuf::from("/dev/null"),
                rel: PathBuf::from("ui/cursor.sprite"),
                const_path: vec!["ui".into(), "CURSOR".into()],
                nitrofs_path: "nitro:/sprites/ui/cursor.sprite".into(),
            },
            Baked {
                input: PathBuf::from("/dev/null"),
                rel: PathBuf::from("ui/select.sprite"),
                const_path: vec!["ui".into(), "SELECT".into()],
                nitrofs_path: "nitro:/sprites/ui/select.sprite".into(),
            },
        ];
        let src = emit_rust_consts(&items);
        // Root-level constant.
        assert!(src.contains("pub const CURSOR: &[u8] = b\"nitro:/sprites/cursor.sprite\\0\";"));
        // Nested module with two constants.
        assert!(src.contains("pub mod ui {"));
        assert!(src.contains("pub const CURSOR: &[u8] = b\"nitro:/sprites/ui/cursor.sprite\\0\";"));
        assert!(src.contains("pub const SELECT: &[u8] = b\"nitro:/sprites/ui/select.sprite\\0\";"));
    }

    #[test]
    fn discover_walks_subdirs_in_sorted_order() {
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("ui")).unwrap();
        // 1x1 PNG bytes wouldn't be valid, but discover() doesn't read them.
        std::fs::write(tmp.join("cursor.png"), b"").unwrap();
        std::fs::write(tmp.join("ui/select.png"), b"").unwrap();
        std::fs::write(tmp.join("ui/cursor.png"), b"").unwrap();

        let items = discover(&tmp).unwrap();
        let paths: Vec<_> = items.iter().map(|b| b.nitrofs_path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "nitro:/sprites/cursor.sprite",
                "nitro:/sprites/ui/cursor.sprite",
                "nitro:/sprites/ui/select.sprite",
            ]
        );
        assert_eq!(items[0].const_path, vec!["CURSOR".to_string()]);
        assert_eq!(
            items[1].const_path,
            vec!["ui".to_string(), "CURSOR".to_string()]
        );
    }

    /// Tiny tempdir helper so the test doesn't need an external dep.
    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("png2sprite-test-{nonce}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
