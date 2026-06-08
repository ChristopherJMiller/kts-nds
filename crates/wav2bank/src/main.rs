//! `wav2bank` CLI — a thin wrapper over the [`wav2bank`](../wav2bank/index.html)
//! library for one-off soundbank bakes:
//!
//! ```text
//! wav2bank --input audio --output build/nitrofs/soundbank.bin --ids src/generated/sounds.rs
//! ```
//!
//! `mmutil` is found via `$MMUTIL`, `$BLOCKSDS/tools/mmutil/mmutil`, or the
//! `PATH` (override with `--mmutil <path>`).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wav2bank: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut ids: Option<PathBuf> = None;
    let mut mmutil: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" | "-i" => input = Some(args.next().ok_or("--input needs a path")?.into()),
            "--output" | "-o" => output = Some(args.next().ok_or("--output needs a path")?.into()),
            "--ids" => ids = Some(args.next().ok_or("--ids needs a path")?.into()),
            "--mmutil" => mmutil = Some(args.next().ok_or("--mmutil needs a path")?.into()),
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    let input = input.ok_or("missing --input <audio dir>")?;
    let output = output.ok_or("missing --output <soundbank.bin>")?;
    let ids = ids.ok_or("missing --ids <sounds.rs>")?;
    let mmutil = mmutil
        .or_else(wav2bank::find_mmutil)
        .ok_or("mmutil not found (set $BLOCKSDS, $MMUTIL or --mmutil; run inside `nix develop`)")?;

    let built = wav2bank::build_dir(&input, &output, &ids, &mmutil, &work_dir(&output))?;
    eprintln!(
        "wav2bank: {} sound(s) -> {} (+ {})",
        built.inputs.len(),
        built.soundbank.display(),
        built.ids_rs.display()
    );
    Ok(())
}

/// A scratch directory for loop-patched copies, kept out of the output tree.
fn work_dir(output: &std::path::Path) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("soundbank");
    dir.push(format!("wav2bank-{stem}"));
    dir
}

fn print_usage() {
    eprintln!(
        "Usage: wav2bank --input <audio dir> --output <soundbank.bin> --ids <sounds.rs> [--mmutil <path>]\n\n\
         Bakes audio/music/*.wav (looped) and audio/sfx/*.wav (one-shot) into a\n\
         maxmod soundbank for NitroFS, plus a Rust module of the sound IDs.\n\n\
         Options:\n  \
         -i, --input <dir>      Audio source dir (expects music/ and sfx/ subdirs)\n  \
         -o, --output <path>    Destination soundbank.bin (parent dirs created)\n      \
         --ids <path>           Destination generated Rust IDs module\n      \
         --mmutil <path>        Path to the mmutil binary (else auto-detected)\n  \
         -h, --help             Show this help"
    );
}
