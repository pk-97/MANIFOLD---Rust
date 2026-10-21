//! Make Copies: one scene object becomes a copy group (copy_group_design.md
//! section 5). One pure graph edit, committed as one undo step through the
//! shared scene transaction.

use super::*;
use manifold_core::change::ChangeSet;
use manifold_core::scene_graph_edit::{SceneGraphEdit, SceneGraphEditError};
use manifold_core::scene_surface::SceneObjectRowKind;

use crate::scene_transaction::SceneGraphTransaction;

use manifold_core::copies_body::SIZE_SPACING_DEFAULT;
use manifold_core::physics_body_slots::fit_world_bodies;
use manifold_core::physics_members::{Family, MEMBER_OUTPUT, world_member};
use manifold_core::scene_slots::{SceneSlot, object_slot};
use manifold_core::scene_variadic::WORLD_COPY_GROUPS;

/// A new group starts as a Line of this many copies.
const START_COUNT: f32 = 4.0;
/// `copy_layout` value of Line.
const LINE: u32 = 2;
const WATER: &str = "Water cannot make copies: its liquid simulation already draws it";
const NO_FREE_GROUP: &str = "The Physics World has no free copy-group input";

/// What Make Copies works from: the object's parts, and whether physics is
/// already on it.
struct Plan {
    physics_on: bool,
}

fn plan(def: &EffectGraphDef, slot: SceneSlot) -> Result<Plan, String> {
    let scene = def
        .nodes
        .iter()
        .find(|node| node.id == slot.render_id)
        .and_then(|node| SceneNodeRef::locate(def, &node.node_id))
        .ok_or("Make Copies scene is unavailable")?;
    let models = manifold_core::scene_object_model::scene_object_models(def, &scene).map_err(|error| error.message)?;
    let model = models.iter().find(|model| model.slot == Some(slot.index)).ok_or("Selected scene object is unavailable")?;
    if model.liquid_domain.is_some() || model.kind == SceneObjectRowKind::LookChild {
        return Err(WATER.into());
    }
    if model.copy_group().is_some() {
        return Err("This object is already a copy group".into());
    }
    if model.swarm().is_some() {
        return Err("A swarm already draws many copies".into());
    }
    if let Some(parent) = &model.parent {
        let parts = models.iter().filter(|other| other.parent.as_ref() == Some(parent)).count();
        return Err(format!("Copies take single-mesh objects; this import has {parts} parts"));
    }
    let parts = scene_object_parts(def, slot.render_id, slot.index)?;
    if parts.compound_sources.len() > 1 {
        return Err(format!("Copies take single-mesh objects; this import has {} parts", parts.compound_sources.len()));
    }
    let transform_nodes = match parts.group_id {
        Some(group) => def.nodes.iter().find(|node| node.id == group).and_then(|node| node.group.as_deref()).map(|group| group.nodes.as_slice()),
        None => Some(def.nodes.as_slice()),
    };
    if let Some(transform) = transform_nodes.and_then(|nodes| nodes.iter().find(|node| node.id == parts.authored_transform_id)) {
        let scale = |name: &str| match transform.params.get(name) {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 1.0,
        };
        let (x, y, z) = (scale("scale_x"), scale("scale_y"), scale("scale_z"));
        if (x - y).abs() > 1e-6 * x.abs().max(1.0) || (x - z).abs() > 1e-6 * x.abs().max(1.0) {
            return Err("Make Copies needs one scale on all three axes".into());
        }
    }
    let physics_on = scene_physics_body_wired(def, &parts).is_ok();
    if physics_on {
        // A loose body is a copy group only in the shape the standard match reads.
        if parts.group_id.is_none() {
            let valid = matches!(
                physics_scene_object_match(&def.nodes, &def.wires, slot.render_id, slot.index, parts.object_id),
                PhysicsSceneObjectMatch::Valid(_)
            );
            if !valid {
                return Err("This object's physics is not a plain body Make Copies can take".into());
            }
        }
    } else {
        if parts.group_id.is_none() {
            return Err("Make Copies works on objects added from the Add menu or imported".into());
        }
        scene_object_physics_plan(def, slot.render_id, slot.index)?;
    }
    let worlds: Vec<u32> = def.nodes.iter().filter(|node| node.type_id == "node.physics_world").map(|node| node.id).collect();
    if let [world] = worlds.as_slice()
        && WORLD_COPY_GROUPS.next_free(&def.wires, *world).is_none()
    {
        return Err(NO_FREE_GROUP.into());
    }
    Ok(Plan { physics_on })
}

/// Why the object at `slot` cannot become a copy group; `Ok` when it can.
pub fn make_copies_eligibility(def: &EffectGraphDef, slot: SceneSlot) -> Result<(), String> {
    plan(def, slot).map(|_| ())
}

fn set(node: &mut EffectGraphNode, name: &str, value: SerializedParamValue) {
    node.params.insert(name.to_string(), value);
}

/// The graph with `object` made a copy group: its body joins the Physics
/// World as the prototype of copy group `n`, and the object draws the body's
/// instances. Physics is switched on first when the object has none.
pub fn make_copies_edit(
    graph: &EffectGraphDef,
    object: &SceneNodeRef,
    body_metadata: &[NodeParamMetadata],
    world_metadata: &[NodeParamMetadata],
    copies_metadata: &[NodeParamMetadata],
) -> Result<SceneGraphEdit, SceneGraphEditError> {
    let refuse = |message: &str| SceneGraphEditError { at: None, message: message.into() };
    let slot = object_slot(graph, object).map_err(|message| refuse(&message))?;
    let Plan { physics_on } = plan(graph, slot).map_err(|message| refuse(&message))?;
    let mut def = if physics_on {
        graph.clone()
    } else {
        enable_physics_edit(graph, object, body_metadata, world_metadata)?.graph
    };
    let parts = scene_object_parts(&def, slot.render_id, slot.index).map_err(refuse)?;

    // The node whose `body` output is the membership wire, and the body.
    let (member_source, body_id) = match parts.group_id {
        Some(group_id) => {
            let group = def.nodes.iter().find(|node| node.id == group_id).and_then(|node| node.group.as_deref());
            let output = group.and_then(|group| group.nodes.iter().find(|node| node.type_id == GROUP_OUTPUT_TYPE_ID));
            let body = output.zip(group).and_then(|(output, group)| {
                group.wires.iter().find(|wire| wire.to_node == output.id && wire.to_port == MEMBER_OUTPUT).map(|wire| wire.from_node)
            });
            (group_id, body.ok_or_else(|| refuse("Make Copies body is unavailable"))?)
        }
        None => {
            let pose = def
                .wires
                .iter()
                .find(|wire| wire.to_node == parts.object_id && wire.to_port == "transform" && wire.from_port == "pose")
                .ok_or_else(|| refuse("Make Copies body is unavailable"))?;
            (pose.from_node, pose.from_node)
        }
    };
    let member = world_member(&def.nodes, &def.wires, member_source).ok_or_else(|| refuse("Make Copies body has no Physics World"))?;
    if member.family != Family::Body {
        return Err(refuse("This object is already a copy group"));
    }
    let membership = def
        .wires
        .iter()
        .position(|wire| {
            wire.from_node == member_source && wire.from_port == MEMBER_OUTPUT && wire.to_node == member.world && wire.to_port == Family::Body.member_port(member.k)
        })
        .ok_or_else(|| refuse("Make Copies body has no Physics World"))?;
    def.wires.remove(membership);
    let number = WORLD_COPY_GROUPS
        .append_member(&mut def.nodes, &def.wires, member.world)
        .ok_or_else(|| refuse(NO_FREE_GROUP))?;
    def.wires.push(scene_build_wire(member_source, MEMBER_OUTPUT, member.world, &Family::Copies.member_port(number)));
    fit_world_bodies(&mut def.nodes, &def.wires, member.world);

    // The object no longer takes one pose: it draws the group's instances.
    let object_id = parts.object_id;
    let rewire = |wires: &mut Vec<EffectGraphWire>| {
        wires.retain(|wire| !(wire.from_node == body_id && wire.from_port == "pose" && wire.to_node == object_id));
        wires.push(scene_build_wire(body_id, "instances", object_id, "instances"));
        wires.push(scene_build_wire(body_id, "active_count", object_id, "instance_count"));
    };
    let body_params = match parts.group_id {
        Some(group_id) => {
            let group = def
                .nodes
                .iter_mut()
                .find(|node| node.id == group_id)
                .and_then(|node| node.group.as_deref_mut())
                .ok_or_else(|| refuse("Make Copies object group is unavailable"))?;
            rewire(&mut group.wires);
            let body = group.nodes.iter_mut().find(|node| node.id == body_id).ok_or_else(|| refuse("Make Copies body is unavailable"))?;
            start_group(body);
            (body.node_id.clone(), body.params.clone())
        }
        None => {
            rewire(&mut def.wires);
            let body = def.nodes.iter_mut().find(|node| node.id == body_id).ok_or_else(|| refuse("Make Copies body is unavailable"))?;
            start_group(body);
            (body.node_id.clone(), body.params.clone())
        }
    };
    let (body_node_id, params) = body_params;
    let meta = def.preset_metadata.get_or_insert_with(unnamed_scene_metadata);
    stamp_scene_node_exposures_into(
        &mut meta.params,
        &mut meta.bindings,
        body_id,
        &body_node_id,
        &format!("{} — Copies", parts.object_handle),
        copies_metadata,
        &params,
    );
    Ok(SceneGraphEdit { graph: def, removed_param_ids: Vec::new(), parameter_id_remaps: Vec::new() })
}

/// A new group: a Line of [`START_COUNT`] copies at the default spacing.
fn start_group(body: &mut EffectGraphNode) {
    set(body, "copy_count", SerializedParamValue::Float { value: START_COUNT });
    set(body, "copy_layout", SerializedParamValue::Enum { value: LINE });
    set(body, "copy_size_spacing", SerializedParamValue::Float { value: SIZE_SPACING_DEFAULT });
}

/// Make Copies, prepared when made so a refusal surfaces before submission
/// and redo reuses the same ids.
#[derive(Debug)]
pub struct MakeCopyGroupCommand {
    transaction: SceneGraphTransaction,
}

impl MakeCopyGroupCommand {
    pub fn new(
        project: &Project,
        target: GraphTarget,
        object: SceneNodeRef,
        body_metadata: &[NodeParamMetadata],
        world_metadata: &[NodeParamMetadata],
        copies_metadata: &[NodeParamMetadata],
        catalog_default: &EffectGraphDef,
    ) -> Result<Self, SceneGraphEditError> {
        Ok(Self {
            transaction: SceneGraphTransaction::prepare(project, target, catalog_default, "Make Copies", |graph| {
                make_copies_edit(graph, &object, body_metadata, world_metadata, copies_metadata)
            })?,
        })
    }
}

impl Command for MakeCopyGroupCommand {
    fn touches(&self) -> ChangeSet {
        self.transaction.touches()
    }
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.transaction.owner.clone());
    }
    fn do_execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
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
