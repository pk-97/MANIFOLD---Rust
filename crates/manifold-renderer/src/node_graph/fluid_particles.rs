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

/// One air-collar entry of the FFT water pressure solve in the six-view
/// surface helper (FFT_WATER_SOLVER_DESIGN.md D4), from `node.chart_entries`.
/// View v = 2·axis + (0 for +, 1 for −).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ChartEntry {
    /// Share of the +x, +y, +z views: max(outward normal component, 0)
    /// over the square root of the entry count of its chart slot.
    pub view_plus: [f32; 3],
    /// Sheet per view, 4 bits each, view v at bits 4v.
    pub sheets: u32,
    /// Share of the −x, −y, −z views, as `view_plus`.
    pub view_minus: [f32; 3],
    /// The entry's cell; u32::MAX for an empty entry.
    pub cell: u32,
}

const _: () = assert!(std::mem::size_of::<ChartEntry>() == 32);

/// Std430: vec3 + u32 pack into one 16-byte slot twice; stride 32.
pub const CHART_ENTRY_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::VIEW_PLUS, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::SHEETS, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::VIEW_MINUS, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::CELL, ty: ChannelElementType::U32 },
];

impl KnownItem for ChartEntry {
    const SPECS: &'static [ChannelSpec] = CHART_ENTRY_SPECS;
}

/// The three lower faces of one cell of the FFT water face grid
/// (FFT_WATER_SOLVER_DESIGN.md D2). A lattice of n cells per axis stores
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

/// Std430: position_lifetime Vec4F at 0, velocity Vec3F at 16, kind U32 at
/// 28; stride 32. The record is manifold_fluids' own, so the GPU spawn atoms
/// and the lifecycle share one definition (GPU_WHITEWATER_DESIGN.md section 3.4).
pub const WHITEWATER_SPAWN_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION_LIFETIME, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::VELOCITY, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::KIND, ty: ChannelElementType::U32 },
];

impl KnownItem for manifold_fluids::WhitewaterSpawn {
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
        assert_eq!(std430_stride(CHART_ENTRY_SPECS) as usize, std::mem::size_of::<ChartEntry>());
        assert_eq!(std430_stride(FACE_SAMPLE_SPECS) as usize, std::mem::size_of::<FaceSample>());
    }
}
