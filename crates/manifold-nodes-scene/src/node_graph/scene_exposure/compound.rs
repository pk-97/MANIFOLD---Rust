//! Upgrade the previous static-import compound shape to editable child transforms.
use manifold_core::{
    NodeId,
    effect_graph_def::{
        BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
    },
};

pub(super) fn migrate(def: &mut EffectGraphDef) -> bool {
    fn max_id(nodes: &[EffectGraphNode]) -> u32 {
        nodes
            .iter()
            .map(|node| {
                node.id
                    .max(node.group.as_ref().map_or(0, |group| max_id(&group.nodes)))
            })
            .max()
            .unwrap_or(0)
    }
    fn upgrade(nodes: &mut [EffectGraphNode], next: &mut u32, upgraded: &mut Vec<Vec<NodeId>>) {
        for node in nodes {
            let Some(group) = node.group.as_mut() else {
                continue;
            };
            upgrade(&mut group.nodes, next, upgraded);
            let objects: Vec<_> = group
                .nodes
                .iter()
                .filter(|n| n.type_id == "node.scene_object")
                .map(|n| n.id)
                .collect();
            if objects.len() < 2 || !node.node_id.as_str().starts_with("object_") {
                continue;
            }
            // Restrict the migration to the old importer's shared-pose, static mesh shape.
            let producer = |id, port| {
                group
                    .wires
                    .iter()
                    .find(|w| w.to_node == id && w.to_port == port)
            };
            let Some(shared) =
                producer(objects[0], "transform").map(|w| (w.from_node, w.from_port.clone()))
            else {
                continue;
            };
            if objects.iter().any(|&id| {
                producer(id, "parent_transform").is_some()
                    || producer(id, "transform").map(|w| (w.from_node, &w.from_port))
                        != Some((shared.0, &shared.1))
                    || producer(id, "vertices")
                        .and_then(|w| group.nodes.iter().find(|n| n.id == w.from_node))
                        .is_none_or(|n| n.type_id != "node.gltf_mesh_source")
            }) {
                continue;
            }
            let Some(template) = group
                .nodes
                .iter()
                .find(|n| n.type_id == "node.transform_3d")
                .cloned()
            else {
                continue;
            };
            let body_id = group
                .nodes
                .iter()
                .find(|n| n.type_id == "node.rigid_body")
                .map(|n| n.id);
            let mut child_ids = Vec::with_capacity(objects.len());
            let mut materials = Vec::with_capacity(objects.len());
            for (slot, object_id) in objects.into_iter().enumerate() {
                let source = group
                    .wires
                    .iter()
                    .find(|w| w.to_node == object_id && w.to_port == "vertices")
                    .unwrap()
                    .from_node;
                let material = group
                    .nodes
                    .iter()
                    .find(|n| n.id == source)
                    .unwrap()
                    .params
                    .get("material_index");
                let material = match material {
                    Some(SerializedParamValue::Int { value }) => *value as f32,
                    Some(SerializedParamValue::Float { value }) => *value,
                    _ => -1.0,
                };
                materials.push(vec![slot as f32, material]);
                let object = group.nodes.iter_mut().find(|n| n.id == object_id).unwrap();
                child_ids.push(object.node_id.clone());
                if slot == 0 {
                    object.handle = Some("Submesh 1".into());
                }
                let mut local = template.clone();
                local.id = *next;
                *next += 1;
                local.node_id = NodeId::new(format!("{}_local", object.node_id));
                local.handle = Some(local.node_id.to_string());
                local.params.clear();
                local.exposed_params.clear();
                for wire in &mut group.wires {
                    if wire.to_node == object_id && wire.to_port == "transform" {
                        wire.to_port = "parent_transform".into();
                    }
                }
                group.wires.push(EffectGraphWire {
                    from_node: local.id,
                    from_port: "transform".into(),
                    to_node: object_id,
                    to_port: "transform".into(),
                });
                if let Some(body) = body_id {
                    group.wires.push(EffectGraphWire {
                        from_node: local.id,
                        from_port: "transform".into(),
                        to_node: body,
                        to_port: format!("part_{slot}"),
                    });
                }
                group.nodes.push(local);
            }
            if let Some(body) = body_id.and_then(|id| group.nodes.iter_mut().find(|n| n.id == id)) {
                body.params.insert(
                    "compound_materials".into(),
                    SerializedParamValue::Table { rows: materials },
                );
            }
            upgraded.push(child_ids);
        }
    }
    let mut next = max_id(&def.nodes).saturating_add(1);
    let mut upgraded = Vec::new();
    upgrade(&mut def.nodes, &mut next, &mut upgraded);
    if let Some(metadata) = &mut def.preset_metadata {
        for children in &upgraded {
            let shared: std::collections::HashSet<_> = metadata
                .bindings
                .iter()
                .filter_map(|binding| {
                    let BindingTarget::Node { node_id, param } = &binding.target else {
                        return None;
                    };
                    (param == "visible"
                        && children.contains(node_id)
                        && metadata
                            .bindings
                            .iter()
                            .filter(|other| other.id == binding.id)
                            .count()
                            == children.len())
                    .then_some(binding.id.clone())
                })
                .collect();
            for binding in &mut metadata.bindings {
                if shared.contains(&binding.id)
                    && let BindingTarget::Node { param, .. } = &mut binding.target
                {
                    *param = "parent_visible".into();
                }
            }
        }
    }
    !upgraded.is_empty()
}
