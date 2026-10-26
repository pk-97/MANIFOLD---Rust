//! Shared schedule ABI for the liquid lattice atoms. Values remain dense for
//! random-access gradients; only occupied 8³ bricks run expensive bodies.
//! An exterior pass writes every inactive slot, including retired bricks.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

pub const COMMON: &str = include_str!("shaders/liquid_bricks_common.wgsl");
pub(crate) const WIDTH: u32 = 8;
pub(crate) const HEADER: u64 = 8;
pub(crate) const GRID_OFFSET: u64 = 4;

/// One mask word and one possible compact-list entry per brick.
pub(crate) fn schedule_words(nodes: [u32; 3]) -> Option<u64> {
    let bricks = nodes.into_iter().try_fold(1u64, |n, axis| {
        n.checked_mul(u64::from(axis.div_ceil(WIDTH)))
    })?;
    HEADER.checked_add(bricks.checked_mul(2)?)
}

pub fn valid_schedule(bricks: &GpuBuffer, nodes: [u32; 3]) -> bool {
    schedule_words(nodes).and_then(|n| n.checked_mul(4)) == Some(bricks.size)
}

/// Pass 0: dense graph; 2: canonical exterior; 1: compact active list.
pub fn dispatch(
    encoder: &mut manifold_gpu::GpuEncoder,
    pipeline: &GpuComputePipeline,
    bindings: &[GpuBinding<'_>],
    bricks: Option<&GpuBuffer>,
    pass: u32,
    count: u32,
    label: &str,
) {
    if pass == 1 {
        encoder.dispatch_compute_indirect(
            pipeline,
            bindings,
            bricks.expect("active brick pass"),
            GRID_OFFSET,
            label,
        );
    } else {
        encoder.dispatch_compute(pipeline, bindings, [count.div_ceil(256), 1, 1], label);
    }
}

#[cfg(test)]
mod tests;
