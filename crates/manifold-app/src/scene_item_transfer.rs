//! Scene items use immutable clipboard snapshots and the same atomic generator
//! replacement as modifier cards. Only the content thread executes edits.
use std::collections::{HashMap, HashSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID, SerializedParamValue,
};
use manifold_core::effects::PresetInstance;
use manifold_core::physics_members::{MEMBER_OUTPUT, WORLD_TYPE_ID, member_input};
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId, NodeId, SceneNodeRef};
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
        kind: SceneItemKind,
        item: &SceneNodeRef,
    ) -> Result<Self, String> {
        let target = GraphTarget::Generator(layer.clone());
        let host = project
            .graph_target_owner(&target)
            .ok_or("Scene is no longer available")?;
        let source =
            crate::graph_target::resolve(project, &target).ok_or("Scene graph is unavailable")?;
        let slot = match kind {
            SceneItemKind::Object => manifold_core::scene_slots::object_slot(&source, item),
            SceneItemKind::Light => manifold_core::scene_slots::light_slot(&source, item),
        }?;
        let (scene, index) = (slot.render_id, slot.index);
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
                manifold_editing::commands::graph::scene_fluid_role_assignments(&source, group)
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
            // A body's membership is an output of the body (or of the group
            // that holds it), so the upstream walk would miss it: take that
            // one wire and the world it joins. The world contributes its own
            // settings but never the other bodies' membership.
            if let Some(member) = source.wires.iter().find(|wire| {
                    wire.from_node == id
                        && wire.from_port == MEMBER_OUTPUT
                        && member_input(&wire.to_port).is_some()
                        && source.nodes.iter().any(|n| n.id == wire.to_node && n.type_id == WORLD_TYPE_ID)
                })
            {
                if !captured_wires.contains(member) {
                    captured_wires.push(member.clone());
                }
                pending.push((member.to_node, member.to_port.clone()));
            }
            for wire in source.wires.iter().filter(|wire| wire.to_node == id) {
                if node.type_id == WORLD_TYPE_ID && member_input(&wire.to_port).is_some() {
                    continue;
                }
                if !captured_wires.contains(wire) {
                    captured_wires.push(wire.clone());
                }
                pending.push((wire.from_node, wire.from_port.clone()));
            }
        }
        let mut graph = (*source).clone();
        graph.nodes.retain(|node| ids.contains(&node.id));
        graph.wires = captured_wires;
        // Forces aimed only at this item belong to it and travel with it.
        let producer = manifold_core::SceneNodeRef {
            scope: Vec::new(),
            node: source.nodes.iter().find(|node| node.id == output.from_node).ok_or("Selected scene item is missing")?.node_id.clone(),
        };
        let owned = manifold_core::scene_modifier_edit::object_modifiers(&source, &producer).owned;
        graph.scene_modifiers.retain(|modifier| owned.contains(&modifier.id));
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
    /// One copy of an object in one undo step, with its own body when the
    /// source has physics.
    DuplicateObject {
        layer: LayerId,
        object: SceneNodeRef,
    },
    DuplicateLight {
        layer: LayerId,
        light: SceneNodeRef,
    },
    /// The object card's Physics switch (P6a): the body's own `enabled` once
    /// a body exists (off keeps its settings, D6); the first switch-on adds
    /// the body, and the shared world when there is none.
    SetPhysics {
        layer: LayerId,
        object: SceneNodeRef,
        enabled: bool,
    },
    Move {
        layer: LayerId,
        kind: SceneItemKind,
        item: SceneNodeRef,
        delta: i32,
    },
}

impl SceneItemAction {
    pub(crate) fn selection_request(&self) -> Option<crate::edit_selection::SelectAfterEdit> {
        let (layer, kind) = match self {
            Self::Paste { layer, clipboard, .. } => (layer, clipboard.kind),
            Self::DuplicateObject { layer, .. } => (layer, SceneItemKind::Object),
            Self::DuplicateLight { layer, .. } => (layer, SceneItemKind::Light),
            Self::Move { .. } | Self::SetPhysics { .. } => return None,
        };
        Some(match kind {
            SceneItemKind::Object => crate::edit_selection::SelectAfterEdit::NewObject(layer.clone()),
            SceneItemKind::Light => crate::edit_selection::SelectAfterEdit::NewLight(layer.clone()),
        })
    }
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

/// Paste's half of the one metadata copier
/// ([`manifold_editing::binding_transfer::transfer_bindings`]): the copy's
/// card rows get fresh ids, sections and labels renamed after its handles.
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
    let node_map: Vec<(NodeId, NodeId)> = ids.iter().map(|(old, new)| (old.clone(), new.clone())).collect();
    let relabel = |text: &str, _: &NodeId| rewrite_label(text.to_string(), handle_rewrites);
    Ok(manifold_editing::binding_transfer::transfer_bindings(source, dest, &node_map, &relabel))
}

/// How the copied item is named.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyNaming {
    /// Every copied handle takes the clone helper's free `_N` suffix.
    Deduped,
    /// Duplicate (SCENE_OBJECT_AND_PANEL_V2_DESIGN D11): the item is named
    /// "<name> N", the first free N from 2, as the importer and Water do.
    Numbered,
}

/// Rename the copied item's root to "<source name> N". A grouped item's
/// inner `scene_object` carries the same name (D6): it is the name the Scene
/// tab shows.
fn name_duplicate(
    clipboard: &SceneItemClipboard,
    doc_ids: &HashMap<u32, u32>,
    added: &mut [EffectGraphNode],
    handles: &mut HashSet<String>,
) {
    let Some(base) = clipboard
        .graph
        .nodes
        .iter()
        .find(|node| node.id == clipboard.root)
        .and_then(|node| node.handle.clone())
    else {
        return;
    };
    let Some(root) = doc_ids
        .get(&clipboard.root)
        .and_then(|id| added.iter_mut().find(|node| node.id == *id))
    else {
        return;
    };
    let name = (2..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !handles.contains(candidate))
        .expect("an unbounded range always finds a free name");
    handles.insert(name.clone());
    // Capture refuses groups with several objects, so a group root holds one.
    if let Some(inner) = root
        .group
        .as_deref_mut()
        .and_then(|group| group.nodes.iter_mut().find(|node| node.type_id == "node.scene_object"))
    {
        inner.handle = Some(name.clone());
    }
    root.handle = Some(name);
}

fn paste(
    project: &Project,
    layer: LayerId,
    scene: u32,
    clipboard: SceneItemClipboard,
    description: &'static str,
    naming: CopyNaming,
) -> Result<Box<dyn Command>, String> {
    let target = GraphTarget::Generator(layer.clone());
    let before = project
        .graph_target_owner(&target)
        .ok_or("Destination scene is unavailable")?
        .clone();
    let mut graph = crate::graph_target::resolve(project, &target)
        .map(|graph| (*graph).clone())
        .ok_or("Destination scene graph is unavailable")?;
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
    let mut reused_nodes = HashSet::new();
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
                reused_nodes.insert(world);
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
    if naming == CopyNaming::Numbered {
        name_duplicate(&clipboard, &doc_ids, &mut added, &mut handles);
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
    // The one wire copier: a shared world keeps its scene-wide setup and only
    // takes the pasted bodies, each in the world's next free slot.
    {
        let doc_id = |id: u32| doc_ids.get(&id).copied();
        let reused = |id: u32| reused_nodes.contains(&id);
        let keep_port = |_: &EffectGraphWire| None;
        manifold_editing::commands::graph::copy_scene_item_wires(
            &mut graph.nodes,
            &mut graph.wires,
            &clipboard.graph.wires,
            &manifold_editing::commands::graph::WireCopy { doc_id: &doc_id, reused: &reused, to_port: &keep_port },
        )?;
    }
    let next_port = graph
        .wires
        .iter()
        .filter(|wire| wire.to_node == scene)
        .filter_map(|wire| wire.to_port.strip_prefix(clipboard.kind.prefix())?.parse::<u32>().ok())
        .max()
        .map_or(0, |last| last + 1);
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
            value: count.max(next_port + 1) as f32,
        },
    );
    graph.wires.push(EffectGraphWire {
        from_node: *doc_ids
            .get(&clipboard.root)
            .ok_or("Copied scene item is missing")?,
        from_port: clipboard.port.clone(),
        to_node: scene,
        to_port: format!("{}{next_port}", clipboard.kind.prefix()),
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
    let mut remaps = transfer_metadata(&clipboard.graph, &mut graph, &stable, &handle_rewrites)?;
    graph = attach_copied_forces(&clipboard, graph, scene, &stable, &node_id_map, &mut remaps)?;
    manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures(&mut graph);
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
    manifold_editing::param_routes::copy_param_routes(&clipboard.host, &mut after, &remaps, manifold_editing::param_routes::RouteTransfer::Copy);
    Ok(Box::new(
        crate::generator_change::ReplaceGeneratorStateCommand::new(
            layer,
            before,
            after,
            description,
        ),
    ))
}

fn set_physics(project: &Project, layer: &LayerId, object: &SceneNodeRef, enabled: bool) -> Result<Box<dyn Command>, String> {
    use manifold_nodes_scene::node_graph::scene_exposure::{metadata_for_node_type, migrate_scene_exposures};
    use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
    let mut default = crate::ui_bridge::project::generator_catalog_default(project, layer).ok_or("Scene is unavailable")?;
    migrate_scene_exposures(&mut default);
    let target = GraphTarget::Generator(layer.clone());
    let physics = project
        .graph_for_target(&target, Some(&default))
        .and_then(SceneVm::from_def)
        .and_then(|vm| vm.objects.into_iter().find_map(|row| match row {
            SceneObjectVm::Known(row) if row.address == *object => row.physics,
            _ => None,
        }));
    match physics {
        // A stale second click would change nothing; it is refused, so no
        // undo step records it.
        Some(physics) if physics.enabled == enabled => {
            Err(if enabled { "Physics is already on" } else { "Physics is already off" }.into())
        }
        Some(physics) => crate::ui_bridge::project::apply_scene_param_write(
            project,
            layer,
            physics.body_scope_path,
            physics.body_node_id,
            "enabled",
            if enabled { 1.0 } else { 0.0 },
        )
        .map(|command| command as Box<dyn Command>)
        .ok_or_else(|| "The body's switch is unavailable".into()),
        None if enabled => manifold_editing::commands::graph::EnableSceneObjectPhysicsCommand::new(
            project,
            target,
            object.clone(),
            &metadata_for_node_type("node.rigid_body"),
            &metadata_for_node_type("node.physics_world"),
            &default,
        )
        .map(|command| Box::new(command) as Box<dyn Command>)
        .map_err(|error| error.message),
        None => Err("Selected object has no physics to switch off".into()),
    }
}

/// Attach a copy of each force the copied item owns, aimed at the copy alone
/// in the scene with doc id `scene`. Each copy's rows take the source rows'
/// presentation, and `remaps` gains them so their values and routes follow.
fn attach_copied_forces(
    clipboard: &SceneItemClipboard,
    mut graph: EffectGraphDef,
    scene: u32,
    stable: &HashMap<NodeId, NodeId>,
    node_id_map: &[(NodeId, NodeId)],
    remaps: &mut Vec<(String, String)>,
) -> Result<EffectGraphDef, String> {
    use manifold_core::scene_modifier_edit::{aim_at_copy, insert_scene_modifier};
    if clipboard.graph.scene_modifiers.is_empty() {
        return Ok(graph);
    }
    let item = manifold_core::SceneNodeRef {
        scope: Vec::new(),
        node: clipboard.graph.nodes.iter().find(|node| node.id == clipboard.root).ok_or("Copied scene item is missing")?.node_id.clone(),
    };
    let scene = manifold_core::SceneNodeRef {
        scope: Vec::new(),
        node: graph.nodes.iter().find(|node| node.id == scene).ok_or("Scene is unavailable")?.node_id.clone(),
    };
    let source_rows = clipboard.graph.preset_metadata.as_ref();
    for modifier in &clipboard.graph.scene_modifiers {
        let (targets, mesh_frames) = aim_at_copy(modifier, &item, node_id_map).map_err(|error| error.to_string())?;
        let legacy_math_view_carrier = match &modifier.legacy_math_view_carrier {
            Some(carrier) => Some(stable.get(carrier).cloned().ok_or("A copied force's carrier was not copied")?),
            None => None,
        };
        let id = loop {
            let candidate = NodeId::new(manifold_core::short_id());
            if !graph.scene_modifiers.iter().any(|existing| existing.id == candidate) {
                break candidate;
            }
        };
        let instance = manifold_core::scene_modifier_preset::SceneModifierInstanceDef {
            id: id.clone(),
            scene: scene.clone(),
            targets,
            mesh_frames,
            legacy_math_view_carrier,
            graph: modifier.graph.clone(),
        };
        let end = graph.scene_modifiers.len();
        graph = insert_scene_modifier(&graph, end, instance).map_err(|error| error.to_string())?.graph;
        let Some(source_rows) = source_rows else { continue };
        let rows = graph.preset_metadata.as_mut().ok_or("Destination scene has no parameter metadata")?;
        for binding in &source_rows.bindings {
            let BindingTarget::SceneModifier { modifier_id, param_id } = &binding.target else { continue };
            if *modifier_id != modifier.id {
                continue;
            }
            let Some(copy) = rows.bindings.iter().find(|copy| {
                matches!(&copy.target, BindingTarget::SceneModifier { modifier_id, param_id: copied } if *modifier_id == id && copied == param_id)
            }) else {
                continue;
            };
            let copy_id = copy.id.clone();
            if let (Some(spec), Some(slot)) = (
                source_rows.params.iter().find(|spec| spec.id == binding.id),
                rows.params.iter_mut().find(|spec| spec.id == copy_id),
            ) {
                *slot = manifold_core::effect_graph_def::ParamSpecDef { id: copy_id.clone(), ..spec.clone() };
            }
            if !remaps.iter().any(|(old, _)| *old == binding.id) {
                remaps.push((binding.id.clone(), copy_id));
            }
        }
    }
    Ok(graph)
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
        } => paste(project, layer, scene, *clipboard, "Paste Scene Item", CopyNaming::Deduped),
        SceneItemAction::DuplicateObject { layer, object } => {
            let target = GraphTarget::Generator(layer);
            let default = crate::graph_target::resolve(project, &target).ok_or("Scene graph is unavailable")?;
            let command = manifold_editing::commands::graph::DuplicateSceneObjectCommand::new(
                project, target.clone(), object, &default,
            )
            .map_err(|error| error.message)?;
            Ok(Box::new(command))
        }
        SceneItemAction::SetPhysics { layer, object, enabled } => set_physics(project, &layer, &object, enabled),
        SceneItemAction::DuplicateLight { layer, light } => {
            let clipboard = SceneItemClipboard::capture(project, &layer, SceneItemKind::Light, &light)?;
            let target = GraphTarget::Generator(layer.clone());
            let default = crate::graph_target::resolve(project, &target).ok_or("Scene graph is unavailable")?;
            let scene = manifold_core::scene_slots::light_slot(&default, &light)?.render_id;
            paste(project, layer, scene, clipboard, "Duplicate Scene Item", CopyNaming::Numbered)
        }
        SceneItemAction::Move {
            layer,
            kind,
            item,
            delta,
        } => {
            let target = GraphTarget::Generator(layer.clone());
            let before = project
                .graph_target_owner(&target)
                .ok_or("Scene is unavailable")?
                .clone();
            let mut graph = crate::graph_target::resolve(project, &target)
                .map(|graph| (*graph).clone())
                .ok_or("Scene graph is unavailable")?;
            let slot = match kind {
                SceneItemKind::Object => manifold_core::scene_slots::object_slot(&graph, &item),
                SceneItemKind::Light => manifold_core::scene_slots::light_slot(&graph, &item),
            }?;
            let (scene, index) = (slot.render_id, slot.index);
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
    use manifold_core::effect_graph_def::BindingTarget;
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

    /// The object or light filling slot `index` of scene node 1, by address.
    fn item_at(project: &Project, layer: &LayerId, kind: SceneItemKind, index: u32) -> SceneNodeRef {
        let target = GraphTarget::Generator(layer.clone());
        let def = crate::graph_target::resolve(project, &target).expect("the layer has a graph");
        match kind {
            SceneItemKind::Object => manifold_core::scene_slots::object_at_slot(&def, 1, index),
            SceneItemKind::Light => manifold_core::scene_slots::light_at_slot(&def, 1, index),
        }
        .expect("the slot holds an item")
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
        layer.gen_params_mut().unwrap().graph = Some(graph);
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
        let host = layer.gen_params_mut().unwrap();
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
            wire(4, "pose", 2, "transform"),
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
            shape: None,
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
            wire(4, "pose", 2, "transform"),
            wire(4, "body", 3, "body_0"),
            wire(5, "body", 3, "body_1"),
        ];
        let (project, layer) =
            project_with_graph(graph(vec![render, object, world, body0, body1], wires));
        let clipboard = SceneItemClipboard::capture(&project, &layer, SceneItemKind::Object, &item_at(&project, &layer, SceneItemKind::Object, 0))
            .expect("selected body should be copyable");
        let ids: HashSet<_> = clipboard.graph.nodes.iter().map(|node| node.id).collect();
        assert!(ids.contains(&4));
        assert!(ids.contains(&3), "the world the body joins travels with it");
        assert!(!ids.contains(&5));
        assert!(
            !clipboard
                .graph
                .wires
                .iter()
                .any(|wire| wire.to_port == "body_1")
        );
    }

    /// D11: Duplicate names the copy "<name> N", the first free N from 2;
    /// Paste keeps the clone helper's `_N` names.
    #[test]
    fn duplicate_names_the_copy_with_the_next_free_number() {
        let mut project = Project::default();
        let layer = add_transfer_layer(&mut project, "Scene", "dup", false);
        let mut editing = EditingService::new();
        for _ in 0..2 {
            let action = SceneItemAction::DuplicateObject { layer: layer.clone(), object: item_at(&project, &layer, SceneItemKind::Object, 0) };
            editing.execute(build_action(&project, action).unwrap(), &mut project);
            assert!(editing.take_rejection().is_none());
        }
        let clipboard = SceneItemClipboard::capture(&project, &layer, SceneItemKind::Object, &item_at(&project, &layer, SceneItemKind::Object, 0)).unwrap();
        let paste = SceneItemAction::Paste { layer: layer.clone(), scene: 1, clipboard: Box::new(clipboard) };
        editing.execute(build_action(&project, paste).unwrap(), &mut project);
        assert!(editing.take_rejection().is_none());
        let graph = project.graph_target_owner(&GraphTarget::Generator(layer)).unwrap().graph.clone().unwrap();
        let mut objects: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.scene_object")
            .filter_map(|node| node.handle.clone())
            .collect();
        objects.sort();
        assert_eq!(objects, ["Object 0", "Object 0 2", "Object 0 3", "Object 0_2"]);
    }

    #[test]
    fn move_swaps_scene_slots_without_changing_item_identity() {
        let nodes = vec![
            node(1, "render", "node.render_scene", "Scene"),
            node(10, "first", "node.scene_object", "First"),
            node(11, "second", "node.scene_object", "Second"),
        ];
        let wires = vec![
            wire(10, "object", 1, "object_0"),
            wire(11, "object", 1, "object_1"),
        ];
        let (mut project, layer) = project_with_graph(graph(nodes, wires));
        // The two upstream node ids need not be reachable for a reorder; the
        // operation intentionally swaps only the scene output ports.
        let first = item_at(&project, &layer, SceneItemKind::Object, 0);
        let mut command = build_action(
            &project,
            SceneItemAction::Move {
                layer,
                kind: SceneItemKind::Object,
                item: first,
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
            SceneItemClipboard::capture(&project, &source_layer, SceneItemKind::Object, &item_at(&project, &source_layer, SceneItemKind::Object, 0))
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
            SceneItemClipboard::capture(&project, &source_layer, SceneItemKind::Light, &item_at(&project, &source_layer, SceneItemKind::Light, 0))
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

    /// A whole physics object: the authored transform feeds a body, the body
    /// shapes the mesh and joins the world's slot 0, and the object reads the
    /// body's pose. `with_force` adds a per-body force on the body.
    fn physics_object_scene(with_force: bool) -> EffectGraphDef {
        let mut nodes = vec![
            node(1, "p-render", "node.render_scene", "Scene"),
            node(2, "p-object", "node.scene_object", "Cube"),
            node(3, "p-material", "node.pbr_material", "Cube Material"),
            node(4, "p-mesh", "node.platonic_solid_mesh", "Cube Mesh"),
            node(5, "p-transform", "node.transform_3d", "Cube Transform"),
            node(6, "p-body", "node.rigid_body", "Cube Body"),
            node(7, "p-world", "node.physics_world", "Physics World"),
        ];
        nodes[0].params.insert("objects".into(), SerializedParamValue::Float { value: 1.0 });
        let mut wires = vec![
            wire(5, "transform", 6, "transform"),
            wire(6, "shape", 4, "shape"),
            wire(4, "vertices", 2, "vertices"),
            wire(3, "out", 2, "material"),
            wire(6, "pose", 2, "transform"),
            wire(2, "object", 1, "object_0"),
            wire(6, "body", 7, "body_0"),
        ];
        if with_force {
            nodes.push(node(8, "p-force", "node.uniform_vector_field", "Wind"));
            wires.push(wire(8, "out", 6, "acceleration"));
        }
        graph(nodes, wires)
    }

    fn graph_in(project: &Project, layer: &LayerId) -> EffectGraphDef {
        project
            .graph_target_owner(&GraphTarget::Generator(layer.clone()))
            .unwrap()
            .graph
            .clone()
            .unwrap()
    }

    /// The graph with every id dropped: each wire as `type.port > type.port`
    /// and each node as its type, sorted, so two copies of one object compare.
    fn shape_of(graph: &EffectGraphDef) -> Vec<String> {
        let type_of = |id: u32| graph.nodes.iter().find(|node| node.id == id).map(|node| node.type_id.clone()).unwrap();
        let mut shape: Vec<String> = graph.nodes.iter().map(|node| format!("node {}", node.type_id)).collect();
        shape.extend(graph.wires.iter().map(|wire| {
            format!("{}.{} > {}.{}", type_of(wire.from_node), wire.from_port, type_of(wire.to_node), wire.to_port)
        }));
        shape.sort();
        shape
    }

    #[test]
    fn paste_gives_the_copy_its_own_per_body_force() {
        let (mut project, layer) = project_with_graph(physics_object_scene(true));
        let clipboard = SceneItemClipboard::capture(&project, &layer, SceneItemKind::Object, &item_at(&project, &layer, SceneItemKind::Object, 0)).unwrap();
        let paste = SceneItemAction::Paste { layer: layer.clone(), scene: 1, clipboard: Box::new(clipboard) };
        let mut editing = EditingService::new();
        editing.execute(build_action(&project, paste).unwrap(), &mut project);
        assert!(editing.take_rejection().is_none());
        let pasted = graph_in(&project, &layer);
        let bodies: Vec<u32> = pasted.nodes.iter().filter(|n| n.type_id == "node.rigid_body").map(|n| n.id).collect();
        assert_eq!(bodies.len(), 2);
        for body in bodies {
            assert!(
                pasted.wires.iter().any(|w| w.to_node == body && w.to_port == "acceleration"),
                "body {body} has its force"
            );
            assert!(
                pasted.wires.iter().any(|w| w.from_node == body && w.from_port == "body" && w.to_node == 7),
                "body {body} joined the world"
            );
        }
        let slots: BTreeSet<_> = pasted.wires.iter().filter(|w| w.to_node == 7).map(|w| w.to_port.clone()).collect();
        assert_eq!(slots, BTreeSet::from(["body_0".to_string(), "body_1".to_string()]));
    }

    #[test]
    fn duplicate_and_paste_of_a_physics_object_make_the_same_graph() {
        for with_force in [false, true] {
            let (mut duplicated, dup_layer) = project_with_graph(physics_object_scene(with_force));
            let mut editing = EditingService::new();
            let action = SceneItemAction::DuplicateObject { layer: dup_layer.clone(), object: item_at(&duplicated, &dup_layer, SceneItemKind::Object, 0) };
            editing.execute(build_action(&duplicated, action).unwrap(), &mut duplicated);
            assert!(editing.take_rejection().is_none());

            let (mut pasted, paste_layer) = project_with_graph(physics_object_scene(with_force));
            let clipboard = SceneItemClipboard::capture(&pasted, &paste_layer, SceneItemKind::Object, &item_at(&pasted, &paste_layer, SceneItemKind::Object, 0)).unwrap();
            let action = SceneItemAction::Paste { layer: paste_layer.clone(), scene: 1, clipboard: Box::new(clipboard) };
            editing.execute(build_action(&pasted, action).unwrap(), &mut pasted);
            assert!(editing.take_rejection().is_none());

            let (from_duplicate, from_paste) = (graph_in(&duplicated, &dup_layer), graph_in(&pasted, &paste_layer));
            assert_eq!(shape_of(&from_duplicate), shape_of(&from_paste), "with_force {with_force}");
            assert!(from_paste.wires.iter().any(|w| w.to_node == 7 && w.to_port == "body_1"));
            if with_force {
                // Each body has a force of its own: editing the copy's wind
                // never moves the original.
                let sources: BTreeSet<u32> = from_duplicate
                    .wires
                    .iter()
                    .filter(|w| w.to_port == "acceleration")
                    .map(|w| w.from_node)
                    .collect();
                assert_eq!(sources.len(), 2, "the duplicate has its own force node");
            }
        }
    }

    #[test]
    fn paste_physics_merges_one_body_and_rejects_full_destination_atomically() {
        let source_graph = physics_graph(1);
        let (mut source_project, source_layer) = project_with_graph(source_graph);
        let clipboard = SceneItemClipboard::capture(
            &source_project,
            &source_layer,
            SceneItemKind::Object,
            &item_at(&source_project, &source_layer, SceneItemKind::Object, 0),
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
        assert_eq!(
            merged.wires.iter().filter(|wire| wire.from_port == "pose" && wire.to_port == "transform").count(),
            2,
            "each object reads its own body's pose"
        );
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

        // Sixteen bodies is not a full world: the paste takes slot 16.
        let busy_layer = add_transfer_layer(&mut source_project, "Busy physics", "physics-busy", false);
        source_project
            .graph_target_owner_mut(&GraphTarget::Generator(busy_layer.clone()))
            .unwrap()
            .graph = Some(physics_graph(16));
        let paste = SceneItemAction::Paste { layer: busy_layer.clone(), scene: 1, clipboard: Box::new(clipboard.clone()) };
        editing.execute(build_action(&source_project, paste).expect("sixteen bodies leave room"), &mut source_project);
        assert!(editing.take_rejection().is_none());
        let busy = source_project.graph_target_owner(&GraphTarget::Generator(busy_layer)).unwrap().graph.as_ref().unwrap();
        assert!(busy.wires.iter().any(|wire| wire.to_port == "body_16"));

        // A world with all 64 of its bodies taken grows to take the paste.
        let grown_layer = add_transfer_layer(&mut source_project, "Grown physics", "physics-grown", false);
        source_project
            .graph_target_owner_mut(&GraphTarget::Generator(grown_layer.clone()))
            .unwrap()
            .graph = Some(physics_graph(64));
        let paste = SceneItemAction::Paste { layer: grown_layer.clone(), scene: 1, clipboard: Box::new(clipboard.clone()) };
        editing.execute(build_action(&source_project, paste).expect("a 64-body world grows"), &mut source_project);
        assert!(editing.take_rejection().is_none());
        let grown = source_project.graph_target_owner(&GraphTarget::Generator(grown_layer)).unwrap().graph.as_ref().unwrap();
        assert!(grown.wires.iter().any(|wire| wire.to_port == "body_64"));
        let world = grown.nodes.iter().find(|node| node.type_id == "node.physics_world").unwrap();
        assert_eq!(manifold_core::physics_body_slots::world_bodies(world), 65);

        let full_layer =
            add_transfer_layer(&mut source_project, "Full physics", "physics-full", false);
        source_project
            .graph_target_owner_mut(&GraphTarget::Generator(full_layer.clone()))
            .unwrap()
            .graph = Some(physics_graph(
                manifold_core::scene_surface::PHYSICS_WORLD_MAX_BODIES as usize,
            ));
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

    /// A duplicate queued for the last object, then a remove of the first that
    /// shifts every slot: the duplicate still copies the object it named. One
    /// queued for the removed object refuses with a reason.
    #[test]
    fn a_queued_duplicate_acts_on_the_object_it_named_after_a_remove() {
        let mut project = Project::default();
        let layer = add_transfer_layer(&mut project, "Scene", "queued", false);
        let mut editing = EditingService::new();
        for _ in 0..2 {
            let first = item_at(&project, &layer, SceneItemKind::Object, 0);
            let action = SceneItemAction::DuplicateObject { layer: layer.clone(), object: first };
            editing.execute(build_action(&project, action).unwrap(), &mut project);
            assert!(editing.take_rejection().is_none());
        }
        let handles = |project: &Project| -> Vec<String> {
            let target = GraphTarget::Generator(layer.clone());
            let def = crate::graph_target::resolve(project, &target).unwrap();
            let mut names: Vec<String> =
                def.nodes.iter().filter(|n| n.type_id == "node.scene_object").filter_map(|n| n.handle.clone()).collect();
            names.sort();
            names
        };
        assert_eq!(handles(&project), ["Object 0", "Object 0 2", "Object 0 3"]);
        let first = item_at(&project, &layer, SceneItemKind::Object, 0);
        let last = item_at(&project, &layer, SceneItemKind::Object, 2);
        let queued = SceneItemAction::DuplicateObject { layer: layer.clone(), object: last };
        let queued_for_removed = SceneItemAction::DuplicateObject { layer: layer.clone(), object: first.clone() };

        let target = GraphTarget::Generator(layer.clone());
        let default = (*crate::graph_target::resolve(&project, &target).unwrap()).clone();
        let remove = manifold_editing::commands::graph::RemoveSceneObjectCommand::new(target, first, default);
        editing.execute(Box::new(remove), &mut project);
        assert!(editing.take_rejection().is_none());
        assert_eq!(handles(&project), ["Object 0 2", "Object 0 3"]);

        editing.execute(build_action(&project, queued).unwrap(), &mut project);
        assert!(editing.take_rejection().is_none());
        assert_eq!(
            handles(&project),
            ["Object 0 2", "Object 0 3", "Object 0 3 2"],
            "the copy is of the last object, which moved to slot 1"
        );
        let refusal = build_action(&project, queued_for_removed).expect_err("the removed object cannot be copied");
        assert!(refusal.contains("no longer"), "{refusal}");
    }

    /// A compound import wires two objects while the scene's count says one.
    /// Paste puts its copy on the next free physical port, not on the port the
    /// count names (BUG-vglg.6).
    #[test]
    fn paste_after_a_compound_import_takes_the_next_physical_port() {
        let mut graph = transfer_graph("compound", false);
        graph.nodes.push(node(6, "compound-part", "node.scene_object", "Part 2"));
        graph.wires.push(wire(6, "object", 1, "object_1"));
        let (mut project, layer) = project_with_graph(graph);
        let first = item_at(&project, &layer, SceneItemKind::Object, 0);
        let clipboard = SceneItemClipboard::capture(&project, &layer, SceneItemKind::Object, &first).unwrap();
        let paste = SceneItemAction::Paste { layer: layer.clone(), scene: 1, clipboard: Box::new(clipboard) };
        let mut editing = EditingService::new();
        editing.execute(build_action(&project, paste).unwrap(), &mut project);
        assert!(editing.take_rejection().is_none());

        let target = GraphTarget::Generator(layer);
        let def = crate::graph_target::resolve(&project, &target).unwrap();
        let mut ports: Vec<String> =
            def.wires.iter().filter(|w| w.to_node == 1 && w.to_port.starts_with("object_")).map(|w| w.to_port.clone()).collect();
        ports.sort();
        assert_eq!(ports, ["object_0", "object_1", "object_2"], "every object has its own port");
        let scene = def.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(scene.params.get("objects"), Some(&SerializedParamValue::Float { value: 3.0 }));
    }
}
