//! Face extension into air over the held slots.

use super::*;
use manifold_water_liquid::tile_storage::TILE_CELLS;
use manifold_water_liquid::world_tiles::{allocated_args_offset, replay_range_index};

/// Gated chain `chain`'s extension passes ([`replay_layers`]) from `source`
/// into `target`, ping-ponging through `scratch` so the last pass lands in
/// `target`. `source` may be `target`: an even count's first pass writes
/// `scratch`, an odd one starts from a copy there.
#[allow(
    clippy::too_many_arguments,
    reason = "one extension chain: the clock, the step, the tiles, its three buffers and its chain"
)]
pub(super) fn extend(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    clock_plan: &GpuBuffer,
    params: &StepParams,
    tiles: &WorldTiles,
    [source, target, scratch]: [&GpuBuffer; 3],
    chain: usize,
    label: &str,
) {
    let layers = replay_layers(params)[chain];
    let mut from = source;
    // In place with an odd count, the first pass would write its own source:
    // start from a copy in `scratch` instead, which that pass does not write.
    if !layers.is_multiple_of(2) && std::ptr::eq(source, target) {
        enc.copy_buffer_to_buffer(source, scratch, source.size.min(scratch.size));
        from = scratch;
    }
    // The replayed dispatch covers every pool slot and its threads past the
    // held list return; the direct one reads the list's own triple.
    let every_slot = groups(u64::from(tiles.pool_tiles()) * u64::from(TILE_CELLS));
    let ring = tiles.reach().ring_max;
    let allocated = allocated_args_offset(ring);
    let gate = params.step_in_tick > 0 && indirect_extend();
    if gate {
        enc.begin_gated_segment(tiles.args(), replay_range_index(ring, chain), layers);
    }
    for i in 0..layers {
        let to = if (layers - i) % 2 == 1 {
            target
        } else {
            scratch
        };
        let bindings = [
            buffer(46, clock_plan),
            uniform(params),
            buffer(3, from),
            buffer(4, to),
            buffer(29, &tiles.lists().by_rank),
            buffer(30, tiles.counts()),
            buffer(51, tiles.table()),
        ];
        if gate {
            // The first slot is normally active and keeps ordinary replay.
            // Later slots run every slot's groups when active, zero groups
            // otherwise; the build's finish writes that decision each slot.
            enc.dispatch_compute_gated(
                &pipes.extend,
                &bindings,
                every_slot,
                tiles.args(),
                allocated,
                label,
            );
        } else {
            enc.dispatch_compute_indirect(&pipes.extend, &bindings, tiles.args(), allocated, label);
        }
        from = to;
    }
    if gate {
        enc.end_gated_segments();
    }
}

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub(super) static FORCE_DENSE_EXTEND: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub fn set_force_dense_extend(on: bool) {
    FORCE_DENSE_EXTEND.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub(super) fn indirect_extend() -> bool {
    #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
    if FORCE_DENSE_EXTEND.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    true
}
