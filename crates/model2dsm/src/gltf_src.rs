//! glTF 2.0 (`.gltf` + `.bin`, or `.glb`) front-end → [`SourceModel`] (#66).
//!
//! Static meshes only: the default scene's node tree is flattened with each
//! node's world transform baked into the vertices (so a Blender object's
//! location / rotation / scale survive export). Skins and animation are out of
//! scope (#66 Open questions).

use std::path::Path;

use bevy_nds_3d_obj::flat_normal;
use bevy_nds_3d_obj::ir::{Material, SourceModel, SubMesh, TextureSrc, Vertex, Wrap};
use bevy_nds_3d_obj::obj::unit_to_u8;
use gltf::texture::WrappingMode;

const DATA_URI_ERR: &str = "embedded base64 (`data:`) URIs aren't supported — export as glTF Binary (.glb) or glTF Separate (.gltf + .bin + textures)";

/// Column-major 4×4 (`m[column][row]`), glTF's layout.
type Mat4 = [[f32; 4]; 4];
const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Read a `.gltf` / `.glb` file; external buffers resolve beside it.
pub fn load_gltf(path: &Path) -> Result<SourceModel, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("."));
    parse_gltf(&bytes, &|uri| {
        let p = base.join(uri);
        std::fs::read(&p).map_err(|e| format!("could not read {}: {e}", p.display()))
    })
}

/// Parse glTF bytes. `read_uri` loads an external buffer by its relative URI.
pub fn parse_gltf(
    bytes: &[u8],
    read_uri: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<SourceModel, String> {
    let gltf::Gltf { document, blob } =
        gltf::Gltf::from_slice(bytes).map_err(|e| format!("not a valid glTF: {e}"))?;

    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for b in document.buffers() {
        buffers.push(match b.source() {
            gltf::buffer::Source::Bin => blob
                .clone()
                .ok_or("a buffer refers to the GLB binary chunk, but there is none")?,
            gltf::buffer::Source::Uri(uri) if uri.starts_with("data:") => {
                return Err(DATA_URI_ERR.into());
            }
            gltf::buffer::Source::Uri(uri) => read_uri(uri)?,
        });
    }

    let mut images: Vec<Vec<u8>> = Vec::new();
    let mut image_src: Vec<TextureSrc> = Vec::new();
    for img in document.images() {
        image_src.push(match img.source() {
            gltf::image::Source::View { view, mime_type } => {
                if mime_type != "image/png" {
                    return Err(format!(
                        "image {} is {mime_type}; only PNG textures are supported",
                        img.index()
                    ));
                }
                let data = buffers
                    .get(view.buffer().index())
                    .ok_or("image buffer missing")?;
                let bytes = data
                    .get(view.offset()..view.offset() + view.length())
                    .ok_or("image buffer view out of range")?;
                images.push(bytes.to_vec());
                TextureSrc::Embedded(images.len() - 1)
            }
            gltf::image::Source::Uri { uri, .. } if uri.starts_with("data:") => {
                return Err(DATA_URI_ERR.into());
            }
            gltf::image::Source::Uri { uri, .. } => TextureSrc::File(uri.to_string()),
        });
    }

    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or("glTF has no scene")?;
    let mut groups: Vec<(Option<usize>, SubMesh)> = Vec::new();
    for node in scene.nodes() {
        visit(&node, &IDENTITY, &buffers, &image_src, &mut groups)?;
    }
    Ok(SourceModel {
        submeshes: groups.into_iter().map(|(_, s)| s).collect(),
        images,
    })
}

fn visit(
    node: &gltf::Node,
    parent: &Mat4,
    buffers: &[Vec<u8>],
    image_src: &[TextureSrc],
    groups: &mut Vec<(Option<usize>, SubMesh)>,
) -> Result<(), String> {
    let world = mul(parent, &node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        let (normal_m, det) = normal_matrix(&world);
        if det.abs() < 1e-12 {
            return Err(format!(
                "node {} has a degenerate (zero-scale) transform",
                node.index()
            ));
        }
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                return Err(format!(
                    "mesh {} uses {:?}; only triangle lists are supported",
                    mesh.index(),
                    prim.mode()
                ));
            }
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| d.as_slice()));
            let pos: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| format!("mesh {} has a primitive with no POSITION", mesh.index()))?
                .collect();
            let nor: Option<Vec<[f32; 3]>> = reader.read_normals().map(|i| i.collect());
            let uv: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|i| i.into_f32().collect());
            let idx: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..pos.len() as u32).collect(),
            };
            if idx.len() % 3 != 0 {
                return Err(format!(
                    "mesh {}: index count {} isn't a multiple of 3",
                    mesh.index(),
                    idx.len()
                ));
            }

            let key = prim.material().index();
            let at = match groups.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    let material = material(&prim.material(), image_src)?;
                    groups.push((
                        key,
                        SubMesh {
                            material,
                            tris: Vec::new(),
                        },
                    ));
                    groups.len() - 1
                }
            };

            let corner = |i: u32| -> Result<Vertex, String> {
                let i = i as usize;
                let p = *pos
                    .get(i)
                    .ok_or_else(|| format!("mesh {}: index {i} out of range", mesh.index()))?;
                Ok(Vertex {
                    pos: transform_point(&world, p),
                    normal: nor
                        .as_ref()
                        .and_then(|n| n.get(i))
                        .map(|n| apply3(&normal_m, *n))
                        .unwrap_or([0.0; 3]),
                    uv: uv.as_ref().and_then(|u| u.get(i)).copied(),
                })
            };
            for t in idx.chunks_exact(3) {
                let mut tri = [corner(t[0])?, corner(t[1])?, corner(t[2])?];
                if det < 0.0 {
                    // A mirroring transform reverses the winding; restore
                    // counter-clockwise-from-outside so DS back-face culling holds.
                    tri.swap(1, 2);
                }
                if nor.is_none() {
                    let f = flat_normal(tri[0].pos, tri[1].pos, tri[2].pos);
                    for v in &mut tri {
                        v.normal = f;
                    }
                }
                groups[at].1.tris.push(tri);
            }
        }
    }
    for child in node.children() {
        visit(&child, &world, buffers, image_src, groups)?;
    }
    Ok(())
}

fn material(m: &gltf::Material, image_src: &[TextureSrc]) -> Result<Material, String> {
    let pbr = m.pbr_metallic_roughness();
    let f = pbr.base_color_factor();
    let mut out = Material {
        name: m.name().unwrap_or("").to_string(),
        diffuse: [unit_to_u8(f[0]), unit_to_u8(f[1]), unit_to_u8(f[2])],
        texture: None,
        wrap: Wrap::default(),
    };
    if let Some(info) = pbr.base_color_texture() {
        if info.tex_coord() != 0 {
            return Err(format!(
                "material `{}` samples TEXCOORD_{}; only TEXCOORD_0 is supported",
                out.name,
                info.tex_coord()
            ));
        }
        let tex = info.texture();
        out.texture = Some(
            image_src
                .get(tex.source().index())
                .cloned()
                .ok_or_else(|| format!("material `{}`: texture image missing", out.name))?,
        );
        let s = tex.sampler();
        let (repeat_s, flip_s) = wrap_bits(s.wrap_s());
        let (repeat_t, flip_t) = wrap_bits(s.wrap_t());
        out.wrap = Wrap {
            repeat_s,
            repeat_t,
            flip_s,
            flip_t,
        };
    }
    Ok(out)
}

/// glTF wrap mode → DS (repeat, flip).
fn wrap_bits(m: WrappingMode) -> (bool, bool) {
    match m {
        WrappingMode::Repeat => (true, false),
        WrappingMode::MirroredRepeat => (true, true),
        WrappingMode::ClampToEdge => (false, false),
    }
}

fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [[0.0f32; 4]; 4];
    for c in 0..4 {
        for row in 0..4 {
            r[c][row] = (0..4).map(|k| a[k][row] * b[c][k]).sum();
        }
    }
    r
}

fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
}

/// The normal matrix — inverse-transpose of the upper 3×3 (`cofactor / det`,
/// row-major) — plus the determinant, whose sign says whether the transform
/// mirrors.
fn normal_matrix(m: &Mat4) -> ([[f32; 3]; 3], f32) {
    let a = |r: usize, c: usize| m[c][r];
    let c = [
        [
            a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1),
            -(a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)),
            a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0),
        ],
        [
            -(a(0, 1) * a(2, 2) - a(0, 2) * a(2, 1)),
            a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0),
            -(a(0, 0) * a(2, 1) - a(0, 1) * a(2, 0)),
        ],
        [
            a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1),
            -(a(0, 0) * a(1, 2) - a(0, 2) * a(1, 0)),
            a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0),
        ],
    ];
    let det = a(0, 0) * c[0][0] + a(0, 1) * c[0][1] + a(0, 2) * c[0][2];
    let inv = if det != 0.0 { 1.0 / det } else { 0.0 };
    (c.map(|row| row.map(|x| x * inv)), det)
}

fn apply3(n: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| n[r][0] * v[0] + n[r][1] * v[1] + n[r][2] * v[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_nds_3d_obj::ir::{TextureSrc, Wrap};

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// A GLB holding one triangle (0,0,0) (1,0,0) (0,1,0) under one node whose
    /// transform JSON is `node`, with an embedded PNG texture.
    fn one_triangle_glb(node: &str, with_normals: bool, png: &[u8]) -> Vec<u8> {
        let mut bin = Vec::new();
        bin.extend(f32s(&[0., 0., 0., 1., 0., 0., 0., 1., 0.])); // 0..36  POSITION
        bin.extend(f32s(&[0., 0., 1., 0., 0., 1., 0., 0., 1.])); // 36..72 NORMAL
        bin.extend(f32s(&[0., 0., 1., 0., 0., 1.])); //             72..96 TEXCOORD_0
        bin.extend([0u8, 0, 1, 0, 2, 0, 0, 0]); //                 96..104 indices + pad
        bin.extend_from_slice(png); //                             104..   image
        let attrs = if with_normals {
            r#""POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2"#
        } else {
            r#""POSITION": 0, "TEXCOORD_0": 2"#
        };
        let json = r#"{
          "asset": {"version": "2.0"},
          "scene": 0,
          "scenes": [{"nodes": [0]}],
          "nodes": [{"mesh": 0, NODE}],
          "meshes": [{"primitives": [{"attributes": {ATTRS}, "indices": 3, "material": 0}]}],
          "materials": [{"name": "crate", "pbrMetallicRoughness": {
              "baseColorFactor": [1.0, 0.5, 0.0, 1.0], "baseColorTexture": {"index": 0}}}],
          "textures": [{"source": 0, "sampler": 0}],
          "samplers": [{"wrapS": 33648, "wrapT": 33071}],
          "images": [{"bufferView": 4, "mimeType": "image/png"}],
          "buffers": [{"byteLength": BUFLEN}],
          "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24},
            {"buffer": 0, "byteOffset": 96, "byteLength": 6},
            {"buffer": 0, "byteOffset": 104, "byteLength": PNGLEN}
          ],
          "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"}
          ]
        }"#
        .replace("NODE", node)
        .replace("ATTRS", attrs)
        .replace("BUFLEN", &bin.len().to_string())
        .replace("PNGLEN", &png.len().to_string());
        glb(&json, &bin)
    }

    fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
        let mut j = json.as_bytes().to_vec();
        while j.len() % 4 != 0 {
            j.push(b' ');
        }
        let mut b = bin.to_vec();
        while b.len() % 4 != 0 {
            b.push(0);
        }
        let total = 12 + 8 + j.len() + 8 + b.len();
        let mut out = Vec::new();
        out.extend_from_slice(b"glTF");
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&(j.len() as u32).to_le_bytes());
        out.extend_from_slice(b"JSON");
        out.extend_from_slice(&j);
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&b);
        out
    }

    fn no_files(uri: &str) -> Result<Vec<u8>, String> {
        Err(format!("unexpected external file {uri}"))
    }

    const FAKE_PNG: &[u8] = b"\x89PNG-not-decoded-here";

    #[test]
    fn reads_geometry_material_and_embedded_texture() {
        let m = parse_gltf(
            &one_triangle_glb(r#""translation": [1.0, 0.0, 0.0]"#, true, FAKE_PNG),
            &no_files,
        )
        .unwrap();
        assert_eq!(m.submeshes.len(), 1);
        let s = &m.submeshes[0];
        assert_eq!(s.material.name, "crate");
        assert_eq!(s.material.diffuse, [255, 128, 0]);
        assert_eq!(s.material.texture, Some(TextureSrc::Embedded(0)));
        assert_eq!(
            s.material.wrap,
            Wrap {
                repeat_s: true,
                repeat_t: false,
                flip_s: true,
                flip_t: false
            }
        );
        assert_eq!(m.images, vec![FAKE_PNG.to_vec()]);
        let t = s.tris[0];
        assert_eq!(t[0].pos, [1.0, 0.0, 0.0]);
        assert_eq!(t[1].pos, [2.0, 0.0, 0.0]);
        assert_eq!(t[2].pos, [1.0, 1.0, 0.0]);
        assert_eq!(t[1].uv, Some([1.0, 0.0])); // glTF is already top-left: no flip
        assert_eq!(t[0].normal, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn mirrored_node_keeps_outward_winding() {
        let m = parse_gltf(
            &one_triangle_glb(r#""scale": [-1.0, 1.0, 1.0]"#, true, FAKE_PNG),
            &no_files,
        )
        .unwrap();
        let t = m.submeshes[0].tris[0];
        // Mirroring X reverses the winding; the front-end swaps corners 1 and 2.
        assert_eq!(t[1].pos, [0.0, 1.0, 0.0]);
        assert_eq!(t[2].pos, [-1.0, 0.0, 0.0]);
        let f = bevy_nds_3d_obj::flat_normal(t[0].pos, t[1].pos, t[2].pos);
        assert!(f[2] > 0.99, "face points away from +Z: {f:?}");
        assert!(t[0].normal[2] > 0.99, "{:?}", t[0].normal);
    }

    #[test]
    fn missing_normals_get_flat_normals() {
        let m = parse_gltf(
            &one_triangle_glb(r#""name": "n""#, false, FAKE_PNG),
            &no_files,
        )
        .unwrap();
        assert!(
            m.submeshes[0].tris[0]
                .iter()
                .all(|v| v.normal == [0.0, 0.0, 1.0])
        );
    }

    #[test]
    fn data_uris_are_rejected_with_export_advice() {
        let json = r#"{"asset": {"version": "2.0"},
            "buffers": [{"byteLength": 4, "uri": "data:application/octet-stream;base64,AAAAAA=="}]}"#;
        let err = parse_gltf(json.as_bytes(), &no_files).unwrap_err();
        assert!(err.contains(".glb"), "{err}");
    }
}
