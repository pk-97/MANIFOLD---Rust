//! The GPU FFT (P7). Unavailable until then: `GpuDevice::supports_fft`
//! is false and the FFT nodes say so.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use super::*;
use crate::*;

pub struct GpuFft {
    _reserved: (),
}

impl GpuFft {
    /// Multi-dimensional plan over `axes` of the real `shape` (row-major, at
    /// most four dimensions).
    pub fn new_nd(
        device: &crate::GpuDevice,
        kind: FftKind,
        shape: &[usize],
        axes: &[usize],
    ) -> Self {
        todo!("vulkan P1")
    }

    pub fn kind(&self) -> FftKind {
        todo!("vulkan P1")
    }

    /// Input buffer size in bytes.
    pub fn input_len_bytes(&self) -> u64 {
        todo!("vulkan P1")
    }

    /// Output buffer size in bytes.
    pub fn output_len_bytes(&self) -> u64 {
        todo!("vulkan P1")
    }

    /// Output size in float32s (a complex value counts as two).
    pub fn output_element_count(&self) -> usize {
        todo!("vulkan P1")
    }

    /// Encode one transform into the encoder's command buffer.
    pub fn encode(&self, enc: &mut GpuEncoder, input: &GpuBuffer, output: &GpuBuffer) {
        todo!("vulkan P1")
    }
}
