//! Inflow emission at empty half-cell sites.

use super::*;

/// Half-cell sites sources emit at: eight a cell (gpu_flip_step/emit.wgsl
/// emit_sites).
pub(crate) fn emit_sites(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| 2 * u64::from(n)).product()
}

/// New inflow, first transferred and advected by the following step: the
/// empty sites' flags, their scan, the births' identities, then the writes
/// and the tally.
pub(super) fn encode_emit(device: &GpuDevice, enc: &mut GpuEncoder, pipes: &Pipelines, sorter: &ParticleSorter, emit_scan: &mut PrefixScan, identity_ops: &manifold_water_liquid::primitives::particle_identity::ParticleIdentity, step: &Step<'_>, base: &StepParams, l: &LatticeBuffers, gate_plan: &GpuBuffer) -> Result<(), String> {
    let cells = l.cells;
    let sites = emit_sites(cells);
    let ranges = sorter.ranges().ok_or("the cell ranges were not reserved")?;
    let scan = emit_scan.buffer(device, sites as usize)?.clone();
    let emission = StepParams { capacity: base.particles, ..*base };
    enc.compute_memory_barrier_buffers();
    enc.dispatch_compute(&pipes.emit_flags, &[
        uniform(&emission), buffer(1, ranges), buffer(2, step.out),
        buffer(9, &l.corners), buffer(15, step.shapes), buffer(16, step.atlas),
        buffer(36, step.regions), buffer(46, step.clock_plan), buffer(37, &scan),
    ], groups(sites), "gpu_flip.step.emit_flags");
    enc.compute_memory_barrier_buffers();
    emit_scan.encode_labelled_gated(enc, sites as usize, ScanLabels {
        blocks: "gpu_flip.step.emit_scan.blocks", add: "gpu_flip.step.emit_scan.add",
    }, gate_plan);
    identity_ops.reserve(enc, manifold_water_liquid::primitives::particle_identity::BirthReservation {
        particles: step.out, identity: step.identity, ranges, scan: &scan, plan: step.clock_plan,
        params: [emission.capacity, cells.iter().product(), sites as u32, 1],
    });
    enc.dispatch_compute(&pipes.emit_write, &[
        uniform(&emission), buffer(1, ranges), buffer(15, step.shapes),
        buffer(16, step.atlas), buffer(36, step.regions), buffer(46, step.clock_plan),
        buffer(37, &scan), buffer(38, step.out), buffer(47, step.identity),
    ], groups(sites), "gpu_flip.step.emit_write");
    enc.dispatch_compute(&pipes.emit_tally, &[
        uniform(&emission), buffer(1, ranges), buffer(37, &scan), buffer(46, step.clock_plan),
        buffer(47, step.identity),
        GpuBinding::Buffer { binding: 22, buffer: step.capped, offset: step.tally },
    ], [1, 1, 1], "gpu_flip.step.emit_tally");
    Ok(())
}
