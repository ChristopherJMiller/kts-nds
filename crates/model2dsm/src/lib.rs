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

pub mod catalog;
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
                std::fs::read_to_string(dir.join(name))
                    .map_err(|e| format!("could not read mtllib {name}: {e}"))
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
        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| MODEL_EXTS.contains(&e))
            {
                out.push(
                    path.strip_prefix(root)
                        .map_err(|e| e.to_string())?
                        .to_path_buf(),
                );
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
            return Err(format!(
                "mesh name `{name}` is defined twice: {} and {}",
                slash(prev),
                slash(&rel)
            ));
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

/// Resolve each sub-mesh's texture for model `rel` under `root` (one entry
/// per sub-mesh, `None` when untextured, and also `None` for a sub-mesh with
/// no triangles — a declared-but-unused material shouldn't bake or budget
/// its texture, #66). File textures are keyed by their path relative to
/// `root`; embedded ones by the content hash of their PNG bytes
/// (`_embedded/<16-hex FNV-1a-64>`), so two models embedding byte-identical
/// images share one bake and one budget entry — the key is model-independent.
pub fn resolve_textures(
    root: &Path,
    rel: &Path,
    model: &SourceModel,
) -> Result<Vec<Option<ResolvedTexture>>, String> {
    model
        .submeshes
        .iter()
        .map(|s| {
            if s.tris.is_empty() {
                return Ok(None);
            }
            match &s.material.texture {
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
                        format!(
                            "material `{}`: could not read texture {}: {e}",
                            s.material.name,
                            source.display()
                        )
                    })?;
                    Ok(Some(ResolvedTexture {
                        key: slash(&key_rel),
                        source,
                        png,
                    }))
                }
                Some(TextureSrc::Embedded(i)) => {
                    let png = model
                        .images
                        .get(*i)
                        .ok_or_else(|| {
                            format!(
                                "material `{}`: embedded image {i} is missing",
                                s.material.name
                            )
                        })?
                        .clone();
                    let key = format!("_embedded/{:016x}", fnv1a64(&png));
                    Ok(Some(ResolvedTexture {
                        key,
                        source: root.join(rel),
                        png,
                    }))
                }
            }
        })
        .collect()
}

/// FNV-1a, 64-bit (offset basis `0xcbf29ce484222325`, prime
/// `0x100000001b3`): a fast, dependency-free content hash used to key
/// embedded images by their bytes rather than by which model embedded them
/// (#66). Not cryptographic — collision resistance isn't the point, content
/// addressing is.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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

/// The `.tex` path (relative to the models output root) a texture key bakes
/// to: `props/crate.png` → `props/crate.tex`; an embedded image's
/// content-hash key `_embedded/<hash>` → `_embedded/<hash>.tex`.
pub fn tex_out_rel(key: &str) -> String {
    slash(&Path::new(key).with_extension(TEX_EXT))
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

/// One model, fully baked (geometry *and* texture-encoded) but not yet written
/// to disk — the shared work between [`build_dir`] (which writes the files)
/// and [`catalog::Catalog::scan`] (which discards the bytes but still forces
/// every error `dsm::encode` can raise — a vertex outside the ±8 model-space
/// range, a texcoord beyond ±2048 texels, a textured sub-mesh with no UVs —
/// through the same gate as duplicate names and texture-path checks, #66).
pub struct ModelBake {
    pub tris: usize,
    /// The encoded `.dsm` container bytes.
    pub dsm_bytes: Vec<u8>,
    /// Texture keys this model references, in its table order.
    pub textures: Vec<String>,
    /// `(key, source)` for each texture newly added to `cache` by this call —
    /// i.e. not already baked by an earlier model sharing the image.
    pub new_textures: Vec<(String, PathBuf)>,
}

/// Load, validate and encode one model (relative to `root`) into its `.dsm`
/// bytes, encoding (and caching) each of its textures along the way. `cache`
/// is shared across a whole directory bake so two models referencing one
/// image encode it once. This is the single place that calls `dsm::encode`
/// (and so the single place a geometry error — zero triangles, an
/// out-of-range vertex/texcoord, a textured material with no UVs — can
/// surface) so `build_dir` and `Catalog::scan` fail on exactly the same
/// models.
pub fn bake_model(
    root: &Path,
    rel: &Path,
    cache: &mut BTreeMap<String, texture::DsTexture>,
) -> Result<ModelBake, String> {
    let input = root.join(rel);
    let model = load_model(&input)?;
    let tris = model.triangle_count();
    if tris == 0 {
        return Err(format!("{}: no triangles", input.display()));
    }

    let resolved =
        resolve_textures(root, rel, &model).map_err(|e| format!("{}: {e}", input.display()))?;
    let mut table_keys: Vec<String> = Vec::new();
    let mut sub_tex: Vec<Option<usize>> = Vec::new();
    let mut new_textures: Vec<(String, PathBuf)> = Vec::new();
    for r in &resolved {
        let Some(r) = r else {
            sub_tex.push(None);
            continue;
        };
        if !cache.contains_key(&r.key) {
            let tex =
                texture::encode_png(&r.png).map_err(|e| format!("{}: {e}", r.source.display()))?;
            cache.insert(r.key.clone(), tex);
            new_textures.push((r.key.clone(), r.source.clone()));
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
        .map(|(k, p)| dsm::TexRef {
            nitro_path: p.as_str(),
            width: cache[k].width,
            height: cache[k].height,
        })
        .collect();
    let dsm_bytes =
        dsm::encode(&model, &table, &sub_tex).map_err(|e| format!("{}: {e}", input.display()))?;

    Ok(ModelBake {
        tris,
        dsm_bytes,
        textures: table_keys,
        new_textures,
    })
}

/// Bake every model under `root` into `<dst>/<rel>.dsm`, and every texture they
/// use (once each) into `<dst>/<tex_out_rel(key)>`.
pub fn build_dir(root: &Path, dst: &Path) -> Result<Built, String> {
    let mut built = Built::default();
    let mut cache: BTreeMap<String, texture::DsTexture> = BTreeMap::new();
    for (name, rel) in model_names(root)? {
        let input = root.join(&rel);
        let bake = bake_model(root, &rel, &mut cache)?;
        if bake.tris > TRI_WARN {
            built.warnings.push(format!(
                "{}: {} triangles — over the {TRI_WARN}-per-model guide (the DS draws ~2048 per frame for everything on screen)",
                input.display(),
                bake.tris
            ));
        }

        for (key, source) in &bake.new_textures {
            let tex = &cache[key];
            let output = dst.join(tex_out_rel(key));
            write_file(&output, &tex.to_le_bytes())?;
            built.textures.push(BuiltTexture {
                key: key.clone(),
                input: source.clone(),
                output,
                texel_bytes: tex.texel_bytes(),
                palette_bytes: tex.palette_bytes(),
            });
        }

        let output = dst.join(rel.with_extension(MODEL_EXT));
        write_file(&output, &bake.dsm_bytes)?;
        built.models.push(BuiltModel {
            name,
            input,
            output,
            tris: bake.tris,
            textures: bake.textures,
        });
    }
    Ok(built)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
}

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
        let rgba: Vec<u8> = (0..64)
            .flat_map(|i| {
                if i % 2 == 0 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
            .collect();
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
        std::fs::write(
            dir.join(format!("{stem}.mtl")),
            format!("newmtl skin\nKd 1 1 1\nmap_Kd {texture}\n"),
        )
        .unwrap();
    }

    #[test]
    fn mesh_names_are_slash_paths_without_extension() {
        assert_eq!(mesh_name(Path::new("props/crate.glb")), "props/crate");
        assert_eq!(mesh_name(Path::new("tree.obj")), "tree");
    }

    #[test]
    fn tex_out_paths() {
        assert_eq!(tex_out_rel("props/crate.png"), "props/crate.tex");
        assert_eq!(
            tex_out_rel("_embedded/0123456789abcdef"),
            "_embedded/0123456789abcdef.tex"
        );
        assert_eq!(
            nitro_tex_path("props/crate.png"),
            "nitro:/models/props/crate.tex"
        );
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
        assert!(
            dsm.windows(29)
                .any(|w| w == b"nitro:/models/props/crate.tex")
        );
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
        assert_eq!(
            built.models[0].textures,
            vec!["shared/skin.png".to_string()]
        );
        assert_eq!(
            built.models[1].textures,
            vec!["shared/skin.png".to_string()]
        );
    }

    #[test]
    fn identical_embedded_images_across_glb_models_bake_once() {
        // Two separate .glb models, each embedding byte-identical PNG bytes:
        // the content-hash key must make them share one bake and one budget
        // entry, exactly like two OBJs sharing a file texture (#66).
        use crate::gltf_src::fixtures::one_triangle_glb;
        let root = temp_dir("embedded-shared");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let glb_bytes = one_triangle_glb(r#""name": "n""#, true, &png_bytes());
        std::fs::write(src.join("a.glb"), &glb_bytes).unwrap();
        std::fs::write(src.join("b.glb"), &glb_bytes).unwrap();

        let built = build_dir(&src, &root.join("out")).unwrap();
        assert_eq!(built.models.len(), 2);
        assert_eq!(built.textures.len(), 1, "{:?}", built.textures);
        assert!(
            built.textures[0].key.starts_with("_embedded/"),
            "{}",
            built.textures[0].key
        );
        assert_eq!(built.models[0].textures, built.models[1].textures);
    }

    #[test]
    fn duplicate_model_names_are_an_error() {
        let root = temp_dir("dup");
        write_quad(&root, "crate", "crate.png");
        std::fs::write(root.join("crate.glb"), b"not read").unwrap();
        let err = model_names(&root).unwrap_err();
        assert!(
            err.contains("crate.obj") && err.contains("crate.glb"),
            "{err}"
        );
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
        assert!(
            built.warnings[0].contains("501 triangles"),
            "{}",
            built.warnings[0]
        );
    }

    #[test]
    fn material_with_no_faces_does_not_bake_or_budget_its_texture() {
        // `usemtl skin` switches the current material but is immediately
        // followed by `usemtl bare` before any face uses it — the `skin`
        // sub-mesh exists (declared) but is empty. Its texture must not be
        // baked or counted in the level's texture budget (#66).
        let root = temp_dir("unused-mat");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("thing.obj"),
            "mtllib thing.mtl\n\
             v 0 0 0\nv 1 0 0\nv 0 1 0\n\
             usemtl skin\nusemtl bare\nf 1 2 3\n",
        )
        .unwrap();
        std::fs::write(
            root.join("thing.mtl"),
            "newmtl skin\nKd 1 1 1\nmap_Kd skin.png\nnewmtl bare\nKd 1 1 1\n",
        )
        .unwrap();
        std::fs::write(root.join("skin.png"), png_bytes()).unwrap();

        let built = build_dir(&root, &root.join("out")).unwrap();
        assert_eq!(built.models.len(), 1);
        assert!(built.textures.is_empty(), "{:?}", built.textures);
        assert!(
            built.models[0].textures.is_empty(),
            "{:?}",
            built.models[0].textures
        );
    }
}
