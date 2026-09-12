//! Shared test wiring for authored v3 scene-modifier recipes.

// Each integration-test binary imports a different subset of these helpers.
#![allow(dead_code)]

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effect_graph_def::EffectGraphNode;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_core::NodeId;
use manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_renderer::node_graph::scene_modifier_authoring::scene_modifier_objects;
use manifold_renderer::node_graph::bundled_preset_def;

pub fn stock_recipe(name: &str) -> EffectGraphDef {
    bundled_preset_def(&PresetTypeId::from_string(name.to_string()))
        .unwrap_or_else(|| panic!("bundled scene-modifier recipe {name} is unavailable"))
        .clone()
}

pub fn render_scene(owner: &EffectGraphDef) -> SceneNodeRef {
    let node = owner
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap_or_else(|| panic!("owner has no render_scene node"));
    SceneNodeRef {
        scope: Vec::new(),
        node: node.node_id.clone(),
    }
}

pub fn attach(
    owner: &EffectGraphDef,
    recipe_name: &str,
    instance_id: &str,
) -> EffectGraphDef {
    let instance = prepare_new_scene_modifier(
        owner,
        &stock_recipe(recipe_name),
        NodeId::new(instance_id),
        render_scene(owner),
        SceneTargetSelection::AllObjects,
    )
    .unwrap_or_else(|error| panic!("{recipe_name} preparation failed: {error}"));
    manifold_core::scene_modifier_edit::insert_scene_modifier(
        owner,
        owner.scene_modifiers.len(),
        instance,
    )
    .unwrap_or_else(|error| panic!("{recipe_name} insertion failed: {error}"))
    .graph
}

pub fn scene_objects(
    owner: &EffectGraphDef,
) -> Vec<manifold_core::scene_modifier_preset::SceneNodeRef> {
    scene_modifier_objects(owner, &render_scene(owner))
        .unwrap_or_else(|error| panic!("scene object enumeration failed: {error}"))
}

/// Find an authored leaf by stable id, including leaves inside stage groups.
pub fn find_node<'a>(nodes: &'a [EffectGraphNode], node_id: &str) -> Option<&'a EffectGraphNode> {
    for node in nodes {
        if node.node_id.as_str() == node_id {
            return Some(node);
        }
        if let Some(group) = node.group.as_deref()
            && let Some(found) = find_node(&group.nodes, node_id)
        {
            return Some(found);
        }
    }
    None
}
