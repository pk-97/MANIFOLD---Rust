//! Scene items use immutable clipboard snapshots and the same atomic generator
//! replacement as modifier cards. Only the content thread executes edits.
use std::collections::{HashMap, HashSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID, SerializedParamValue,
};
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId, NodeId};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::SceneFluidRoleAssignment;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SceneItemKind {
    Object,
    Light,
}

impl SceneItemKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Object => "object_",
            Self::Light => "light_",
        }
    }
    fn count_param(self) -> &'static str {
        match self {
            Self::Object => "objects",
            Self::Light => "lights",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SceneItemClipboard {
    host: Box<PresetInstance>,
    graph: Box<EffectGraphDef>,
    root: u32,
    port: String,
    pub(crate) kind: SceneItemKind,
    fluid_role_group: Option<u32>,
    fluid_role_assignments: Vec<SceneFluidRoleAssignment>,
}

fn visit_nodes(nodes: &[EffectGraphNode], f: &mut impl FnMut(&EffectGraphNode)) {
    for node in nodes {
        f(node);
        if let Some(group) = &node.group {
            visit_nodes(&group.nodes, f);
        }
    }
}

impl SceneItemClipboard {
    pub(crate) fn capture(
        project: &Project,
        layer: &LayerId,
        scene: u32,
        kind: SceneItemKind,
        index: u32,
    ) -> Result<Self, String> {
        let target = GraphTarget::Generator(layer.clone());
        let host = project
            .graph_target_owner(&target)
            .ok_or("Scene is no longer available")?;
        let source =
            crate::graph_target::resolve(project, &target).ok_or("Scene graph is unavailable")?;
        let output = source
            .wires
            .iter()
            .find(|wire| {
                wire.to_node == scene && wire.to_port == format!("{}{index}", kind.prefix())
            })
            .ok_or("Selected scene item is no longer available")?;
        let fluid_role_group = source
            .nodes
            .iter()
            .find(|node| node.id == output.from_node && node.type_id == GROUP_TYPE_ID)
            .map(|node| node.id);
        if let Some(group_id) = fluid_role_group
            && let Some(group) = source.nodes.iter().find(|node| node.id == group_id)
            && let Some(group) = &group.group
        {
            let object_outputs = group
                .interface
                .outputs
                .iter()
                .filter(|port| port.name.starts_with("object"))
                .count();
            if object_outputs > 1 {
                return Err("Groups with several objects cannot be copied yet; duplicate an imported model with its ⧉ button".into());
            }
        }
        let fluid_role_assignments = fluid_role_group
            .map(|group| {
                manifold_editing::commands::graph::scene_fluid_role_assignments(source, group)
            })
            .transpose()?
            .unwrap_or_default();
        // Walk the actual upstream graph, including shared material, animation,
        // mesh and map producers. Physics worlds contribute only the body slots
        // used by this item, never all the other objects in the simulation.
        let mut pending = vec![(output.from_node, output.from_port.clone())];
        let mut outputs = HashSet::new();
        let mut ids = HashSet::new();
        let mut captured_wires = Vec::new();
        while let Some((id, port)) = pending.pop() {
            if !outputs.insert((id, port.clone())) {
                continue;
            }
            let node = source
                .nodes
                .iter()
                .find(|node| node.id == id)
                .ok_or("Scene item has a missing dependency")?;
            if node.type_id == "node.render_scene" {
                return Err("Scene feedback must be copied in the graph editor".into());
            }
            ids.insert(id);
            let physics_slot = if node.type_id == "node.physics_world" {
                let slot = port
                    .strip_prefix("pose_")
                    .ok_or("Unsupported physics output on scene item")?
                    .to_string();
                let body_port = format!("body_{slot}");
                let body_inputs: Vec<_> = source
                    .wires
                    .iter()
                    .filter(|wire| wire.to_node == id && wire.to_port == body_port)
                    .collect();
                if body_inputs.len() != 1 {
                    return Err("Selected Physics World body slot is missing or ambiguous".into());
                }
                Some(slot)
            } else {
                None
            };
            for wire in source.wires.iter().filter(|wire| wire.to_node == id) {
                if let Some(slot) = &physics_slot
                    && wire.to_port.starts_with("body_")
                    && wire.to_port != format!("body_{slot}")
                {
                    continue;
                }
                if !captured_wires.contains(wire) {
                    captured_wires.push(wire.clone());
                }
                pending.push((wire.from_node, wire.from_port.clone()));
            }
        }
        let mut graph = source.clone();
        graph.nodes.retain(|node| ids.contains(&node.id));
        graph.wires = captured_wires;
        graph.scene_modifiers.clear();
        Ok(Self {
            host: Box::new(host.clone()),
            graph: Box::new(graph),
            root: output.from_node,
            port: output.from_port.clone(),
            kind,
            fluid_role_group,
            fluid_role_assignments,
        })
    }
}

#[derive(Debug)]
pub(crate) enum SceneItemAction {
    Paste {
        layer: LayerId,
        scene: u32,
        clipboard: Box<SceneItemClipboard>,
    },
    Duplicate {
        layer: LayerId,
        scene: u32,
        kind: SceneItemKind,
        index: u32,
    },
    Move {
        layer: LayerId,
        scene: u32,
        kind: SceneItemKind,
        index: u32,
        delta: i32,
    },
}

impl SceneItemAction {
    pub(crate) fn selection_request(&self) -> Option<crate::edit_selection::SelectAfterEdit> {
        let (layer, kind) = match self {
            Self::Paste { layer, clipboard, .. } => (layer, clipboard.kind),
            Self::Duplicate { layer, kind, .. } => (layer, *kind),
            Self::Move { .. } => return None,
        };
        Some(match kind {
            SceneItemKind::Object => crate::edit_selection::SelectAfterEdit::NewObject(layer.clone()),
            SceneItemKind::Light => crate::edit_selection::SelectAfterEdit::NewLight(layer.clone()),
        })
    }
}

fn remapped_target(target: &BindingTarget, ids: &HashMap<NodeId, NodeId>) -> Option<BindingTarget> {
    let BindingTarget::Node { node_id, param } = target else {
        return None;
    };
    Some(BindingTarget::Node {
        node_id: ids.get(node_id)?.clone(),
        param: param.clone(),
    })
}

fn unique(existing: &mut HashSet<String>, base: &str) -> String {
    let mut value = base.to_string();
    let mut suffix = 2;
    while !existing.insert(value.clone()) {
        value = format!("{base}_{suffix}");
        suffix += 1;
    }
    value
}

fn node_by_stable_id<'a>(nodes: &'a [EffectGraphNode], id: &NodeId) -> Option<&'a EffectGraphNode> {
    for node in nodes {
        if node.node_id == *id {
            return Some(node);
        }
        if let Some(group) = &node.group
            && let Some(found) = node_by_stable_id(&group.nodes, id)
        {
            return Some(found);
        }
    }
    None
}

fn cloned_handle_rewrites(
    source: &EffectGraphDef,
    destination: &EffectGraphDef,
    ids: &HashMap<NodeId, NodeId>,
) -> Vec<(String, String)> {
    let mut rewrites = HashMap::new();
    visit_nodes(&source.nodes, &mut |node| {
        let Some(old_handle) = node.handle.as_deref() else {
            return;
        };
        let Some(new_id) = ids.get(&node.node_id) else {
            return;
        };
        let Some(new_node) = node_by_stable_id(&destination.nodes, new_id) else {
            return;
        };
        let Some(new_handle) = new_node.handle.as_deref() else {
            return;
        };
        if old_handle != new_handle {
            // Groups may share their authored label with the inner scene
            // object. The inner object's label is the outliner's display name.
            rewrites.insert(old_handle.to_owned(), new_handle.to_owned());
        }
    });
    let mut rewrites: Vec<_> = rewrites.into_iter().collect();
    rewrites.sort_by_key(|(old, _)| (std::cmp::Reverse(old.len()), old.clone()));
    rewrites
}

fn rewrite_label(label: String, rewrites: &[(String, String)]) -> String {
    for (old, new) in rewrites {
        if label.contains(old) {
            // A copied handle contains its source as a prefix. Rewriting the
            // result again would append a second copy suffix to section labels.
            return label.replace(old, new);
        }
    }
    label
}

fn transfer_metadata(
    source: &EffectGraphDef,
    destination: &mut EffectGraphDef,
    ids: &HashMap<NodeId, NodeId>,
    handle_rewrites: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    let Some(source) = &source.preset_metadata else {
        return Ok(Vec::new());
    };
    let dest = destination
        .preset_metadata
        .as_mut()
        .ok_or("Destination scene has no parameter metadata")?;
    let mut taken: HashSet<String> = dest
        .params
        .iter()
        .map(|p| p.id.clone())
        .chain(dest.string_params.iter().map(|p| p.id.clone()))
        .collect();
    // The shared scene projection resolves cloned bindings by stable target
    // when their IDs carry the existing `_duplicate` suffix convention.
    let mut remaps = HashMap::new();
    for binding in &source.bindings {
        if let Some(target) = remapped_target(&binding.target, ids) {
            let id = remaps
                .entry(binding.id.clone())
                .or_insert_with(|| unique(&mut taken, &format!("{}_duplicate", binding.id)))
                .clone();
            let mut copy = binding.clone();
            copy.id = id;
            copy.target = target;
            copy.label = rewrite_label(copy.label, handle_rewrites);
            dest.bindings.push(copy);
        }
    }
    for binding in &source.string_bindings {
        if let Some(target) = remapped_target(&binding.target, ids) {
            let id = remaps
                .entry(binding.id.clone())
                .or_insert_with(|| unique(&mut taken, &format!("{}_duplicate", binding.id)))
                .clone();
            let mut copy = binding.clone();
            copy.id = id;
            copy.target = target;
            copy.label = rewrite_label(copy.label, handle_rewrites);
            dest.string_bindings.push(copy);
        }
    }
    for spec in &source.params {
        if let Some(id) = remaps.get(&spec.id) {
            let mut copy = spec.clone();
            copy.id = id.clone();
            copy.section = copy
                .section
                .map(|section| rewrite_label(section, handle_rewrites));
            dest.params.push(copy);
        }
    }
    for spec in &source.string_params {
        if let Some(id) = remaps.get(&spec.id) {
            let mut copy = spec.clone();
            copy.id = id.clone();
            copy.name = rewrite_label(copy.name, handle_rewrites);
            dest.string_params.push(copy);
        }
    }
    Ok(remaps.into_iter().collect())
}

fn paste(
    project: &Project,
    layer: LayerId,
    scene: u32,
    clipboard: SceneItemClipboard,
    description: &'static str,
) -> Result<Box<dyn Command>, String> {
    let target = GraphTarget::Generator(layer.clone());
    let before = project
        .graph_target_owner(&target)
        .ok_or("Destination scene is unavailable")?
        .clone();
    let mut graph = crate::graph_target::resolve(project, &target)
        .ok_or("Destination scene graph is unavailable")?
        .clone();
    if !graph
        .nodes
        .iter()
        .any(|node| node.id == scene && node.type_id == "node.render_scene")
    {
        return Err("Destination scene is no longer available".into());
    }
    let mut next_id = 0;
    let mut handles = HashSet::new();
    visit_nodes(&graph.nodes, &mut |node| {
        next_id = next_id.max(node.id);
        if let Some(handle) = &node.handle {
            handles.insert(handle.clone());
        }
    });
    next_id = next_id.checked_add(1).ok_or("Scene node IDs exhausted")?;
    let mut doc_ids = HashMap::new();
    let mut node_ids = Vec::new();
    let mut added = Vec::new();
    let mut physics_ports = HashMap::new();
    for source in &clipboard.graph.nodes {
        if source.type_id == "node.physics_world" {
            let worlds: Vec<_> = graph
                .nodes
                .iter()
                .filter(|node| node.type_id == "node.physics_world")
                .map(|n| n.id)
                .collect();
            if worlds.len() > 1 {
                return Err(
                    "Paste requires at most one Physics World in the destination scene".into(),
                );
            }
            if let Some(&world) = worlds.first() {
                doc_ids.insert(source.id, world);
                let mut used: HashSet<u32> = graph
                    .wires
                    .iter()
                    .filter(|w| w.to_node == world)
                    .filter_map(|w| w.to_port.strip_prefix("body_")?.parse().ok())
                    .collect();
                for input in clipboard
                    .graph
                    .wires
                    .iter()
                    .filter(|w| w.to_node == source.id && w.to_port.starts_with("body_"))
                {
                    let slot = (0..16)
                        .find(|slot| !used.contains(slot))
                        .ok_or("Physics World has no free body slots")?;
                    used.insert(slot);
                    let old = input.to_port.trim_start_matches("body_");
                    for prefix in ["body_", "pose_", "instances_"] {
                        physics_ports.insert(
                            (source.id, format!("{prefix}{old}")),
                            format!("{prefix}{slot}"),
                        );
                    }
                }
                continue;
            }
        }
        if source.type_id == "system.generator_input" {
            let input = graph
                .nodes
                .iter()
                .find(|node| node.type_id == source.type_id)
                .ok_or("Destination has no generator input")?;
            doc_ids.insert(source.id, input.id);
            continue;
        }
        let clone = manifold_editing::commands::graph::deep_clone_with_fresh_ids(
            source,
            &mut next_id,
            &mut handles,
            &mut node_ids,
        );
        doc_ids.insert(source.id, clone.id);
        added.push(clone);
    }
    let node_id_map = node_ids;
    let stable: HashMap<_, _> = node_id_map.iter().cloned().collect();
    // The common clone helper deliberately clears exposures for graph copies.
    // Scene copies carry their bindings and therefore restore these flags.
    fn restore_exposures(
        source: &[EffectGraphNode],
        copies: &mut [EffectGraphNode],
        ids: &HashMap<NodeId, NodeId>,
    ) {
        let mut exposed = HashMap::new();
        visit_nodes(source, &mut |node| {
            if let Some(id) = ids.get(&node.node_id) {
                exposed.insert(id.clone(), node.exposed_params.clone());
            }
        });
        fn apply(
            nodes: &mut [EffectGraphNode],
            exposed: &HashMap<NodeId, std::collections::BTreeSet<String>>,
        ) {
            for node in nodes {
                if let Some(params) = exposed.get(&node.node_id) {
                    node.exposed_params = params.clone();
                }
                if let Some(group) = &mut node.group {
                    apply(&mut group.nodes, exposed);
                }
            }
        }
        apply(copies, &exposed);
    }
    restore_exposures(&clipboard.graph.nodes, &mut added, &stable);
    graph.nodes.extend(added);
    for wire in &clipboard.graph.wires {
        let mut copy = wire.clone();
        copy.from_node = *doc_ids
            .get(&wire.from_node)
            .ok_or("Copied source is missing")?;
        copy.to_node = *doc_ids
            .get(&wire.to_node)
            .ok_or("Copied destination is missing")?;
        if let Some(port) = physics_ports.get(&(wire.from_node, wire.from_port.clone())) {
            copy.from_port = port.clone();
        }
        if let Some(port) = physics_ports.get(&(wire.to_node, wire.to_port.clone())) {
            copy.to_port = port.clone();
        }
        // A reused world keeps its scene-wide configuration; only append bodies.
        let reused_world = clipboard
            .graph
            .nodes
            .iter()
            .any(|n| n.id == wire.to_node && n.type_id == "node.physics_world")
            && graph
                .wires
                .iter()
                .any(|w| w.to_node == copy.to_node && w.to_port == copy.to_port);
        if !reused_world {
            graph.wires.push(copy);
        }
    }
    let node = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == scene)
        .ok_or("Scene is unavailable")?;
    let count = match node.params.get(clipboard.kind.count_param()) {
        Some(SerializedParamValue::Float { value }) => *value as u32,
        Some(SerializedParamValue::Int { value }) => *value as u32,
        _ => 0,
    };
    node.params.insert(
        clipboard.kind.count_param().into(),
        SerializedParamValue::Float {
            value: (count + 1) as f32,
        },
    );
    graph.wires.push(EffectGraphWire {
        from_node: *doc_ids
            .get(&clipboard.root)
            .ok_or("Copied scene item is missing")?,
        from_port: clipboard.port,
        to_node: scene,
        to_port: format!("{}{count}", clipboard.kind.prefix()),
    });
    if !clipboard.fluid_role_assignments.is_empty() {
        let original_group = clipboard
            .fluid_role_group
            .ok_or("Copied fluid role group identity is unavailable")?;
        let cloned_group = *doc_ids
            .get(&original_group)
            .ok_or("Copied fluid role group is unavailable in destination")?;
        manifold_editing::commands::graph::restore_scene_object_fluid_roles(
            &mut graph,
            &clipboard.fluid_role_assignments,
            cloned_group,
            &node_id_map,
        )?;
    }
    let handle_rewrites = cloned_handle_rewrites(&clipboard.graph, &graph, &stable);
    let remaps = transfer_metadata(&clipboard.graph, &mut graph, &stable, &handle_rewrites)?;
    manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(&mut graph);
    let mut after = before.clone();
    after.graph = Some(graph);
    after.refresh_manifest_from_graph();
    // A copied source with tracked bases must keep that wire-level contract;
    // otherwise the copied base is silently read back as its effective value.
    let source_base_tracked = clipboard.host.base_tracked;
    if source_base_tracked && !after.base_tracked {
        for param in after.params.iter_mut() {
            param.base = param.value;
        }
    }
    after.base_tracked |= source_base_tracked;
    let copied_base_is_tracked = after.base_tracked && !source_base_tracked;
    for (old, new) in &remaps {
        if let (Some(source), Some(dest)) =
            (clipboard.host.params.get(old), after.params.get_mut(new))
        {
            let spec = dest.spec.clone();
            *dest = source.clone();
            dest.spec = spec;
            if copied_base_is_tracked {
                dest.base = dest.value;
            }
        }
    }
    crate::scene_modifier_transfer::copy_host_routes(&clipboard.host, &mut after, &remaps, false);
    Ok(Box::new(
        crate::generator_change::ReplaceGeneratorStateCommand::new(
            layer,
            before,
            after,
            description,
        ),
    ))
}

pub(crate) fn build_action(
    project: &Project,
    action: SceneItemAction,
) -> Result<Box<dyn Command>, String> {
    match action {
        SceneItemAction::Paste {
            layer,
            scene,
            clipboard,
        } => paste(project, layer, scene, *clipboard, "Paste Scene Item"),
        SceneItemAction::Duplicate {
            layer,
            scene,
            kind,
            index,
        } => {
            let clipboard = SceneItemClipboard::capture(project, &layer, scene, kind, index)?;
            paste(project, layer, scene, clipboard, "Duplicate Scene Item")
        }
        SceneItemAction::Move {
            layer,
            scene,
            kind,
            index,
            delta,
        } => {
            let target = GraphTarget::Generator(layer.clone());
            let before = project
                .graph_target_owner(&target)
                .ok_or("Scene is unavailable")?
                .clone();
            let mut graph = crate::graph_target::resolve(project, &target)
                .ok_or("Scene graph is unavailable")?
                .clone();
            let adjacent = i64::from(index) + i64::from(delta);
            if adjacent < 0 {
                return Err("Scene item is already first".into());
            }
            let from = format!("{}{index}", kind.prefix());
            let to = format!("{}{adjacent}", kind.prefix());
            if !graph
                .wires
                .iter()
                .any(|w| w.to_node == scene && w.to_port == from)
                || !graph
                    .wires
                    .iter()
                    .any(|w| w.to_node == scene && w.to_port == to)
            {
                return Err("Scene item cannot move further".into());
            }
            for wire in graph.wires.iter_mut().filter(|w| w.to_node == scene) {
                if wire.to_port == from {
                    wire.to_port = to.clone();
                } else if wire.to_port == to {
                    wire.to_port = from.clone();
                }
            }
            let mut after = before.clone();
            after.graph = Some(graph);
            Ok(Box::new(
                crate::generator_change::ReplaceGeneratorStateCommand::new(
                    layer,
                    before,
                    after,
                    "Move Scene Item",
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    use manifold_core::effect_graph_def::{
        BindingDef, EffectGraphDef, ParamSpecDef, PresetMetadata, StringBindingDef,
        StringParamSpecDef,
    };
    use manifold_core::effects::{ParamConvert, ParameterDriver};
    use manifold_core::layer::Layer;
    use manifold_core::preset_type_id::PresetTypeId;
    use manifold_core::project::Project;
    use manifold_core::types::{BeatDivision, DriverWaveform};
    use manifold_editing::service::EditingService;

    fn node(id: u32, stable: &str, type_id: &str, handle: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(stable),
            type_id: type_id.to_string(),
            handle: Some(handle.to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
        EffectGraphWire {
            from_node,
            from_port: from_port.to_string(),
            to_node,
            to_port: to_port.to_string(),
        }
    }

    fn graph(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes,
            wires,
        }
    }

    fn project_with_graph(graph: EffectGraphDef) -> (Project, LayerId) {
        let mut project = Project::default();
        let mut layer = Layer::new_generator(
            "Transfer test".to_string(),
            PresetTypeId::new("transfer_test"),
            0,
        );
        let layer_id = layer.layer_id.clone();
        layer.gen_params_or_init().graph = Some(graph);
        project.timeline.layers.push(layer);
        (project, layer_id)
    }

    fn transfer_graph(prefix: &str, include_light: bool) -> EffectGraphDef {
        let mut render = node(1, &format!("{prefix}-render"), "node.render_scene", "Scene");
        render.params.insert(
            "objects".to_string(),
            SerializedParamValue::Float { value: 1.0 },
        );
        render.params.insert(
            "lights".to_string(),
            SerializedParamValue::Float {
                value: if include_light { 1.0 } else { 0.0 },
            },
        );
        let object = node(
            2,
            &format!("{prefix}-object"),
            "node.scene_object",
            "Object 0",
        );
        let mut material = node(
            3,
            &format!("{prefix}-material"),
            "node.pbr_material",
            "Object 0 Material",
        );
        material.params.insert(
            "baked_look".to_string(),
            SerializedParamValue::Float { value: 1.0 },
        );
        material.params.insert(
            "base_color_map".to_string(),
            SerializedParamValue::String {
                value: "/textures/source-albedo.png".to_string(),
            },
        );
        let animation = node(
            4,
            &format!("{prefix}-animation"),
            "node.value",
            "Object 0 Animation",
        );
        let mut nodes = vec![render, object, material, animation];
        let mut wires = vec![
            wire(2, "object", 1, "object_0"),
            wire(3, "material", 2, "material"),
            wire(4, "value", 3, "roughness"),
        ];
        if include_light {
            nodes.push(node(5, &format!("{prefix}-light"), "node.light", "Light 0"));
            wires.push(wire(5, "light", 1, "light_0"));
        }
        let material_id = nodes
            .iter()
            .find(|node| node.type_id == "node.pbr_material")
            .map(|node| node.node_id.clone())
            .expect("material fixture node");
        let mut result = graph(nodes, wires);
        result.version = 2;
        result.preset_metadata = Some(metadata(material_id));
        result
    }

    fn add_transfer_layer(
        project: &mut Project,
        name: &str,
        prefix: &str,
        include_light: bool,
    ) -> LayerId {
        let mut layer = Layer::new_generator(
            name.to_string(),
            PresetTypeId::new("transfer_test"),
            project.timeline.layers.len() as i32,
        );
        let layer_id = layer.layer_id.clone();
        let host = layer.gen_params_or_init();
        host.graph = Some(transfer_graph(prefix, include_light));
        host.refresh_manifest_from_graph();
        project.timeline.layers.push(layer);
        layer_id
    }

    fn material_node<'a>(graph: &'a EffectGraphDef, prefix: &str) -> &'a EffectGraphNode {
        graph
            .nodes
            .iter()
            .find(|node| node.node_id == NodeId::new(format!("{prefix}-material")))
            .expect("material node")
    }

    fn physics_graph(existing_slots: usize) -> EffectGraphDef {
        let mut nodes = vec![
            node(1, "physics-render", "node.render_scene", "Scene"),
            node(2, "physics-object", "node.scene_object", "Object 0"),
            node(3, "physics-world", "node.physics_world", "Physics World"),
            node(4, "physics-body-0", "node.rigid_body", "Body 0"),
        ];
        nodes[0]
            .params
            .insert("objects".into(), SerializedParamValue::Float { value: 1.0 });
        let mut wires = vec![
            wire(2, "object", 1, "object_0"),
            wire(3, "pose_0", 2, "transform"),
            wire(4, "body", 3, "body_0"),
        ];
        for slot in 1..existing_slots {
            let id = 4 + slot as u32;
            nodes.push(node(
                id,
                &format!("physics-body-{slot}"),
                "node.rigid_body",
                &format!("Body {slot}"),
            ));
            wires.push(wire(id, "body", 3, &format!("body_{slot}")));
        }
        graph(nodes, wires)
    }

    fn metadata(node_id: NodeId) -> PresetMetadata {
        PresetMetadata {
            id: PresetTypeId::new("transfer_test"),
            display_name: "Transfer test".to_string(),
            category: "Test".to_string(),
            osc_prefix: "transfer_test".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: vec![ParamSpecDef {
                id: "roughness".to_string(),
                name: "Object 0 Material roughness".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 0.5,
                section: Some("Object 0 Material".to_string()),
                ..Default::default()
            }],
            bindings: vec![BindingDef {
                id: "roughness".to_string(),
                label: "Object 0 Material roughness".to_string(),
                default_value: 0.5,
                target: BindingTarget::Node {
                    node_id: node_id.clone(),
                    param: "roughness".to_string(),
                },
                convert: ParamConvert::default(),
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: false,
            }],
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: vec![StringParamSpecDef {
                id: "model_path".to_string(),
                name: "Object 0 Material model path".to_string(),
                default_value: "/assets/original.glb".to_string(),
                is_file_picker: true,
                use_dropdown: false,
                is_file_path: true,
            }],
            string_bindings: vec![StringBindingDef {
                id: "model_path".to_string(),
                label: "Object 0 Material model path".to_string(),
                default_value: "/assets/original.glb".to_string(),
                target: BindingTarget::Node {
                    node_id,
                    param: "model_path".to_string(),
                },
            }],
            scene_bounds: None,
            scene_modifier: None,
        }
    }

    #[test]
    fn metadata_duplicate_rewrites_labels_and_keeps_external_string_values() {
        let source_id = NodeId::new("source-material");
        let destination_id = NodeId::new("copied-material");
        let source = EffectGraphDef {
            version: 2,
            name: None,
            description: None,
            preset_metadata: Some(metadata(source_id.clone())),
            scene_modifiers: Vec::new(),
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let mut destination = EffectGraphDef {
            version: 2,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                params: Vec::new(),
                bindings: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                ..metadata(destination_id.clone())
            }),
            scene_modifiers: Vec::new(),
            nodes: vec![node(
                9,
                "copied-material",
                "node.pbr_material",
                "Object 1 Material",
            )],
            wires: Vec::new(),
        };
        // The destination starts empty so its metadata only supplies the
        // schema fields; the copied rows receive fresh ids.
        let mut ids = HashMap::new();
        ids.insert(source_id, destination_id);
        let remaps = transfer_metadata(
            &source,
            &mut destination,
            &ids,
            &[(
                "Object 0 Material".to_string(),
                "Object 1 Material".to_string(),
            )],
        )
        .expect("metadata should copy");
        assert_eq!(remaps.len(), 2);
        let metadata = destination.preset_metadata.as_ref().unwrap();
        let copied = metadata
            .params
            .iter()
            .find(|param| param.id == "roughness_duplicate")
            .unwrap();
        assert_eq!(copied.section.as_deref(), Some("Object 1 Material"));
        assert_eq!(metadata.bindings[0].label, "Object 1 Material roughness");
        let string = metadata
            .string_params
            .iter()
            .find(|param| param.id == "model_path_duplicate")
            .unwrap();
        assert_eq!(string.name, "Object 1 Material model path");
        assert_eq!(string.default_value, "/assets/original.glb");
        assert_eq!(
            metadata.string_bindings[0].label,
            "Object 1 Material model path"
        );
        assert_eq!(
            metadata.string_bindings[0].default_value,
            "/assets/original.glb"
        );
    }

    #[test]
    fn capture_keeps_only_the_selected_physics_body() {
        let render = node(1, "render", "node.render_scene", "Scene");
        let object = node(2, "object", "node.scene_object", "Object 0");
        let world = node(3, "world", "node.physics_world", "Physics World");
        let body0 = node(4, "body-0", "node.rigid_body", "Body 0");
        let body1 = node(5, "body-1", "node.rigid_body", "Body 1");
        let wires = vec![
            wire(2, "object", 1, "object_0"),
            wire(3, "pose_0", 2, "transform"),
            wire(4, "body", 3, "body_0"),
            wire(5, "body", 3, "body_1"),
        ];
        let (project, layer) =
            project_with_graph(graph(vec![render, object, world, body0, body1], wires));
        let clipboard = SceneItemClipboard::capture(&project, &layer, 1, SceneItemKind::Object, 0)
            .expect("selected body should be copyable");
        let ids: HashSet<_> = clipboard.graph.nodes.iter().map(|node| node.id).collect();
        assert!(ids.contains(&4));
        assert!(!ids.contains(&5));
        assert!(
            !clipboard
                .graph
                .wires
                .iter()
                .any(|wire| wire.to_port == "body_1")
        );
    }

    #[test]
    fn capture_rejects_ambiguous_physics_body_atomically() {
        let wires = vec![
            wire(2, "object", 1, "object_0"),
            wire(3, "pose_0", 2, "transform"),
            wire(4, "body", 3, "body_0"),
            wire(5, "body", 3, "body_0"),
        ];
        let nodes = vec![
            node(1, "render", "node.render_scene", "Scene"),
            node(2, "object", "node.scene_object", "Object 0"),
            node(3, "world", "node.physics_world", "Physics World"),
            node(4, "body-0a", "node.rigid_body", "Body 0 A"),
            node(5, "body-0b", "node.rigid_body", "Body 0 B"),
        ];
        let (project, layer) = project_with_graph(graph(nodes, wires));
        let result = SceneItemClipboard::capture(&project, &layer, 1, SceneItemKind::Object, 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ambiguous"));
    }

    #[test]
    fn move_swaps_scene_slots_without_changing_item_identity() {
        let nodes = vec![node(1, "render", "node.render_scene", "Scene")];
        let wires = vec![
            wire(10, "object", 1, "object_0"),
            wire(11, "object", 1, "object_1"),
        ];
        let (mut project, layer) = project_with_graph(graph(nodes, wires));
        // The two upstream node ids need not be reachable for a reorder; the
        // operation intentionally swaps only the scene output ports.
        let mut command = build_action(
            &project,
            SceneItemAction::Move {
                layer,
                scene: 1,
                kind: SceneItemKind::Object,
                index: 0,
                delta: 1,
            },
        )
        .expect("adjacent scene item should move");
        command.execute(&mut project);
        let moved = project
            .timeline
            .layers
            .first()
            .and_then(Layer::generator_graph)
            .unwrap();
        assert!(
            moved
                .wires
                .iter()
                .any(|wire| wire.from_node == 10 && wire.to_port == "object_1")
        );
        assert!(
            moved
                .wires
                .iter()
                .any(|wire| wire.from_node == 11 && wire.to_port == "object_0")
        );
        command.undo(&mut project);
        let restored = project
            .timeline
            .layers
            .first()
            .and_then(Layer::generator_graph)
            .unwrap();
        assert!(
            restored
                .wires
                .iter()
                .any(|wire| wire.from_node == 10 && wire.to_port == "object_0")
        );
    }

    #[test]
    fn paste_cut_preserves_material_animation_values_routes_and_exact_undo_redo() {
        let mut project = Project::default();
        let source_layer = add_transfer_layer(&mut project, "Source", "source", false);
        let destination_layer =
            add_transfer_layer(&mut project, "Destination", "destination", false);
        {
            let destination_host = project
                .graph_target_owner_mut(&GraphTarget::Generator(destination_layer.clone()))
                .unwrap();
            destination_host
                .graph
                .as_mut()
                .unwrap()
                .preset_metadata
                .as_mut()
                .unwrap()
                .params
                .push(ParamSpecDef {
                    id: "destination_only".to_string(),
                    name: "Destination only".to_string(),
                    min: 0.0,
                    max: 1.0,
                    default_value: 0.5,
                    ..Default::default()
                });
            destination_host.refresh_manifest_from_graph();
            destination_host.base_tracked = false;
            let destination_only = destination_host.params.get_mut("destination_only").unwrap();
            destination_only.value = 0.31;
            destination_only.base = 9.2;
        }
        let source_target = GraphTarget::Generator(source_layer.clone());
        let source_param = "roughness";
        let source_host = project.graph_target_owner_mut(&source_target).unwrap();
        source_host.base_tracked = true;
        source_host.params.get_mut(source_param).unwrap().base = 0.83;
        source_host.params.get_mut(source_param).unwrap().value = 0.67;
        source_host.drivers = Some(vec![ParameterDriver::new(
            source_param.to_string(),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
        let clipboard =
            SceneItemClipboard::capture(&project, &source_layer, 1, SceneItemKind::Object, 0)
                .expect("source object should copy");
        let copied_material = material_node(&clipboard.graph, "source");
        assert_eq!(
            copied_material.params.get("base_color_map"),
            Some(&SerializedParamValue::String {
                value: "/textures/source-albedo.png".to_string()
            })
        );

        // Cut the source after capture. The immutable clipboard still owns
        // the material, animation producer, and host state needed for paste.
        let source_host = project.graph_target_owner_mut(&source_target).unwrap();
        source_host
            .graph
            .as_mut()
            .unwrap()
            .wires
            .retain(|wire| !(wire.to_node == 1 && wire.to_port == "object_0"));

        let destination_target = GraphTarget::Generator(destination_layer.clone());
        let before =
            serde_json::to_value(project.graph_target_owner(&destination_target).unwrap()).unwrap();
        let command = build_action(
            &project,
            SceneItemAction::Paste {
                layer: destination_layer.clone(),
                scene: 1,
                clipboard: Box::new(clipboard),
            },
        )
        .expect("paste should prepare");
        let mut editing = EditingService::new();
        editing.execute(command, &mut project);
        assert!(editing.take_rejection().is_none());

        let after_host = project.graph_target_owner(&destination_target).unwrap();
        let after_graph = after_host.graph.as_ref().unwrap();
        let copied_material = after_graph
            .nodes
            .iter()
            .find(|node| {
                node.type_id == "node.pbr_material"
                    && node.node_id != NodeId::new("destination-material")
            })
            .expect("pasted material");
        assert_ne!(copied_material.node_id, NodeId::new("source-material"));
        assert_eq!(
            copied_material.params.get("baked_look"),
            Some(&SerializedParamValue::Float { value: 1.0 })
        );
        assert_eq!(
            copied_material.params.get("base_color_map"),
            Some(&SerializedParamValue::String {
                value: "/textures/source-albedo.png".to_string()
            })
        );
        assert!(after_graph.nodes.iter().any(|node| {
            node.type_id == "node.value" && node.node_id != NodeId::new("destination-animation")
        }));
        let copied_binding = after_graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .find(|binding| {
                binding.id == "roughness_duplicate"
                    && matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &copied_material.node_id && param == "roughness")
            })
            .expect("pasted material binding");
        let copied_param = after_host.params.get(&copied_binding.id).unwrap();
        assert!(after_host.base_tracked);
        assert_eq!(copied_param.base, 0.83);
        assert_eq!(copied_param.value, 0.67);
        let destination_only = after_host.params.get("destination_only").unwrap();
        assert_eq!(destination_only.base, destination_only.value);
        assert!(
            after_host
                .drivers
                .as_ref()
                .unwrap()
                .iter()
                .any(|driver| driver.param_id.as_ref() == copied_binding.id)
        );
        let after = serde_json::to_value(after_host).unwrap();

        assert!(editing.undo(&mut project));
        assert_eq!(
            serde_json::to_value(project.graph_target_owner(&destination_target).unwrap()).unwrap(),
            before
        );
        assert!(editing.redo(&mut project));
        assert!(editing.take_rejection().is_none());
        assert_eq!(
            serde_json::to_value(project.graph_target_owner(&destination_target).unwrap()).unwrap(),
            after
        );
    }

    #[test]
    fn paste_light_uses_light_slot_and_fresh_identity() {
        let mut project = Project::default();
        let source_layer = add_transfer_layer(&mut project, "Source", "light-source", true);
        let destination_layer =
            add_transfer_layer(&mut project, "Destination", "light-destination", true);
        let clipboard =
            SceneItemClipboard::capture(&project, &source_layer, 1, SceneItemKind::Light, 0)
                .expect("light should copy");
        let source_light_id = clipboard
            .graph
            .nodes
            .iter()
            .find(|node| node.type_id == "node.light")
            .unwrap()
            .node_id
            .clone();
        let command = build_action(
            &project,
            SceneItemAction::Paste {
                layer: destination_layer.clone(),
                scene: 1,
                clipboard: Box::new(clipboard),
            },
        )
        .unwrap();
        let mut editing = EditingService::new();
        editing.execute(command, &mut project);
        assert!(editing.take_rejection().is_none());
        let graph = project
            .graph_target_owner(&GraphTarget::Generator(destination_layer))
            .unwrap()
            .graph
            .as_ref()
            .unwrap();
        let light_outputs: Vec<_> = graph
            .wires
            .iter()
            .filter(|wire| wire.to_node == 1 && wire.to_port.starts_with("light_"))
            .collect();
        assert_eq!(light_outputs.len(), 2);
        let copied_light = graph
            .nodes
            .iter()
            .find(|node| {
                node.type_id == "node.light"
                    && node.node_id != source_light_id
                    && node.node_id != NodeId::new("light-destination-light")
            })
            .expect("fresh light identity");
        assert!(graph.wires.iter().any(|wire| {
            wire.from_node == copied_light.id && wire.to_node == 1 && wire.to_port == "light_1"
        }));
    }

    #[test]
    fn paste_physics_merges_one_body_and_rejects_full_destination_atomically() {
        let source_graph = physics_graph(1);
        let (mut source_project, source_layer) = project_with_graph(source_graph);
        let clipboard = SceneItemClipboard::capture(
            &source_project,
            &source_layer,
            1,
            SceneItemKind::Object,
            0,
        )
        .expect("physics body should copy");
        let destination_layer = add_transfer_layer(
            &mut source_project,
            "Physics destination",
            "physics-dest",
            false,
        );
        // Replace the ordinary destination graph with one world that already
        // owns slot zero, forcing the copied body into slot one.
        source_project
            .graph_target_owner_mut(&GraphTarget::Generator(destination_layer.clone()))
            .unwrap()
            .graph = Some(physics_graph(1));
        let command = build_action(
            &source_project,
            SceneItemAction::Paste {
                layer: destination_layer.clone(),
                scene: 1,
                clipboard: Box::new(clipboard.clone()),
            },
        )
        .expect("physics paste should merge into existing world");
        let mut editing = EditingService::new();
        editing.execute(command, &mut source_project);
        assert!(editing.take_rejection().is_none());
        let merged = source_project
            .graph_target_owner(&GraphTarget::Generator(destination_layer.clone()))
            .unwrap()
            .graph
            .as_ref()
            .unwrap();
        assert!(merged.wires.iter().any(|wire| wire.to_port == "body_0"));
        assert!(merged.wires.iter().any(|wire| wire.to_port == "body_1"));
        assert!(merged.wires.iter().any(|wire| wire.from_port == "pose_1"));
        assert_eq!(
            merged
                .wires
                .iter()
                .filter(|wire| wire.to_node == 1 && wire.to_port == "object_0")
                .count(),
            1,
        );
        assert_eq!(
            merged
                .wires
                .iter()
                .filter(|wire| wire.to_node == 1 && wire.to_port == "object_1")
                .count(),
            1,
        );

        let full_layer =
            add_transfer_layer(&mut source_project, "Full physics", "physics-full", false);
        source_project
            .graph_target_owner_mut(&GraphTarget::Generator(full_layer.clone()))
            .unwrap()
            .graph = Some(physics_graph(16));
        let before = serde_json::to_value(
            source_project
                .graph_target_owner(&GraphTarget::Generator(full_layer.clone()))
                .unwrap(),
        )
        .unwrap();
        let error = build_action(
            &source_project,
            SceneItemAction::Paste {
                layer: full_layer.clone(),
                scene: 1,
                clipboard: Box::new(clipboard),
            },
        )
        .expect_err("full physics world must reject atomically");
        assert!(error.contains("free body slots"));
        assert_eq!(
            serde_json::to_value(
                source_project
                    .graph_target_owner(&GraphTarget::Generator(full_layer))
                    .unwrap(),
            )
            .unwrap(),
            before
        );
    }
}
