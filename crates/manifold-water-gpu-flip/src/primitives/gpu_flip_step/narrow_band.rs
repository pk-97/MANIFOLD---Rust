//! Narrow Band FLIP (Ferstl et al. 2016) on the step's world tiles: the
//! storage and the passes `shaders/gpu_flip_step/narrow_band.wgsl`
//! documents. Off, nothing here is reserved or encoded, and the step's own
//! passes read no band field.

use super::super::gpu_flip_sheet_distance::SheetDistance;
use super::*;
use manifold_node_engine::scene::world_tiles::NONE;
use manifold_water_liquid::primitives::particle_identity::BirthReservation;
use manifold_water_liquid::tile_storage::pool_len;
use manifold_water_liquid::world_tiles::{
    allocated_args_offset, exhausted_word, fresh_args_offset, list_args_offset,
};

/// `StepParams::narrow_band`: the band runs.
pub(crate) const ON: u32 = 1;
/// The band runs once more and refills the water its grid holds with
/// particles: the tick after Narrow Band is switched off.
pub(crate) const RESTORE: u32 = 2;

/// R, the band's depth in cells: the velocity extension's (`step_band`), so
/// the faces a band particle samples are known wherever the CFL lets it go.
pub(crate) fn band_cells() -> u32 {
    step_band()
}

/// The band's distance is true two cells past R, so the deep test and the
/// reseed's entering test read true values.
fn distance_cells() -> f64 {
    f64::from(band_cells() + 2)
}

/// Bytes per fine cell the band holds while it runs: the interior, its
/// faces, the reseed scan's first level and the distance's fields.
pub(crate) fn pooled_bytes_per_cell() -> u64 {
    4 + size_of::<FaceSample>() as u64
        + 4
        + super::super::gpu_flip_sheet_distance::FIELD_BYTES_PER_CELL
}

const SCAN_LABELS: ScanLabels = ScanLabels {
    blocks: "gpu_flip.step.narrow_band.scan.blocks",
    add: "gpu_flip.step.narrow_band.scan.add",
};

/// The band's persistent fields, for one pool.
struct Storage {
    grid: GpuBuffer,
    grid_faces: GpuBuffer,
    kept: GpuBuffer,
    status: GpuBuffer,
    pool_tiles: u32,
    /// The canonical elements are written by the next step.
    canon_pending: bool,
}

pub(crate) struct NarrowBand {
    distance: SheetDistance,
    scan: PrefixScan,
    storage: Option<Storage>,
    /// The last run had the band on.
    was_on: bool,
}

impl Default for NarrowBand {
    fn default() -> Self {
        Self {
            distance: SheetDistance::to_depth(distance_cells()),
            scan: PrefixScan::default(),
            storage: None,
            was_on: false,
        }
    }
}

/// The fields the start of a substep leaves for the rest of it.
pub(super) struct Band<'a> {
    /// The substep's distance, over the held slots.
    pub(super) distance: &'a GpuBuffer,
}

impl NarrowBand {
    /// At install, so switching the band on mid-show compiles nothing.
    pub(super) fn prepare(&mut self, device: &GpuDevice) {
        self.distance.prepare(device);
        self.scan.prepare(device);
    }

    /// This run's pool and mode from the step's `next` pool and the Narrow
    /// Band switch: on, the restore after it was on, or off. The grid's water
    /// lives on the band's pool and does not move to another, so while the
    /// band holds water on a pool other than `next`, the run stays on that
    /// pool once more and restores the water with particles; the band starts
    /// over on `next` the run after.
    pub(super) fn plan(&self, on: bool, next: u32) -> (u32, u32) {
        match (&self.storage, on, self.was_on) {
            (Some(storage), _, true) if storage.pool_tiles != next => (storage.pool_tiles, RESTORE),
            (_, true, _) => (next, ON),
            (_, false, true) => (next, RESTORE),
            (_, false, false) => (next, 0),
        }
    }

    /// Size the band for `pool` tiles before a run in `mode`; off, it holds
    /// nothing. A new pool starts with no interior.
    pub(super) fn reserve(
        &mut self,
        device: &GpuDevice,
        pool: u32,
        mode: u32,
    ) -> Result<(), String> {
        if mode == 0 {
            self.storage = None;
            return Ok(());
        }
        if self.storage.as_ref().is_none_or(|s| s.pool_tiles != pool) {
            self.storage = None;
            let cells = pool_len(pool);
            let kept = allocate_zeroed(device, 4 * u64::from(pool))?;
            // SAFETY: a shared buffer of `pool` words no GPU work has touched.
            unsafe { kept.write(0, bytemuck::cast_slice(&vec![NONE; pool as usize])) };
            self.storage = Some(Storage {
                grid: allocate(device, 4 * cells)?,
                grid_faces: allocate(device, size_of::<FaceSample>() as u64 * cells)?,
                kept,
                status: allocate_zeroed(device, 16)?,
                pool_tiles: pool,
                canon_pending: true,
            });
        }
        self.distance.reserve(device, pool)?;
        self.scan.buffer(device, pool_len(pool) as usize)?;
        Ok(())
    }

    /// After a run in `mode`: a restore ends the band.
    pub(super) fn finish(&mut self, mode: u32) {
        self.was_on = mode == ON;
        if mode == RESTORE {
            self.storage = None;
        }
    }

    /// The interior, while the band runs.
    pub(super) fn interior(&self) -> Option<&GpuBuffer> {
        self.storage.as_ref().map(|s| &s.grid)
    }

    /// The kept words for the build, while the band runs: the grid's water
    /// holds its tiles, so a restore reaches the water it refills. A restore
    /// writes none after it; the particles it adds hold their own tiles.
    pub(super) fn kept(&self, mode: u32) -> Option<&GpuBuffer> {
        self.storage.as_ref().filter(|_| mode != 0).map(|s| &s.kept)
    }

    fn storage(&self) -> &Storage {
        self.storage.as_ref().expect("narrow band reserved")
    }
}

/// A pass over a list of `tiles`, 512 threads a slot, with the bindings
/// every list pass takes.
fn over(
    enc: &mut GpuEncoder,
    pipeline: &GpuComputePipeline,
    base: &StepParams,
    tiles: &WorldTiles,
    plan: &GpuBuffer,
    offset: u64,
    bindings: &[GpuBinding<'_>],
    label: &str,
) {
    let mut bound = vec![
        uniform(base),
        buffer(29, &tiles.lists().by_rank),
        buffer(46, plan),
        buffer(51, tiles.table()),
    ];
    bound.extend_from_slice(bindings);
    enc.dispatch_compute_indirect(pipeline, &bound, tiles.args(), offset, label);
    enc.compute_memory_barrier_buffers();
}

impl NarrowBand {
    /// After the build's fresh pass: the canonical elements of a new
    /// storage, then the fresh slots' interior and faces.
    pub(super) fn encode_fresh(
        &mut self,
        enc: &mut GpuEncoder,
        pipes: &Pipelines,
        base: &StepParams,
        tiles: &WorldTiles,
    ) {
        let storage = self.storage.as_mut().expect("narrow band reserved");
        let bound = [
            uniform(base),
            buffer(51, tiles.table()),
            buffer(53, &tiles.lists().fresh),
            buffer(65, &storage.grid),
            buffer(66, &storage.grid_faces),
        ];
        if storage.canon_pending {
            enc.dispatch_compute(
                &pipes.nb_canon,
                &bound,
                [1, 1, 1],
                "gpu_flip.step.narrow_band.canon",
            );
            storage.canon_pending = false;
        }
        enc.dispatch_compute_indirect(
            &pipes.nb_fresh,
            &bound,
            tiles.args(),
            fresh_args_offset(tiles.reach().ring_max),
            "gpu_flip.step.narrow_band.fresh",
        );
        enc.compute_memory_barrier_buffers();
    }

    /// After the particle distance into `phi` (C): the union with the
    /// interior, then the substep's distance over the held slots.
    pub(super) fn encode_union<'a>(
        &'a self,
        enc: &mut GpuEncoder,
        pipes: &Pipelines,
        base: &StepParams,
        tiles: &WorldTiles,
        step: &Step<'_>,
        phi: &GpuBuffer,
    ) -> Band<'a> {
        let storage = self.storage();
        over(
            enc,
            &pipes.nb_union,
            base,
            tiles,
            step.clock_plan,
            list_args_offset(0),
            &[
                buffer(5, phi),
                buffer(47, step.identity),
                buffer(65, &storage.grid),
                buffer(69, &storage.status),
            ],
            "gpu_flip.step.narrow_band.union",
        );
        Band {
            distance: self.distance.encode_gated(
                enc,
                phi,
                None,
                base.cell_size,
                step.clock_plan,
                tiles,
            ),
        }
    }

    /// After the gather into `gathered` (C): the deep faces take the grid's.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode_combine(
        &self,
        enc: &mut GpuEncoder,
        pipes: &Pipelines,
        base: &StepParams,
        tiles: &WorldTiles,
        step: &Step<'_>,
        band: &Band<'_>,
        gathered: &GpuBuffer,
        corners: &GpuBuffer,
    ) {
        let storage = self.storage();
        over(
            enc,
            &pipes.nb_combine,
            base,
            tiles,
            step.clock_plan,
            list_args_offset(0),
            &[
                buffer(4, gathered),
                buffer(9, corners),
                buffer(47, step.identity),
                buffer(65, &storage.grid),
                buffer(66, &storage.grid_faces),
                buffer(67, band.distance),
                buffer(69, &storage.status),
            ],
            "gpu_flip.step.narrow_band.combine",
        );
    }

    /// After the move (held): the interior and its faces advected through
    /// `faces`, the step's new faces.
    pub(super) fn encode_advect(
        &self,
        enc: &mut GpuEncoder,
        pipes: &Pipelines,
        base: &StepParams,
        tiles: &WorldTiles,
        step: &Step<'_>,
        band: &Band<'_>,
        faces: &GpuBuffer,
    ) {
        let storage = self.storage();
        let held = allocated_args_offset(tiles.reach().ring_max);
        let bound = [
            buffer(3, faces),
            // The face sampler names the saved and spread faces, which the
            // band never reads; Vulkan refuses a named binding left out.
            buffer(17, faces),
            buffer(18, faces),
            buffer(47, step.identity),
            buffer(65, &storage.grid),
            buffer(66, &storage.grid_faces),
            buffer(67, band.distance),
            buffer(69, &storage.status),
        ];
        over(
            enc,
            &pipes.nb_advect_phi,
            base,
            tiles,
            step.clock_plan,
            held,
            &bound,
            "gpu_flip.step.narrow_band.advect_phi",
        );
        over(
            enc,
            &pipes.nb_advect_faces,
            base,
            tiles,
            step.clock_plan,
            held,
            &bound,
            "gpu_flip.step.narrow_band.advect_faces",
        );
    }

    /// After the move's sort into `sorted` (`ranges` its cells, `record`
    /// the slots in use): on, the deep particles removed; then the cells
    /// entering the band reseeded past the live end, one birth stage the
    /// next sort absorbs.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn encode_reseed(
        &self,
        enc: &mut GpuEncoder,
        pipes: &Pipelines,
        identity_ops: &manifold_water_liquid::primitives::particle_identity::ParticleIdentity,
        base: &StepParams,
        tiles: &WorldTiles,
        step: &Step<'_>,
        band: &Band<'_>,
        sorted: &GpuBuffer,
        ranges: &GpuBuffer,
        record: &GpuBuffer,
        corners: &GpuBuffer,
        faces: &GpuBuffer,
    ) {
        let storage = self.storage();
        let table = tiles.table();
        if base.narrow_band == ON {
            enc.dispatch_compute_indirect(
                &pipes.nb_delete,
                &[
                    uniform(base),
                    buffer(9, corners),
                    buffer(38, sorted),
                    buffer(42, record),
                    buffer(46, step.clock_plan),
                    buffer(51, table),
                    buffer(65, &storage.grid),
                    buffer(67, band.distance),
                ],
                record,
                0,
                "gpu_flip.step.narrow_band.delete",
            );
            enc.compute_memory_barrier_buffers();
        }
        let scan = self.scan.buffer_ref();
        let elements = pool_len(tiles.pool_tiles());
        enc.clear_buffer(scan);
        let reseed = [
            buffer(1, ranges),
            buffer(3, faces),
            // As in the advect: named by the face sampler, never read.
            buffer(17, faces),
            buffer(18, faces),
            buffer(9, corners),
            buffer(38, sorted),
            buffer(47, step.identity),
            buffer(65, &storage.grid),
            buffer(67, band.distance),
            buffer(70, scan),
        ];
        over(
            enc,
            &pipes.nb_reseed_flags,
            base,
            tiles,
            step.clock_plan,
            list_args_offset(0),
            &reseed,
            "gpu_flip.step.narrow_band.reseed_flags",
        );
        self.scan
            .encode_labelled_gated(enc, elements as usize, SCAN_LABELS, step.clock_plan);
        identity_ops.reserve(
            enc,
            BirthReservation {
                particles: sorted,
                identity: step.identity,
                ranges,
                scan,
                plan: step.clock_plan,
                tile_counts: tiles.counts(),
                step_record: Some(record),
                params: [
                    base.particles,
                    elements as u32,
                    elements as u32,
                    exhausted_word(tiles.reach().ring_max) as u32,
                ],
            },
        );
        over(
            enc,
            &pipes.nb_reseed_write,
            base,
            tiles,
            step.clock_plan,
            list_args_offset(0),
            &reseed,
            "gpu_flip.step.narrow_band.reseed_write",
        );
        enc.dispatch_compute(
            &pipes.nb_reseed_tally,
            &[
                uniform(base),
                buffer(1, ranges),
                buffer(46, step.clock_plan),
                buffer(47, step.identity),
                buffer(70, scan),
                GpuBinding::Buffer {
                    binding: 22,
                    buffer: step.capped,
                    offset: step.tally,
                },
            ],
            [1, 1, 1],
            "gpu_flip.step.narrow_band.reseed_tally",
        );
        enc.compute_memory_barrier_buffers();
    }

    /// At the substep's end, while on: each held slot's kept cell for the
    /// next build.
    pub(super) fn encode_keep(
        &self,
        enc: &mut GpuEncoder,
        pipes: &Pipelines,
        base: &StepParams,
        tiles: &WorldTiles,
        step: &Step<'_>,
    ) {
        let storage = self.storage();
        enc.dispatch_compute(
            &pipes.nb_kept_clear,
            &[
                uniform(base),
                buffer(46, step.clock_plan),
                buffer(68, &storage.kept),
            ],
            groups(u64::from(tiles.pool_tiles())),
            "gpu_flip.step.narrow_band.kept_clear",
        );
        enc.compute_memory_barrier_buffers();
        over(
            enc,
            &pipes.nb_keep,
            base,
            tiles,
            step.clock_plan,
            allocated_args_offset(tiles.reach().ring_max),
            &[buffer(65, &storage.grid), buffer(68, &storage.kept)],
            "gpu_flip.step.narrow_band.keep",
        );
    }
}
