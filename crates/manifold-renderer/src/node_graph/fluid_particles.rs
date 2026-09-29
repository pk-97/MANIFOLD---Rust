//! Records of the particle-frame seam (GPU_FLUID_SURFACE_DESIGN.md section 3
//! (The particle-frame contract)). Any liquid solver publishes
//! `Array(FluidParticle)` frames; the surface atoms read them unchanged.

use crate::node_graph::channel_names::well_known;
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, KnownItem};

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

const _: () = {
    use core::mem::{offset_of, size_of};
    use manifold_fluids::ParticleRecord;
    assert!(size_of::<FluidParticle>() == 32);
    assert!(size_of::<FluidParticle>() == size_of::<ParticleRecord>());
    assert!(offset_of!(FluidParticle, position_radius) == offset_of!(ParticleRecord, position_radius));
    assert!(offset_of!(FluidParticle, velocity) == offset_of!(ParticleRecord, velocity));
    assert!(offset_of!(FluidParticle, id) == offset_of!(ParticleRecord, id));
};

/// Std430: position_radius Vec4F at 0, velocity Vec3F at 16, id U32 at 28;
/// stride 32 (vec3 + u32 pack into one 16-byte slot, as `Particle`'s
/// velocity/life do).
pub const FLUID_PARTICLE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION_RADIUS, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::VELOCITY, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::ID, ty: ChannelElementType::U32 },
];

impl KnownItem for FluidParticle {
    const SPECS: &'static [ChannelSpec] = FLUID_PARTICLE_SPECS;
}

/// One anisotropic surface kernel (Yu & Turk 2010), from
/// `node.shape_particle_blobs`. The kernel is `(1 − |G·(x − c)|²)³` inside the
/// ellipsoid `|G·(x − c)| < 1`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FluidBlob {
    /// Smoothed centre xyz; w = bounding radius in metres, 0 = inactive.
    pub center_radius: [f32; 4],
    /// Symmetric shape matrix G: xx, yy, zz; w = det(G).
    pub shape_diag: [f32; 4],
    /// G: xy, xz, yz; w = 0.
    pub shape_off: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<FluidBlob>() == 48);

pub const FLUID_BLOB_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::CENTER_RADIUS, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::SHAPE_DIAG, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::SHAPE_OFF, ty: ChannelElementType::Vec4F },
];

impl KnownItem for FluidBlob {
    const SPECS: &'static [ChannelSpec] = FLUID_BLOB_SPECS;
}

/// The run of sorted particles inside one spatial bin.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CellRange {
    pub start: u32,
    pub count: u32,
}

pub const CELL_RANGE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::START, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::COUNT, ty: ChannelElementType::U32 },
];

impl KnownItem for CellRange {
    const SPECS: &'static [ChannelSpec] = CELL_RANGE_SPECS;
}

/// Spatial bins covering an axis-aligned box: `max(1, ceil(size / cell))`
/// bins per axis, bin (i, j, k) spanning `min + (i, j, k)·cell`. One rule for
/// the sort and every atom that searches its bins.
pub fn bin_counts(size: [f32; 3], cell_size: f32) -> [u32; 3] {
    size.map(|extent| (extent / cell_size).ceil().max(1.0) as u32)
}

/// View of a `FluidParticle` buffer as the solver's record type, for workers
/// that write the seam from CPU memory.
pub(crate) fn as_records(particles: &mut [FluidParticle]) -> &mut [manifold_fluids::ParticleRecord] {
    // SAFETY: identical size, field offsets and plain-old-data fields,
    // asserted above; every bit pattern is valid for both.
    unsafe {
        std::slice::from_raw_parts_mut(
            particles.as_mut_ptr().cast::<manifold_fluids::ParticleRecord>(),
            particles.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::std430_stride;

    #[test]
    fn fluid_particle_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(FLUID_PARTICLE_SPECS) as usize,
            std::mem::size_of::<FluidParticle>()
        );
        assert_eq!(std430_stride(FLUID_BLOB_SPECS) as usize, std::mem::size_of::<FluidBlob>());
        assert_eq!(std430_stride(CELL_RANGE_SPECS) as usize, std::mem::size_of::<CellRange>());
    }
}
