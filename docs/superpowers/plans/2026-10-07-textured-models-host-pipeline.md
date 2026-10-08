# Textured Models — Host Pipeline (Plan A of #66) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bake Blender exports (OBJ+MTL and glTF/GLB, with PNG textures) under `assets/models/**` into DS-ready model (`.dsm`) and texture (`.tex`) blobs, and fail a level's bake when its textures exceed the VRAM budget — all host-side and unit-tested, without touching the DS runtime yet.

**Architecture:** A shared host-side *source model* representation (`bevy_nds_3d_obj::ir`) sits between two front-ends — the existing OBJ parser, extended with `vt` / `usemtl` / `mtllib`, and a new glTF front-end — and one encoder (`submesh_display_list`, one display list per material). A new host crate `model2dsm` owns the glTF front-end, a pure-Rust PNG → DS paletted texture encoder, the `.dsm` container, the directory baker and a mesh-name `Catalog`; `scene2bin` uses the catalog to enforce the per-level texture budget. The legacy `assets/*.obj` → `.dl` path stays byte-identical.

**Tech Stack:** Rust 2024, `png` 0.18, `gltf` 1.4 (`default-features = false`, `utils` + `names`), the repo's `just` recipes inside `nix develop`.

**Spec:** GitHub issue #66, `## Locked` — https://github.com/ChristopherJMiller/kts-nds/issues/66 (read it first). Hub: #17.

## Global Constraints

- Run every command inside the Nix shell: `nix develop -c <cmd>`.
- Work on branch `feat/66-textured-models-host` (create it in Task 1, Step 0). Commit after every task; end each commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Host-only.** No FFI, no change to the `.scene` format, `bevy_nds_scene`, `bevy_nds_3d` or the game crate in this plan (that is Plan B).
- **Legacy output is byte-identical.** `obj_to_display_list` on `assets/{cube,teapot,prim_ramp,prim_cylinder}.obj` must keep these (word count, FNV-1a-64 of LE words + AABB bits): cube 129 / `0xA090E6EDB4C1E052` (both `center` settings); teapot 5799 / `0x704ABDB1698573AD` (`center: false`) and `0xECF35A540E956C3C` (`center: true`); prim_ramp 87 / `0xAAF654701B0AB47E`; prim_cylinder 507 / `0x06EBAC94A57B1947`.
- **Source formats:** `assets/models/**` — `.obj` (+ `.mtl`), `.gltf` (+ `.bin`), `.glb`; textures are PNG files next to their model, or PNGs embedded in a `.glb`. `data:` URIs are rejected with a message telling the artist to export `.glb` or glTF Separate.
- **Mesh names:** a model's path under `assets/models/` without extension, `/`-separated (`props/crate.glb` → `props/crate`). Legacy `assets/*.obj` keep their bare stem. A name defined twice is a bake **Error**.
- **New models keep their authored origin** — nothing under `assets/models/**` is recentred.
- **UV convention in the IR:** top-left origin (glTF / DS). OBJ `vt` is flipped (`v' = 1 − v`) on parse.
- **Texture rules (authoring contract v1):** each side a power of two in `{8, 16, 32, 64, 128, 256}`; format picked from the colour count after 15-bit conversion — ≤ 4 entries → 2bpp (`Pal4`, DS code 2), ≤ 16 → 4bpp (`Pal16`, 3), ≤ 256 → 8bpp (`Pal256`, 4); pixels with alpha 0 become palette index 0 (and take a palette slot); more than 256 entries is an Error. Texels are row-major, first pixel in the lowest bits.
- **Geometry rules:** model-space coordinates within the `VERTEX16` range (−8 .. 32767/4096); texture coordinates within ±2048 texels (12.4 fixed, `i16`); a bake **warning** above 500 triangles per model.
- **Material colours:** diffuse = MTL `Kd` / glTF `baseColorFactor` (white when absent). Ambient is **always** `diffuse / 4` per channel — MTL `Ka` is ignored (Blender writes `Ka 1 1 1`, which would wash out the DS lighting, and glTF has no ambient, so ignoring it keeps both exports of one model identical).
- **Budget constants:** `TEXTURE_VRAM_BYTES = 262_144` (banks B + D), `PALETTE_VRAM_BYTES = 16_384` (bank F). Palette cost per texture rounds up to 16 bytes.
- Group-1 test command (pure host crates): `nix develop -c cargo test -p <crate> --target x86_64-unknown-linux-gnu`. Group-2 crates (e.g. `scene2bin`): `nix develop -c just test-crate <crate> [filter]`. **`just test <arg>` is a test-name filter, not a crate filter.**

## Review Focus

- **A mirrored glTF node** (negative scale on one axis, common after Blender's mirror) must keep outward-facing winding, or the DS back-face culls it inside-out → test `mirrored_node_keeps_outward_winding` (Task 4).
- **One PNG shared by two models** must bake once and count once against the level budget → tests `shared_texture_bakes_once` (Task 5) and `texture_budget_counts_shared_textures_once` (Task 7).
- **Indexed / grayscale / RGB PNGs** (sprite tools and Blender emit all of these) must decode to the same texture as the equivalent RGBA → test `png_colour_types_decode_alike` (Task 3).
- **A mesh name defined twice** (`assets/crate.obj` + `assets/models/crate.glb`, or `crate.obj` + `crate.glb` side by side) must be an Error naming both files, never a silent pick → tests `duplicate_model_names_are_an_error` (Task 5) and `legacy_and_model_name_clash_is_an_error` (Task 6).
- **A bad texture reference** (`map_Kd ../../elsewhere.png` escaping `assets/models`, or a missing file) must be an Error naming the material, not a panic → tests `texture_outside_models_root_is_an_error` and `missing_texture_file_is_an_error` (Task 5).

---

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `crates/bevy_nds_3d_obj/src/ir.rs` | Create | Source-model types: `Vertex`, `Triangle`, `Wrap`, `TextureSrc`, `Material`, `SubMesh`, `SourceModel`. |
| `crates/bevy_nds_3d_obj/src/obj.rs` | Create | OBJ + MTL front-end → `SourceModel` (`parse_obj`, `parse_mtl`, `ObjOptions`). |
| `crates/bevy_nds_3d_obj/src/lib.rs` | Modify | Legacy encoder rebuilt on the IR; `flat_normal` / `normalize` made `pub`; new `submesh_display_list` (texcoords, range checks). |
| `crates/model2dsm/Cargo.toml` | Create | New host crate (lib + `model2dsm` CLI). |
| `crates/model2dsm/src/texture.rs` | Create | PNG / RGBA → `DsTexture` (paletted 2/4/8 bpp) + `.tex` blob. |
| `crates/model2dsm/src/gltf_src.rs` | Create | glTF / GLB front-end → `SourceModel` (node transforms baked in). |
| `crates/model2dsm/src/dsm.rs` | Create | `.dsm` container writer. |
| `crates/model2dsm/src/catalog.rs` | Create | `Catalog` (mesh names → texture costs) + cheap `mesh_names`. |
| `crates/model2dsm/src/lib.rs` | Create | Constants, `load_model`, model walking / naming, texture resolution, `build_dir`. |
| `crates/model2dsm/src/main.rs` | Create | `model2dsm <models-dir> <out-dir>` CLI. |
| `crates/scene2bin/Cargo.toml`, `src/lib.rs` | Modify | `validate_all_with_catalog` + level texture-budget rule; bake / `--check` use the catalog; `mesh_names` re-export for the editor. |
| `tools/scene-editor/src/app.rs` | Modify | Mesh picker lists `assets/models/**` names too (one line). |
| `build.rs`, `Cargo.toml`, `Justfile`, `CLAUDE.md` | Modify | `compile_models` bake step, workspace member + build-dep, test group 1, docs. |

---

### Task 1: Source-model IR + OBJ/MTL front-end (legacy output unchanged)

**Files:**
- Create: `crates/bevy_nds_3d_obj/src/ir.rs`, `crates/bevy_nds_3d_obj/src/obj.rs`
- Modify: `crates/bevy_nds_3d_obj/src/lib.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces (later tasks rely on these exact names):
  - `bevy_nds_3d_obj::ir::{Vertex { pos: [f32;3], normal: [f32;3], uv: Option<[f32;2]> }, Triangle = [Vertex;3], Wrap { repeat_s, repeat_t, flip_s, flip_t: bool } (Default = repeat both), TextureSrc::{File(String), Embedded(usize)}, Material { name: String, diffuse: [u8;3], texture: Option<TextureSrc>, wrap: Wrap } (Default = "" / white / None / Wrap::default()), SubMesh { material: Material, tris: Vec<Triangle> }, SourceModel { submeshes: Vec<SubMesh>, images: Vec<Vec<u8>> }}` with `SourceModel::triangle_count(&self) -> usize` and `SourceModel::aabb(&self) -> Option<[[f32;3];2]>`.
  - `bevy_nds_3d_obj::obj::{ObjOptions { materials: bool }, parse_obj(source: &str, opts: ObjOptions, load_mtl: impl FnMut(&str) -> Result<String, String>) -> Result<SourceModel, String>, parse_mtl(source: &str) -> Result<Vec<Material>, String>, unit_to_u8(f32) -> u8}`.
  - `bevy_nds_3d_obj::{flat_normal(a,b,c: [f32;3]) -> [f32;3], normalize([f32;3]) -> [f32;3]}` (now `pub`).
  - `Vertex.normal` is the **raw** source normal (not necessarily unit); encoders normalise once.

- [ ] **Step 0: Branch**

```bash
git checkout -b feat/66-textured-models-host
```

- [ ] **Step 1: Pin today's legacy output (characterisation test)**

Append to the `tests` module in `crates/bevy_nds_3d_obj/src/lib.rs`:

```rust
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
```

- [ ] **Step 2: Run it — it must PASS before any refactor**

Run: `nix develop -c cargo test -p bevy_nds_3d_obj --target x86_64-unknown-linux-gnu legacy_encoder_is_byte_identical`
Expected: `test result: ok. 1 passed`. (If it fails, stop: the golden values or the assets changed.)

- [ ] **Step 3: Create the IR**

Create `crates/bevy_nds_3d_obj/src/ir.rs`:

```rust
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
```

- [ ] **Step 4: Write the failing front-end tests**

Create `crates/bevy_nds_3d_obj/src/obj.rs` containing **only** the test module for now (the implementation comes in Step 6):

```rust
//! Wavefront OBJ (+ MTL) front-end → [`SourceModel`] (#66).

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
        let names: Vec<&str> = m.submeshes.iter().map(|s| s.material.name.as_str()).collect();
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
    fn undefined_material_is_an_error() {
        let src = "v 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl ghost\nf 1 2 3\n";
        let err = parse_obj(src, ObjOptions { materials: true }, |_| Ok(String::new())).unwrap_err();
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
```

Add the modules at the top of `crates/bevy_nds_3d_obj/src/lib.rs` (after the crate doc comment, before `use std::fmt::Write as _;`):

```rust
pub mod ir;
pub mod obj;
```

- [ ] **Step 5: Run the tests to verify they fail**

Run: `nix develop -c cargo test -p bevy_nds_3d_obj --target x86_64-unknown-linux-gnu`
Expected: compile error — `cannot find function parse_obj` / `ObjOptions` in `obj.rs`.

- [ ] **Step 6: Implement the front-end**

Insert above the `#[cfg(test)]` module in `crates/bevy_nds_3d_obj/src/obj.rs`:

```rust
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
        let mut it = line.trim().split_whitespace();
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
                for name in it {
                    let text = load_mtl(name).map_err(|e| at(&e))?;
                    library.extend(parse_mtl(&text).map_err(|e| at(&format!("{name}: {e}")))?);
                }
            }
            Some("usemtl") if opts.materials => {
                let name = it.next().unwrap_or("");
                current = Some(submesh_index(&mut subs, name, &library).map_err(|e| at(&e))?);
            }
            Some("f") => {
                let mut corners: Vec<([f32; 3], Option<[f32; 3]>, Option<[f32; 2]>)> = Vec::new();
                for tok in it {
                    let (vi, ti, ni) = parse_face_vertex(tok)
                        .ok_or_else(|| at(&format!("malformed face vertex {tok:?}")))?;
                    let pos = *resolve(&positions, vi).ok_or_else(|| at("vertex index out of range"))?;
                    let nor = match ni {
                        Some(ni) => Some(*resolve(&normals, ni).ok_or_else(|| at("normal index out of range"))?),
                        None => None,
                    };
                    let uv = match ti {
                        Some(ti) if opts.materials => Some(
                            *resolve(&uvs, ti).ok_or_else(|| at("texture coordinate index out of range"))?,
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
fn submesh_index(subs: &mut Vec<SubMesh>, name: &str, library: &[Material]) -> Result<usize, String> {
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

/// Parse the MTL subset the pipeline uses: `newmtl`, `Kd`, `map_Kd`. `map_Kd`'s
/// **last** token is the path (options such as `-s 1 1 1` come first), so
/// texture paths can't contain spaces. `Ka` is deliberately ignored — see
/// [`Material`].
pub fn parse_mtl(source: &str) -> Result<Vec<Material>, String> {
    let mut out: Vec<Material> = Vec::new();
    for (lineno, line) in source.lines().enumerate() {
        let mut it = line.trim().split_whitespace();
        let key = it.next();
        if key == Some("newmtl") {
            out.push(Material {
                name: it.next().unwrap_or("").to_string(),
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
                let path = it.last().ok_or_else(|| format!("line {}: map_Kd has no path", lineno + 1))?;
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
```

- [ ] **Step 7: Move the legacy encoder onto the IR**

In `crates/bevy_nds_3d_obj/src/lib.rs`:

1. Add after `use std::fmt::Write as _;`:

```rust
use ir::{SourceModel, Triangle};
```

2. Replace the body of `obj_to_display_list` with:

```rust
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
```

3. In `obj_preview_mesh`, replace `let tris = parse_obj(source)?;` with `let tris = legacy_triangles(source)?;` and replace `let pos = [t.verts[0].0, t.verts[1].0, t.verts[2].0];` with `let pos = [t[0].pos, t[1].pos, t[2].pos];`.

4. Delete the private `struct Tri`, `fn parse_obj`, `fn resolve`, `fn parse_vec3` and `fn parse_face_vertex` from `lib.rs` (they now live in `obj.rs`).

5. Replace `apply_origin` with:

```rust
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
```

6. In `display_list`, change the signature to `fn display_list(tris: &[Triangle], compress: bool) -> (Vec<u32>, [[f32; 3]; 2])` and its vertex loop to:

```rust
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
```

7. Make the two helpers public (model2dsm needs `flat_normal`): change `fn flat_normal(` to `pub fn flat_normal(` and `fn normalize(` to `pub fn normalize(`, and give each a one-line doc comment if it lacks one (`/// Geometric (flat) normal of a triangle, normalised; zero if degenerate.` already exists on `flat_normal`; add `/// Normalise a vector; zero stays zero.` on `normalize`).

8. Update the two existing tests that called the old private parser. In `single_triangle_display_list_layout` replace
`let tris = parse_obj("v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1\n").unwrap();` with
`let tris = legacy_triangles("v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1\n").unwrap();`
and in `quads_are_fan_triangulated` replace `parse_obj(` with `legacy_triangles(`.

- [ ] **Step 8: Run the crate's tests**

Run: `nix develop -c cargo test -p bevy_nds_3d_obj --target x86_64-unknown-linux-gnu`
Expected: all pass, including `legacy_encoder_is_byte_identical_on_committed_assets` and the six new `obj::tests`.

- [ ] **Step 9: Check the other consumers still compile**

Run: `nix develop -c cargo test -p obj2dl -p bevy_nds_3d_macros --target x86_64-unknown-linux-gnu && nix develop -c just check-editor`
Expected: both succeed (the editor uses `obj_preview_mesh` / `obj_to_display_list`, whose signatures are unchanged).

- [ ] **Step 10: Commit**

```bash
git add crates/bevy_nds_3d_obj
git commit -m "feat(models): shared source-model IR + OBJ/MTL front-end (#66)

Legacy .dl / include_obj! output pinned byte-identical by golden hashes.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Per-material textured display-list encoder

**Files:**
- Modify: `crates/bevy_nds_3d_obj/src/lib.rs`

**Interfaces:**
- Consumes: `ir::Triangle` (Task 1).
- Produces: `bevy_nds_3d_obj::submesh_display_list(tris: &[Triangle], tex_size: Option<[u16; 2]>) -> Result<Vec<u32>, String>` (leading body-length word included, exactly like the legacy list) and `bevy_nds_3d_obj::VERTEX_LIMIT: f32`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/bevy_nds_3d_obj/src/lib.rs`:

```rust
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
        let src = std::fs::read_to_string(format!("{}/../../assets/cube.obj", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let tris = legacy_triangles(&src).unwrap();
        assert_eq!(submesh_display_list(&tris, None).unwrap(), display_list(&tris, false).0);
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
```

- [ ] **Step 2: Run them to verify they fail**

Run: `nix develop -c cargo test -p bevy_nds_3d_obj --target x86_64-unknown-linux-gnu`
Expected: compile error — `cannot find function submesh_display_list` / `FIFO_TEX_COORD`.

- [ ] **Step 3: Implement**

In `crates/bevy_nds_3d_obj/src/lib.rs`, add the command ID next to the other FIFO constants:

```rust
const FIFO_TEX_COORD: u8 = 0x22; // GFX_TEX_COORD 0x04000488 — 1 argument
```

and add after `display_list`:

```rust
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
pub fn submesh_display_list(tris: &[Triangle], tex_size: Option<[u16; 2]>) -> Result<Vec<u32>, String> {
    let mut ops: Vec<(u8, Vec<u32>)> = Vec::with_capacity(tris.len() * 9 + 2);
    ops.push((FIFO_BEGIN, vec![GL_TRIANGLES]));
    for (ti, tri) in tris.iter().enumerate() {
        for v in tri {
            check_range(v.pos).map_err(|e| format!("triangle {ti}: {e}"))?;
            if let Some([w, h]) = tex_size {
                let uv = v
                    .uv
                    .ok_or_else(|| format!("triangle {ti}: textured material but a vertex has no UV"))?;
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
```

- [ ] **Step 4: Run the tests**

Run: `nix develop -c cargo test -p bevy_nds_3d_obj --target x86_64-unknown-linux-gnu`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/bevy_nds_3d_obj
git commit -m "feat(models): per-material textured display-list encoder (#66)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `model2dsm` crate + PNG → DS texture encoder

**Files:**
- Create: `crates/model2dsm/Cargo.toml`, `crates/model2dsm/src/lib.rs`, `crates/model2dsm/src/texture.rs`
- Modify: `Cargo.toml` (workspace `members`), `Justfile` (`test` group 1)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `model2dsm::texture::{TexFormat::{Pal4 = 2, Pal16 = 3, Pal256 = 4}, TexFormat::capacity(self) -> usize, TexFormat::bits_per_texel(self) -> usize, SIZES: [u32; 6], DsTexture { width: u16, height: u16, format: TexFormat, transparent0: bool, palette: Vec<u16>, texels: Vec<u8> }, DsTexture::texel_bytes(&self) -> u32, DsTexture::palette_bytes(&self) -> u32, DsTexture::to_le_bytes(&self) -> Vec<u8>, TEX_MAGIC: u32, rgb15(r, g, b: u8) -> u16, encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<DsTexture, String>, decode_png_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String>, encode_png(bytes: &[u8]) -> Result<DsTexture, String>}`.

- [ ] **Step 1: Scaffold the crate**

Create `crates/model2dsm/Cargo.toml`:

```toml
[package]
name = "model2dsm"
version = "0.1.0"
edition = "2024"
description = "Bakes OBJ+MTL and glTF models (with PNG textures) into DS model (.dsm) and texture (.tex) assets for NitroFS (#66)."

# Host-side tool, never shipped in the ROM. Shares the geometry encoder in
# `bevy_nds_3d_obj` (one source of truth for display-list packing) and adds the
# glTF front-end, the PNG -> DS texture encoder and the .dsm container.

[lib]
path = "src/lib.rs"

[dependencies]
bevy_nds_3d_obj = { path = "../bevy_nds_3d_obj" }
png = "0.18"
# Static-mesh reading only: no `import` (it drags in the `image` crate) — the
# front-end resolves buffers and PNG bytes itself.
gltf = { version = "1.4", default-features = false, features = ["utils", "names"] }
```

Create `crates/model2dsm/src/lib.rs`:

```rust
//! `model2dsm` — bake OBJ+MTL and glTF models (with PNG textures) into DS
//! assets for NitroFS (#66).

pub mod texture;
```

In the root `Cargo.toml` `[workspace] members`, add `"crates/model2dsm",` after `"crates/obj2dl",`.

In the `Justfile` `test` recipe's first `cargo test` line, add `-p model2dsm` after `-p obj2dl`:

```
    cargo test -p bevy_nds_3d_obj -p obj2dl -p model2dsm -p bevy_nds_3d_macros -p png2sprite -p png2bg -p perfread \
```

- [ ] **Step 2: Write the failing tests**

Create `crates/model2dsm/src/texture.rs` with only the tests:

```rust
//! PNG → DS paletted texture (#66).

#[cfg(test)]
mod tests {
    use super::*;

    /// RGBA for a `w`×`h` image filled by `f(x, y)`.
    fn img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| f(x, y)).collect()
    }

    /// `n` distinct opaque colours spread over an 8×8 (or 16×16 for n > 64) image.
    fn colours(n: u32) -> (u32, Vec<u8>) {
        let side = if n > 64 { 16 } else { 8 };
        let rgba = img(side, side, |x, y| {
            let i = (y * side + x) % n;
            [((i & 31) << 3) as u8, (((i >> 5) & 31) << 3) as u8, 0, 255]
        });
        (side, rgba)
    }

    fn png(w: u32, h: u32, color: ::png::ColorType, data: &[u8], palette: Option<&[u8]>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = ::png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(::png::BitDepth::Eight);
            if let Some(p) = palette {
                enc.set_palette(p.to_vec());
            }
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(data).unwrap();
            wr.finish().unwrap();
        }
        out
    }

    #[test]
    fn rgb15_takes_top_five_bits() {
        assert_eq!(rgb15(255, 0, 0), 0x001F);
        assert_eq!(rgb15(0, 255, 0), 0x03E0);
        assert_eq!(rgb15(0, 0, 255), 0x7C00);
        assert_eq!(rgb15(7, 7, 7), 0);
    }

    #[test]
    fn format_follows_colour_count() {
        let fmt = |n| {
            let (s, rgba) = colours(n);
            encode_rgba(s, s, &rgba).unwrap().format
        };
        assert_eq!(fmt(2), TexFormat::Pal4);
        assert_eq!(fmt(4), TexFormat::Pal4);
        assert_eq!(fmt(5), TexFormat::Pal16);
        assert_eq!(fmt(16), TexFormat::Pal16);
        assert_eq!(fmt(17), TexFormat::Pal256);
        assert_eq!(fmt(256), TexFormat::Pal256);
    }

    #[test]
    fn transparency_takes_palette_slot_zero() {
        // Four opaque colours + transparent = 5 entries → no longer fits 2bpp.
        let rgba = img(8, 8, |x, _| match x {
            0 => [0, 0, 0, 0],
            n => [(n as u8 % 4) * 64, 0, 0, 255],
        });
        let t = encode_rgba(8, 8, &rgba).unwrap();
        assert!(t.transparent0);
        assert_eq!(t.palette[0], 0);
        assert_eq!(t.format, TexFormat::Pal16);
        // Pixel (0,0) is transparent → index 0 in the low nibble of byte 0.
        assert_eq!(t.texels[0] & 0x0F, 0);
    }

    #[test]
    fn too_many_colours_is_an_error() {
        // 32×16 with a distinct 15-bit colour per pixel: 512 entries.
        let rgba = img(32, 16, |x, y| [(x * 8) as u8, (y * 16) as u8, 0, 255]);
        let err = encode_rgba(32, 16, &rgba).unwrap_err();
        assert!(err.contains("512") && err.contains("256"), "{err}");
    }

    #[test]
    fn colours_merge_after_15_bit_conversion() {
        let rgba = img(8, 8, |x, _| if x % 2 == 0 { [0, 0, 0, 255] } else { [7, 7, 7, 255] });
        let t = encode_rgba(8, 8, &rgba).unwrap();
        assert_eq!(t.palette, vec![0]);
        assert_eq!(t.format, TexFormat::Pal4);
    }

    #[test]
    fn texels_pack_low_bits_first() {
        // Row 0: A, B, A, A, ... → palette [A, B]; 2bpp byte 0 = 0 | 1<<2 = 0x04.
        let rgba = img(8, 8, |x, y| if (x, y) == (1, 0) { [0, 0, 255, 255] } else { [255, 0, 0, 255] });
        let t = encode_rgba(8, 8, &rgba).unwrap();
        assert_eq!(t.format, TexFormat::Pal4);
        assert_eq!(t.palette, vec![0x001F, 0x7C00]);
        assert_eq!(t.texels.len(), 16);
        assert_eq!(t.texels[0], 0x04);
        assert!(t.texels[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn rejects_bad_sizes() {
        for (w, h) in [(12, 8), (512, 8), (4, 8), (8, 0)] {
            let rgba = vec![255u8; (w * h * 4) as usize];
            let err = encode_rgba(w, h, &rgba).unwrap_err();
            assert!(err.contains("power of two"), "{w}x{h}: {err}");
        }
    }

    #[test]
    fn png_colour_types_decode_alike() {
        let rgba = img(8, 8, |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        let want = encode_rgba(8, 8, &rgba).unwrap();

        let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        assert_eq!(encode_png(&png(8, 8, ::png::ColorType::Rgb, &rgb, None)).unwrap(), want);
        assert_eq!(encode_png(&png(8, 8, ::png::ColorType::Rgba, &rgba, None)).unwrap(), want);

        let idx: Vec<u8> = rgba.chunks(4).map(|p| if p[0] == 255 { 0 } else { 1 }).collect();
        let pal = [255, 0, 0, 0, 0, 255];
        assert_eq!(encode_png(&png(8, 8, ::png::ColorType::Indexed, &idx, Some(&pal))).unwrap(), want);

        let grey = img(8, 8, |x, _| if x < 4 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let g: Vec<u8> = grey.chunks(4).map(|p| p[0]).collect();
        assert_eq!(
            encode_png(&png(8, 8, ::png::ColorType::Grayscale, &g, None)).unwrap(),
            encode_rgba(8, 8, &grey).unwrap()
        );
    }

    #[test]
    fn blob_layout() {
        let t = encode_rgba(8, 8, &img(8, 8, |_, _| [255, 0, 0, 255])).unwrap();
        let b = t.to_le_bytes();
        assert_eq!(&b[0..4], b"DST1");
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 8);
        assert_eq!(u16::from_le_bytes([b[6], b[7]]), 8);
        assert_eq!(b[8], TexFormat::Pal4 as u8);
        assert_eq!(b[9], 0); // no transparency
        assert_eq!(u16::from_le_bytes([b[10], b[11]]), 1); // one palette entry
        assert_eq!(u32::from_le_bytes([b[12], b[13], b[14], b[15]]), 16);
        assert_eq!(&b[16..20], &[0x1F, 0x00, 0, 0]); // palette, padded to 4
        assert_eq!(b.len(), 20 + 16);
        assert_eq!(t.palette_bytes(), 16); // 2 bytes rounded up to 16
        assert_eq!(t.texel_bytes(), 16);
    }
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu`
Expected: compile errors — `rgb15`, `encode_rgba`, `TexFormat` … not found.

- [ ] **Step 4: Implement**

Insert above the test module in `crates/model2dsm/src/texture.rs`:

```rust
//! Pure Rust, no external tool: the DS texture layout is linear (row-major), so
//! there is nothing for `grit` to tile, and owning the encoder keeps the
//! colour-count rule (authoring contract v1) host-tested.

use std::collections::HashMap;

/// The DS texture formats this pipeline emits. The discriminant is the
/// `TEXIMAGE_PARAM` format code (bits 26–28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TexFormat {
    Pal4 = 2,
    Pal16 = 3,
    Pal256 = 4,
}

impl TexFormat {
    pub fn capacity(self) -> usize {
        match self {
            Self::Pal4 => 4,
            Self::Pal16 => 16,
            Self::Pal256 => 256,
        }
    }

    pub fn bits_per_texel(self) -> usize {
        match self {
            Self::Pal4 => 2,
            Self::Pal16 => 4,
            Self::Pal256 => 8,
        }
    }
}

/// Allowed side lengths (authoring contract v1, #66).
pub const SIZES: [u32; 6] = [8, 16, 32, 64, 128, 256];

/// Magic identifying a `.tex` blob: ASCII `"DST1"`.
pub const TEX_MAGIC: u32 = u32::from_le_bytes(*b"DST1");

/// A baked DS paletted texture.
#[derive(Clone, Debug, PartialEq)]
pub struct DsTexture {
    pub width: u16,
    pub height: u16,
    pub format: TexFormat,
    /// Palette entry 0 is transparent (the source had alpha-0 pixels).
    pub transparent0: bool,
    /// RGB15 entries actually used, ≤ the format's capacity.
    pub palette: Vec<u16>,
    /// Row-major texel indices, first pixel in the lowest bits of each byte.
    pub texels: Vec<u8>,
}

impl DsTexture {
    pub fn texel_bytes(&self) -> u32 {
        self.texels.len() as u32
    }

    /// Palette VRAM cost, rounded up to the 16-byte palette alignment.
    pub fn palette_bytes(&self) -> u32 {
        (self.palette.len() as u32 * 2 + 15) & !15
    }

    /// Serialise to the runtime NitroFS `.tex` format. All little-endian:
    ///
    /// | offset | type      | field                                        |
    /// |--------|-----------|----------------------------------------------|
    /// | 0      | `u32`     | magic [`TEX_MAGIC`]                          |
    /// | 4      | `u16`     | width                                        |
    /// | 6      | `u16`     | height                                       |
    /// | 8      | `u8`      | format ([`TexFormat`] / `TEXIMAGE_PARAM` code) |
    /// | 9      | `u8`      | flags (bit 0: palette entry 0 transparent)   |
    /// | 10     | `u16`     | palette entry count P                        |
    /// | 12     | `u32`     | texel byte count T                           |
    /// | 16     | `u16` × P | palette (RGB15), zero-padded to 4 bytes      |
    /// | …      | `u8` × T  | texels                                       |
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.palette.len() * 2 + 2 + self.texels.len());
        out.extend_from_slice(&TEX_MAGIC.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(self.format as u8);
        out.push(self.transparent0 as u8);
        out.extend_from_slice(&(self.palette.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.texels.len() as u32).to_le_bytes());
        for c in &self.palette {
            out.extend_from_slice(&c.to_le_bytes());
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(&self.texels);
        out
    }
}

/// RGB888 → DS RGB15 (`r | g << 5 | b << 10`, the top five bits of each).
pub fn rgb15(r: u8, g: u8, b: u8) -> u16 {
    (r as u16 >> 3) | ((g as u16 >> 3) << 5) | ((b as u16 >> 3) << 10)
}

/// Encode RGBA8 pixels (row-major) as the smallest paletted format that holds
/// them. Alpha-0 pixels become palette index 0; any other alpha is opaque.
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<DsTexture, String> {
    if !SIZES.contains(&width) || !SIZES.contains(&height) {
        return Err(format!(
            "texture is {width}×{height}; each side must be a power of two from 8 to 256 (authoring contract v1, #66)"
        ));
    }
    let n = (width * height) as usize;
    if rgba.len() != n * 4 {
        return Err(format!("expected {} RGBA bytes, got {}", n * 4, rgba.len()));
    }

    let transparent0 = rgba.chunks_exact(4).any(|p| p[3] == 0);
    let mut palette: Vec<u16> = Vec::new();
    if transparent0 {
        palette.push(0);
    }
    let mut lookup: HashMap<u16, usize> = HashMap::new();
    for p in rgba.chunks_exact(4).filter(|p| p[3] != 0) {
        let c = rgb15(p[0], p[1], p[2]);
        lookup.entry(c).or_insert_with(|| {
            palette.push(c);
            palette.len() - 1
        });
    }

    let format = [TexFormat::Pal4, TexFormat::Pal16, TexFormat::Pal256]
        .into_iter()
        .find(|f| palette.len() <= f.capacity())
        .ok_or_else(|| {
            format!(
                "texture uses {} palette entries after 15-bit conversion{}; the most a DS texture holds is 256 (authoring contract v1, #66)",
                palette.len(),
                if transparent0 { " (incl. one for transparency)" } else { "" }
            )
        })?;

    let indices: Vec<u8> = rgba
        .chunks_exact(4)
        .map(|p| if p[3] == 0 { 0 } else { lookup[&rgb15(p[0], p[1], p[2])] as u8 })
        .collect();

    let bits = format.bits_per_texel();
    let per = 8 / bits;
    let mut texels = vec![0u8; n / per];
    for (i, &ix) in indices.iter().enumerate() {
        texels[i / per] |= ix << ((i % per) * bits);
    }

    Ok(DsTexture {
        width: width as u16,
        height: height as u16,
        format,
        transparent0,
        palette,
        texels,
    })
}

/// Decode any 8- or 16-bit PNG (indexed, grey, RGB, with or without alpha) to
/// RGBA8.
pub fn decode_png_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| format!("not a readable PNG: {e}"))?;
    let size = reader.output_buffer_size().ok_or("PNG is too large to decode")?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("PNG decode failed: {e}"))?;
    let n = (info.width * info.height) as usize;
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..n * 4].to_vec(),
        png::ColorType::Rgb => buf[..n * 3].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..n * 2].chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf[..n].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("PNG is still indexed after expansion".into()),
    };
    Ok((info.width, info.height, rgba))
}

/// Decode a PNG and encode it as a DS texture.
pub fn encode_png(bytes: &[u8]) -> Result<DsTexture, String> {
    let (w, h, rgba) = decode_png_rgba(bytes)?;
    encode_rgba(w, h, &rgba)
}
```

- [ ] **Step 5: Run the tests**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu`
Expected: 9 passed. (If `set_palette` is reported with a different signature by `png` 0.18, adapt only the test helper; the encoder doesn't use it.)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock Justfile crates/model2dsm
git commit -m "feat(models): model2dsm crate + PNG -> DS paletted texture encoder (#66)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: glTF / GLB front-end

**Files:**
- Create: `crates/model2dsm/src/gltf_src.rs`
- Modify: `crates/model2dsm/src/lib.rs` (add `pub mod gltf_src;`)

**Interfaces:**
- Consumes: `bevy_nds_3d_obj::ir::*`, `bevy_nds_3d_obj::flat_normal`, `bevy_nds_3d_obj::obj::unit_to_u8` (Task 1); `texture` module is not used here.
- Produces: `model2dsm::gltf_src::{load_gltf(path: &Path) -> Result<SourceModel, String>, parse_gltf(bytes: &[u8], read_uri: &dyn Fn(&str) -> Result<Vec<u8>, String>) -> Result<SourceModel, String>}`. Sub-meshes are grouped by glTF material index in first-use order; embedded PNGs land in `SourceModel::images` as `TextureSrc::Embedded(i)`, external images as `TextureSrc::File(uri)`.

- [ ] **Step 1: Write the failing tests**

Create `crates/model2dsm/src/gltf_src.rs` with only the tests, and add `pub mod gltf_src;` to `crates/model2dsm/src/lib.rs`:

```rust
//! glTF 2.0 (`.gltf` + `.bin`, or `.glb`) front-end → [`SourceModel`] (#66).

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
        let m = parse_gltf(&one_triangle_glb(r#""translation": [1.0, 0.0, 0.0]"#, true, FAKE_PNG), &no_files).unwrap();
        assert_eq!(m.submeshes.len(), 1);
        let s = &m.submeshes[0];
        assert_eq!(s.material.name, "crate");
        assert_eq!(s.material.diffuse, [255, 128, 0]);
        assert_eq!(s.material.texture, Some(TextureSrc::Embedded(0)));
        assert_eq!(
            s.material.wrap,
            Wrap { repeat_s: true, repeat_t: false, flip_s: true, flip_t: false }
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
        let m = parse_gltf(&one_triangle_glb(r#""scale": [-1.0, 1.0, 1.0]"#, true, FAKE_PNG), &no_files).unwrap();
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
        let m = parse_gltf(&one_triangle_glb(r#""name": "n""#, false, FAKE_PNG), &no_files).unwrap();
        assert!(m.submeshes[0].tris[0].iter().all(|v| v.normal == [0.0, 0.0, 1.0]));
    }

    #[test]
    fn data_uris_are_rejected_with_export_advice() {
        let json = r#"{"asset": {"version": "2.0"},
            "buffers": [{"byteLength": 4, "uri": "data:application/octet-stream;base64,AAAAAA=="}]}"#;
        let err = parse_gltf(json.as_bytes(), &no_files).unwrap_err();
        assert!(err.contains(".glb"), "{err}");
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu gltf`
Expected: compile error — `parse_gltf` not found.

- [ ] **Step 3: Implement**

Insert above the test module in `crates/model2dsm/src/gltf_src.rs`:

```rust
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
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
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
            gltf::buffer::Source::Uri(uri) if uri.starts_with("data:") => return Err(DATA_URI_ERR.into()),
            gltf::buffer::Source::Uri(uri) => read_uri(uri)?,
        });
    }

    let mut images: Vec<Vec<u8>> = Vec::new();
    let mut image_src: Vec<TextureSrc> = Vec::new();
    for img in document.images() {
        image_src.push(match img.source() {
            gltf::image::Source::View { view, mime_type } => {
                if mime_type != "image/png" {
                    return Err(format!("image {} is {mime_type}; only PNG textures are supported", img.index()));
                }
                let data = buffers.get(view.buffer().index()).ok_or("image buffer missing")?;
                let bytes = data
                    .get(view.offset()..view.offset() + view.length())
                    .ok_or("image buffer view out of range")?;
                images.push(bytes.to_vec());
                TextureSrc::Embedded(images.len() - 1)
            }
            gltf::image::Source::Uri { uri, .. } if uri.starts_with("data:") => return Err(DATA_URI_ERR.into()),
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
            return Err(format!("node {} has a degenerate (zero-scale) transform", node.index()));
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
            let uv: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|i| i.into_f32().collect());
            let idx: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..pos.len() as u32).collect(),
            };
            if idx.len() % 3 != 0 {
                return Err(format!("mesh {}: index count {} isn't a multiple of 3", mesh.index(), idx.len()));
            }

            let key = prim.material().index();
            let at = match groups.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    let material = material(&prim.material(), image_src)?;
                    groups.push((key, SubMesh { material, tris: Vec::new() }));
                    groups.len() - 1
                }
            };

            let corner = |i: u32| -> Result<Vertex, String> {
                let i = i as usize;
                let p = *pos.get(i).ok_or_else(|| format!("mesh {}: index {i} out of range", mesh.index()))?;
                Ok(Vertex {
                    pos: transform_point(&world, p),
                    normal: nor.as_ref().and_then(|n| n.get(i)).map(|n| apply3(&normal_m, *n)).unwrap_or([0.0; 3]),
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
        out.wrap = Wrap { repeat_s, repeat_t, flip_s, flip_t };
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
```

- [ ] **Step 4: Run the tests**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu`
Expected: all pass (texture + 4 glTF tests). If `gltf` rejects the fixture JSON at validation, the error text names the field — fix the fixture, not the front-end.

- [ ] **Step 5: Commit**

```bash
git add crates/model2dsm
git commit -m "feat(models): glTF/GLB front-end with baked node transforms (#66)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `.dsm` container, directory baker and CLI

**Files:**
- Create: `crates/model2dsm/src/dsm.rs`, `crates/model2dsm/src/main.rs`
- Modify: `crates/model2dsm/src/lib.rs`, `crates/model2dsm/Cargo.toml` (add the `[[bin]]`)

**Interfaces:**
- Consumes: `submesh_display_list` (Task 2), `texture::{encode_png, DsTexture}` (Task 3), `gltf_src::load_gltf` (Task 4), `obj::parse_obj` (Task 1).
- Produces:
  - `model2dsm::dsm::{DSM_MAGIC: u32, NO_TEXTURE: u16, TexRef<'a> { nitro_path: &'a str, width: u16, height: u16 }, encode(model: &SourceModel, table: &[TexRef], sub_tex: &[Option<usize>]) -> Result<Vec<u8>, String>, ambient(diffuse: [u8;3]) -> [u8;3], wrap_bits(Wrap) -> u8}`.
  - `model2dsm::{MODELS_SUBDIR = "models", MODEL_EXT = "dsm", TEX_EXT = "tex", MODEL_EXTS = ["obj","gltf","glb"], TRI_WARN = 500, TEXTURE_VRAM_BYTES = 262_144, PALETTE_VRAM_BYTES = 16_384}`.
  - `model2dsm::{load_model(&Path) -> Result<SourceModel, String>, walk_models(root: &Path) -> Result<Vec<PathBuf>, String>, mesh_name(rel: &Path) -> String, model_names(root: &Path) -> Result<Vec<(String, PathBuf)>, String>, ResolvedTexture { key: String, source: PathBuf, png: Vec<u8> }, resolve_textures(root: &Path, rel: &Path, model: &SourceModel) -> Result<Vec<Option<ResolvedTexture>>, String>, tex_out_rel(key: &str) -> String, nitro_tex_path(key: &str) -> String, build_dir(root: &Path, dst: &Path) -> Result<Built, String>}` with `Built { models: Vec<BuiltModel>, textures: Vec<BuiltTexture>, warnings: Vec<String> }`, `BuiltModel { name, input, output, tris, textures: Vec<String> }`, `BuiltTexture { key, input, output, texel_bytes, palette_bytes }`.
  - The `.dsm` layout (Plan B's runtime reader must match it — documented on `dsm::encode`).

- [ ] **Step 1: Write the failing tests**

Create `crates/model2dsm/src/dsm.rs` with only the tests:

```rust
//! The `.dsm` model container (#66).

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
```

Append to `crates/model2dsm/src/lib.rs` (module declarations go at the top, next to the existing ones) — **tests only for now**:

```rust
pub mod dsm;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("model2dsm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// An 8×8 two-colour PNG.
    fn png_bytes() -> Vec<u8> {
        let rgba: Vec<u8> = (0..64).flat_map(|i| if i % 2 == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 8, 8);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&rgba).unwrap();
            wr.finish().unwrap();
        }
        out
    }

    /// A textured quad `<dir>/<stem>.obj` using `map_Kd <texture>`.
    fn write_quad(dir: &Path, stem: &str, texture: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(format!("{stem}.obj")),
            format!(
                "mtllib {stem}.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\n\
                 vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 1\n\
                 usemtl skin\nf 1/1/1 2/2/1 3/3/1 4/4/1\n"
            ),
        )
        .unwrap();
        std::fs::write(dir.join(format!("{stem}.mtl")), format!("newmtl skin\nKd 1 1 1\nmap_Kd {texture}\n")).unwrap();
    }

    #[test]
    fn mesh_names_are_slash_paths_without_extension() {
        assert_eq!(mesh_name(Path::new("props/crate.glb")), "props/crate");
        assert_eq!(mesh_name(Path::new("tree.obj")), "tree");
    }

    #[test]
    fn tex_out_paths() {
        assert_eq!(tex_out_rel("props/crate.png"), "props/crate.tex");
        assert_eq!(tex_out_rel("props/barrel.glb#0"), "props/barrel.glb.0.tex");
        assert_eq!(nitro_tex_path("props/crate.png"), "nitro:/models/props/crate.tex");
    }

    #[test]
    fn bakes_a_textured_obj() {
        let root = temp_dir("bake");
        let src = root.join("src");
        write_quad(&src.join("props"), "crate", "crate.png");
        std::fs::write(src.join("props/crate.png"), png_bytes()).unwrap();
        let dst = root.join("out");

        let built = build_dir(&src, &dst).unwrap();
        assert_eq!(built.models.len(), 1);
        assert_eq!(built.models[0].name, "props/crate");
        assert_eq!(built.models[0].tris, 2);
        assert_eq!(built.textures.len(), 1);
        assert_eq!(built.textures[0].key, "props/crate.png");
        assert_eq!(built.textures[0].texel_bytes, 16); // 8×8 at 2bpp
        let dsm = std::fs::read(dst.join("props/crate.dsm")).unwrap();
        assert_eq!(&dsm[0..4], b"DSM1");
        assert!(dsm.windows(29).any(|w| w == b"nitro:/models/props/crate.tex"));
        let tex = std::fs::read(dst.join("props/crate.tex")).unwrap();
        assert_eq!(&tex[0..4], b"DST1");
        assert!(built.warnings.is_empty(), "{:?}", built.warnings);
    }

    #[test]
    fn shared_texture_bakes_once() {
        let root = temp_dir("shared");
        let src = root.join("src");
        write_quad(&src, "a", "shared/skin.png");
        write_quad(&src, "b", "shared/skin.png");
        std::fs::create_dir_all(src.join("shared")).unwrap();
        std::fs::write(src.join("shared/skin.png"), png_bytes()).unwrap();

        let built = build_dir(&src, &root.join("out")).unwrap();
        assert_eq!(built.models.len(), 2);
        assert_eq!(built.textures.len(), 1);
        assert_eq!(built.models[0].textures, vec!["shared/skin.png".to_string()]);
        assert_eq!(built.models[1].textures, vec!["shared/skin.png".to_string()]);
    }

    #[test]
    fn duplicate_model_names_are_an_error() {
        let root = temp_dir("dup");
        write_quad(&root, "crate", "crate.png");
        std::fs::write(root.join("crate.glb"), b"not read").unwrap();
        let err = model_names(&root).unwrap_err();
        assert!(err.contains("crate.obj") && err.contains("crate.glb"), "{err}");
    }

    #[test]
    fn texture_outside_models_root_is_an_error() {
        let root = temp_dir("escape");
        write_quad(&root, "crate", "../../elsewhere.png");
        let err = build_dir(&root, &root.join("out")).unwrap_err();
        assert!(err.contains("skin") && err.contains("outside"), "{err}");
    }

    #[test]
    fn missing_texture_file_is_an_error() {
        let root = temp_dir("missing");
        write_quad(&root, "crate", "nope.png");
        let err = build_dir(&root, &root.join("out")).unwrap_err();
        assert!(err.contains("skin") && err.contains("nope.png"), "{err}");
    }

    #[test]
    fn heavy_model_warns() {
        let root = temp_dir("heavy");
        let mut obj = String::from("v 0 0 0\nv 1 0 0\nv 0 1 0\n");
        for _ in 0..501 {
            obj.push_str("f 1 2 3\n");
        }
        std::fs::write(root.join("heavy.obj"), obj).unwrap();
        let built = build_dir(&root, &root.join("out")).unwrap();
        assert_eq!(built.warnings.len(), 1);
        assert!(built.warnings[0].contains("501 triangles"), "{}", built.warnings[0]);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu`
Expected: compile errors — `encode`, `TexRef`, `build_dir`, `mesh_name` … not found.

- [ ] **Step 3: Implement the container**

Insert above the test module in `crates/model2dsm/src/dsm.rs`:

```rust
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
```

- [ ] **Step 4: Implement the baker**

Replace the non-test part of `crates/model2dsm/src/lib.rs` (keep the `#[cfg(test)] mod tests` from Step 1 at the bottom) with:

```rust
//! `model2dsm` — bake OBJ+MTL and glTF models (with PNG textures) into DS
//! assets for NitroFS (#66).
//!
//! `assets/models/**/<name>.{obj,gltf,glb}` → `build/nitrofs/models/**/<name>.dsm`
//! (sub-meshes + material table, see [`dsm::encode`]) and each texture →
//! `build/nitrofs/models/**/<name>.tex` ([`texture::DsTexture::to_le_bytes`]).
//! A model keeps its authored origin (no recentring). Usable from `build.rs`
//! ([`build_dir`]) or as the `model2dsm` CLI.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use bevy_nds_3d_obj::ir::{SourceModel, TextureSrc};
use bevy_nds_3d_obj::obj::{ObjOptions, parse_obj};

pub mod dsm;
pub mod gltf_src;
pub mod texture;

/// Subdirectory of both `assets/` and `build/nitrofs/` holding models.
pub const MODELS_SUBDIR: &str = "models";
pub const MODEL_EXT: &str = "dsm";
pub const TEX_EXT: &str = "tex";
/// Source extensions the pipeline reads.
pub const MODEL_EXTS: [&str; 3] = ["obj", "gltf", "glb"];
/// Bake-warning threshold per model (authoring contract v1, #66).
pub const TRI_WARN: usize = 500;
/// A level's texture budget: VRAM banks B + D (#66).
pub const TEXTURE_VRAM_BYTES: u32 = 256 * 1024;
/// A level's texture-palette budget: VRAM bank F (#66).
pub const PALETTE_VRAM_BYTES: u32 = 16 * 1024;

/// Load any supported model file into the shared representation.
pub fn load_model(path: &Path) -> Result<SourceModel, String> {
    let ctx = |e: String| format!("{}: {e}", path.display());
    match path.extension().and_then(|e| e.to_str()) {
        Some("obj") => {
            let src = std::fs::read_to_string(path).map_err(|e| ctx(e.to_string()))?;
            let dir = path.parent().unwrap_or(Path::new("."));
            parse_obj(&src, ObjOptions { materials: true }, |name| {
                std::fs::read_to_string(dir.join(name)).map_err(|e| format!("could not read mtllib {name}: {e}"))
            })
            .map_err(ctx)
        }
        Some("gltf" | "glb") => gltf_src::load_gltf(path).map_err(ctx),
        _ => Err(ctx("not a model file (expected .obj, .gltf or .glb)".into())),
    }
}

/// Every model file under `root` (recursively), relative to it, sorted. A
/// missing `root` is simply empty.
pub fn walk_models(root: &Path) -> Result<Vec<PathBuf>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| MODEL_EXTS.contains(&e)) {
                out.push(path.strip_prefix(root).map_err(|e| e.to_string())?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// `/`-joined, platform-independent form of a relative path.
fn slash(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The mesh name an instance uses for model `rel` (relative to
/// `assets/models/`): the path without its extension — `props/crate.glb` →
/// `props/crate`.
pub fn mesh_name(rel: &Path) -> String {
    slash(&rel.with_extension(""))
}

/// `(mesh name, relative path)` for every model under `root`, sorted by name.
/// Two files with one name (`crate.obj` + `crate.glb`) are an error — an
/// instance couldn't say which it means.
pub fn model_names(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    for rel in walk_models(root)? {
        let name = mesh_name(&rel);
        if let Some(prev) = seen.get(&name) {
            return Err(format!("mesh name `{name}` is defined twice: {} and {}", slash(prev), slash(&rel)));
        }
        seen.insert(name, rel);
    }
    Ok(seen.into_iter().collect())
}

/// One sub-mesh's texture, located: a stable `key` (so two models sharing an
/// image share one upload and one budget entry), the file the bytes came from,
/// and the PNG bytes.
#[derive(Clone, Debug)]
pub struct ResolvedTexture {
    pub key: String,
    pub source: PathBuf,
    pub png: Vec<u8>,
}

/// Resolve each sub-mesh's texture for model `rel` under `root` (one entry per
/// sub-mesh, `None` when untextured). File textures are keyed by their path
/// relative to `root`; embedded ones by `"<model rel path>#<image index>"`.
pub fn resolve_textures(root: &Path, rel: &Path, model: &SourceModel) -> Result<Vec<Option<ResolvedTexture>>, String> {
    model
        .submeshes
        .iter()
        .map(|s| match &s.material.texture {
            None => Ok(None),
            Some(TextureSrc::File(uri)) => {
                let joined = rel.parent().unwrap_or(Path::new("")).join(uri);
                let key_rel = normalize_rel(&joined).ok_or_else(|| {
                    format!(
                        "material `{}`: texture `{uri}` points outside {}",
                        s.material.name,
                        root.display()
                    )
                })?;
                let source = root.join(&key_rel);
                let png = std::fs::read(&source).map_err(|e| {
                    format!("material `{}`: could not read texture {}: {e}", s.material.name, source.display())
                })?;
                Ok(Some(ResolvedTexture { key: slash(&key_rel), source, png }))
            }
            Some(TextureSrc::Embedded(i)) => {
                let png = model
                    .images
                    .get(*i)
                    .ok_or_else(|| format!("material `{}`: embedded image {i} is missing", s.material.name))?
                    .clone();
                Ok(Some(ResolvedTexture { key: format!("{}#{i}", slash(rel)), source: root.join(rel), png }))
            }
        })
        .collect()
}

/// Lexically resolve `.` / `..` in a relative path; `None` if it escapes.
fn normalize_rel(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// The `.tex` path (relative to the models output root) a texture key bakes to:
/// `props/crate.png` → `props/crate.tex`; `props/barrel.glb#0` →
/// `props/barrel.glb.0.tex`.
pub fn tex_out_rel(key: &str) -> String {
    match key.split_once('#') {
        Some((model, i)) => format!("{model}.{i}.{TEX_EXT}"),
        None => slash(&Path::new(key).with_extension(TEX_EXT)),
    }
}

/// The NitroFS path a texture key is loaded from at runtime.
pub fn nitro_tex_path(key: &str) -> String {
    format!("nitro:/{MODELS_SUBDIR}/{}", tex_out_rel(key))
}

#[derive(Clone, Debug)]
pub struct BuiltModel {
    pub name: String,
    pub input: PathBuf,
    pub output: PathBuf,
    pub tris: usize,
    /// Texture keys this model references, in its table order.
    pub textures: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct BuiltTexture {
    pub key: String,
    pub input: PathBuf,
    pub output: PathBuf,
    pub texel_bytes: u32,
    pub palette_bytes: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Built {
    pub models: Vec<BuiltModel>,
    pub textures: Vec<BuiltTexture>,
    pub warnings: Vec<String>,
}

/// Bake every model under `root` into `<dst>/<rel>.dsm`, and every texture they
/// use (once each) into `<dst>/<tex_out_rel(key)>`.
pub fn build_dir(root: &Path, dst: &Path) -> Result<Built, String> {
    let mut built = Built::default();
    let mut baked: BTreeMap<String, texture::DsTexture> = BTreeMap::new();
    for (name, rel) in model_names(root)? {
        let input = root.join(&rel);
        let model = load_model(&input)?;
        let tris = model.triangle_count();
        if tris == 0 {
            return Err(format!("{}: no triangles", input.display()));
        }
        if tris > TRI_WARN {
            built.warnings.push(format!(
                "{}: {tris} triangles — over the {TRI_WARN}-per-model guide (the DS draws ~2048 per frame for everything on screen)",
                input.display()
            ));
        }

        let resolved = resolve_textures(root, &rel, &model).map_err(|e| format!("{}: {e}", input.display()))?;
        let mut table_keys: Vec<String> = Vec::new();
        let mut sub_tex: Vec<Option<usize>> = Vec::new();
        for r in &resolved {
            let Some(r) = r else {
                sub_tex.push(None);
                continue;
            };
            if !baked.contains_key(&r.key) {
                let tex = texture::encode_png(&r.png).map_err(|e| format!("{}: {e}", r.source.display()))?;
                let output = dst.join(tex_out_rel(&r.key));
                write_file(&output, &tex.to_le_bytes())?;
                built.textures.push(BuiltTexture {
                    key: r.key.clone(),
                    input: r.source.clone(),
                    output,
                    texel_bytes: tex.texel_bytes(),
                    palette_bytes: tex.palette_bytes(),
                });
                baked.insert(r.key.clone(), tex);
            }
            let slot = match table_keys.iter().position(|k| *k == r.key) {
                Some(i) => i,
                None => {
                    table_keys.push(r.key.clone());
                    table_keys.len() - 1
                }
            };
            sub_tex.push(Some(slot));
        }

        let paths: Vec<String> = table_keys.iter().map(|k| nitro_tex_path(k)).collect();
        let table: Vec<dsm::TexRef> = table_keys
            .iter()
            .zip(&paths)
            .map(|(k, p)| dsm::TexRef { nitro_path: p.as_str(), width: baked[k].width, height: baked[k].height })
            .collect();
        let bytes = dsm::encode(&model, &table, &sub_tex).map_err(|e| format!("{}: {e}", input.display()))?;
        let output = dst.join(rel.with_extension(MODEL_EXT));
        write_file(&output, &bytes)?;
        built.models.push(BuiltModel { name, input, output, tris, textures: table_keys });
    }
    Ok(built)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
}
```

- [ ] **Step 5: Add the CLI**

Append to `crates/model2dsm/Cargo.toml` after the `[lib]` table:

```toml
[[bin]]
name = "model2dsm"
path = "src/main.rs"
```

Create `crates/model2dsm/src/main.rs`:

```rust
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
```

- [ ] **Step 6: Run the tests**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu`
Expected: all pass (texture, glTF, the 2 `dsm` tests and the 8 `lib` tests).

- [ ] **Step 7: Commit**

```bash
git add crates/model2dsm
git commit -m "feat(models): .dsm container, directory baker and model2dsm CLI (#66)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Mesh-name catalogue

**Files:**
- Create: `crates/model2dsm/src/catalog.rs`
- Modify: `crates/model2dsm/src/lib.rs` (add `pub mod catalog;`)

**Interfaces:**
- Consumes: `load_model`, `model_names`, `walk_models`, `mesh_name`, `resolve_textures`, `texture::encode_png`, `MODELS_SUBDIR` (Task 5).
- Produces: `model2dsm::catalog::{TexUse { key: String, texel_bytes: u32, palette_bytes: u32 }, MeshEntry { source: PathBuf, textures: Vec<TexUse> }, Catalog { meshes: BTreeMap<String, MeshEntry> }, Catalog::scan(assets_dir: &Path) -> Result<Catalog, String>, Catalog::contains(&self, &str) -> bool, Catalog::textures(&self, &str) -> &[TexUse], mesh_names(assets_dir: &Path) -> Vec<String>}`.

- [ ] **Step 1: Write the failing tests**

Create `crates/model2dsm/src/catalog.rs` with only the tests, and add `pub mod catalog;` to `lib.rs`:

```rust
//! Mesh-name catalogue for level validation (#66).

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("model2dsm-cat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png_8x8_two_colours() -> Vec<u8> {
        let rgba: Vec<u8> = (0..64).flat_map(|i| if i % 2 == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 8, 8);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&rgba).unwrap();
            wr.finish().unwrap();
        }
        out
    }

    const TRI: &str = "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n";

    #[test]
    fn scans_legacy_and_textured_models() {
        let assets = temp_dir("scan");
        std::fs::write(assets.join("cube.obj"), TRI).unwrap();
        let props = assets.join("models/props");
        std::fs::create_dir_all(&props).unwrap();
        std::fs::write(
            props.join("crate.obj"),
            "mtllib crate.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl skin\nf 1/1 2/1 3/1\n",
        )
        .unwrap();
        std::fs::write(props.join("crate.mtl"), "newmtl skin\nmap_Kd crate.png\n").unwrap();
        std::fs::write(props.join("crate.png"), png_8x8_two_colours()).unwrap();

        let cat = Catalog::scan(&assets).unwrap();
        assert!(cat.contains("cube"));
        assert!(cat.contains("props/crate"));
        assert!(cat.textures("cube").is_empty());
        assert_eq!(
            cat.textures("props/crate"),
            &[TexUse { key: "props/crate.png".into(), texel_bytes: 16, palette_bytes: 16 }]
        );
        assert_eq!(mesh_names(&assets), vec!["cube".to_string(), "props/crate".to_string()]);
    }

    #[test]
    fn legacy_and_model_name_clash_is_an_error() {
        let assets = temp_dir("clash");
        std::fs::write(assets.join("crate.obj"), TRI).unwrap();
        std::fs::create_dir_all(assets.join("models")).unwrap();
        std::fs::write(assets.join("models/crate.glb"), b"not read").unwrap();
        let err = Catalog::scan(&assets).unwrap_err();
        assert!(err.contains("crate.obj") && err.contains("crate.glb"), "{err}");
    }

    #[test]
    fn missing_assets_dir_is_empty() {
        let cat = Catalog::scan(Path::new("/nonexistent/kts-assets")).unwrap();
        assert!(cat.meshes.is_empty());
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu catalog`
Expected: compile errors — `Catalog`, `TexUse`, `mesh_names` not found.

- [ ] **Step 3: Implement**

Insert above the test module in `crates/model2dsm/src/catalog.rs`:

```rust
//! Which mesh names exist, and what each would put in texture VRAM. Consumed by
//! `scene2bin`'s level validation (the per-level texture budget) and, through
//! the cheap [`mesh_names`], by the editor's mesh picker.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{MODELS_SUBDIR, load_model, mesh_name, model_names, resolve_textures, texture, walk_models};

/// One texture's VRAM cost, keyed so an image shared by two models counts once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TexUse {
    pub key: String,
    pub texel_bytes: u32,
    pub palette_bytes: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshEntry {
    pub source: PathBuf,
    pub textures: Vec<TexUse>,
}

/// Every mesh name an instance may reference.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub meshes: BTreeMap<String, MeshEntry>,
}

/// Legacy top-level `assets/*.obj` (untextured `.dl` path): `(stem, path)`.
fn legacy_objs(assets_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(assets_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("obj"))
        .filter_map(|p| Some((p.file_stem()?.to_str()?.to_string(), p.clone())))
        .collect();
    out.sort();
    out
}

impl Catalog {
    /// Scan legacy `assets/*.obj` and every model under `assets/models/**`,
    /// measuring each model's textures. A name defined twice is an error.
    pub fn scan(assets_dir: &Path) -> Result<Catalog, String> {
        let mut meshes: BTreeMap<String, MeshEntry> = BTreeMap::new();
        for (name, source) in legacy_objs(assets_dir) {
            meshes.insert(name, MeshEntry { source, textures: Vec::new() });
        }

        let root = assets_dir.join(MODELS_SUBDIR);
        let mut sizes: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        for (name, rel) in model_names(&root)? {
            let source = root.join(&rel);
            if let Some(prev) = meshes.get(&name) {
                return Err(format!(
                    "mesh name `{name}` is defined twice: {} and {}",
                    prev.source.display(),
                    source.display()
                ));
            }
            let model = load_model(&source)?;
            let mut textures: Vec<TexUse> = Vec::new();
            let resolved = resolve_textures(&root, &rel, &model).map_err(|e| format!("{}: {e}", source.display()))?;
            for r in resolved.into_iter().flatten() {
                if textures.iter().any(|t| t.key == r.key) {
                    continue;
                }
                let (texel_bytes, palette_bytes) = match sizes.get(&r.key) {
                    Some(&s) => s,
                    None => {
                        let t = texture::encode_png(&r.png).map_err(|e| format!("{}: {e}", r.source.display()))?;
                        let s = (t.texel_bytes(), t.palette_bytes());
                        sizes.insert(r.key.clone(), s);
                        s
                    }
                };
                textures.push(TexUse { key: r.key, texel_bytes, palette_bytes });
            }
            meshes.insert(name, MeshEntry { source, textures });
        }
        Ok(Catalog { meshes })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.meshes.contains_key(name)
    }

    /// The textures mesh `name` uses (empty when untextured or unknown).
    pub fn textures(&self, name: &str) -> &[TexUse] {
        self.meshes.get(name).map(|m| m.textures.as_slice()).unwrap_or(&[])
    }
}

/// Every mesh name under `assets_dir` without loading any model — cheap enough
/// for the editor's picker and its continuous validation. Unreadable
/// directories and duplicates are skipped here; [`Catalog::scan`] (the bake)
/// reports them.
pub fn mesh_names(assets_dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = legacy_objs(assets_dir).into_iter().map(|(n, _)| n).collect();
    if let Ok(rels) = walk_models(&assets_dir.join(MODELS_SUBDIR)) {
        out.extend(rels.iter().map(|r| mesh_name(r)));
    }
    out.sort();
    out.dedup();
    out
}
```

- [ ] **Step 4: Run the tests**

Run: `nix develop -c cargo test -p model2dsm --target x86_64-unknown-linux-gnu`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/model2dsm
git commit -m "feat(models): mesh-name catalogue with per-mesh texture costs (#66)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Level texture budget in `scene2bin` + editor mesh list

**Files:**
- Modify: `crates/scene2bin/Cargo.toml`, `crates/scene2bin/src/lib.rs`, `tools/scene-editor/src/app.rs`

**Interfaces:**
- Consumes: `model2dsm::catalog::{Catalog, MeshEntry, TexUse, mesh_names}`, `model2dsm::{TEXTURE_VRAM_BYTES, PALETTE_VRAM_BYTES}` (Tasks 5–6).
- Produces: `scene2bin::{Catalog, MeshEntry, TexUse}` (re-exports), `scene2bin::validate_all_with_catalog(level: &Level, zones: &[(String, Space)], catalog: &Catalog) -> Vec<Issue>`, `scene2bin::mesh_names(assets_dir: &Path) -> Vec<String>`. `validate_all(level, zones, mesh_exists: impl Fn(&str) -> bool)` keeps its signature (existence only, no texture rule). `build_levels_dir` / `validate_levels_dir` keep their signatures and now use the catalog.

- [ ] **Step 1: Add the dependency**

In `crates/scene2bin/Cargo.toml` `[dependencies]`, add:

```toml
# Mesh-name catalogue + texture costs for the per-level texture budget (#66);
# its `mesh_names` also feeds the detached editor's mesh picker through us.
model2dsm = { path = "../model2dsm" }
```

- [ ] **Step 2: Write the failing tests**

In `crates/scene2bin/src/lib.rs`, inside the existing `#[cfg(test)] mod tests` (next to the other `validate_all` tests), add:

```rust
    fn prop(mesh: &str) -> Instance {
        let mut i = inst("prop");
        i.mesh = Some(mesh.to_string());
        i
    }

    /// A catalogue where each listed mesh uses the given textures.
    fn catalog(entries: &[(&str, Vec<TexUse>)]) -> Catalog {
        Catalog {
            meshes: entries
                .iter()
                .map(|(n, t)| {
                    (
                        n.to_string(),
                        MeshEntry { source: std::path::PathBuf::from(format!("{n}.obj")), textures: t.clone() },
                    )
                })
                .collect(),
        }
    }

    fn tex(key: &str, kb: u32) -> TexUse {
        TexUse { key: key.to_string(), texel_bytes: kb * 1024, palette_bytes: 32 }
    }

    fn texture_errors(issues: &[Issue]) -> Vec<&Issue> {
        issues
            .iter()
            .filter(|i| i.severity == Severity::Error && (i.msg.contains("texture") || i.msg.contains("palette")))
            .collect()
    }

    #[test]
    fn texture_budget_counts_shared_textures_once() {
        let cat = catalog(&[("a", vec![tex("shared.png", 200)]), ("b", vec![tex("shared.png", 200)])]);
        let (level, zones) =
            one_zone_level("atrium", with_instances(std::vec![inst("avatar"), prop("a"), prop("b")]));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        assert!(texture_errors(&issues).is_empty(), "{issues:#?}");
    }

    #[test]
    fn texture_budget_over_256_kb_is_a_level_error() {
        let cat = catalog(&[("a", vec![tex("a.png", 200)]), ("b", vec![tex("b.png", 100)])]);
        let (level, zones) =
            one_zone_level("atrium", with_instances(std::vec![inst("avatar"), prop("a"), prop("b")]));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        let errs = texture_errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert_eq!(errs[0].zone, None);
        assert!(errs[0].msg.contains("300.0 KB"), "{}", errs[0].msg);
        assert!(errs[0].msg.contains("a.png (200.0 KB)"), "{}", errs[0].msg);
    }

    #[test]
    fn palette_budget_over_16_kb_is_a_level_error() {
        let entries: Vec<(String, Vec<TexUse>)> = (0..33)
            .map(|i| {
                (
                    format!("m{i}"),
                    vec![TexUse { key: format!("t{i}.png"), texel_bytes: 64, palette_bytes: 512 }],
                )
            })
            .collect();
        let refs: Vec<(&str, Vec<TexUse>)> = entries.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
        let cat = catalog(&refs);
        let mut instances = std::vec![inst("avatar")];
        instances.extend(entries.iter().map(|(n, _)| prop(n)));
        let (level, zones) = one_zone_level("atrium", with_instances(instances));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        let errs = texture_errors(&issues);
        assert_eq!(errs.len(), 1, "{issues:#?}");
        assert!(errs[0].msg.contains("palette"), "{}", errs[0].msg);
    }

    #[test]
    fn catalog_validation_reports_unknown_meshes() {
        let cat = catalog(&[]);
        let (level, zones) = one_zone_level("atrium", with_instances(std::vec![inst("avatar"), prop("ghost")]));
        let issues = validate_all_with_catalog(&level, &zones, &cat);
        assert!(errors(&issues).iter().any(|i| i.msg.contains("ghost.obj")), "{issues:#?}");
    }
```

- [ ] **Step 3: Run them to verify they fail**

Run: `nix develop -c just test-crate scene2bin budget`
Expected: compile errors — `Catalog`, `MeshEntry`, `TexUse`, `validate_all_with_catalog` not found.

- [ ] **Step 4: Implement**

In `crates/scene2bin/src/lib.rs`:

1. Next to the existing imports near the top, add:

```rust
pub use model2dsm::catalog::{Catalog, MeshEntry, TexUse};

/// Every mesh name under `assets_dir` (legacy `assets/*.obj` + `assets/models/**`)
/// without loading any model — for the editor's mesh picker (#66).
pub fn mesh_names(assets_dir: &Path) -> Vec<String> {
    model2dsm::catalog::mesh_names(assets_dir)
}
```

2. Just above `pub fn validate_all`, add the lookup seam:

```rust
/// What the validator needs to know about mesh names. Private: callers pick an
/// entry point — [`validate_all`] (existence only) or
/// [`validate_all_with_catalog`] (existence + texture costs).
trait MeshLookup {
    fn exists(&self, mesh: &str) -> bool;
    fn textures(&self, mesh: &str) -> &[TexUse];
}

/// An existence-only lookup over a plain predicate (the editor's and the tests' form).
struct ExistsOnly<F>(F);

impl<F: Fn(&str) -> bool> MeshLookup for ExistsOnly<F> {
    fn exists(&self, mesh: &str) -> bool {
        (self.0)(mesh)
    }
    fn textures(&self, _mesh: &str) -> &[TexUse] {
        &[]
    }
}

impl MeshLookup for Catalog {
    fn exists(&self, mesh: &str) -> bool {
        self.contains(mesh)
    }
    fn textures(&self, mesh: &str) -> &[TexUse] {
        Catalog::textures(self, mesh)
    }
}
```

3. Replace the body of `validate_all` and add the catalog entry point below it:

```rust
pub fn validate_all(
    level: &Level,
    zones: &[(String, Space)],
    mesh_exists: impl Fn(&str) -> bool,
) -> Vec<Issue> {
    validate_with(level, zones, &ExistsOnly(mesh_exists))
}

/// [`validate_all`] plus the rules that need to know what meshes *contain* — the
/// per-level texture budget (#66). The bake and `--check` use this.
pub fn validate_all_with_catalog(level: &Level, zones: &[(String, Space)], catalog: &Catalog) -> Vec<Issue> {
    validate_with(level, zones, catalog)
}

fn validate_with(level: &Level, zones: &[(String, Space)], meshes: &impl MeshLookup) -> Vec<Issue> {
    let mut out = Vec::new();
    for (stem, space) in zones {
        validate_zone(stem, space, meshes, &mut out);
    }
    validate_level(level, zones, &mut out);
    validate_textures(zones, meshes, &mut out);
    out
}
```

Update the `validate_all` doc comment's last paragraph to: `` `mesh_exists` reports whether a referenced mesh exists (existence only — the texture budget needs [`validate_all_with_catalog`]). ``

4. Change `validate_zone`'s parameter `mesh_exists: &impl Fn(&str) -> bool` to `meshes: &impl MeshLookup`, and its mesh rule to:

```rust
        if let Some(mesh) = &inst.mesh {
            if !meshes.exists(mesh) {
                out.push(Issue::error(
                    stem,
                    Some(i),
                    format!(
                        "(role `{}`): mesh `{mesh}` has no source model — expected `assets/{mesh}.obj` or `assets/models/{mesh}.obj` / `.gltf` / `.glb`",
                        inst.role
                    ),
                ));
            }
        }
```

5. In `validate` (the single-zone wrapper), change `validate_zone("", space, &mesh_exists, &mut issues);` to `validate_zone("", space, &ExistsOnly(mesh_exists), &mut issues);`.

6. Add the budget rule after `validate_level`:

```rust
/// Level-scope texture budget (#66): a level's whole texture set is resident
/// from boot, so every distinct texture any instance's mesh uses must fit,
/// together, in the texture VRAM (banks B + D) and the palette VRAM (bank F).
fn validate_textures(zones: &[(String, Space)], meshes: &impl MeshLookup, out: &mut Vec<Issue>) {
    let mut seen: std::collections::BTreeMap<&str, &TexUse> = std::collections::BTreeMap::new();
    for (_, space) in zones {
        for inst in &space.instances {
            if let Some(m) = &inst.mesh {
                for t in meshes.textures(m) {
                    seen.entry(t.key.as_str()).or_insert(t);
                }
            }
        }
    }
    let kb = |b: u32| format!("{:.1} KB", b as f32 / 1024.0);
    let texels: u32 = seen.values().map(|t| t.texel_bytes).sum();
    let palettes: u32 = seen.values().map(|t| t.palette_bytes).sum();
    if texels > model2dsm::TEXTURE_VRAM_BYTES {
        let mut largest: Vec<&TexUse> = seen.values().copied().collect();
        largest.sort_by(|a, b| b.texel_bytes.cmp(&a.texel_bytes).then(a.key.cmp(&b.key)));
        let top: Vec<String> = largest.iter().take(3).map(|t| format!("{} ({})", t.key, kb(t.texel_bytes))).collect();
        out.push(Issue {
            zone: None,
            instance: None,
            severity: Severity::Error,
            msg: format!(
                "textures need {} of texture VRAM; a level has {} (banks B + D, #66). Largest: {}",
                kb(texels),
                kb(model2dsm::TEXTURE_VRAM_BYTES),
                top.join(", ")
            ),
        });
    }
    if palettes > model2dsm::PALETTE_VRAM_BYTES {
        out.push(Issue {
            zone: None,
            instance: None,
            severity: Severity::Error,
            msg: format!(
                "texture palettes need {} of palette VRAM; a level has {} (bank F, #66) — use fewer colours per texture",
                kb(palettes),
                kb(model2dsm::PALETTE_VRAM_BYTES)
            ),
        });
    }
}
```

7. In `validate_levels_dir`, replace the `mesh_exists` closure and the loop body with:

```rust
    let catalog = Catalog::scan(assets_dir)?;
    let mut out = Vec::new();
    for lv in load_levels_dir(levels_root, prefab_dir)? {
        out.extend(validate_all_with_catalog(&lv.level, &lv.zones, &catalog));
    }
    Ok(out)
```

8. In `build_levels_dir`, replace `let mesh_exists = |name: &str| assets_dir.join(format!("{name}.obj")).is_file();` with `let catalog = Catalog::scan(assets_dir)?;` and `let issues = validate_all(&level, &zones, mesh_exists);` with `let issues = validate_all_with_catalog(&level, &zones, &catalog);`. Update both functions' doc comments: "`assets_dir` is the geometry root; its legacy `*.obj` and `models/**` are scanned into a [`Catalog`] for mesh existence and the texture budget."

- [ ] **Step 5: Run the scene2bin tests**

Run: `nix develop -c just test-crate scene2bin`
Expected: all pass (the existing 25+ plus the 4 new ones; `validate_all_reports_multiple_issues_where_validate_stops_at_first` still finds `ghost.obj` in the new message).

- [ ] **Step 6: Point the editor's mesh list at the new names**

In `tools/scene-editor/src/app.rs`, replace (line ~214)

```rust
        self.meshes = stems(&self.assets_dir, "obj");
```

with

```rust
        // Legacy `assets/*.obj` + every `assets/models/**` model (#66).
        self.meshes = scene2bin::mesh_names(std::path::Path::new(&self.assets_dir));
```

If `stems` is now unused in `app.rs`, drop it from that file's `use` list (it stays defined in `widgets.rs`, which still uses it).

- [ ] **Step 7: Check the editor**

Run: `nix develop -c just check-editor && nix develop -c just test-editor`
Expected: both succeed with no new warnings.

- [ ] **Step 8: Commit**

```bash
git add Cargo.lock crates/scene2bin tools/scene-editor
git commit -m "feat(scene): per-level texture budget via the model catalogue (#66)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Wire the bake into `build.rs`, document, verify end to end

**Files:**
- Modify: `build.rs`, `Cargo.toml` (`[build-dependencies]`), `CLAUDE.md`

**Interfaces:**
- Consumes: `model2dsm::{build_dir, MODELS_SUBDIR}` (Task 5).
- Produces: `build/nitrofs/models/**.{dsm,tex}` on every build (packed into the ROM by the existing `just rom` `ndstool -d build/nitrofs`).

- [ ] **Step 1: Add the build step**

In the root `Cargo.toml` `[build-dependencies]`, after `obj2dl`:

```toml
# Bakes `assets/models/**` (OBJ+MTL, glTF) into `build/nitrofs/models/**.dsm`
# plus their PNG textures as `.tex` (#66); loaded at runtime by bevy_nds_3d (Plan B).
model2dsm = { path = "crates/model2dsm" }
```

In `build.rs`, call it from `main` right after `compile_assets();`:

```rust
    compile_models();
```

and add, after `compile_assets`:

```rust
/// Bake every model under `assets/models/**` (OBJ + MTL, glTF / GLB) into
/// `build/nitrofs/models/**.dsm`, plus each PNG texture they use into `.tex`,
/// via the `model2dsm` library (#66). Pure Rust (no external tool), so it runs
/// identically inside or outside `nix develop`. Unlike `compile_assets`, models
/// keep their authored origin (no recentring).
fn compile_models() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join(ASSET_DIR).join(model2dsm::MODELS_SUBDIR);
    let dst = manifest.join(NITROFS_DIR).join(model2dsm::MODELS_SUBDIR);

    println!("cargo:rerun-if-changed={}", src.display());
    if !src.is_dir() {
        return;
    }
    match model2dsm::build_dir(&src, &dst) {
        Ok(built) => {
            for w in &built.warnings {
                println!("cargo:warning={w}");
            }
        }
        Err(e) => println!(
            "cargo:warning=model baking FAILED — build/nitrofs/models holds STALE or MISSING blobs: {e}"
        ),
    }
}
```

Also extend the module doc list at the top of `build.rs`: after item 1 add

```rust
//! 1b. **Compile textured models (#66).** Bakes `assets/models/**`
//!    (OBJ + MTL, glTF) into `build/nitrofs/models/**.dsm` + `.tex` via
//!    `model2dsm`.
```

- [ ] **Step 2: Smoke-test the real build path**

```bash
mkdir -p assets/models/_smoke
printf 'mtllib quad.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 1\nusemtl skin\nf 1/1/1 2/2/1 3/3/1 4/4/1\n' > assets/models/_smoke/quad.obj
printf 'newmtl skin\nKd 1 1 1\nmap_Kd blip.png\n' > assets/models/_smoke/quad.mtl
cp assets/sprites/blip.png assets/models/_smoke/blip.png
nix develop -c just build 2>&1 | grep -E 'model baking|level baking|^error' ; ls -l build/nitrofs/models/_smoke/
nix develop -c just check-levels
rm -rf assets/models/_smoke build/nitrofs/models
```

Expected: `build/nitrofs/models/_smoke/quad.dsm` and `blip.tex` exist; no "model baking FAILED" line; `just check-levels` exits 0. (`blip.png` is a 16×16 indexed sprite PNG — it also exercises the indexed-PNG path.) The cleanup removes the scratch model so nothing unreferenced lands in the ROM.

- [ ] **Step 3: Document**

In `CLAUDE.md`:

1. In "Testing", group 1 list: change ``1. `bevy_nds_3d_obj`, `obj2dl`, `bevy_nds_3d_macros` — pure host crates`` to ``1. `bevy_nds_3d_obj`, `obj2dl`, `model2dsm`, `bevy_nds_3d_macros` — pure host crates``.
2. Replace the `bevy_nds_3d_obj` bullet under "Capability crates" with:

```markdown
- **`crates/bevy_nds_3d_obj`** — host-side model core: the shared **source-model
  IR** (`ir`: sub-meshes per material, UVs top-left, raw normals), the OBJ + MTL
  front-end (`obj`), and the display-list encoders — legacy untextured
  (`obj_to_display_list`, byte-identical, golden-hash tested) and per-material
  textured (`submesh_display_list`, texcoords in texels). The single source of
  truth for geometry packing.
```

3. Add after the `obj2dl` bullet:

```markdown
- **`crates/model2dsm`** — host CLI + library (#66): bakes `assets/models/**`
  (`.obj` + `.mtl`, `.gltf` + `.bin`, `.glb`) into `build/nitrofs/models/**.dsm`
  (sub-meshes + material table, `"DSM1"`) and each PNG texture into `.tex`
  (`"DST1"`, paletted 2/4/8 bpp picked from the colour count). Pure Rust — the
  glTF front-end (node transforms baked in, mirrored nodes re-wound), the PNG
  encoder and the container all host-tested. Models keep their authored origin.
  Its `Catalog` (mesh names → texture costs) backs `scene2bin`'s per-level
  texture budget (256 KB texture + 16 KB palette VRAM). Writer of both formats;
  `bevy_nds_3d` will be the reader (Plan B) — keep them in sync.
```

4. In "Asset pipeline", append:

```markdown
**Textured models (#66).** Blender exports go under `assets/models/**` — OBJ +
MTL (`map_Kd` texture) or glTF / GLB — with PNG textures next to them. Mesh
names are the path under `assets/models/` without extension
(`props/crate.glb` → `props/crate`). `build.rs` → `model2dsm` writes
`build/nitrofs/models/**.dsm` + `.tex`; `scene2bin` errors when a level's
distinct textures exceed 256 KB (or palettes 16 KB). Authoring contract v1
(scale, axes, ±8 range, 500-triangle guide, 8–256 px power-of-two textures) is
Locked on #66. The DS runtime that draws them is the next plan.
```

- [ ] **Step 4: Full verification**

Run each; all must succeed:

```bash
nix develop -c just test
nix develop -c just check-levels
nix develop -c just check
nix develop -c just check-editor
nix develop -c just test-editor
nix develop -c cargo fmt --check
```

Expected: every test green (the first `just test` run builds `std` for group 2 and is slow); `check-levels` exit 0; `check` compiles the DS target with the new build-dependency; `fmt --check` clean (run `nix develop -c just fmt` and amend if not).

- [ ] **Step 5: Commit**

```bash
git add build.rs Cargo.toml Cargo.lock CLAUDE.md
git commit -m "build: bake assets/models into NitroFS via model2dsm (#66)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## After this plan

- **Plan B — DS runtime** (written once this lands, against the real `.dsm` / `.tex` layouts): map banks B + D as texture slots 0–1 and bank F as texture palette; a `.dsm` / `.tex` reader in `bevy_nds_3d` (pure parser host-tested); upload a level's textures at boot (per-level residency); per sub-mesh bind `TEXIMAGE_PARAM` / `PLTT_BASE` + material (instance `DsMaterial` tints) and `glCallList`; `bevy_nds_scene` resolves `props/crate` → `nitro:/models/props/crate.dsm` (legacy names → `.dl`); a proof model **exported by you from Blender** in both formats placed in `facility`; `preview-rom` + `asset-audit`; the artist guide for authoring contract v1; `design-sync` the shipped state to #66.
- **Plan C — Editor**: textured viewport preview and glTF thumbnails, and the editor's problems panel on `validate_all_with_catalog`.
