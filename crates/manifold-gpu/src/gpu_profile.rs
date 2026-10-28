//! What a profiled GPU frame reports: one span per dispatch or pass, and
//! how the encoder groups dispatches while profiling. Each backend fills
//! these from its own timestamp queries.

/// What kind of encoder a profiled span covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuWorkKind {
    Compute,
    Render,
    Blit,
    AccelerationStructure,
}

impl GpuWorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuWorkKind::Compute => "compute",
            GpuWorkKind::Render => "render",
            GpuWorkKind::Blit => "blit",
            GpuWorkKind::AccelerationStructure => "acceleration_structure",
        }
    }
}

/// One resolved span: a single compute dispatch, render pass, blit pass, or
/// acceleration structure pass.
#[derive(Clone, Debug)]
pub struct GpuProfiledSpan {
    /// Attribution tag set by the host via [`GpuEncoder::set_profile_tag`](crate::GpuEncoder::set_profile_tag)
    /// (e.g. the executor's step index). Empty if no tag was set.
    pub tag: String,
    /// The dispatch/pass debug label.
    pub label: String,
    pub kind: GpuWorkKind,
    /// The dispatched pipeline's static threadgroup (WGSL `var<workgroup>`)
    /// memory in bytes; 0 for every non-compute span.
    pub threadgroup_bytes: u32,
    /// GPU start time relative to the frame's first sample, milliseconds.
    pub start_ms: f64,
    /// GPU time spent in this span, milliseconds.
    pub millis: f64,
}

/// A whole profiled command buffer, resolved.
#[derive(Clone, Debug, Default)]
pub struct GpuFrameProfile {
    /// Whole-command-buffer GPU time (`GPUEndTime - GPUStartTime`), ms.
    pub total_ms: f64,
    pub spans: Vec<GpuProfiledSpan>,
    /// Dispatches that ran unprofiled because the sampler filled up. Above
    /// zero, span times are scaled wrong: the timed spans are stretched to
    /// the whole command buffer's time and the untimed tail reads as zero.
    pub overflow: usize,
    /// Spans whose samples resolved to `COUNTER_ERROR` (dropped).
    pub invalid: usize,
    pub failed_command_buffers: usize,
}

impl GpuFrameProfile {
    /// Sum of all resolved span times, ms. Work the spans don't cover
    /// (MPS/MetalFX internal encoders, overflow) shows up as
    /// `total_ms - attributed_ms`.
    pub fn attributed_ms(&self) -> f64 {
        self.spans.iter().map(|s| s.millis).sum()
    }
}

/// How finely a profiled frame is cut into spans. `Dispatch` opens a sampled
/// encoder per compute dispatch, so every dispatch is timed and the frame
/// loses all cross-dispatch overlap: the spans rank passes against each
/// other inside one encoder, never against the budget. `Tag` keeps one
/// sampled encoder open while the host's profile tag is unchanged, so the
/// dispatches of one node share an encoder exactly as a production frame
/// batches them: the spans are the plain frame's per-node time. A node still
/// yields several spans when a render, blit or acceleration pass, or a
/// replayed stretch, ends its encoder; the sum per tag is the node's time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileGranularity {
    Dispatch,
    Tag,
}

impl ProfileGranularity {
    /// Whether the next profiled dispatch, tagged `tag`, keeps the open
    /// sampled compute encoder (tagged `open`, `None` when no sampled
    /// encoder is open) instead of ending it and opening its own.
    pub fn reuses_open_encoder(self, open: Option<&str>, tag: &str) -> bool {
        self == ProfileGranularity::Tag && open == Some(tag)
    }
}
