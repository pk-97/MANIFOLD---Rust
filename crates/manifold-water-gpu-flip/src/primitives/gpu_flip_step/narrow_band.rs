//! Narrow Band FLIP's per-step parameters, redistancing and history (Ferstl et al. 2016).

use super::*;

#[derive(Clone, Copy)]
pub(super) struct NarrowHistory {
    epoch: Option<u32>,
    box_min: [f32; 3],
    cell_size: f32,
    cells: [u32; 3],
    slots: u32,
    enabled: bool,
    last_tick: i32,
}

pub(super) fn narrow_uniform(params: &NbParams) -> GpuBinding<'_> {
    GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::bytes_of(params),
    }
}

pub(super) fn narrow_params(params: &StepParams, initialized: bool) -> NbParams {
    NbParams {
        n: params.n,
        slots: params.capacity,
        minimum: params.box_min,
        h: params.cell_size,
        dt: params.step_dt,
        axis: 0,
        initialized: u32::from(initialized),
        closed_faces: params.closed_faces,
    }
}

pub(super) fn narrow_redistance(enc: &mut GpuEncoder, pipes: &BandPipelines, params: &NbParams, source: &GpuBuffer, output: &GpuBuffer, cells: [u32; 3], label: &str) {
    enc.dispatch_compute(
        &pipes.seed,
        &[narrow_uniform(params), buffer(1, source), buffer(2, output)],
        groups(u64::from(cells[0]) * u64::from(cells[1]) * u64::from(cells[2])),
        label,
    );
    enc.compute_memory_barrier_buffers();
    for axis in 0..3u32 {
        let mut pass = *params;
        pass.axis = axis;
        let lines = match axis {
            0 => u64::from(cells[1]) * u64::from(cells[2]),
            1 => u64::from(cells[0]) * u64::from(cells[2]),
            _ => u64::from(cells[0]) * u64::from(cells[1]),
        };
        enc.dispatch_compute(&pipes.sweep, &[narrow_uniform(&pass), buffer(2, output)], groups(lines), label);
        enc.compute_memory_barrier_buffers();
    }
}

impl StepState {
    /// Update the narrow-band identity before a step.  Geometry, capacity,
    /// epoch and an unwired epoch's tick rollback all invalidate its distance
    /// history.  A plain enable toggle is handled separately so disabling can
    /// restore the held interior before dense FLIP resumes.
    pub(super) fn sync_narrow_history(&mut self, epoch: Option<u32>, box_min: [f32; 3], cell_size: f32, cells: [u32; 3], slots: u32, tick: i32, enabled: bool) -> bool {
        let previous = self.narrow_history;
        let identity_changed = previous.is_none_or(|history| {
            history.epoch != epoch
                || history.box_min.map(f32::to_bits) != box_min.map(f32::to_bits)
                || history.cell_size.to_bits() != cell_size.to_bits()
                || history.cells != cells
                || history.slots != slots
                || tick < history.last_tick
        });
        if identity_changed {
            self.narrow.initialized = false;
            self.narrow_reset_pending = true;
            self.narrow_full_count = false;
        }
        let restore = !enabled && previous.is_some_and(|history| history.enabled) && !identity_changed;
        if !enabled && previous.is_some_and(|history| history.enabled) {
            self.narrow.initialized = false;
        }
        self.narrow_history = Some(NarrowHistory {
            epoch,
            box_min,
            cell_size,
            cells,
            slots,
            enabled,
            last_tick: tick,
        });
        restore
    }
}
