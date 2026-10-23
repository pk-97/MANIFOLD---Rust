//! Object: a mesh, a transform and a material bound by one scene object.
//! Inside a group, the object sits in the group's frame and follows its
//! visibility.

use manifold_core::effect_graph_def::{EffectGraphWire, SerializedParamValue};
use manifold_core::scene_doc::{Item, ItemFields, Shape};
use manifold_core::scene_schema::can_copy;

use manifold_node_engine::scene::mesh_source::MeshSource;

use super::builder::{Builder, Drawn, Rest, number};
use super::copies;
use super::group::Frame;
use super::material::surface_for;
use super::ocean;
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
/// is one. `None` for the shapes with their own emitter (Ocean, Mesh from
/// file, the curve shapes).
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
        Shape::Ocean | Shape::Mesh { .. } | Shape::Extrude { .. } | Shape::Revolve { .. } | Shape::Tube { .. } => return None,
    })
}

/// A built-in shape's rest pose: the bounds and count of the vertices its
/// mesh node makes at its own size, read from the same mesh source the node
/// describes, so every shape is measured the same way.
fn builtin_rest(b: &mut Builder<'_>, mesh_type: &str, solid: Option<u32>) -> Option<Rest> {
    let mut param = |name: &str| b.param_meta(mesh_type, name).map(|meta| number(&meta.default_value));
    let source = match mesh_type {
        "node.cube_mesh" => MeshSource::Cube { size: param("size")? },
        "node.sphere_mesh" => MeshSource::Sphere {
            radius: param("radius")?,
            segments: param("segments")? as u32,
            rings: param("rings")? as u32,
        },
        "node.plane_mesh" => MeshSource::Plane { width: param("width")?, height: param("height")? },
        "node.platonic_solid_mesh" => MeshSource::Platonic { shape: solid?, radius: param("radius")? },
        _ => return None,
    };
    let vertices = source.load_vertices().ok()?;
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for vertex in &vertices {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(vertex.position[axis]);
            hi[axis] = hi[axis].max(vertex.position[axis]);
        }
    }
    lo.iter().chain(hi.iter()).all(|value| value.is_finite()).then_some(Rest {
        lo,
        hi,
        offset: [0.0; 3],
        vertices: u32::try_from(vertices.len()).unwrap_or(u32::MAX),
    })
}

/// The vertex count a deformed built-in shape is split up to: enough for a
/// cube face to curve smoothly under a bend or a wave. A quality target, not
/// a cap: the split stops at the first level past it.
const DETAIL_VERTICES: u32 = 6000;

/// The deepest split the target needs: a Plane, the sparsest built-in at 6
/// vertices, passes it at 5. `node.subdivide_mesh` goes to 8.
const MAX_DETAIL_LEVEL: u32 = 5;

/// How many times to split a built-in shape's triangles: none unless a
/// deforming Modifier that is on reaches it, then the fewest splits that give
/// it [`DETAIL_VERTICES`]. Each split multiplies the count by four.
fn detail_level(b: &Builder<'_>, item: &Item, rest: Option<&Rest>) -> u32 {
    let Some(rest) = rest else {
        return 0;
    };
    if !b.deformed.contains(&item.id) {
        return 0;
    }
    let mut level = 0;
    let mut count = u64::from(rest.vertices);
    while level < MAX_DETAIL_LEVEL && count < u64::from(DETAIL_VERTICES) {
        level += 1;
        count *= 4;
    }
    level
}

pub(crate) fn emit(b: &mut Builder<'_>, item: &Item, frame: Option<&Frame>) {
    let ItemFields::Object(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    // Inside a copied group, or drawn as a swarm's particles.
    // An Ocean is one surface drawn once: it has no mesh source to copy.
    let ocean = fields.shape == Shape::Ocean;
    let group_copies = frame.and_then(|frame| frame.copies).or_else(|| b.swarm_draws.get(&item.id).copied()).filter(|_| !ocean);
    let own_copies = fields.copies && can_copy(&fields.shape) && group_copies.is_none();
    // A swarm drawing this object with Copies on carries its layout to every
    // particle, as it does a Group's (`swarm::nest_layout`).
    let nested = fields.copies
        && can_copy(&fields.shape)
        && frame.and_then(|frame| frame.copies).is_none()
        && group_copies.is_some_and(|nest| b.nests.contains_key(&nest));
    let mut center = None;
    let rest;
    let (mesh, vertices) = if let Shape::Mesh { file, node_path, vertex_count, primitives } = &fields.shape {
        let part = super::mesh::emit_part(b, item, file, node_path, *vertex_count, *primitives);
        center = part.center;
        rest = part.rest;
        (part.source, part.vertices)
    } else if ocean {
        let Some(triangles) = ocean::emit_surface(b, item) else {
            return;
        };
        rest = None;
        (triangles, (triangles, "out"))
    } else if let Some(curve) = super::curves::emit(b, item, &fields.shape) {
        rest = curve.rest;
        (curve.solid, curve.vertices)
    } else {
        let Some((mesh_type, solid)) = mesh_type(&fields.shape) else {
            return;
        };
        let mesh = b.add(owner, "mesh", mesh_type);
        if let Some(solid) = solid {
            b.set(mesh, "shape", SerializedParamValue::Enum { value: solid });
        }
        rest = builtin_rest(b, mesh_type, solid);
        let level = detail_level(b, item, rest.as_ref());
        if level > 0 {
            let detail = b.add(owner, "detail", "node.subdivide_mesh");
            b.set(detail, "level", SerializedParamValue::Int { value: level as i32 });
            b.wire(mesh, "vertices", detail, "in");
            (mesh, (detail, "out"))
        } else {
            (mesh, (mesh, "vertices"))
        }
    };
    let transform = b.add(owner, "transform", "node.transform_3d");
    if let Some(center) = center {
        for (axis, param) in ["pos_x", "pos_y", "pos_z"].into_iter().enumerate() {
            b.set(transform, param, SerializedParamValue::Float { value: center[axis] });
        }
    }
    let object = b.add(owner, "object", "node.scene_object");
    if ocean {
        // A sea does not shadow itself; the Shadows row turns it on.
        b.set(object, "cast_shadows", SerializedParamValue::Float { value: 0.0 });
    }
    if let Some(rest) = rest {
        b.drawn.push(Drawn { object, item: item.id.clone(), rest });
    }
    let (surface, maps) = surface_for(b, &item.id);
    // An object that is its own body draws at the body's solved pose.
    // A surface made on the GPU each frame has no collision mesh.
    let pose = if ocean { None } else { super::physics::object(b, item, frame, mesh, transform) };
    for w in object_wires(mesh, surface, transform, object) {
        if w.to_port == "vertices" {
            b.wire(vertices.0, vertices.1, object, "vertices");
            continue;
        }
        // A body's transform is its start, and its own copies' layout
        // centre; the draw reads the solved poses instead.
        if (pose.is_some() || own_copies || nested) && w.to_port == "transform" {
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
        } else if let Some(nest) = group_copies.filter(|_| nested) {
            super::swarm::nest_layout(b, item, transform, nest);
            // The layout body sizes its spacing by the object's solid.
            if let Some(Some(layout)) = b.nests.get(&nest).copied() {
                b.wire(mesh, "source", layout, "source");
            }
            copies::draw_instances(b, nest, object, false);
        } else if let Some(body) = group_copies {
            if can_copy(&fields.shape) {
                copies::add_part(b, body, mesh, transform);
            }
            copies::draw_instances(b, body, object, true);
        }
    } else if let Some(parent) = parent {
        b.wire(parent, "transform", object, "parent_transform");
    }
    b.objects.push(object);
    b.object_items.push(item.id.clone());
    b.rows(item);
    if ocean {
        ocean::follow_dials(b, item);
    }
    super::curves::follow_rows(b, item, &fields.shape);
}
