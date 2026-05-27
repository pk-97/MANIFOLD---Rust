//! Shared mesh-domain item layouts used by Phase B of
//! `BUFFER_PORT_PLAN` — the primitives that decompose 3D and
//! 4D wireframe generators (MetallicGlass, Tesseract,
//! Duocylinder, WireframeZoo, NestedCubes, DigitalPlants).
//!
//! Each struct is `#[repr(C)]` + `bytemuck::Pod` so it can flow
//! through an `Array<T>` wire and be read by both Rust producers
//! and WGSL shaders with matching layouts. Sizes are asserted at
//! compile time — if these break, the matching WGSL struct
//! definitions need updating.
//!
//! Every struct here implements
//! [`KnownItem`](crate::node_graph::ports::KnownItem) so the
//! `primitive!` macro's `Array(T)` declaration carries the
//! coordinate / convention tag on the wire — see
//! [`ItemKind`](crate::node_graph::ports::ItemKind).

use crate::node_graph::channel_names::well_known;
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, ItemKind, KnownItem};

/// A 3D mesh vertex with surface normal and UV. Used by
/// `node.generate_grid_mesh` and consumed by `node.render_3d_mesh`.
///
/// Layout (48 bytes, std430 / 16-byte aligned):
/// - position(12) + pad(4)
/// - normal(12)   + pad(4)
/// - uv(8)        + pad(8)
///
/// The 8-byte `_pad2` tail is the reserved slot for adding
/// `tangent: [f32; 4]` in a follow-up extension (tangent-space
/// normal mapping) without another layout change.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub _pad0: f32,
    pub normal: [f32; 3],
    pub _pad1: f32,
    pub uv: [f32; 2],
    pub _pad2: [f32; 2],
}

const _: () = assert!(std::mem::size_of::<MeshVertex>() == 48);

/// Channels signature for [`MeshVertex`] per `docs/CHANNEL_TYPE_SYSTEM.md` §6.2.
/// Std430 stride 48 matches the existing `#[repr(C)]` struct (vec3+vec3+vec2
/// with std430 alignment producing the same 48 bytes).
pub const MESH_VERTEX_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::NORMAL,   ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::UV,       ty: ChannelElementType::Vec2F },
];

impl KnownItem for MeshVertex {
    const ITEM_KIND: ItemKind = ItemKind::MeshVertex;
    const SPECS: &'static [ChannelSpec] = MESH_VERTEX_SPECS;
}

/// A 4D vertex in homogeneous hypercube space, before 4D rotation
/// and projection-to-3D. Used by Tesseract / Duocylinder /
/// WireframeZoo. 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vec4Vertex {
    pub position: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<Vec4Vertex>() == 16);

/// Channels signature for [`Vec4Vertex`] per `docs/CHANNEL_TYPE_SYSTEM.md` §6.4.
/// Paired scalars (x, y, z, w) at 4-byte alignment — preserves byte parity
/// with `[f32; 4]` consumers. A single `position: Vec4F` would force 16-byte
/// alignment and break those.
pub const VEC4_VERTEX_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::X, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::Y, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::Z, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::W, ty: ChannelElementType::F32 },
];

impl KnownItem for Vec4Vertex {
    const ITEM_KIND: ItemKind = ItemKind::Vec4Vertex;
    const SPECS: &'static [ChannelSpec] = VEC4_VERTEX_SPECS;
}

/// Per-instance transform for instanced mesh rendering. Matches
/// the existing `generators::mesh_pipeline::MeshInstance` layout
/// so legacy generators and graph primitives speak the same
/// bytes. 32 bytes.
///
/// - `pos_scale`: xyz world position, w uniform scale
/// - `rot_pad`: xyz Euler rotation (radians, XYZ order), w padding
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceTransform {
    pub pos_scale: [f32; 4],
    pub rot_pad: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<InstanceTransform>() == 32);

/// Channels signature for [`InstanceTransform`] per `docs/CHANNEL_TYPE_SYSTEM.md` §6.6.
/// Two Vec4F slots — std430 stride 32, align 16. The struct's Rust-side
/// `align_of` is 4 (from `[f32; 4]`); the std430 alignment of 16 takes
/// over at GPU bind time, which is what the existing shaders already
/// expect.
pub const INSTANCE_TRANSFORM_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POS_SCALE, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::ROT,       ty: ChannelElementType::Vec4F },
];

impl KnownItem for InstanceTransform {
    const ITEM_KIND: ItemKind = ItemKind::InstanceTransform;
    const SPECS: &'static [ChannelSpec] = INSTANCE_TRANSFORM_SPECS;
}

/// A 2D point in **origin-centered pre-aspect curve space** — the
/// canonical wire type between every curve / wireframe producer
/// (`pack_curve_xy`, `project_3d`, `project_4d`,
/// `polygon_shape`, `concentric_outlines`) and `node.render_lines`.
/// 8 bytes.
///
/// Coordinates are centred at the origin: a value of `(0.0, 0.0)`
/// renders at the visual centre of the output texture. The
/// consumer (`node.render_lines`) applies aspect correction and
/// the `+0.5` screen shift in its vertex shader. **Do not pre-shift
/// in the producer** — that would double-apply the offset and the
/// drawing would cluster near the top-right of the output.
///
/// This contract is enforced by the
/// [`KnownItem`](crate::node_graph::ports::KnownItem) impl: any
/// producer whose `Array(CurvePoint)` output wires into
/// `Array(CurvePoint)` declares this convention; the wire validator
/// refuses to connect a `CurvePoint` output to a port expecting
/// any other [`ItemKind`](crate::node_graph::ports::ItemKind).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CurvePoint {
    pub xy: [f32; 2],
}

const _: () = assert!(std::mem::size_of::<CurvePoint>() == 8);

/// Channels signature for [`CurvePoint`] per `docs/CHANNEL_TYPE_SYSTEM.md` §6.3.
/// Paired scalars (x, y) at 4-byte alignment, not a single Vec2F —
/// the §13(3) resolution: a Vec2F here would force 8-byte alignment
/// and break consumers expecting the existing `[f32; 2]` layout.
pub const CURVE_POINT_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::X, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::Y, ty: ChannelElementType::F32 },
];

impl KnownItem for CurvePoint {
    const ITEM_KIND: ItemKind = ItemKind::CurvePoint;
    const SPECS: &'static [ChannelSpec] = CURVE_POINT_SPECS;
}

/// An explicit edge between two vertices in an `Array<CurvePoint>` or
/// `Array<MeshVertex>` buffer, identified by their indices. Used by
/// wireframe-shape producers (Platonic solids, Tesseract, Duocylinder,
/// any future user-imported wireframe mesh) to tell `node.render_lines`
/// which vertex pairs to connect, when the topology isn't the implicit
/// "sequential" / "closed loop" pattern that curve generators use. 8
/// bytes.
///
/// Unused slots in a fixed-capacity edge buffer are marked with
/// `a == u32::MAX` (sentinel); `node.render_lines` skips any slot
/// matching this on its way through the buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EdgePair {
    pub a: u32,
    pub b: u32,
}

impl EdgePair {
    /// Sentinel value for unused slots in a fixed-capacity edges
    /// buffer. `node.render_lines` checks `a == u32::MAX` and skips.
    pub const SENTINEL: EdgePair = EdgePair {
        a: u32::MAX,
        b: u32::MAX,
    };
}

const _: () = assert!(std::mem::size_of::<EdgePair>() == 8);

/// Channels signature for [`EdgePair`] per `docs/CHANNEL_TYPE_SYSTEM.md` §6.5.
/// Two `u32` index channels at 4-byte alignment. Matches the struct's
/// 8-byte size; the `SENTINEL` (a == u32::MAX) for unused slots survives
/// as an associated constant on the Rust struct.
pub const EDGE_PAIR_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::A_INDEX, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::B_INDEX, ty: ChannelElementType::U32 },
];

impl KnownItem for EdgePair {
    const ITEM_KIND: ItemKind = ItemKind::EdgePair;
    const SPECS: &'static [ChannelSpec] = EDGE_PAIR_SPECS;
}

// ── Platonic-solid schema (shared by polytope_vertices + polytope_edges) ──
//
// Five Platonic solids exist in 3D; the set is mathematically closed
// (Euclid, ~300 BC), so the variant labels and per-shape constants
// live as compiled-in tables here in `mesh_common`, imported by both
// the GPU vertex atom and the CPU edge atom so the schema can never
// drift between them.

/// Shape enum labels. Order is the public wire contract — saved JSON
/// presets reference these by index. Never reorder; never remove. New
/// non-Platonic shape families (geodesic sphere, prism, loaded mesh)
/// ship as sibling primitives, not as appended entries here.
pub const PLATONIC_SHAPES: &[&str] = &[
    "Tetrahedron",
    "Cube",
    "Octahedron",
    "Icosahedron",
    "Dodecahedron",
];

/// Number of Platonic-solid variants. Matches `PLATONIC_SHAPES.len()`.
pub const PLATONIC_SHAPE_COUNT: u32 = 5;

/// Maximum vertex count across the five shapes (Dodecahedron = 20).
/// Array<MeshVertex> output capacity for `node.polytope_vertices`.
pub const PLATONIC_MAX_VERTS: u32 = 20;

/// Maximum edge count across the five shapes (Icosa / Dodeca = 30).
/// Array<EdgePair> output capacity for `node.polytope_edges`.
pub const PLATONIC_MAX_EDGES: u32 = 30;

const TETRA_EDGES: [EdgePair; 6] = [
    EdgePair { a: 0, b: 1 },
    EdgePair { a: 0, b: 2 },
    EdgePair { a: 0, b: 3 },
    EdgePair { a: 1, b: 2 },
    EdgePair { a: 1, b: 3 },
    EdgePair { a: 2, b: 3 },
];

const CUBE_EDGES: [EdgePair; 12] = [
    EdgePair { a: 0, b: 1 },
    EdgePair { a: 1, b: 2 },
    EdgePair { a: 2, b: 3 },
    EdgePair { a: 3, b: 0 },
    EdgePair { a: 4, b: 5 },
    EdgePair { a: 5, b: 6 },
    EdgePair { a: 6, b: 7 },
    EdgePair { a: 7, b: 4 },
    EdgePair { a: 0, b: 4 },
    EdgePair { a: 1, b: 5 },
    EdgePair { a: 2, b: 6 },
    EdgePair { a: 3, b: 7 },
];

const OCTA_EDGES: [EdgePair; 12] = [
    EdgePair { a: 0, b: 2 },
    EdgePair { a: 0, b: 3 },
    EdgePair { a: 0, b: 4 },
    EdgePair { a: 0, b: 5 },
    EdgePair { a: 1, b: 2 },
    EdgePair { a: 1, b: 3 },
    EdgePair { a: 1, b: 4 },
    EdgePair { a: 1, b: 5 },
    EdgePair { a: 2, b: 4 },
    EdgePair { a: 2, b: 5 },
    EdgePair { a: 3, b: 4 },
    EdgePair { a: 3, b: 5 },
];

const ICOSA_EDGES: [EdgePair; 30] = [
    EdgePair { a: 0, b: 1 },
    EdgePair { a: 0, b: 5 },
    EdgePair { a: 0, b: 7 },
    EdgePair { a: 0, b: 10 },
    EdgePair { a: 0, b: 11 },
    EdgePair { a: 1, b: 5 },
    EdgePair { a: 1, b: 7 },
    EdgePair { a: 1, b: 8 },
    EdgePair { a: 1, b: 9 },
    EdgePair { a: 2, b: 3 },
    EdgePair { a: 2, b: 4 },
    EdgePair { a: 2, b: 6 },
    EdgePair { a: 2, b: 10 },
    EdgePair { a: 2, b: 11 },
    EdgePair { a: 3, b: 4 },
    EdgePair { a: 3, b: 6 },
    EdgePair { a: 3, b: 8 },
    EdgePair { a: 3, b: 9 },
    EdgePair { a: 4, b: 5 },
    EdgePair { a: 4, b: 9 },
    EdgePair { a: 4, b: 11 },
    EdgePair { a: 5, b: 9 },
    EdgePair { a: 5, b: 11 },
    EdgePair { a: 6, b: 7 },
    EdgePair { a: 6, b: 8 },
    EdgePair { a: 6, b: 10 },
    EdgePair { a: 7, b: 8 },
    EdgePair { a: 7, b: 10 },
    EdgePair { a: 8, b: 9 },
    EdgePair { a: 10, b: 11 },
];

const DODECA_EDGES: [EdgePair; 30] = [
    EdgePair { a: 0, b: 8 },
    EdgePair { a: 0, b: 12 },
    EdgePair { a: 0, b: 16 },
    EdgePair { a: 1, b: 9 },
    EdgePair { a: 1, b: 13 },
    EdgePair { a: 1, b: 16 },
    EdgePair { a: 2, b: 10 },
    EdgePair { a: 2, b: 12 },
    EdgePair { a: 2, b: 17 },
    EdgePair { a: 3, b: 11 },
    EdgePair { a: 3, b: 13 },
    EdgePair { a: 3, b: 17 },
    EdgePair { a: 4, b: 8 },
    EdgePair { a: 4, b: 14 },
    EdgePair { a: 4, b: 18 },
    EdgePair { a: 5, b: 9 },
    EdgePair { a: 5, b: 15 },
    EdgePair { a: 5, b: 18 },
    EdgePair { a: 6, b: 10 },
    EdgePair { a: 6, b: 14 },
    EdgePair { a: 6, b: 19 },
    EdgePair { a: 7, b: 11 },
    EdgePair { a: 7, b: 15 },
    EdgePair { a: 7, b: 19 },
    EdgePair { a: 8, b: 9 },
    EdgePair { a: 10, b: 11 },
    EdgePair { a: 12, b: 14 },
    EdgePair { a: 13, b: 15 },
    EdgePair { a: 16, b: 17 },
    EdgePair { a: 18, b: 19 },
];

/// Edge-pair table for the Platonic solid at `shape` (0..=4). Falls
/// back to the dodecahedron table on out-of-range indices so a clamped
/// upstream selector never reads garbage.
pub fn platonic_edges(shape: u32) -> &'static [EdgePair] {
    match shape {
        0 => &TETRA_EDGES,
        1 => &CUBE_EDGES,
        2 => &OCTA_EDGES,
        3 => &ICOSA_EDGES,
        _ => &DODECA_EDGES,
    }
}

/// A detected blob (bounding box) emitted by the FFI blob detector
/// and consumed by overlay-render primitives. 16 bytes.
///
/// All four components are in normalized 0..1 image space: `x` /
/// `y` are the top-left corner, `width` / `height` are the box
/// extents. Out-of-range values are clamped at render time.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable, Debug, Default)]
pub struct Blob {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

const _: () = assert!(std::mem::size_of::<Blob>() == 16);

/// Channels signature for [`Blob`] per `docs/CHANNEL_TYPE_SYSTEM.md` §6.7.
/// Four scalar f32 channels (x, y, width, height) — generic for any
/// rectangle stream (detector boxes, face regions, sprite rects, viewport
/// extents). Phase 4 deletes the `Blob` struct entirely; the wire type
/// becomes `Channels[x, y, width, height]` with no Rust struct anchor.
pub const BLOB_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::X,      ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::Y,      ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::WIDTH,  ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::HEIGHT, ty: ChannelElementType::F32 },
];

impl KnownItem for Blob {
    const ITEM_KIND: ItemKind = ItemKind::Blob;
    const SPECS: &'static [ChannelSpec] = BLOB_SPECS;
}

#[cfg(test)]
mod mesh_common_specs_drift {
    use super::*;
    use crate::node_graph::ports::std430_stride;

    #[test]
    fn mesh_vertex_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(MESH_VERTEX_SPECS) as usize,
            std::mem::size_of::<MeshVertex>(),
            "MESH_VERTEX_SPECS std430 stride drifted from struct MeshVertex size."
        );
    }

    #[test]
    fn vec4_vertex_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(VEC4_VERTEX_SPECS) as usize,
            std::mem::size_of::<Vec4Vertex>(),
            "VEC4_VERTEX_SPECS std430 stride drifted from struct Vec4Vertex size."
        );
    }

    #[test]
    fn instance_transform_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(INSTANCE_TRANSFORM_SPECS) as usize,
            std::mem::size_of::<InstanceTransform>(),
            "INSTANCE_TRANSFORM_SPECS std430 stride drifted from struct InstanceTransform size."
        );
    }

    #[test]
    fn curve_point_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(CURVE_POINT_SPECS) as usize,
            std::mem::size_of::<CurvePoint>(),
            "CURVE_POINT_SPECS std430 stride drifted from struct CurvePoint size."
        );
    }

    #[test]
    fn edge_pair_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(EDGE_PAIR_SPECS) as usize,
            std::mem::size_of::<EdgePair>(),
            "EDGE_PAIR_SPECS std430 stride drifted from struct EdgePair size."
        );
    }

    #[test]
    fn blob_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(BLOB_SPECS) as usize,
            std::mem::size_of::<Blob>(),
            "BLOB_SPECS std430 stride drifted from struct Blob size."
        );
    }
}
