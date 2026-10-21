//! Scene items (SCENE_SURFACES_DESIGN.md section 3.5 (scene items)): the one
//! list of things a scene holds, and the one rule for which item owns a
//! control. The card and the Scene tab both partition rows with
//! [`owner_of`], so they never disagree about where a control lives (D3 (one
//! scene item list)).
//!
//! A control belongs to the item whose nodes contain its primary binding's
//! target (the first binding under its id), or to a force when that target
//! is the force's scene modifier. Controls no item owns (composite macros,
//! authored controls on other nodes) are the scene's authored group.

use manifold_core::{NodeId, SceneNodeRef};
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, PresetMetadata};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, is_force_recipe};
use manifold_foundation::scene_surface::{ObjectKind, SceneItemKind};

use super::scene_vm::{MaterialVm, SceneLightVm, SceneObjectKnownRow, SceneObjectVm, SceneVm};

/// One thing in a scene and the nodes whose controls it owns.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneItem {
    pub kind: SceneItemKind,
    pub id: SceneItemId,
    pub name: String,
    pub owned_nodes: Vec<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SceneItemId {
    Camera,
    World,
    Light(NodeId),
    Object(NodeId),
    /// A force, by its scene-modifier instance id.
    Force(NodeId),
    /// A physics world, by its node.
    Physics(NodeId),
    /// A placed fluid source, by its role node, under the water row
    /// (its address) it feeds.
    Source { node: NodeId, water: SceneNodeRef },
}

/// Every item of the scene, in display order: Camera, lights, objects,
/// placed fluid sources, forces, physics worlds, World. Pure function of the
/// def; built on the UI thread at structural sync, at the same cadence as
/// [`SceneVm`].
pub fn scene_items(def: &EffectGraphDef, vm: &SceneVm) -> Vec<SceneItem> {
    let mut items = vec![SceneItem {
        kind: SceneItemKind::Camera,
        id: SceneItemId::Camera,
        name: "Camera".into(),
        owned_nodes: vm.camera_nodes.clone(),
    }];
    items.extend(vm.lights.iter().filter_map(|light| match light {
        SceneLightVm::Known(row) => Some(SceneItem {
            kind: SceneItemKind::Light,
            id: SceneItemId::Light(row.node.clone()),
            name: row.name.clone(),
            owned_nodes: vec![row.node.clone()],
        }),
        SceneLightVm::Custom { .. } => None,
    }));
    items.extend(vm.objects.iter().filter_map(|object| match object {
        SceneObjectVm::Known(row) => Some(SceneItem {
            kind: SceneItemKind::Object(object_kind(row)),
            id: SceneItemId::Object(row.object.clone()),
            name: row.name.clone(),
            owned_nodes: object_nodes(row, &vm.objects),
        }),
        _ => None,
    }));
    // Two rows drawing one water list its sources once, under the first.
    let mut listed: Vec<&NodeId> = Vec::new();
    for object in &vm.objects {
        let SceneObjectVm::Known(row) = object else { continue };
        for source in &row.placed_sources {
            if listed.contains(&&source.node) {
                continue;
            }
            listed.push(&source.node);
            items.push(SceneItem {
                kind: SceneItemKind::Source(source.role),
                id: SceneItemId::Source { node: source.node.clone(), water: row.address.clone() },
                name: source.name.clone(),
                owned_nodes: source.owned.clone(),
            });
        }
    }
    items.extend(def.scene_modifiers.iter().filter(|instance| is_force_recipe(&instance.graph)).map(|instance| {
        SceneItem {
            kind: SceneItemKind::Force,
            id: SceneItemId::Force(instance.id.clone()),
            name: scene_modifier_display_name(def, instance).unwrap_or_else(|| instance.id.to_string()),
            owned_nodes: Vec::new(),
        }
    }));
    let several = vm.physics_nodes.len() > 1;
    items.extend(vm.physics_nodes.iter().enumerate().filter_map(|(index, nodes)| {
        let world = nodes.first()?.clone();
        let title = node_by_id(&def.nodes, &world).and_then(|node| node.title.clone()).filter(|t| !t.is_empty());
        let name = title.unwrap_or_else(|| if several { format!("Physics {}", index + 1) } else { "Physics".into() });
        Some(SceneItem {
            kind: SceneItemKind::Physics,
            id: SceneItemId::Physics(world),
            name,
            owned_nodes: nodes.clone(),
        })
    }));
    items.push(SceneItem {
        kind: SceneItemKind::World,
        id: SceneItemId::World,
        name: "World".into(),
        owned_nodes: vm.world_nodes.clone(),
    });
    items
}

/// Index into `items` of the item owning control `param_id` by its primary
/// binding; `None` is the scene's authored group. A material wired into
/// several objects belongs to the first of them here, though the Scene tab
/// shows it on each.
pub fn owner_of(items: &[SceneItem], meta: &PresetMetadata, param_id: &str) -> Option<usize> {
    let binding = meta.bindings.iter().find(|binding| binding.id == param_id)?;
    match &binding.target {
        BindingTarget::Node { node_id, .. } => items.iter().position(|item| item.owned_nodes.contains(node_id)),
        BindingTarget::SceneModifier { modifier_id, .. } => {
            items.iter().position(|item| matches!(&item.id, SceneItemId::Force(id) if id == modifier_id))
        }
        BindingTarget::Composite { .. } => None,
    }
}

/// The display name shared by force and modifier cards, trigger target
/// groups and the force's scene item. Duplicate recipe names get an
/// occurrence suffix only when the stack needs disambiguation.
pub fn scene_modifier_display_name(def: &EffectGraphDef, instance: &SceneModifierInstanceDef) -> Option<String> {
    let base = instance.graph.preset_metadata.as_ref()?.display_name.clone();
    let same_name = |candidate: &&SceneModifierInstanceDef| {
        candidate.graph.preset_metadata.as_ref().is_some_and(|metadata| metadata.display_name == base)
    };
    if def.scene_modifiers.iter().filter(same_name).count() <= 1 {
        return Some(base);
    }
    let occurrence = def
        .scene_modifiers
        .iter()
        .take_while(|candidate| candidate.id != instance.id)
        .filter(same_name)
        .count()
        + 1;
    Some(format!("{base} #{occurrence}"))
}

fn object_kind(row: &SceneObjectKnownRow) -> ObjectKind {
    if row.liquid_domain.is_some() {
        ObjectKind::Water
    } else if row.group_node_id.is_some() || row.skin.is_some() {
        ObjectKind::Model
    } else {
        ObjectKind::Mesh
    }
}

/// The nodes whose controls an object owns: its scene_object, its liquid,
/// its enabled body, transform, material and every modifier. Shared
/// transforms and modifiers belong to their first consuming object in scene
/// order; a material stays with every consumer.
fn object_nodes(row: &SceneObjectKnownRow, objects: &[SceneObjectVm]) -> Vec<NodeId> {
    let mut owned = vec![row.object.clone()];
    owned.extend(row.controls.iter().map(|control| control.node.clone()));
    if let Some(physics) = row.physics.as_ref().filter(|physics| physics.enabled) {
        owned.push(physics.body.clone());
    }
    if let Some(transform) = &row.transform {
        owned.push(transform.node.clone());
    }
    if let MaterialVm::Known(material) = &row.material {
        owned.push(material.node.clone());
    }
    owned.extend(row.modifier_chain.iter().map(|modifier| modifier.node.clone()));
    owned.extend(row.transform_chain.iter().map(|modifier| modifier.node.clone()));
    for object in objects {
        let SceneObjectVm::Known(other) = object else { continue };
        if other.object == row.object && other.index == row.index {
            break;
        }
        for node in other
            .transform
            .iter()
            .map(|transform| &transform.node)
            .chain(other.modifier_chain.iter().map(|modifier| &modifier.node))
            .chain(other.transform_chain.iter().map(|modifier| &modifier.node))
        {
            owned.retain(|owned_node| owned_node != node);
        }
    }
    owned
}

fn node_by_id<'a>(nodes: &'a [EffectGraphNode], id: &NodeId) -> Option<&'a EffectGraphNode> {
    nodes.iter().find_map(|node| {
        if &node.node_id == id {
            Some(node)
        } else {
            node.group.as_deref().and_then(|group| node_by_id(&group.nodes, id))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::BindingDef;

    fn def(json: serde_json::Value) -> EffectGraphDef {
        serde_json::from_value(json).expect("scene def")
    }

    fn binding(id: &str, node: &str, param: &str) -> BindingDef {
        serde_json::from_value(serde_json::json!({
            "id": id, "defaultValue": 0.0,
            "target": {"kind": "node", "nodeId": node, "param": param}
        }))
        .expect("binding")
    }

    /// Camera first, then lights, objects, physics worlds and World; the
    /// copy body and its transform belong to the physics world.
    #[test]
    fn items_follow_display_order_and_the_world_owns_its_copy_body() {
        let mut def = def(serde_json::json!({
            "version": 2,
            "nodes": [
                {"id": 1, "nodeId": "camera", "typeId": "node.orbit_camera"},
                {"id": 4, "nodeId": "sun", "typeId": "node.light", "handle": "sun"},
                {"id": 40, "nodeId": "world", "typeId": "node.physics_world"},
                {"id": 120, "nodeId": "box_transform", "typeId": "node.transform_3d"},
                {"id": 121, "nodeId": "box_body", "typeId": "node.rigid_body"},
                {"id": 30, "nodeId": "scene", "typeId": "node.render_scene",
                    "params": {"lights": {"type": "Float", "value": 1.0}}},
                {"id": 31, "nodeId": "final", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 1, "fromPort": "out", "toNode": 30, "toPort": "camera"},
                {"fromNode": 4, "fromPort": "out", "toNode": 30, "toPort": "light_0"},
                {"fromNode": 120, "fromPort": "transform", "toNode": 121, "toPort": "transform"},
                {"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies_0"},
                {"fromNode": 30, "fromPort": "color", "toNode": 31, "toPort": "in"}
            ],
            "presetMetadata": {
                "id": "t", "displayName": "T", "category": "Diagnostic", "oscPrefix": "t",
                "params": [], "bindings": []
            }
        }));
        let meta = def.preset_metadata.as_mut().unwrap();
        meta.bindings = vec![
            binding("1_orbit", "camera", "orbit"),
            binding("4_intensity", "sun", "intensity"),
            binding("40_copy_count", "box_body", "copy_count"),
            binding("121_shape", "box_body", "shape"),
            binding("120_pos_x", "box_transform", "pos_x"),
            binding("30_rt_enabled", "scene", "rt_enabled"),
        ];
        let vm = SceneVm::from_def(&def).expect("a scene");
        let items = scene_items(&def, &vm);
        let kinds: Vec<_> = items.iter().map(|item| item.kind).collect();
        assert_eq!(
            kinds,
            [SceneItemKind::Camera, SceneItemKind::Light, SceneItemKind::Physics, SceneItemKind::World]
        );
        assert_eq!(items[2].name, "Physics");
        let meta = def.preset_metadata.as_ref().unwrap();
        let owner = |id: &str| owner_of(&items, meta, id).map(|i| items[i].kind);
        assert_eq!(owner("1_orbit"), Some(SceneItemKind::Camera));
        assert_eq!(owner("4_intensity"), Some(SceneItemKind::Light));
        assert_eq!(owner("40_copy_count"), Some(SceneItemKind::Physics));
        assert_eq!(owner("121_shape"), Some(SceneItemKind::Physics));
        assert_eq!(owner("120_pos_x"), Some(SceneItemKind::Physics));
        assert_eq!(owner("30_rt_enabled"), Some(SceneItemKind::World));
        assert_eq!(owner("missing"), None);
    }
}
