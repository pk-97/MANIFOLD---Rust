//! Sealed pockets: seeds, rounds, sums and the coarse levels.

use super::*;

/// The coarse pockets' bytes: a state and a label per level-1 cell (every
/// deeper level has fewer). A lattice with no level 1 has no solve level
/// above 0 and never reads them.
pub(super) fn pocket_coarse_bytes(cells: [u32; 3]) -> u64 {
    level_lattices(cells)
        .get(1)
        .map_or(0, |&coarse| 2 * cell_bytes(coarse))
}

/// Three words a cell (a pocket's 64-bit sum and its count, indexed by its
/// leader cell's box index), then the removed total's two. A dense array,
/// not a pooled field.
pub(super) fn pocket_sum_bytes(cells: [u32; 3]) -> u64 {
    3 * cell_bytes(cells) + 8
}

/// Words of the pocket spread's gate (gpu_flip_step/pockets.wgsl `pocket_gate`).
pub(crate) const POCKET_GATE_WORDS: u64 = 17;

/// The aligned replay range at gate words 12 and 13, after the flags.
pub(super) const POCKET_RANGE_INDEX: u32 = 6;

pub(super) const POCKET_ROUND_OFFSET: u64 = 14 * 4;

/// Rounds of the sealed-pocket spread: one round of line sweeps crosses the
/// lattice along each axis, so the cap is the longest side.
pub(crate) fn pocket_rounds(cells: [u32; 3]) -> u32 {
    cells.into_iter().max().unwrap_or(1)
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub(super) static FORCE_INDIRECT_POCKETS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_force_indirect_pockets(on: bool) {
    FORCE_INDIRECT_POCKETS.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub(super) fn segment_pockets() -> bool {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    if FORCE_INDIRECT_POCKETS.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    true
}

/// Which water reaches air (gpu_flip_step/pockets.wgsl pocket_*): seed, up to
/// [`pocket_rounds`] rounds of indirect sweeps that stop once a round changes
/// nothing, the unfinished check, then the tally.
pub(super) fn encode_pockets(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    table: &GpuBuffer,
    ranges: &GpuBuffer,
    cells: [u32; 3],
    capped: &GpuBuffer,
    tally: u64,
    plan: &GpuBuffer,
) {
    let cell_count: u64 = cells.iter().map(|&n| u64::from(n)).product();
    enc.dispatch_compute(
        &pipes.pocket_seed,
        &[
            uniform(params),
            buffer(6, &l.water),
            buffer(10, &l.s),
            buffer(23, &l.pocket),
            buffer(25, &l.pocket_label),
            buffer(46, plan),
            buffer(51, table),
        ],
        groups(cell_count),
        "gpu_flip.step.pocket_seed",
    );
    enc.dispatch_compute(
        &pipes.pocket_start,
        &[buffer(24, &l.pocket_gate), buffer(46, plan)],
        [1, 1, 1],
        "gpu_flip.step.pocket_start",
    );
    let segmented = segment_pockets();
    let lines = [
        u64::from(cells[1]) * u64::from(cells[2]),
        u64::from(cells[2]) * u64::from(cells[0]),
        u64::from(cells[0]) * u64::from(cells[1]),
    ];
    let round_bindings = [
        uniform(params),
        buffer(24, &l.pocket_gate),
        buffer(46, plan),
    ];
    if segmented {
        enc.dispatch_compute(
            &pipes.pocket_round,
            &round_bindings,
            [1, 1, 1],
            "gpu_flip.step.pocket_round",
        );
    }
    for _ in 0..pocket_rounds(cells) {
        if segmented {
            // The last command prepares the next round. Once no cell changes,
            // subsequent segments skip their sweeps and their setup together.
            // The final unused setup only updates dispatch metadata; the cap
            // check below inspects the cells and labels independently.
            enc.begin_gated_segment(&l.pocket_gate, POCKET_RANGE_INDEX, 4);
        } else {
            enc.dispatch_compute(
                &pipes.pocket_round,
                &round_bindings,
                [1, 1, 1],
                "gpu_flip.step.pocket_round",
            );
        }
        for (axis, sweep) in pipes.pocket_sweep.iter().enumerate() {
            let bindings = [
                uniform(params),
                buffer(10, &l.s),
                buffer(23, &l.pocket),
                buffer(24, &l.pocket_gate),
                buffer(25, &l.pocket_label),
                buffer(51, table),
            ];
            if segmented {
                enc.dispatch_compute_gated(
                    sweep,
                    &bindings,
                    groups(lines[axis]),
                    &l.pocket_gate,
                    12 * axis as u64,
                    "gpu_flip.step.pocket_sweep",
                );
            } else {
                enc.dispatch_compute_indirect(
                    sweep,
                    &bindings,
                    &l.pocket_gate,
                    12 * axis as u64,
                    "gpu_flip.step.pocket_sweep",
                );
            }
        }
        if segmented {
            enc.dispatch_compute_gated(
                &pipes.pocket_round,
                &round_bindings,
                [1, 1, 1],
                &l.pocket_gate,
                POCKET_ROUND_OFFSET,
                "gpu_flip.step.pocket_round",
            );
            enc.end_gated_segments();
        }
    }
    enc.dispatch_compute(
        &pipes.pocket_check,
        &[
            uniform(params),
            buffer(10, &l.s),
            buffer(23, &l.pocket),
            buffer(24, &l.pocket_gate),
            buffer(25, &l.pocket_label),
            buffer(46, plan),
            buffer(51, table),
        ],
        groups(cell_count),
        "gpu_flip.step.pocket_check",
    );
    // Adds the step's unfinished spread to the solver word, cleared on the
    // tick's first step, and writes the step's dry, sealed and air counts.
    enc.dispatch_compute(
        &pipes.pocket_tally,
        &[
            uniform(params),
            buffer(1, ranges),
            buffer(6, &l.water),
            buffer(7, &l.phi),
            buffer(10, &l.s),
            buffer(23, &l.pocket),
            buffer(24, &l.pocket_gate),
            GpuBinding::Buffer {
                binding: 22,
                buffer: capped,
                offset: tally,
            },
            buffer(46, plan),
            buffer(51, table),
        ],
        [1, 1, 1],
        "gpu_flip.step.pocket_tally",
    );
}

/// The pockets one lattice's right-hand side is corrected over: each
/// cell's sealed state and its pocket's label (a cell of that lattice), and
/// the sums, three words a cell plus the removed total's two.
pub(super) struct Pockets<'a> {
    /// The fine lattice's pockets are pooled fields; a coarse level's are
    /// dense boxes.
    pub(super) fine: bool,
    pub(super) state: &'a GpuBuffer,
    pub(super) label: &'a GpuBuffer,
    pub(super) sums: &'a GpuBuffer,
}

impl LatticeBuffers {
    pub(super) fn fine_pockets(&self) -> Pockets<'_> {
        Pockets {
            fine: true,
            state: &self.pocket,
            label: &self.pocket_label,
            sums: &self.pocket_sum,
        }
    }

    pub(super) fn coarse_pockets(&self) -> Pockets<'_> {
        Pockets {
            fine: false,
            state: &self.pocket_coarse,
            label: &self.pocket_coarse_label,
            sums: &self.pocket_sum,
        }
    }
}

/// Each sealed pocket's mean taken off the right-hand side `rhs` on the
/// lattice `params.n` (gpu_flip_step/pockets.wgsl pocket_accumulate,
/// pocket_remove): with no air cell its solve is pure Neumann, solvable only
/// for a right-hand side summing to 0. What was removed goes to solver word
/// 4 (`solve` 0, pressure) or 5 (1, density): h³ of the lattice's cell
/// size, so a coarse level adds its own volume rate.
pub(super) fn encode_pocket_mean(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    pockets: Pockets<'_>,
    table: &GpuBuffer,
    rhs: &GpuBuffer,
    solve: usize,
    capped: &GpuBuffer,
    tally: u64,
    plan: &GpuBuffer,
) {
    let which = usize::from(!pockets.fine);
    let cell_count: u64 = params.n.iter().map(|&n| u64::from(n)).product();
    let sums = [uniform(params), buffer(26, pockets.sums), buffer(46, plan)];
    enc.dispatch_compute(
        &pipes.pocket_clear,
        &sums,
        groups(3 * cell_count + 2),
        "gpu_flip.step.pocket_clear",
    );
    let cells = [
        uniform(params),
        buffer(5, rhs),
        buffer(23, pockets.state),
        buffer(25, pockets.label),
        buffer(26, pockets.sums),
        buffer(46, plan),
        buffer(51, table),
    ];
    enc.dispatch_compute(
        &pipes.pocket_accumulate[which],
        &cells,
        groups(cell_count),
        "gpu_flip.step.pocket_accumulate",
    );
    enc.dispatch_compute(
        &pipes.pocket_remove[which],
        &cells,
        groups(cell_count),
        "gpu_flip.step.pocket_remove",
    );
    enc.dispatch_compute(
        &pipes.pocket_flux[solve],
        &[
            uniform(params),
            buffer(26, pockets.sums),
            GpuBinding::Buffer {
                binding: 22,
                buffer: capped,
                offset: tally,
            },
            buffer(46, plan),
        ],
        [1, 1, 1],
        "gpu_flip.step.pocket_flux",
    );
}

/// The pockets at solve level `params.solve_level` from the fine ones
/// (gpu_flip_step/pockets.wgsl pocket_coarsen): a level cell is sealed when every
/// fine cell under it is sealed with one label, and takes the lowest such
/// level cell of its pocket as its label.
pub(super) fn encode_pocket_coarsen(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    table: &GpuBuffer,
    plan: &GpuBuffer,
) {
    let level = params.solve_level as usize;
    let coarse_count: u64 = level_lattices(l.cells)[level]
        .iter()
        .map(|&n| u64::from(n))
        .product();
    let cell_count: u64 = l.cells.iter().map(|&n| u64::from(n)).product();
    enc.dispatch_compute(
        &pipes.pocket_coarse[0],
        &[
            uniform(params),
            buffer(41, &l.pocket_leader),
            buffer(46, plan),
            buffer(51, table),
        ],
        groups(cell_count),
        "gpu_flip.step.pocket_leader_clear",
    );
    let bindings = [
        uniform(params),
        buffer(6, &l.water),
        buffer(10, &l.s),
        buffer(23, &l.pocket),
        buffer(25, &l.pocket_label),
        buffer(39, &l.pocket_coarse),
        buffer(40, &l.pocket_coarse_label),
        buffer(41, &l.pocket_leader),
        buffer(46, plan),
        buffer(51, table),
    ];
    enc.dispatch_compute(
        &pipes.pocket_coarse[1],
        &bindings,
        groups(coarse_count),
        "gpu_flip.step.pocket_coarsen",
    );
    enc.dispatch_compute(
        &pipes.pocket_coarse[2],
        &bindings,
        groups(coarse_count),
        "gpu_flip.step.pocket_relabel",
    );
}

/// I8's guard: each line of `source` that uses an atomic, outside the named
/// functions and variable declarations of `allowed`; exchange and
/// compare-exchange are never allowed. Comments are skipped.
#[cfg(test)]
pub(crate) fn atomic_sites_outside(source: &str, allowed: &[&str]) -> Vec<String> {
    let mut owner = "";
    let mut stray = Vec::new();
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("fn ") {
            owner = rest.split('(').next().unwrap_or("");
        } else if let Some(at) = line.find("var<").filter(|_| !line.starts_with(' ')) {
            owner = line[at..]
                .split_once("> ")
                .map_or("", |(_, rest)| rest.split(':').next().unwrap_or("").trim());
        }
        let code = line.split("//").next().unwrap_or("");
        if !code.contains("atomic") {
            continue;
        }
        if code.contains("atomicExchange")
            || code.contains("atomicCompareExchange")
            || !allowed.contains(&owner)
        {
            stray.push(format!("{owner}: {}", line.trim()));
        }
    }
    stray
}
