//! Shape Mesh: the geometry of a model file (SCENE_DOCUMENT_DESIGN.md section
//! 7.7 (Imports)). An Object draws one file node's own mesh, already placed
//! in the file's world; a Group with Combine Meshes on draws every mesh
//! below it as one.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Item, ItemFields, Shape};

use super::builder::Builder;
use super::group::Frame;
use super::material::surface_for;
use super::object::object_wires;

/// `node.gltf_mesh_source`'s smallest buffer, in vertices.
const MIN_CAPACITY: u32 = 36;
/// Its largest.
const MAX_CAPACITY: u32 = 8_000_000;

/// The mesh node for `file`'s node `node_path` (the whole file when empty),
/// sized for `vertex_count` vertices.
pub(crate) fn emit_source(b: &mut Builder<'_>, owner: &str, file: &str, node_path: &str, vertex_count: u32) -> u32 {
    let mesh = b.add(owner, "mesh", "node.gltf_mesh_source");
    b.set(mesh, "path", SerializedParamValue::String { value: file.to_owned() });
    if !node_path.is_empty() {
        b.set(mesh, "node_path", SerializedParamValue::String { value: node_path.to_owned() });
    }
    if vertex_count > 0 {
        let capacity = vertex_count.clamp(MIN_CAPACITY, MAX_CAPACITY);
        b.set(mesh, "max_capacity", SerializedParamValue::Int { value: capacity as i32 });
    }
    b.set(mesh, "vertex_colors", SerializedParamValue::Bool { value: true });
    mesh
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
    let transform = b.add(owner, "combined_transform", "node.transform_3d");
    let object = b.add(owner, "combined_object", "node.scene_object");
    let (surface, texture) = surface_for(b, &group.id);
    for w in object_wires(mesh, surface, transform, object) {
        b.wire(w.from_node, &w.from_port, w.to_node, &w.to_port);
    }
    if let Some(texture) = texture {
        b.wire(texture, "out", object, "base_color_map");
    }
    b.wire(frame.transform, "transform", object, "parent_transform");
    b.wire(frame.visible, "out", object, "parent_visible");
    b.objects.push(object);
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
