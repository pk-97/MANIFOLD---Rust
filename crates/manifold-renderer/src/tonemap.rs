//! ACES tonemapping pipeline — mechanical translation of Unity's
//! CompositorStack.ApplyTonemap() + ACESTonemap.shader.
//!
//! Owned by the compositor. Applied as the final step after master effects,
//! before the blit to the display surface.
//!
//! Uses a native Metal compute dispatch via manifold-gpu. This eliminates
//! Metal TBDR tile alloc/load/store overhead (~290us at 4K per pass).

use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use manifold_core::TonemapCurve;
use manifold_gpu::{GpuDevice, GpuTexture};

/// Per-frame tonemap settings. Matches Unity CompositorStack properties:
/// TonemapExposure, HDROutputEnabled, PaperWhiteNits, MaxDisplayNits.
#[derive(Debug, Clone, Copy)]
pub struct TonemapSettings {
    /// Exposure multiplier for ACES tonemapping. 1.0 = neutral.
    /// Matches Unity CompositorStack.TonemapExposure.
    pub exposure: f32,
    /// HDR output mode. false = SDR (sRGB tonemap), true = HDR display-linear (EDR).
    /// Matches Unity CompositorStack.HDROutputEnabled.
    pub hdr_output_enabled: bool,
    /// Paper white in nits (scene 1.0 maps to this). Typical: 200 nits.
    /// Matches Unity CompositorStack.PaperWhiteNits.
    pub paper_white_nits: f32,
    /// Display maximum luminance in nits. HDR TVs: 1000, LED walls: 5000+.
    /// Matches Unity CompositorStack.MaxDisplayNits.
    pub max_display_nits: f32,
    /// Tonemapping curve selection.
    pub curve: TonemapCurve,
}

impl Default for TonemapSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            hdr_output_enabled: false,
            paper_white_nits: 200.0,
            max_display_nits: 1000.0,
            curve: TonemapCurve::AcesNarkowicz,
        }
    }
}

/// Uniform buffer layout for the tonemap shader.
/// Two u32 fields: mode (SDR/PQ/EDR) and curve (Narkowicz/Hill/AgX).
/// 24 bytes total — padded to 32 bytes for 16-byte alignment.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TonemapUniforms {
    exposure: f32,
    paper_white: f32,
    max_nits: f32,
    mode: u32,  // 0 = SDR, 1 = PQ, 2 = EDR, 3 = EDR passthrough
    curve: u32, // 0 = Narkowicz, 1 = Hill, 2 = AgX
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// GPU pipeline for ACES tonemapping.
pub struct TonemapPipeline {
    pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
    /// Tonemap output buffer. Matches Unity's tonemappedOutput RenderTexture.
    /// Separate from the compositor's main buffer so PreTonemapOutput survives.
    pub output: RenderTarget,
}

impl TonemapPipeline {
    pub fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        let format = manifold_gpu::GpuTextureFormat::Rgba16Float;

        let pipeline = device.create_compute_pipeline(
            include_str!("effects/shaders/aces_tonemap_compute.wgsl"),
            "cs_main",
            "Tonemap Native",
        );

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());

        let output = RenderTarget::new(device, width, height, format, "TonemappedOutput");

        Self {
            pipeline,
            sampler,
            output,
        }
    }

    /// Apply ACES tonemapping to the HDR source buffer.
    /// Matches Unity CompositorStack.ApplyTonemap().
    ///
    /// Realtime display uses SDR (mode 0) or EDR (mode 2) depending on
    /// hdr_output_enabled. PQ (mode 1) is reserved for export pipeline.
    pub fn apply(&self, gpu: &mut GpuEncoder, hdr_source: &GpuTexture, settings: &TonemapSettings) {
        // Realtime HDR preview uses EDR passthrough (3) — no ACES compression,
        // linear values passed directly to macOS EDR with soft-clip at display peak.
        let mode = if settings.hdr_output_enabled {
            3u32
        } else {
            0u32
        };

        let uniforms = TonemapUniforms {
            exposure: settings.exposure,
            paper_white: settings.paper_white_nits,
            max_nits: settings.max_display_nits,
            mode,
            curve: settings.curve as u32,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: hdr_source,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.output.texture,
                },
            ],
            [
                self.output.width.div_ceil(16),
                self.output.height.div_ceil(16),
                1,
            ],
            "Tonemap Compute",
        );
    }

    /// Clear the tonemap output to black. Used when no clips are active
    /// to skip the full tonemap + master effect chain (Unity parity:
    /// CompositorStack returns immediately for empty playback).
    pub fn clear(&self, gpu: &mut GpuEncoder) {
        gpu.clear_texture(&self.output.texture, 0.0, 0.0, 0.0, 0.0);
    }

    /// Resize the tonemap output buffer. Matches Unity's lazy reallocation in
    /// ApplyTonemap() when hdrSource dimensions change.
    pub fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        self.output.resize(device, width, height);
    }
}
