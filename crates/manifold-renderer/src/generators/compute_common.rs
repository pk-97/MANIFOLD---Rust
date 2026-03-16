/// Shared compute infrastructure for particle and agent-based generators.
///
/// Provides buffer creation helpers and the shared Particle struct layout
/// that matches Unity's ParticleCommon.cginc (48 bytes per particle).

/// Create a storage buffer with the given byte size.
pub fn create_storage_buffer(device: &wgpu::Device, size: u64, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// Create a storage buffer initialized with zeroes (for atomic accumulators).
pub fn create_zero_buffer(device: &wgpu::Device, size: u64, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    })
    // Note: mapped_at_creation=true + zero-init is handled by wgpu (zeroed memory).
    // We need to unmap after creation — caller must call buffer.unmap().
}

/// Particle struct matching Unity's ParticleCommon.cginc.
/// 48 bytes = 12 floats. Used by FluidSimulation and ComputeStrangeAttractor.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Particle {
    pub position: [f32; 3],   // UV-space position (0-1 range)
    pub velocity: [f32; 3],   // per-frame velocity
    pub life: f32,            // 0=dead, 1=alive
    pub age: f32,             // seconds since spawn
    pub color: [f32; 4],      // RGBA
}

/// Mycelium agent struct (16 bytes = 4 floats).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PhysarumAgent {
    pub pos: [f32; 2],        // UV-space position (0-1 range)
    pub angle: f32,           // heading angle in radians
    pub _pad: f32,
}

/// Fixed-point scale factor for atomic scatter operations.
/// Energy values are multiplied by this before atomicAdd, divided after resolve.
pub const FIXED_POINT_SCALE: f32 = 4096.0;

/// Particle common WGSL source (WangHash, noise, etc.).
/// Include this in compute shaders that need hash/noise functions.
pub const PARTICLE_COMMON_WGSL: &str = include_str!("shaders/particle_common.wgsl");
