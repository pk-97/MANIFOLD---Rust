//! GPU format conversion: linear Rgba16Float → sRGB Bgra8Unorm.
//!
//! Runs in the content thread's command buffer so the recording thread
//! has zero GPU work. Uses the same dispatch pattern as PqEncoder.

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuDevice, GpuTexture};

/// Converts linear Rgba16Float compositor output to sRGB Bgra8Unorm
/// for H.264 recording. Created once per recording session.
pub struct FormatConverter {
    pipeline: GpuComputePipeline,
}

impl FormatConverter {
    pub fn new(device: &GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/linear_to_srgb.wgsl"),
            "cs_main",
            "Recording sRGB Convert",
        );
        Self { pipeline }
    }

    /// Dispatch format conversion. Must be called on the content thread's
    /// encoder (same command buffer as the IOSurface blit).
    pub fn encode(
        &self,
        encoder: &mut manifold_gpu::GpuEncoder,
        source: &GpuTexture,
        dest: &GpuTexture,
    ) {
        encoder.dispatch_compute(
            &self.pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: dest,
                },
            ],
            [dest.width.div_ceil(16), dest.height.div_ceil(16), 1],
            "Recording sRGB Convert",
        );
    }
}
