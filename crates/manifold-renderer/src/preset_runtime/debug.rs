//! Debug readback types for bisecting chain-internal black-frame bugs.
//!
//! Extracted from `core.rs` to stay under the godfile ceiling.

use super::{PresetIo, PresetRuntime};
use crate::node_graph::ResourceId;
use manifold_gpu::GpuTexture;

/// Debug readback: one fused step's output resource info.
pub struct StepDebugInfo<'a> {
    pub step_idx: usize,
    pub port_name: &'static str,
    pub resource_id: ResourceId,
    pub texture: Option<&'a GpuTexture>,
}

/// Debug readback: full chain intermediate texture state for bisecting
/// black-frame bugs. Source texture, every step's output texture, and
/// the output_slot texture.
pub struct ChainDebugInfo<'a> {
    pub source: Option<&'a GpuTexture>,
    pub output: Option<&'a GpuTexture>,
    pub step_outputs: Vec<StepDebugInfo<'a>>,
}

impl PresetRuntime {
    /// Debug readback: source texture, every step's output texture, and
    /// the output_slot texture. For bisecting black-frame bugs where the
    /// chain executes fully but presents zeros.
    pub fn chain_debug_info(&self) -> Option<ChainDebugInfo<'_>> {
        let PresetIo::Transform {
            source_slot,
            output_slot,
        } = self.io
        else {
            return None;
        };
        let backend = self.executor.backend();
        let source_tex = backend.texture_2d(source_slot);
        let output_tex = backend.texture_2d(output_slot);

        let mut step_outputs = Vec::new();
        for (idx, step) in self.plan.steps().iter().enumerate() {
            for (port_name, res_id) in &step.outputs {
                let tex = backend.slot_for(*res_id).and_then(|s| backend.texture_2d(s));
                step_outputs.push(StepDebugInfo {
                    step_idx: idx,
                    port_name,
                    resource_id: *res_id,
                    texture: tex,
                });
            }
        }

        Some(ChainDebugInfo {
            source: source_tex,
            output: output_tex,
            step_outputs,
        })
    }
}
