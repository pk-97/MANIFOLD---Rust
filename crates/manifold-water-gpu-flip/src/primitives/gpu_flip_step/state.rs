//! The step state's install, storage reservation, slot record and plan
//! readbacks.

use super::*;
use manifold_water_liquid::tile_storage::pool_len;
use manifold_water_liquid::world_tiles::LIQUID_POINTS;

impl StepState {
    /// Build every pipeline a step dispatches, at install.
    pub(super) fn prepare_pipelines(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            self.pipelines = Some(Pipelines::new(device));
        }
        self.sorter.prepare_pooled(device);
        // A one-slot table until the first reserve: its pipelines are the
        // ones every pool builds (cached by source), so the build compiles here.
        if self.tiles.is_none() {
            match WorldTiles::new(device, 1, step_reach(), LIQUID_POINTS) {
                Ok(mut tiles) => {
                    tiles.prepare(device);
                    tiles.prepare_kept(device);
                    self.tiles = Some(tiles);
                }
                Err(error) => log::warn!("GPU FLIP Step: world tiles at install: {error}"),
            }
        }
        self.identity_ops.prepare(device);
        self.emit_scan.prepare(device);
        self.solver.prepare_pipelines(device);
        self.bodies.prepare_pipelines(device);
        // Sheet seeding prepares even while its rate is 0: switching it on mid-show
        // must not compile pipelines on the content thread.
        self.sheet_distance.prepare(device);
        self.sheeting.prepare(device);
        self.narrow_band.prepare(device);
    }

    /// The pool this tick runs on, starting at the domain's pool `start`
    /// and growing up to `ceiling`, from the last tick's and its table's late
    /// read (`tiles.rs` next_pool).
    pub(super) fn next_pool(&mut self, start: u32, ceiling: u32) -> u32 {
        let read = self.tiles.as_ref().and_then(WorldTiles::late_held);
        let next = next_pool(start, ceiling, self.pool, read);
        self.pool = Some(next);
        next.0
    }

    /// Size every array for a pool of `pool` tiles and `slots` before anything is encoded;
    /// removal reuses the sorted array. The emission scan is sized by the
    /// encode from the tick's sites. `narrow_band` is the run's band mode.
    pub(super) fn reserve(
        &mut self,
        device: &GpuDevice,
        pool: u32,
        slots: u64,
        sheeting: bool,
        narrow_band: u32,
    ) -> Result<(), String> {
        if self.zeros.is_none() {
            let zeros = device.try_create_buffer_shared(ZERO_BYTES)?;
            zeros.zero_fill();
            self.zeros = Some(zeros);
        }
        if self.slot_record.is_none() {
            self.slot_record = Some(allocate_zeroed(device, SLOT_RECORD_BYTES)?);
        }
        // One range per element of a pooled field: the sort is keyed on the
        // pool index of each cell.
        self.sorter.reserve_keys(device, pool_len(pool))?;
        // A box resized within its pool (a scale drag, mostly) keeps the
        // pooled arrays and the table; only a new pool rebuilds them.
        match self.lattice.as_ref() {
            Some(l) if l.pool_tiles == pool => {}
            _ => {
                // A new pool holds no tile yet: its table starts empty too.
                self.lattice = None;
                self.tiles = None;
                self.pool_canon_pending = true;
                self.solid_velocity_is_zero = false;
                self.lattice = Some(LatticeBuffers::new(device, pool)?);
            }
        }
        if self.tiles.as_ref().is_none_or(|t| t.pool_tiles() != pool) {
            self.tiles = None;
            let mut tiles = WorldTiles::new(device, pool, step_reach(), LIQUID_POINTS)?;
            tiles.prepare(device);
            tiles.prepare_kept(device);
            self.tiles = Some(tiles);
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
        self.narrow_band.reserve(device, pool, narrow_band)?;
        if sheeting {
            let slot_count = u32::try_from(slots)
                .map_err(|_| "sheeting particle capacity exceeds u32".to_string())?;
            self.sheet_distance.reserve(device, pool)?;
            self.sheeting.reserve(device, slot_count, pool)?;
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

/// Bytes of a plan the CPU reads: step dt, elapsed, remaining and the
/// fastest speed (gpu_flip_clock.wgsl Plan).
const PLAN_READBACK_BYTES: u64 = 16;
/// Plan readbacks in flight; a tick with none free skips its copy, so live
/// never waits on the GPU.
const PLAN_READBACKS: usize = 3;

/// One tick's last clock plan, copied for the CPU.
pub(crate) struct PlanReadback {
    buffer: GpuBuffer,
    stamp: u64,
    pending: bool,
}

impl StepState {
    /// The fastest speed of the newest tick whose frame has retired.
    pub(super) fn poll_plans(&mut self, clock: &manifold_gpu::FrameClock) -> Option<f32> {
        let mut newest: Option<(u64, f32)> = None;
        for slot in self.plan_readbacks.iter_mut().filter(|s| s.pending) {
            if !clock.is_complete(slot.stamp) {
                continue;
            }
            slot.pending = false;
            let Some(ptr) = slot.buffer.mapped_ptr() else {
                continue;
            };
            // SAFETY: the frame that wrote this shared slot has retired.
            let words = unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), 4) };
            if newest.is_none_or(|(stamp, _)| slot.stamp >= stamp) {
                newest = Some((slot.stamp, words[3]));
            }
        }
        if let Some((_, speed)) = newest {
            self.retired_speed = Some(speed);
        }
        self.retired_speed
    }

    /// Copy `plan`, this tick's last, for the CPU when a readback is free.
    pub(super) fn read_plan(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        plan: &GpuBuffer,
        clock: &manifold_gpu::FrameClock,
    ) {
        let index = match self.plan_readbacks.iter().position(|s| !s.pending) {
            Some(index) => index,
            None if self.plan_readbacks.len() < PLAN_READBACKS => {
                self.plan_readbacks.push(PlanReadback {
                    buffer: device.create_buffer_shared(PLAN_READBACK_BYTES),
                    stamp: 0,
                    pending: false,
                });
                self.plan_readbacks.len() - 1
            }
            None => return,
        };
        let slot = &mut self.plan_readbacks[index];
        enc.copy_buffer_range(plan, 0, &slot.buffer, 0, PLAN_READBACK_BYTES);
        slot.stamp = clock.stamp();
        slot.pending = true;
    }
}
