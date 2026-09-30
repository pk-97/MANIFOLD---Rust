//! GPU FFT primitive backed by MPSGraph's Fourier-transform ops.
//!
//! One compiled `MPSGraphExecutable` per plan (kind, shape, axes), encoded
//! into an existing `GpuEncoder`'s command buffer each dispatch. No graph
//! rebuild per frame.
//!
//! Plans are described by the logical REAL shape, row-major (last dimension
//! fastest). Complex data is interleaved float32 pairs `[re, im, ...]`. A
//! half spectrum keeps `n/2 + 1` entries along the LAST transformed axis:
//!
//!   * `RealToHermitean` — real `shape` → half spectrum, unscaled.
//!   * `HermiteanToReal` — half spectrum → real `shape`, inverse sign, scaled
//!     by 1 / (product of the transformed lengths), so it undoes
//!     `RealToHermitean` exactly.
//!   * `ComplexToComplex { inverse }` — full complex `shape`, unscaled.
//!
//! MPSGraph transforms only within the last four dimensions of a tensor.
//! `GpuFft` is `Send + Sync`: `MPSGraphExecutable` is thread-safe for encoding
//! per Apple's docs.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber};
use objc2_metal::{MTLBuffer, MTLDevice};
use objc2_metal_performance_shaders::{MPSCommandBuffer, MPSDataType};
use objc2_metal_performance_shaders_graph::{
    MPSGraph, MPSGraphCompilationDescriptor, MPSGraphDevice, MPSGraphExecutable,
    MPSGraphExecutableExecutionDescriptor, MPSGraphFFTDescriptor, MPSGraphFFTScalingMode,
    MPSGraphShapedType, MPSGraphTensor, MPSGraphTensorData,
};

use super::GpuBuffer;
use super::encoder::GpuEncoder;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FftKind {
    /// Real input → half spectrum (interleaved complex), unscaled.
    RealToHermitean,
    /// Half spectrum → real output, inverse sign, scaled by 1 / volume.
    HermiteanToReal,
    /// Interleaved complex input and output, unscaled.
    ComplexToComplex { inverse: bool },
}

/// What a plan is compiled for: the kind, the real shape (row-major, at most
/// four dimensions) and the transformed axes. Fixed size, so a lookup
/// allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FftPlanKey {
    kind: FftKind,
    shape: [usize; 4],
    dims: usize,
    /// Bit `a` set: axis `a` is transformed.
    axes: u8,
}

impl FftPlanKey {
    pub fn new(kind: FftKind, shape: &[usize], axes: &[usize]) -> Self {
        assert!(!shape.is_empty() && shape.len() <= 4, "FftPlanKey: 1 to 4 dimensions (got {shape:?})");
        let mut padded = [0; 4];
        padded[..shape.len()].copy_from_slice(shape);
        let mask = axes.iter().fold(0u8, |m, &a| {
            assert!(a < shape.len(), "FftPlanKey: axis {a} past shape {shape:?}");
            m | (1 << a)
        });
        Self { kind, shape: padded, dims: shape.len(), axes: mask }
    }

    pub(crate) fn build(&self, device: &super::GpuDevice) -> GpuFft {
        let axes: Vec<usize> = (0..self.dims).filter(|a| self.axes & (1 << a) != 0).collect();
        GpuFft::new_nd(device, self.kind, &self.shape[..self.dims], &axes)
    }
}

/// Compiled FFT plan. Build once, encode many times.
pub struct GpuFft {
    kind: FftKind,
    input_shape: Vec<usize>,
    output_shape: Vec<usize>,
    executable: Retained<MPSGraphExecutable>,
    #[expect(dead_code, reason = "ownership-only: keeps the MPSGraph alive for the executable's lifetime; un-suppress: never")]
    graph: Retained<MPSGraph>,
}

// Safety: `MPSGraphExecutable` is thread-safe for encoding (Apple docs:
// "Using Callables"). `GpuFft` exposes only `&self` encode.
unsafe impl Send for GpuFft {}
unsafe impl Sync for GpuFft {}

impl GpuFft {
    /// 1D real-to-half-spectrum FFT of length `n` (a power of two ≥ 2).
    pub fn new_r2c(device: &ProtocolObject<dyn MTLDevice>, n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "GpuFft::new_r2c: n must be a power of two ≥ 2 (got {n})");
        build_plan(device, FftKind::RealToHermitean, &[n], &[0])
    }

    /// 1D complex-to-complex FFT of length `n` (a power of two ≥ 2).
    pub fn new_c2c(device: &ProtocolObject<dyn MTLDevice>, n: usize, inverse: bool) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "GpuFft::new_c2c: n must be a power of two ≥ 2 (got {n})");
        build_plan(device, FftKind::ComplexToComplex { inverse }, &[n], &[0])
    }

    /// Multi-dimensional plan over `axes` of the real `shape` (row-major, at
    /// most four dimensions). Every transformed length is at least 2, any
    /// factors; the last transformed one is even, because the half-spectrum
    /// inverse is built for an even length (`setRoundToOddHermitean(false)`).
    pub fn new_nd(device: &super::GpuDevice, kind: FftKind, shape: &[usize], axes: &[usize]) -> Self {
        assert!(!shape.is_empty() && shape.len() <= 4, "GpuFft::new_nd: 1 to 4 dimensions (got {shape:?})");
        assert!(!axes.is_empty(), "GpuFft::new_nd: no axes");
        let mut seen = [false; 4];
        for &a in axes {
            assert!(a < shape.len() && !seen[a], "GpuFft::new_nd: bad axes {axes:?} for shape {shape:?}");
            seen[a] = true;
            assert!(shape[a] >= 2, "GpuFft::new_nd: transformed length must be at least 2 (got {shape:?})");
        }
        let last = *axes.iter().max().expect("at least one axis");
        assert!(shape[last].is_multiple_of(2), "GpuFft::new_nd: the last transformed length must be even (got {shape:?})");
        build_plan(device.raw_device(), kind, shape, axes)
    }

    pub fn kind(&self) -> FftKind {
        self.kind
    }

    /// Input buffer size in bytes.
    pub fn input_len_bytes(&self) -> u64 {
        (element_count(&self.input_shape) * dtype_bytes(input_dtype(self.kind))) as u64
    }

    /// Output buffer size in bytes.
    pub fn output_len_bytes(&self) -> u64 {
        (self.output_element_count() * 4) as u64
    }

    /// Output size in float32s (a complex value counts as two).
    pub fn output_element_count(&self) -> usize {
        element_count(&self.output_shape) * dtype_bytes(output_dtype(self.kind)) / 4
    }

    /// Encode one transform into the encoder's command buffer. Takes `&mut`
    /// because MPSGraph installs its own encoders, so any open pass ends.
    pub fn encode(&self, enc: &mut GpuEncoder, input: &GpuBuffer, output: &GpuBuffer) {
        let cmd_buf = enc.raw_cmd_buf();
        unsafe {
            let input_data = tensor_data_for_buffer(&input.raw, &self.input_shape, input_dtype(self.kind));
            let output_data = tensor_data_for_buffer(&output.raw, &self.output_shape, output_dtype(self.kind));
            let inputs = NSArray::from_retained_slice(&[input_data]);
            let outputs = NSArray::from_retained_slice(&[output_data]);
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
}

fn input_dtype(kind: FftKind) -> MPSDataType {
    match kind {
        FftKind::RealToHermitean => MPSDataType::Float32,
        FftKind::HermiteanToReal | FftKind::ComplexToComplex { .. } => MPSDataType::ComplexFloat32,
    }
}

fn output_dtype(kind: FftKind) -> MPSDataType {
    match kind {
        FftKind::HermiteanToReal => MPSDataType::Float32,
        FftKind::RealToHermitean | FftKind::ComplexToComplex { .. } => MPSDataType::ComplexFloat32,
    }
}

fn dtype_bytes(dtype: MPSDataType) -> usize {
    if dtype == MPSDataType::ComplexFloat32 { 8 } else { 4 }
}

fn element_count(shape: &[usize]) -> usize {
    shape.iter().product()
}

fn build_plan(device: &ProtocolObject<dyn MTLDevice>, kind: FftKind, shape: &[usize], axes: &[usize]) -> GpuFft {
    let last_axis = *axes.iter().max().expect("at least one axis");
    let mut half = shape.to_vec();
    half[last_axis] = shape[last_axis] / 2 + 1;
    let (input_shape, output_shape) = match kind {
        FftKind::RealToHermitean => (shape.to_vec(), half),
        FftKind::HermiteanToReal => (half, shape.to_vec()),
        FftKind::ComplexToComplex { .. } => (shape.to_vec(), shape.to_vec()),
    };
    unsafe {
        let graph = MPSGraph::new();
        let input_ns_shape = nsnumber_array(&input_shape);
        let input_tensor =
            graph.placeholderWithShape_dataType_name(Some(&input_ns_shape), input_dtype(kind), None);

        let fft_desc = MPSGraphFFTDescriptor::descriptor().expect("MPSGraphFFTDescriptor::descriptor returned nil");
        match kind {
            FftKind::RealToHermitean => fft_desc.setInverse(false),
            FftKind::HermiteanToReal => {
                fft_desc.setInverse(true);
                fft_desc.setScalingMode(MPSGraphFFTScalingMode::Size);
                fft_desc.setRoundToOddHermitean(false);
            }
            FftKind::ComplexToComplex { inverse } => fft_desc.setInverse(inverse),
        }
        let ns_axes = nsnumber_array(axes);
        let output_tensor: Retained<MPSGraphTensor> = match kind {
            FftKind::RealToHermitean => {
                graph.realToHermiteanFFTWithTensor_axes_descriptor_name(&input_tensor, &ns_axes, &fft_desc, None)
            }
            FftKind::HermiteanToReal => {
                graph.HermiteanToRealFFTWithTensor_axes_descriptor_name(&input_tensor, &ns_axes, &fft_desc, None)
            }
            FftKind::ComplexToComplex { .. } => {
                graph.fastFourierTransformWithTensor_axes_descriptor_name(&input_tensor, &ns_axes, &fft_desc, None)
            }
        };

        let mps_device = MPSGraphDevice::deviceWithMTLDevice(device);
        let shaped =
            MPSGraphShapedType::initWithShape_dataType(MPSGraphShapedType::alloc(), Some(&input_ns_shape), input_dtype(kind));
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
        GpuFft { kind, input_shape, output_shape, executable, graph }
    }
}

fn nsnumber_array(dims: &[usize]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = dims.iter().map(|&d| NSNumber::new_usize(d)).collect();
    NSArray::from_retained_slice(&numbers)
}

unsafe fn tensor_data_for_buffer(
    buf: &Retained<ProtocolObject<dyn MTLBuffer>>,
    shape: &[usize],
    dtype: MPSDataType,
) -> Retained<MPSGraphTensorData> {
    let shape_array = nsnumber_array(shape);
    unsafe { MPSGraphTensorData::initWithMTLBuffer_shape_dataType(MPSGraphTensorData::alloc(), buf, &shape_array, dtype) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metal::GpuDevice;

    fn upload(device: &GpuDevice, values: &[f32]) -> GpuBuffer {
        let buf = device.create_buffer_shared((values.len() * 4) as u64);
        let ptr = buf.mapped_ptr().expect("shared buffer has mapped_ptr");
        // SAFETY: shared buffer sized for `values`; no GPU work in flight.
        unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, values.len()).copy_from_slice(values) };
        buf
    }

    fn download(buf: &GpuBuffer, count: usize) -> Vec<f32> {
        let ptr = buf.mapped_ptr().expect("shared buffer has mapped_ptr");
        // SAFETY: shared buffer holding `count` floats; GPU work done.
        unsafe { std::slice::from_raw_parts(ptr as *const f32, count) }.to_vec()
    }

    /// A 1 kHz sine peaks at the expected bin with magnitude N/2.
    #[test]
    #[cfg(target_os = "macos")]
    fn r2c_unit_sine_peaks_at_expected_bin() {
        let device = GpuDevice::new();
        let n: usize = 4096;
        let (sr, freq) = (48_000.0_f32, 1_000.0_f32);
        let expected_bin = (freq * n as f32 / sr).round() as usize;
        let samples: Vec<f32> =
            (0..n).map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin()).collect();
        let in_buf = upload(&device, &samples);
        let fft = GpuFft::new_r2c(device.raw_device(), n);
        let out_buf = device.create_buffer_shared(fft.output_len_bytes());
        let mut enc = device.create_encoder("gpu-fft-test");
        fft.encode(&mut enc, &in_buf, &out_buf);
        enc.commit_and_wait_completed();
        let out = download(&out_buf, fft.output_element_count());
        let (peak_bin, peak_mag2) = (0..n / 2 + 1)
            .map(|b| (b, out[2 * b] * out[2 * b] + out[2 * b + 1] * out[2 * b + 1]))
            .fold((0, 0.0_f32), |best, cur| if cur.1 > best.1 { cur } else { best });
        assert!(peak_bin.abs_diff(expected_bin) <= 1, "GPU FFT peak bin {peak_bin}, expected {expected_bin}");
        let ratio = peak_mag2.sqrt() / (n as f32 / 2.0);
        assert!(ratio > 0.8 && ratio < 1.2, "peak magnitude ratio {ratio}");
    }

    /// 3D real → half spectrum matches a direct f64 DFT, and the inverse
    /// plan returns the input.
    #[test]
    #[cfg(target_os = "macos")]
    fn nd_real_transform_matches_direct_dft_and_round_trips() {
        let device = GpuDevice::new();
        let shape = [4usize, 8, 16];
        let total: usize = shape.iter().product();
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let values: Vec<f32> = (0..total)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            })
            .collect();
        let forward = GpuFft::new_nd(&device, FftKind::RealToHermitean, &shape, &[0, 1, 2]);
        let inverse = GpuFft::new_nd(&device, FftKind::HermiteanToReal, &shape, &[0, 1, 2]);
        let half_x = shape[2] / 2 + 1;
        assert_eq!(forward.output_element_count(), shape[0] * shape[1] * half_x * 2);
        assert_eq!(inverse.input_len_bytes(), forward.output_len_bytes());

        let in_buf = upload(&device, &values);
        let spectrum = device.create_buffer_shared(forward.output_len_bytes());
        let back = device.create_buffer_shared(inverse.output_len_bytes());
        let mut enc = device.create_encoder("gpu-fft-nd-test");
        forward.encode(&mut enc, &in_buf, &spectrum);
        inverse.encode(&mut enc, &spectrum, &back);
        enc.commit_and_wait_completed();

        let spec = download(&spectrum, forward.output_element_count());
        let tau = std::f64::consts::TAU;
        let mut worst = 0.0_f64;
        for kz in 0..shape[0] {
            for ky in 0..shape[1] {
                for kx in 0..half_x {
                    let (mut re, mut im) = (0.0_f64, 0.0_f64);
                    for z in 0..shape[0] {
                        for y in 0..shape[1] {
                            for x in 0..shape[2] {
                                let phase = -tau
                                    * ((kz * z) as f64 / shape[0] as f64
                                        + (ky * y) as f64 / shape[1] as f64
                                        + (kx * x) as f64 / shape[2] as f64);
                                let v = f64::from(values[x + shape[2] * (y + shape[1] * z)]);
                                re += v * phase.cos();
                                im += v * phase.sin();
                            }
                        }
                    }
                    let i = 2 * (kx + half_x * (ky + shape[1] * kz));
                    worst = worst.max((f64::from(spec[i]) - re).abs()).max((f64::from(spec[i + 1]) - im).abs());
                }
            }
        }
        assert!(worst < 1e-4, "half spectrum differs from the direct DFT by {worst}");
        let round_trip = download(&back, total);
        let err = values.iter().zip(&round_trip).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        assert!(err < 1e-5, "inverse plan does not return the input: {err}");
    }

    /// Mixed-radix plans, with every buffer checked on the CPU against the
    /// plan's byte lengths before anything runs.
    fn mixed_plans(device: &GpuDevice, shape: &[usize], values: &[f32]) -> (GpuFft, GpuFft, GpuBuffer, GpuBuffer, GpuBuffer) {
        let axes: Vec<usize> = (0..shape.len()).collect();
        let forward = GpuFft::new_nd(device, FftKind::RealToHermitean, shape, &axes);
        let inverse = GpuFft::new_nd(device, FftKind::HermiteanToReal, shape, &axes);
        let total: usize = shape.iter().product();
        let last = shape.len() - 1;
        let half: usize = shape[..last].iter().product::<usize>() * (shape[last] / 2 + 1);
        assert_eq!(values.len(), total);
        assert_eq!(forward.input_len_bytes(), (total * 4) as u64);
        assert_eq!(forward.output_len_bytes(), (half * 8) as u64);
        assert_eq!(inverse.input_len_bytes(), forward.output_len_bytes());
        assert_eq!(inverse.output_len_bytes(), (total * 4) as u64);
        let input = upload(device, values);
        let spectrum = device.create_buffer_shared(forward.output_len_bytes());
        let back = device.create_buffer_shared(inverse.output_len_bytes());
        assert!(input.size >= forward.input_len_bytes() && spectrum.size >= forward.output_len_bytes());
        assert!(back.size >= inverse.output_len_bytes());
        (forward, inverse, input, spectrum, back)
    }

    /// MPSGraph transforms lengths with factors 3, 5 and 7, and a prime on a
    /// leading axis, matching a direct DFT and round-tripping.
    #[test]
    #[cfg(target_os = "macos")]
    fn nd_real_transform_at_mixed_radix_lengths() {
        let device = GpuDevice::new();
        for shape in [[7usize, 6, 10], [12, 10, 6], [5, 3, 96]] {
            let total: usize = shape.iter().product();
            let mut state = 0x9e37_79b9_7f4a_7c15_u64;
            let values: Vec<f32> = (0..total)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
                })
                .collect();
            let (forward, inverse, input, spectrum, back) = mixed_plans(&device, &shape, &values);
            let mut enc = device.create_encoder("gpu-fft-mixed-test");
            forward.encode(&mut enc, &input, &spectrum);
            inverse.encode(&mut enc, &spectrum, &back);
            enc.commit_and_wait_completed();
            let spec = download(&spectrum, forward.output_element_count());
            let half_x = shape[2] / 2 + 1;
            let tau = std::f64::consts::TAU;
            let mut worst = 0.0_f64;
            for kz in 0..shape[0] {
                for ky in 0..shape[1] {
                    for kx in 0..half_x {
                        let (mut re, mut im) = (0.0_f64, 0.0_f64);
                        for z in 0..shape[0] {
                            for y in 0..shape[1] {
                                for x in 0..shape[2] {
                                    let phase = -tau
                                        * ((kz * z) as f64 / shape[0] as f64
                                            + (ky * y) as f64 / shape[1] as f64
                                            + (kx * x) as f64 / shape[2] as f64);
                                    let v = f64::from(values[x + shape[2] * (y + shape[1] * z)]);
                                    re += v * phase.cos();
                                    im += v * phase.sin();
                                }
                            }
                        }
                        let i = 2 * (kx + half_x * (ky + shape[1] * kz));
                        worst = worst.max((f64::from(spec[i]) - re).abs()).max((f64::from(spec[i + 1]) - im).abs());
                    }
                }
            }
            let round_trip = download(&back, total);
            let err = values.iter().zip(&round_trip).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
            eprintln!("FFT {shape:?}: direct DFT error {worst:.2e}, round trip {err:.2e}");
            assert!(worst < 1e-4, "{shape:?}: half spectrum differs from the direct DFT by {worst}");
            assert!(err < 1e-5, "{shape:?}: inverse plan does not return the input: {err}");
        }
    }

    /// A plane wave at 96³ and 80×112×96 lands on its one bin; the round trip
    /// returns it. Prints forward-plus-inverse GPU time beside 64³ and 128³.
    #[test]
    #[cfg(target_os = "macos")]
    fn nd_real_transform_plane_wave_and_cost_at_lattice_sizes() {
        let device = GpuDevice::new();
        for shape in [[64usize, 64, 64], [80, 112, 96], [96, 96, 96], [128, 128, 128]] {
            let k = [3usize, 5, 7];
            let total: usize = shape.iter().product();
            let tau = std::f64::consts::TAU;
            let mut values = vec![0.0f32; total];
            for z in 0..shape[0] {
                for y in 0..shape[1] {
                    for x in 0..shape[2] {
                        let phase = tau
                            * ((k[0] * z) as f64 / shape[0] as f64
                                + (k[1] * y) as f64 / shape[1] as f64
                                + (k[2] * x) as f64 / shape[2] as f64);
                        values[x + shape[2] * (y + shape[1] * z)] = phase.cos() as f32;
                    }
                }
            }
            let (forward, inverse, input, spectrum, back) = mixed_plans(&device, &shape, &values);
            let mut enc = device.create_encoder("gpu-fft-plane-wave");
            forward.encode(&mut enc, &input, &spectrum);
            inverse.encode(&mut enc, &spectrum, &back);
            enc.commit_and_wait_completed();
            let spec = download(&spectrum, forward.output_element_count());
            let half_x = shape[2] / 2 + 1;
            let peak = 2 * (k[2] + half_x * (k[1] + shape[1] * k[0]));
            let expected = total as f32 / 2.0;
            let mut stray = 0.0f32;
            for (i, pair) in spec.chunks_exact(2).enumerate() {
                if 2 * i != peak {
                    stray = stray.max(pair[0].abs()).max(pair[1].abs());
                }
            }
            let round_trip = download(&back, total);
            let err = values.iter().zip(&round_trip).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
            let mut ms = Vec::new();
            for _ in 0..12 {
                let mut enc = device.create_encoder("gpu-fft-cost");
                forward.encode(&mut enc, &input, &spectrum);
                inverse.encode(&mut enc, &spectrum, &back);
                ms.push(enc.commit_and_wait_completed_timed() * 1000.0);
            }
            ms.sort_by(f64::total_cmp);
            eprintln!(
                "FFT {shape:?}: peak {:.1} of {expected:.1}, stray {stray:.2e}, round trip {err:.2e}, forward+inverse {:.3} ms median",
                spec[peak], ms[ms.len() / 2]
            );
            assert!((spec[peak] - expected).abs() < expected * 1e-4 && spec[peak + 1].abs() < expected * 1e-4);
            assert!(stray < expected * 1e-4, "{shape:?}: energy outside the plane wave's bin: {stray}");
            assert!(err < 1e-4, "{shape:?}: round trip error {err}");
        }
    }
}
