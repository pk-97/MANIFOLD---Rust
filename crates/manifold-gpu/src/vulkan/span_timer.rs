//! Per-span GPU time, until the GPU timing consolidation replaces it.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use super::*;

#[derive(Default)]
pub struct GpuSpanTimer {
    _reserved: (),
}

impl GpuSpanTimer {
    /// Start span `key` of `frame` in the encoder's current command buffer.
    pub fn begin(&mut self, encoder: &mut GpuEncoder, key: u64, frame: u64) {
        todo!("vulkan P1")
    }

    /// End the open span in the encoder's current command buffer, which the
    /// caller commits before encoding anything that is not the span's.
    pub fn end(&mut self, encoder: &mut GpuEncoder) {
        todo!("vulkan P1")
    }

    /// Hand `each` the key, frame and longest buffer's GPU seconds of every
    /// finished span, oldest first.
    pub fn poll(&mut self, each: impl FnMut(u64, u64, f64)) {
        todo!("vulkan P1")
    }

    /// The frame of the oldest span still on the GPU.
    pub fn in_flight_frame(&self) -> Option<u64> {
        todo!("vulkan P1")
    }
}
