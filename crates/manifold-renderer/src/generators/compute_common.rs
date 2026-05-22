//! Shared compute infrastructure for particle and agent-based generators.
//!
//! Provides the shared Particle struct layout that matches Unity's
//! ParticleCommon.cginc (48 bytes per particle).

/// Particle struct matching WGSL layout of ParticleCommon (vec3 alignment = 16).
/// WGSL pads vec3<f32> to 16-byte alignment, so the struct is 64 bytes, not 48.
/// Layout: position(12) + pad(4) + velocity(12) + life(4) + age(4) + pad(12) + color(16) = 64.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Particle {
    pub position: [f32; 3], // UV-space position (0-1 range)
    pub _pad0: f32,         // WGSL vec3 alignment padding
    pub velocity: [f32; 3], // per-frame velocity
    pub life: f32,          // 0=dead, 1=alive (offset 28 — follows vec3 without padding)
    pub age: f32,           // seconds since spawn
    pub _pad1: [f32; 3],    // WGSL padding to align color vec4 to 16 bytes
    pub color: [f32; 4],    // RGBA
}

// Compile-time layout assertions — must match WGSL struct sizes exactly.
// If these fail, you've changed a field without updating the WGSL counterpart (or vice versa).
const _: () = assert!(std::mem::size_of::<Particle>() == 64);

impl crate::node_graph::ports::KnownItem for Particle {
    const ITEM_KIND: crate::node_graph::ports::ItemKind =
        crate::node_graph::ports::ItemKind::Particle;
}

/// Fixed-point scale factor for atomic scatter operations.
/// Energy values are multiplied by this before atomicAdd, divided after resolve.
pub const FIXED_POINT_SCALE: f32 = 4096.0;

/// Particle common WGSL source (WangHash, noise, etc.).
/// Include this in compute shaders that need hash/noise functions.
pub const PARTICLE_COMMON_WGSL: &str = include_str!("shaders/particle_common.wgsl");
