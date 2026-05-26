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
