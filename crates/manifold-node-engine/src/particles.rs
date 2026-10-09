//! Shared particle buffer layouts and compute constants.

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

/// Channels signature for [`Particle`] per `docs/CHANNEL_TYPE_SYSTEM.md` section 6.1.
/// Std430 layout reproduces the existing `#[repr(C)]` 64-byte stride:
///   position: Vec3F at 0   (size 12, align 16 → next 16)
///   velocity: Vec3F at 16  (size 12, align 16 → next 28)
///   life:     F32   at 28  (size 4,  align 4  → next 32)
///   age:      F32   at 32  (size 4,  align 4  → next 36)
///   color:    Vec4F at 48  (align-16 pad from 36 → 48; next 64)
///   stride = round_up(64, 16) = 64.
pub const PARTICLE_SPECS: &[crate::ports::ChannelSpec] = &[
    crate::ports::ChannelSpec {
        name: crate::channel_names::well_known::POSITION,
        ty: crate::ports::ChannelElementType::Vec3F,
    },
    crate::ports::ChannelSpec {
        name: crate::channel_names::well_known::VELOCITY,
        ty: crate::ports::ChannelElementType::Vec3F,
    },
    crate::ports::ChannelSpec {
        name: crate::channel_names::well_known::LIFE,
        ty: crate::ports::ChannelElementType::F32,
    },
    crate::ports::ChannelSpec {
        name: crate::channel_names::well_known::AGE,
        ty: crate::ports::ChannelElementType::F32,
    },
    crate::ports::ChannelSpec {
        name: crate::channel_names::well_known::COLOR,
        ty: crate::ports::ChannelElementType::Vec4F,
    },
];

impl crate::ports::KnownItem for Particle {
    const SPECS: &'static [crate::ports::ChannelSpec] = PARTICLE_SPECS;
}

/// Fixed-point scale factor for atomic scatter operations.
/// Energy values are multiplied by this before atomicAdd, divided after resolve.
pub const FIXED_POINT_SCALE: f32 = 4096.0;

/// Particle common WGSL source (WangHash, noise, etc.).
/// Include this in compute shaders that need hash/noise functions.
pub const PARTICLE_COMMON_WGSL: &str = include_str!("shaders/particle_common.wgsl");

#[cfg(test)]
mod particle_specs_drift {
    use super::*;
    use crate::ports::std430_stride;

    #[test]
    fn particle_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(PARTICLE_SPECS) as usize,
            std::mem::size_of::<Particle>(),
            "PARTICLE_SPECS std430 stride drifted from struct Particle size. \
             Update PARTICLE_SPECS or struct Particle so they describe the \
             same byte layout."
        );
    }
}

/// One liquid particle in scene space. Layout equals
/// `manifold_fluids::ParticleRecord`, so the FLIP worker writes it directly.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FluidParticle {
    /// Metres; w = physical radius in metres. w = 0 marks an unused slot.
    pub position_radius: [f32; 4],
    /// Metres per second.
    pub velocity: [f32; 3],
    /// Birth order within the frame's identity epoch. 0 = no identity.
    pub id: u32,
}

/// Std430: position_radius Vec4F at 0, velocity Vec3F at 16, id U32 at 28;
/// stride 32 (vec3 + u32 pack into one 16-byte slot, as `Particle`'s
/// velocity/life do).
pub const FLUID_PARTICLE_SPECS: &[crate::ports::ChannelSpec] = &[
    crate::ports::ChannelSpec { name: crate::channel_names::well_known::POSITION_RADIUS, ty: crate::ports::ChannelElementType::Vec4F },
    crate::ports::ChannelSpec { name: crate::channel_names::well_known::VELOCITY, ty: crate::ports::ChannelElementType::Vec3F },
    crate::ports::ChannelSpec { name: crate::channel_names::well_known::ID, ty: crate::ports::ChannelElementType::U32 },
];

impl crate::ports::KnownItem for FluidParticle {
    const SPECS: &'static [crate::ports::ChannelSpec] = FLUID_PARTICLE_SPECS;
}
