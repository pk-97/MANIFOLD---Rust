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

use crate::node_graph::ports::{ItemKind, KnownItem};

/// A 3D mesh vertex with surface normal. Used by
/// `node.generate_grid_mesh` and consumed by `node.render_3d_mesh`.
///
/// Layout (32 bytes):
/// - position(12) + pad(4) + normal(12) + pad(4)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub _pad0: f32,
    pub normal: [f32; 3],
    pub _pad1: f32,
}

const _: () = assert!(std::mem::size_of::<MeshVertex>() == 32);

impl KnownItem for MeshVertex {
    const ITEM_KIND: ItemKind = ItemKind::MeshVertex;
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

impl KnownItem for Vec4Vertex {
    const ITEM_KIND: ItemKind = ItemKind::Vec4Vertex;
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

impl KnownItem for InstanceTransform {
    const ITEM_KIND: ItemKind = ItemKind::InstanceTransform;
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

impl KnownItem for CurvePoint {
    const ITEM_KIND: ItemKind = ItemKind::CurvePoint;
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

impl KnownItem for EdgePair {
    const ITEM_KIND: ItemKind = ItemKind::EdgePair;
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

impl KnownItem for Blob {
    const ITEM_KIND: ItemKind = ItemKind::Blob;
}
