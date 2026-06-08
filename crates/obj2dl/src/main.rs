//! `obj2dl` CLI — a thin wrapper over the [`obj2dl`](../obj2dl/index.html)
//! library for one-off conversions:
//!
//! ```text
//! obj2dl --input assets/teapot.obj --output build/nitrofs/teapot.dl [--center] [--offset x y z]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use obj2dl::Options;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("obj2dl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut opts = Options::default();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" | "-i" => input = Some(args.next().ok_or("--input needs a path")?.into()),
            "--output" | "-o" => output = Some(args.next().ok_or("--output needs a path")?.into()),
            "--center" | "-c" => opts.center = true,
            "--offset" => {
                opts.offset = [
                    parse_f32(args.next(), "offset x")?,
                    parse_f32(args.next(), "offset y")?,
                    parse_f32(args.next(), "offset z")?,
                ];
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    let input = input.ok_or("missing --input <file.obj>")?;
    let output = output.ok_or("missing --output <file.dl>")?;

    let words = obj2dl::convert_file(&input, &output, &opts)?;
    eprintln!(
        "obj2dl: {} -> {} ({words} words)",
        input.display(),
        output.display()
    );
    Ok(())
}

fn parse_f32(arg: Option<String>, what: &str) -> Result<f32, String> {
    arg.ok_or_else(|| format!("--offset needs three numbers ({what} missing)"))?
        .parse()
        .map_err(|_| format!("{what} is not a number"))
}

fn print_usage() {
    eprintln!(
        "Usage: obj2dl --input <file.obj> --output <file.dl> [--center] [--offset x y z]\n\n\
         Bakes a Wavefront OBJ into a Bevy-DS display-list asset for NitroFS.\n\n\
         Options:\n  \
         -i, --input <path>    Source .obj file\n  \
         -o, --output <path>   Destination .dl file (parent dirs are created)\n  \
         -c, --center          Recentre geometry on its bounding-box midpoint\n      \
         --offset x y z        Translate every vertex (after --center)\n  \
         -h, --help            Show this help"
    );
}
