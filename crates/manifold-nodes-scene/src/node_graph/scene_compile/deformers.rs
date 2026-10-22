//! Deformer cards (SCENE_DOCUMENT_DESIGN.md section 7.19 (Modifiers)): what
//! a deformer card's fields change in its stock recipe. Displace by Texture
//! reads the image its Texture field picks.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::scene_doc::{ModifierFields, ModifierTexture, ModifierType};

/// The node of the Displace by Texture recipe that makes its image.
const TEXTURE_NODE: &str = "noise";

/// Point Displace by Texture at the card's Texture: the recipe's built-in
/// noise, a layer (`node.layer_source`) or a file (`node.gltf_texture_source`),
/// the sources a Material's texture uses. A file not yet picked keeps the
/// noise, so the card never displaces by a blank image.
pub(crate) fn tune(graph: &mut EffectGraphDef, card: &ModifierFields) {
    if card.modifier != ModifierType::DisplaceByTexture {
        return;
    }
    let (type_id, param, value) = match &card.texture {
        ModifierTexture::Noise => return,
        ModifierTexture::File { path } if path.is_empty() => return,
        ModifierTexture::File { path } => ("node.gltf_texture_source", "path", path.clone()),
        ModifierTexture::Layer { layer } => ("node.layer_source", "layer", layer.to_string()),
    };
    if let Some(node) = find(&mut graph.nodes, TEXTURE_NODE) {
        type_id.clone_into(&mut node.type_id);
        node.params = BTreeMap::from([(param.to_owned(), SerializedParamValue::String { value })]);
    }
}

fn find<'a>(nodes: &'a mut [EffectGraphNode], node_id: &str) -> Option<&'a mut EffectGraphNode> {
    for node in nodes {
        if node.node_id.as_str() == node_id {
            return Some(node);
        }
        if let Some(body) = node.group.as_deref_mut()
            && let Some(found) = find(&mut body.nodes, node_id)
        {
            return Some(found);
        }
    }
    None
}
