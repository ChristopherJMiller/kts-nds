//! Host-side Wavefront OBJ → libnds display-list baker.
//!
//! The Nintendo DS has no runtime asset server, so model data is always *bytes
//! at a ROM address*. This crate turns an OBJ into the exact Geometry Engine
//! command block the DS draws with `glCallList`, doing all the fixed-point and
//! normal packing on the host. The same output is consumed two ways:
//!
//! - [`bevy_nds_3d_macros::include_obj!`] bakes it into the ARM9 binary at
//!   compile time (a `&'static` display list).
//! - The `obj2dl` converter writes it to a `.bin` placed in NitroFS and loaded
//!   at runtime.
//!
//! Keeping the encoder here means the packing math (which must match
//! `bevy_nds_3d::ffi`) lives in exactly one place.

pub mod ir;
pub mod obj;

use ir::{SourceModel, Triangle};
use std::fmt::Write as _;

/// Build-time origin adjustments for a model (applied to the baked geometry, so
/// they cost nothing at runtime).
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Recentre the geometry on the midpoint of its bounding box.
    pub center: bool,
    /// Constant translation applied to every vertex (after `center`).
    pub offset: [f32; 3],
    /// Use compressed `VTX_10` vertices (one command word each, 4.6 fixed)
    /// instead of `VERTEX16` (two words each, 4.12 fixed). Roughly a third
    /// smaller display list and DMA, at the cost of vertex precision (~1/64
    /// world unit), which can facet smooth surfaces. Off by default.
    pub compress: bool,
}

/// A baked model: a libnds display list plus its local-space bounding box.
#[derive(Clone, Debug)]
pub struct Model {
    /// The display list (leading body-length word, then packed commands).
    pub words: Vec<u32>,
    /// Local-space axis-aligned bounds, `[min, max]`.
    pub aabb: [[f32; 3]; 2],
}

/// Parse a Wavefront OBJ and bake it into a hardware-lit display list.
pub fn obj_to_display_list(source: &str, opts: &Options) -> Result<Model, String> {
    let mut tris = legacy_triangles(source)?;
    if tris.is_empty() {
        return Err("no triangles found".into());
    }
    apply_origin(&mut tris, opts);
    let (words, aabb) = display_list(&tris, opts.compress);
    Ok(Model { words, aabb })
}

/// Every triangle of an OBJ in file order, materials and UVs ignored — the
/// legacy `.dl` / `include_obj!` / editor-preview reading.
fn legacy_triangles(source: &str) -> Result<Vec<Triangle>, String> {
    let model = obj::parse_obj(source, obj::ObjOptions::default(), |_| Ok(String::new()))?;
    Ok(flatten(model))
}

/// Concatenate a model's sub-meshes, in order.
fn flatten(model: SourceModel) -> Vec<Triangle> {
    model.submeshes.into_iter().flat_map(|s| s.tris).collect()
}

/// Format the display list as a Rust `&[u32]` array body (hex, 12 per line),
/// for the proc-macro to splice into generated source.
pub fn format_words_rust(words: &[u32]) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i % 12 == 0 {
            out.push_str("\n        ");
        }
        let _ = write!(out, "0x{w:08X},");
    }
    out
}

/// Serialise the display list to little-endian bytes (for a NitroFS `.bin`).
pub fn words_to_le_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for w in words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Magic identifying a Bevy-DS display-list asset file: ASCII `"BDL1"`.
pub const ASSET_MAGIC: u32 = u32::from_le_bytes(*b"BDL1");

/// Serialise a baked model to the runtime NitroFS asset format.
///
/// Unlike [`words_to_le_bytes`] (raw display list only), this prepends a small
/// header so the runtime loader can recover the bounding box for frustum culling
/// without re-parsing the geometry. All fields are little-endian:
///
/// | offset | type      | field                     |
/// |--------|-----------|---------------------------|
/// | 0      | `u32`     | magic [`ASSET_MAGIC`]      |
/// | 4      | `f32` x3  | AABB min (x, y, z)        |
/// | 16     | `f32` x3  | AABB max (x, y, z)        |
/// | 28     | `u32`     | display-list word count   |
/// | 32     | `u32` x N | display list              |
pub fn model_to_le_bytes(model: &Model) -> Vec<u8> {
    let [min, max] = model.aabb;
    let mut out = Vec::with_capacity(32 + model.words.len() * 4);
    out.extend_from_slice(&ASSET_MAGIC.to_le_bytes());
    for axis in min.iter().chain(max.iter()) {
        out.extend_from_slice(&axis.to_le_bytes());
    }
    out.extend_from_slice(&(model.words.len() as u32).to_le_bytes());
    out.extend_from_slice(&words_to_le_bytes(&model.words));
    out
}

/// One triangle for host-side **preview** rendering: the three corner positions
/// plus the triangle's flat (geometric) normal. See [`obj_preview_mesh`].
#[derive(Clone, Copy, Debug)]
pub struct PreviewTri {
    /// Corner positions in the OBJ's local space (no origin adjustment applied).
    pub pos: [[f32; 3]; 3],
    /// Flat normal, normalised (zero if degenerate).
    pub normal: [f32; 3],
}

/// A parsed OBJ ready to *draw*: its triangles plus the local-space axis-aligned
/// bounding box (`[min, max]`).
#[derive(Clone, Debug)]
pub struct PreviewMesh {
    pub tris: Vec<PreviewTri>,
    pub aabb: [[f32; 3]; 2],
}

/// Parse a Wavefront OBJ into raw triangles for **preview** rendering (e.g. the
/// scene editor's 3D viewport).
///
/// Unlike [`obj_to_display_list`], this exposes the geometry instead of packing
/// it into a Geometry-Engine command block — a tool that *renders* the mesh needs
/// vertices, not GE commands. Parsing reuses the same [`parse_obj`] reader as the
/// baker, so OBJ support stays defined in one place (no origin adjustment is
/// applied; previews draw the geometry as authored).
pub fn obj_preview_mesh(source: &str) -> Result<PreviewMesh, String> {
    let tris = legacy_triangles(source)?;
    if tris.is_empty() {
        return Err("no triangles found".into());
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut out = Vec::with_capacity(tris.len());
    for t in &tris {
        let pos = [t[0].pos, t[1].pos, t[2].pos];
        for p in &pos {
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        out.push(PreviewTri {
            pos,
            normal: flat_normal(pos[0], pos[1], pos[2]),
        });
    }
    Ok(PreviewMesh {
        tris: out,
        aabb: [min, max],
    })
}

/// Shift the baked geometry's origin per the `center` / `offset` settings.
fn apply_origin(tris: &mut [Triangle], opts: &Options) {
    let mut shift = [0.0f32; 3];

    if opts.center {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for v in tris.iter().flatten() {
            for k in 0..3 {
                min[k] = min[k].min(v.pos[k]);
                max[k] = max[k].max(v.pos[k]);
            }
        }
        for k in 0..3 {
            shift[k] = -0.5 * (min[k] + max[k]);
        }
    }
    for k in 0..3 {
        shift[k] += opts.offset[k];
    }

    if shift == [0.0, 0.0, 0.0] {
        return;
    }
    for v in tris.iter_mut().flatten() {
        for k in 0..3 {
            v.pos[k] += shift[k];
        }
    }
}

/// Geometric (flat) normal of a triangle, normalised; zero if degenerate.
pub fn flat_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    normalize(n)
}

/// Normalise a vector; zero stays zero.
pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 0.0]
    }
}

// Packed Geometry-Engine FIFO command IDs, i.e. `REG2ID(reg) = (addr - 0x04000400) >> 2`
// from `<nds/arm9/videoGL.h>`. These index a register so four of them pack into
// one 32-bit word via [`fifo_pack`]; the arguments each command consumes then
// follow, in order, in the words after the packed-command word.
const FIFO_NOP: u8 = 0x00; // GFX_FIFO 0x04000400 — padding, no arguments
const FIFO_NORMAL: u8 = 0x21; // GFX_NORMAL 0x04000484 — 1 argument
const FIFO_VERTEX16: u8 = 0x23; // GFX_VERTEX16 0x0400048C — 2 arguments
const FIFO_VERTEX10: u8 = 0x24; // GFX_VERTEX10 0x04000490 — 1 argument (compressed)
const FIFO_TEX_COORD: u8 = 0x22; // GFX_TEX_COORD 0x04000488 — 1 argument
const FIFO_BEGIN: u8 = 0x40; // GFX_BEGIN 0x04000500 — 1 argument (primitive type)
const FIFO_END: u8 = 0x41; // GFX_END 0x04000504 — no arguments
/// `GL_TRIANGLES` primitive selector for `GFX_BEGIN`.
const GL_TRIANGLES: u32 = 0;

/// Build a libnds display list for a hardware-lit triangle mesh, plus its
/// local-space axis-aligned bounding box (`[min, max]`).
///
/// A display list is a self-contained command block the GPU consumes in one DMA
/// burst (`glCallList`). Its layout (see the BlocksDS `display_list_creation`
/// example) is: a leading word giving the body length in `u32`s, then the body —
/// words that pack four command IDs each ([`fifo_pack`]), with the arguments for
/// those commands following in order. We emit one `GFX_BEGIN(GL_TRIANGLES)`, then
/// per vertex a `GFX_NORMAL` (1 word) and either a `GFX_VERTEX16` (2 words) or,
/// when `compress` is set, a `GFX_VERTEX10` (1 word), then `GFX_END`.
/// Lighting/material/poly-format are set by the renderer *outside* the list, so
/// the same baked geometry honours the live material and lights.
fn display_list(tris: &[Triangle], compress: bool) -> (Vec<u32>, [[f32; 3]; 2]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];

    // (command id, its argument words), in submission order.
    let mut ops: Vec<(u8, Vec<u32>)> = Vec::with_capacity(tris.len() * 6 + 2);
    ops.push((FIFO_BEGIN, vec![GL_TRIANGLES]));
    for tri in tris {
        for v in tri {
            let pos = v.pos;
            for k in 0..3 {
                min[k] = min[k].min(pos[k]);
                max[k] = max[k].max(pos[k]);
            }
            let n = normalize(v.normal);
            ops.push((FIFO_NORMAL, vec![normal_pack(n[0], n[1], n[2])]));
            if compress {
                ops.push((FIFO_VERTEX10, vec![vertex10(pos[0], pos[1], pos[2])]));
            } else {
                let (xy, z) = vertex16(pos[0], pos[1], pos[2]);
                ops.push((FIFO_VERTEX16, vec![xy, z]));
            }
        }
    }
    ops.push((FIFO_END, vec![]));

    (pack_display_list(&ops), [min, max])
}

/// Largest model-space coordinate a `VERTEX16` (4.12 fixed, `i16`) can hold;
/// the smallest is −8.0.
pub const VERTEX_LIMIT: f32 = 32767.0 / 4096.0;

/// Encode **one sub-mesh** (one material) into a libnds display list (#66):
/// `GFX_BEGIN(GL_TRIANGLES)`, then per vertex an optional `GFX_TEX_COORD`, a
/// `GFX_NORMAL` and a `GFX_VERTEX16`, then `GFX_END`. With `tex_size = Some([w,
/// h])` every vertex must carry a UV; it is emitted in **texels** (12.4 fixed),
/// which is why the encoder needs the texture's size. Texture, material and
/// polygon format are set by the renderer *before* calling the list, so the list
/// never depends on where a texture lands in VRAM.
///
/// Unlike the legacy encoder this rejects geometry the hardware can't represent
/// (outside ±8 units, or texcoords beyond ±2048 texels) instead of wrapping it.
pub fn submesh_display_list(
    tris: &[Triangle],
    tex_size: Option<[u16; 2]>,
) -> Result<Vec<u32>, String> {
    let mut ops: Vec<(u8, Vec<u32>)> = Vec::with_capacity(tris.len() * 9 + 2);
    ops.push((FIFO_BEGIN, vec![GL_TRIANGLES]));
    for (ti, tri) in tris.iter().enumerate() {
        for v in tri {
            check_range(v.pos).map_err(|e| format!("triangle {ti}: {e}"))?;
            if let Some([w, h]) = tex_size {
                let uv = v.uv.ok_or_else(|| {
                    format!("triangle {ti}: textured material but a vertex has no UV")
                })?;
                let word = texcoord_pack(uv[0] * w as f32, uv[1] * h as f32)
                    .map_err(|e| format!("triangle {ti}: {e}"))?;
                ops.push((FIFO_TEX_COORD, vec![word]));
            }
            let n = normalize(v.normal);
            ops.push((FIFO_NORMAL, vec![normal_pack(n[0], n[1], n[2])]));
            let (xy, z) = vertex16(v.pos[0], v.pos[1], v.pos[2]);
            ops.push((FIFO_VERTEX16, vec![xy, z]));
        }
    }
    ops.push((FIFO_END, vec![]));
    Ok(pack_display_list(&ops))
}

/// Reject a position the 4.12 `VERTEX16` format can't hold (authoring contract
/// v1: a model fits within ±8 units of its origin).
fn check_range(p: [f32; 3]) -> Result<(), String> {
    for (k, c) in p.iter().enumerate() {
        if !c.is_finite() || *c < -8.0 || *c > VERTEX_LIMIT {
            return Err(format!(
                "vertex {} = {c} is outside the DS ±8 model-space range (authoring contract v1, #66)",
                ["x", "y", "z"][k]
            ));
        }
    }
    Ok(())
}

/// Pack texel-space `(s, t)` into a `GFX_TEX_COORD` word: each 12.4 fixed `i16`,
/// `s` in the low half — libnds `TEXTURE_PACK`.
fn texcoord_pack(s: f32, t: f32) -> Result<u32, String> {
    let q = |v: f32| -> Result<u32, String> {
        let r = (v * 16.0).round();
        if !(-32768.0..=32767.0).contains(&r) {
            return Err(format!(
                "texture coordinate {v} texels is outside the DS ±2048-texel range"
            ));
        }
        Ok(r as i32 as i16 as u16 as u32)
    };
    Ok(q(s)? | (q(t)? << 16))
}

/// Pack four FIFO command IDs into one little-endian word (`c0` in the low byte),
/// matching libnds' `FIFO_COMMAND_PACK`.
fn fifo_pack(cmds: [u8; 4]) -> u32 {
    (cmds[0] as u32) | ((cmds[1] as u32) << 8) | ((cmds[2] as u32) << 16) | ((cmds[3] as u32) << 24)
}

/// Encode `(command, args)` ops into the display-list `u32` stream: a leading
/// body-length word, then groups of one packed-command word (four IDs, padded
/// with [`FIFO_NOP`]) followed by those commands' argument words in order.
fn pack_display_list(ops: &[(u8, Vec<u32>)]) -> Vec<u32> {
    let mut body: Vec<u32> = Vec::new();
    for chunk in ops.chunks(4) {
        let mut ids = [FIFO_NOP; 4];
        for (i, (cmd, _)) in chunk.iter().enumerate() {
            ids[i] = *cmd;
        }
        body.push(fifo_pack(ids));
        for (_, args) in chunk {
            body.extend_from_slice(args);
        }
    }

    let mut out = Vec::with_capacity(body.len() + 1);
    out.push(body.len() as u32); // glCallList: first word is the body length in words
    out.extend_from_slice(&body);
    out
}

/// Pack a position into the DS `GFX_VERTEX16` command pair, matching
/// `bevy_nds_3d::ffi::gl::vertex_v16`: each component is 4.12 fixed (`* 4096`),
/// `(xy, z)` as two command words.
fn vertex16(x: f32, y: f32, z: f32) -> (u32, u32) {
    let xi = (x * 4096.0) as i16 as u16 as u32;
    let yi = (y * 4096.0) as i16 as u16 as u32;
    let zi = (z * 4096.0) as i16 as u16 as u32;
    ((yi << 16) | xi, zi)
}

/// Pack a position into the DS compressed `GFX_VERTEX10` command word: each
/// component is 4.6 fixed (`* 64`), 10 bits each (`x` low, then `y`, then `z`).
/// Same ±8 range as `VERTEX16` but coarser (~1/64 unit) — see [`Options::compress`].
fn vertex10(x: f32, y: f32, z: f32) -> u32 {
    let c = |v: f32| ((v * 64.0) as i32 as u32) & 0x3FF;
    c(x) | (c(y) << 10) | (c(z) << 20)
}

/// Pack a unit normal into the DS `GFX_NORMAL` command word, matching
/// `bevy_nds_3d::ffi::normal_pack` (10-bit signed per component).
fn normal_pack(x: f32, y: f32, z: f32) -> u32 {
    float_to_v10(x) | (float_to_v10(y) << 10) | (float_to_v10(z) << 20)
}

/// Float → 10-bit signed `v10` (1.0 → 0x1FF), matching `ffi::float_to_v10`.
fn float_to_v10(v: f32) -> u32 {
    let x = if v >= 1.0 {
        0x1FF
    } else if v < -1.0 {
        0x200
    } else {
        ((v * 512.0) as i32) & 0x3FF
    };
    x as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_pack_is_little_endian() {
        assert_eq!(fifo_pack([0x40, 0x21, 0x23, 0x41]), 0x4123_2140);
        // NOP padding lands in the high bytes.
        assert_eq!(
            fifo_pack([FIFO_END, FIFO_NOP, FIFO_NOP, FIFO_NOP]),
            0x0000_0041
        );
    }

    /// One triangle → BEGIN + 3×(NORMAL, VERTEX16) + END = 8 ops, packed four to
    /// a command word with their args interleaved, and a correct length header.
    #[test]
    fn single_triangle_display_list_layout() {
        let tris =
            legacy_triangles("v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1\n").unwrap();
        assert_eq!(tris.len(), 1);

        let (words, _aabb) = display_list(&tris, false);

        // 8 ops → 2 command words. Args: BEGIN 1, NORMAL 1 (×3), VERTEX16 2 (×3),
        // END 0 = 1 + 3 + 6 = 10 arg words. Body = 2 + 10 = 12; +1 length word.
        assert_eq!(words[0], 12, "length header counts body words only");
        assert_eq!(words.len(), 13);

        // First command word packs BEGIN, NORMAL, VERTEX16, NORMAL.
        assert_eq!(
            words[1],
            fifo_pack([FIFO_BEGIN, FIFO_NORMAL, FIFO_VERTEX16, FIFO_NORMAL])
        );
        // BEGIN's argument is the GL_TRIANGLES selector.
        assert_eq!(words[2], GL_TRIANGLES);

        // chunk0 args (BEGIN1 + NORMAL1 + VERTEX16:2 + NORMAL1 = 5) put the second
        // command word at index 7, packing the run's tail: VERTEX16, NORMAL,
        // VERTEX16, END (exactly four ops, so no NOP padding needed here).
        assert_eq!(
            words[7],
            fifo_pack([FIFO_VERTEX16, FIFO_NORMAL, FIFO_VERTEX16, FIFO_END])
        );
    }

    /// The packing math must stay identical to `bevy_nds_3d::ffi` so baked words
    /// mean the same thing as the runtime path (4.12 fixed; v10 signed normals).
    #[test]
    fn packing_matches_hardware_format() {
        // 1.0 in 4.12 fixed is 0x1000; packed (x,y) low/high halves.
        assert_eq!(vertex16(1.0, 0.0, 0.0), (0x0000_1000, 0x0000_0000));
        // -1.0 → 0xF000 as i16, zero-extended into the half word.
        assert_eq!(vertex16(-1.0, 0.0, 0.0).0 & 0xFFFF, 0xF000);

        // Unit +Z normal: only the z field (bits 20..30) is set to +0.998 (0x1FF).
        assert_eq!(normal_pack(0.0, 0.0, 1.0), 0x1FF << 20);
        // float_to_v10 clamps to the representable signed range.
        assert_eq!(float_to_v10(2.0), 0x1FF);
        assert_eq!(float_to_v10(-2.0), 0x200);
    }

    /// Quad faces are fan-triangulated into two triangles.
    #[test]
    fn quads_are_fan_triangulated() {
        let tris = legacy_triangles("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n").unwrap();
        assert_eq!(tris.len(), 2);
    }

    /// `center` recentres geometry on its bounding-box midpoint.
    #[test]
    fn center_recentres_bounds() {
        let src = "v 0 0 0\nv 2 0 0\nv 0 2 0\nf 1 2 3\n";
        let m = obj_to_display_list(
            src,
            &Options {
                center: true,
                ..Default::default()
            },
        )
        .unwrap();
        // Original midpoint was (0.666, 0.666, 0); after centring, bounds are
        // symmetric about the origin on each axis.
        for k in 0..3 {
            assert!(
                (m.aabb[0][k] + m.aabb[1][k]).abs() < 1e-4,
                "axis {k} not centred"
            );
        }
    }

    /// `VTX_10` packs three 4.6-fixed components into one 10-bit-each word.
    #[test]
    fn vertex10_packs_three_components() {
        // 1.0 in 4.6 fixed is 64 (0x40); -1.0 is -64 → 0x3C0 in 10-bit two's-comp.
        assert_eq!(vertex10(1.0, 0.0, 0.0), 0x40);
        assert_eq!(vertex10(0.0, 1.0, 0.0), 0x40 << 10);
        assert_eq!(vertex10(0.0, 0.0, 1.0), 0x40 << 20);
        assert_eq!(vertex10(-1.0, 0.0, 0.0) & 0x3FF, 0x3C0);
    }

    /// Compression swaps each two-word VERTEX16 for a one-word VERTEX10, so the
    /// compressed list is shorter but encodes the same triangle count.
    #[test]
    fn compress_shrinks_display_list() {
        let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1\n";
        let plain = obj_to_display_list(src, &Options::default()).unwrap();
        let packed = obj_to_display_list(
            src,
            &Options {
                compress: true,
                ..Default::default()
            },
        )
        .unwrap();
        // One triangle = 3 vertices; compression saves one word per vertex.
        assert_eq!(plain.words.len() - packed.words.len(), 3);
        assert_eq!(plain.aabb, packed.aabb);
    }

    /// FNV-1a-64 over the display-list words (LE) then the AABB's f32 bits —
    /// a compact fingerprint of everything the legacy encoder emits.
    fn fnv(words: &[u32], aabb: &[[f32; 3]; 2]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: u8| {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for w in words {
            for b in w.to_le_bytes() {
                eat(b);
            }
        }
        for v in aabb.iter().flatten() {
            for b in v.to_bits().to_le_bytes() {
                eat(b);
            }
        }
        h
    }

    /// The #66 refactor (OBJ parsing moved onto the shared IR) must not change a
    /// single byte of the legacy `.dl` / `include_obj!` output. Values captured
    /// from the pre-refactor encoder on 2026-10-07.
    #[test]
    fn legacy_encoder_is_byte_identical_on_committed_assets() {
        let cases: [(&str, bool, usize, u64); 8] = [
            ("cube", false, 129, 0xA090_E6ED_B4C1_E052),
            ("cube", true, 129, 0xA090_E6ED_B4C1_E052),
            ("teapot", false, 5799, 0x704A_BDB1_6985_73AD),
            ("teapot", true, 5799, 0xECF3_5A54_0E95_6C3C),
            ("prim_ramp", false, 87, 0xAAF6_5470_1B0A_B47E),
            ("prim_ramp", true, 87, 0xAAF6_5470_1B0A_B47E),
            ("prim_cylinder", false, 507, 0x06EB_AC94_A57B_1947),
            ("prim_cylinder", true, 507, 0x06EB_AC94_A57B_1947),
        ];
        for (name, center, len, hash) in cases {
            let path = format!("{}/../../assets/{name}.obj", env!("CARGO_MANIFEST_DIR"));
            let src = std::fs::read_to_string(&path).unwrap();
            let m = obj_to_display_list(
                &src,
                &Options {
                    center,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(m.words.len(), len, "{name} center={center}");
            assert_eq!(fnv(&m.words, &m.aabb), hash, "{name} center={center}");
        }
    }

    use crate::ir::Vertex;

    fn tri_uv(uv: Option<[f32; 2]>) -> [Vertex; 3] {
        let v = |p: [f32; 3]| Vertex {
            pos: p,
            normal: [0.0, 0.0, 1.0],
            uv,
        };
        [v([0.0, 0.0, 0.0]), v([1.0, 0.0, 0.0]), v([0.0, 1.0, 0.0])]
    }

    /// Textured: per vertex TEX_COORD, NORMAL, VERTEX16. `uv` (0.5, 0.25) on a
    /// 32×16 texture is (16, 4) texels → 12.4 fixed (0x100, 0x40).
    #[test]
    fn texcoord_is_packed_in_texels() {
        let words = submesh_display_list(&[tri_uv(Some([0.5, 0.25]))], Some([32, 16])).unwrap();
        assert_eq!(
            words[1],
            fifo_pack([FIFO_BEGIN, FIFO_TEX_COORD, FIFO_NORMAL, FIFO_VERTEX16])
        );
        assert_eq!(words[2], GL_TRIANGLES);
        assert_eq!(words[3], 0x0040_0100);
    }

    #[test]
    fn negative_texcoords_are_twos_complement() {
        let words = submesh_display_list(&[tri_uv(Some([-0.5, 0.0]))], Some([16, 16])).unwrap();
        // -8 texels → -128 in 12.4 → 0xFF80 in the low half.
        assert_eq!(words[3], 0x0000_FF80);
    }

    /// Untextured sub-meshes encode exactly like the legacy list.
    #[test]
    fn untextured_submesh_matches_legacy_list() {
        let src = std::fs::read_to_string(format!(
            "{}/../../assets/cube.obj",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let tris = legacy_triangles(&src).unwrap();
        assert_eq!(
            submesh_display_list(&tris, None).unwrap(),
            display_list(&tris, false).0
        );
    }

    #[test]
    fn textured_vertex_without_uv_is_an_error() {
        let err = submesh_display_list(&[tri_uv(None)], Some([8, 8])).unwrap_err();
        assert!(err.contains("no UV"), "{err}");
    }

    #[test]
    fn vertex_outside_pm8_is_an_error() {
        let mut t = tri_uv(None);
        t[1].pos[0] = 9.0;
        let err = submesh_display_list(&[t], None).unwrap_err();
        assert!(err.contains("±8"), "{err}");
    }

    #[test]
    fn uv_outside_texel_range_is_an_error() {
        let err = submesh_display_list(&[tri_uv(Some([40.0, 0.0]))], Some([256, 256])).unwrap_err();
        assert!(err.contains("2048"), "{err}");
    }
}
