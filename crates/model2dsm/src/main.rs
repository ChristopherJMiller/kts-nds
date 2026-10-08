//! `model2dsm <models-dir> <out-dir>` — bake every model under a directory (#66).

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: model2dsm <models-dir> <out-dir>");
        std::process::exit(2);
    }
    match model2dsm::build_dir(std::path::Path::new(&args[1]), std::path::Path::new(&args[2])) {
        Ok(built) => {
            for w in &built.warnings {
                eprintln!("warning: {w}");
            }
            println!("baked {} model(s), {} texture(s)", built.models.len(), built.textures.len());
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
