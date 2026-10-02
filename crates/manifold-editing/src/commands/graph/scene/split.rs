//! Physics scene command helpers extracted from `scene.rs`.

use super::*;

pub(super) fn split_capacity_value(value: &SerializedParamValue) -> SerializedParamValue {
    let raw = match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        _ => return value.clone(),
    };
    let pieces = (raw.max(0.0).ceil() as u32).div_ceil(8).div_ceil(3) * 3;
    SerializedParamValue::Int {
        value: pieces.max(36) as i32,
    }
}

pub(super) fn mutate_fragment_source(node: &mut EffectGraphNode, fragment_index: u32) -> Option<NodeId> {
    if node.type_id == "node.gltf_mesh_source" {
        node.params.insert(
            "fragment_count".to_string(),
            SerializedParamValue::Int { value: 8 },
        );
        node.params.insert(
            "fragment_index".to_string(),
            SerializedParamValue::Int {
                value: fragment_index as i32,
            },
        );
        if let Some(value) = node.params.get("max_capacity").cloned() {
            node.params
                .insert("max_capacity".to_string(), split_capacity_value(&value));
        }
        if let Some(value) = node.params.get("source_vertex_count").cloned() {
            node.params.insert(
                "source_vertex_count".to_string(),
                split_capacity_value(&value),
            );
        }
        return Some(node.node_id.clone());
    }
    node.group
        .as_deref_mut()?
        .nodes
        .iter_mut()
        .find_map(|child| mutate_fragment_source(child, fragment_index))
}

pub(super) fn find_node_id_in_tree(nodes: &[EffectGraphNode], type_id: &str) -> Option<u32> {
    nodes.iter().find_map(|node| {
        (node.type_id == type_id).then_some(node.id).or_else(|| {
            node.group
                .as_deref()
                .and_then(|group| find_node_id_in_tree(&group.nodes, type_id))
        })
    })
}

/// Replace one imported object with eight independently rendered and
/// simulated fragments.  The command snapshots the complete authored level,
/// so rejection (unsupported source or fewer than eight world slots) is
/// atomic and undo/redo restores the exact original wiring and exposures.
#[derive(Debug)]
pub struct SplitSceneObjectCommand {
    target: GraphTarget,
    render_scene_node_id: u32,
    object_index: u32,
    body_metadata: Vec<SceneParamMetadata>,
    world_metadata: Option<Vec<SceneParamMetadata>>,
    catalog_default: EffectGraphDef,
    prev: Option<(
        Vec<EffectGraphNode>,
        Vec<EffectGraphWire>,
        Option<PresetMetadata>,
    )>,
    rejection: Option<String>,
}

impl SplitSceneObjectCommand {
    pub fn new(
        target: GraphTarget,
        render_scene_node_id: u32,
        object_index: u32,
        body_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            render_scene_node_id,
            object_index,
            body_metadata,
            world_metadata: None,
            catalog_default,
            prev: None,
            rejection: None,
        }
    }

    pub fn with_world_metadata(mut self, metadata: Vec<SceneParamMetadata>) -> Self {
        self.world_metadata = Some(metadata);
        self
    }
}

impl Command for SplitSceneObjectCommand {
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.target.clone());
    }

    fn execute(&mut self, project: &mut Project) {
        self.prev = None;
        self.rejection = None;
        let Some(def) = project.graph_for_target(&self.target, Some(&self.catalog_default)) else {
            return;
        };
        let Ok(parts) = scene_object_parts(def, self.render_scene_node_id, self.object_index)
        else {
            self.rejection = Some("Split Object supports standard imported glTF objects only".into());
            return;
        };
        if parts.group_id.is_none() {
            self.rejection = Some(
                "Split supports imported object groups; group the object before splitting".into(),
            );
            return;
        }
        if parts.source.kind != ScenePhysicsSourceKind::Imported
            || parts
                .compound_sources
                .iter()
                .any(|source| source.kind != ScenePhysicsSourceKind::Imported)
        {
            self.rejection = Some(
                "Split supports imported glTF mesh sources only; builtin meshes cannot be split"
                    .into(),
            );
            return;
        }
        if parts.source.params.get("fragment_count").is_some_and(|value| match value {
            SerializedParamValue::Int { value } => *value > 1,
            SerializedParamValue::Float { value } => *value > 1.0,
            _ => false,
        }) {
            self.rejection = Some("This object is already a split piece".into());
            return;
        }
        let existing_binding = scene_physics_binding(def, &parts).ok();
        let existing_body = existing_binding.as_ref().and_then(|binding| {
            if let Some(group_id) = parts.group_id {
                def.nodes
                    .iter()
                    .find(|node| node.id == group_id)?
                    .group
                    .as_deref()?
                    .nodes
                    .iter()
                    .find(|node| node.id == binding.body_id)
            } else {
                def.nodes.iter().find(|node| node.id == binding.body_id)
            }
        });
        let body_float = |name: &str, fallback: f32| match existing_body
            .and_then(|node| node.params.get(name))
        {
            Some(SerializedParamValue::Float { value }) => *value,
            Some(SerializedParamValue::Int { value }) => *value as f32,
            _ => fallback,
        };
        // Pieces keep the parent's density, so their masses sum to the parent's.
        let density = existing_body.and_then(|node| node.params.get("density")).cloned();
        let friction = body_float("friction", 0.5);
        let bounce = body_float("bounce", 0.15);
        let body_params = match scene_body_params(&parts.source, def) {
            Ok(mut params) => {
                params.insert(
                    "motion".to_string(),
                    SerializedParamValue::Enum { value: 1 },
                );
                if let Some(density) = density.clone() {
                    params.insert("density".to_string(), density);
                }
                params.insert(
                    "friction".to_string(),
                    SerializedParamValue::Float { value: friction },
                );
                params.insert(
                    "bounce".to_string(),
                    SerializedParamValue::Float { value: bounce },
                );
                params.insert(
                    "collider_parts".to_string(),
                    SerializedParamValue::Int { value: 1 },
                );
                params
            }
            Err(reason) => {
                self.rejection = Some(reason.into());
                return;
            }
        };
        let worlds: Vec<u32> = def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.physics_world")
            .map(|node| node.id)
            .collect();
        if worlds.len() > 1 {
            self.rejection = Some("Split Object requires one shared root Physics World".into());
            return;
        }
        let world_id = worlds
            .first()
            .copied()
            .unwrap_or_else(|| max_node_id_over(&def.nodes).saturating_add(1));
        let mut free_slots = Vec::new();
        if let Some(binding) = existing_binding.as_ref() {
            free_slots.push(binding.body_slot);
        }
        for slot in 0..PHYSICS_BODY_SLOTS {
            if existing_binding
                .as_ref()
                .is_some_and(|binding| binding.body_slot == slot)
            {
                continue;
            }
            if physics_body_slot_available(&def.wires, world_id, slot) {
                free_slots.push(slot);
            }
            if free_slots.len() == 8 {
                break;
            }
        }
        if free_slots.len() != 8 {
            self.rejection = Some("Physics World has fewer than eight free body slots".into());
            return;
        }
        let original_strings = def
            .preset_metadata
            .as_ref()
            .map(|meta| meta.string_bindings.clone())
            .unwrap_or_default();
        let mut candidate = def.clone();
        let result = (|| {
            let def = &mut candidate;
            let previous = (
                def.nodes.clone(),
                def.wires.clone(),
                def.preset_metadata.clone(),
            );
            let mut next_id = max_node_id_over(&def.nodes).checked_add(1)?;
            let mut taken = std::collections::HashSet::new();
            collect_all_handles(&def.nodes, &mut taken);
            if worlds.is_empty() {
                let handle = dedup_handle("Physics World", &mut taken);
                def.nodes.push(fresh_scene_node(
                    next_id,
                    "node.physics_world",
                    Some(handle),
                    BTreeMap::new(),
                ));
                next_id += 1;
            }
            let mut source_node = def
                .nodes
                .iter()
                .find(|node| node.id == parts.producer_id)?
                .clone();
            let mut removed = Vec::new();
            collect_node_ids(std::slice::from_ref(&source_node), &mut removed);
            if existing_binding.is_some() {
                strip_group_physics_for_split(source_node.group.as_deref_mut()?).ok()?;
            }
            let source_count = match def
                .nodes
                .iter()
                .find(|node| node.id == self.render_scene_node_id)?
                .params
                .get("objects")
            {
                Some(SerializedParamValue::Float { value }) => *value as u32,
                Some(SerializedParamValue::Int { value }) => (*value).max(0) as u32,
                _ => return None,
            };
            if self.object_index >= source_count {
                return None;
            }
            let inherited_fields: Vec<_> =
                existing_binding.as_ref().map_or_else(Vec::new, |binding| {
                    let field_port = format!("body_acceleration_{}", binding.body_slot);
                    def.wires
                        .iter()
                        .filter(|wire| {
                            wire.to_node == binding.world_id && wire.to_port == field_port
                        })
                        .cloned()
                        .collect()
                });
            if let Some(binding) = existing_binding.as_ref() {
                def.wires.retain(|wire| {
                    !(wire.to_node == binding.world_id
                        && (wire.to_port == format!("body_{}", binding.body_slot)
                            || wire.to_port == format!("body_acceleration_{}", binding.body_slot))
                        || wire.from_node == binding.world_id
                            && wire.from_port == format!("pose_{}", binding.body_slot)
                            && wire.to_node == parts.producer_id)
                        && !(wire.from_node == binding.body_id && wire.to_node == binding.world_id)
                        && !(wire.from_node == parts.authored_transform_id
                            && wire.to_node == binding.body_id)
                });
                if parts.group_id.is_none() {
                    def.nodes.retain(|node| node.id != binding.body_id);
                }
            }
            def.wires.retain(|wire| {
                wire.from_node != parts.producer_id
                    && !(wire.to_node == self.render_scene_node_id
                        && wire.to_port == format!("object_{}", self.object_index))
            });
            // The eight fragments occupy the replaced slot and the seven
            // additional slots immediately after it. Existing objects move
            // upward by seven slots.
            for wire in &mut def.wires {
                if wire.to_node == self.render_scene_node_id
                    && let Some(index) = wire
                        .to_port
                        .strip_prefix("object_")
                        .and_then(|s| s.parse::<u32>().ok())
                    && index > self.object_index
                {
                    wire.to_port = format!("object_{}", index + 7);
                }
            }
            let mut body_entries = Vec::new();
            for (fragment_index, body_slot) in free_slots.iter().copied().enumerate() {
                let mut map = Vec::new();
                let mut clone =
                    deep_clone_with_fresh_ids(&source_node, &mut next_id, &mut taken, &mut map);
                let piece_name = dedup_handle(&format!("{} Piece {}", parts.object_handle, fragment_index + 1), &mut taken);
                clone.handle = Some(piece_name.clone());
                if let Some(group) = clone.group.as_deref_mut() {
                    group.nodes.iter_mut().find(|n| n.type_id == "node.scene_object")?.handle = Some(piece_name.clone());
                }
                let fragment_object_id =
                    find_node_id_in_tree(std::slice::from_ref(&clone), "node.scene_object")?;
                let fragment_transform_id =
                    find_node_id_in_tree(std::slice::from_ref(&clone), "node.transform_3d")?;
                mutate_fragment_source(&mut clone, fragment_index as u32)?;
                let mut cloned_source_params = clone_fragment_body_params(&body_params);
                cloned_source_params.insert(
                    "fragment_count".to_string(),
                    SerializedParamValue::Int { value: 8 },
                );
                cloned_source_params.insert(
                    "fragment_index".to_string(),
                    SerializedParamValue::Int {
                        value: fragment_index as i32,
                    },
                );
                cloned_source_params.insert(
                    "collider_parts".to_string(),
                    SerializedParamValue::Int { value: 1 },
                );
                let body_id = next_id;
                next_id += 1;
                let body_handle = dedup_handle(
                    &format!(
                        "{} Piece {} Physics",
                        parts.object_handle,
                        fragment_index + 1
                    ),
                    &mut taken,
                );
                let body_node_id = if let Some(group) = clone.group.as_deref_mut() {
                    let input_id = next_id;
                    next_id += 1;
                    let output_id = next_id;
                    next_id += 1;
                    let inner_transform = find_node_id_in_tree(&group.nodes, "node.transform_3d")?;
                    let inner_object = find_node_id_in_tree(&group.nodes, "node.scene_object")?;
                    let (body_node_id, _) = add_group_physics(
                        group,
                        body_id,
                        cloned_source_params.clone(),
                        body_handle,
                        input_id,
                        output_id,
                        inner_transform,
                        inner_object,
                        None,
                    )
                    .ok()?;
                    body_node_id
                } else {
                    let body = fresh_scene_node(
                        body_id,
                        "node.rigid_body",
                        Some(body_handle),
                        cloned_source_params.clone(),
                    );
                    let body_node_id = body.node_id.clone();
                    clone_fragment_root_wires(
                        &mut def.wires,
                        fragment_object_id,
                        fragment_transform_id,
                        world_id,
                        body_slot,
                        body_id,
                    );
                    clone.group = None;
                    // The body is a root-level producer for a bare object.
                    def.nodes.push(body);
                    body_node_id
                };
                let clone_id = clone.id;
                def.nodes.push(clone);
                if parts.group_id.is_some() {
                    def.wires.push(scene_build_wire(
                        clone_id,
                        "body",
                        world_id,
                        &format!("body_{body_slot}"),
                    ));
                    def.wires.push(scene_build_wire(
                        world_id,
                        &format!("pose_{body_slot}"),
                        clone_id,
                        "pose",
                    ));
                }
                for source in &inherited_fields {
                    let mut field = source.clone();
                    if field.from_node == parts.producer_id {
                        field.from_node = clone_id;
                    }
                    field.to_port = format!("body_acceleration_{body_slot}");
                    def.wires.push(field);
                }
                def.wires.push(scene_build_wire(
                    clone_id,
                    "object",
                    self.render_scene_node_id,
                    &format!("object_{}", self.object_index + fragment_index as u32),
                ));
                if let Some(meta) = def.preset_metadata.as_mut() {
                    for binding in &original_strings {
                        if let BindingTarget::Node { node_id, param } = &binding.target
                            && let Some((_, new_id)) = map.iter().find(|(old, _)| old == node_id)
                        {
                            let mut copied = binding.clone();
                            copied.target = BindingTarget::Node {
                                node_id: new_id.clone(),
                                param: param.clone(),
                            };
                            meta.string_bindings.push(copied);
                        }
                    }
                }
                if let Some(binding) =
                    original_strings
                        .iter()
                        .find_map(|binding| match &binding.target {
                            BindingTarget::Node { node_id, param }
                                if node_id == &parts.source.node_id && param == "path" =>
                            {
                                Some(StringBindingDef {
                                    id: binding.id.clone(),
                                    label: binding.label.clone(),
                                    default_value: binding.default_value.clone(),
                                    target: BindingTarget::Node {
                                        node_id: body_node_id.clone(),
                                        param: "path".to_string(),
                                    },
                                })
                            }
                            _ => None,
                        })
                    && let Some(meta) = def.preset_metadata.as_mut()
                {
                    meta.string_bindings.push(binding);
                }
                clone_sections::clone_scene_bindings(def, &map);
                // Separate sections keep every piece independently editable.
                if let Some(meta) = def.preset_metadata.as_mut() {
                    let ids: std::collections::HashSet<_> = meta.bindings.iter().filter_map(|binding| {
                        matches!(&binding.target, BindingTarget::Node { node_id, .. } if map.iter().any(|(_, new)| new == node_id)).then_some(binding.id.clone())
                    }).collect();
                    for param in &mut meta.params {
                        if ids.contains(&param.id) {
                            param.section = Some(format!("{} — {}", piece_name, param.section.as_deref().unwrap_or("Object")));
                        }
                    }
                }
                body_entries.push((body_id, body_node_id, cloned_source_params));
            }
            let render = def
                .nodes
                .iter_mut()
                .find(|node| node.id == self.render_scene_node_id)?;
            render.params.insert(
                "objects".to_string(),
                SerializedParamValue::Float {
                    value: (source_count + 7) as f32,
                },
            );
            def.nodes.retain(|node| node.id != parts.producer_id);
            prune_scene_object_metadata(def, &removed);
            for id in &removed {
                remove_string_binding_target(def, id);
            }
            if let Some(meta) = def.preset_metadata.as_mut() {
                for (index, (body_id, body_node_id, params)) in body_entries.iter().enumerate() {
                    stamp_scene_node_exposures_into(
                        &mut meta.params,
                        &mut meta.bindings,
                        *body_id,
                        body_node_id,
                        "node.rigid_body",
                        &format!("{} Piece {} — Physics", parts.object_handle, index + 1),
                        &self.body_metadata,
                        params,
                    );
                }
            }
            if worlds.is_empty()
                && let (Some(world), Some(world_metadata)) = (
                    def.nodes.iter().find(|node| node.id == world_id),
                    self.world_metadata.as_ref(),
                )
                && let Some(meta) = def.preset_metadata.as_mut()
            {
                stamp_scene_node_exposures_into(
                    &mut meta.params,
                    &mut meta.bindings,
                    world.id,
                    &world.node_id,
                    "node.physics_world",
                    "Physics World",
                    world_metadata,
                    &world.params,
                );
            }
            Some(previous)
        })();
        if let Some(previous) = result {
            let _ =
                with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
                    *def = candidate
                });
            self.prev = Some(previous);
            refresh_target_manifest(project, &self.target);
        } else {
            self.rejection =
                Some("Physics edit requires an unmodified scene object graph".into());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((nodes, wires, metadata)) = self.prev.take() else {
            return;
        };
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.nodes = nodes;
            def.wires = wires;
            def.preset_metadata = metadata;
        });
        refresh_target_manifest(project, &self.target);
    }
    fn description(&self) -> &str {
        "Split Object into 8 Pieces"
    }
    fn was_applied(&self) -> bool {
        self.prev.is_some()
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
}

pub(super) fn clone_fragment_body_params(
    body_params: &BTreeMap<String, SerializedParamValue>,
) -> BTreeMap<String, SerializedParamValue> {
    body_params.clone()
}

pub(super) fn clone_fragment_root_wires(
    wires: &mut Vec<EffectGraphWire>,
    object_id: u32,
    transform_id: u32,
    world_id: u32,
    body_slot: u32,
    body_id: u32,
) {
    wires.push(scene_build_wire(
        world_id,
        &format!("pose_{body_slot}"),
        object_id,
        "transform",
    ));
    wires.push(scene_build_wire(
        transform_id,
        "transform",
        body_id,
        "transform",
    ));
    wires.push(scene_build_wire(
        body_id,
        "body",
        world_id,
        &format!("body_{body_slot}"),
    ));
}

pub(super) fn remap_physics_wire(
    wire: &EffectGraphWire,
    node_map: &std::collections::HashMap<u32, u32>,
    world_id: u32,
    old_slot: u32,
    new_slot: u32,
    render_id: u32,
    old_object_indices: &[u32],
    new_object_index: u32,
) -> EffectGraphWire {
    let map_node = |id: u32| node_map.get(&id).copied().unwrap_or(id);
    let mut from_port = wire.from_port.clone();
    let mut to_port = wire.to_port.clone();
    if wire.from_node == world_id && wire.from_port == format!("pose_{old_slot}") {
        from_port = format!("pose_{new_slot}");
    }
    if wire.to_node == world_id && wire.to_port == format!("body_{old_slot}") {
        to_port = format!("body_{new_slot}");
    }
    if wire.to_node == world_id && wire.to_port == format!("body_acceleration_{old_slot}") {
        to_port = format!("body_acceleration_{new_slot}");
    }
    if wire.to_node == render_id
        && let Some(old_index) = wire
            .to_port
            .strip_prefix("object_")
            .and_then(|value| value.parse::<u32>().ok())
        && let Some(part) = old_object_indices.iter().position(|index| *index == old_index)
    {
        to_port = format!("object_{}", new_object_index + part as u32);
    }
    EffectGraphWire {
        from_node: map_node(wire.from_node),
        from_port,
        to_node: map_node(wire.to_node),
        to_port,
    }
}

pub(super) fn append_physics_duplicate(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    physics: &PhysicsSceneObject,
    render_id: u32,
    source_indices: &[u32],
    new_index: u32,
    new_slot: u32,
    node_id_map: &mut Vec<(NodeId, NodeId)>,
) -> Option<()> {
    let mut next_id = max_node_id_over(nodes) + 1;
    let mut taken = std::collections::HashSet::new();
    collect_all_handles(nodes, &mut taken);
    let owned = physics
        .owned_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut clones = std::collections::HashMap::<u32, EffectGraphNode>::new();
    for old_id in &physics.owned_ids {
        let source = nodes.iter().find(|node| node.id == *old_id)?;
        let clone = deep_clone_with_fresh_ids(source, &mut next_id, &mut taken, node_id_map);
        clones.insert(*old_id, clone);
    }
    let source_object = nodes.iter().find(|node| node.id == physics.object_id)?;
    let cloned_handle = source_object.handle.as_ref().map(|handle| {
        let mut suffix = 2;
        loop {
            let candidate = format!("{handle} {suffix}");
            if !taken.contains(&candidate) {
                break candidate;
            }
            suffix += 1;
        }
    });
    if let Some(clone) = clones.get_mut(&physics.object_id) {
        clone.handle = cloned_handle;
        clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));
    }
    if let Some(clone) = clones.get_mut(&physics.transform_id) {
        let current = match clone.params.get("pos_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        };
        clone.params.insert(
            "pos_x".to_string(),
            SerializedParamValue::Float {
                value: current + 0.5,
            },
        );
    }
    if physics.grouped
        && let Some(clone) = clones.get_mut(&physics.object_id)
        && let Some(transform) = clone.group.as_deref_mut().and_then(|group| {
            group
                .nodes
                .iter_mut()
                .find(|node| node.type_id == "node.transform_3d")
        })
    {
        let current = match transform.params.get("pos_x") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        };
        transform.params.insert(
            "pos_x".to_string(),
            SerializedParamValue::Float {
                value: current + 0.5,
            },
        );
    }

    let mut node_map = std::collections::HashMap::new();
    for (old_id, clone) in &clones {
        node_map.insert(*old_id, clone.id);
    }
    for old_id in &physics.owned_ids {
        nodes.push(clones.remove(old_id)?);
    }
    for wire in wires.clone() {
        if owned.contains(&wire.from_node)
            || owned.contains(&wire.to_node)
            || (wire.to_node == physics.world_id
                && wire.to_port == format!("body_acceleration_{}", physics.body_slot))
        {
            wires.push(remap_physics_wire(
                &wire,
                &node_map,
                physics.world_id,
                physics.body_slot,
                new_slot,
                render_id,
                source_indices,
                new_index,
            ));
        }
    }
    Some(())
}


