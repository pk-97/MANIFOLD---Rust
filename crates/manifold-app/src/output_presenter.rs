//! Pixel-perfect output presenter — inline blit from the main frame encoder.
//!
//! `OutputBlitter` holds a `GpuSurface` (CAMetalLayer) at project resolution
//! and a compiled WGSL render pipeline. Each frame, `present_all_windows()`
//! checks whether the content thread has published a new IOSurface index and,
//! if so, encodes a fullscreen-triangle blit into the output drawable from
//! within the same `GpuEncoder` used for the workspace window.
//!
//! Architecture (single queue, no contention):
//!   UI Thread GpuDevice (single queue)
//!     frame encoder:
//!       Pass 1-5: workspace rendering
//!       Pass 6:   IOSurface → output drawable (fullscreen triangle)
//!       present_drawable(workspace_drawable)
//!       present_drawable(output_drawable)
//!       commit()  ← single submission
//!
//! Properties:
//! - **drawableSize = project resolution** (always, regardless of window size)
//! - **Fullscreen triangle render pass** (TBDR tile-friendly, not linear blit)
//! - **EDR** — Rgba16Float + extendedLinearSRGB + wantsExtendedDynamicRangeContent
//! - **No dedicated thread** — no CPU overhead, no separate command queue
//! - **Only presents on new content** — no redundant GPU work

// ---------------------------------------------------------------------------
// WGSL blit shader — fullscreen triangle passthrough
// ---------------------------------------------------------------------------

const BLIT_WGSL: &str = r#"
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

// ---------------------------------------------------------------------------
// OutputBlitter — inline output window presenter
// ---------------------------------------------------------------------------

/// Inline output window presenter. No dedicated thread, no separate queue.
///
/// Created by `open_output_window()`, stored on `Application`.
/// Each frame, `present_all_windows()` calls into this struct to blit the
/// latest IOSurface content into the output drawable from the main encoder.
pub struct OutputBlitter {
    /// CAMetalLayer on the output window (project resolution, EDR, vsync).
    pub(crate) surface: manifold_gpu::GpuSurface,
    /// Fullscreen blit pipeline (Rgba16Float output).
    pipeline: manifold_gpu::GpuRenderPipeline,
    sampler: manifold_gpu::GpuSampler,
    /// Last front_index blitted — skip if unchanged (no new content).
    pub(crate) last_front_index: usize,
}

impl OutputBlitter {
    /// Create an `OutputBlitter` for the given window.
    ///
    /// Attaches a `GpuSurface` (CAMetalLayer) at project resolution with EDR,
    /// compiles the WGSL blit pipeline, and initialises the sampler.
    /// The layer uses the same `GpuDevice` as the UI thread so its drawables
    /// can be presented from the main frame encoder's command buffer.
    pub fn new(
        gpu_device: &manifold_gpu::GpuDevice,
        window: &winit::window::Window,
        proj_w: u32,
        proj_h: u32,
    ) -> Self {
        // displaySyncEnabled = false: nextDrawable returns immediately.
        // We only call it when content has changed, so no tearing.
        // Core Animation still presents at vsync.
        let surface = gpu_device.create_surface(
            window,
            proj_w,
            proj_h,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            false,
        );
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);

        let pipeline = gpu_device.create_render_pipeline(
            BLIT_WGSL,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Output Blit Pipeline",
        );

        let sampler = gpu_device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Nearest,
            mag_filter: manifold_gpu::GpuFilterMode::Nearest,
            ..Default::default()
        });

        log::info!(
            "[OutputBlitter] Created: {}x{} Rgba16Float, EDR, inline encoder",
            proj_w, proj_h,
        );

        Self {
            surface,
            pipeline,
            sampler,
            last_front_index: usize::MAX,
        }
    }

    /// Blit `front_index` IOSurface to the output drawable if it has changed.
    ///
    /// Acquires the next drawable, encodes a fullscreen-triangle render pass,
    /// and schedules the drawable for presentation. Returns `true` if a
    /// drawable was presented (caller should call `encoder.present_drawable`).
    pub(crate) fn blit_if_new(
        &mut self,
        front_index: usize,
        compositor_tex: &manifold_gpu::GpuTexture,
        encoder: &mut manifold_gpu::GpuEncoder,
    ) -> Option<manifold_gpu::GpuDrawable> {
        if front_index == self.last_front_index {
            return None;
        }
        self.last_front_index = front_index;

        let drawable = self.surface.next_drawable()?;
        let output_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Rgba16Float);

        encoder.draw_fullscreen(
            &self.pipeline,
            &output_tex,
            &[
                manifold_gpu::GpuBinding::Texture { binding: 0, texture: compositor_tex },
                manifold_gpu::GpuBinding::Sampler { binding: 1, sampler: &self.sampler },
            ],
            true,  // clear to black (letterbox bars)
            true,
            "Output Blit",
        );

        Some(drawable)
    }
}
