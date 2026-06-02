//! Hand-fused reference kernels — known-good single-kernel equivalents of
//! pointwise effect chains, validated against the unfused graph through the
//! [`TextureDiff`](super::TextureDiff) oracle. These are the targets the
//! eventual codegen must reproduce; until codegen exists they are written by
//! hand and serve three consumers: the oracle proof tests (correctness), the
//! `freeze-profile` bench (the real fused-vs-unfused timing), and codegen
//! (a reference output to diff against).
//!
//! Real (non-test) code: the bench is a separate bin and needs these too.

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice, GpuEncoder, GpuTexture};

/// Packed uniform for the fused ColorGrade kernel — mirrors `struct U` in
/// `shaders/colorgrade_fused.wgsl` exactly (16 words, 64 bytes). Each field is
/// the corresponding atom's authored param; [`Default`] reproduces the
/// ColorGrade preset's defaults (a neutral pass-through).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ColorGradeParams {
    pub gain: f32,
    pub sat_s: f32,
    pub hue_deg: f32,
    pub sat_h: f32,
    pub val_h: f32,
    pub contrast: f32,
    pub col_amount: f32,
    pub col_hue: f32,
    pub col_sat: f32,
    pub col_focus: f32,
    pub mix_amount: f32,
    pub mix_mode: u32,
    pub clamp_min: f32,
    pub clamp_max: f32,
    pub _pad0: f32,
    pub _pad1: f32,
}

impl Default for ColorGradeParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            sat_s: 1.0,
            hue_deg: 0.0,
            sat_h: 1.0,
            val_h: 1.0,
            contrast: 1.0,
            col_amount: 0.0,
            col_hue: 0.0,
            col_sat: 1.0,
            col_focus: 0.75,
            mix_amount: 0.0,
            mix_mode: 0,
            clamp_min: 0.0,
            clamp_max: 65000.0,
            _pad0: 0.0,
            _pad1: 0.0,
        }
    }
}

/// Compile the fused ColorGrade pipeline. Caches in the device's compute +
/// binary-archive caches; cheap to call repeatedly.
pub fn colorgrade_pipeline(device: &GpuDevice) -> GpuComputePipeline {
    device.create_compute_pipeline(
        include_str!("shaders/colorgrade_fused.wgsl"),
        "cs_main",
        "freeze.colorgrade_fused",
    )
}

/// Encode one fused-ColorGrade dispatch into `enc` (no commit — the caller
/// commits, so the bench can time the command buffer). Reads `input`, writes
/// `output`; both must be the same dimensions.
pub fn dispatch_fused_colorgrade(
    enc: &mut GpuEncoder,
    pipeline: &GpuComputePipeline,
    input: &GpuTexture,
    output: &GpuTexture,
    params: &ColorGradeParams,
) {
    let (w, h) = (output.width, output.height);
    enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(params),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: input,
            },
            GpuBinding::Texture {
                binding: 3,
                texture: output,
            },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "freeze.colorgrade_fused",
    );
}
