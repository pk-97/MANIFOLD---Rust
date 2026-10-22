//! Material: one shared surface node per Material item, every param on its
//! card (the material family rule), its features set from the features
//! field, and its texture source feeding the base colour of each object
//! drawing with it. A material of a model file starts at the file's factors
//! and feeds every map the file gives it (`model_material.rs`). An object
//! with no material anywhere above it draws with the scene's plain material.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::material_inspector::{MATERIAL_FEATURE_MODE_PARAMS, MATERIAL_FEATURES};
use manifold_core::scene_doc::{Item, ItemFields, ItemId, MaterialType, TextureFeeds, TextureSource};
use manifold_core::scene_schema::{CardFacts, MATERIAL_FAMILY};
use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardLabel, SurfaceRole, Unit};

use super::builder::{Builder, SCENE, card_spec, number};
use crate::node_graph::gltf_import::model_file::model_file;

/// The family's placement: Scene tab rows in the Material section.
const SURFACE_FACTS: CardFacts = CardFacts {
    role: SurfaceRole::Setup,
    band: None,
    order: 0,
    label: CardLabel::Param,
    gate: None,
    switch: None,
    colour: None,
    unit: None,
    limit: HardLimit::Slider,
    toggle: false,
    range: None,
};

/// Emit every Material item, in tree order.
pub(crate) fn emit_all(b: &mut Builder<'_>) {
    let mut materials = Vec::new();
    b.doc.walk(|item, _| {
        if let ItemFields::Material(_) = item.fields {
            materials.push(item.clone());
        }
    });
    for item in &materials {
        emit(b, item);
    }
}

fn emit(b: &mut Builder<'_>, item: &Item) {
    let ItemFields::Material(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let node_type = manifold_core::scene_schema::material_node_type(fields);
    let surface = b.add(owner, MATERIAL_FAMILY.node, node_type);
    // A layer feeds the surface the Material says; a file always feeds the
    // base colour.
    let feeds = match fields.texture_feeds {
        TextureFeeds::BaseColour => "base_color_map",
        TextureFeeds::Emission => "emissive_map",
    };
    // A model material's surface starts at the file's values, so its card
    // rows do too.
    let maps = match &fields.texture {
        TextureSource::None => Vec::new(),
        TextureSource::Layer { layer } => {
            let texture = b.add(owner, "texture", "node.layer_source");
            b.set(texture, "layer", SerializedParamValue::String { value: layer.to_string() });
            vec![(texture, feeds)]
        }
        TextureSource::File { path } => {
            let texture = b.add(owner, "texture", "node.gltf_texture_source");
            b.set(texture, "path", SerializedParamValue::String { value: path.clone() });
            vec![(texture, "base_color_map")]
        }
        TextureSource::Model { file, material } => match model_file(file) {
            Some(model) => super::model_material::emit(b, owner, surface, file, &model, *material),
            None => Vec::new(),
        },
    };
    b.material_maps.insert(owner.to_owned(), maps);
    for meta in b.type_params(node_type) {
        if MATERIAL_FAMILY.automatic.contains(&meta.name.as_str()) {
            continue;
        }
        let mut facts = SURFACE_FACTS;
        if matches!(meta.name.as_str(), "iridescence_thickness_min" | "iridescence_thickness_max") {
            facts.unit = Some(Unit::Nanometres);
        }
        let id = manifold_core::scene_doc::param_id(&item.id, &meta.name);
        let default = b.started(surface, &meta.name).unwrap_or_else(|| number(&meta.default_value));
        let mut spec = card_spec(id, &meta.label, &meta, default, MATERIAL_FAMILY.section, &facts);
        spec.material_role = meta.material_role;
        b.declare(spec, &meta, &[surface], &meta.name);
    }
    let features = if node_type == MaterialType::Pbr.node_type() { fields.features.as_slice() } else { &[] };
    for (feature, param) in MATERIAL_FEATURES.iter().zip(MATERIAL_FEATURE_MODE_PARAMS) {
        if let Some(setting) = features.iter().find(|setting| setting.feature == *feature) {
            b.set_number(surface, param, setting.mode as f32);
        }
    }
    b.rows(item);
}

/// The surface node an item draws with, and each texture source and the
/// `node.scene_object` port it feeds: its effective material's, else the
/// scene's plain material (made on first use).
pub(crate) fn surface_for(b: &mut Builder<'_>, item: &ItemId) -> (u32, Vec<(u32, &'static str)>) {
    if let Some(material) = b.doc.effective_material(item)
        && let Some(surface) = b.id(material.as_str(), MATERIAL_FAMILY.node)
    {
        return (surface, b.material_maps.get(material.as_str()).cloned().unwrap_or_default());
    }
    let plain = match b.id(SCENE, "default_material") {
        Some(id) => id,
        None => b.add(SCENE, "default_material", MATERIAL_FAMILY.type_id),
    };
    (plain, Vec::new())
}
