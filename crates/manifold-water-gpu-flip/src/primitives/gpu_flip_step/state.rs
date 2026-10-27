//! The step state's install, storage reservation and slot record.

use super::*;
use manifold_water_liquid::tile_storage::pool_len;

impl StepState {
    /// Build every pipeline a step dispatches, at install.
    pub(super) fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::new(device));
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
        if self.slot_record.is_none() {
            self.slot_record = Some(allocate_zeroed(device, SLOT_RECORD_BYTES)?);
        }
        let pool = pool_tiles(cells);
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

    /// The tick's first slot, before its clock: the slot record becomes the
    /// identity's `[groups, 1, 1, n]`, which a reset or a recovery rewrote
    /// since the last tick.
    pub(super) fn load_slot(&self, enc: &mut GpuEncoder, identity: &GpuBuffer) {
        let pipes = self.pipelines.as_ref().expect("step pipelines prepared");
        let record = self.slot_record.as_ref().expect("slot record reserved");
        enc.dispatch_compute(
            &pipes.slot_load,
            &[buffer(42, record), buffer(47, identity)],
            [1, 1, 1],
            "gpu_flip.step.slot_load",
        );
        enc.compute_memory_barrier_buffers();
    }

    /// After each slot's clock plan: the slot record is the identity's when
    /// the plan has time to step, else it holds no groups, so the slot's
    /// particle passes launch nothing.
    pub(super) fn open_slot(&self, enc: &mut GpuEncoder, plan: &GpuBuffer, identity: &GpuBuffer) {
        let pipes = self.pipelines.as_ref().expect("step pipelines prepared");
        let record = self.slot_record.as_ref().expect("slot record reserved");
        enc.dispatch_compute(
            &pipes.slot_open,
            &[buffer(42, record), buffer(46, plan), buffer(47, identity)],
            [1, 1, 1],
            "gpu_flip.step.slot_open",
        );
        enc.compute_memory_barrier_buffers();
    }
}
