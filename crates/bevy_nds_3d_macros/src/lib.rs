//! Compile-time model loaders for [`bevy_nds_3d`](../bevy_nds_3d/index.html).
//!
//! The Nintendo DS has no runtime filesystem or asset server, so model data can
//! only ever be *bytes at a ROM address*. This crate bridges the gap: it parses
//! model files **on the host at build time** and emits a `&'static` mesh that the
//! game crate bakes straight into the ROM. The ergonomics stay Bevy-flavoured —
//! you reference a model by path — but there is no runtime loading.
//!
//! Currently it supports Wavefront OBJ via [`include_obj!`].

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use proc_macro::TokenStream;

/// Bake a Wavefront `.obj` model into the ROM as a `bevy_nds_3d::DsMesh`.
///
/// The path is resolved relative to the calling crate's `Cargo.toml`
/// (`CARGO_MANIFEST_DIR`). The OBJ is parsed at compile time: positions (`v`),
/// optional normals (`vn`) and faces (`f`) are read, faces with more than three
/// vertices are **fan-triangulated**, and any face lacking explicit normals gets
/// a computed flat normal. The result is a `&'static [[Vertex; 3]]` embedded in
/// the binary, wrapped in a hardware-**lit** [`DsMesh`].
///
/// ```ignore
/// use bevy_nds_3d::prelude::*;
/// commands.spawn((include_obj!("assets/teapot.obj"), Transform3d::default()));
/// ```
#[proc_macro]
pub fn include_obj(input: TokenStream) -> TokenStream {
    let path = match parse_string_literal(input) {
        Ok(p) => p,
        Err(e) => return compile_error(&e),
    };

    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".into());
    let full = PathBuf::from(&manifest_dir).join(&path);

    let source = match fs::read_to_string(&full) {
        Ok(s) => s,
        Err(e) => {
            return compile_error(&format!(
                "include_obj!: could not read {}: {e}",
                full.display()
            ));
        }
    };

    let tris = match parse_obj(&source) {
        Ok(t) => t,
        Err(e) => return compile_error(&format!("include_obj!({path:?}): {e}")),
    };
    if tris.is_empty() {
        return compile_error(&format!("include_obj!({path:?}): no triangles found"));
    }

    let code = emit(&tris, &full);
    TokenStream::from_str(&code).expect("include_obj! produced invalid tokens")
}

/// One triangle's worth of baked vertex data: position + normal per corner.
struct Tri {
    verts: [([f32; 3], [f32; 3]); 3],
}

/// Parse the subset of Wavefront OBJ we need: `v`, `vn`, `f`. Faces are
/// fan-triangulated; missing per-vertex normals are filled with the triangle's
/// flat (geometric) normal.
fn parse_obj(source: &str) -> Result<Vec<Tri>, String> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<Tri> = Vec::new();

    for (lineno, line) in source.lines().enumerate() {
        let line = line.trim();
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let v = parse_vec3(&mut it)
                    .ok_or_else(|| format!("line {}: malformed vertex", lineno + 1))?;
                positions.push(v);
            }
            Some("vn") => {
                let n = parse_vec3(&mut it)
                    .ok_or_else(|| format!("line {}: malformed normal", lineno + 1))?;
                normals.push(n);
            }
            Some("f") => {
                // Collect the face's (position, optional-normal) corner indices.
                let mut corners: Vec<([f32; 3], Option<[f32; 3]>)> = Vec::new();
                for tok in it {
                    let (vi, ni) = parse_face_vertex(tok)
                        .ok_or_else(|| format!("line {}: malformed face vertex {tok:?}", lineno + 1))?;
                    let pos = *resolve(&positions, vi)
                        .ok_or_else(|| format!("line {}: vertex index out of range", lineno + 1))?;
                    let nor = match ni {
                        Some(ni) => Some(
                            *resolve(&normals, ni).ok_or_else(|| {
                                format!("line {}: normal index out of range", lineno + 1)
                            })?,
                        ),
                        None => None,
                    };
                    corners.push((pos, nor));
                }
                if corners.len() < 3 {
                    return Err(format!("line {}: face has < 3 vertices", lineno + 1));
                }
                // Fan-triangulate: (0, i, i+1) for i in 1..n-1.
                for i in 1..corners.len() - 1 {
                    let a = corners[0];
                    let b = corners[i];
                    let c = corners[i + 1];
                    let flat = flat_normal(a.0, b.0, c.0);
                    tris.push(Tri {
                        verts: [
                            (a.0, a.1.unwrap_or(flat)),
                            (b.0, b.1.unwrap_or(flat)),
                            (c.0, c.1.unwrap_or(flat)),
                        ],
                    });
                }
            }
            _ => {} // comments, o/g/s/usemtl/mtllib, blanks, unsupported records
        }
    }

    Ok(tris)
}

/// Resolve a 1-based OBJ index (negative = relative to the end) into a slice.
fn resolve<T>(items: &[T], idx: i32) -> Option<&T> {
    if idx > 0 {
        items.get((idx - 1) as usize)
    } else if idx < 0 {
        let from_end = items.len() as i32 + idx;
        usize::try_from(from_end).ok().and_then(|i| items.get(i))
    } else {
        None
    }
}

fn parse_vec3<'a>(it: &mut impl Iterator<Item = &'a str>) -> Option<[f32; 3]> {
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    let z = it.next()?.parse().ok()?;
    Some([x, y, z])
}

/// Parse one face vertex token (`v`, `v/t`, `v//n`, or `v/t/n`) into a vertex
/// index and an optional normal index.
fn parse_face_vertex(tok: &str) -> Option<(i32, Option<i32>)> {
    let mut parts = tok.split('/');
    let v: i32 = parts.next()?.parse().ok()?;
    let _t = parts.next(); // texture coord index, ignored
    let n = match parts.next() {
        Some(s) if !s.is_empty() => Some(s.parse().ok()?),
        _ => None,
    };
    Some((v, n))
}

/// Geometric (flat) normal of a triangle, normalised; zero if degenerate.
fn flat_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    normalize(n)
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 0.0]
    }
}

/// Emit the `DsMesh` expression. A leading `include_bytes!` ties the build to the
/// source file so edits to the model trigger a recompile (proc-macros don't track
/// file reads on their own).
fn emit(tris: &[Tri], full: &std::path::Path) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    let _ = writeln!(
        out,
        "    const _: &[u8] = include_bytes!({:?});",
        full.display().to_string()
    );
    out.push_str("    const TRIS: &[[::bevy_nds_3d::Vertex; 3]] = &[\n");
    for tri in tris {
        out.push_str("        [");
        for (pos, nor) in &tri.verts {
            // Pre-normalise so the runtime never has to.
            let n = normalize(*nor);
            let _ = write!(
                out,
                "::bevy_nds_3d::Vertex::from_raw([{}f32,{}f32,{}f32],[{}f32,{}f32,{}f32],[200u8,200u8,210u8]),",
                fl(pos[0]), fl(pos[1]), fl(pos[2]),
                fl(n[0]), fl(n[1]), fl(n[2]),
            );
        }
        out.push_str("],\n");
    }
    out.push_str("    ];\n");
    out.push_str("    ::bevy_nds_3d::DsMesh::from_static(TRIS, true)\n");
    out.push_str("}\n");
    out
}

/// Format an `f32` so it always round-trips as a float literal (`0` -> `0.0`).
fn fl(v: f32) -> String {
    let mut s = format!("{v:?}");
    if !s.contains('.') && !s.contains('e') && !s.contains("inf") && !s.contains("NaN") {
        s.push_str(".0");
    }
    s
}

/// Pull a single string literal out of the macro input.
fn parse_string_literal(input: TokenStream) -> Result<String, String> {
    let s = input.to_string();
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        Ok(s[1..s.len() - 1].to_string())
    } else {
        Err("include_obj! expects a single string-literal path, e.g. include_obj!(\"assets/model.obj\")".into())
    }
}

/// Produce a `compile_error!` token stream with the given message.
fn compile_error(msg: &str) -> TokenStream {
    TokenStream::from_str(&format!("compile_error!({msg:?})")).expect("valid compile_error tokens")
}
