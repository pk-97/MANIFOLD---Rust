//! Records of the particle-frame seam (GPU_FLUID_SURFACE_DESIGN.md section 3
//! (The particle-frame contract)). Any liquid solver publishes
//! `Array(FluidParticle)` frames; the surface atoms read them unchanged.

use manifold_node_engine::channel_names::well_known;
#[cfg(test)]
use manifold_node_engine::particles::FLUID_PARTICLE_SPECS;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::ports::{ChannelElementType, ChannelSpec, KnownItem};

const _: () = {
    use core::mem::{offset_of, size_of};
    use manifold_fluids::ParticleRecord;
    assert!(size_of::<FluidParticle>() == 32);
    assert!(size_of::<FluidParticle>() == size_of::<ParticleRecord>());
    assert!(
        offset_of!(FluidParticle, position_radius) == offset_of!(ParticleRecord, position_radius)
    );
    assert!(offset_of!(FluidParticle, velocity) == offset_of!(ParticleRecord, velocity));
    assert!(offset_of!(FluidParticle, id) == offset_of!(ParticleRecord, id));
};

/// One surface kernel, from `node.shape_particle_blobs`: an ellipsoid round
/// the particle's smoothed centre. `node.particle_volume` blends the blobs
/// near each node into the level set. A point at offset y from the centre is
/// inside the blob when |A y| < r, A the symmetric shape matrix: the identity
/// for a round blob of radius r, flattened along a thin sheet's normal and
/// widened along it, or a multiple of the identity that shrinks a lone drop.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FluidBlob {
    /// Centre xyz; w = r, the round blob's radius in metres, 0 = inactive.
    pub center_radius: [f32; 4],
    /// A's diagonal xx, yy, zz; w = how far the blob's weight reaches over
    /// a round blob's, its longest semi-axis over r · size, which bounds the
    /// nodes it reaches.
    pub shape_diagonal: [f32; 4],
    /// A's xy, xz, yz; w = size, the blob's shrink: the volume weighs it by
    /// |size · A y|, so a shrunk blob blends as far as a full one.
    pub shape_off_diagonal: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<FluidBlob>() == 48);

impl FluidBlob {
    /// A round blob of radius `r` round `centre`.
    pub fn round(centre: [f32; 3], r: f32) -> Self {
        Self {
            center_radius: [centre[0], centre[1], centre[2], r],
            shape_diagonal: [1.0, 1.0, 1.0, 1.0],
            shape_off_diagonal: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

pub const FLUID_BLOB_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::CENTER_RADIUS,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::SHAPE_DIAGONAL,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::SHAPE_OFF_DIAGONAL,
        ty: ChannelElementType::Vec4F,
    },
];

impl KnownItem for FluidBlob {
    const SPECS: &'static [ChannelSpec] = FLUID_BLOB_SPECS;
}

/// One Matter grain as a liquid stepping the same tick sees it
/// (`node.matter_stats`' `grain_view`, a Water step's `sand`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterGrain {
    /// Metres; w = the radius of the sphere of its bulk volume, 0 for an
    /// unused slot.
    pub position_radius: [f32; 4],
    /// Metres per second; w = the share of its bulk its grains fill (sand's
    /// packing, 1 for a material with no pores).
    pub velocity_solid: [f32; 4],
}

pub const MATTER_GRAIN_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::POSITION_RADIUS,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::VELOCITY_SOLID,
        ty: ChannelElementType::Vec4F,
    },
];

impl KnownItem for MatterGrain {
    const SPECS: &'static [ChannelSpec] = MATTER_GRAIN_SPECS;
}

/// The run of sorted particles inside one spatial bin.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CellRange {
    pub start: u32,
    pub count: u32,
}

pub const CELL_RANGE_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::START,
        ty: ChannelElementType::U32,
    },
    ChannelSpec {
        name: well_known::COUNT,
        ty: ChannelElementType::U32,
    },
];

impl KnownItem for CellRange {
    const SPECS: &'static [ChannelSpec] = CELL_RANGE_SPECS;
}

/// The three lower faces of one cell of the GPU FLIP face grid
///A lattice of n cells per axis stores
/// (n + 1)³ of these, padded cell (i, j, k) at i + (nx + 1)·(j + (ny + 1)·k):
/// the x face at (i, j + ½, k + ½)·h, the y face at (i + ½, j, k + ½)·h, the
/// z face at (i + ½, j + ½, k)·h from the lattice minimum. A face whose
/// other two indices reach n does not exist and holds zeros.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FaceSample {
    /// Metres per second along each face's normal; w = 0.
    pub velocity: [f32; 4],
    /// Per face: the particle weight gathered there, or 1/0 valid after the
    /// pressure step; w = 0.
    pub weight: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<FaceSample>() == 32);

pub const FACE_SAMPLE_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::FACE_VELOCITY,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::FACE_WEIGHT,
        ty: ChannelElementType::Vec4F,
    },
];

impl KnownItem for FaceSample {
    const SPECS: &'static [ChannelSpec] = FACE_SAMPLE_SPECS;
}

/// GPU spawn record, layout-compatible with the native whitewater lifecycle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WhitewaterSpawn {
    /// Scene metres; w is lifetime in seconds, ≤ 0 marks an empty slot.
    pub position_lifetime: [f32; 4],
    /// m/s.
    pub velocity: [f32; 3],
    /// FLIP's DiffuseParticleType: 0 bubble, 1 foam, 2 spray.
    pub kind: u32,
}

const _: () = {
    use core::mem::{align_of, offset_of, size_of};
    use manifold_fluids::WhitewaterSpawn as NativeSpawn;
    assert!(size_of::<WhitewaterSpawn>() == 32);
    assert!(size_of::<WhitewaterSpawn>() == size_of::<NativeSpawn>());
    assert!(align_of::<WhitewaterSpawn>() == align_of::<NativeSpawn>());
    assert!(
        offset_of!(WhitewaterSpawn, position_lifetime)
            == offset_of!(NativeSpawn, position_lifetime)
    );
    assert!(offset_of!(WhitewaterSpawn, velocity) == offset_of!(NativeSpawn, velocity));
    assert!(offset_of!(WhitewaterSpawn, kind) == offset_of!(NativeSpawn, kind));
};

/// Std430: position_lifetime Vec4F at 0, velocity Vec3F at 16, kind U32 at
/// 28; stride 32.
pub const WHITEWATER_SPAWN_SPECS: &[ChannelSpec] = &[
    ChannelSpec {
        name: well_known::POSITION_LIFETIME,
        ty: ChannelElementType::Vec4F,
    },
    ChannelSpec {
        name: well_known::VELOCITY,
        ty: ChannelElementType::Vec3F,
    },
    ChannelSpec {
        name: well_known::KIND,
        ty: ChannelElementType::U32,
    },
];

impl KnownItem for WhitewaterSpawn {
    const SPECS: &'static [ChannelSpec] = WHITEWATER_SPAWN_SPECS;
}

/// Bins in a grid of `bins` per axis.
pub fn bin_total(bins: [u32; 3]) -> u64 {
    bins.iter().map(|&n| u64::from(n)).product()
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::ports::std430_stride;

    #[test]
    fn fluid_particle_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(FLUID_PARTICLE_SPECS) as usize,
            std::mem::size_of::<FluidParticle>()
        );
        assert_eq!(
            std430_stride(FLUID_BLOB_SPECS) as usize,
            std::mem::size_of::<FluidBlob>()
        );
        assert_eq!(
            std430_stride(CELL_RANGE_SPECS) as usize,
            std::mem::size_of::<CellRange>()
        );
        assert_eq!(
            std430_stride(FACE_SAMPLE_SPECS) as usize,
            std::mem::size_of::<FaceSample>()
        );
        assert_eq!(
            std430_stride(WHITEWATER_SPAWN_SPECS) as usize,
            std::mem::size_of::<WhitewaterSpawn>()
        );
    }
}
