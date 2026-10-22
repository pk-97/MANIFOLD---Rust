//! Object: its shape's mesh, its transform and its draw (`node.scene_object`).
//! The size of a built-in shape is its Scale; the mesh node's own size stays
//! at one unit.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, row, setup};
use crate::scene_doc::{ItemFields, Shape};

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
const MOVE_BY_POSITION: &str = "Position moves the object";
const IMPORT_RECORD: &str = "a provenance note the draw never reads";

pub static OBJECT_ROWS: [PropertyRow; 39] = [
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
    row("file_material", MESH, Transform, Exposure::Internal(WHOLE_FILE_NODE)).param("material_index").when(from_file),
    row("file_pieces", MESH, Transform, Exposure::Internal("a model is not cut into pieces here")).param("fragment_count").when(from_file),
    row("file_piece", MESH, Transform, Exposure::Internal("a model is not cut into pieces here")).param("fragment_index").when(from_file),
    row("file_fit", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE)).param("fit").when(from_file),
    row("file_recenter", MESH, Transform, Exposure::Internal(SCALE_SETS_SIZE)).param("recenter").when(from_file),
    row("file_shift_x", MESH, Transform, Exposure::Internal(MOVE_BY_POSITION)).param("translate_x").when(from_file),
    row("file_shift_y", MESH, Transform, Exposure::Internal(MOVE_BY_POSITION)).param("translate_y").when(from_file),
    row("file_shift_z", MESH, Transform, Exposure::Internal(MOVE_BY_POSITION)).param("translate_z").when(from_file),
    row("file_source_vertices", MESH, Transform, Exposure::Internal(IMPORT_RECORD)).param("source_vertex_count").when(from_file),
    row("file_source_radius", MESH, Transform, Exposure::Internal(IMPORT_RECORD)).param("source_bbox_radius").when(from_file),
];
