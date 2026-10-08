//! Host-side **source model** representation (#66): what every model front-end
//! (OBJ + MTL in [`crate::obj`], glTF in `model2dsm`) produces and every encoder
//! consumes. Geometry is grouped into sub-meshes, one per material, because the
//! DS binds one texture + material per display list.

/// One corner of a triangle in model space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    /// Surface normal **as the source gave it** (not necessarily unit length);
    /// front-ends fill a flat normal when the source has none. Encoders normalise.
    pub normal: [f32; 3],
    /// Texture coordinate with a **top-left** origin (v grows downward) — the
    /// glTF / DS convention. OBJ's bottom-left `vt` is flipped on parse. `None`
    /// when the source gave this corner no coordinate.
    pub uv: Option<[f32; 2]>,
}

/// Three corners, counter-clockwise when seen from the front.
pub type Triangle = [Vertex; 3];

/// How a texture behaves outside `0..1`: maps onto the DS `TEXIMAGE_PARAM`
/// repeat / flip bits; an axis that doesn't repeat clamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wrap {
    pub repeat_s: bool,
    pub repeat_t: bool,
    pub flip_s: bool,
    pub flip_t: bool,
}

impl Default for Wrap {
    fn default() -> Self {
        Self {
            repeat_s: true,
            repeat_t: true,
            flip_s: false,
            flip_t: false,
        }
    }
}

/// Where a material's texture image comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextureSrc {
    /// A path as written in the source (MTL `map_Kd`, glTF image `uri`),
    /// relative to the directory of the file that named it.
    File(String),
    /// Index into [`SourceModel::images`] (an image embedded in a `.glb`).
    Embedded(usize),
}

/// A surface description. Ambient is not stored: the encoder derives it from
/// `diffuse` so OBJ and glTF exports of one model render identically (#66).
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    /// MTL `newmtl` / glTF material name; `""` for the default material.
    pub name: String,
    /// Diffuse colour (MTL `Kd`, glTF `baseColorFactor`), 0–255 per channel.
    pub diffuse: [u8; 3],
    pub texture: Option<TextureSrc>,
    pub wrap: Wrap,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: String::new(),
            diffuse: [255, 255, 255],
            texture: None,
            wrap: Wrap::default(),
        }
    }
}

/// The triangles drawn with one material.
#[derive(Clone, Debug, PartialEq)]
pub struct SubMesh {
    pub material: Material,
    pub tris: Vec<Triangle>,
}

/// A whole model as authored, before any DS encoding.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceModel {
    pub submeshes: Vec<SubMesh>,
    /// Encoded images embedded in the source (PNG bytes), indexed by
    /// [`TextureSrc::Embedded`].
    pub images: Vec<Vec<u8>>,
}

impl SourceModel {
    pub fn triangle_count(&self) -> usize {
        self.submeshes.iter().map(|s| s.tris.len()).sum()
    }

    /// Model-space bounds over every sub-mesh, `[min, max]`; `None` when empty.
    pub fn aabb(&self) -> Option<[[f32; 3]; 2]> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for tri in self.submeshes.iter().flat_map(|s| &s.tris) {
            for v in tri {
                any = true;
                for k in 0..3 {
                    min[k] = min[k].min(v.pos[k]);
                    max[k] = max[k].max(v.pos[k]);
                }
            }
        }
        any.then_some([min, max])
    }
}
