//! Object: a mesh, a transform and a material bound by one scene object.
//! Inside a group, the object sits in the group's frame and follows its
//! visibility.

use manifold_core::effect_graph_def::{EffectGraphWire, SerializedParamValue};
use manifold_core::scene_doc::{Item, ItemFields, Shape};
use manifold_core::scene_schema::can_copy;

use super::builder::Builder;
use super::copies;
use super::group::Frame;
use super::material::surface_for;
use crate::node_graph::scene_graph::wire;

/// The wires that bind one object's mesh, material and transform to its
/// draw.
pub(crate) fn object_wires(mesh: u32, material: u32, transform: u32, object: u32) -> Vec<EffectGraphWire> {
    vec![
        wire(mesh, "vertices", object, "vertices"),
        wire(material, "out", object, "material"),
        wire(transform, "transform", object, "transform"),
    ]
}

/// The mesh node a built-in shape is, and the platonic solid index where it
/// is one. `None` for the shapes with their own emitter (Ocean, Mesh from file).
fn mesh_type(shape: &Shape) -> Option<(&'static str, Option<u32>)> {
    Some(match shape {
        Shape::Cube => ("node.cube_mesh", None),
        Shape::Sphere => ("node.sphere_mesh", None),
        Shape::Plane => ("node.plane_mesh", None),
        // `node.platonic_solid_mesh`'s shapes: Tetrahedron 0, Cube 1,
        // Octahedron 2, Icosahedron 3, Dodecahedron 4.
        Shape::Tetrahedron => ("node.platonic_solid_mesh", Some(0)),
        Shape::Octahedron => ("node.platonic_solid_mesh", Some(2)),
        Shape::Icosahedron => ("node.platonic_solid_mesh", Some(3)),
        Shape::Dodecahedron => ("node.platonic_solid_mesh", Some(4)),
        Shape::Ocean | Shape::Mesh { .. } => return None,
    })
}

pub(crate) fn emit(b: &mut Builder<'_>, item: &Item, frame: Option<&Frame>) {
    let ItemFields::Object(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let group_copies = frame.and_then(|frame| frame.copies);
    let own_copies = fields.copies && can_copy(&fields.shape) && group_copies.is_none();
    let mut center = None;
    let (mesh, vertices) = if let Shape::Mesh { file, node_path, vertex_count, primitives } = &fields.shape {
        let part = super::mesh::emit_part(b, item, file, node_path, *vertex_count, *primitives);
        center = part.center;
        (part.source, part.vertices)
    } else {
        let Some((mesh_type, solid)) = mesh_type(&fields.shape) else {
            return;
        };
        let mesh = b.add(owner, "mesh", mesh_type);
        if let Some(solid) = solid {
            b.set(mesh, "shape", SerializedParamValue::Enum { value: solid });
        }
        (mesh, (mesh, "vertices"))
    };
    let transform = b.add(owner, "transform", "node.transform_3d");
    if let Some(center) = center {
        for (axis, param) in ["pos_x", "pos_y", "pos_z"].into_iter().enumerate() {
            b.set(transform, param, SerializedParamValue::Float { value: center[axis] });
        }
    }
    let object = b.add(owner, "object", "node.scene_object");
    let (surface, maps) = surface_for(b, &item.id);
    // An object that is its own body draws at the body's solved pose.
    let pose = super::physics::object(b, item, frame, mesh, transform);
    for w in object_wires(mesh, surface, transform, object) {
        if w.to_port == "vertices" {
            b.wire(vertices.0, vertices.1, object, "vertices");
            continue;
        }
        // A body's transform is its start, and its own copies' layout
        // centre; the draw reads the solved poses instead.
        if (pose.is_some() || own_copies) && w.to_port == "transform" {
            continue;
        }
        b.wire(w.from_node, &w.from_port, w.to_node, &w.to_port);
    }
    for (source, port) in maps {
        b.wire(source, "out", object, port);
    }
    if let Some(frame) = frame {
        b.wire(frame.visible, "out", object, "parent_visible");
    }
    let parent = frame.and_then(|frame| frame.transform);
    if let Some(pose) = pose {
        b.wire(pose, "transform", object, "parent_transform");
    } else if own_copies || group_copies.is_some() {
        // Copies are laid out and drawn in the world: the transform carries
        // its enclosing frame (inside one copy, under a group's copies).
        if let Some(parent) = parent {
            b.wire(parent, "transform", transform, "parent");
        }
        if own_copies {
            copies::emit_object(b, item, mesh, transform, object, fields.physics);
        } else if let Some(body) = group_copies {
            copies::add_part(b, body, mesh, transform);
            copies::draw_instances(b, body, object, true);
        }
    } else if let Some(parent) = parent {
        b.wire(parent, "transform", object, "parent_transform");
    }
    b.objects.push(object);
    b.object_items.push(item.id.clone());
    b.rows(item);
}
