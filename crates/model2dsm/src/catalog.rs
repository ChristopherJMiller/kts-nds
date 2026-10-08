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
