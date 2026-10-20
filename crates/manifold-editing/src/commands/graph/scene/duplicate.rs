//! Duplicate one scene object N times as one undo step (SCENE_OBJECT_AND_PANEL_V2_DESIGN
//! D11, PHYSICS_ENGINE_BOUNDARY_DESIGN.md P6a). The copies are one pure graph
//! edit committed through the shared scene transaction.

use super::*;
use manifold_core::change::ChangeSet;
use manifold_core::scene_graph_edit::{SceneGraphEdit, SceneGraphEditError};
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_core::scene_surface::{MAX_DUPLICATE_COPIES, SceneObjectRowKind, copies_shortfall};

use crate::scene_transaction::SceneGraphTransaction;

/// How far each copy sits along X from the one before it (D11).
const COPY_OFFSET_X: f32 = 0.5;

/// Duplicate one scene object `copies` times. Each copy is a deep clone with
/// fresh document ids and NodeIds, named "<name> N" (the first free N from 2),
/// wired into the next free render slot and placed 0.5 m along X from the
/// copy before it. A physics object's copy takes the next free body slot of
/// the same Physics World and the forces on the source body; when the world
/// has fewer free slots than copies, nothing changes. Water and a Physics
/// World's copies object refuse.
#[derive(Debug)]
pub struct DuplicateSceneObjectCommand {
    transaction: SceneGraphTransaction,
    /// The copies' card values, with a moved transform's offset applied;
    /// written after each commit and undone with the instance snapshot.
    values: Vec<(String, f32)>,
}

impl DuplicateSceneObjectCommand {
    pub fn new(
        project: &Project,
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        source_index: u32,
        copies: u32,
        catalog_default: &EffectGraphDef,
    ) -> Result<Self, SceneGraphEditError> {
        let mut copy_maps = Vec::new();
        let transaction =
            SceneGraphTransaction::prepare(project, target.clone(), catalog_default, "Duplicate Object", |graph| {
                let (edit, maps) =
                    duplicate_scene_object_edit(graph, &scope_path, render_scene_node_id, source_index, copies)?;
                copy_maps = maps;
                Ok(edit)
            })?
            .without_ableton_mappings();
        let refuse = |message: String| SceneGraphEditError { at: None, message };
        let owner = project
            .graph_target_owner(&target)
            .ok_or_else(|| refuse("Duplicate Object parameter owner is unavailable".into()))?;
        let mut values = Vec::new();
        for map in &copy_maps {
            values.extend(clone_values::prepare(owner, transaction.prepared_graph(), map).map_err(refuse)?);
        }
        Ok(Self { transaction, values })
    }
}

impl Command for DuplicateSceneObjectCommand {
    fn touches(&self) -> ChangeSet {
        self.transaction.touches()
    }
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.transaction.owner.clone());
    }
    fn do_execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
        if self.transaction.was_applied()
            && let Some(owner) = project.graph_target_owner_mut(&self.transaction.owner)
        {
            for (id, value) in &self.values {
                owner.set_base_param_from_automation(id, *value);
            }
        }
    }
    fn do_undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
    fn rejection_reason(&self) -> Option<&str> {
        self.transaction.rejection_reason()
    }
}

/// Free body slots in the Physics World that render slot `object_index` of a
/// root-level scene simulates in; `None` when that object has no standard
/// physics. The card reads it to say why a copy count cannot fit; `worlds`
/// is read once per sync for every card.
pub fn physics_free_body_slots(
    graph: &EffectGraphDef,
    worlds: &WorldBodySlots,
    render_scene_node_id: u32,
    object_index: u32,
) -> Option<u32> {
    let source = object_producer_id(&graph.wires, render_scene_node_id, object_index)?;
    match physics_scene_object_match(&graph.nodes, &graph.wires, render_scene_node_id, object_index, source) {
        PhysicsSceneObjectMatch::Valid(physics) if !physics.copies => worlds.get(physics.world_id).map(BodySlots::free),
        _ => None,
    }
}

/// The render scene at `scope` (document ids, root first) as a stable reference.
fn scene_reference(graph: &EffectGraphDef, scope: &[u32], render_id: u32) -> Option<SceneNodeRef> {
    let mut nodes = graph.nodes.as_slice();
    let mut stable = Vec::with_capacity(scope.len());
    for group_id in scope {
        let group = nodes.iter().find(|node| node.id == *group_id)?;
        stable.push(group.node_id.clone());
        nodes = group.group.as_deref()?.nodes.as_slice();
    }
    let scene = nodes.iter().find(|node| node.id == render_id)?;
    Some(SceneNodeRef { scope: stable, node: scene.node_id.clone() })
}

/// Source node id to copy node id, for one copy.
type NodeIdMap = Vec<(NodeId, NodeId)>;
/// Each copy's map from source NodeIds to its fresh ones, in copy order.
type CopyMaps = Vec<NodeIdMap>;

/// The pure duplicate edit and its copies' NodeId maps.
fn duplicate_scene_object_edit(
    graph: &EffectGraphDef,
    scope: &[u32],
    render_id: u32,
    source_index: u32,
    copies: u32,
) -> Result<(SceneGraphEdit, CopyMaps), SceneGraphEditError> {
    let refuse = |message: &str| SceneGraphEditError { at: None, message: message.into() };
    if copies == 0 || copies > MAX_DUPLICATE_COPIES {
        return Err(SceneGraphEditError {
            at: None,
            message: format!("Duplicate Object makes 1 to {MAX_DUPLICATE_COPIES} copies"),
        });
    }
    let scene = scene_reference(graph, scope, render_id).ok_or_else(|| refuse("Duplicate Object scene is unavailable"))?;
    // The one scene-object model decides what may be copied, so a shortcut
    // cannot copy what the card hides: Water and its looks refuse.
    let models = manifold_core::scene_object_model::scene_object_models(graph, &scene)?;
    if let Some(model) = models.iter().find(|model| model.slot == Some(source_index))
        && !model.capabilities.duplicate
    {
        return Err(refuse(if model.liquid_domain.is_some() || model.kind != SceneObjectRowKind::Object {
            "Water cannot be duplicated"
        } else {
            "This object cannot be duplicated"
        }));
    }
    let (nodes, wires) = graph_level(graph, scope).ok_or_else(|| refuse("Duplicate Object scene scope is unavailable"))?;
    let source_id =
        object_producer_id(wires, render_id, source_index).ok_or_else(|| refuse("Duplicate Object source object is unavailable"))?;
    let physics = match physics_scene_object_match(nodes, wires, render_id, source_index, source_id) {
        PhysicsSceneObjectMatch::Malformed(reason) => return Err(refuse(reason)),
        PhysicsSceneObjectMatch::NotPhysics => None,
        PhysicsSceneObjectMatch::Valid(physics) => {
            if !scope.is_empty() {
                return Err(refuse("Duplicate Object physics ownership requires a root-level scene"));
            }
            if physics.copies {
                return Err(refuse("Duplicate Object cannot duplicate a Physics World copies object"));
            }
            if let Some(message) = copies_shortfall(copies, BodySlots::of(wires, physics.world_id).free()) {
                return Err(SceneGraphEditError { at: None, message });
            }
            Some(physics)
        }
    };
    let source_group = nodes.iter().any(|node| node.id == source_id && node.type_id == GROUP_TYPE_ID);

    let mut candidate = graph.clone();
    let mut maps = Vec::with_capacity(copies as usize);
    let mut remaps = Vec::new();
    for copy in 1..=copies {
        let offset = COPY_OFFSET_X * copy as f32;
        let (map, cloned_group) =
            append_copy(&mut candidate, scope, render_id, source_index, physics.as_ref(), offset).map_err(refuse)?;
        if let Some(cloned_group) = cloned_group.filter(|_| source_group && scope.is_empty()) {
            fluid::duplicate_scene_object_fluid_roles(&mut candidate, source_id, cloned_group, &map)
                .map_err(|message| SceneGraphEditError { at: None, message })?;
        }
        clone_string_bindings(&mut candidate, &map);
        remaps.extend(clone_sections::clone_scene_bindings(&mut candidate, &map));
        maps.push(map);
    }
    Ok((SceneGraphEdit { graph: candidate, removed_param_ids: Vec::new(), parameter_id_remaps: remaps }, maps))
}

/// The first "<base> N" from 2 no node uses yet.
fn numbered_copy_name(base: &str, taken: &mut std::collections::HashSet<String>) -> String {
    let name = (2..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unbounded range always finds a free name");
    taken.insert(name.clone());
    name
}

fn offset_pos_x(node: &mut EffectGraphNode, offset: f32) {
    let current = match node.params.get("pos_x") {
        Some(SerializedParamValue::Float { value }) => *value,
        _ => 0.0,
    };
    node.params.insert("pos_x".to_string(), SerializedParamValue::Float { value: current + offset });
}

/// Append one copy of render slot `src_k`'s object at `offset` metres along
/// X: its nodes, wires, render slots and object count. Returns the copy's
/// NodeId map and, for a grouped source, the copy's group id.
fn append_copy(
    def: &mut EffectGraphDef,
    scope: &[u32],
    render_id: u32,
    src_k: u32,
    physics: Option<&PhysicsSceneObject>,
    offset: f32,
) -> Result<(NodeIdMap, Option<u32>), &'static str> {
    // Document ids and handles are global even when the edit is addressed
    // through a nested scope, so both allocators see the whole tree.
    let mut next_id = max_node_id_over(&def.nodes).checked_add(1).ok_or("Duplicate Object document ids are exhausted")?;
    let mut taken = std::collections::HashSet::new();
    collect_all_handles(&def.nodes, &mut taken);
    let (nodes, wires) =
        descend_level(&mut def.nodes, &mut def.wires, scope).ok_or("Duplicate Object scene scope is unavailable")?;
    let new_k = scene_object_append_slot(nodes, wires, render_id)?;
    let source_id = object_producer_id(wires, render_id, src_k).ok_or("Duplicate Object source object is unavailable")?;
    let count = match physics {
        Some(physics) => physics.render_indices.len(),
        None => nodes
            .iter()
            .find(|node| node.id == source_id)
            .filter(|node| node.type_id == "node.scene_object")
            .map(|_| {
                wires
                    .iter()
                    .filter(|wire| {
                        wire.from_node == source_id
                            && wire.to_node == render_id
                            && (wire.to_port == "object" || wire.to_port.starts_with("object_"))
                    })
                    .count()
            })
            .unwrap_or_else(|| group_render_indices(wires, render_id, source_id).len()),
    };
    let count = u32::try_from(count).map_err(|_| "Duplicate Object object count is exhausted")?;
    let new_count = new_k
        .checked_add(count)
        .filter(|end| *end as f32 as u32 == *end)
        .ok_or("Duplicate Object object count is exhausted")?;
    if count == 0
        || (new_k..new_count).any(|index| {
            wires.iter().any(|wire| wire.to_node == render_id && wire.to_port == format!("object_{index}"))
        })
    {
        return Err("Duplicate Object destination slots are unavailable");
    }
    let mut node_id_map = Vec::new();
    let mut cloned_group = None;
    if let Some(physics) = physics {
        let new_slot = first_free_physics_body_slot(wires, physics.world_id).ok_or("The Physics World has no free body slot")?;
        let copy = PhysicsCopy {
            next_id: &mut next_id,
            taken: &mut taken,
            node_id_map: &mut node_id_map,
            new_index: new_k,
            new_slot,
            offset,
        };
        append_physics_duplicate(nodes, wires, physics, render_id, copy)?;
    } else {
        let source_node = nodes.iter().find(|node| node.id == source_id).ok_or("Duplicate Object source object is unavailable")?.clone();
        let source_outputs = source_render_outputs(wires, source_id, render_id, source_node.type_id == "node.scene_object");
        if source_outputs.is_empty() {
            return Err("Duplicate Object source object is unavailable");
        }
        let cloned_handle = source_node.handle.as_deref().map(|handle| numbered_copy_name(handle, &mut taken));
        let clone_id = if source_node.type_id == "node.scene_object" {
            // A loose object copies its exclusive upstream chain; shared
            // producers stay in place and feed the copy too.
            let owned = loose_scene_object_owned_ids(nodes, wires, source_id);
            if owned.is_empty() {
                return Err("Duplicate Object source object is unavailable");
            }
            let mut clones = Vec::new();
            let mut numeric_map = std::collections::HashMap::new();
            let mut offset_applied = false;
            for source in nodes.iter().filter(|node| owned.contains(&node.id)) {
                let mut clone = deep_clone_with_fresh_ids(source, &mut next_id, &mut taken, &mut node_id_map);
                if source.id == source_id {
                    clone.handle = cloned_handle.clone();
                    clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));
                }
                if clone.type_id == "node.transform_3d" && !offset_applied {
                    offset_applied = true;
                    offset_pos_x(&mut clone, offset);
                }
                numeric_map.insert(source.id, clone.id);
                clones.push(clone);
            }
            let clone_id = *numeric_map.get(&source_id).ok_or("Duplicate Object source object is unavailable")?;
            let cloned_wires: Vec<_> = wires
                .iter()
                .filter_map(|wire| {
                    let to_node = numeric_map.get(&wire.to_node).copied()?;
                    let from_node = numeric_map.get(&wire.from_node).copied().unwrap_or(wire.from_node);
                    Some(EffectGraphWire { from_node, from_port: wire.from_port.clone(), to_node, to_port: wire.to_port.clone() })
                })
                .collect();
            nodes.extend(clones);
            wires.extend(cloned_wires);
            clone_id
        } else {
            let mut clone = deep_clone_with_fresh_ids(&source_node, &mut next_id, &mut taken, &mut node_id_map);
            clone.handle = cloned_handle.clone();
            clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));
            // D6: a grouped object's name follows its group's.
            if let Some(body) = clone.group.as_deref_mut() {
                if let Some(inner_object) = body.nodes.iter_mut().find(|node| node.type_id == "node.scene_object") {
                    inner_object.handle = cloned_handle;
                }
                if let Some(transform) = body.nodes.iter_mut().find(|node| node.type_id == "node.transform_3d") {
                    offset_pos_x(transform, offset);
                }
            }
            let clone_id = clone.id;
            if source_node.type_id == GROUP_TYPE_ID {
                cloned_group = Some(clone_id);
                // Group inputs are shared upstream signals, such as World
                // controls: the copy's boundary takes them too.
                let incoming: Vec<_> = wires
                    .iter()
                    .filter(|wire| wire.to_node == source_id)
                    .map(|wire| EffectGraphWire { to_node: clone_id, ..wire.clone() })
                    .collect();
                wires.extend(incoming);
            }
            nodes.push(clone);
            clone_id
        };
        for (part, (_, from_port)) in source_outputs.iter().enumerate() {
            wires.push(scene_build_wire(clone_id, from_port, render_id, &format!("object_{}", new_k + part as u32)));
        }
    }
    nodes
        .iter_mut()
        .find(|node| node.id == render_id)
        .ok_or("Duplicate Object render scene is unavailable")?
        .params
        .insert("objects".to_string(), SerializedParamValue::Float { value: new_count as f32 });
    Ok((node_id_map, cloned_group))
}

/// The source's render outputs as (slot, output port), in slot order. A bare
/// `object` port counts as slot 0 for a loose object only.
fn source_render_outputs(wires: &[EffectGraphWire], source_id: u32, render_id: u32, loose: bool) -> Vec<(u32, String)> {
    let mut outputs: Vec<(u32, String)> = wires
        .iter()
        .filter(|wire| wire.from_node == source_id && wire.to_node == render_id)
        .filter_map(|wire| {
            let index = match wire.to_port.strip_prefix("object_") {
                Some(index) => index.parse().ok()?,
                None if loose && wire.to_port == "object" => 0,
                None => return None,
            };
            Some((index, wire.from_port.clone()))
        })
        .collect();
    outputs.sort_unstable();
    outputs.dedup_by_key(|(index, _)| *index);
    outputs
}

/// Clone every string binding (the importer's Model File path, one entry
/// per file-dependent node) whose target the copy cloned, retargeted at the
/// copy (BUG-212 (duplicate drops the model file)). Same id, label and
/// default, so one Model File still drives every copy.
fn clone_string_bindings(def: &mut EffectGraphDef, node_id_map: &[(NodeId, NodeId)]) {
    let Some(meta) = def.preset_metadata.as_mut() else { return };
    let copies: Vec<StringBindingDef> = meta
        .string_bindings
        .iter()
        .filter_map(|binding| {
            let BindingTarget::Node { node_id, param } = &binding.target else { return None };
            let (_, new_id) = node_id_map.iter().find(|(old, _)| old == node_id)?;
            Some(StringBindingDef {
                id: binding.id.clone(),
                label: binding.label.clone(),
                default_value: binding.default_value.clone(),
                target: BindingTarget::Node { node_id: new_id.clone(), param: param.clone() },
            })
        })
        .collect();
    meta.string_bindings.extend(copies);
}

/// One physics copy: the id allocators it draws from and where it goes.
struct PhysicsCopy<'a> {
    next_id: &'a mut u32,
    taken: &'a mut std::collections::HashSet<String>,
    node_id_map: &'a mut Vec<(NodeId, NodeId)>,
    /// The copy's first render slot.
    new_index: u32,
    /// The world body slot the copy's body takes.
    new_slot: u32,
    /// Metres along X from the source.
    offset: f32,
}

/// Append one copy of a physics object's owned chain: its body moves to
/// `new_slot` of the same world and the forces on the source body follow it.
fn append_physics_duplicate(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    physics: &PhysicsSceneObject,
    render_id: u32,
    copy: PhysicsCopy<'_>,
) -> Result<(), &'static str> {
    let PhysicsCopy { next_id, taken, node_id_map, new_index, new_slot, offset } = copy;
    let unavailable = "Duplicate Object physics source is unavailable";
    let owned: std::collections::HashSet<u32> = physics.owned_ids.iter().copied().collect();
    let mut clones = std::collections::HashMap::<u32, EffectGraphNode>::new();
    for old_id in &physics.owned_ids {
        let source = nodes.iter().find(|node| node.id == *old_id).ok_or(unavailable)?;
        clones.insert(*old_id, deep_clone_with_fresh_ids(source, next_id, taken, node_id_map));
    }
    let source_object = nodes.iter().find(|node| node.id == physics.object_id).ok_or(unavailable)?;
    let cloned_handle = source_object.handle.as_deref().map(|handle| numbered_copy_name(handle, taken));
    if let Some(clone) = clones.get_mut(&physics.object_id) {
        clone.handle = cloned_handle.clone();
        clone.editor_pos = clone.editor_pos.map(|(x, y)| (x + 40.0, y + 40.0));
        if physics.grouped
            && let Some(body) = clone.group.as_deref_mut()
        {
            if let Some(inner_object) = body.nodes.iter_mut().find(|node| node.type_id == "node.scene_object") {
                inner_object.handle = cloned_handle;
            }
            if let Some(transform) = body.nodes.iter_mut().find(|node| node.type_id == "node.transform_3d") {
                offset_pos_x(transform, offset);
            }
        }
    }
    if let Some(clone) = clones.get_mut(&physics.transform_id) {
        offset_pos_x(clone, offset);
    }
    let node_map: std::collections::HashMap<u32, u32> = clones.iter().map(|(old, clone)| (*old, clone.id)).collect();
    for old_id in &physics.owned_ids {
        nodes.push(clones.remove(old_id).ok_or(unavailable)?);
    }
    let source_force = format!("body_acceleration_{}", physics.body_slot);
    let copied: Vec<_> = wires
        .iter()
        .filter(|wire| {
            owned.contains(&wire.from_node)
                || owned.contains(&wire.to_node)
                || (wire.to_node == physics.world_id && wire.to_port == source_force)
        })
        .map(|wire| remap_physics_wire(wire, &node_map, physics, new_slot, render_id, new_index))
        .collect();
    wires.extend(copied);
    Ok(())
}

/// One source wire as the copy's: copied nodes swap in, the world's body,
/// pose and force ports move to `new_slot`, render slots move to the copy's.
fn remap_physics_wire(
    wire: &EffectGraphWire,
    node_map: &std::collections::HashMap<u32, u32>,
    physics: &PhysicsSceneObject,
    new_slot: u32,
    render_id: u32,
    new_object_index: u32,
) -> EffectGraphWire {
    let (world_id, old_slot) = (physics.world_id, physics.body_slot);
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
        && let Some(old_index) = wire.to_port.strip_prefix("object_").and_then(|value| value.parse::<u32>().ok())
        && let Some(part) = physics.render_indices.iter().position(|index| *index == old_index)
    {
        to_port = format!("object_{}", new_object_index + part as u32);
    }
    EffectGraphWire { from_node: map_node(wire.from_node), from_port, to_node: map_node(wire.to_node), to_port }
}
