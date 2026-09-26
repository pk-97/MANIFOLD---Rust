//! Content-owned copy, paste, and duplicate operations for object mesh modifiers.
//!
//! Object cards are graph nodes inside a generator.  Their document IDs are
//! local to the graph, while their stable `NodeId` is the identity used by
//! exposed card bindings.  Transfer therefore prepares a fresh node through
//! the existing mesh-stack command and then remaps the authored card state.

use std::collections::{BTreeSet, HashMap, HashSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID,
};
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId, NodeId};
use manifold_editing::command::Command;

const MESH_MODIFIER_TYPES: &[&str] = &[
    "node.bend_mesh",
    "node.twist_mesh",
    "node.taper_mesh",
    "node.push_along_normals",
    "node.push_mesh",
    "node.morph_mesh",
    "node.rotate_3d",
];

#[derive(Debug, Clone)]
pub(crate) struct ObjectModifierClipboard {
    host: Box<PresetInstance>,
    graph: Box<EffectGraphDef>,
    owner_id: u32,
    node_doc_id: u32,
    node: EffectGraphNode,
    scope_path: Vec<u32>,
    side_wires: Vec<EffectGraphWire>,
}

impl ObjectModifierClipboard {
    pub(crate) fn capture(
        project: &Project,
        layer: &LayerId,
        owner_id: u32,
        node_doc_id: u32,
    ) -> Result<Self, String> {
        let target = GraphTarget::Generator(layer.clone());
        let host = project
            .graph_target_owner(&target)
            .ok_or("Generator is no longer available")?;
        let graph = crate::graph_target::resolve(project, &target)
            .ok_or("Generator graph is unavailable")?;
        let (scope_path, nodes, wires) = owner_level(graph, owner_id)?;
        let node = nodes
            .iter()
            .find(|node| node.id == node_doc_id)
            .cloned()
            .ok_or("Object modifier is no longer available")?;
        if !MESH_MODIFIER_TYPES.contains(&node.type_id.as_str()) {
            return Err("Selected node is not an object modifier".into());
        }
        if !modifier_chain(graph, owner_id)?.contains(&node_doc_id) {
            return Err("Selected node is not in the object's modifier chain".into());
        }
        if node.node_id.is_empty() {
            return Err("Object modifier has no stable identity".into());
        }
        if node.exposed_params.iter().any(|param| {
            !graph.preset_metadata.as_ref().is_some_and(|metadata| {
                metadata.bindings.iter().any(|binding| {
                    matches!(&binding.target, BindingTarget::Node { node_id, param: target_param }
                        if node_id == &node.node_id && target_param == param)
                })
            })
        }) {
            return Err("Object modifier has an unresolved custom exposure".into());
        }
        let side_wires = wires
            .iter()
            .filter(|wire| {
                (wire.to_node == node_doc_id && wire.to_port != "in")
                    || (wire.from_node == node_doc_id && wire.from_port != "out")
            })
            .cloned()
            .collect();
        Ok(Self {
            host: Box::new(host.clone()),
            graph: Box::new(graph.clone()),
            owner_id,
            node_doc_id,
            node,
            scope_path,
            side_wires,
        })
    }
}

#[derive(Debug)]
pub(crate) enum ObjectModifierAction {
    Add {
        layer_id: LayerId,
        owner_id: u32,
        type_id: String,
        after: Option<u32>,
    },
    Paste {
        layer_id: LayerId,
        owner_id: u32,
        after: Option<u32>,
        clipboard: ObjectModifierClipboard,
    },
    Duplicate {
        layer_id: LayerId,
        owner_id: u32,
        node_doc_id: u32,
    },
}

fn owner_level<'a>(
    graph: &'a EffectGraphDef,
    owner_id: u32,
) -> Result<(Vec<u32>, &'a [EffectGraphNode], &'a [EffectGraphWire]), String> {
    let owner = graph
        .nodes
        .iter()
        .find(|node| node.id == owner_id)
        .ok_or("Object modifier owner is no longer available")?;
    if owner.type_id == GROUP_TYPE_ID {
        let body = owner
            .group
            .as_deref()
            .ok_or("Object modifier owner group has no body")?;
        let mut output = body
            .nodes
            .iter()
            .filter(|node| node.type_id == GROUP_OUTPUT_TYPE_ID);
        if output.next().is_none() {
            return Err("Object modifier owner group has no output".into());
        }
        return Ok((vec![owner_id], &body.nodes, &body.wires));
    }
    if owner.type_id == "node.scene_object" {
        return Ok((Vec::new(), &graph.nodes, &graph.wires));
    }
    Err("Object modifier owner is not a scene object or object group".into())
}

pub(crate) fn modifier_node_ids(
    project: &Project,
    layer_id: &LayerId,
    owner_id: u32,
) -> Result<Vec<u32>, String> {
    let graph = crate::graph_target::resolve(project, &GraphTarget::Generator(layer_id.clone()))
        .ok_or("Generator graph is unavailable")?;
    let (_, nodes, _) = owner_level(graph, owner_id)?;
    Ok(nodes
        .iter()
        .filter(|node| MESH_MODIFIER_TYPES.contains(&node.type_id.as_str()))
        .map(|node| node.id)
        .collect())
}

fn modifier_chain(graph: &EffectGraphDef, owner_id: u32) -> Result<Vec<u32>, String> {
    let (_, nodes, wires) = owner_level(graph, owner_id)?;
    let owner = graph
        .nodes
        .iter()
        .find(|node| node.id == owner_id)
        .ok_or("Object modifier owner is no longer available")?;
    let terminal = if owner.type_id == GROUP_TYPE_ID {
        let body = owner
            .group
            .as_deref()
            .ok_or("Object modifier owner has no body")?;
        let output = body
            .nodes
            .iter()
            .find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID)
            .ok_or("Object modifier owner group has no output")?;
        wires
            .iter()
            .find(|wire| {
                wire.to_node == output.id && wire.to_port == "object" && wire.from_port == "object"
            })
            .map(|wire| wire.from_node)
            .unwrap_or(output.id)
    } else {
        owner_id
    };
    let mut cursor = wires
        .iter()
        .find(|wire| wire.to_node == terminal && wire.to_port == "vertices")
        .map(|wire| wire.from_node)
        .ok_or("Object modifier chain is malformed")?;
    let mut reverse = Vec::new();
    let mut seen = BTreeSet::new();
    while MESH_MODIFIER_TYPES.iter().any(|type_id| {
        nodes
            .iter()
            .any(|node| node.id == cursor && node.type_id == *type_id)
    }) {
        if !seen.insert(cursor) {
            return Err("Object modifier chain contains a cycle".into());
        }
        reverse.push(cursor);
        cursor = wires
            .iter()
            .find(|wire| wire.to_node == cursor && wire.to_port == "in")
            .map(|wire| wire.from_node)
            .ok_or("Object modifier chain is malformed")?;
    }
    reverse.reverse();
    Ok(reverse)
}

fn insertion_position(
    graph: &EffectGraphDef,
    owner_id: u32,
    after: Option<u32>,
) -> Result<Option<usize>, String> {
    let chain = modifier_chain(graph, owner_id)?;
    match after {
        None => Ok(None),
        Some(after) => chain
            .iter()
            .position(|id| *id == after)
            .map(|position| Some(position + 1))
            .ok_or_else(|| "Object modifier insertion point is no longer available".into()),
    }
}

fn node_at_scope_mut<'a>(
    graph: &'a mut EffectGraphDef,
    scope_path: &[u32],
    node_id: u32,
) -> Option<&'a mut EffectGraphNode> {
    let mut nodes = &mut graph.nodes;
    for scope in scope_path {
        nodes = nodes
            .iter_mut()
            .find(|node| node.id == *scope)?
            .group
            .as_deref_mut()
            .map(|group| &mut group.nodes)?;
    }
    nodes.iter_mut().find(|node| node.id == node_id)
}

fn node_at_scope<'a>(
    graph: &'a EffectGraphDef,
    scope_path: &[u32],
    node_id: u32,
) -> Option<&'a EffectGraphNode> {
    let mut nodes = graph.nodes.as_slice();
    for scope in scope_path {
        nodes = nodes
            .iter()
            .find(|node| node.id == *scope)?
            .group
            .as_deref()?
            .nodes
            .as_slice();
    }
    nodes.iter().find(|node| node.id == node_id)
}

fn unique_id(existing: &HashSet<String>, base: String) -> String {
    if !existing.contains(&base) {
        return base;
    }
    (1..)
        .map(|n| format!("{base}_{n}"))
        .find(|candidate| !existing.contains(candidate))
        .expect("unbounded parameter ID space")
}

fn transfer_node_metadata(
    source_graph: &EffectGraphDef,
    destination_graph: &mut EffectGraphDef,
    source_node_id: &NodeId,
    destination_node_id: &NodeId,
    destination_doc_id: u32,
) -> Result<HashMap<String, String>, String> {
    let Some(source_metadata) = source_graph.preset_metadata.as_ref() else {
        return Ok(HashMap::new());
    };
    let Some(destination_metadata) = destination_graph.preset_metadata.as_mut() else {
        return Err("Destination scene metadata is unavailable".into());
    };
    let mut selected_bindings = Vec::new();
    selected_bindings.extend(source_metadata.bindings.iter().filter(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id, .. } if node_id == source_node_id)
    }).cloned());
    let mut selected_string_bindings = Vec::new();
    selected_string_bindings.extend(source_metadata.string_bindings.iter().filter(|binding| {
        matches!(&binding.target, BindingTarget::Node { node_id, .. } if node_id == source_node_id)
    }).cloned());
    if selected_bindings.is_empty() && selected_string_bindings.is_empty() {
        return Ok(HashMap::new());
    }

    let selected_ids: HashSet<String> = selected_bindings
        .iter()
        .map(|binding| binding.id.clone())
        .chain(
            selected_string_bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .collect();
    let mut existing: HashSet<String> = destination_metadata
        .params
        .iter()
        .map(|param| param.id.clone())
        .chain(
            destination_metadata
                .string_params
                .iter()
                .map(|param| param.id.clone()),
        )
        .chain(
            destination_metadata
                .bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .chain(
            destination_metadata
                .string_bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .collect();
    let destination_binding_ids: HashSet<String> = destination_metadata
        .bindings
        .iter()
        .filter(|binding| matches!(&binding.target, BindingTarget::Node { node_id, .. } if node_id == destination_node_id))
        .map(|binding| binding.id.clone())
        .chain(destination_metadata.string_bindings.iter().filter(|binding| matches!(&binding.target, BindingTarget::Node { node_id, .. } if node_id == destination_node_id)).map(|binding| binding.id.clone()))
        .collect();
    // The insertion command has already stamped the destination section.
    // Preserve it when replacing generated specs with copied authored specs;
    // the source section may identify a different object.
    let destination_sections_by_target: HashMap<String, Option<String>> = destination_metadata
        .bindings
        .iter()
        .filter_map(|binding| {
            let BindingTarget::Node { node_id, param } = &binding.target else { return None };
            (node_id == destination_node_id).then(|| {
                let section = destination_metadata
                    .params
                    .iter()
                    .find(|spec| spec.id == binding.id)
                    .map(|spec| spec.section.clone())?;
                Some((param.clone(), section))
            })?
        })
        .collect();
    let source_sections_by_binding: HashMap<String, Option<String>> = selected_bindings
        .iter()
        .filter_map(|binding| {
            let BindingTarget::Node { param, .. } = &binding.target else { return None };
            Some((
                binding.id.clone(),
                destination_sections_by_target.get(param).cloned().unwrap_or(None),
            ))
        })
        .collect();
    destination_metadata
        .bindings
        .retain(|binding| !destination_binding_ids.contains(&binding.id));
    destination_metadata
        .string_bindings
        .retain(|binding| !destination_binding_ids.contains(&binding.id));
    destination_metadata
        .params
        .retain(|param| !destination_binding_ids.contains(&param.id));
    destination_metadata
        .string_params
        .retain(|param| !destination_binding_ids.contains(&param.id));
    for id in &destination_binding_ids {
        existing.remove(id);
    }

    let mut remap = HashMap::new();
    for source_id in &selected_ids {
        let new_id = unique_id(&existing, format!("{}_{}", destination_doc_id, source_id));
        existing.insert(new_id.clone());
        remap.insert(source_id.clone(), new_id);
    }
    for param in &source_metadata.params {
        if let Some(new_id) = remap.get(&param.id) {
            let mut copy = param.clone();
            copy.id = new_id.clone();
            if let Some(section) = source_sections_by_binding.get(&param.id) {
                copy.section = section.clone();
            }
            destination_metadata.params.push(copy);
        }
    }
    for param in &source_metadata.string_params {
        if let Some(new_id) = remap.get(&param.id) {
            let mut copy = param.clone();
            copy.id = new_id.clone();
            destination_metadata.string_params.push(copy);
        }
    }
    for mut binding in selected_bindings {
        binding.id = remap
            .get(&binding.id)
            .cloned()
            .expect("binding ID was remapped");
        binding.target = BindingTarget::Node {
            node_id: destination_node_id.clone(),
            param: match binding.target {
                BindingTarget::Node { param, .. } => param,
                _ => unreachable!(),
            },
        };
        destination_metadata.bindings.push(binding);
    }
    for mut binding in selected_string_bindings {
        binding.id = remap
            .get(&binding.id)
            .cloned()
            .expect("binding ID was remapped");
        binding.target = BindingTarget::Node {
            node_id: destination_node_id.clone(),
            param: match binding.target {
                BindingTarget::Node { param, .. } => param,
                _ => unreachable!(),
            },
        };
        destination_metadata.string_bindings.push(binding);
    }
    Ok(remap)
}

fn remap_routes(
    source: &PresetInstance,
    destination: &mut PresetInstance,
    remap: &HashMap<String, String>,
) {
    for (source_id, destination_id) in remap {
        if let (Some(source_param), Some(destination_param)) = (
            source.params.get(source_id),
            destination.params.get_mut(destination_id),
        ) {
            let spec = destination_param.spec.clone();
            *destination_param = source_param.clone();
            destination_param.spec = spec;
        }
    }
    let remaps: Vec<_> = remap
        .iter()
        .map(|(source_id, destination_id)| (source_id.clone(), destination_id.clone()))
        .collect();
    crate::scene_modifier_transfer::copy_host_routes(source, destination, &remaps, false);
}

fn build_transfer(
    project: &Project,
    layer_id: LayerId,
    owner_id: u32,
    after: Option<u32>,
    clipboard: ObjectModifierClipboard,
    description: &'static str,
) -> Result<Box<dyn Command>, String> {
    let target = GraphTarget::Generator(layer_id.clone());
    let before = project
        .graph_target_owner(&target)
        .ok_or("Generator is no longer available")?
        .clone();
    let destination_graph =
        crate::graph_target::resolve(project, &target).ok_or("Generator graph is unavailable")?;
    let position = insertion_position(destination_graph, owner_id, after)?;
    let (destination_scope, destination_nodes, _) = owner_level(destination_graph, owner_id)?;
    let destination_before_ids: HashSet<u32> =
        destination_nodes.iter().map(|node| node.id).collect();
    let (_, layer) = project
        .timeline
        .find_layer_by_id(&layer_id)
        .ok_or("Generator layer is no longer available")?;
    let mut scratch = Project::default();
    scratch.timeline.layers.push(layer.clone());
    let default = crate::graph_target::owner_default(project, &target)
        .ok_or("Generator preset is unavailable")?;
    let mut insert = manifold_editing::commands::graph::InsertMeshModifierCommand::new(
        target.clone(),
        Vec::new(),
        owner_id,
        clipboard.node.type_id.clone(),
        position,
        manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
            &clipboard.node.type_id,
        ),
        default,
    );
    insert.execute(&mut scratch);
    if let Some(reason) = insert.rejection_reason() {
        return Err(reason.to_string());
    }
    if !insert.was_applied() {
        return Err("Object modifier insertion was rejected".into());
    }
    let (_, scratch_layer) = scratch
        .timeline
        .find_layer_by_id(&layer_id)
        .ok_or("Scratch generator layer is unavailable")?;
    let mut after_host = scratch_layer
        .gen_params()
        .ok_or("Scratch generator is unavailable")?
        .clone();
    let mut after_graph = after_host
        .graph
        .clone()
        .ok_or("Scratch generator graph is unavailable")?;
    let (_, source_nodes, _) = owner_level(&clipboard.graph, clipboard.owner_id)?;
    let (_, destination_nodes, _) = owner_level(&after_graph, owner_id)?;
    let inserted_id = destination_nodes
        .iter()
        .find(|node| !destination_before_ids.contains(&node.id))
        .map(|node| node.id)
        .ok_or("Inserted object modifier cannot be resolved")?;
    let inserted_node_id = node_at_scope(&after_graph, &destination_scope, inserted_id)
        .map(|node| node.node_id.clone())
        .ok_or("Inserted object modifier identity is unavailable")?;
    let destination_scope_matches = clipboard.scope_path == destination_scope;
    let mut side_wire_remap = HashMap::new();
    if !clipboard.side_wires.is_empty() {
        if !destination_scope_matches {
            return Err(
                "Object modifier has external graph inputs unavailable in the destination scope"
                    .into(),
            );
        }
        let destination_stable_ids: HashSet<NodeId> = destination_nodes
            .iter()
            .map(|node| node.node_id.clone())
            .collect();
        let source_nodes_by_doc: HashMap<u32, NodeId> = source_nodes
            .iter()
            .map(|node| (node.id, node.node_id.clone()))
            .collect();
        if clipboard.side_wires.iter().any(|wire| {
            [wire.from_node, wire.to_node].into_iter().any(|doc_id| {
                doc_id != clipboard.node_doc_id
                    && source_nodes_by_doc
                        .get(&doc_id)
                        .is_none_or(|stable_id| !destination_stable_ids.contains(stable_id))
            })
        }) {
            return Err(
                "Object modifier has external graph inputs unavailable in the destination scope"
                    .into(),
            );
        }
        let destination_by_stable: HashMap<NodeId, u32> = destination_nodes
            .iter()
            .map(|node| (node.node_id.clone(), node.id))
            .collect();
        for (source_doc_id, source_stable_id) in source_nodes_by_doc {
            if source_doc_id == clipboard.node_doc_id {
                side_wire_remap.insert(source_doc_id, inserted_id);
            } else if let Some(destination_doc_id) = destination_by_stable.get(&source_stable_id) {
                side_wire_remap.insert(source_doc_id, *destination_doc_id);
            }
        }
    }
    if let Some(node) = node_at_scope_mut(&mut after_graph, &destination_scope, inserted_id) {
        let stable_id = node.node_id.clone();
        *node = clipboard.node.clone();
        node.id = inserted_id;
        node.node_id = stable_id;
    } else {
        return Err("Inserted object modifier cannot be resolved".into());
    }
    let remap = transfer_node_metadata(
        &clipboard.graph,
        &mut after_graph,
        &clipboard.node.node_id,
        &inserted_node_id,
        inserted_id,
    )?;
    if !clipboard.side_wires.is_empty() {
        // Side wires are validated above. The mesh command owns the normal
        // `in`/`out` chain; side ports are copied into the destination level
        // below after the node has a fresh document identity.
        let target_wires = if destination_scope.is_empty() {
            &mut after_graph.wires
        } else {
            &mut after_graph
                .nodes
                .iter_mut()
                .find(|node| node.id == destination_scope[0])
                .and_then(|node| node.group.as_deref_mut())
                .ok_or("Object modifier destination scope is unavailable")?
                .wires
        };
        for mut wire in clipboard.side_wires.clone() {
            wire.from_node = side_wire_remap
                .get(&wire.from_node)
                .copied()
                .ok_or("Object modifier side input is unavailable in the destination scope")?;
            wire.to_node = side_wire_remap
                .get(&wire.to_node)
                .copied()
                .ok_or("Object modifier side input is unavailable in the destination scope")?;
            if !target_wires.contains(&wire) {
                target_wires.push(wire);
            }
        }
    }
    after_host.graph = Some(after_graph);
    after_host.refresh_manifest_from_graph();
    remap_routes(&clipboard.host, &mut after_host, &remap);
    Ok(Box::new(
        crate::generator_change::ReplaceGeneratorStateCommand::new(
            layer_id,
            before,
            after_host,
            description,
        ),
    ))
}

pub(crate) fn build_action(
    project: &Project,
    action: ObjectModifierAction,
) -> Result<Box<dyn Command>, String> {
    match action {
        ObjectModifierAction::Paste {
            layer_id,
            owner_id,
            after,
            clipboard,
        } => build_transfer(
            project,
            layer_id,
            owner_id,
            after,
            clipboard,
            "Paste Object Modifier",
        ),
        ObjectModifierAction::Duplicate {
            layer_id,
            owner_id,
            node_doc_id,
        } => {
            let clipboard =
                ObjectModifierClipboard::capture(project, &layer_id, owner_id, node_doc_id)?;
            build_transfer(
                project,
                layer_id,
                owner_id,
                Some(node_doc_id),
                clipboard,
                "Duplicate Object Modifier",
            )
        }
        ObjectModifierAction::Add {
            layer_id,
            owner_id,
            type_id,
            after,
        } => {
            let target = GraphTarget::Generator(layer_id.clone());
            let before = project
                .graph_target_owner(&target)
                .ok_or("Generator is no longer available")?
                .clone();
            let source_graph = crate::graph_target::resolve(project, &target)
                .ok_or("Generator graph is unavailable")?;
            let position = insertion_position(source_graph, owner_id, after)?;
            let (_, layer) = project
                .timeline
                .find_layer_by_id(&layer_id)
                .ok_or("Generator layer is no longer available")?;
            let mut scratch = Project::default();
            scratch.timeline.layers.push(layer.clone());
            let default = crate::graph_target::owner_default(project, &target)
                .ok_or("Generator preset is unavailable")?;
            let mut command = manifold_editing::commands::graph::InsertMeshModifierCommand::new(
                target,
                Vec::new(),
                owner_id,
                type_id.clone(),
                position,
                manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(&type_id),
                default,
            );
            command.execute(&mut scratch);
            if let Some(reason) = command.rejection_reason() {
                return Err(reason.to_string());
            }
            let (_, scratch_layer) = scratch
                .timeline
                .find_layer_by_id(&layer_id)
                .ok_or("Scratch generator layer is unavailable")?;
            let after = scratch_layer
                .gen_params()
                .ok_or("Scratch generator is unavailable")?
                .clone();
            Ok(Box::new(
                crate::generator_change::ReplaceGeneratorStateCommand::new(
                    layer_id,
                    before,
                    after,
                    "Add Object Modifier",
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::effects::{AutomationLane, AutomationPoint, ParamEnvelope, ParameterDriver};
    use manifold_core::layer::Layer;
    use manifold_core::types::{BeatDivision, DriverWaveform};
    use manifold_core::{AudioSendId, Beats, PresetTypeId};
    use manifold_editing::command::Command;
    use manifold_editing::service::EditingService;

    fn fixture() -> (Project, LayerId, Vec<u32>) {
        let mut project = Project::default();
        let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::new("SceneStarter"), 0);
        let graph =
            manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("SceneStarter"))
                .expect("SceneStarter resolves")
                .clone();
        let layer_id = layer.layer_id.clone();
        layer.gen_params_or_init().graph = Some(graph);
        layer.gen_params_or_init().refresh_manifest_from_graph();
        project.timeline.layers.push(layer);
        let graph =
            crate::graph_target::resolve(&project, &GraphTarget::Generator(layer_id.clone()))
                .expect("fixture graph");
        let owners = graph
            .nodes
            .iter()
            .filter(|node| node.type_id == GROUP_TYPE_ID)
            .map(|node| node.id)
            .collect();
        (project, layer_id, owners)
    }

    fn insert_modifier(project: &mut Project, layer_id: &LayerId, owner_id: u32) -> u32 {
        let target = GraphTarget::Generator(layer_id.clone());
        let default = crate::graph_target::owner_default(project, &target).unwrap();
        let mut command = manifold_editing::commands::graph::InsertMeshModifierCommand::new(
            target,
            Vec::new(),
            owner_id,
            "node.twist_mesh".into(),
            None,
            manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                "node.twist_mesh",
            ),
            default,
        );
        command.execute(project);
        assert!(command.was_applied(), "fixture insertion should apply");
        let graph =
            crate::graph_target::resolve(project, &GraphTarget::Generator(layer_id.clone()))
                .unwrap();
        graph
            .nodes
            .iter()
            .find(|node| node.id == owner_id)
            .and_then(|owner| owner.group.as_deref())
            .and_then(|group| {
                group
                    .nodes
                    .iter()
                    .find(|node| node.type_id == "node.twist_mesh")
            })
            .map(|node| node.id)
            .expect("inserted modifier")
    }

    fn binding_for(project: &Project, layer_id: &LayerId, node_id: &NodeId) -> (String, String) {
        let graph =
            crate::graph_target::resolve(project, &GraphTarget::Generator(layer_id.clone()))
                .unwrap();
        graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .find_map(|binding| match &binding.target {
                BindingTarget::Node {
                    node_id: target_id,
                    param,
                } if target_id == node_id => Some((binding.id.clone(), param.clone())),
                _ => None,
            })
            .expect("inserted modifier exposure")
    }

    #[test]
    fn duplicate_copies_authored_state_and_has_exact_undo_redo() {
        let (mut project, layer_id, owners) = fixture();
        let owner_id = owners[0];
        let node_doc_id = insert_modifier(&mut project, &layer_id, owner_id);
        let graph =
            crate::graph_target::resolve(&project, &GraphTarget::Generator(layer_id.clone()))
                .unwrap();
        let source_node_id = owner_level(graph, owner_id)
            .unwrap()
            .1
            .iter()
            .find(|node| node.id == node_doc_id)
            .unwrap()
            .node_id
            .clone();
        let (param_id, _) = binding_for(&project, &layer_id, &source_node_id);
        let host = project
            .graph_target_owner_mut(&GraphTarget::Generator(layer_id.clone()))
            .unwrap();
        host.params.get_mut(&param_id).unwrap().value = 0.42;
        host.params.get_mut(&param_id).unwrap().base = 0.37;
        host.drivers = Some(vec![ParameterDriver::new(
            param_id.clone(),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
        host.envelopes = Some(vec![ParamEnvelope::new(param_id.clone())]);
        host.audio_mods = Some(vec![ParameterAudioMod::new(
            param_id.clone().into(),
            AudioSendId::new("send"),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        )]);
        host.automation_lanes = Some(vec![AutomationLane {
            param_id: param_id.clone().into(),
            enabled: true,
            points: vec![AutomationPoint {
                beat: Beats(2.0),
                value: 0.81,
                shape: manifold_core::effects::SegmentShape::Linear,
            }],
        }]);
        let before = serde_json::to_value(host).unwrap();

        let command = build_action(
            &project,
            ObjectModifierAction::Duplicate {
                layer_id: layer_id.clone(),
                owner_id,
                node_doc_id,
            },
        )
        .unwrap();
        assert_eq!(command.description(), "Duplicate Object Modifier");
        let mut editing = EditingService::new();
        editing.execute(
            crate::scene_modifier_edit::with_admission(command),
            &mut project,
        );
        assert!(editing.take_rejection().is_none());
        let after_host = project
            .graph_target_owner(&GraphTarget::Generator(layer_id.clone()))
            .unwrap();
        let after = serde_json::to_value(after_host).unwrap();
        let (_, nodes, _) = owner_level(after_host.graph.as_ref().unwrap(), owner_id).unwrap();
        let copied_nodes: Vec<_> = nodes
            .iter()
            .filter(|node| node.type_id == "node.twist_mesh")
            .collect();
        assert_eq!(copied_nodes.len(), 2);
        assert_ne!(copied_nodes[0].node_id, copied_nodes[1].node_id);
        assert!(
            after_host
                .drivers
                .as_ref()
                .unwrap()
                .iter()
                .any(|driver| driver.param_id.as_ref() != param_id)
        );
        assert!(editing.undo(&mut project));
        assert_eq!(
            serde_json::to_value(
                project
                    .graph_target_owner(&GraphTarget::Generator(layer_id.clone()))
                    .unwrap()
            )
            .unwrap(),
            before
        );
        assert!(editing.redo(&mut project));
        assert!(editing.take_rejection().is_none());
        assert_eq!(
            serde_json::to_value(
                project
                    .graph_target_owner(&GraphTarget::Generator(layer_id))
                    .unwrap()
            )
            .unwrap(),
            after
        );
    }

    #[test]
    fn paste_supports_another_object_and_layer_from_a_cut_snapshot() {
        let (mut project, source_layer, owners) = fixture();
        let source_owner = owners[0];
        let destination_owner = owners[1];
        let source_doc_id = insert_modifier(&mut project, &source_layer, source_owner);
        let clipboard =
            ObjectModifierClipboard::capture(&project, &source_layer, source_owner, source_doc_id)
                .unwrap();
        let mut destination =
            Layer::new_generator("Other layer".into(), PresetTypeId::new("SceneStarter"), 1);
        destination.gen_params_or_init().graph = Some(
            manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("SceneStarter"))
                .unwrap()
                .clone(),
        );
        destination
            .gen_params_or_init()
            .refresh_manifest_from_graph();
        let destination_layer = destination.layer_id.clone();
        project.timeline.layers.push(destination);
        // Model Cut removing the source card after capture. The clipboard must
        // retain its authored graph and host snapshots independently.
        let source_host = project
            .graph_target_owner_mut(&GraphTarget::Generator(source_layer.clone()))
            .unwrap();
        let source_graph = source_host.graph.as_mut().unwrap();
        let source_group = source_graph
            .nodes
            .iter_mut()
            .find(|node| node.id == source_owner)
            .unwrap()
            .group
            .as_deref_mut()
            .unwrap();
        source_group.nodes.retain(|node| node.id != source_doc_id);
        source_group
            .wires
            .retain(|wire| wire.from_node != source_doc_id && wire.to_node != source_doc_id);

        let command = build_action(
            &project,
            ObjectModifierAction::Paste {
                layer_id: destination_layer.clone(),
                owner_id: destination_owner,
                after: None,
                clipboard,
            },
        )
        .unwrap();
        assert_eq!(command.description(), "Paste Object Modifier");
        let mut editing = EditingService::new();
        editing.execute(
            crate::scene_modifier_edit::with_admission(command),
            &mut project,
        );
        assert!(editing.take_rejection().is_none());
        let destination_graph =
            crate::graph_target::resolve(&project, &GraphTarget::Generator(destination_layer.clone()))
                .unwrap();
        let (_, nodes, _) = owner_level(destination_graph, destination_owner).unwrap();
        let copied = nodes
            .iter()
            .find(|node| node.type_id == "node.twist_mesh")
            .expect("pasted modifier");
        let (binding_id, _) = binding_for(&project, &destination_layer, &copied.node_id);
        let copied_spec = destination_graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .find(|spec| spec.id == binding_id)
            .expect("pasted modifier metadata");
        let destination_name = destination_graph
            .nodes
            .iter()
            .find(|node| node.id == destination_owner)
            .and_then(|node| node.handle.clone())
            .expect("destination object name");
        let expected_section = format!("{destination_name} — Twist_mesh");
        assert_eq!(copied_spec.section.as_deref(), Some(expected_section.as_str()));
    }

    #[test]
    fn capture_rejects_invalid_owner_unconnected_modifier_and_missing_side_input() {
        let (mut project, layer_id, owners) = fixture();
        let owner_id = owners[0];
        let node_doc_id = insert_modifier(&mut project, &layer_id, owner_id);
        assert!(
            ObjectModifierClipboard::capture(&project, &layer_id, 999_999, node_doc_id).is_err()
        );

        let graph = project
            .graph_target_owner_mut(&GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph
            .as_mut()
            .unwrap();
        let group = graph
            .nodes
            .iter_mut()
            .find(|node| node.id == owner_id)
            .unwrap()
            .group
            .as_deref_mut()
            .unwrap();
        let mut disconnected = group
            .nodes
            .iter()
            .find(|node| node.id == node_doc_id)
            .unwrap()
            .clone();
        disconnected.id = 900_000;
        disconnected.node_id = NodeId::new("disconnected");
        group.nodes.push(disconnected);
        assert!(
            ObjectModifierClipboard::capture(&project, &layer_id, owner_id, 900_000)
                .unwrap_err()
                .contains("modifier chain")
        );

        let mut clipboard =
            ObjectModifierClipboard::capture(&project, &layer_id, owner_id, node_doc_id).unwrap();
        clipboard.side_wires.push(EffectGraphWire {
            from_node: 1_234_567,
            from_port: "value".into(),
            to_node: clipboard.node_doc_id,
            to_port: "amount".into(),
        });
        assert!(
            build_action(
                &project,
                ObjectModifierAction::Paste {
                    layer_id,
                    owner_id,
                    after: None,
                    clipboard,
                },
            )
            .unwrap_err()
            .contains("external graph inputs")
        );
    }
}
