//! Face extension into air, dense or by the tile lists.

use super::*;

/// `layers` extension passes from `source` into `target`, ping-ponging
/// through `scratch` so the last pass lands in `target`. `source` may be
/// `target`: an even count's first pass writes `scratch`, an odd one starts from a copy there.
pub(super) fn extend(
    enc: &mut GpuEncoder,
    pipes: &Pipelines,
    clock_plan: &GpuBuffer,
    params: &StepParams,
    face_groups: [u32; 3],
    table: Option<&TileTable>,
    [source, target, scratch]: [&GpuBuffer; 3],
    layers: u32,
    label: &str,
) {
    let mut from = source;
    // In place with an odd count, the first pass would write its own source:
    // start from a copy in `scratch` instead, which that pass does not write.
    if !layers.is_multiple_of(2) && std::ptr::eq(source, target) {
        enc.copy_buffer_to_buffer(source, scratch, source.size.min(scratch.size));
        from = scratch;
    }
    let gate = table.filter(|_| params.step_in_tick > 0 && indirect_extend());
    if let Some(table) = gate {
        debug_assert_eq!(params.extension_layers, layers);
        enc.begin_gated_segment(&table.args, extension_range_index(table.ring_max), layers);
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
        ];
        if let Some(table) = gate {
            // The first slot is normally active and keeps ordinary replay.
            // Later slots use the same full grid when active, zero groups
            // otherwise; tiles_lists already writes that decision each slot.
            enc.dispatch_compute_gated(
                &pipes.extend,
                &bindings,
                face_groups,
                &table.args,
                face_args_offset(table.ring_max),
                label,
            );
        } else {
            enc.dispatch_compute(&pipes.extend, &bindings, face_groups, label);
        }
        from = to;
    }
    if gate.is_some() {
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
