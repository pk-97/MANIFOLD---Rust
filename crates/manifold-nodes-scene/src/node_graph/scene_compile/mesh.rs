//! Shape Mesh: the geometry of a model file (SCENE_DOCUMENT_DESIGN.md section
//! 7.7 (Imports)). An Object draws one file node's own mesh in that node's
//! own space: its Transform starts at the node's transform, so the node's
//! origin is its pivot, as in Blender's glTF import. A Group with Combine
//! Meshes on is one object of every mesh below it, each part keeping its
//! material and animation, drawn where the file puts it.
//!
//! Every part records the file's radius for mesh modifiers to measure by. A
//! skinned part deforms by its skin and an animated one by its node chain
//! as a one-slot palette, the pose seen from the part's node at rest, so it
//! draws where the file puts it until its item moves; a morphed one blends
//! its targets first, every clock playing the clip its import Group picks.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, MeshPrimitives, SceneDoc, Shape};

use super::builder::{Builder, Drawn, Rest};
use super::group::Frame;
use super::material::surface_for;
use super::object::object_wires;
use crate::node_graph::gltf_import::model_file::{ClipDurations, Morph, Rig, model_file};
use crate::node_graph::gltf_load::{DEFAULT_MATERIAL_MESH_PARAM, FilePart};

/// `node.gltf_mesh_source`'s smallest buffer, in vertices.
const MIN_CAPACITY: u32 = 36;

/// The buffer size, in vertices, for a mesh of `vertex_count`: the mesh's own
/// size, never a cap (a photoscan of millions of triangles draws whole). The
/// param carries it as a float, so it rounds up to one a float holds exactly.
/// A buffer the GPU cannot give is refused by the source, with its reason.
fn capacity_for(vertex_count: u32) -> u32 {
    let exact = vertex_count.clamp(MIN_CAPACITY, i32::MAX as u32);
    let rounded = exact as f32;
    if rounded as u64 >= u64::from(exact) { rounded as u32 } else { rounded.next_up() as u32 }
}

/// The vertices of `parts` together.
fn vertices_of(parts: &[FilePart]) -> u32 {
    parts.iter().fold(0u32, |total, part| total.saturating_add(part.vertex_count))
}

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
        let capacity = capacity_for(vertex_count);
        b.set(source, "max_capacity", SerializedParamValue::Int { value: capacity.min(i32::MAX as u32) as i32 });
        b.set(source, "source_vertex_count", SerializedParamValue::Int { value: vertex_count.min(i32::MAX as u32) as i32 });
    }
    b.set(source, "vertex_colors", SerializedParamValue::Bool { value: true });
}

/// What an Object's part compiled to.
pub(crate) struct Part {
    /// The mesh source node.
    pub source: u32,
    /// The node and port the draw reads its vertices from.
    pub vertices: (u32, &'static str),
    /// Its rest pose: posed parts rest at their bind pose.
    pub rest: Option<Rest>,
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
    emit_part_in(b, item, file, node_path, vertex_count, primitives, true)
}

/// [`emit_part`], in the node's own space when `own_space`; otherwise
/// where the file puts it (a Combine Meshes part).
fn emit_part_in(
    b: &mut Builder<'_>,
    item: &Item,
    file: &str,
    node_path: &str,
    vertex_count: u32,
    primitives: MeshPrimitives,
    own_space: bool,
) -> Part {
    let owner = item.id.as_str();
    let model = model_file(b.doc, file);
    let facts = model.and_then(|model| model.nodes.get(node_path).map(|facts| (model, facts)));
    // The file's own count, once its facts arrive, over the one the
    // document recorded at import: a re-export to the same path may be
    // bigger, and its buffer must hold it.
    let vertex_count = facts.map_or(vertex_count, |(_, facts)| match primitives.material() {
        Some(material) => facts.part(Some(material)).map_or(vertex_count, |part| part.vertex_count),
        None => vertices_of(&facts.parts),
    });
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
        b.set(source, "own_space", SerializedParamValue::Bool { value: own_space });
        if let Some((model, _)) = facts {
            b.set(source, "source_bbox_radius", SerializedParamValue::Float { value: model.radius });
        }
        // A morphing part rests at its base shape.
        let rest = part.map(|part| rest_of(part, own_space, vertex_count));
        let vertices = match morph {
            Some(morph) => emit_morph(b, item, file, node_path, known, vertex_count, morph, (source, "vertices"), own_space),
            None => (source, "vertices"),
        };
        return Part { source, vertices, rest };
    };
    // A skinned or animated part: the pose places its vertices, seen from
    // its node at rest when it draws in the node's own space.
    let source = b.add(owner, "mesh", "node.gltf_skinned_mesh_source");
    source_params(b, source, file, node_path, vertex_count);
    b.set(source, "material_index", SerializedParamValue::Int { value: known });
    b.set(source, "source_bbox_radius", SerializedParamValue::Float { value: model.radius });
    let pose = b.add(owner, "pose", "node.gltf_skeleton_pose");
    b.set(pose, "path", SerializedParamValue::String { value: file.to_owned() });
    let (node, skin_index, joint_count, clips) = match rig {
        Rig::Skin { node_index, skin_index, joint_count, clips } => (*node_index, *skin_index as i32, *joint_count, clips),
        Rig::Chain { node_index, clips } => {
            // -2 selects the node-slot palette: slot 0 is this part's node.
            let slots = vec![vec![*node_index as f32]];
            b.set(pose, "node_slots", SerializedParamValue::Table { rows: slots });
            (*node_index, -2, 1, clips)
        }
    };
    if own_space {
        b.set(pose, "space_node", SerializedParamValue::Int { value: node.min(i32::MAX as u32) as i32 });
    }
    b.set(pose, "skin_index", SerializedParamValue::Int { value: skin_index });
    b.set(pose, "joint_count", SerializedParamValue::Int { value: joint_count as i32 });
    clock_params(b, pose, clips, import_clip(b.doc, &item.id));
    b.clocks.push((item.id.clone(), pose));
    let skin = b.add(owner, "skin", "node.skin_mesh");
    b.set(skin, "joint_count", SerializedParamValue::Int { value: joint_count as i32 });
    b.wire(source, "joints", skin, "joints");
    b.wire(source, "weights", skin, "weights");
    b.wire(pose, "joint_matrices", skin, "matrices");
    // Morph first, then skin (glTF 2.0 section 3.7.2): the deltas stay in
    // the bind pose's space.
    let deformed = match morph {
        Some(morph) => emit_morph(b, item, file, node_path, known, vertex_count, morph, (source, "vertices"), true),
        None => (source, "vertices"),
    };
    b.wire(deformed.0, deformed.1, skin, "in");
    // The part rests at its bind pose; a modifier bends the posed result.
    let rest = part.map(|part| rest_of(part, own_space, vertex_count));
    Part { source, vertices: (skin, "out"), rest }
}

/// A file part's rest pose: its box in the node's own space when it draws
/// there (`own_space`), else where the file puts it. Either way it records
/// where the file puts it, for the scene's box.
fn rest_of(part: &FilePart, own_space: bool, vertex_count: u32) -> Rest {
    let span = |center: [f32; 3], half: [f32; 3]| -> ([f32; 3], [f32; 3]) {
        (std::array::from_fn(|axis| center[axis] - half[axis]), std::array::from_fn(|axis| center[axis] + half[axis]))
    };
    let placed = span(part.center, part.half_extent);
    let (lo, hi) = if own_space { span(part.own_center, part.own_half_extent) } else { placed };
    Rest { lo, hi, placed: Some(placed), own_space, vertices: part.vertex_count.max(vertex_count) }
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
    own_space: bool,
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
    b.set(deltas, "own_space", SerializedParamValue::Bool { value: own_space });
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

/// A Group whose Source is a file and whose Combine Meshes is on: one object
/// of the whole file, in the group's frame, in place of its children. Its
/// shape (the body's and the copies' collision shape) is the whole file as
/// one mesh. Each part still draws with its own material, skin, morphs and
/// clip, where the file puts it, since a draw takes one material. Returns
/// whether it drew, so the walk skips the children.
pub(crate) fn emit_combined(b: &mut Builder<'_>, group: &Item, frame: &Frame) -> bool {
    let ItemFields::Group(fields) = &group.fields else {
        return false;
    };
    let Some(file) = fields.source.as_deref().filter(|_| fields.combine_meshes) else {
        return false;
    };
    let mut parts = Vec::new();
    for child in &group.children {
        parts_below(child, &mut parts);
    }
    // The whole file's count from its facts, else what the parts recorded.
    let vertices = match model_file(b.doc, file) {
        Some(model) => model.nodes.values().fold(0u32, |total, node| total.saturating_add(vertices_of(&node.parts))),
        None => parts.iter().fold(0u32, |total, part| match &part.fields {
            ItemFields::Object(object) => match object.shape {
                Shape::Mesh { vertex_count, .. } => total.saturating_add(vertex_count),
                _ => total,
            },
            _ => total,
        }),
    };
    let owner = group.id.as_str();
    // Nothing draws the shape, so it loads only when a body or copies read it.
    let shape = emit_source(b, owner, file, "", vertices);
    if let Some(model) = model_file(b.doc, file) {
        b.set(shape, "source_bbox_radius", SerializedParamValue::Float { value: model.radius });
    }
    let transform = b.add(owner, "combined_transform", "node.transform_3d");
    if let Some(body) = frame.copies {
        // Inside one copy, as an Object under copies is.
        if let Some(parent) = frame.transform {
            b.wire(parent, "transform", transform, "parent");
        }
        super::copies::add_part(b, body, shape, transform);
    } else {
        super::physics::combined(b, group, shape, transform);
    }
    for part in parts {
        let ItemFields::Object(object_fields) = &part.fields else {
            continue;
        };
        let Shape::Mesh { file, node_path, vertex_count, primitives } = &object_fields.shape else {
            continue;
        };
        let drawn = emit_part_in(b, part, file, node_path, *vertex_count, *primitives, false);
        let object = b.add(part.id.as_str(), "object", "node.scene_object");
        if let Some(rest) = drawn.rest {
            b.drawn.push(Drawn { object, item: part.id.clone(), rest });
        }
        let (surface, maps) = surface_for(b, &part.id);
        for w in object_wires(drawn.source, surface, transform, object) {
            if w.to_port == "vertices" {
                b.wire(drawn.vertices.0, drawn.vertices.1, object, "vertices");
            } else {
                b.wire(w.from_node, &w.from_port, w.to_node, &w.to_port);
            }
        }
        for (source, port) in maps {
            b.wire(source, "out", object, port);
        }
        b.wire(frame.visible, "out", object, "parent_visible");
        if let Some(body) = frame.copies {
            super::copies::draw_instances(b, body, object, true);
        } else if let Some(parent) = frame.transform {
            b.wire(parent, "transform", object, "parent_transform");
        }
        b.objects.push(object);
        // A click on any part picks the combined group.
        b.object_items.push(group.id.clone());
    }
    true
}

/// Every Object below `item` (itself included) whose Shape is a file mesh.
fn parts_below<'a>(item: &'a Item, out: &mut Vec<&'a Item>) {
    if let ItemFields::Object(object) = &item.fields
        && matches!(object.shape, Shape::Mesh { .. })
    {
        out.push(item);
    }
    for child in &item.children {
        parts_below(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::capacity_for;

    #[test]
    fn a_photoscan_gets_a_buffer_as_big_as_its_mesh() {
        // 3 million triangles, past the old 8 million vertex cap.
        for vertices in [9_000_000u32, 16_777_217, 100_000_001] {
            let capacity = capacity_for(vertices);
            assert!(capacity >= vertices, "{vertices} vertices got a buffer of {capacity}");
            assert_eq!(capacity as f32 as u32, capacity, "the float param holds it exactly");
        }
        assert_eq!(capacity_for(3), 36);
    }
}
