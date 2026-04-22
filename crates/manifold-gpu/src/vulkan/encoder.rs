//! Vulkan `GpuEncoder` — wraps one `VkCommandBuffer` being recorded into.
//!
//! Phase 0 scaffold. Phase 1 brings up `VkCommandBuffer` allocation,
//! `dispatch_compute`, `draw_fullscreen`, render pass begin/end, pipeline
//! barriers, and `commit_and_wait_completed` (submit + fence wait).
//!
//! The public method surface will mirror `metal::encoder::GpuEncoder`.

/// Per-frame command encoder. Recorded commands go into the wrapped
/// `VkCommandBuffer`; `commit_and_wait_completed` submits + waits for
/// the fence, then the encoder is dropped.
pub struct GpuEncoder {
    pub(crate) _reserved: (),
}
