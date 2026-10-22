//! Shape Mesh: the geometry of a model file (SCENE_DOCUMENT_DESIGN.md section
//! 7.7 (Imports)). An Object draws one file node's own mesh, already placed
//! in the file's world; a Group with Combine Meshes on draws every mesh
//! below it as one.
//!
//! A part keeps what the old importer gave it: a static part is recentred on
//! its own box so its Transform turns it about itself, and records the
//! file's radius for mesh modifiers to measure by; a skinned part deforms by
//! its skin, an animated one by its node chain as a one-slot palette, and a
//! morphed one blends its targets first, every clock playing the clip its
//! import Group picks.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, MeshPrimitives, SceneDoc, Shape};

use super::builder::Builder;
use super::group::Frame;
use super::material::surface_for;
use super::object::object_wires;
use crate::node_graph::gltf_import::model_file::{ClipDurations, Morph, Rig, model_file};
use crate::node_graph::gltf_load::DEFAULT_MATERIAL_MESH_PARAM;

/// `node.gltf_mesh_source`'s smallest buffer, in vertices.
const MIN_CAPACITY: u32 = 36;
/// Its largest.
const MAX_CAPACITY: u32 = 8_000_000;

/// The mesh node for `file`'s node `node_path` (the whole file when empty),
/// sized for `vertex_count` vertices.
pub(crate) fn emit_source(b: &mut Builder<'_>, owner: &str, file: &str, node_path: &str, vertex_count: u32) -> u32 {
    let mesh = b.add(owner, "mesh", "node.gltf_mesh_source");
    source_params(b, mesh, file, node_path, vertex_count);
    mesh
}

fn source_params(b: &mut Builder<'_>, source: u32, file: &str, node_path: &str, vertex_count: u32) {
    b.set(source, "path", SerializedParamValue::String { value: file.to_owned() });
    if !node_path.is_empty() {
        b.set(source, "node_path", SerializedParamValue::String { value: node_path.to_owned() });
    }
    if vertex_count > 0 {
        let capacity = vertex_count.clamp(MIN_CAPACITY, MAX_CAPACITY);
        b.set(source, "max_capacity", SerializedParamValue::Int { value: capacity as i32 });
        b.set(source, "source_vertex_count", SerializedParamValue::Int { value: vertex_count.min(MAX_CAPACITY) as i32 });
    }
    b.set(source, "vertex_colors", SerializedParamValue::Bool { value: true });
}

/// What an Object's part compiled to.
pub(crate) struct Part {
    /// The mesh source node.
    pub source: u32,
    /// The node and port the draw reads its vertices from.
    pub vertices: (u32, &'static str),
    /// Where the Transform starts: the part's own centre for a static part,
    /// so it turns about itself.
    pub center: Option<[f32; 3]>,
}

/// Emit the part `item` draws: `file`'s node `node_path`, its `primitives`.
pub(crate) fn emit_part(
    b: &mut Builder<'_>,
    item: &Item,
    file: &str,
    node_path: &str,
    vertex_count: u32,
    primitives: MeshPrimitives,
) -> Part {
    let owner = item.id.as_str();
    let model = model_file(file);
    let facts = model.as_deref().and_then(|model| model.nodes.get(node_path).map(|facts| (model, facts)));
    // The glTF material the part's primitives use (`None` the default), when
    // one is known: the selection's, else the node's only material.
    let part = facts.and_then(|(_, facts)| facts.part(primitives.material()));
    let material = primitives.material().or_else(|| part.map(|part| part.material));
    let material_param = match primitives.material() {
        None => -1,
        Some(None) => DEFAULT_MATERIAL_MESH_PARAM,
        Some(Some(index)) => index as i32,
    };
    let known = material.map_or(-1, |material| material.map_or(DEFAULT_MATERIAL_MESH_PARAM, |index| index as i32));
    let morph = facts.zip(material).and_then(|((_, facts), material)| facts.morph(material));
    let Some((model, rig)) = facts.and_then(|(model, facts)| facts.rig.as_ref().map(|rig| (model, rig))) else {
        let source = emit_source(b, owner, file, node_path, vertex_count);
        if material_param != -1 {
            b.set(source, "material_index", SerializedParamValue::Int { value: material_param });
        }
        let mut center = None;
        if let Some((model, _)) = facts {
            b.set(source, "source_bbox_radius", SerializedParamValue::Float { value: model.radius });
            if let Some(part) = part {
                for (axis, param) in ["translate_x", "translate_y", "translate_z"].into_iter().enumerate() {
                    b.set(source, param, SerializedParamValue::Float { value: -part.center[axis] });
                }
                center = Some(part.center);
            }
        }
        let vertices = match morph {
            Some(morph) => emit_morph(b, item, file, node_path, known, vertex_count, morph, (source, "vertices"), false),
            None => (source, "vertices"),
        };
        return Part { source, vertices, center };
    };
    // A skinned or animated part: its vertices stay in the node's own space
    // and the pose places them.
    let source = b.add(owner, "mesh", "node.gltf_skinned_mesh_source");
    source_params(b, source, file, node_path, vertex_count);
    b.set(source, "material_index", SerializedParamValue::Int { value: known });
    b.set(source, "source_bbox_radius", SerializedParamValue::Float { value: model.radius });
    let pose = b.add(owner, "pose", "node.gltf_skeleton_pose");
    b.set(pose, "path", SerializedParamValue::String { value: file.to_owned() });
    let (skin_index, joint_count, clips) = match rig {
        Rig::Skin { skin_index, joint_count, clips } => (*skin_index as i32, *joint_count, clips),
        Rig::Chain { node_index, clips } => {
            // -2 selects the node-slot palette: slot 0 is this part's node.
            let slots = vec![vec![*node_index as f32]];
            b.set(pose, "node_slots", SerializedParamValue::Table { rows: slots });
            (-2, 1, clips)
        }
    };
    b.set(pose, "skin_index", SerializedParamValue::Int { value: skin_index });
    b.set(pose, "joint_count", SerializedParamValue::Int { value: joint_count as i32 });
    clock_params(b, pose, clips, import_clip(b.doc, &item.id));
    b.clocks.push((item.id.clone(), pose));
    let skin = b.add(owner, "skin", "node.skin_mesh");
    b.set(skin, "joint_count", SerializedParamValue::Int { value: joint_count as i32 });
    b.wire(source, "joints", skin, "joints");
    b.wire(source, "weights", skin, "weights");
    b.wire(pose, "joint_matrices", skin, "matrices");
    // Morph first, then skin (glTF 2.0 section 3.7.2).
    let deformed = match morph {
        Some(morph) => emit_morph(b, item, file, node_path, known, vertex_count, morph, (source, "vertices"), true),
        None => (source, "vertices"),
    };
    b.wire(deformed.0, deformed.1, skin, "in");
    Part { source, vertices: (skin, "out"), center: None }
}

/// Blend `base` with the part's morph targets; returns the blend's output.
#[allow(clippy::too_many_arguments)]
fn emit_morph(
    b: &mut Builder<'_>,
    item: &Item,
    file: &str,
    node_path: &str,
    material_param: i32,
    vertex_count: u32,
    morph: &Morph,
    base: (u32, &'static str),
    skinned: bool,
) -> (u32, &'static str) {
    let owner = item.id.as_str();
    let weights = b.add(owner, "morph_weights", "node.gltf_morph_weights");
    b.set(weights, "path", SerializedParamValue::String { value: file.to_owned() });
    b.set(weights, "target_node", SerializedParamValue::Int { value: morph.node_index as i32 });
    b.set(weights, "target_count", SerializedParamValue::Int { value: morph.target_count as i32 });
    b.set(weights, "static_weights", SerializedParamValue::Table { rows: morph.static_weights.clone() });
    clock_params(b, weights, &morph.clips, import_clip(b.doc, &item.id));
    b.clocks.push((item.id.clone(), weights));
    let deltas = b.add(owner, "morph_deltas", "node.gltf_morph_deltas_source");
    b.set(deltas, "path", SerializedParamValue::String { value: file.to_owned() });
    b.set(deltas, "node_path", SerializedParamValue::String { value: node_path.to_owned() });
    // The deltas' selection matches the base's, or the node's only material.
    let deltas_material = if material_param == -1 { 0 } else { material_param };
    b.set(deltas, "material_index", SerializedParamValue::Int { value: deltas_material });
    let capacity = (morph.target_count.max(1) as u64 * u64::from(vertex_count.max(1))).min(i32::MAX as u64);
    b.set(deltas, "max_capacity", SerializedParamValue::Int { value: capacity as i32 });
    b.set(deltas, "skinned", SerializedParamValue::Bool { value: skinned });
    let blend = b.add(owner, "morph_blend", "node.morph_targets_blend");
    b.set(blend, "target_count", SerializedParamValue::Int { value: morph.target_count as i32 });
    b.wire(base.0, base.1, blend, "in");
    b.wire(deltas, "deltas", blend, "deltas");
    b.wire(weights, "weights", blend, "weights");
    (blend, "out")
}

/// A clock's clip table, its pre-load duration and the clip it plays.
fn clock_params(b: &mut Builder<'_>, clock: u32, clips: &ClipDurations, clip: u32) {
    b.set(clock, "duration_s", SerializedParamValue::Float { value: clips.first });
    b.set(clock, "clip_durations", SerializedParamValue::Table { rows: clips.rows.clone() });
    b.set(clock, "clip_index", SerializedParamValue::Int { value: clip as i32 });
}

/// The clip of the import Group nearest above `item`; 0 outside any.
fn import_clip(doc: &SceneDoc, item: &ItemId) -> u32 {
    let mut clip = 0;
    doc.walk(|candidate, ancestors| {
        if &candidate.id == item {
            clip = ancestors
                .iter()
                .rev()
                .find_map(|holder| match &holder.fields {
                    ItemFields::Group(group) if group.source.is_some() => Some(group.clip),
                    _ => None,
                })
                .unwrap_or(0);
        }
    });
    clip
}

/// A Group whose Source is a file and whose Combine Meshes is on: one draw
/// of the whole file's meshes, in the group's frame, in place of its
/// children. Returns whether it drew, so the walk skips the children.
pub(crate) fn emit_combined(b: &mut Builder<'_>, group: &Item, frame: &Frame) -> bool {
    let ItemFields::Group(fields) = &group.fields else {
        return false;
    };
    let Some(file) = fields.source.as_deref().filter(|_| fields.combine_meshes) else {
        return false;
    };
    let mut vertices = 0u32;
    for child in &group.children {
        meshes_below(child, &mut |shape| {
            if let Shape::Mesh { vertex_count, .. } = shape {
                vertices = vertices.saturating_add(*vertex_count);
            }
        });
    }
    let owner = group.id.as_str();
    let mesh = emit_source(b, owner, file, "", vertices);
    if let Some(model) = model_file(file) {
        b.set(mesh, "source_bbox_radius", SerializedParamValue::Float { value: model.radius });
    }
    let transform = b.add(owner, "combined_transform", "node.transform_3d");
    let object = b.add(owner, "combined_object", "node.scene_object");
    let (surface, maps) = surface_for(b, &group.id);
    for w in object_wires(mesh, surface, transform, object) {
        b.wire(w.from_node, &w.from_port, w.to_node, &w.to_port);
    }
    for (source, port) in maps {
        b.wire(source, "out", object, port);
    }
    if let Some(parent) = frame.transform {
        b.wire(parent, "transform", object, "parent_transform");
    }
    b.wire(frame.visible, "out", object, "parent_visible");
    if let Some(body) = frame.copies {
        super::copies::draw_instances(b, body, object, true);
    }
    b.objects.push(object);
    b.object_items.push(group.id.clone());
    true
}

fn meshes_below(item: &Item, f: &mut impl FnMut(&Shape)) {
    if let ItemFields::Object(object) = &item.fields {
        f(&object.shape);
    }
    for child in &item.children {
        meshes_below(child, f);
    }
}
