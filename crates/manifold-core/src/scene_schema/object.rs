//! Object: its shape's mesh, its transform and its draw (`node.scene_object`).
//! The size of a built-in shape is its Scale; the mesh node's own size stays
//! at one unit.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::fields::{COPIES, Choice, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, PHYSICS, always, enabled, wrong_value};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, row, setup};
use crate::scene_doc::{ItemFields, ObjectFields, Shape};

const T: Reach = Reach::Own("transform");
const DRAW: Reach = Reach::Own("object");
const MESH: Reach = Reach::Own("mesh");
use ControlGroup::{Look, Transform};

fn cube(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Cube)
}

fn plane(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Plane)
}

fn platonic(fields: &ItemFields) -> bool {
    matches!(
        fields,
        ItemFields::Object(o) if matches!(
            o.shape,
            Shape::Tetrahedron | Shape::Octahedron | Shape::Icosahedron | Shape::Dodecahedron
        )
    )
}

const SCALE_SETS_SIZE: &str = "Scale sets the size";

pub static OBJECT_ROWS: [PropertyRow; 23] = [
    row("pos_x", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_y", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_z", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("rot_x", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_y", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_z", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("scale_x", T, Transform, setup()),
    row("scale_y", T, Transform, setup()),
    row("scale_z", T, Transform, setup()),
    row("billboard", T, Transform, setup()),
    row("visible", DRAW, Look, setup()),
    row("cast_shadows", DRAW, Look, setup()),
    row("emission_strength", DRAW, Look, setup()).limit(AT_LEAST_ZERO),
    row("gain", DRAW, Look, setup()).limit(AT_LEAST_ZERO),
    row("parent_visible", DRAW, Look, Exposure::Automatic("the enclosing group's visibility")),
    row("instances_space", DRAW, Look, Exposure::Automatic("the Copies section")),
    row("cube_size", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE)).param("size").when(cube),
    row("cube_capacity", MESH, Transform, Exposure::Internal("a cube has a fixed vertex count"))
        .param("max_capacity")
        .when(cube),
    row("plane_width", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE)).param("width").when(plane),
    row("plane_height", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE)).param("height").when(plane),
    row("plane_capacity", MESH, Transform, Exposure::Internal("a plane has a fixed vertex count"))
        .param("max_capacity")
        .when(plane),
    row("solid", MESH, Transform, Exposure::Automatic("the Shape field")).param("shape").when(platonic),
    // Half a unit, so a solid fits the unit box a cube fills.
    row("solid_radius", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE))
        .param("radius")
        .default(0.5)
        .when(platonic),
];

const SHAPES: &[Choice] = &[
    Choice { key: "cube", label: "Cube" },
    Choice { key: "plane", label: "Plane" },
    Choice { key: "tetrahedron", label: "Tetrahedron" },
    Choice { key: "octahedron", label: "Octahedron" },
    Choice { key: "icosahedron", label: "Icosahedron" },
    Choice { key: "dodecahedron", label: "Dodecahedron" },
    Choice { key: "ocean", label: "Ocean" },
    Choice { key: "mesh", label: "Mesh from file" },
];

fn shape_key(shape: &Shape) -> &'static str {
    match shape {
        Shape::Cube => "cube",
        Shape::Plane => "plane",
        Shape::Tetrahedron => "tetrahedron",
        Shape::Octahedron => "octahedron",
        Shape::Icosahedron => "icosahedron",
        Shape::Dodecahedron => "dodecahedron",
        Shape::Ocean => "ocean",
        Shape::Mesh { .. } => "mesh",
    }
}

fn mesh_shape(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if matches!(o.shape, Shape::Mesh { .. }))
}

pub static OBJECT_FIELDS: [FieldRow; 5] = [
    FieldRow {
        id: "shape",
        label: "Shape",
        section: ControlGroup::Shape,
        control: FieldControl::Closed(SHAPES),
        applies: always,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Object(o) => FieldValue::Choice(shape_key(&o.shape).to_owned()),
            _ => FieldValue::Choice(String::new()),
        },
        write: |fields, value| {
            let (ItemFields::Object(o), FieldValue::Choice(key)) = (fields, value) else {
                return Err(wrong_value("shape", value));
            };
            o.shape = match key.as_str() {
                "cube" => Shape::Cube,
                "plane" => Shape::Plane,
                "tetrahedron" => Shape::Tetrahedron,
                "octahedron" => Shape::Octahedron,
                "icosahedron" => Shape::Icosahedron,
                "dodecahedron" => Shape::Dodecahedron,
                "ocean" => Shape::Ocean,
                "mesh" if matches!(o.shape, Shape::Mesh { .. }) => o.shape.clone(),
                "mesh" => Shape::Mesh { file: String::new(), node_path: String::new() },
                _ => return Err(wrong_value("shape", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "mesh_file",
        label: "Mesh File",
        section: ControlGroup::Shape,
        control: FieldControl::File { extensions: &["glb", "gltf"] },
        applies: mesh_shape,
        disabled: enabled,
        read: |fields| match fields {
            ItemFields::Object(ObjectFields { shape: Shape::Mesh { file, .. }, .. }) => FieldValue::File(file.clone()),
            _ => FieldValue::File(String::new()),
        },
        write: |fields, value| match (fields, value) {
            (ItemFields::Object(ObjectFields { shape: Shape::Mesh { file, .. }, .. }), FieldValue::File(path)) => {
                *file = path.clone();
                Ok(())
            }
            _ => Err(wrong_value("mesh_file", value)),
        },
    },
    MATERIAL_BINDING,
    PHYSICS,
    COPIES,
];
