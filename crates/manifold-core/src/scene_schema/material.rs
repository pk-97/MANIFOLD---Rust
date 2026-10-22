//! Material: one `node.pbr_material` that every item bound to it shares, and
//! its texture source (`node.layer_source` for a layer, `node.gltf_texture_source`
//! for a file) feeding the base colour of each object drawing with it.
//!
//! The surface params are a family, not one row each: every param the
//! material node declares is a card row in the Material section with the
//! param's own name as its property, placed by its material inspector role,
//! except the feature-mode params, which the Material's features field sets.

use manifold_foundation::scene_surface::ControlGroup;

use super::{Exposure, PropertyRow, Reach, row};
use crate::scene_doc::{ItemFields, TextureSource};

/// The family rule for the material node's own params.
#[derive(Debug, Clone, Copy)]
pub struct MaterialFamily {
    /// The role of the node the family covers.
    pub node: &'static str,
    pub type_id: &'static str,
    pub section: ControlGroup,
    /// Params the compiler sets from the features field.
    pub automatic: &'static [&'static str],
    pub automatic_reason: &'static str,
}

pub const MATERIAL_FAMILY: MaterialFamily = MaterialFamily {
    node: "surface",
    type_id: "node.pbr_material",
    section: ControlGroup::Material,
    automatic: crate::material_inspector::MATERIAL_FEATURE_MODE_PARAMS,
    automatic_reason: "the Material's features field",
};

fn layer(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Material(m) if matches!(m.texture, TextureSource::Layer { .. }))
}

fn file(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Material(m) if matches!(m.texture, TextureSource::File { .. }))
}

const TEXTURE: Reach = Reach::Own("texture");
use ControlGroup::Material;

pub static MATERIAL_ROWS: [PropertyRow; 8] = [
    row("texture_layer", TEXTURE, Material, Exposure::Automatic("the texture source field")).param("layer").when(layer),
    row("texture_file", TEXTURE, Material, Exposure::Automatic("the texture source field")).param("path").when(file),
    row("texture_index", TEXTURE, Material, Exposure::Internal("a picked file holds one image")).when(file),
    row("color_space", TEXTURE, Material, Exposure::Internal("a base colour image is sRGB")).when(file),
    row("texture_width", TEXTURE, Material, Exposure::Internal("the file's own size")).param("width").when(file),
    row("texture_height", TEXTURE, Material, Exposure::Internal("the file's own size")).param("height").when(file),
    row("repack", TEXTURE, Material, Exposure::Internal("a base colour image is not repacked")).param("mode").when(file),
    row("glossiness_factor", TEXTURE, Material, Exposure::Internal("only a repacked gloss map reads it")).when(file),
];
