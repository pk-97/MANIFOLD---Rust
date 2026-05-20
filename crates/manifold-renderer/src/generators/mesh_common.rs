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

/// A 4D vertex in homogeneous hypercube space, before 4D rotation
/// and projection-to-3D. Used by Tesseract / Duocylinder /
/// WireframeZoo. 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vec4Vertex {
    pub position: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<Vec4Vertex>() == 16);

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

/// A 2D screen-space line point — the canonical item type for the
/// line family of primitives (Lissajous curves, oscilloscope traces,
/// audio waveforms). 8 bytes.
///
/// Coordinates are in [0, 1] screen space (aspect-corrected by the
/// renderer). Out-of-range values are clamped at render time.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LinePoint {
    pub xy: [f32; 2],
}

const _: () = assert!(std::mem::size_of::<LinePoint>() == 8);

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
