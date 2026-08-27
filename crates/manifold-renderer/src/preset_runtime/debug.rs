//! Debug readback types for bisecting chain-internal black-frame bugs.
//!
//! Extracted from `core.rs` to stay under the godfile ceiling.

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
