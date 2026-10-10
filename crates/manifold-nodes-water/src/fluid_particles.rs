//! Records of the particle-frame seam (GPU_FLUID_SURFACE_DESIGN.md section 3
//! (The particle-frame contract)). Any liquid solver publishes
//! `Array(FluidParticle)` frames; the surface atoms read them unchanged.

use manifold_node_engine::channel_names::well_known;
use manifold_node_engine::particles::FluidParticle;
#[cfg(test)]
use manifold_node_engine::particles::FLUID_PARTICLE_SPECS;
use manifold_node_engine::ports::{ChannelElementType, ChannelSpec, KnownItem};

const _: () = {
    use core::mem::{offset_of, size_of};
    use manifold_fluids::ParticleRecord;
    assert!(size_of::<FluidParticle>() == 32);
    assert!(size_of::<FluidParticle>() == size_of::<ParticleRecord>());
    assert!(offset_of!(FluidParticle, position_radius) == offset_of!(ParticleRecord, position_radius));
    assert!(offset_of!(FluidParticle, velocity) == offset_of!(ParticleRecord, velocity));
    assert!(offset_of!(FluidParticle, id) == offset_of!(ParticleRecord, id));
};

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
    /// G: xy, xz, yz; w = centre displacement from the sorted particle.
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

/// The three lower faces of one cell of the GPU FLIP face grid
/// (docs/GPU_FLIP_PRESSURE_SOLVE.md). A lattice of n cells per axis stores
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
    ChannelSpec { name: well_known::FACE_VELOCITY, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::FACE_WEIGHT, ty: ChannelElementType::Vec4F },
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
    assert!(offset_of!(WhitewaterSpawn, position_lifetime) == offset_of!(NativeSpawn, position_lifetime));
    assert!(offset_of!(WhitewaterSpawn, velocity) == offset_of!(NativeSpawn, velocity));
    assert!(offset_of!(WhitewaterSpawn, kind) == offset_of!(NativeSpawn, kind));
};

/// Std430: position_lifetime Vec4F at 0, velocity Vec3F at 16, kind U32 at
/// 28; stride 32 (GPU_WHITEWATER_DESIGN.md section 3.4).
pub const WHITEWATER_SPAWN_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION_LIFETIME, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::VELOCITY, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::KIND, ty: ChannelElementType::U32 },
];

impl KnownItem for WhitewaterSpawn {
    const SPECS: &'static [ChannelSpec] = WHITEWATER_SPAWN_SPECS;
}

/// Spatial bins covering an axis-aligned box: `max(1, ceil(size / cell))`
/// bins per axis, bin (i, j, k) spanning `min + (i, j, k)·cell`. Only the
/// sort evaluates it; every atom that searches its bins takes the sort's
/// `bins_x/y/z` outputs instead, because a GPU fast-math division of the same
/// floats can land one bin higher (the ratio is usually an exact integer).
pub fn bin_counts(size: [f32; 3], cell_size: f32) -> [u32; 3] {
    size.map(|extent| (extent / cell_size).ceil().max(1.0) as u32)
}

/// Bins in a grid of `bins` per axis.
pub fn bin_total(bins: [u32; 3]) -> u64 {
    bins.iter().map(|&n| u64::from(n)).product()
}

/// Most bins a grid may hold: the searching kernels form the linear bin index
/// in i32.
pub const MAX_BINS: u64 = i32::MAX as u64;

/// The bin grid a searching atom reads from its `bins_x/y/z` wires, checked
/// against the `cell_ranges` storage it indexes: every axis whole and at least
/// 1, and one range per bin. Nothing may read the ranges past this.
pub fn searched_bins(bins: [f32; 3], range_bytes: u64, atom: &str) -> Result<[u32; 3], String> {
    if bins.iter().any(|b| !(b.is_finite() && *b >= 1.0 && b.fract() == 0.0 && *b <= MAX_BINS as f32)) {
        return Err(format!(
            "{atom}: bins_x/y/z must be whole and at least 1; wire them from the node.sort_particles_into_cells that wrote cell_ranges"
        ));
    }
    let bins = bins.map(|b| b as u32);
    let ranges = range_bytes / std::mem::size_of::<CellRange>() as u64;
    let total = bin_total(bins);
    if total > ranges.min(MAX_BINS) {
        return Err(format!(
            "{atom}: a {}×{}×{} bin grid needs {total} cell ranges; cell_ranges holds {ranges}. Wire bins_x/y/z and cell_ranges from the same node.sort_particles_into_cells.",
            bins[0], bins[1], bins[2]
        ));
    }
    Ok(bins)
}

/// View of a `FluidParticle` buffer as the solver's record type, for workers
/// that write the seam from CPU memory.
#[cfg(feature = "gpu-proofs")]
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
    use manifold_node_engine::ports::std430_stride;

    #[test]
    fn fluid_particle_specs_stride_matches_struct() {
        assert_eq!(
            std430_stride(FLUID_PARTICLE_SPECS) as usize,
            std::mem::size_of::<FluidParticle>()
        );
        assert_eq!(std430_stride(FLUID_BLOB_SPECS) as usize, std::mem::size_of::<FluidBlob>());
        assert_eq!(std430_stride(CELL_RANGE_SPECS) as usize, std::mem::size_of::<CellRange>());
        assert_eq!(std430_stride(FACE_SAMPLE_SPECS) as usize, std::mem::size_of::<FaceSample>());
        assert_eq!(std430_stride(WHITEWATER_SPAWN_SPECS) as usize, std::mem::size_of::<WhitewaterSpawn>());
    }
}
