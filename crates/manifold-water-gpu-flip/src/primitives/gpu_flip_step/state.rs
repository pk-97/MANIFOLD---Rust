//! The step state's install, storage reservation and commit mask.

use super::*;
use manifold_water_liquid::tile_storage::pool_len;

impl StepState {
    /// Build every pipeline a step dispatches, at install.
    pub(super) fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::new(device));
        }
        if self.mask_pipeline.is_none() {
            self.mask_pipeline = Some(device.create_compute_pipeline(
                MASK_SHADER,
                "commit_mask",
                "gpu_flip.commit_mask",
            ));
        }
        self.sorter.prepare_pooled(device);
        self.identity_ops.prepare(device);
        self.emit_scan.prepare(device);
        self.solver.prepare_pipelines(device);
        self.bodies.prepare_pipelines(device);
        // Sheet seeding prepares even while its rate is 0: switching it on mid-show
        // must not compile pipelines on the content thread.
        self.sheet_distance.prepare(device);
        self.sheeting.prepare(device);
    }

    /// Size every array for `cells` and `slots` before anything is encoded;
    /// `sources` adds the emission scan; removal reuses the sorted array.
    pub(super) fn reserve(
        &mut self,
        device: &GpuDevice,
        cells: [u32; 3],
        slots: u64,
        ring_max: u32,
        sources: bool,
        sheeting: bool,
    ) -> Result<(), String> {
        if self.zeros.is_none() {
            let zeros = device.try_create_buffer_shared(ZERO_BYTES)?;
            zeros.zero_fill();
            self.zeros = Some(zeros);
        }
        let saved_particle_bytes = slots.max(1) * size_of::<FluidParticle>() as u64;
        if self
            .saved_particles
            .as_ref()
            .is_none_or(|buffer| buffer.size < saved_particle_bytes)
        {
            self.saved_particles = Some(allocate(device, saved_particle_bytes)?);
        }
        let pool = pool_tiles(cells);
        let saved_distance_bytes = pool_cell_bytes(pool);
        if self
            .saved_distance
            .as_ref()
            .is_none_or(|buffer| buffer.size < saved_distance_bytes)
        {
            self.saved_distance = Some(allocate(device, saved_distance_bytes)?);
        }
        let saved_capped_bytes = slots.max(1) * 8 + u64::from(SOLVER_WORDS) * 4;
        if self
            .saved_capped
            .as_ref()
            .is_none_or(|buffer| buffer.size < saved_capped_bytes)
        {
            self.saved_capped = Some(allocate(device, saved_capped_bytes)?);
        }
        // One range per element of a pooled field: the sort is keyed on the
        // pool index of each cell.
        self.sorter.reserve_keys(device, pool_len(pool))?;
        if self.lattice.as_ref().is_none_or(|l| l.cells != cells) {
            self.lattice = None;
            self.tiles = None;
            self.pool_canon_pending = true;
            self.solid_velocity_is_zero = false;
            self.lattice = Some(LatticeBuffers::new(device, cells, pool)?);
        }
        if self.tiles.as_ref().is_none_or(|t| t.ring_max != ring_max) {
            self.tiles = None;
            self.tiles = Some(TileTable::new(device, cells, ring_max)?);
        }
        let sorted = slots.max(1) * size_of::<FluidParticle>() as u64;
        if self
            .sorted
            .as_ref()
            .is_none_or(|buffer| buffer.size < sorted)
        {
            self.sorted = None;
            self.sorted = Some(allocate(device, sorted)?);
        }
        if sources {
            self.emit_scan.buffer(device, emit_sites(cells) as usize)?;
        }
        if sheeting {
            let slot_count = u32::try_from(slots)
                .map_err(|_| "sheeting particle capacity exceeds u32".to_string())?;
            self.sheet_distance.reserve(device, cells, pool)?;
            self.sheeting.reserve(device, cells, slot_count, pool)?;
            if self
                .sheet_order
                .as_ref()
                .is_none_or(|buffer| buffer.size < 4 * slots.max(1))
            {
                self.sheet_order = Some(allocate(device, 4 * slots.max(1))?);
            }
        }
        Ok(())
    }

    pub(super) fn commit_mask(
        &self,
        enc: &mut GpuEncoder,
        plan: &GpuBuffer,
        target: &GpuBuffer,
        saved: &GpuBuffer,
        label: &'static str,
    ) {
        let words = (target.size.min(saved.size) / 4).min(u64::from(u32::MAX)) as u32;
        if words == 0 {
            return;
        }
        let params = [words, 0, 0, 0];
        enc.dispatch_compute(
            self.mask_pipeline.as_ref().expect("mask pipeline prepared"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                buffer(1, plan),
                buffer(2, target),
                buffer(3, saved),
            ],
            groups(u64::from(words)),
            label,
        );
        enc.compute_memory_barrier_buffers();
    }
}
