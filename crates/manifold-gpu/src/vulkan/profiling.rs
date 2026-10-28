//! Per-dispatch timestamps on query pools (D17, P2). Not built yet.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

#[derive(Clone)]
pub struct GpuTimestampSampler {
    _reserved: (),
}

impl GpuTimestampSampler {
    /// Maximum number of spans (encoder start/end pairs) one frame can record.
    pub fn max_spans(&self) -> usize {
        todo!("vulkan P1")
    }
}
