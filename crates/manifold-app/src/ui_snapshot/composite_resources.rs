//! The one-time GPU resources `crate::ui_frame::composite_main_ui_frame`
//! needs (offscreen target + atlas/blit pipelines) — the live app builds
//! these once at GPU init (`Application::resumed`, app.rs:1840-1922).
//!
//! P2 (`docs/UI_HARNESS_UNIFICATION_DESIGN.md`, D3): every headless caller of
//! the seam needs the identical resources. Extracted here (was a private
//! `CompositeResources` duplicated inside `mod.rs`'s `#[cfg(test)]`
//! `cache_path_full_render` module) so `render.rs`'s `render_ui_to_png` —
//! which is NOT test-only, it's the `cargo xtask ui-snap` production path —
//! can build through the real `UICacheManager` too, without a second copy of
//! this shader/pipeline setup.

use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};
use manifold_renderer::ui_cache_manager::UICacheManager;
use manifold_renderer::ui_renderer::UIRenderer;

use crate::ui_frame::composite_main_ui_frame;
use crate::ui_root::UIRoot;

pub(super) struct CompositeResources {
    pub(super) offscreen: GpuTexture,
    atlas_pipeline: manifold_gpu::GpuRenderPipeline,
    atlas_sampler: manifold_gpu::GpuSampler,
    // `pub(super)`, not private: `render.rs`'s `render_ui_to_png` also needs
    // these to call `crate::ui_frame::render_main_ui_passes` (`inputs.
    // blit_pipeline`/`blit_sampler`, the VQT-blit resources — `None`/absent
    // in the harness, but the seam's signature still requires the plain
    // refs since the live app always has them once GPU is initialized).
    pub(super) blit_pipeline: manifold_gpu::GpuRenderPipeline,
    pub(super) blit_sampler: manifold_gpu::GpuSampler,
}

impl CompositeResources {
    pub(super) fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        // Fullscreen triangle from vertex_index — verbatim from
        // app.rs:1844-1864 (blit) and :1880-1900 (atlas; same shader,
        // separate pipeline object for its distinct blend state).
        let blit_shader = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;
        let blit_pipeline = device.create_render_pipeline(
            blit_shader,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Bgra8Unorm,
            None,
            "Blit Pipeline",
        );
        let blit_sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        });

        let premultiplied_blend = manifold_gpu::GpuBlendState {
            src_factor: manifold_gpu::GpuBlendFactor::One,
            dst_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
            operation: manifold_gpu::GpuBlendOp::Add,
            src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
            dst_alpha_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
            alpha_operation: manifold_gpu::GpuBlendOp::Add,
        };
        let atlas_pipeline = device.create_render_pipeline(
            blit_shader,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Bgra8Unorm,
            Some(premultiplied_blend),
            "Atlas Blit Pipeline",
        );
        let atlas_sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Nearest,
            mag_filter: manifold_gpu::GpuFilterMode::Nearest,
            ..Default::default()
        });

        // Mirrors `Application::resize_ui_offscreen` (app.rs:2905-2920).
        let offscreen = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: GpuTextureFormat::Bgra8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "UI Offscreen (harness)",
            mip_levels: 1,
        });

        Self { offscreen, atlas_pipeline, atlas_sampler, blit_pipeline, blit_sampler }
    }
}

/// Calls the shared seam (`composite_main_ui_frame`) with `video: None` (D8
/// gap #2 — no compositor output in any headless caller). `scale_factor` is
/// threaded through (not hardcoded) so a future Retina (2x) capture run (D8
/// Deferred) is a call-site change, not a rewrite here.
pub(super) fn composite_frame(
    device: &GpuDevice,
    ui_renderer: &mut UIRenderer,
    cache: &mut UICacheManager,
    ui: &mut UIRoot,
    res: &CompositeResources,
    scale_factor: f64,
) {
    composite_main_ui_frame(
        device,
        ui_renderer,
        cache,
        ui,
        &res.offscreen,
        &res.atlas_pipeline,
        &res.atlas_sampler,
        &res.blit_pipeline,
        &res.blit_sampler,
        scale_factor,
        None,
        (0.0, 0.0),
    );
}
