//! Object: a mesh, a transform and a material bound by one scene object.
//! Inside a group, the object sits in the group's frame and follows its
//! visibility.

use manifold_core::effect_graph_def::{EffectGraphWire, SerializedParamValue};
use manifold_core::scene_doc::{Item, ItemFields, Shape};

use super::builder::Builder;
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
/// is one. `None` for the shapes later lanes emit (Ocean, Mesh from file).
fn mesh_type(shape: &Shape) -> Option<(&'static str, Option<u32>)> {
    Some(match shape {
        Shape::Cube => ("node.cube_mesh", None),
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
    let Some((mesh_type, solid)) = mesh_type(&fields.shape) else {
        return;
    };
    let owner = item.id.as_str();
    let mesh = b.add(owner, "mesh", mesh_type);
    if let Some(solid) = solid {
        b.set(mesh, "shape", SerializedParamValue::Enum { value: solid });
    }
    let transform = b.add(owner, "transform", "node.transform_3d");
    let object = b.add(owner, "object", "node.scene_object");
    let (surface, texture) = surface_for(b, &item.id);
    for w in object_wires(mesh, surface, transform, object) {
        b.wire(w.from_node, &w.from_port, w.to_node, &w.to_port);
    }
    if let Some(texture) = texture {
        b.wire(texture, "out", object, "base_color_map");
    }
    if let Some(frame) = frame {
        b.wire(frame.transform, "transform", object, "parent_transform");
        b.wire(frame.visible, "out", object, "parent_visible");
    }
    b.objects.push(object);
    b.rows(item);
}
