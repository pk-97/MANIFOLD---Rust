//! GPU-accelerated CQT pipeline: MPSGraph R2C FFT + WGSL sparse CSR mat-vec.
//!
//! Sits between the worker thread's rolling audio window and its CPU-side
//! synchrosqueezing / dB-conversion step. Per hop:
//!
//!   1. memcpy rolling window → `fft_input` shared MTLBuffer
//!   2. encode R2C FFT via `GpuFft`, output in `fft_output`
//!   3. encode sparse CSR mat-vec compute shader, output in `cqt_output`
//!   4. `commit_and_wait_completed` — unified memory means no explicit
//!      download; the mapped pointer of `cqt_output` is now readable.
//!
//! Replaces the CPU rustfft + scalar sparse mat-vec that used to run
//! inline in `spectrum_worker::WorkerState::emit_column`. Uses unified
//! memory throughout (shared buffers) so CPU and GPU see the same bytes.

use crate::cqt::{CqtComplex, CqtTransform};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuFft};

const SHADER: &str = include_str!("../shaders/cqt_kernel_mul.wgsl");

#[repr(C)]
#[derive(Copy, Clone)]
struct KernelMulUniforms {
    n_fft: u32,
    num_bins: u32,
    _pad0: u32,
    _pad1: u32,
}

pub struct GpuCqt {
    n_fft: usize,
    num_bins: usize,

    /// Cached bin-geometry arrays — the synchrosqueezing pass on the
    /// worker thread still reads these on CPU (IF computation, gating),
    /// so we stash them here instead of round-tripping through
    /// `CqtTransform`.
    center_freqs: Vec<f32>,
    bandwidths_hz: Vec<f32>,

    /// R2C FFT plan (Apple MPSGraph).
    fft: GpuFft,
    /// Compute pipeline for the CSR sparse mat-vec.
    kernel_mul: GpuComputePipeline,

    /// Per-hop buffers. All shared (unified memory) so CPU can memcpy
    /// the rolling window in and read out the CQT result without an
    /// explicit transfer.
    fft_input: GpuBuffer,
    fft_output: GpuBuffer,
    cqt_output: GpuBuffer,

    /// Sparse kernel buffers — written once at construction, immutable
    /// thereafter.
    row_ptr_buf: GpuBuffer,
    col_idx_buf: GpuBuffer,
    coef_buf: GpuBuffer,
}

impl GpuCqt {
    /// Build the GPU CQT pipeline from an already-constructed CPU
    /// `CqtTransform`. The sparse kernel (row_ptr / col_idx / coef) is
    /// uploaded once here; the expensive CPU-side kernel-construction
    /// FFTs run on the caller's thread and aren't repeated.
    pub fn new(device: &GpuDevice, cqt: &CqtTransform) -> Self {
        let n_fft = cqt.n_fft();
        let num_bins = cqt.num_bins();

        let fft = GpuFft::new_r2c(device.raw_device(), n_fft);

        // Per-hop buffers.
        let fft_input = device.create_buffer_shared((n_fft * 4) as u64);
        let fft_output = device.create_buffer_shared(fft.output_len_bytes());
        let cqt_output = device.create_buffer_shared((num_bins * 8) as u64);

        // Kernel buffers. Sizes are known from CqtTransform accessors.
        let (row_ptr, col_idx, coef) = cqt.csr_raw();
        let row_ptr_buf = device.create_buffer_shared((row_ptr.len() * 4) as u64);
        let col_idx_buf = device.create_buffer_shared((col_idx.len() * 4) as u64);
        let coef_buf = device.create_buffer_shared((coef.len() * 8) as u64);

        unsafe {
            copy_slice_to_buffer(&row_ptr_buf, row_ptr);
            copy_slice_to_buffer(&col_idx_buf, col_idx);
            copy_slice_to_buffer(&coef_buf, coef);
        }

        let kernel_mul = device.create_compute_pipeline(
            SHADER,
            "cqt_kernel_mul",
            "manifold-analyzer-cqt-kernel-mul",
        );

        Self {
            n_fft,
            num_bins,
            center_freqs: cqt.center_freqs().to_vec(),
            bandwidths_hz: cqt.bandwidths_hz().to_vec(),
            fft,
            kernel_mul,
            fft_input,
            fft_output,
            cqt_output,
            row_ptr_buf,
            col_idx_buf,
            coef_buf,
        }
    }

    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    pub fn center_freqs(&self) -> &[f32] {
        &self.center_freqs
    }

    pub fn bandwidths_hz(&self) -> &[f32] {
        &self.bandwidths_hz
    }

    /// Run one CQT hop. `rolling` must be exactly `n_fft` samples in
    /// oldest→newest order (matching what `CqtTransform::process_complex`
    /// expected). Writes the complex CQT output into `out` (length
    /// `num_bins`).
    pub fn process(
        &mut self,
        device: &GpuDevice,
        rolling: &[f32],
        out: &mut [CqtComplex<f32>],
    ) {
        debug_assert_eq!(rolling.len(), self.n_fft);
        debug_assert_eq!(out.len(), self.num_bins);

        // 1. Upload samples into the FFT input buffer.
        if let Some(ptr) = self.fft_input.mapped_ptr() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    rolling.as_ptr() as *const u8,
                    ptr,
                    rolling.len() * 4,
                );
            }
        }

        // 2 + 3. Encode FFT → CSR mat-vec into a single command buffer.
        let mut enc = device.create_encoder("cqt-hop");
        self.fft.encode(&mut enc, &self.fft_input, &self.fft_output);

        let uniforms = KernelMulUniforms {
            n_fft: self.n_fft as u32,
            num_bins: self.num_bins as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let uniform_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                &uniforms as *const _ as *const u8,
                std::mem::size_of::<KernelMulUniforms>(),
            )
        };

        // One thread per CQT bin, 64-wide workgroups → ceil(num_bins/64)
        // workgroups. The shader bounds-checks inside.
        let workgroups_x = self.num_bins.div_ceil(64) as u32;
        enc.dispatch_compute(
            &self.kernel_mul,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: uniform_bytes,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.fft_output,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &self.row_ptr_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &self.col_idx_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &self.coef_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: &self.cqt_output,
                    offset: 0,
                },
            ],
            [workgroups_x, 1, 1],
            "cqt_kernel_mul",
        );

        // 4. Submit + block. Apple Silicon unified memory means the
        // mapped pointer is immediately readable once the command buffer
        // completes — no explicit download.
        enc.commit_and_wait_completed();

        if let Some(ptr) = self.cqt_output.mapped_ptr() {
            unsafe {
                let src =
                    std::slice::from_raw_parts(ptr as *const CqtComplex<f32>, self.num_bins);
                out.copy_from_slice(src);
            }
        }
    }
}

/// Copy a `&[T]` into a shared MTLBuffer's mapped region. Caller must
/// ensure the buffer was allocated with at least `size_of_val(src)` bytes.
unsafe fn copy_slice_to_buffer<T>(buf: &GpuBuffer, src: &[T]) {
    if let Some(ptr) = buf.mapped_ptr() {
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.as_ptr() as *const u8,
                ptr,
                std::mem::size_of_val(src),
            );
        }
    }
}
