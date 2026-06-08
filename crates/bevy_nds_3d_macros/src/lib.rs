//! Compile-time model loaders for [`bevy_nds_3d`](../bevy_nds_3d/index.html).
//!
//! The Nintendo DS has no runtime filesystem or asset server, so model data can
//! only ever be *bytes at a ROM address*. This crate bridges the gap: it parses
//! model files **on the host at build time** (via [`bevy_nds_3d_obj`]) and emits
//! a `&'static` display list that the game crate bakes straight into the ROM.
//! The ergonomics stay Bevy-flavoured — you reference a model by path — but there
//! is no runtime loading. (For runtime loading from the ROM filesystem instead,
//! see NitroFS + `bevy_nds_3d::load_model`.)
//!
//! Currently it supports Wavefront OBJ via [`include_obj!`].

use std::env;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use bevy_nds_3d_obj::Options;
use proc_macro::{TokenStream, TokenTree};

/// Bake a Wavefront `.obj` model into the ROM as a `bevy_nds_3d::DsMesh`.
///
/// The path is resolved relative to the calling crate's `Cargo.toml`
/// (`CARGO_MANIFEST_DIR`). The OBJ is parsed at compile time: positions (`v`),
/// optional normals (`vn`) and faces (`f`) are read, faces with more than three
/// vertices are **fan-triangulated**, and any face lacking explicit normals gets
/// a computed flat normal. The result is a libnds *display list* embedded in the
/// binary, wrapped in a hardware-**lit** [`DsMesh`] drawn with `glCallList`.
///
/// # Origin / offset settings
///
/// Models are often authored around an off-centre origin (the Utah teapot sits
/// on the XY plane, so its pivot is at the *base*, not the middle), which makes
/// rotation look like it is tumbling around the wrong point. Two optional,
/// comma-separated settings adjust the model-space origin at build time (so they
/// cost nothing at runtime):
///
/// - `center` — recentre the geometry on the midpoint of its bounding box, so
///   the entity's [`Transform3d`] rotates it about its visual centre.
/// - `offset = [x, y, z]` — translate every vertex by this amount (applied
///   *after* `center` if both are given).
/// - `compress` — emit compressed `VTX_10` vertices (smaller list, coarser
///   precision; see `bevy_nds_3d_obj::Options::compress`).
///
/// ```ignore
/// use bevy_nds_3d::prelude::*;
/// // As authored:
/// commands.spawn((include_obj!("assets/teapot.obj"), Transform3d::default()));
/// // Recentred so it spins about its middle:
/// commands.spawn((include_obj!("assets/teapot.obj", center), Transform3d::default()));
/// // Recentred, then nudged down a touch:
/// commands.spawn(include_obj!("assets/teapot.obj", center, offset = [0.0, -0.2, 0.0]));
/// ```
#[proc_macro]
pub fn include_obj(input: TokenStream) -> TokenStream {
    let args = match parse_args(input) {
        Ok(a) => a,
        Err(e) => return compile_error(&e),
    };
    let path = &args.path;

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let full = PathBuf::from(&manifest_dir).join(path);

    let source = match fs::read_to_string(&full) {
        Ok(s) => s,
        Err(e) => {
            return compile_error(&format!(
                "include_obj!: could not read {}: {e}",
                full.display()
            ));
        }
    };

    let model = match bevy_nds_3d_obj::obj_to_display_list(&source, &args.opts) {
        Ok(m) => m,
        Err(e) => return compile_error(&format!("include_obj!({path:?}): {e}")),
    };

    let code = emit(&model, &full);
    TokenStream::from_str(&code).expect("include_obj! produced invalid tokens")
}

/// Parsed `include_obj!` arguments: the model path plus the origin settings.
struct Args {
    path: String,
    opts: Options,
}

/// Emit the `DsMesh` expression from a baked model. A leading `include_bytes!`
/// ties the build to the source file so edits to the model trigger a recompile
/// (proc-macros don't track file reads on their own).
fn emit(model: &bevy_nds_3d_obj::Model, full: &std::path::Path) -> String {
    let [min, max] = model.aabb;
    format!(
        "{{\n    const _: &[u8] = include_bytes!({path:?});\n    \
         const WORDS: &[u32] = &[{words}\n    ];\n    \
         ::bevy_nds_3d::DsMesh::from_baked(WORDS, \
         [{}f32,{}f32,{}f32], [{}f32,{}f32,{}f32])\n}}\n",
        fl(min[0]),
        fl(min[1]),
        fl(min[2]),
        fl(max[0]),
        fl(max[1]),
        fl(max[2]),
        path = full.display().to_string(),
        words = bevy_nds_3d_obj::format_words_rust(&model.words),
    )
}

/// Format an `f32` so it always round-trips as a float literal (`0` -> `0.0`).
fn fl(v: f32) -> String {
    let mut s = format!("{v:?}");
    if !s.contains('.') && !s.contains('e') && !s.contains("inf") && !s.contains("NaN") {
        s.push_str(".0");
    }
    s
}

/// Parse the macro input: a string-literal path, optionally followed by
/// comma-separated `center` and/or `offset = [x, y, z]` settings.
fn parse_args(input: TokenStream) -> Result<Args, String> {
    let mut trees = input.into_iter().peekable();

    let path = match trees.next() {
        Some(TokenTree::Literal(lit)) => unquote(&lit.to_string()).ok_or_else(|| {
            "include_obj!: first argument must be a string-literal path".to_string()
        })?,
        _ => {
            return Err(
                "include_obj! expects a path, e.g. include_obj!(\"assets/model.obj\")".into(),
            );
        }
    };

    let mut args = Args {
        path,
        opts: Options::default(),
    };

    while let Some(tt) = trees.next() {
        match tt {
            TokenTree::Punct(p) if p.as_char() == ',' => continue,
            TokenTree::Ident(id) if id.to_string() == "center" => {
                args.opts.center = true;
            }
            TokenTree::Ident(id) if id.to_string() == "compress" => {
                args.opts.compress = true;
            }
            TokenTree::Ident(id) if id.to_string() == "offset" => {
                match trees.next() {
                    Some(TokenTree::Punct(p)) if p.as_char() == '=' => {}
                    other => {
                        return Err(format!(
                            "include_obj!: expected `=` after `offset`, found {}",
                            describe(other.as_ref())
                        ));
                    }
                }
                match trees.next() {
                    Some(TokenTree::Group(g)) => {
                        args.opts.offset = parse_f32_triple(&g.stream())?;
                    }
                    other => {
                        return Err(format!(
                            "include_obj!: expected `[x, y, z]` after `offset =`, found {}",
                            describe(other.as_ref())
                        ));
                    }
                }
            }
            other => return Err(format!("include_obj!: unknown setting `{other}`")),
        }
    }

    Ok(args)
}

/// Parse three comma-separated float literals (the body of an `[x, y, z]`).
fn parse_f32_triple(stream: &TokenStream) -> Result<[f32; 3], String> {
    let mut out = [0.0f32; 3];
    let mut i = 0;
    let mut pending_neg = false;
    for tt in stream.clone() {
        match tt {
            TokenTree::Punct(p) if p.as_char() == ',' => {
                pending_neg = false;
            }
            TokenTree::Punct(p) if p.as_char() == '-' => {
                pending_neg = true;
            }
            TokenTree::Literal(lit) => {
                if i >= 3 {
                    return Err("include_obj!: offset takes exactly 3 numbers".into());
                }
                let v: f32 = lit
                    .to_string()
                    .parse()
                    .map_err(|_| format!("include_obj!: `{lit}` is not a number"))?;
                out[i] = if pending_neg { -v } else { v };
                pending_neg = false;
                i += 1;
            }
            other => {
                return Err(format!(
                    "include_obj!: unexpected token `{other}` in offset"
                ));
            }
        }
    }
    if i != 3 {
        return Err("include_obj!: offset takes exactly 3 numbers, e.g. [0.0, -0.2, 0.0]".into());
    }
    Ok(out)
}

/// Strip surrounding double quotes from a string-literal token's text.
fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

/// A short human description of an optional token, for error messages.
fn describe(tt: Option<&TokenTree>) -> String {
    match tt {
        Some(tt) => format!("`{tt}`"),
        None => "end of input".to_string(),
    }
}

/// Produce a `compile_error!` token stream with the given message.
fn compile_error(msg: &str) -> TokenStream {
    TokenStream::from_str(&format!("compile_error!({msg:?})")).expect("valid compile_error tokens")
}
