//! Oracle diff core — the foundational verification primitive for the
//! freeze/fusion compiler (design §7, §11.D).
//!
//! The frozen (fused) build is a mechanical transform of the unfused graph, so
//! the unfused graph is a free exact oracle: render both ways on the same
//! inputs and diff. [`TextureDiff`] is the diff half — it compares two
//! same-dimension textures entirely on the GPU and reads back a 16-byte
//! verdict, instead of the per-pixel CPU scan the legacy parity harness used
//! (slow, doesn't scale to fuzzed multi-resolution sweeps).
//!
//! The verdict is two-sided and discontinuity-aware (design §11.D): a texel
//! fails only when it breaks BOTH the absolute and relative bounds, and the
//! pass test tolerates a small fraction of such texels rather than tripping on
//! one — the f16-round-trip vs f32-register split lands a few boundary texels
//! on the wrong side of a clamp/step, and that is expected, not a regression.
//!
//! Backend-agnostic by construction: WGSL through `create_compute_pipeline`,
//! dispatch through `GpuEncoder`, readback through a shared `GpuBuffer`. No
//! Metal types appear here; a Vulkan backend runs the identical path.

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice, GpuTexture};

/// Parameters handed to the reduction shader. `#[repr(C)]`, 16 bytes, matches
/// `Params` in `shaders/diff_reduce.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DiffParams {
    width: u32,
    height: u32,
    abs_tol: f32,
    rel_tol: f32,
}

/// Outcome of comparing two textures. `max_abs`/`max_rel` are the worst single
/// texel seen; `over_count` is how many texels broke both tolerances.
#[derive(Debug, Clone, Copy)]
pub struct DiffResult {
    /// Largest per-texel absolute diff (max over channels) over the FINITE
    /// texels only — non-finite texels are classified separately
    /// (`special_count`) and excluded so a NaN/Inf can't corrupt this maximum.
    pub max_abs: f32,
    /// Largest per-texel relative diff over finite texels:
    /// `abs_diff / max(|a|, |b|, eps)`.
    pub max_rel: f32,
    /// Count of texels that exceeded BOTH the absolute and relative bounds.
    pub over_count: u32,
    /// Count of texels where the two sides DISAGREE on finiteness — one side
    /// produced a NaN/Inf the other didn't (fusion introduced or erased a
    /// special value). Any non-zero count fails the verdict: a divergent NaN/Inf
    /// is never "within tolerance" (design §12.3 step 6). Texels where both
    /// sides agree on a non-finite result are not counted.
    pub special_count: u32,
    /// Total texels compared (`width * height`).
    pub total: u32,
}

impl DiffResult {
    /// Fraction of texels that failed both bounds (0.0 = identical-enough).
    pub fn over_fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            f64::from(self.over_count) / f64::from(self.total)
        }
    }

    /// Verdict: pass when no more than `max_over_fraction` of texels broke both
    /// tolerances, AND no texel diverged on a NaN/Inf, AND no non-finite value
    /// leaked into the diagnostic maxima. The tolerances themselves were applied
    /// in the shader; this is the discontinuity-aware fraction gate plus the
    /// hard NaN/Inf-agreement gate on top (a divergent special is never "within
    /// tolerance", design §12.3 step 6).
    pub fn passes(&self, max_over_fraction: f64) -> bool {
        self.special_count == 0
            && self.max_abs.is_finite()
            && self.max_rel.is_finite()
            && self.over_fraction() <= max_over_fraction
    }
}

/// GPU texture-diff reducer. Build once (compiles the reduction pipeline),
/// reuse across many compares — the fuzzer calls [`Self::compare`] per
/// (input, param) sample.
pub struct TextureDiff {
    pipeline: GpuComputePipeline,
}

impl TextureDiff {
    /// Compile the reduction pipeline. Cheap to hold; the pipeline caches in
    /// the device's compute cache + binary archive.
    pub fn new(device: &GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/diff_reduce.wgsl"),
            "cs_main",
            "freeze.diff_reduce",
        );
        Self { pipeline }
    }

    /// Compare two same-dimension textures. `abs_tol`/`rel_tol` set the
    /// per-texel "fails both" threshold the shader counts into `over_count`.
    ///
    /// Panics if the textures differ in dimensions — the oracle only ever
    /// diffs two renders of the same graph at the same resolution.
    pub fn compare(
        &self,
        device: &GpuDevice,
        a: &GpuTexture,
        b: &GpuTexture,
        abs_tol: f32,
        rel_tol: f32,
    ) -> DiffResult {
        assert_eq!(
            (a.width, a.height),
            (b.width, b.height),
            "TextureDiff::compare requires identical dimensions"
        );
        let (w, h) = (a.width, a.height);

        // 16-byte shared result buffer: [max_abs_bits, max_rel_bits,
        // over_count, _pad]. Zero it before the dispatch — the shader folds
        // into it with atomicMax/atomicAdd, so it must start at the identity
        // (0 = bits of +0.0 = the smallest non-negative float, and 0 counts).
        let out_buf = device.create_buffer_shared(16);
        // SAFETY: shared buffers expose a CPU-visible pointer for exactly this.
        unsafe {
            let ptr = out_buf
                .mapped_ptr()
                .expect("shared result buffer must expose a mapped pointer");
            std::ptr::write_bytes(ptr, 0, 16);
        }

        let params = DiffParams {
            width: w,
            height: h,
            abs_tol,
            rel_tol,
        };

        let mut enc = device.create_encoder("freeze.diff");
        enc.dispatch_compute(
            &self.pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: &out_buf,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: a,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: b,
                },
                GpuBinding::Bytes {
                    binding: 3,
                    data: bytemuck::bytes_of(&params),
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "freeze.diff",
        );
        enc.commit_and_wait_completed();

        // SAFETY: same shared buffer, now holding the reduced result.
        let raw: [u32; 4] = unsafe {
            let ptr = out_buf
                .mapped_ptr()
                .expect("shared result buffer must expose a mapped pointer")
                .cast::<u32>();
            [
                ptr.read_unaligned(),
                ptr.add(1).read_unaligned(),
                ptr.add(2).read_unaligned(),
                ptr.add(3).read_unaligned(),
            ]
        };

        DiffResult {
            max_abs: f32::from_bits(raw[0]),
            max_rel: f32::from_bits(raw[1]),
            over_count: raw[2],
            special_count: raw[3],
            total: w * h,
        }
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

    fn cleared(device: &GpuDevice, w: u32, h: u32, rgba: [f64; 4], label: &str) -> RenderTarget {
        let rt = RenderTarget::new(device, w, h, FMT, label);
        crate::clear_texture_committed(device, &rt.texture, rgba, label);
        rt
    }

    #[test]
    fn identical_textures_diff_to_zero() {
        let device = crate::test_device();
        let (w, h) = (64u32, 48u32);
        let a = cleared(&device, w, h, [0.25, 0.5, 0.75, 1.0], "diff-id-a");
        let b = cleared(&device, w, h, [0.25, 0.5, 0.75, 1.0], "diff-id-b");

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &a.texture, &b.texture, 1e-4, 1e-3);

        assert_eq!(r.total, w * h);
        assert_eq!(r.over_count, 0, "identical clears must not exceed tolerance");
        assert!(r.max_abs < 1e-4, "max_abs should be ~0, got {}", r.max_abs);
        assert!(r.passes(0.0), "identical textures must pass at zero fraction");
    }

    #[test]
    fn known_delta_is_measured_exactly() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        // Green differs by 0.25; the max-over-channels abs diff must be 0.25.
        let a = cleared(&device, w, h, [0.5, 0.25, 0.5, 1.0], "diff-d-a");
        let b = cleared(&device, w, h, [0.5, 0.50, 0.5, 1.0], "diff-d-b");

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &a.texture, &b.texture, 1e-3, 1e-3);

        // f16 stores 0.25 and 0.5 exactly, so the diff is exactly 0.25.
        assert!(
            (r.max_abs - 0.25).abs() < 1e-3,
            "expected max_abs ~0.25, got {}",
            r.max_abs
        );
        // Every texel differs and 0.25 >> both tolerances → all fail both.
        assert_eq!(r.over_count, w * h, "every texel should exceed tolerance");
        assert!(!r.passes(0.01), "a 0.25 delta everywhere must fail the verdict");
    }

    #[test]
    fn introduced_inf_is_counted_and_fails_verdict() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        // One side finite, the other Inf on the R channel — the classic
        // "fusion blew up where the oracle stayed finite" case the `max()` path
        // would have let slip (Inf is not NaN).
        let finite = cleared(&device, w, h, [0.5, 0.5, 0.5, 1.0], "diff-inf-a");
        let blown = cleared(&device, w, h, [f64::INFINITY, 0.5, 0.5, 1.0], "diff-inf-b");

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &finite.texture, &blown.texture, 1e-3, 1e-3);

        assert_eq!(
            r.special_count,
            w * h,
            "every texel diverges on finiteness → all counted as special"
        );
        assert!(
            !r.passes(0.5),
            "a divergent Inf must fail the verdict regardless of over_fraction \
             (special_count={})",
            r.special_count
        );
    }

    #[test]
    fn agreed_inf_is_not_counted() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        // Both sides Inf on R — the fused kernel reproduced the oracle's
        // non-finite result. Agreement, not a regression: not counted.
        let a = cleared(&device, w, h, [f64::INFINITY, 0.25, 0.5, 1.0], "diff-inf2-a");
        let b = cleared(&device, w, h, [f64::INFINITY, 0.25, 0.5, 1.0], "diff-inf2-b");

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &a.texture, &b.texture, 1e-3, 1e-3);

        assert_eq!(r.special_count, 0, "matching Inf is agreement, not divergence");
        assert!(r.passes(0.0), "both sides identical (incl. the Inf) must pass");
    }

    #[test]
    fn small_delta_within_abs_tolerance_passes() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        // ~0.002 apart: above neither-and 0.001? It's above abs_tol 1e-3 but we
        // raise abs_tol to 5e-3 so it stays within the absolute bound and the
        // "fails both" count stays zero.
        let a = cleared(&device, w, h, [0.5, 0.5, 0.5, 1.0], "diff-s-a");
        let b = cleared(&device, w, h, [0.502, 0.5, 0.5, 1.0], "diff-s-b");

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &a.texture, &b.texture, 5e-3, 1e-2);

        assert_eq!(
            r.over_count, 0,
            "a delta within the absolute bound must not be counted (max_abs={})",
            r.max_abs
        );
        assert!(r.passes(0.0));
    }
}
