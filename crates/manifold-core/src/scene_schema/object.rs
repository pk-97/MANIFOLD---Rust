//! Object: its shape's mesh, its transform and its draw (`node.scene_object`).
//! The size of a built-in shape is its Scale; the mesh node's own size stays
//! at one unit.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::fields::{
    COPIES, Choice, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, PHYSICS, enabled,
    wrong_value,
};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, row, setup};
use crate::scene_doc::{Item, ItemFields, ObjectFields, SceneDoc, Shape};

const T: Reach = Reach::Own("transform");
const DRAW: Reach = Reach::Own("object");
const MESH: Reach = Reach::Own("mesh");
const POSE: Reach = Reach::Own("pose");
const SKIN: Reach = Reach::Own("skin");
const DETAIL: Reach = Reach::Own("detail");
const WEIGHTS: Reach = Reach::Own("morph_weights");
const DELTAS: Reach = Reach::Own("morph_deltas");
const BLEND: Reach = Reach::Own("morph_blend");
use ControlGroup::{Look, Transform};

fn cube(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Cube)
}

fn sphere(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Sphere)
}

fn plane(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Plane)
}

fn from_file(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if matches!(o.shape, Shape::Mesh { .. }))
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
const SPHERE_DETAIL: &str = "a sphere has a fixed vertex count";
const WHOLE_FILE_NODE: &str = "the file node path picks the mesh";
const FROM_FILE: &str = "read from the model file";
const OWN_SPACE: &str =
    "a part draws in its file node's own space, so the node's origin is its pivot";
const GROUP_CLOCK: &str = "the import Group's Animation rows";
const CLOCK_PAYLOAD: &str = "the clip data loads from the model file";

pub static OBJECT_ROWS: [PropertyRow; 78] = [
    row("pos_x", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited),
    row("pos_y", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited),
    row("pos_z", T, Transform, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited),
    row("rot_x", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_y", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_z", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("scale_x", T, Transform, setup()),
    row("scale_y", T, Transform, setup()),
    row("scale_z", T, Transform, setup()),
    row("billboard", T, Transform, setup()),
    row("visible", DRAW, Look, setup()).toggle(),
    row("cast_shadows", DRAW, Look, setup()),
    row("emission_strength", DRAW, Look, setup()).limit(AT_LEAST_ZERO),
    row("gain", DRAW, Look, setup()).limit(AT_LEAST_ZERO),
    row(
        "parent_visible",
        DRAW,
        Look,
        Exposure::Automatic("the enclosing group's visibility"),
    ),
    row(
        "instances_space",
        DRAW,
        Look,
        Exposure::Automatic("the Copies section"),
    ),
    row(
        "cube_size",
        MESH,
        Transform,
        Exposure::Internal(SCALE_SETS_SIZE),
    )
    .param("size")
    .when(cube),
    row(
        "cube_capacity",
        MESH,
        Transform,
        Exposure::Internal("a cube has a fixed vertex count"),
    )
    .param("max_capacity")
    .when(cube),
    row(
        "plane_width",
        MESH,
        Transform,
        Exposure::Internal(SCALE_SETS_SIZE),
    )
    .param("width")
    .when(plane),
    row(
        "plane_height",
        MESH,
        Transform,
        Exposure::Internal(SCALE_SETS_SIZE),
    )
    .param("height")
    .when(plane),
    row(
        "plane_capacity",
        MESH,
        Transform,
        Exposure::Internal("a plane has a fixed vertex count"),
    )
    .param("max_capacity")
    .when(plane),
    // Half a unit, so the sphere fits the unit box a cube fills.
    row(
        "sphere_radius",
        MESH,
        Transform,
        Exposure::Internal(SCALE_SETS_SIZE),
    )
    .param("radius")
    .default(0.5)
    .when(sphere),
    row(
        "sphere_segments",
        MESH,
        Transform,
        Exposure::Internal(SPHERE_DETAIL),
    )
    .param("segments")
    .when(sphere),
    row(
        "sphere_rings",
        MESH,
        Transform,
        Exposure::Internal(SPHERE_DETAIL),
    )
    .param("rings")
    .when(sphere),
    row(
        "solid",
        MESH,
        Transform,
        Exposure::Automatic("the Shape field"),
    )
    .param("shape")
    .when(platonic),
    // Half a unit, so a solid fits the unit box a cube fills.
    row(
        "solid_radius",
        MESH,
        Transform,
        Exposure::Internal(SCALE_SETS_SIZE),
    )
    .param("radius")
    .default(0.5)
    .when(platonic),
    // A built-in shape a deforming Modifier reaches gets its triangles split
    // (`node.subdivide_mesh`) so the bend has vertices to move.
    row(
        "detail_level",
        DETAIL,
        Transform,
        Exposure::Automatic("set while a Modifier bends this shape"),
    )
    .param("level"),
    // The model file's mesh node (`node.gltf_mesh_source`): the Shape field names the file and the
    // file node; the rest keeps the importer's defaults.
    row(
        "file",
        MESH,
        Transform,
        Exposure::Automatic("the Shape field"),
    )
    .param("path")
    .when(from_file),
    row(
        "file_node",
        MESH,
        Transform,
        Exposure::Automatic("the Shape field"),
    )
    .param("node_path")
    .when(from_file),
    row(
        "file_own_space",
        MESH,
        Transform,
        Exposure::Automatic(OWN_SPACE),
    )
    .param("own_space")
    .when(from_file),
    row(
        "file_capacity",
        MESH,
        Transform,
        Exposure::Automatic("the vertex count read at import"),
    )
    .param("max_capacity")
    .when(from_file),
    row(
        "file_colours",
        MESH,
        Transform,
        Exposure::Automatic("always on: the file's vertex colours draw"),
    )
    .param("vertex_colors")
    .when(from_file),
    row(
        "file_mesh",
        MESH,
        Transform,
        Exposure::Internal(WHOLE_FILE_NODE),
    )
    .param("mesh_index")
    .when(from_file),
    row(
        "file_primitive",
        MESH,
        Transform,
        Exposure::Internal(WHOLE_FILE_NODE),
    )
    .param("primitive_index")
    .when(from_file),
    row(
        "file_material",
        MESH,
        Transform,
        Exposure::Automatic("the Shape field's primitives"),
    )
    .param("material_index")
    .when(from_file),
    row(
        "file_pieces",
        MESH,
        Transform,
        Exposure::Internal("a model is not cut into pieces here"),
    )
    .param("fragment_count")
    .when(from_file),
    row(
        "file_piece",
        MESH,
        Transform,
        Exposure::Internal("a model is not cut into pieces here"),
    )
    .param("fragment_index")
    .when(from_file),
    row(
        "file_fit",
        MESH,
        Transform,
        Exposure::Internal(SCALE_SETS_SIZE),
    )
    .param("fit")
    .when(from_file),
    row(
        "file_recenter",
        MESH,
        Transform,
        Exposure::Internal(SCALE_SETS_SIZE),
    )
    .param("recenter")
    .when(from_file),
    row(
        "file_shift_x",
        MESH,
        Transform,
        Exposure::Internal(OWN_SPACE),
    )
    .param("translate_x")
    .when(from_file),
    row(
        "file_shift_y",
        MESH,
        Transform,
        Exposure::Internal(OWN_SPACE),
    )
    .param("translate_y")
    .when(from_file),
    row(
        "file_shift_z",
        MESH,
        Transform,
        Exposure::Internal(OWN_SPACE),
    )
    .param("translate_z")
    .when(from_file),
    row(
        "file_source_vertices",
        MESH,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("source_vertex_count")
    .when(from_file),
    row(
        "file_source_radius",
        MESH,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("source_bbox_radius")
    .when(from_file),
    // An animated part: its skin, or its node chain as a one-slot palette
    // (`node.gltf_skeleton_pose`), deforming its mesh (`node.skin_mesh`).
    row(
        "pose_file",
        POSE,
        Transform,
        Exposure::Automatic("the Shape field"),
    )
    .param("path")
    .when(from_file),
    row("pose_skin", POSE, Transform, Exposure::Automatic(FROM_FILE))
        .param("skin_index")
        .when(from_file),
    row(
        "pose_slots",
        POSE,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("node_slots")
    .when(from_file),
    row(
        "pose_space",
        POSE,
        Transform,
        Exposure::Automatic(OWN_SPACE),
    )
    .param("space_node")
    .when(from_file),
    row(
        "pose_joints",
        POSE,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("joint_count")
    .when(from_file),
    row(
        "pose_duration",
        POSE,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("duration_s")
    .when(from_file),
    row(
        "pose_clip_durations",
        POSE,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("clip_durations")
    .when(from_file),
    row(
        "pose_rate",
        POSE,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("rate")
    .when(from_file),
    row(
        "pose_clip",
        POSE,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("clip_index")
    .when(from_file),
    row(
        "pose_loop_mode",
        POSE,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("loop_mode")
    .when(from_file),
    row(
        "pose_retrigger",
        POSE,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("trigger_count")
    .when(from_file),
    row(
        "pose_parents",
        POSE,
        Transform,
        Exposure::Internal(CLOCK_PAYLOAD),
    )
    .param("joint_parent_table")
    .when(from_file),
    row(
        "pose_root_worlds",
        POSE,
        Transform,
        Exposure::Internal(CLOCK_PAYLOAD),
    )
    .param("joint_root_world_table")
    .when(from_file),
    row(
        "pose_inverse_binds",
        POSE,
        Transform,
        Exposure::Internal(CLOCK_PAYLOAD),
    )
    .param("inverse_bind_table")
    .when(from_file),
    row(
        "pose_translations",
        POSE,
        Transform,
        Exposure::Internal(CLOCK_PAYLOAD),
    )
    .param("translation_tracks")
    .when(from_file),
    row(
        "pose_rotations",
        POSE,
        Transform,
        Exposure::Internal(CLOCK_PAYLOAD),
    )
    .param("rotation_tracks")
    .when(from_file),
    row(
        "pose_scales",
        POSE,
        Transform,
        Exposure::Internal(CLOCK_PAYLOAD),
    )
    .param("scale_tracks")
    .when(from_file),
    row(
        "skin_joints",
        SKIN,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("joint_count")
    .when(from_file),
    // A morphed part (`node.gltf_morph_weights`, `node.gltf_morph_deltas_source`,
    // `node.morph_targets_blend`).
    row(
        "morph_file",
        WEIGHTS,
        Transform,
        Exposure::Automatic("the Shape field"),
    )
    .param("path")
    .when(from_file),
    row(
        "morph_node",
        WEIGHTS,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("target_node")
    .when(from_file),
    row(
        "morph_duration",
        WEIGHTS,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("duration_s")
    .when(from_file),
    row(
        "morph_clip_durations",
        WEIGHTS,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("clip_durations")
    .when(from_file),
    row(
        "morph_targets",
        WEIGHTS,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("target_count")
    .when(from_file),
    row(
        "morph_static_weights",
        WEIGHTS,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("static_weights")
    .when(from_file),
    row(
        "morph_rate",
        WEIGHTS,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("rate")
    .when(from_file),
    row(
        "morph_clip",
        WEIGHTS,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("clip_index")
    .when(from_file),
    row(
        "morph_loop_mode",
        WEIGHTS,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("loop_mode")
    .when(from_file),
    row(
        "morph_retrigger",
        WEIGHTS,
        Transform,
        Exposure::Automatic(GROUP_CLOCK),
    )
    .param("trigger_count")
    .when(from_file),
    row(
        "deltas_file",
        DELTAS,
        Transform,
        Exposure::Automatic("the Shape field"),
    )
    .param("path")
    .when(from_file),
    row(
        "deltas_node",
        DELTAS,
        Transform,
        Exposure::Automatic("the Shape field"),
    )
    .param("node_path")
    .when(from_file),
    row(
        "deltas_material",
        DELTAS,
        Transform,
        Exposure::Automatic("the Shape field's primitives"),
    )
    .param("material_index")
    .when(from_file),
    row(
        "deltas_capacity",
        DELTAS,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("max_capacity")
    .when(from_file),
    row(
        "deltas_own_space",
        DELTAS,
        Transform,
        Exposure::Automatic(OWN_SPACE),
    )
    .param("own_space")
    .when(from_file),
    row(
        "blend_targets",
        BLEND,
        Transform,
        Exposure::Automatic(FROM_FILE),
    )
    .param("target_count")
    .when(from_file),
];

/// The built-in solids, the curve shapes and the Ocean surface: what Shape offers. A model file is not a shape
/// pick; it comes in through Add or a drop and lives in Model File.
const SHAPES: &[Choice] = &[
    Choice {
        key: "cube",
        label: "Cube",
    },
    Choice {
        key: "sphere",
        label: "Sphere",
    },
    Choice {
        key: "plane",
        label: "Plane",
    },
    Choice {
        key: "tetrahedron",
        label: "Tetrahedron",
    },
    Choice {
        key: "octahedron",
        label: "Octahedron",
    },
    Choice {
        key: "icosahedron",
        label: "Icosahedron",
    },
    Choice {
        key: "dodecahedron",
        label: "Dodecahedron",
    },
    Choice {
        key: "ocean",
        label: "Ocean",
    },
    Choice {
        key: "terrain",
        label: "Terrain",
    },
    Choice {
        key: "extrude",
        label: "Extrude",
    },
    Choice {
        key: "revolve",
        label: "Revolve",
    },
    Choice {
        key: "tube",
        label: "Tube",
    },
];

/// An imported object's Shape: its file, kept until a built-in is picked.
const MESH_SHAPES: &[Choice] = &[
    Choice {
        key: "mesh",
        label: "From Model File",
    },
    Choice {
        key: "cube",
        label: "Cube",
    },
    Choice {
        key: "sphere",
        label: "Sphere",
    },
    Choice {
        key: "plane",
        label: "Plane",
    },
    Choice {
        key: "tetrahedron",
        label: "Tetrahedron",
    },
    Choice {
        key: "octahedron",
        label: "Octahedron",
    },
    Choice {
        key: "icosahedron",
        label: "Icosahedron",
    },
    Choice {
        key: "dodecahedron",
        label: "Dodecahedron",
    },
    Choice {
        key: "ocean",
        label: "Ocean",
    },
    Choice {
        key: "terrain",
        label: "Terrain",
    },
    Choice {
        key: "extrude",
        label: "Extrude",
    },
    Choice {
        key: "revolve",
        label: "Revolve",
    },
    Choice {
        key: "tube",
        label: "Tube",
    },
];

fn shape_key(shape: &Shape) -> &'static str {
    match shape {
        Shape::Cube => "cube",
        Shape::Sphere => "sphere",
        Shape::Plane => "plane",
        Shape::Tetrahedron => "tetrahedron",
        Shape::Octahedron => "octahedron",
        Shape::Icosahedron => "icosahedron",
        Shape::Dodecahedron => "dodecahedron",
        Shape::Ocean => "ocean",
        Shape::Terrain { .. } => "terrain",
        Shape::Extrude { .. } => "extrude",
        Shape::Revolve { .. } => "revolve",
        Shape::Tube { .. } => "tube",
        Shape::Mesh { .. } => "mesh",
    }
}

/// The name a new item of `shape` gets: the built-in's label, or the model
/// file's name.
pub fn shape_name(shape: &Shape) -> String {
    match shape {
        Shape::Mesh { file, .. } => std::path::Path::new(file).file_stem().map_or_else(
            || "Mesh".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        ),
        Shape::Ocean => "Ocean".to_owned(),
        builtin => {
            let key = shape_key(builtin);
            SHAPES
                .iter()
                .find(|choice| choice.key == key)
                .map_or(key, |choice| choice.label)
                .to_owned()
        }
    }
}

/// Whether `name` is still the default name for `shape`: its shape name, or
/// that name numbered as `unique_name` numbers it. A renamed item keeps its
/// name when its Shape changes.
pub fn has_shape_name(name: &str, shape: &Shape) -> bool {
    let base = shape_name(shape);
    name == base
        || name
            .strip_prefix(base.as_str())
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(|number| number.parse::<u32>().is_ok_and(|n| n >= 2))
}

fn mesh_shape(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if matches!(o.shape, Shape::Mesh { .. }))
}

fn builtin_shape(fields: &ItemFields) -> bool {
    !mesh_shape(fields)
}

fn read_shape(fields: &ItemFields) -> FieldValue {
    match fields {
        ItemFields::Object(o) => FieldValue::Choice(shape_key(&o.shape).to_owned()),
        _ => FieldValue::Choice(String::new()),
    }
}

/// Set a built-in shape. Picking a built-in on an imported object drops its
/// file; "mesh" only keeps the file an object already has.
fn write_shape(fields: &mut ItemFields, value: &FieldValue) -> Result<(), String> {
    let (ItemFields::Object(o), FieldValue::Choice(key)) = (fields, value) else {
        return Err(wrong_value("shape", value));
    };
    let curve = o.shape.curve().unwrap_or_default();
    let caps = !matches!(
        o.shape,
        Shape::Extrude { caps: false, .. } | Shape::Tube { caps: false, .. }
    );
    o.shape = match key.as_str() {
        "cube" => Shape::Cube,
        "sphere" => Shape::Sphere,
        "plane" => Shape::Plane,
        "tetrahedron" => Shape::Tetrahedron,
        "octahedron" => Shape::Octahedron,
        "icosahedron" => Shape::Icosahedron,
        "dodecahedron" => Shape::Dodecahedron,
        "ocean" => Shape::Ocean,
        // A Terrain keeps its layers through a pick of Terrain again.
        "terrain" => match &o.shape {
            Shape::Terrain { cliff, peak } => Shape::Terrain {
                cliff: cliff.clone(),
                peak: peak.clone(),
            },
            _ => Shape::Terrain {
                cliff: None,
                peak: None,
            },
        },
        "extrude" => Shape::Extrude { curve, caps },
        "revolve" => Shape::Revolve { curve },
        "tube" => Shape::Tube { curve, caps },
        "mesh" if matches!(o.shape, Shape::Mesh { .. }) => o.shape.clone(),
        _ => return Err(wrong_value("shape", value)),
    };
    Ok(())
}

/// A part inside an import Group takes its file from the Group's Model File.
fn part_of_import(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    let mut inside = false;
    doc.walk(|other, ancestors| {
        if other.id == item.id {
            inside = ancestors.iter().any(
                |above| matches!(&above.fields, ItemFields::Group(group) if group.source.is_some()),
            );
        }
    });
    inside.then_some("This part comes from its group's Model File; swap it there")
}

pub static OBJECT_FIELDS: [FieldRow; 11] = [
    FieldRow {
        id: "shape",
        label: "Shape",
        section: ControlGroup::Shape,
        control: FieldControl::Closed(SHAPES),
        applies: builtin_shape,
        disabled: enabled,
        read: read_shape,
        write: write_shape,
    },
    FieldRow {
        id: "shape",
        label: "Shape",
        section: ControlGroup::Shape,
        control: FieldControl::Closed(MESH_SHAPES),
        applies: mesh_shape,
        disabled: part_of_import,
        read: read_shape,
        write: write_shape,
    },
    FieldRow {
        id: "model_file",
        label: "Model File",
        section: ControlGroup::Shape,
        control: FieldControl::File {
            extensions: &["glb", "gltf"],
        },
        applies: mesh_shape,
        disabled: part_of_import,
        read: |fields| match fields {
            ItemFields::Object(ObjectFields {
                shape: Shape::Mesh { file, .. },
                ..
            }) => FieldValue::File(file.clone()),
            _ => FieldValue::File(String::new()),
        },
        write: |fields, value| match (fields, value) {
            (
                ItemFields::Object(ObjectFields {
                    shape: Shape::Mesh { file, .. },
                    ..
                }),
                FieldValue::File(path),
            ) => {
                *file = path.clone();
                Ok(())
            }
            _ => Err(wrong_value("model_file", value)),
        },
    },
    super::curves::CURVE_FIELD,
    super::curves::CAPS_FIELD,
    MATERIAL_BINDING,
    super::terrain::CLIFF_MATERIAL,
    super::terrain::PEAK_MATERIAL,
    PHYSICS,
    COPIES,
    super::copies::COPIES_SURFACE,
];
