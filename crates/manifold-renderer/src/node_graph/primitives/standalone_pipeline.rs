//! The single pipeline-get for the standalone codegen path (BUG-elb0).
//!
//! Every barrier-free per-element atom builds its runtime kernel from its
//! `wgsl_body` spec via `standalone_for_spec` — never from hand-authored
//! WGSL. This helper concentrates that rule in one enforcement point: the
//! 150+ call sites it replaces each re-stated it in a comment.

use manifold_gpu::{GpuComputePipeline, GpuDevice};

use crate::node_graph::freeze::codegen;
use crate::node_graph::primitive::Primitive;

/// Get-or-create the primitive's standalone codegen compute pipeline,
/// labelled with the primitive's TYPE_ID. Replaces the per-file
/// `pipeline.get_or_insert_with(|| device.create_compute_pipeline(
/// &standalone_for_spec::<Self>().expect(...), ENTRY, "<type_id>"))`
/// closure — the only thing those closures ever varied was the label.
pub fn standalone_pipeline<'a, P: Primitive>(
    slot: &'a mut Option<GpuComputePipeline>,
    device: &GpuDevice,
) -> &'a mut GpuComputePipeline {
    slot.get_or_insert_with(|| {
        device.create_compute_pipeline(
            &codegen::standalone_for_spec::<P>()
                .unwrap_or_else(|e| panic!("{} standalone codegen: {e:?}", P::TYPE_ID)),
            codegen::ENTRY,
            P::TYPE_ID,
        )
    })
}
