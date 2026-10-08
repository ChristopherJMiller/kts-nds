//! The `.dsm` model container (#66).

use bevy_nds_3d_obj::ir::{SourceModel, SubMesh, Wrap};
use bevy_nds_3d_obj::submesh_display_list;

/// Magic identifying a `.dsm` model: ASCII `"DSM1"`.
pub const DSM_MAGIC: u32 = u32::from_le_bytes(*b"DSM1");
/// Sub-mesh texture index meaning "untextured".
pub const NO_TEXTURE: u16 = 0xFFFF;

/// A texture as the container references it.
#[derive(Clone, Copy, Debug)]
pub struct TexRef<'a> {
    /// NitroFS path of the `.tex` blob (stored NUL-terminated).
    pub nitro_path: &'a str,
    pub width: u16,
    pub height: u16,
}

/// The ambient colour every material gets: a quarter of its diffuse (#66 — MTL
/// `Ka` is ignored so OBJ and glTF exports of one model match).
pub fn ambient(diffuse: [u8; 3]) -> [u8; 3] {
    diffuse.map(|c| c / 4)
}

/// `TEXIMAGE_PARAM`-ordered wrap bits: 0 repeat S, 1 repeat T, 2 flip S, 3 flip T.
pub fn wrap_bits(w: Wrap) -> u8 {
    (w.repeat_s as u8) | ((w.repeat_t as u8) << 1) | ((w.flip_s as u8) << 2) | ((w.flip_t as u8) << 3)
}

/// Serialise a model to the runtime `.dsm` format. `table` lists the model's
/// textures; `sub_tex` gives each sub-mesh's index into it (`None` =
/// untextured), one entry per `model.submeshes`. Empty sub-meshes are skipped.
/// All fields little-endian, every record 4-byte aligned:
///
/// | offset | type | field |
/// |---|---|---|
/// | 0 | `u32` | magic [`DSM_MAGIC`] |
/// | 4 | `f32` × 3 | AABB min |
/// | 16 | `f32` × 3 | AABB max |
/// | 28 | `u16` | texture count T |
/// | 30 | `u16` | sub-mesh count S |
/// | 32 | T × | `u16` path length incl. NUL, `u16` 0, path bytes + NUL, zero-pad to 4 |
/// | … | S × | `u16` texture index ([`NO_TEXTURE`] = none), `u8` [`wrap_bits`], `u8` 0, `u8` × 3 diffuse, `u8` × 3 ambient, `u8` × 2 0, `u32` word count N, `u32` × N display list (leading body-length word included) |
pub fn encode(model: &SourceModel, table: &[TexRef], sub_tex: &[Option<usize>]) -> Result<Vec<u8>, String> {
    if sub_tex.len() != model.submeshes.len() {
        return Err("internal: one texture slot per sub-mesh expected".into());
    }
    let aabb = model.aabb().ok_or("model has no triangles")?;
    let subs: Vec<(&SubMesh, Option<usize>)> = model
        .submeshes
        .iter()
        .zip(sub_tex.iter().copied())
        .filter(|(s, _)| !s.tris.is_empty())
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&DSM_MAGIC.to_le_bytes());
    for v in aabb.iter().flatten() {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&(table.len() as u16).to_le_bytes());
    out.extend_from_slice(&(subs.len() as u16).to_le_bytes());
    for t in table {
        let mut path = t.nitro_path.as_bytes().to_vec();
        path.push(0);
        out.extend_from_slice(&(path.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&path);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    for (s, tex) in subs {
        let size = match tex {
            Some(i) => {
                let t = table.get(i).ok_or("internal: texture index out of range")?;
                Some([t.width, t.height])
            }
            None => None,
        };
        let words = submesh_display_list(&s.tris, size).map_err(|e| format!("material `{}`: {e}", s.material.name))?;
        out.extend_from_slice(&tex.map_or(NO_TEXTURE, |i| i as u16).to_le_bytes());
        out.push(wrap_bits(s.material.wrap));
        out.push(0);
        out.extend_from_slice(&s.material.diffuse);
        out.extend_from_slice(&ambient(s.material.diffuse));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&(words.len() as u32).to_le_bytes());
        for w in &words {
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_nds_3d_obj::ir::{Material, SubMesh, Vertex};

    fn tri(uv: bool) -> [Vertex; 3] {
        let v = |p: [f32; 3], t: [f32; 2]| Vertex { pos: p, normal: [0.0, 0.0, 1.0], uv: uv.then_some(t) };
        [v([0.0, 0.0, 0.0], [0.0, 0.0]), v([1.0, 0.0, 0.0], [1.0, 0.0]), v([0.0, 1.0, 0.0], [0.0, 1.0])]
    }

    fn model() -> SourceModel {
        SourceModel {
            submeshes: vec![
                SubMesh {
                    material: Material { name: "skin".into(), diffuse: [200, 100, 40], ..Material::default() },
                    tris: vec![tri(true)],
                },
                SubMesh { material: Material::default(), tris: vec![tri(false)] },
            ],
            images: Vec::new(),
        }
    }

    fn u16_at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }
    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    #[test]
    fn container_layout() {
        let table = [TexRef { nitro_path: "nitro:/models/a.tex", width: 8, height: 8 }];
        let b = encode(&model(), &table, &[Some(0), None]).unwrap();
        assert_eq!(&b[0..4], b"DSM1");
        assert_eq!(f32::from_le_bytes([b[16], b[17], b[18], b[19]]), 1.0); // aabb max.x
        assert_eq!(u16_at(&b, 28), 1); // textures
        assert_eq!(u16_at(&b, 30), 2); // sub-meshes
        // Texture entry: 20 path bytes incl. NUL, then padding to 4.
        assert_eq!(u16_at(&b, 32), 20);
        assert_eq!(&b[36..56], b"nitro:/models/a.tex\0");
        // First sub-mesh header at 56.
        assert_eq!(u16_at(&b, 56), 0); // texture index
        assert_eq!(b[58], 0b0011); // default wrap: repeat S + T
        assert_eq!(&b[60..63], &[200, 100, 40]); // diffuse
        assert_eq!(&b[63..66], &[50, 25, 10]); // ambient = diffuse / 4
        let words = bevy_nds_3d_obj::submesh_display_list(&model().submeshes[0].tris, Some([8, 8])).unwrap();
        assert_eq!(u32_at(&b, 68), words.len() as u32);
        let second = 72 + words.len() * 4;
        assert_eq!(u16_at(&b, second), NO_TEXTURE);
    }

    #[test]
    fn textured_submesh_without_uvs_is_an_error() {
        let table = [TexRef { nitro_path: "nitro:/models/a.tex", width: 8, height: 8 }];
        let err = encode(&model(), &table, &[Some(0), Some(0)]).unwrap_err();
        assert!(err.contains("no UV"), "{err}");
    }
}
