//! `scene2bin` CLI — a thin wrapper over the [`scene2bin`](../scene2bin/index.html)
//! library for one-off conversions and validation:
//!
//! ```text
//! scene2bin --input assets/spaces/atrium.ron --output build/nitrofs/spaces/atrium.scene [--assets assets]
//! scene2bin --check  assets/spaces/atrium.ron [--assets assets]
//! ```

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
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut assets: PathBuf = PathBuf::from("assets");
    let mut check_only = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" | "-i" => input = Some(args.next().ok_or("--input needs a path")?.into()),
            "--output" | "-o" => output = Some(args.next().ok_or("--output needs a path")?.into()),
            "--assets" => assets = args.next().ok_or("--assets needs a path")?.into(),
            "--check" => {
                check_only = true;
                input = Some(args.next().ok_or("--check needs a path")?.into());
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    let input = input.ok_or("missing --input <file.ron>")?;
    let src = std::fs::read_to_string(&input)
        .map_err(|e| format!("could not read {}: {e}", input.display()))?;
    let space = scene2bin::parse_ron(&src).map_err(|e| format!("{}: {e}", input.display()))?;

    let mesh_exists = |name: &str| assets.join(format!("{name}.obj")).is_file();
    scene2bin::validate(&space, mesh_exists).map_err(|e| format!("{}: {e}", input.display()))?;
    for w in scene2bin::validate_warnings(&space, |_| true) {
        eprintln!("scene2bin: warning: {}: {w}", input.display());
    }

    if check_only {
        eprintln!(
            "scene2bin: {} ok ({} instances, {} exits)",
            input.display(),
            space.instances.len(),
            space.exits.len()
        );
        return Ok(());
    }

    let output = output.ok_or("missing --output <file.scene>")?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let blob = scene2bin::encode(&space);
    std::fs::write(&output, &blob)
        .map_err(|e| format!("could not write {}: {e}", output.display()))?;
    eprintln!(
        "scene2bin: {} -> {} ({} bytes)",
        input.display(),
        output.display(),
        blob.len()
    );
    Ok(())
}

fn print_usage() {
    eprintln!(
        "Usage:\n  \
         scene2bin --input <file.ron> --output <file.scene> [--assets <dir>]\n  \
         scene2bin --check <file.ron> [--assets <dir>]\n\n\
         Bakes a RON space sidecar into a .scene NitroFS blob (issue #27).\n\n\
         Options:\n  \
         -i, --input <path>    Source .ron space file\n  \
         -o, --output <path>   Destination .scene file (parent dirs are created)\n      \
         --assets <dir>        Geometry root for mesh validation (default: assets)\n      \
         --check <path>        Parse + validate only, write nothing\n  \
         -h, --help            Show this help"
    );
}
