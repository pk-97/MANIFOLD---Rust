//! Sealed pockets: seeds, rounds, sums and the coarse levels, over world
//! tiles. The cell passes run over the table's lists (512 threads a slot) and
//! the line sweeps start in every held slot with no held neighbour on the low
//! side of the axis.

use super::*;
use manifold_node_engine::scene::world_tiles::{TILE, pool_len};
use manifold_water_liquid::world_tiles::{WorldTiles, allocated_args_offset, list_args_offset};

/// The coarse pockets' bytes: a state and a label (4 bytes each) per cell of
/// the level-1 pool of `level_tiles` slots (every deeper level has fewer). A
/// lattice with no level 1 has no solve level above 0 and never reads them.
pub(super) fn pocket_coarse_bytes(level_tiles: u32) -> u64 {
    2 * 4 * pool_len(level_tiles)
}

/// The removed total's two words, then three words a cell (a pocket's 64-bit
/// sum and its count, indexed by its leader cell's pool index). A dense array
/// over the pool's cells, not a pooled field. It must start at zero: the
/// removed total is zeroed again by each solve's flux, not cleared before it.
pub(super) fn pocket_sum_bytes(pool_tiles: u32) -> u64 {
    4 * (2 + 3 * pool_len(pool_tiles))
}

/// Words of the pocket spread's gate (gpu_flip_step/pockets.wgsl `pocket_gate`).
pub(crate) const POCKET_GATE_WORDS: u64 = 17;

/// The aligned replay range at gate words 12 and 13, after the flags.
pub(super) const POCKET_RANGE_INDEX: u32 = 6;

pub(super) const POCKET_ROUND_OFFSET: u64 = 14 * 4;

/// Line sweeps a held slot starts along one axis: one per cell of a tile face.
const LINES_PER_TILE: u64 = (TILE * TILE) as u64;

/// Rounds of the sealed-pocket spread: one round of line sweeps crosses
/// every run of held tiles along each axis, so the cap is the longest side,
/// in cells, of the box the pool is sized for.
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

/// Which water reaches air (gpu_flip_step/pockets.wgsl pocket_*), over the
/// fine table `tiles`: seed every held cell, up to [`pocket_rounds`] rounds of
/// line sweeps that stop once a round changes nothing, the unfinished check
/// over the cell set C, then the tally over the held cells.
pub(super) fn encode_pockets(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    tiles: &WorldTiles,
    ranges: &GpuBuffer,
    capped: &GpuBuffer,
    tally: u64,
    plan: &GpuBuffer,
) {
    let held = allocated_args_offset(tiles.reach().ring_max);
    let list = buffer(29, &tiles.lists().by_rank);
    let table = buffer(51, tiles.table());
    enc.dispatch_compute_indirect(
        &pipes.pocket_seed,
        &[
            uniform(params),
            buffer(6, &l.water),
            buffer(10, &l.s),
            buffer(23, &l.pocket),
            buffer(25, &l.pocket_label),
            list,
            buffer(46, plan),
            table,
        ],
        tiles.args(),
        held,
        "gpu_flip.step.pocket_seed",
    );
    enc.dispatch_compute(
        &pipes.pocket_start,
        &[
            uniform(params),
            buffer(24, &l.pocket_gate),
            buffer(31, tiles.args()),
            buffer(46, plan),
        ],
        [1, 1, 1],
        "gpu_flip.step.pocket_start",
    );
    let segmented = segment_pockets();
    // The most threads the sweeps can need: every pool slot held. The gate's
    // sizes (and the sweeps' own bound) cut it to the held slots.
    let sweep_groups = groups(u64::from(tiles.pool_tiles()) * LINES_PER_TILE);
    let round_bindings = [buffer(24, &l.pocket_gate), buffer(46, plan)];
    if segmented {
        enc.dispatch_compute(
            &pipes.pocket_round,
            &round_bindings,
            [1, 1, 1],
            "gpu_flip.step.pocket_round",
        );
    }
    for _ in 0..pocket_rounds(l.cells) {
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
                list,
                table,
            ];
            if segmented {
                enc.dispatch_compute_gated(
                    sweep,
                    &bindings,
                    sweep_groups,
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
    enc.dispatch_compute_indirect(
        &pipes.pocket_check,
        &[
            uniform(params),
            buffer(10, &l.s),
            buffer(23, &l.pocket),
            buffer(24, &l.pocket_gate),
            buffer(25, &l.pocket_label),
            list,
            buffer(46, plan),
            table,
        ],
        tiles.args(),
        list_args_offset(0),
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
            list,
            buffer(46, plan),
            table,
        ],
        [1, 1, 1],
        "gpu_flip.step.pocket_tally",
    );
}

/// The pockets one lattice's right-hand side is corrected over: each
/// cell's sealed state and its pocket's label (a pool index of that lattice's
/// pool), and the sums, three words a cell plus the removed total's two.
pub(super) struct Pockets<'a> {
    pub(super) state: &'a GpuBuffer,
    pub(super) label: &'a GpuBuffer,
    pub(super) sums: &'a GpuBuffer,
}

impl LatticeBuffers {
    pub(super) fn fine_pockets(&self) -> Pockets<'_> {
        Pockets {
            state: &self.pocket,
            label: &self.pocket_label,
            sums: &self.pocket_sum,
        }
    }

    pub(super) fn coarse_pockets(&self) -> Pockets<'_> {
        Pockets {
            state: &self.pocket_coarse,
            label: &self.pocket_coarse_label,
            sums: &self.pocket_sum,
        }
    }
}

/// Each sealed pocket's mean taken off the right-hand side `rhs` of the
/// lattice `tiles` holds (gpu_flip_step/pockets.wgsl pocket_accumulate,
/// pocket_remove): with no air cell its solve is pure Neumann, solvable only
/// for a right-hand side summing to 0. What was removed goes to solver word
/// 4 (`solve` 0, pressure) or 5 (1, density): h³ of the lattice's cell
/// size (`params.cell_size`), so a coarse level adds its own volume rate.
/// The passes run over `tiles`' cell set C: the fine table's, or a coarse
/// level's own (every slot of it); they read no table, only the list.
pub(super) fn encode_pocket_mean(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    pockets: Pockets<'_>,
    tiles: &WorldTiles,
    rhs: &GpuBuffer,
    solve: usize,
    capped: &GpuBuffer,
    tally: u64,
    plan: &GpuBuffer,
) {
    let list = buffer(29, &tiles.lists().by_rank);
    enc.dispatch_compute_indirect(
        &pipes.pocket_clear,
        &[buffer(26, pockets.sums), list, buffer(46, plan)],
        tiles.args(),
        list_args_offset(0),
        "gpu_flip.step.pocket_clear",
    );
    let cells = [
        buffer(5, rhs),
        buffer(23, pockets.state),
        buffer(25, pockets.label),
        buffer(26, pockets.sums),
        list,
        buffer(46, plan),
    ];
    enc.dispatch_compute_indirect(
        &pipes.pocket_accumulate,
        &cells,
        tiles.args(),
        list_args_offset(0),
        "gpu_flip.step.pocket_accumulate",
    );
    enc.dispatch_compute_indirect(
        &pipes.pocket_remove,
        &cells,
        tiles.args(),
        list_args_offset(0),
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

/// The coarse level's table as the coarsening reads it: its H, to find its
/// slot rows (gpu_flip_step/pockets.wgsl `PocketLevel`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PocketLevel {
    entries: u32,
    _pad: [u32; 3],
}

/// The pockets at solve level `params.solve_level` from the fine ones
/// (gpu_flip_step/pockets.wgsl pocket_coarsen): a level cell is sealed when
/// every fine cell under it is sealed with one label, and takes the lowest
/// such level cell of its pocket as its label. `fine` is the step's table
/// and `level` the table of the solve level, whose slots the level's passes
/// run over.
pub(super) fn encode_pocket_coarsen(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    params: &StepParams,
    l: &LatticeBuffers,
    fine: &WorldTiles,
    level: &WorldTiles,
    plan: &GpuBuffer,
) {
    enc.dispatch_compute_indirect(
        &pipes.pocket_coarse[0],
        &[
            buffer(41, &l.pocket_leader),
            buffer(29, &fine.lists().by_rank),
            buffer(46, plan),
        ],
        fine.args(),
        list_args_offset(0),
        "gpu_flip.step.pocket_leader_clear",
    );
    let entries = PocketLevel {
        entries: level.table_entries(),
        _pad: [0; 3],
    };
    let bindings = [
        uniform(params),
        buffer(6, &l.water),
        buffer(10, &l.s),
        buffer(23, &l.pocket),
        buffer(25, &l.pocket_label),
        buffer(39, &l.pocket_coarse),
        buffer(40, &l.pocket_coarse_label),
        buffer(41, &l.pocket_leader),
        buffer(29, &level.lists().by_rank),
        buffer(46, plan),
        buffer(51, fine.table()),
        GpuBinding::Bytes {
            binding: 63,
            data: bytemuck::bytes_of(&entries),
        },
        buffer(64, level.table()),
    ];
    enc.dispatch_compute_indirect(
        &pipes.pocket_coarse[1],
        &bindings,
        level.args(),
        list_args_offset(0),
        "gpu_flip.step.pocket_coarsen",
    );
    enc.dispatch_compute_indirect(
        &pipes.pocket_coarse[2],
        &bindings,
        level.args(),
        list_args_offset(0),
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
