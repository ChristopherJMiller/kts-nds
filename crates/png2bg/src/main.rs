//! Tiny CLI around the `png2bg` library. Not used by the demo's `build.rs`
//! (which calls into the library directly) — handy for `cargo run -p png2bg
//! -- tiled assets/backgrounds/tiled/forest.png` style spot-baking.

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(kind_arg) = args.next() else {
        return usage();
    };
    let Some(input) = args.next().map(PathBuf::from) else {
        return usage();
    };
    let kind = match kind_arg.to_str() {
        Some("tile") | Some("tiled") => png2bg::Kind::Tile,
        Some("bitmap") | Some("bmp") => png2bg::Kind::Bitmap,
        _ => return usage(),
    };
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&input).with_extension(kind.extension()));

    let grit = match png2bg::find_grit() {
        Some(g) => g,
        None => {
            eprintln!(
                "png2bg: grit not found. Set $GRIT, run inside `nix develop` \
                 (which sets $BLOCKSDS), or add grit to PATH."
            );
            return ExitCode::from(2);
        }
    };
    let work = env::temp_dir().join("png2bg-cli");

    let bytes = match kind {
        png2bg::Kind::Tile => match png2bg::bake_tile(&grit, &input, &work) {
            Ok(bg) => png2bg::encode_tile(&bg),
            Err(e) => {
                eprintln!("png2bg: {e}");
                return ExitCode::from(1);
            }
        },
        png2bg::Kind::Bitmap => match png2bg::bake_bitmap(&grit, &input, &work) {
            Ok(bg) => png2bg::encode_bitmap(&bg),
            Err(e) => {
                eprintln!("png2bg: {e}");
                return ExitCode::from(1);
            }
        },
    };
    if let Err(e) = std::fs::write(&output, &bytes) {
        eprintln!("png2bg: write {}: {e}", output.display());
        return ExitCode::from(1);
    }
    println!("wrote {} ({} bytes)", output.display(), bytes.len());
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!("usage: png2bg <tile|bitmap> <input.png> [output]");
    ExitCode::from(2)
}
