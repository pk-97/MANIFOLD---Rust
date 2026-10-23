//! Sheet seeding inside the step (fluidsimulation.cpp _updateSheetSeeding); the stage itself is [`super::super::gpu_flip_sheeting`].

use super::*;

impl StepState {
    /// Sheet seeding, after the engine's own marker update for this substep
    /// and before the FLIP update: births join the move, so they take this
    /// substep's velocity change and advection. Off (rate 0), the caller
    /// encodes nothing.
    pub(super) fn encode_sheeting(&self, enc: &mut GpuEncoder, step: &Step<'_>, base: &StepParams, l: &LatticeBuffers, sorted: &GpuBuffer, sheet_order: Option<&GpuBuffer>) -> Result<(), String> {
        let p = step.params;
        let capacity = p.capacity;
        let cells = l.cells;
        enc.set_profile_tag("gpu_flip.stage.sheeting");
        let ranges = self.sorter.ranges().ok_or("the cell ranges were not reserved")?;
        let order = sheet_order.ok_or("sheet seeding storage was not reserved")?;
        // The engine's surface distance (its curvature grid's), from the
        // liquid level set the pressure solve read.
        let phi = self.surface.encode_gated(enc, &l.phi, p.cell_size, step.clock_plan);
        enc.compute_memory_barrier_buffers();
        let inputs = super::super::gpu_flip_sheeting::SheetInputs {
            particles: sorted,
            order,
            ranges,
            count: capacity,
            phi,
            origin: p.box_min,
            h: p.cell_size,
            threshold: super::super::gpu_flip_sheeting::FILL_THRESHOLD,
        };
        let births = super::super::gpu_flip_sheeting::StepBirths {
            old: &l.a,
            identity: step.identity,
            slots: base.particles,
            rate: step.sheet_rate,
            tick: p.tick_index.max(0) as u32,
            substep: p.step_in_tick.max(0) as u32,
        };
        self.sheeting.encode_draw(enc, &inputs, step.clock_plan, &births);
        self.identity_ops.reserve(enc, manifold_water_liquid::primitives::particle_identity::BirthReservation {
            particles: sorted, identity: step.identity, ranges, scan: self.sheeting.winners(), plan: step.clock_plan,
            params: [base.particles, cells.iter().product(), self.sheeting.ranks(), 1],
        });
        enc.compute_memory_barrier_buffers();
        self.sheeting.encode_write(enc, &inputs, step.clock_plan, &births);
        Ok(())
    }
}
