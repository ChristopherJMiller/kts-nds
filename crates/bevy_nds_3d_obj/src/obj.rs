//! Wavefront OBJ (+ MTL) front-end → [`SourceModel`] (#66).

use crate::flat_normal;
use crate::ir::{Material, SourceModel, SubMesh, TextureSrc, Vertex};

/// How much of an OBJ to honour.
#[derive(Clone, Copy, Debug, Default)]
pub struct ObjOptions {
    /// Honour `mtllib` / `usemtl` / `vt`: faces group into one sub-mesh per
    /// material (first-use order) and carry UVs. When `false` — the legacy `.dl`
    /// / `include_obj!` path — every face lands in one default sub-mesh **in
    /// file order**, texture coordinates are ignored and no MTL is read, so the
    /// legacy output stays byte-identical.
    pub materials: bool,
}

/// Parse a Wavefront OBJ into a [`SourceModel`]. `load_mtl` reads an `mtllib`
/// by the name the OBJ gives it (relative to the OBJ's directory); it is never
/// called unless `opts.materials` is set. Faces are fan-triangulated; a corner
/// without a normal takes its triangle's flat normal.
pub fn parse_obj(
    source: &str,
    opts: ObjOptions,
    mut load_mtl: impl FnMut(&str) -> Result<String, String>,
) -> Result<SourceModel, String> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut library: Vec<Material> = Vec::new();
    let mut subs: Vec<SubMesh> = Vec::new();
    let mut current: Option<usize> = None;

    for (lineno, line) in source.lines().enumerate() {
        let at = |msg: &str| format!("line {}: {msg}", lineno + 1);
        let trimmed = line.trim();
        let mut it = trimmed.split_whitespace();
        match it.next() {
            Some("v") => positions.push(parse_vec3(&mut it).ok_or_else(|| at("malformed vertex"))?),
            Some("vn") => normals.push(parse_vec3(&mut it).ok_or_else(|| at("malformed normal"))?),
            Some("vt") if opts.materials => {
                let u: f32 = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| at("malformed texture coordinate"))?;
                let v: f32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                uvs.push([u, 1.0 - v]); // OBJ is bottom-left; the IR is top-left
            }
            Some("mtllib") if opts.materials => {
                // The rest of the line is ONE file name, trimmed (Blender
                // writes a single mtllib, and names may contain spaces) —
                // not one name per whitespace-split token.
                let name = trimmed.strip_prefix("mtllib").unwrap().trim();
                if !name.is_empty() {
                    let text = load_mtl(name).map_err(|e| at(&e))?;
                    library.extend(parse_mtl(&text).map_err(|e| at(&format!("{name}: {e}")))?);
                }
            }
            Some("usemtl") if opts.materials => {
                // Rest of the line, trimmed: a material name may contain spaces.
                let name = trimmed.strip_prefix("usemtl").unwrap().trim();
                current = Some(submesh_index(&mut subs, name, &library).map_err(|e| at(&e))?);
            }
            Some("f") => {
                let mut corners: Vec<([f32; 3], Option<[f32; 3]>, Option<[f32; 2]>)> = Vec::new();
                for tok in it {
                    let (vi, ti, ni) = parse_face_vertex(tok)
                        .ok_or_else(|| at(&format!("malformed face vertex {tok:?}")))?;
                    let pos =
                        *resolve(&positions, vi).ok_or_else(|| at("vertex index out of range"))?;
                    let nor = match ni {
                        Some(ni) => Some(
                            *resolve(&normals, ni)
                                .ok_or_else(|| at("normal index out of range"))?,
                        ),
                        None => None,
                    };
                    let uv = match ti {
                        Some(ti) if opts.materials => Some(
                            *resolve(&uvs, ti)
                                .ok_or_else(|| at("texture coordinate index out of range"))?,
                        ),
                        _ => None,
                    };
                    corners.push((pos, nor, uv));
                }
                if corners.len() < 3 {
                    return Err(at("face has < 3 vertices"));
                }
                let si = match current {
                    Some(i) => i,
                    None => {
                        let i = submesh_index(&mut subs, "", &library).map_err(|e| at(&e))?;
                        current = Some(i);
                        i
                    }
                };
                // Fan-triangulate: (0, i, i+1) for i in 1..n-1.
                for i in 1..corners.len() - 1 {
                    let (a, b, c) = (corners[0], corners[i], corners[i + 1]);
                    let flat = flat_normal(a.0, b.0, c.0);
                    let vert = |k: ([f32; 3], Option<[f32; 3]>, Option<[f32; 2]>)| Vertex {
                        pos: k.0,
                        normal: k.1.unwrap_or(flat),
                        uv: k.2,
                    };
                    subs[si].tris.push([vert(a), vert(b), vert(c)]);
                }
            }
            _ => {} // comments, o/g/s, blanks, and (legacy mode) vt/usemtl/mtllib
        }
    }

    Ok(SourceModel {
        submeshes: subs,
        images: Vec::new(),
    })
}

/// The sub-mesh for material `name`, created on first use. `""` is the default
/// material (faces before any `usemtl`); a named one must come from an MTL.
fn submesh_index(
    subs: &mut Vec<SubMesh>,
    name: &str,
    library: &[Material],
) -> Result<usize, String> {
    if let Some(i) = subs.iter().position(|s| s.material.name == name) {
        return Ok(i);
    }
    let material = if name.is_empty() {
        Material::default()
    } else {
        library
            .iter()
            .find(|m| m.name == name)
            .cloned()
            .ok_or_else(|| format!("`usemtl {name}` but no loaded mtllib defines it"))?
    };
    subs.push(SubMesh {
        material,
        tris: Vec::new(),
    });
    Ok(subs.len() - 1)
}

/// Parse the MTL subset the pipeline uses: `newmtl`, `Kd`, `map_Kd`.
/// `newmtl`'s name is the rest of the line, trimmed, so it may contain
/// spaces. `map_Kd`'s path is likewise the rest of the line, trimmed —
/// *unless* the line also carries option tokens (any token starting with
/// `-`, e.g. `-s 1 1 1`), in which case the **last** token is the path
/// instead (matching how options precede the path), so a path combined with
/// options still can't contain spaces. `Ka` is deliberately ignored — see
/// [`Material`].
pub fn parse_mtl(source: &str) -> Result<Vec<Material>, String> {
    let mut out: Vec<Material> = Vec::new();
    for (lineno, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let mut it = trimmed.split_whitespace();
        let key = it.next();
        if key == Some("newmtl") {
            out.push(Material {
                name: trimmed.strip_prefix("newmtl").unwrap().trim().to_string(),
                ..Material::default()
            });
            continue;
        }
        let Some(m) = out.last_mut() else { continue };
        match key {
            Some("Kd") => {
                m.diffuse = parse_vec3(&mut it)
                    .map(|c| c.map(unit_to_u8))
                    .ok_or_else(|| format!("line {}: malformed Kd", lineno + 1))?;
            }
            Some("map_Kd") => {
                let rest = trimmed.strip_prefix("map_Kd").unwrap().trim();
                let has_options = rest.split_whitespace().any(|t| t.starts_with('-'));
                let path = if has_options {
                    it.last()
                } else if rest.is_empty() {
                    None
                } else {
                    Some(rest)
                }
                .ok_or_else(|| format!("line {}: map_Kd has no path", lineno + 1))?;
                m.texture = Some(TextureSrc::File(path.to_string()));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// A `0.0..=1.0` colour channel → `0..=255` (rounded, clamped).
pub fn unit_to_u8(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
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

/// One face vertex token (`v`, `v/t`, `v//n`, `v/t/n`) → (vertex, texcoord,
/// normal) indices. A malformed texcoord reads as absent, exactly as the legacy
/// parser (which ignored the field) behaved.
fn parse_face_vertex(tok: &str) -> Option<(i32, Option<i32>, Option<i32>)> {
    let mut parts = tok.split('/');
    let v: i32 = parts.next()?.parse().ok()?;
    let t = parts.next().and_then(|s| s.parse().ok());
    let n = match parts.next() {
        Some(s) if !s.is_empty() => Some(s.parse().ok()?),
        _ => None,
    };
    Some((v, t, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Material, TextureSrc};

    const TWO_MATERIALS: &str = "mtllib m.mtl\n\
        v 0 0 0\nv 1 0 0\nv 0 1 0\n\
        vt 0.25 0.75\n\
        usemtl a\nf 1/1 2/1 3/1\n\
        usemtl b\nf 1/1 2/1 3/1\n\
        usemtl a\nf 1/1 2/1 3/1\n";
    const MTL: &str = "newmtl a\nKd 1 0.5 0\nmap_Kd -s 1 1 1 tex/a.png\nnewmtl b\nKd 0 0 1\n";

    fn with_mtl(name: &str) -> Result<String, String> {
        assert_eq!(name, "m.mtl");
        Ok(MTL.to_string())
    }

    #[test]
    fn faces_group_by_material_in_first_use_order() {
        let m = parse_obj(TWO_MATERIALS, ObjOptions { materials: true }, with_mtl).unwrap();
        let names: Vec<&str> = m
            .submeshes
            .iter()
            .map(|s| s.material.name.as_str())
            .collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(m.submeshes[0].tris.len(), 2);
        assert_eq!(m.submeshes[1].tris.len(), 1);
        assert_eq!(m.submeshes[1].material.diffuse, [0, 0, 255]);
    }

    #[test]
    fn vt_is_flipped_to_top_left_origin() {
        let m = parse_obj(TWO_MATERIALS, ObjOptions { materials: true }, with_mtl).unwrap();
        assert_eq!(m.submeshes[0].tris[0][0].uv, Some([0.25, 0.25]));
    }

    #[test]
    fn mtl_reads_kd_and_map_kd_last_token() {
        let mats = parse_mtl(MTL).unwrap();
        assert_eq!(
            mats[0],
            Material {
                name: "a".into(),
                diffuse: [255, 128, 0],
                texture: Some(TextureSrc::File("tex/a.png".into())),
                ..Material::default()
            }
        );
        assert_eq!(mats[1].texture, None);
    }

    #[test]
    fn mtllib_name_may_contain_spaces() {
        let src = "mtllib Wooden Crate.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let mut seen = None;
        parse_obj(src, ObjOptions { materials: true }, |name| {
            seen = Some(name.to_string());
            Ok(String::new())
        })
        .unwrap();
        assert_eq!(seen, Some("Wooden Crate.mtl".to_string()));
    }

    #[test]
    fn map_kd_without_options_takes_rest_of_line() {
        let mtl = "newmtl skin\nmap_Kd tex/Wooden Crate.png\n";
        let mats = parse_mtl(mtl).unwrap();
        assert_eq!(
            mats[0].texture,
            Some(TextureSrc::File("tex/Wooden Crate.png".into()))
        );
    }

    #[test]
    fn newmtl_and_usemtl_with_spaced_names_group_correctly() {
        let src = "mtllib m.mtl\n\
            v 0 0 0\nv 1 0 0\nv 0 1 0\n\
            usemtl Wood Crate\nf 1 2 3\n\
            usemtl Wood Crate\nf 1 2 3\n";
        let mtl = "newmtl Wood Crate\nKd 1 0.5 0\n";
        let m = parse_obj(src, ObjOptions { materials: true }, |_| Ok(mtl.to_string())).unwrap();
        assert_eq!(m.submeshes.len(), 1);
        assert_eq!(m.submeshes[0].material.name, "Wood Crate");
        assert_eq!(m.submeshes[0].tris.len(), 2);
    }

    #[test]
    fn undefined_material_is_an_error() {
        let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl ghost\nf 1 2 3\n";
        let err =
            parse_obj(src, ObjOptions { materials: true }, |_| Ok(String::new())).unwrap_err();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn faces_before_usemtl_use_the_default_material() {
        let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";
        let m = parse_obj(src, ObjOptions { materials: true }, |_| Ok(String::new())).unwrap();
        assert_eq!(m.submeshes.len(), 1);
        assert_eq!(m.submeshes[0].material, Material::default());
    }

    /// The legacy path (`.dl` / `include_obj!`) ignores mtllib / usemtl / vt:
    /// one sub-mesh in file order, no UVs, and no MTL is ever read.
    #[test]
    fn legacy_mode_ignores_materials_and_uvs() {
        let m = parse_obj(TWO_MATERIALS, ObjOptions::default(), |_| {
            panic!("legacy mode must not read an MTL")
        })
        .unwrap();
        assert_eq!(m.submeshes.len(), 1);
        assert_eq!(m.submeshes[0].tris.len(), 3);
        assert!(m.submeshes[0].tris.iter().flatten().all(|v| v.uv.is_none()));
    }
}
