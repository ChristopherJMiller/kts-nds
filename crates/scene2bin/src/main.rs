//! `scene2bin` CLI — a thin wrapper over the [`scene2bin`](../scene2bin/index.html)
//! library for one-off bakes and validation of a level tree:
//!
//! ```text
//! scene2bin --levels assets/levels --out build/nitrofs/levels [--assets assets] [--prefabs assets/prefabs]
//! scene2bin --check  assets/levels [--assets assets] [--prefabs assets/prefabs]
//! ```
//!
//! A *level* is a directory under `--levels` holding a `level.ron` manifest plus
//! one `<zone>.ron` content file per zone; `--prefabs` is the shared prefab
//! library. The directory bake here is the same path `build.rs` drives via
//! [`scene2bin::build_levels_dir`] (the authoritative path that derives
//! connections from the whole level's layout).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("scene2bin: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut levels: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut assets: PathBuf = PathBuf::from("assets");
    let mut prefabs: PathBuf = PathBuf::from("assets/prefabs");
    let mut check_only = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--levels" | "-l" => levels = Some(args.next().ok_or("--levels needs a path")?.into()),
            "--out" | "-o" => out = Some(args.next().ok_or("--out needs a path")?.into()),
            "--assets" => assets = args.next().ok_or("--assets needs a path")?.into(),
            "--prefabs" => prefabs = args.next().ok_or("--prefabs needs a path")?.into(),
            "--check" => {
                check_only = true;
                levels = Some(args.next().ok_or("--check needs a path")?.into());
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    let levels = levels.ok_or("missing --levels <dir> (or --check <dir>)")?;

    // `--check` bakes into a throwaway temp dir so parse/validate/derive all run
    // without touching the build tree.
    let dst = if check_only {
        std::env::temp_dir().join("scene2bin-check")
    } else {
        out.ok_or("missing --out <dir>")?
    };

    let built = scene2bin::build_levels_dir(&levels, &dst, &assets, &prefabs)?;
    for b in &built {
        for w in &b.warnings {
            eprintln!("scene2bin: warning: {w}");
        }
    }

    if check_only {
        let _ = std::fs::remove_dir_all(&dst);
        eprintln!("scene2bin: {} ok ({} zones)", levels.display(), built.len());
    } else {
        eprintln!(
            "scene2bin: baked {} zones from {} -> {}",
            built.len(),
            levels.display(),
            dst.display()
        );
    }
    Ok(())
}

fn print_usage() {
    eprintln!(
        "Usage:\n  \
         scene2bin --levels <dir> --out <dir> [--assets <dir>] [--prefabs <dir>]\n  \
         scene2bin --check <dir> [--assets <dir>] [--prefabs <dir>]\n\n\
         Bakes a tree of RON level directories into .scene NitroFS blobs (issue #27).\n\n\
         Options:\n  \
         -l, --levels <path>   Source levels root (dirs of level.ron + <zone>.ron)\n  \
         -o, --out <path>      Destination root (baked to <out>/<level>/<zone>.scene)\n      \
         --assets <dir>        Geometry root for mesh validation (default: assets)\n      \
         --prefabs <dir>       Prefab library (default: assets/prefabs)\n      \
         --check <path>        Parse + validate + derive only, write nothing\n  \
         -h, --help            Show this help"
    );
}
