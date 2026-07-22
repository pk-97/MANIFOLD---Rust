//! CPU-side mirrors of the Metal structs in `shaders/rt_trace.metal` and
//! `shaders/gbuffer.metal`. Field order and padding must match exactly —
//! these are memcpy'd straight into GPU buffers.

/// Mirrors `TraceParams` in rt_trace.metal. 96 bytes, 16-byte aligned rows.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TraceParams {
    pub sun_dir: [f32; 3],
    pub sun_cone: f32,
    pub sun_color: [f32; 3],
    pub ao_radius: f32,
    pub env_zenith: [f32; 3],
    pub shadow_spp: u32,
    pub env_horizon: [f32; 3],
    pub ao_spp: u32,
    pub gi_spp: u32,
    pub frame_index: u32,
    pub trace_size: [u32; 2],
    pub gbuffer_size: [u32; 2],
    pub _pad0: u32,
    pub _pad1: u32,
}

/// Mirrors `Material` in rt_trace.metal (indexed by `mat_index[primitive_id]`
/// for the GI emissive-gather term). 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RtMaterial {
    pub albedo: [f32; 3],
    pub _p0: f32,
    pub emissive: [f32; 3],
    pub _p1: f32,
}

/// Mirrors `GMaterial` in gbuffer.metal (indexed by per-vertex material_id
/// for the raster shade term). 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GpuMaterial {
    pub albedo: [f32; 3],
    pub _p0: f32,
    pub metallic: f32,
    pub roughness: f32,
    pub _p1: [f32; 2],
    pub emissive: [f32; 3],
    pub _p2: f32,
}

/// Mirrors `CameraUniforms` in gbuffer.metal. 80 bytes (4x4 matrix + vec3 +
/// pad). Column-major, matching Metal's default `float4x4` layout.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CameraUniforms {
    pub view_proj: [f32; 16],
    pub cam_pos: [f32; 3],
    pub _pad0: f32,
}

/// Padded float3 for the `constant float3&` binding in shade_combine's
/// `cam_pos` buffer(2) — MSL's `float3` aligns to 16 bytes as a scalar type,
/// so the backing buffer must reserve the full 16 bytes even though only
/// 12 are read.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PaddedVec3 {
    pub xyz: [f32; 3],
    pub _pad: f32,
}
