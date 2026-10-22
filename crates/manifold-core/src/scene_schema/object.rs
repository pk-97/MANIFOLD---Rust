//! Object: its shape's mesh, its transform and its draw (`node.scene_object`).
//! The size of a built-in shape is its Scale; the mesh node's own size stays
//! at one unit.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::fields::{COPIES, Choice, FieldControl, FieldRow, FieldValue, MATERIAL_BINDING, PHYSICS, always, enabled, wrong_value};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, row, setup};
use crate::scene_doc::{Item, ItemFields, MeshModifierKind, ObjectFields, SceneDoc, Shape};

const T: Reach = Reach::Own("transform");
const DRAW: Reach = Reach::Own("object");
const MESH: Reach = Reach::Own("mesh");
const POSE: Reach = Reach::Own("pose");
const SKIN: Reach = Reach::Own("skin");
const WEIGHTS: Reach = Reach::Own("morph_weights");
const DELTAS: Reach = Reach::Own("morph_deltas");
const BLEND: Reach = Reach::Own("morph_blend");
use ControlGroup::{Look, Transform};

fn cube(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Cube)
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
const WHOLE_FILE_NODE: &str = "the file node path picks the mesh";
const FROM_FILE: &str = "read from the model file";
const OWN_CENTRE: &str = "the part's own centre, so it turns about itself";
const GROUP_CLOCK: &str = "the import Group's Animation rows";
const CLOCK_PAYLOAD: &str = "the clip data loads from the model file";

pub static OBJECT_ROWS: [PropertyRow; 72] = [
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
    // The model file's mesh node (`node.gltf_mesh_source`): the Shape field names the file and the
    // file node; the rest keeps the importer's defaults.
    row("file", MESH, Transform, Exposure::Automatic("the Shape field")).param("path").when(from_file),
    row("file_node", MESH, Transform, Exposure::Automatic("the Shape field")).param("node_path").when(from_file),
    row("file_capacity", MESH, Transform, Exposure::Automatic("the vertex count read at import"))
        .param("max_capacity")
        .when(from_file),
    row("file_colours", MESH, Transform, Exposure::Automatic("always on: the file's vertex colours draw")).param("vertex_colors").when(from_file),
    row("file_mesh", MESH, Transform, Exposure::Internal(WHOLE_FILE_NODE)).param("mesh_index").when(from_file),
    row("file_primitive", MESH, Transform, Exposure::Internal(WHOLE_FILE_NODE)).param("primitive_index").when(from_file),
    row("file_material", MESH, Transform, Exposure::Automatic("the Shape field's primitives")).param("material_index").when(from_file),
    row("file_pieces", MESH, Transform, Exposure::Internal("a model is not cut into pieces here")).param("fragment_count").when(from_file),
    row("file_piece", MESH, Transform, Exposure::Internal("a model is not cut into pieces here")).param("fragment_index").when(from_file),
    row("file_fit", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE)).param("fit").when(from_file),
    row("file_recenter", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE)).param("recenter").when(from_file),
    row("file_shift_x", MESH, Transform, Exposure::Automatic(OWN_CENTRE)).param("translate_x").when(from_file),
    row("file_shift_y", MESH, Transform, Exposure::Automatic(OWN_CENTRE)).param("translate_y").when(from_file),
    row("file_shift_z", MESH, Transform, Exposure::Automatic(OWN_CENTRE)).param("translate_z").when(from_file),
    row("file_source_vertices", MESH, Transform, Exposure::Automatic(FROM_FILE)).param("source_vertex_count").when(from_file),
    row("file_source_radius", MESH, Transform, Exposure::Automatic(FROM_FILE)).param("source_bbox_radius").when(from_file),
    // An animated part: its skin, or its node chain as a one-slot palette
    // (`node.gltf_skeleton_pose`), deforming its mesh (`node.skin_mesh`).
    row("pose_file", POSE, Transform, Exposure::Automatic("the Shape field")).param("path").when(from_file),
    row("pose_skin", POSE, Transform, Exposure::Automatic(FROM_FILE)).param("skin_index").when(from_file),
    row("pose_slots", POSE, Transform, Exposure::Automatic(FROM_FILE)).param("node_slots").when(from_file),
    row("pose_joints", POSE, Transform, Exposure::Automatic(FROM_FILE)).param("joint_count").when(from_file),
    row("pose_duration", POSE, Transform, Exposure::Automatic(FROM_FILE)).param("duration_s").when(from_file),
    row("pose_clip_durations", POSE, Transform, Exposure::Automatic(FROM_FILE)).param("clip_durations").when(from_file),
    row("pose_rate", POSE, Transform, Exposure::Automatic(GROUP_CLOCK)).param("rate").when(from_file),
    row("pose_clip", POSE, Transform, Exposure::Automatic(GROUP_CLOCK)).param("clip_index").when(from_file),
    row("pose_loop_mode", POSE, Transform, Exposure::Automatic(GROUP_CLOCK)).param("loop_mode").when(from_file),
    row("pose_retrigger", POSE, Transform, Exposure::Automatic(GROUP_CLOCK)).param("trigger_count").when(from_file),
    row("pose_parents", POSE, Transform, Exposure::Internal(CLOCK_PAYLOAD)).param("joint_parent_table").when(from_file),
    row("pose_root_worlds", POSE, Transform, Exposure::Internal(CLOCK_PAYLOAD)).param("joint_root_world_table").when(from_file),
    row("pose_inverse_binds", POSE, Transform, Exposure::Internal(CLOCK_PAYLOAD)).param("inverse_bind_table").when(from_file),
    row("pose_translations", POSE, Transform, Exposure::Internal(CLOCK_PAYLOAD)).param("translation_tracks").when(from_file),
    row("pose_rotations", POSE, Transform, Exposure::Internal(CLOCK_PAYLOAD)).param("rotation_tracks").when(from_file),
    row("pose_scales", POSE, Transform, Exposure::Internal(CLOCK_PAYLOAD)).param("scale_tracks").when(from_file),
    row("skin_joints", SKIN, Transform, Exposure::Automatic(FROM_FILE)).param("joint_count").when(from_file),
    // A morphed part (`node.gltf_morph_weights`, `node.gltf_morph_deltas_source`,
    // `node.morph_targets_blend`).
    row("morph_file", WEIGHTS, Transform, Exposure::Automatic("the Shape field")).param("path").when(from_file),
    row("morph_node", WEIGHTS, Transform, Exposure::Automatic(FROM_FILE)).param("target_node").when(from_file),
    row("morph_duration", WEIGHTS, Transform, Exposure::Automatic(FROM_FILE)).param("duration_s").when(from_file),
    row("morph_clip_durations", WEIGHTS, Transform, Exposure::Automatic(FROM_FILE)).param("clip_durations").when(from_file),
    row("morph_targets", WEIGHTS, Transform, Exposure::Automatic(FROM_FILE)).param("target_count").when(from_file),
    row("morph_static_weights", WEIGHTS, Transform, Exposure::Automatic(FROM_FILE)).param("static_weights").when(from_file),
    row("morph_rate", WEIGHTS, Transform, Exposure::Automatic(GROUP_CLOCK)).param("rate").when(from_file),
    row("morph_clip", WEIGHTS, Transform, Exposure::Automatic(GROUP_CLOCK)).param("clip_index").when(from_file),
    row("morph_loop_mode", WEIGHTS, Transform, Exposure::Automatic(GROUP_CLOCK)).param("loop_mode").when(from_file),
    row("morph_retrigger", WEIGHTS, Transform, Exposure::Automatic(GROUP_CLOCK)).param("trigger_count").when(from_file),
    row("deltas_file", DELTAS, Transform, Exposure::Automatic("the Shape field")).param("path").when(from_file),
    row("deltas_node", DELTAS, Transform, Exposure::Automatic("the Shape field")).param("node_path").when(from_file),
    row("deltas_material", DELTAS, Transform, Exposure::Automatic("the Shape field's primitives")).param("material_index").when(from_file),
    row("deltas_capacity", DELTAS, Transform, Exposure::Automatic(FROM_FILE)).param("max_capacity").when(from_file),
    row("deltas_skinned", DELTAS, Transform, Exposure::Automatic("whether the part is skinned")).param("skinned").when(from_file),
    row("blend_targets", BLEND, Transform, Exposure::Automatic(FROM_FILE)).param("target_count").when(from_file),
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

/// A part inside an import Group takes its file from the Group's Model File.
fn part_of_import(doc: &SceneDoc, item: &Item) -> Option<&'static str> {
    let mut inside = false;
    doc.walk(|other, ancestors| {
        if other.id == item.id {
            inside = ancestors.iter().any(|above| matches!(&above.fields, ItemFields::Group(group) if group.source.is_some()));
        }
    });
    inside.then_some("This part comes from its group's Model File; swap it there")
}

const MODIFIER_KINDS: &[Choice] = &[
    Choice { key: "surface_peel", label: "Surface Peel" },
    Choice { key: "ordered_recon", label: "Ordered Recon" },
    Choice { key: "surface_waves", label: "Surface Waves" },
    Choice { key: "spatial_echoes", label: "Spatial Echoes" },
];

/// The label a modifier kind shows: its section title and Add entry.
pub fn modifier_label(kind: MeshModifierKind) -> &'static str {
    MODIFIER_KINDS[MeshModifierKind::ALL.iter().position(|k| *k == kind).unwrap_or(0)].label
}

/// Only a mesh from a file takes modifiers, so a built-in solid has no Add
/// Modifier row at all.
fn takes_modifiers(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(object) if super::modifiers_unavailable(&object.shape).is_none())
}

pub static OBJECT_FIELDS: [FieldRow; 6] = [
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
                "mesh" => Shape::Mesh { file: String::new(), node_path: String::new(), vertex_count: 0, primitives: Default::default() },
                _ => return Err(wrong_value("shape", value)),
            };
            Ok(())
        },
    },
    FieldRow {
        id: "model_file",
        label: "Model File",
        section: ControlGroup::Shape,
        control: FieldControl::File { extensions: &["glb", "gltf"] },
        applies: mesh_shape,
        disabled: part_of_import,
        read: |fields| match fields {
            ItemFields::Object(ObjectFields { shape: Shape::Mesh { file, .. }, .. }) => FieldValue::File(file.clone()),
            _ => FieldValue::File(String::new()),
        },
        write: |fields, value| match (fields, value) {
            (ItemFields::Object(ObjectFields { shape: Shape::Mesh { file, .. }, .. }), FieldValue::File(path)) => {
                *file = path.clone();
                Ok(())
            }
            _ => Err(wrong_value("model_file", value)),
        },
    },
    MATERIAL_BINDING,
    PHYSICS,
    COPIES,
    // Adds a modifier at the end of the list; each modifier is its own
    // section, removed and reordered from its header.
    FieldRow {
        id: "add_modifier",
        label: "Add Modifier",
        section: ControlGroup::Modifier,
        control: FieldControl::Closed(MODIFIER_KINDS),
        applies: takes_modifiers,
        disabled: enabled,
        read: |_| FieldValue::Choice(String::new()),
        write: |fields, value| {
            let (ItemFields::Object(object), FieldValue::Choice(key)) = (&*fields, value) else {
                return Err(wrong_value("add_modifier", value));
            };
            let Some(at) = MODIFIER_KINDS.iter().position(|choice| choice.key == key) else {
                return Err(wrong_value("add_modifier", value));
            };
            let next = object.with_modifier(crate::math::short_id(), MeshModifierKind::ALL[at]);
            *fields = ItemFields::Object(next);
            Ok(())
        },
    },
];
