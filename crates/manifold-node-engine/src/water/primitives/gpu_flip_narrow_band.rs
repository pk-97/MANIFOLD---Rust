//! Runtime-owned scratch and pipelines for the Ferstl et al. (2016) narrow
//! band FLIP stage.  The numerical method follows F. Ferstl, R. Ando,
//! C. Wojtan, R. Westermann and N. Thuerey, "Narrow Band FLIP for Liquid
//! Simulations", Computer Graphics Forum 35(2), 225–232, 2016,
//! doi:10.1111/cgf.12825.  No code is taken from FLIP Fluids in this module.

use std::mem::size_of;

use manifold_gpu::{GpuBuffer, GpuComputePipeline, GpuDevice};

use crate::particles::{FluidParticle};
use crate::water::fluid_particles::{FaceSample};

use super::prefix_scan::PrefixScan;

const SHADER: &str = include_str!("shaders/gpu_flip_narrow_band.wgsl");
const PIPELINE_LABEL: &str = "node.gpu_flip_narrow_band";

/// The common uniform for every narrow-band pass.  This is deliberately kept
/// identical to the WGSL `NbParams` declaration: twelve 32-bit words.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct NbParams {
    pub(crate) n: [u32; 3],
    pub(crate) slots: u32,
    pub(crate) minimum: [f32; 3],
    pub(crate) h: f32,
    pub(crate) dt: f32,
    pub(crate) axis: u32,
    pub(crate) initialized: u32,
    pub(crate) closed_faces: u32,
}

/// Compiled S1 narrow-band kernels.  The step binds these pipelines itself so
/// that all resources remain in the existing manifold-gpu encoder path.
pub(crate) struct BandPipelines {
    pub advect_phi: GpuComputePipeline,
    pub advect_faces: GpuComputePipeline,
    pub union: GpuComputePipeline,
    pub seed: GpuComputePipeline,
    pub sweep: GpuComputePipeline,
    pub mask: GpuComputePipeline,
    pub distance_support: GpuComputePipeline,
    pub combine: GpuComputePipeline,
    pub delete: GpuComputePipeline,
    pub flags: GpuComputePipeline,
    pub restore_flags: GpuComputePipeline,
    pub status: GpuComputePipeline,
    pub write: GpuComputePipeline,
}

impl BandPipelines {
    fn prepare(device: &GpuDevice) -> Self {
        let pipe = |entry: &str| device.create_compute_pipeline(SHADER, entry, PIPELINE_LABEL);
        Self {
            advect_phi: pipe("nb_advect_phi"),
            advect_faces: pipe("nb_advect_faces"),
            union: pipe("nb_union"),
            seed: pipe("nb_distance_seed"),
            sweep: pipe("nb_distance_sweep"),
            mask: pipe("nb_band_mask"),
            distance_support: pipe("nb_distance_support"),
            combine: pipe("nb_combine_faces"),
            delete: pipe("nb_delete"),
            flags: pipe("nb_reseed_flags"),
            restore_flags: pipe("nb_restore_flags"),
            status: pipe("nb_reseed_status"),
            write: pipe("nb_reseed_write"),
        }
    }
}

/// Narrow-band state whose extents are tied to one FLIP lattice and particle
/// pool.  The external particle, solid and cell-range buffers stay owned by
/// the step; these are the stage's persistent distance, face and lifecycle
/// buffers.
pub(crate) struct BandBuffers {
    pub previous_phi: GpuBuffer,
    pub advected_phi: GpuBuffer,
    pub union_phi: GpuBuffer,
    pub phi: GpuBuffer,
    pub mask: GpuBuffer,
    pub advected_faces: GpuBuffer,
    pub previous_faces: GpuBuffer,
    pub particles: GpuBuffer,
    pub status: GpuBuffer,
    /// Capacity failure latched until the liquid epoch/history is reset.
    pub failure: GpuBuffer,
    pub n: [u32; 3],
    pub slots: u32,
}

/// The narrow-band owner held by `StepState` during S2 integration.
#[derive(Default)]
pub(crate) struct NarrowBand {
    pub pipes: Option<BandPipelines>,
    pub buffers: Option<BandBuffers>,
    pub scan: PrefixScan,
    pub initialized: bool,
}

impl NarrowBand {
    /// Compile every pass at installation time, including the lifecycle
    /// restore pass supplied by the step integration.
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipes.is_none() {
            self.pipes = Some(BandPipelines::prepare(device));
        }
        self.scan.prepare(device);
    }

    /// Allocate exact lattice and particle-pool extents.  A changed lattice
    /// or capacity starts a new distance history; no prior epoch is reused.
    pub(crate) fn reserve(
        &mut self,
        device: &GpuDevice,
        n: [u32; 3],
        slots: u32,
    ) -> Result<(), String> {
        let changed = self
            .buffers
            .as_ref()
            .is_none_or(|buffers| buffers.n != n || buffers.slots != slots);
        if !changed {
            return Ok(());
        }

        let buffers = BandBuffers::new(device, n, slots)?;
        let cell_count = extent_count(n, "cell lattice")?;
        let scan_words = checked_mul(cell_count, 8, "narrow-band scan words")?;
        let scan_words = usize::try_from(scan_words).map_err(|_| {
            "narrow-band scan extent cannot be represented by the scan allocator".to_string()
        })?;
        let mut scan = PrefixScan::default();
        scan.prepare(device);
        scan.buffer(device, scan_words)?;
        self.buffers = Some(buffers);
        self.scan = scan;
        self.initialized = false;
        Ok(())
    }
}

impl BandBuffers {
    fn new(device: &GpuDevice, n: [u32; 3], slots: u32) -> Result<Self, String> {
        if n.contains(&0) {
            return Err(format!(
                "narrow-band lattice dimensions must be nonzero: {n:?}"
            ));
        }
        if slots == 0 {
            return Err("narrow-band particle capacity must be nonzero".to_string());
        }

        let cells = extent_count(n, "cell lattice")?;
        let face_extent = [
            n[0].checked_add(1)
                .ok_or_else(|| "narrow-band face extent overflows u32".to_string())?,
            n[1].checked_add(1)
                .ok_or_else(|| "narrow-band face extent overflows u32".to_string())?,
            n[2].checked_add(1)
                .ok_or_else(|| "narrow-band face extent overflows u32".to_string())?,
        ];
        let faces = extent_count(face_extent, "face lattice")?;
        let cell_bytes = checked_mul(cells, size_of::<f32>() as u64, "narrow-band cell bytes")?;
        let mask_bytes = checked_mul(cells, size_of::<u32>() as u64, "narrow-band mask bytes")?;
        let face_bytes = checked_mul(
            faces,
            size_of::<FaceSample>() as u64,
            "narrow-band face bytes",
        )?;
        let particle_bytes = checked_mul(
            u64::from(slots),
            size_of::<FluidParticle>() as u64,
            "narrow-band particle bytes",
        )?;

        Ok(Self {
            previous_phi: allocate(device, cell_bytes, "previous distance")?,
            advected_phi: allocate(device, cell_bytes, "advected distance")?,
            union_phi: allocate(device, cell_bytes, "union distance")?,
            phi: allocate(device, cell_bytes, "narrow-band distance")?,
            mask: allocate(device, mask_bytes, "narrow-band mask")?,
            advected_faces: allocate(device, face_bytes, "advected faces")?,
            previous_faces: allocate(device, face_bytes, "previous faces")?,
            particles: allocate(device, particle_bytes, "narrow-band particles")?,
            status: allocate(device, 2 * size_of::<u32>() as u64, "narrow-band status")?,
            failure: allocate(device, size_of::<u32>() as u64, "narrow-band failure")?,
            n,
            slots,
        })
    }
}

fn extent_count(extent: [u32; 3], label: &str) -> Result<u64, String> {
    extent.into_iter().try_fold(1u64, |count, axis| {
        count
            .checked_mul(u64::from(axis))
            .ok_or_else(|| format!("{label} extent overflows u64: {extent:?}"))
    })
}

fn checked_mul(value: u64, factor: u64, label: &str) -> Result<u64, String> {
    value
        .checked_mul(factor)
        .ok_or_else(|| format!("{label} overflows u64"))
}

fn allocate(device: &GpuDevice, bytes: u64, label: &str) -> Result<GpuBuffer, String> {
    let buffer = crate::load::expand::admit_candidate_bytes(
        device.modifier_memory_snapshot(),
        bytes,
    )
    .map_err(|error| format!("narrow-band {label} allocation refused: {error}"))
    .and_then(|()| device.try_create_buffer_shared(bytes))
    .map_err(|error| format!("narrow-band {label} allocation failed: {error}"))?;
    buffer.zero_fill();
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nb_params_matches_wgsl_uniform() {
        assert_eq!(size_of::<NbParams>(), 48);
        assert_eq!(std::mem::offset_of!(NbParams, n), 0);
        assert_eq!(std::mem::offset_of!(NbParams, slots), 12);
        assert_eq!(std::mem::offset_of!(NbParams, minimum), 16);
        assert_eq!(std::mem::offset_of!(NbParams, h), 28);
        assert_eq!(std::mem::offset_of!(NbParams, dt), 32);
        assert_eq!(std::mem::offset_of!(NbParams, closed_faces), 44);
    }

    #[test]
    fn nb_extents_are_exact_and_checked() {
        assert_eq!(extent_count([2, 3, 4], "cells").unwrap(), 24);
        assert_eq!(extent_count([3, 4, 5], "faces").unwrap(), 60);
        assert_eq!(checked_mul(24, 8, "scan").unwrap(), 192);
        assert!(extent_count([0, 2, 2], "cells").is_ok());
        assert!(extent_count([u32::MAX, u32::MAX, u32::MAX], "cells").is_err());
        assert!(checked_mul(u64::MAX, 2, "overflow").is_err());
    }
}
