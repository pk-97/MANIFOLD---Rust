//! A MaterialX material file (`.mtlx`) read as a model file with one
//! material and no parts. Its pictures become textures by index and its
//! values a glTF material, so a Material pointing at one compiles through the
//! model-material wiring (`scene_compile::model_material`) and decodes through
//! the same texture jobs as a GLB's material.
//!
//! Material libraries ship roughness and metalness as two grey pictures,
//! where glTF packs them into one (G = roughness, B = metalness), and an
//! opacity picture beside the colour, where glTF keeps it in the colour's
//! alpha. A texture here is therefore a stack: a whole picture or a fill,
//! with other pictures' red channels written into chosen channels. The
//! decode packs a stack once, so drawing it costs what a GLB's map costs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use manifold_core::materialx::{self, MapSlot, Material};
use manifold_core::scene_doc::ModelFacts;

use super::model_file::{ModelFile, Tiles};
use crate::node_graph::gltf_load::{self, GltfMaterialInfo};

/// One texture of a material file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StackedTexture {
    /// The picture the texture starts from, read whole.
    pub whole: Option<PathBuf>,
    /// The texel the texture starts from when there is no whole picture.
    pub fill: [u8; 4],
    /// Each picture whose red channel is written into channel `.1`.
    pub channels: Vec<(PathBuf, usize)>,
}

impl StackedTexture {
    fn whole(path: PathBuf) -> Self {
        Self {
            whole: Some(path),
            fill: [0; 4],
            channels: Vec::new(),
        }
    }

    /// Every picture the texture reads, the whole one first.
    pub(crate) fn pictures(&self) -> impl Iterator<Item = &Path> {
        self.whole
            .iter()
            .map(PathBuf::as_path)
            .chain(self.channels.iter().map(|(path, _)| path.as_path()))
    }
}

/// The textures of `material` by texture index, and the glTF material that
/// names them. Values stand in for a missing picture: a material with a
/// roughness picture and no metalness picture reads its metalness value.
pub(crate) fn textures(material: &Material) -> (Vec<StackedTexture>, GltfMaterialInfo) {
    let picture = |slot| material.map(slot).map(|map| map.path.clone());
    let mut textures = Vec::new();
    let mut add = |texture: StackedTexture| {
        textures.push(texture);
        Some(textures.len() as u32 - 1)
    };
    let mut info = gltf_load::default_material_info();

    let colour = picture(MapSlot::BaseColor);
    let opacity = picture(MapSlot::Opacity);
    let [r, g, b] = material.base_color;
    info.base_color_factor = [
        r,
        g,
        b,
        if opacity.is_some() {
            1.0
        } else {
            material.opacity
        },
    ];
    info.alpha_mask = opacity.is_some();
    if colour.is_some() || opacity.is_some() {
        info.base_color_texture = add(StackedTexture {
            whole: colour,
            fill: [255; 4],
            channels: opacity.into_iter().map(|path| (path, 3)).collect(),
        });
    }

    if let Some(normal) = picture(MapSlot::Normal) {
        info.normal_texture = add(StackedTexture::whole(normal));
        info.normal_scale = material.normal_scale;
    }

    let roughness = picture(MapSlot::Roughness);
    let metalness = picture(MapSlot::Metalness);
    info.roughness = if roughness.is_some() {
        1.0
    } else {
        material.roughness
    };
    info.metallic = if metalness.is_some() {
        1.0
    } else {
        material.metalness
    };
    if roughness.is_some() || metalness.is_some() {
        // A missing channel reads 1, so its factor alone is the value.
        let channels = [(roughness, 1), (metalness, 2)]
            .into_iter()
            .filter_map(|(path, channel)| path.map(|path| (path, channel)))
            .collect();
        info.mr_texture = add(StackedTexture {
            whole: None,
            fill: [0, 255, 255, 255],
            channels,
        });
    }

    if let Some(occlusion) = picture(MapSlot::Occlusion) {
        info.occlusion_texture = add(StackedTexture::whole(occlusion));
    }

    info.emissive = material.emission;
    if let Some(emission) = picture(MapSlot::Emission) {
        info.emissive_texture = add(StackedTexture::whole(emission));
    }
    (textures, info)
}

/// Read the facts of the MaterialX file at `path`: on the worker only. A
/// picture the file names that is not there fails the read, so the Material
/// shows the problem instead of drawing a stand-in.
pub(crate) fn read_facts(path: &Path) -> Result<(ModelFacts, Vec<String>), String> {
    let material = materialx::read(path)?;
    let (textures, info) = textures(&material);
    let mut texture_dims = Vec::with_capacity(textures.len());
    for texture in &textures {
        let mut size = None;
        for picture in texture.pictures() {
            let dims = gltf_load::picture_file_dims(picture).ok_or_else(|| {
                format!(
                    "{} names {}, which is missing or not a picture",
                    path.display(),
                    picture.display()
                )
            })?;
            if size.is_some_and(|size| size != dims) {
                return Err(format!(
                    "{} packs pictures of different sizes into one texture ({})",
                    path.display(),
                    picture.display()
                ));
            }
            size = Some(dims);
        }
        texture_dims.push(size.unwrap_or((2, 2)));
    }
    let facts = ModelFile {
        materials: vec![info],
        default_material: gltf_load::default_material_info(),
        texture_dims,
        radius: 1e-3,
        nodes: HashMap::new(),
        // A file that gives no real-world size tiles once a metre.
        tiles: Some(Tiles {
            size: material.tile_size.unwrap_or(1.0),
            hex: material.hex_tiled,
        }),
    };
    Ok((ModelFacts::new(facts), material.warnings))
}

/// Texture `texture_index` of the MaterialX file at `path`, for the decode.
pub(crate) fn texture(path: &Path, texture_index: u32) -> Result<StackedTexture, String> {
    let material = materialx::read(path)?;
    let (mut textures, _) = textures(&material);
    let count = textures.len();
    if (texture_index as usize) < count {
        Ok(textures.swap_remove(texture_index as usize))
    } else {
        Err(format!(
            "{} has {count} textures; texture {texture_index} is out of range",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::materialx::MapFile;

    fn map(slot: MapSlot, name: &str) -> MapFile {
        MapFile {
            slot,
            relative: name.to_owned(),
            path: PathBuf::from(name),
        }
    }

    fn material(maps: Vec<MapFile>) -> Material {
        Material {
            maps,
            base_color: [1.0; 3],
            opacity: 1.0,
            roughness: 0.4,
            metalness: 0.7,
            normal_scale: 1.0,
            emission: [0.0; 3],
            tile_size: None,
            hex_tiled: false,
            warnings: Vec::new(),
        }
    }

    /// Separate roughness and metalness pictures pack into one glTF
    /// metal-rough texture, G then B, at full factors.
    #[test]
    fn roughness_and_metalness_pictures_pack_into_green_and_blue() {
        let (textures, info) = textures(&material(vec![
            map(MapSlot::BaseColor, "c.jpg"),
            map(MapSlot::Metalness, "m.jpg"),
            map(MapSlot::Roughness, "r.jpg"),
        ]));
        let mr = &textures[info.mr_texture.unwrap() as usize];
        assert_eq!(mr.whole, None);
        assert_eq!(
            mr.channels,
            vec![(PathBuf::from("r.jpg"), 1), (PathBuf::from("m.jpg"), 2)]
        );
        assert_eq!((info.roughness, info.metallic), (1.0, 1.0));
    }

    /// With only a roughness picture, the blue channel fills to 1 so the
    /// metalness value alone sets metal; the opacity picture lands in the
    /// colour's alpha and turns on the cut-out.
    #[test]
    fn a_missing_picture_reads_its_value_and_opacity_cuts_out() {
        let (textures, info) = textures(&material(vec![
            map(MapSlot::BaseColor, "c.jpg"),
            map(MapSlot::Opacity, "o.jpg"),
            map(MapSlot::Roughness, "r.jpg"),
        ]));
        let mr = &textures[info.mr_texture.unwrap() as usize];
        assert_eq!(mr.fill[2], 255);
        assert_eq!((info.roughness, info.metallic), (1.0, 0.7));
        let colour = &textures[info.base_color_texture.unwrap() as usize];
        assert_eq!(colour.whole, Some(PathBuf::from("c.jpg")));
        assert_eq!(colour.channels, vec![(PathBuf::from("o.jpg"), 3)]);
        assert!(info.alpha_mask);
    }
}
