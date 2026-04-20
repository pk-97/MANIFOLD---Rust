//! GPU FFT primitive backed by MPSGraph's Fourier-transform ops.
//!
//! Apple's MetalPerformanceShadersGraph ships a compiled-graph FFT. We build
//! the graph once per (N, direction) plan, compile it to an
//! `MPSGraphExecutable`, and encode it into an existing `GpuEncoder`'s command
//! buffer each dispatch. No per-frame allocation, no graph rebuild.
//!
//! Two plan kinds today:
//!
//!   * `GpuFft::new_r2c(device, n)` — real-input forward FFT (length N) →
//!     Hermitean-packed complex output. Output layout = an `(N/2 + 1) × 2`
//!     float32 tensor (interleaved `[re0, im0, re1, im1, ...]`), which is
//!     how MPSGraph materialises complex tensors as float32 pairs. First
//!     and last bins have zero imaginary part by construction.
//!
//!   * `GpuFft::new_c2c(device, n, inverse)` — complex-to-complex, interleaved
//!     `[re, im, re, im, ...]` layout for both input and output.
//!
//! Typical analyzer use:
//!
//! ```ignore
//! let fft = GpuFft::new_r2c(&device, 65536);
//! // per hop:
//! let mut enc = device.create_encoder("cqt fft");
//! fft.encode(&mut enc, &audio_buffer, &spectrum_buffer);
//! enc.commit_and_wait_completed();
//! ```
//!
//! Thread safety: `GpuFft` is `Send + Sync` — the underlying
//! `MPSGraphExecutable` is thread-safe for encoding per Apple's docs. Safe to
//! stash on a worker thread and share across plugin instances (though we
//! create one per instance today).

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber};
use objc2_metal::{MTLBuffer, MTLDevice};
use objc2_metal_performance_shaders::{MPSCommandBuffer, MPSDataType};
use objc2_metal_performance_shaders_graph::{
    MPSGraph, MPSGraphCompilationDescriptor, MPSGraphDevice, MPSGraphExecutable,
    MPSGraphExecutableExecutionDescriptor, MPSGraphFFTDescriptor, MPSGraphShapedType,
    MPSGraphTensor, MPSGraphTensorData,
};

use super::GpuBuffer;
use super::encoder::GpuEncoder;

/// What kind of transform this plan implements. Only the sizes + memory
/// layouts the analyzer needs today; c2c inverse is included because
/// spectral effects down the road will want it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FftKind {
    /// Real input (length `n` float32) → Hermitean-packed complex output
    /// (length `n/2 + 1`, interleaved float32 pairs).
    RealToHermitean,
    /// Interleaved complex input/output, forward (`inverse = false`) or
    /// inverse (`inverse = true`).
    ComplexToComplex { inverse: bool },
}

/// Compiled FFT plan. Reusable across dispatches — build once per plugin
/// instance (or worker thread), encode many times.
pub struct GpuFft {
    kind: FftKind,
    n: usize,
    executable: Retained<MPSGraphExecutable>,
    // Keep the graph + its tensors alive for the executable's lifetime.
    // MPSGraph's ownership model doesn't strictly require this once the
    // executable is compiled, but retaining them is cheap insurance.
    #[allow(dead_code)]
    graph: Retained<MPSGraph>,
}

// Safety: `MPSGraphExecutable` is thread-safe for encoding (Apple docs:
// "Using Callables"). `GpuFft` exposes only `&self` encode; no interior
// mutability.
unsafe impl Send for GpuFft {}
unsafe impl Sync for GpuFft {}

impl GpuFft {
    /// Real-to-Hermitean forward FFT. `n` must be a power of two and ≥ 2.
    /// Output layout: `(n/2 + 1) × 2` float32s interleaved
    /// `[re_0, im_0, re_1, im_1, ...]`. The conjugate-symmetric negative
    /// half is implied.
    pub fn new_r2c(device: &ProtocolObject<dyn MTLDevice>, n: usize) -> Self {
        assert!(
            n.is_power_of_two() && n >= 2,
            "GpuFft::new_r2c: n must be a power of two ≥ 2 (got {n})"
        );
        build_plan(device, FftKind::RealToHermitean, n)
    }

    /// Complex-to-complex FFT (forward or inverse). Input/output are
    /// interleaved `[re, im, re, im, ...]` float32 pairs of length `n`.
    pub fn new_c2c(device: &ProtocolObject<dyn MTLDevice>, n: usize, inverse: bool) -> Self {
        assert!(
            n.is_power_of_two() && n >= 2,
            "GpuFft::new_c2c: n must be a power of two ≥ 2 (got {n})"
        );
        build_plan(device, FftKind::ComplexToComplex { inverse }, n)
    }

    pub fn n(&self) -> usize {
        self.n
    }

    pub fn kind(&self) -> FftKind {
        self.kind
    }

    /// Output buffer size in bytes (float32 elements × 4). For r2c this is
    /// `(n/2 + 1) * 2 * 4`; for c2c it's `n * 2 * 4`.
    pub fn output_len_bytes(&self) -> u64 {
        (self.output_element_count() * 4) as u64
    }

    /// Output element count in float32s (complex pairs = 2 floats each).
    pub fn output_element_count(&self) -> usize {
        match self.kind {
            FftKind::RealToHermitean => (self.n / 2 + 1) * 2,
            FftKind::ComplexToComplex { .. } => self.n * 2,
        }
    }

    /// Encode one FFT dispatch into the given encoder's command buffer.
    /// Takes `&mut enc` because it must end any in-flight compute/render
    /// pass first — MPSGraph installs its own encoders.
    pub fn encode(&self, enc: &mut GpuEncoder, input: &GpuBuffer, output: &GpuBuffer) {
        let cmd_buf = enc.raw_cmd_buf();

        unsafe {
            let input_data = tensor_data_for_buffer(
                &input.raw,
                &self.input_shape(),
                self.input_dtype(),
            );
            let output_data = tensor_data_for_buffer(
                &output.raw,
                &self.output_shape(),
                self.output_dtype(),
            );

            let inputs = NSArray::from_retained_slice(&[input_data]);
            let outputs = NSArray::from_retained_slice(&[output_data]);

            // MPSGraphExecutable.encode wants an MPSCommandBuffer. Wrap
            // our raw MTLCommandBuffer in one; MPSCommandBuffer is a
            // thin shim that MPS uses to support commitAndContinue.
            let mps_cmd_buf = MPSCommandBuffer::commandBufferWithCommandBuffer(cmd_buf);

            let exec_desc = MPSGraphExecutableExecutionDescriptor::new();
            exec_desc.setWaitUntilCompleted(false);

            self.executable.encodeToCommandBuffer_inputsArray_resultsArray_executionDescriptor(
                &mps_cmd_buf,
                &inputs,
                Some(&outputs),
                Some(&exec_desc),
            );
        }
    }

    fn input_shape(&self) -> Vec<usize> {
        // Real input: 1D real tensor of length N.
        // Complex input: 1D complex tensor of length N (each element is
        // 8 bytes when bound as ComplexFloat32).
        vec![self.n]
    }

    fn output_shape(&self) -> Vec<usize> {
        match self.kind {
            FftKind::RealToHermitean => vec![self.n / 2 + 1],
            FftKind::ComplexToComplex { .. } => vec![self.n],
        }
    }

    fn input_dtype(&self) -> MPSDataType {
        match self.kind {
            FftKind::RealToHermitean => MPSDataType::Float32,
            FftKind::ComplexToComplex { .. } => MPSDataType::ComplexFloat32,
        }
    }

    fn output_dtype(&self) -> MPSDataType {
        // Both R2C and C2C produce complex output; bind as
        // ComplexFloat32 so MPSGraph lays re/im out as interleaved
        // float32 pairs (which is what the buffer actually holds).
        MPSDataType::ComplexFloat32
    }
}

// ─── Graph construction ────────────────────────────────────────────────

fn build_plan(
    device: &ProtocolObject<dyn MTLDevice>,
    kind: FftKind,
    n: usize,
) -> GpuFft {
    unsafe {
        let graph = MPSGraph::new();

        // 1D tensors. The real placeholder has dtype Float32; complex input
        // would use ComplexFloat32. MPSGraph packs complex values as
        // interleaved re/im float32 pairs in the underlying MTLBuffer.
        let input_shape = nsnumber_array(&[n]);
        let input_dtype = match kind {
            FftKind::RealToHermitean => MPSDataType::Float32,
            FftKind::ComplexToComplex { .. } => MPSDataType::ComplexFloat32,
        };

        let input_tensor = graph.placeholderWithShape_dataType_name(
            Some(&input_shape),
            input_dtype,
            None,
        );

        // FFT descriptor. No scaling — the analyzer applies its own
        // `2/Σw` kernel normalisation so a raw unscaled transform keeps
        // us equivalent to rustfft's default output.
        let fft_desc = MPSGraphFFTDescriptor::descriptor()
            .expect("MPSGraphFFTDescriptor::descriptor returned nil");
        match kind {
            FftKind::RealToHermitean => fft_desc.setInverse(false),
            FftKind::ComplexToComplex { inverse } => fft_desc.setInverse(inverse),
        }

        // Axis-0 transform (the N-length dimension).
        let axes = nsnumber_array(&[0usize]);

        let output_tensor: Retained<MPSGraphTensor> = match kind {
            FftKind::RealToHermitean => graph.realToHermiteanFFTWithTensor_axes_descriptor_name(
                &input_tensor,
                &axes,
                &fft_desc,
                None,
            ),
            FftKind::ComplexToComplex { .. } => graph
                .fastFourierTransformWithTensor_axes_descriptor_name(
                    &input_tensor,
                    &axes,
                    &fft_desc,
                    None,
                ),
        };

        // Compile. `feeds` is NSDictionary<MPSGraphTensor*, MPSGraphShapedType*>
        // that nails the input shape so the graph can specialise.
        let mps_device = MPSGraphDevice::deviceWithMTLDevice(device);
        let shaped = MPSGraphShapedType::initWithShape_dataType(
            MPSGraphShapedType::alloc(),
            Some(&input_shape),
            input_dtype,
        );
        let feeds: Retained<NSDictionary<MPSGraphTensor, MPSGraphShapedType>> =
            NSDictionary::from_slices(&[&*input_tensor], &[&*shaped]);

        let targets = NSArray::from_retained_slice(&[output_tensor]);
        let compile_desc = MPSGraphCompilationDescriptor::new();
        compile_desc.setWaitForCompilationCompletion(true);

        let executable = graph.compileWithDevice_feeds_targetTensors_targetOperations_compilationDescriptor(
            Some(&mps_device),
            &feeds,
            &targets,
            None,
            Some(&compile_desc),
        );

        GpuFft {
            kind,
            n,
            executable,
            graph,
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────

fn nsnumber_array(dims: &[usize]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = dims
        .iter()
        .map(|&d| NSNumber::new_usize(d))
        .collect();
    NSArray::from_retained_slice(&numbers)
}

unsafe fn tensor_data_for_buffer(
    buf: &Retained<ProtocolObject<dyn MTLBuffer>>,
    shape: &[usize],
    dtype: MPSDataType,
) -> Retained<MPSGraphTensorData> {
    let shape_array = nsnumber_array(shape);
    unsafe {
        MPSGraphTensorData::initWithMTLBuffer_shape_dataType(
            MPSGraphTensorData::alloc(),
            buf,
            &shape_array,
            dtype,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metal::GpuDevice;

    /// Write a 1 kHz unit-amplitude sine into a real-input buffer, run the
    /// GPU R2C FFT, and assert the peak magnitude lands in the expected
    /// bin. Mirrors the CPU FFT test in `plugins/manifold-analyzer-dsp`.
    #[test]
    #[cfg(target_os = "macos")]
    fn r2c_unit_sine_peaks_at_expected_bin() {
        let device = GpuDevice::new();
        let n: usize = 4096;
        let sr: f32 = 48_000.0;
        let target_freq: f32 = 1_000.0;
        let expected_bin = (target_freq * n as f32 / sr).round() as usize;

        let in_buf = device.create_buffer_shared((n * 4) as u64);
        let ptr = in_buf.mapped_ptr().expect("shared buffer has mapped_ptr");
        unsafe {
            let slice = std::slice::from_raw_parts_mut(ptr as *mut f32, n);
            for (i, s) in slice.iter_mut().enumerate() {
                *s = (2.0 * std::f32::consts::PI * target_freq * i as f32 / sr).sin();
            }
        }

        let fft = GpuFft::new_r2c(device.raw_device(), n);
        let out_buf = device.create_buffer_shared(fft.output_len_bytes());

        let mut enc = device.create_encoder("gpu-fft-test");
        fft.encode(&mut enc, &in_buf, &out_buf);
        enc.commit_and_wait_completed();

        let out_ptr = out_buf.mapped_ptr().expect("output buffer has mapped_ptr");
        let out = unsafe {
            std::slice::from_raw_parts(out_ptr as *const f32, fft.output_element_count())
        };

        let mut peak_bin = 0usize;
        let mut peak_mag2 = 0.0_f32;
        for bin in 0..n / 2 + 1 {
            let re = out[2 * bin];
            let im = out[2 * bin + 1];
            let mag2 = re * re + im * im;
            if mag2 > peak_mag2 {
                peak_mag2 = mag2;
                peak_bin = bin;
            }
        }

        assert!(
            peak_bin.abs_diff(expected_bin) <= 1,
            "GPU FFT peak bin {peak_bin}, expected {expected_bin}"
        );
        // |X[k]| for a unit cosine at bin k is N/2. Sine has the same
        // magnitude, just a π/2 phase offset.
        let expected_mag = n as f32 / 2.0;
        let peak_mag = peak_mag2.sqrt();
        assert!(
            peak_mag / expected_mag > 0.8 && peak_mag / expected_mag < 1.2,
            "peak mag {peak_mag}, expected ≈ {expected_mag}"
        );
    }
}
