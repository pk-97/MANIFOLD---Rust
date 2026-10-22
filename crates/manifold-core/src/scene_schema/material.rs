//! Material: one `node.pbr_material` that every item bound to it shares, and
//! its texture source (`node.layer_source` for a layer, `node.gltf_texture_source`
//! for a file) feeding the base colour of each object drawing with it. A
//! material of a model file reads its factors, every map and every extension
//! from the file: one `node.gltf_texture_source` per map, each feeding its
//! own port of the objects drawing with it.
//!
//! The surface params are a family, not one row each: every param the
//! material node declares is a card row in the Material section with the
//! param's own name as its property, placed by its material inspector role,
//! except the feature-mode params, which the Material's features field sets.

use manifold_foundation::scene_surface::ControlGroup;

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, enabled, wrong_value};
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

fn model(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Material(m) if matches!(m.texture, TextureSource::Model { .. }))
}

const TEXTURE: Reach = Reach::Own("texture");
use ControlGroup::Material;

const FROM_MODEL: Exposure = Exposure::Automatic("the model file's material");

/// The roles of a model material's texture nodes: its base colour, then one
/// per map family (`scene_compile::material`), each named for the
/// `node.scene_object` port it feeds.
pub const MODEL_MAP_ROLES: [&str; 19] = [
    "base_color_map",
    "normal_map",
    "mr_map",
    "occlusion_map",
    "emissive_map",
    "sheen_color_map",
    "sheen_roughness_map",
    "iridescence_map",
    "iridescence_thickness_map",
    "anisotropy_map",
    "clearcoat_map",
    "clearcoat_roughness_map",
    "clearcoat_normal_map",
    "specular_map",
    "specular_color_map",
    "transmission_map",
    "diffuse_transmission_map",
    "diffuse_transmission_color_map",
    "volume_thickness_map",
];

/// The rows of every `node.gltf_texture_source` param on each model map
/// role: all set from the file.
macro_rules! material_rows {
    ($($role:literal),* $(,)?) => {
        [
            row("texture_layer", TEXTURE, Material, Exposure::Automatic("the texture source field")).param("layer").when(layer),
            row("texture_file", TEXTURE, Material, Exposure::Automatic("the texture source field")).param("path").when(file),
            row("texture_index", TEXTURE, Material, Exposure::Internal("a picked file holds one image")).when(file),
            row("color_space", TEXTURE, Material, Exposure::Internal("a base colour image is sRGB")).when(file),
            row("texture_width", TEXTURE, Material, Exposure::Internal("the file's own size")).param("width").when(file),
            row("texture_height", TEXTURE, Material, Exposure::Internal("the file's own size")).param("height").when(file),
            row("repack", TEXTURE, Material, Exposure::Internal("a base colour image is not repacked")).param("mode").when(file),
            row("glossiness_factor", TEXTURE, Material, Exposure::Internal("only a repacked gloss map reads it")).when(file),
            $(
                row(concat!($role, "_path"), Reach::Own($role), Material, FROM_MODEL).param("path").when(model),
                row(concat!($role, "_index"), Reach::Own($role), Material, FROM_MODEL).param("texture_index").when(model),
                row(concat!($role, "_color_space"), Reach::Own($role), Material, FROM_MODEL).param("color_space").when(model),
                row(concat!($role, "_width"), Reach::Own($role), Material, FROM_MODEL).param("width").when(model),
                row(concat!($role, "_height"), Reach::Own($role), Material, FROM_MODEL).param("height").when(model),
                row(concat!($role, "_repack"), Reach::Own($role), Material, FROM_MODEL).param("mode").when(model),
                row(concat!($role, "_glossiness"), Reach::Own($role), Material, FROM_MODEL).param("glossiness_factor").when(model),
            )*
        ]
    };
}

pub static MATERIAL_ROWS: [PropertyRow; 8 + 7 * MODEL_MAP_ROLES.len()] = material_rows![
    "base_color_map",
    "normal_map",
    "mr_map",
    "occlusion_map",
    "emissive_map",
    "sheen_color_map",
    "sheen_roughness_map",
    "iridescence_map",
    "iridescence_thickness_map",
    "anisotropy_map",
    "clearcoat_map",
    "clearcoat_roughness_map",
    "clearcoat_normal_map",
    "specular_map",
    "specular_color_map",
    "transmission_map",
    "diffuse_transmission_map",
    "diffuse_transmission_color_map",
    "volume_thickness_map",
];

const TEXTURE_SOURCES: &[Choice] = &[Choice { key: "none", label: "None" }, Choice { key: "file", label: "File" }];

/// A model file's material keeps its file's maps until another source is
/// picked.
const MODEL_TEXTURE_SOURCES: &[Choice] =
    &[Choice { key: "model", label: "From Model File" }, Choice { key: "none", label: "None" }, Choice { key: "file", label: "File" }];

fn not_model(fields: &ItemFields) -> bool {
    !model(fields)
}

fn read_texture(fields: &ItemFields) -> FieldValue {
    match fields {
        ItemFields::Material(m) => match &m.texture {
            TextureSource::None => FieldValue::Choice("none".to_owned()),
            TextureSource::File { .. } => FieldValue::Choice("file".to_owned()),
            TextureSource::Layer { layer } => FieldValue::Layer(layer.clone()),
            TextureSource::Model { .. } => FieldValue::Choice("model".to_owned()),
        },
        _ => FieldValue::Choice("none".to_owned()),
    }
}

fn write_texture(fields: &mut ItemFields, value: &FieldValue) -> Result<(), String> {
    let ItemFields::Material(m) = fields else {
        return Err(wrong_value("texture", value));
    };
    m.texture = match value {
        FieldValue::Choice(key) if key == "none" => TextureSource::None,
        FieldValue::Choice(key) if key == "file" => match &m.texture {
            TextureSource::File { .. } => m.texture.clone(),
            _ => TextureSource::File { path: String::new() },
        },
        // The model's maps can be kept, never picked back once replaced.
        FieldValue::Choice(key) if key == "model" && matches!(m.texture, TextureSource::Model { .. }) => m.texture.clone(),
        FieldValue::Layer(layer) => TextureSource::Layer { layer: layer.clone() },
        other => return Err(wrong_value("texture", other)),
    };
    Ok(())
}

/// The texture source is a field; the features are set by the feature menu
/// (one `Set` with the feature's defaults, so one undo removes it).
pub static MATERIAL_FIELDS: [FieldRow; 3] = [
    FieldRow {
        id: "texture",
        label: "Texture",
        section: Material,
        control: FieldControl::ClosedOrLayer(TEXTURE_SOURCES),
        applies: not_model,
        disabled: enabled,
        read: read_texture,
        write: write_texture,
    },
    FieldRow {
        id: "texture",
        label: "Texture",
        section: Material,
        control: FieldControl::ClosedOrLayer(MODEL_TEXTURE_SOURCES),
        applies: model,
        disabled: enabled,
        read: read_texture,
        write: write_texture,
    },
    FieldRow {
        id: "texture_file",
        label: "Texture File",
        section: Material,
        control: FieldControl::File { extensions: &["png", "jpg", "jpeg", "tga", "bmp", "exr", "hdr"] },
        applies: file,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Material(m) => match &m.texture {
                TextureSource::File { path } => FieldValue::File(path.clone()),
                _ => FieldValue::File(String::new()),
            },
            _ => FieldValue::File(String::new()),
        },
        write: |fields, value| match (fields, value) {
            (ItemFields::Material(m), FieldValue::File(path)) => {
                m.texture = TextureSource::File { path: path.clone() };
                Ok(())
            }
            _ => Err(wrong_value("texture_file", value)),
        },
    },
];
