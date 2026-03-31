use std::sync::Arc;
use parking_lot::RwLock;

use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::types::BlendMode;
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::layer_compositor::CompositeClipDescriptor;
#[cfg(target_os = "macos")]
use manifold_media::video_renderer::VideoRenderer;
use manifold_renderer::tonemap::TonemapSettings;
use manifold_playback::engine::{PlaybackEngine, TickResult};

/// Thread-safe shared output view. The content thread writes a new view
/// after each swap; the UI thread reads it for blitting to screen.
///
/// On macOS the IOSurface path is used exclusively — the wgpu view fields
/// are compiled out. On other platforms the wgpu fallback path is active.
pub struct SharedOutputView {
    #[cfg(not(target_os = "macos"))]
    view: RwLock<Option<wgpu::TextureView>>,
    dimensions: RwLock<(u32, u32)>,
}

impl SharedOutputView {
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_os = "macos"))]
            view: RwLock::new(None),
            dimensions: RwLock::new((1920, 1080)),
        }
    }

    /// Read the current front buffer view (called by UI thread).
    #[cfg(not(target_os = "macos"))]
    pub fn get_view(&self) -> Option<wgpu::TextureView> {
        self.view.read().clone()
    }

    /// Update the front buffer view (called by content thread after swap).
    #[cfg(not(target_os = "macos"))]
    pub fn set_view(&self, view: wgpu::TextureView) {
        *self.view.write() = Some(view);
    }

    /// Update dimensions (called by content thread on resize).
    pub fn set_dimensions(&self, w: u32, h: u32) {
        *self.dimensions.write() = (w, h);
    }

    /// Get current output dimensions (called by UI thread for aspect ratio).
    pub fn get_dimensions(&self) -> (u32, u32) {
        *self.dimensions.read()
    }
}

/// Self-contained content rendering pipeline.
///
/// Owns the compositor and orchestrates GPU rendering of generators + compositing.
/// The PlaybackEngine (which owns GeneratorRenderer) is borrowed for each frame.
///
/// On macOS, uses native Metal encoding via manifold-gpu.
/// IOSurface triple-buffering for zero-copy cross-device sharing with the UI thread.
/// Combined with separate Metal command queues (content=native, UI=wgpu),
/// this allows 2 content frames in flight without starving the UI thread.
pub struct ContentPipeline {
    compositor: Box<dyn Compositor>,
    /// EDR headroom from the display (1.0 = SDR, e.g. 2.0 = 2x SDR white).
    /// Used to compute max_display_nits for tonemapping.
    pub edr_headroom: f64,
    /// PQ encoder for HDR export. Lazily created on first HDR export frame.
    pq_encoder: Option<manifold_renderer::pq_encoder::PqEncoder>,
    /// Shared output view for cross-thread access (fallback for non-macOS).
    shared_output: Arc<SharedOutputView>,
    /// MetalFX Spatial full-frame upscaler. Present only when render_scale < 1.0
    /// and MetalFX is supported (macOS 13+, Apple Silicon). Preferred over FSR.
    #[cfg(target_os = "macos")]
    metalfx: Option<manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler>,
    /// FSR 1.0 spatial upscaler. Present only when render_scale < 1.0
    /// AND MetalFX is not available. Fallback for older hardware.
    #[cfg(target_os = "macos")]
    fsr1: Option<manifold_renderer::fsr1::Fsr1Upscaler>,
    /// Full output dimensions (what the IOSurface and UI see).
    /// May differ from compositor dimensions when FSR is active.
    output_w: u32,
    output_h: u32,
    /// Triple-buffered IOSurface textures on the content device (native GpuTexture).
    /// Content writes to shared_textures[write_surface_index]; UI reads the front surface.
    #[cfg(target_os = "macos")]
    shared_textures: [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// Triple-buffered IOSurface textures for the workspace preview.
    #[cfg(target_os = "macos")]
    preview_textures: [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// Which surface we're writing to THIS frame (0, 1, or 2).
    #[cfg(target_os = "macos")]
    write_surface_index: usize,
    /// IOSurface bridge for cross-device sharing.
    #[cfg(target_os = "macos")]
    shared_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// IOSurface bridge for the workspace preview path.
    #[cfg(target_os = "macos")]
    preview_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// Last seen bridge generation — used to detect resize and re-import.
    #[cfg(target_os = "macos")]
    shared_generation: u64,
    /// Last seen preview bridge generation.
    #[cfg(target_os = "macos")]
    preview_generation: u64,
    /// Per-surface signal values — tracks the GpuEvent signal value from the last
    /// frame that wrote to each surface. Before writing to surface S, we wait for
    /// surface_signal_values[S] to complete (the frame that last used it).
    #[cfg(target_os = "macos")]
    surface_signal_values: [u64; crate::shared_texture::SURFACE_COUNT],
    /// Which IOSurface the PREVIOUS frame wrote to (published after fence ready).
    #[cfg(target_os = "macos")]
    last_write_surface: usize,
    /// Duration of the last GPU fence wait in milliseconds.
    /// Non-zero means the GPU was still working when the content thread woke up.
    /// Exposed unconditionally for the performance overlay.
    last_fence_wait_ms: f64,
    /// Duration of the last GPU poll (wait for completion) in milliseconds.
    /// Captured inside render_content(), read by the profiler.
    #[cfg(feature = "profiling")]
    gpu_poll_ms: f64,
    /// GPU pass-level profiler (timestamp queries). Created on first use.
    #[cfg(feature = "profiling")]
    gpu_profiler: Option<manifold_renderer::gpu_profiler::GpuProfiler>,
    /// GPU pass timing results from the last frame.
    #[cfg(feature = "profiling")]
    last_gpu_pass_results: Vec<manifold_renderer::gpu_profiler::GpuPassTiming>,
    /// Native Metal GPU device from manifold-gpu (macOS only).
    /// Owns metal::Device + metal::CommandQueue for zero-wgpu encoding.
    #[cfg(target_os = "macos")]
    native_device: Option<manifold_gpu::GpuDevice>,
    /// Native Metal shared event for frame completion (macOS only).
    #[cfg(target_os = "macos")]
    native_event: Option<manifold_gpu::GpuEvent>,
    /// Signal value from the native event.
    #[cfg(target_os = "macos")]
    native_signal_value: u64,
    /// Texture pool backed by MTLHeap for zero-kernel-call allocation.
    #[cfg(target_os = "macos")]
    texture_pool: Option<manifold_gpu::TexturePool>,
    /// Downscale blit used for the workspace preview texture.
    #[cfg(target_os = "macos")]
    preview_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Linear sampler for preview downscaling.
    #[cfg(target_os = "macos")]
    preview_sampler: Option<manifold_gpu::GpuSampler>,
}

impl ContentPipeline {
    pub fn new(
        compositor: Box<dyn Compositor>,
    ) -> Self {
        let shared = Arc::new(SharedOutputView::new());
        Self {
            compositor,
            edr_headroom: 1.0,
            pq_encoder: None,
            shared_output: shared,
            #[cfg(target_os = "macos")]
            metalfx: None,
            #[cfg(target_os = "macos")]
            fsr1: None,
            output_w: 1920,
            output_h: 1080,
            #[cfg(target_os = "macos")]
            shared_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            preview_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            write_surface_index: 0,
            #[cfg(target_os = "macos")]
            shared_bridge: None,
            #[cfg(target_os = "macos")]
            preview_bridge: None,
            #[cfg(target_os = "macos")]
            shared_generation: 0,
            #[cfg(target_os = "macos")]
            preview_generation: 0,
            #[cfg(target_os = "macos")]
            surface_signal_values: [0; crate::shared_texture::SURFACE_COUNT],
            #[cfg(target_os = "macos")]
            last_write_surface: 0,
            last_fence_wait_ms: 0.0,
            #[cfg(feature = "profiling")]
            gpu_poll_ms: 0.0,
            #[cfg(feature = "profiling")]
            gpu_profiler: None,
            #[cfg(feature = "profiling")]
            last_gpu_pass_results: Vec::new(),
            #[cfg(target_os = "macos")]
            native_device: None,
            #[cfg(target_os = "macos")]
            native_event: None,
            #[cfg(target_os = "macos")]
            native_signal_value: 0,
            #[cfg(target_os = "macos")]
            texture_pool: None,
            #[cfg(target_os = "macos")]
            preview_pipeline: None,
            #[cfg(target_os = "macos")]
            preview_sampler: None,
        }
    }

    /// Initialize the native Metal GPU device, event, and texture pool.
    /// Called once at startup after the content pipeline is created.
    #[cfg(target_os = "macos")]
    /// Set a pre-created native GPU device (transfers ownership).
    /// Used when the device must exist before the content pipeline (e.g. for
    /// compositor native pipeline creation).
    #[cfg(target_os = "macos")]
    pub fn set_native_gpu(&mut self, device: manifold_gpu::GpuDevice) {
        let event = device.create_event();
        // 3 frames in flight (triple buffering).
        let pool = device.create_texture_pool(3);
        let preview_shader = r#"
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
        self.preview_pipeline = Some(device.create_render_pipeline(
            preview_shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Workspace Preview Blit",
        ));
        self.preview_sampler = Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        }));
        self.native_device = Some(device);
        self.native_event = Some(event);
        self.texture_pool = Some(pool);
    }

    /// Reference to the native GPU device (if initialized).
    #[cfg(target_os = "macos")]
    pub fn native_device(&self) -> Option<&manifold_gpu::GpuDevice> {
        self.native_device.as_ref()
    }

    /// Raw Metal device pointer for FFI interop (encoder sharing).
    #[cfg(target_os = "macos")]
    pub fn native_device_ptr(&self) -> Option<*mut std::ffi::c_void> {
        self.native_device.as_ref().map(|d| d.raw_device_ptr())
    }

    /// Set the triple-buffered IOSurface shared textures (native GpuTexture) and bridge.
    /// Called during init after the bridge imports all textures on the content device.
    #[cfg(target_os = "macos")]
    pub fn set_shared_textures(
        &mut self,
        textures: [manifold_gpu::GpuTexture; crate::shared_texture::SURFACE_COUNT],
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.shared_textures = textures.map(Some);
        self.shared_bridge = Some(bridge);
    }

    #[cfg(target_os = "macos")]
    pub fn set_preview_textures(
        &mut self,
        textures: [manifold_gpu::GpuTexture; crate::shared_texture::SURFACE_COUNT],
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.preview_textures = textures.map(Some);
        self.preview_bridge = Some(bridge);
    }

    /// Get a clone of the shared output handle. The UI thread holds this
    /// to read the front buffer view and dimensions.
    pub fn shared_output(&self) -> Arc<SharedOutputView> {
        Arc::clone(&self.shared_output)
    }

    /// Wait for the surface we're about to write to, if it has pending GPU work.
    /// Called at the START of each frame before encoding new work.
    /// Also publishes the most recently completed surface so the UI can read it.
    fn wait_for_surface(&mut self) {
        // Publish the last completed frame (if any) so UI can read it.
        if let Some(ref native_event) = self.native_event
            && self.native_signal_value > 0
        {
            // Wait for the PREVIOUS frame to finish (the one we just submitted).
            // Timeout after 5s — if GPU is hung, skip the frame rather than deadlock.
            if !native_event.wait_until_done_timeout(self.native_signal_value, 5000)
            {
                log::error!(
                    "[ContentPipeline] GPU timeout waiting for previous frame \
                     (signal={}, signaled={})",
                    self.native_signal_value,
                    native_event.signaled_value(),
                );
                return;
            }
            if let Some(ref bridge) = self.shared_bridge {
                bridge.publish_front(self.last_write_surface as u32);
            }
            if let Some(ref bridge) = self.preview_bridge {
                bridge.publish_front(self.last_write_surface as u32);
            }
        }

        // Wait for the surface we're about to write to — it may still have
        // GPU work from 2 frames ago (triple buffering: surface reuse every 3 frames).
        #[cfg(target_os = "macos")]
        if let Some(ref native_event) = self.native_event {
            let pending = self.surface_signal_values[self.write_surface_index];
            if pending > 0
                && !native_event.wait_until_done_timeout(pending, 5000)
            {
                log::error!(
                    "[ContentPipeline] GPU timeout waiting for surface {} \
                     (signal={}, signaled={})",
                    self.write_surface_index,
                    pending,
                    native_event.signaled_value(),
                );
            }
        }
    }

    /// Render all generators and composite, then submit asynchronously.
    ///
    /// Uses native Metal encoding on macOS via manifold-gpu.
    /// IOSurface double-buffering for zero-copy cross-device sharing.
    ///
    /// When `export_mode` is true, skips IOSurface wait/blit/swap — the export
    /// pipeline reads directly from `export_output_texture()` and doesn't need
    /// the cross-device surface bridge.
    pub fn render_content(
        &mut self,
        gpu: &manifold_renderer::gpu::GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
        export_mode: bool,
    ) {
        let _t_frame = std::time::Instant::now();

        // Wait for the surface we're about to write to (may have pending GPU work
        // from 2 frames ago with triple buffering). Also publishes the last completed frame.
        // Skipped in export mode — export reads output_texture directly, no IOSurface needed.
        let fence_wait_start = std::time::Instant::now();
        if !export_mode {
            self.wait_for_surface();
        }
        self.last_fence_wait_ms = fence_wait_start.elapsed().as_secs_f64() * 1000.0;
        let _poll_ms = self.last_fence_wait_ms;

        // Extract timing values before split borrow
        let time = engine.current_time();
        let beat = engine.current_beat();
        let time_f64 = engine.current_time_double();
        let beat_f64 = engine.current_beat_f64();

        // === NATIVE METAL PATH ===
        // When manifold-gpu is initialized, use raw Metal encoding.
        // Zero wgpu on the content hot path — no "(wgpu internal) Signal".
        #[cfg(target_os = "macos")]
        if self.native_device.is_some() {
            self.render_content_native(
                gpu, engine, tick_result, dt, frame_count,
                time.as_f32(), beat.as_f32(), time_f64, beat_f64,
                _t_frame, _poll_ms, export_mode,
            );
        }

        // Non-macOS: not yet supported (native Metal path required).
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (gpu, engine, tick_result, dt, frame_count, time, beat, time_f64, beat_f64);
            log::warn!("[ContentPipeline] Non-macOS render path not available");
        }
    }

    /// Native Metal render path — zero wgpu on the content hot path.
    ///
    /// Uses manifold_gpu::GpuDevice + GpuEncoder for ALL encoding.
    /// No wgpu::Queue::submit(), no wgpu::CommandEncoder, no "(wgpu internal) Signal".
    /// Generators/effects dispatch through the native encoder via GpuEncoder wrapper.
    #[cfg(target_os = "macos")]
    #[allow(clippy::too_many_arguments)]
    fn render_content_native(
        &mut self,
        _gpu: &manifold_renderer::gpu::GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
        time: f32,
        beat: f32,
        time_f64: f64,
        beat_f64: f64,
        _t_frame: std::time::Instant,
        _poll_ms: f64,
        export_mode: bool,
    ) {
        let native_device = self.native_device.as_ref().unwrap();
        let texture_pool = self.texture_pool.as_ref();

        // Split borrow: get renderers + project from engine simultaneously.
        let (renderers, project) = engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // ── Generator rendering (separate command buffer) ─────────────
        // Generators get their own CB that is committed BEFORE the compositor.
        // This guarantees generator texture writes are visible to the compositor's
        // per-layer command buffers in the parallel dispatch path. Metal executes
        // CBs in commit order, so committing gen_enc first ensures generators
        // complete before per-layer effects read their textures.
        let _t0 = std::time::Instant::now();

        // Advance the pool's frame counter — drives frame-stamped recycling.
        // Prune stale textures every 300 frames (~5s at 60fps) to free GPU memory
        // after resolution changes or project switches.
        if let Some(pool) = texture_pool {
            pool.begin_frame();
            if pool.current_frame() % 300 == 0 {
                pool.prune_stale(300);
            }
        }

        {
            let mut gen_enc = native_device.create_encoder("Generators");
            {
                let mut gpu_gen = if let Some(pool) = texture_pool {
                    GpuEncoder::with_pool(&mut gen_enc, native_device, pool)
                } else {
                    GpuEncoder::new(&mut gen_enc, native_device)
                };

                for renderer in renderers.iter_mut() {
                    if let Some(gen_renderer) =
                        renderer.as_any_mut().downcast_mut::<GeneratorRenderer>()
                    {
                        // Sync upscale mode from project settings (per-frame, zero-cost read).
                        if let Some(p) = project {
                            use manifold_core::types::UpscaleMode;
                            match p.settings.upscale_mode {
                                UpscaleMode::Native => {
                                    gen_renderer.set_scaling_enabled(false);
                                }
                                UpscaleMode::MetalFxSpatial => {
                                    gen_renderer.set_scaling_enabled(true);
                                    gen_renderer.set_upscale_mode(
                                        manifold_gpu::metalfx::UpscaleMode::MetalFxSpatial,
                                    );
                                }
                                UpscaleMode::MpsLanczos => {
                                    gen_renderer.set_scaling_enabled(true);
                                    gen_renderer.set_upscale_mode(
                                        manifold_gpu::metalfx::UpscaleMode::MpsLanczos,
                                    );
                                }
                            }
                        }
                        gen_renderer.render_all(
                            &mut gpu_gen, time_f64, beat_f64, dt as f32, layers,
                        );
                        break;
                    }
                }
            }
            gen_enc.commit();
        }
        let _gen_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // ── Create compositor command buffer ─────────────────────────
        let mut native_enc = native_device.create_encoder("Compositor");

        // ── Build clip + layer descriptors (CPU only) ────────────────
        let _t0 = std::time::Instant::now();
        let empty_effects: &[EffectInstance] = &[];
        let empty_groups: &[EffectGroup] = &[];

        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());
        for clip in &tick_result.ready_clips {
            let clip_texture = renderers.iter().find_map(|r| {
                if let Some(gen_r) =
                    r.as_any().downcast_ref::<GeneratorRenderer>()
                    && let Some(t) = gen_r.get_clip_texture(&clip.id)
                {
                    return Some(t);
                }
                #[cfg(target_os = "macos")]
                if let Some(vid_r) =
                    r.as_any().downcast_ref::<VideoRenderer>()
                    && let Some(t) = vid_r.get_clip_texture(&clip.id)
                {
                    return Some(t);
                }
                None
            });
            if let Some(texture) = clip_texture {
                let clip_li = layers.iter().position(|l| {
                    l.clips.iter().any(|c| c.id == clip.id)
                }).unwrap_or(0);
                let layer = layers.get(clip_li);
                clip_descs.push(CompositeClipDescriptor {
                    clip_id: &clip.id,
                    texture,
                    layer_index: clip_li as i32,
                    blend_mode: layer
                        .map_or(BlendMode::Normal, |l| l.default_blend_mode),
                    opacity: layer.map_or(1.0, |l| l.opacity),
                    translate_x: clip.translate_x,
                    translate_y: clip.translate_y,
                    scale: clip.scale,
                    rotation: clip.rotation,
                    effects: &[],
                    effect_groups: &[],
                });
            }
        }


        // Sort clips descending by layer_index: higher index = bottom of timeline = rendered first
        // as base layer. This ordering is required by generate_layers' consecutive-run grouping.
        clip_descs.sort_unstable_by(|a, b| b.layer_index.cmp(&a.layer_index));

        let layer_descs: Vec<CompositeLayerDescriptor> = layers
            .iter()
            .map(|layer| CompositeLayerDescriptor {
                layer_index: layer.index,
                layer_id: layer.layer_id.clone(),
                blend_mode: layer.default_blend_mode,
                opacity: layer.opacity,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                effects: layer.effects.as_deref().unwrap_or(empty_effects),
                effect_groups: layer
                    .effect_groups
                    .as_deref()
                    .unwrap_or(empty_groups),
            })
            .collect();

        let master_effects =
            project.map_or(empty_effects, |p| &p.settings.master_effects);
        let master_effect_groups = project
            .and_then(|p| p.settings.master_effect_groups.as_deref())
            .unwrap_or(empty_groups);
        let led_exit_index = project.map_or(-1, |p| p.settings.led_exit_index);

        let frame = CompositorFrame {
            time,
            beat,
            dt: dt as f32,
            frame_count,
            compositor_dirty: tick_result.compositor_dirty,
            clips: &clip_descs,
            layers: &layer_descs,
            master_effects,
            master_effect_groups,
            led_exit_index,
            tonemap: TonemapSettings {
                exposure: 1.0,
                hdr_output_enabled: self.edr_headroom > 1.0,
                paper_white_nits: 200.0,
                max_display_nits: (200.0 * self.edr_headroom as f32).min(10000.0),
            },
            output_width: self.output_w,
            output_height: self.output_h,
        };
        let _desc_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // ── Compositor (same native encoder) ─────────────────────────
        let _t0 = std::time::Instant::now();
        {
            let mut gpu_comp = if let Some(pool) = texture_pool {
                GpuEncoder::with_pool(&mut native_enc, native_device, pool)
            } else {
                GpuEncoder::new(&mut native_enc, native_device)
            };

            let _compositor_tex =
                self.compositor.render(&mut gpu_comp, &frame);
        }

        // Upscale (render-res → output-res) + IOSurface copy.
        // MetalFX preferred; FSR 1.0 as fallback; direct blit when scale = 1.0.
        // Skipped in export mode (export reads output_texture directly).
        if !export_mode {
            let (comp_w, comp_h) = self.compositor.dimensions();

            if let Some(ref mfx) = self.metalfx {
                // MetalFX Spatial: ML-based upscale directly on the command buffer.
                {
                    let mut gpu_upscale = if let Some(pool) = texture_pool {
                        GpuEncoder::with_pool(&mut native_enc, native_device, pool)
                    } else {
                        GpuEncoder::new(&mut native_enc, native_device)
                    };
                    // RCAS sharpness: lower = sharper. AMD default 0.547.
                    // 0.35 ≈ exp2(-1.5) — crisper without ringing.
                    mfx.upscale(&mut gpu_upscale, self.compositor.output_texture(), 0.35);
                }
                if let Some(ref shared_tex) =
                    self.shared_textures[self.write_surface_index]
                    && shared_tex.width == self.output_w
                    && shared_tex.height == self.output_h
                {
                    native_enc.copy_texture_to_texture(
                        &mfx.output.texture, shared_tex,
                        self.output_w, self.output_h, 1,
                    );
                }
                Self::update_workspace_preview(
                    &mut native_enc,
                    &mfx.output.texture,
                    self.preview_textures[self.write_surface_index].as_ref(),
                    self.preview_pipeline.as_ref(),
                    self.preview_sampler.as_ref(),
                );
            } else if let Some(ref fsr) = self.fsr1 {
                // FSR 1.0 fallback: EASU + RCAS compute dispatches.
                {
                    let mut gpu_fsr = if let Some(pool) = texture_pool {
                        GpuEncoder::with_pool(&mut native_enc, native_device, pool)
                    } else {
                        GpuEncoder::new(&mut native_enc, native_device)
                    };
                    // RCAS sharpness: lower = sharper. AMD default 0.547.
                    // 0.35 ≈ exp2(-1.5) — crisper without ringing.
                    fsr.upscale(&mut gpu_fsr, self.compositor.output_texture(), 0.35);
                }
                if let Some(ref shared_tex) =
                    self.shared_textures[self.write_surface_index]
                    && shared_tex.width == self.output_w
                    && shared_tex.height == self.output_h
                {
                    native_enc.copy_texture_to_texture(
                        &fsr.output.texture, shared_tex,
                        self.output_w, self.output_h, 1,
                    );
                }
                Self::update_workspace_preview(
                    &mut native_enc,
                    &fsr.output.texture,
                    self.preview_textures[self.write_surface_index].as_ref(),
                    self.preview_pipeline.as_ref(),
                    self.preview_sampler.as_ref(),
                );
            } else {
                // No upscaling: blit compositor output directly to IOSurface.
                if let Some(ref shared_tex) =
                    self.shared_textures[self.write_surface_index]
                    && shared_tex.width == comp_w
                    && shared_tex.height == comp_h
                {
                    native_enc.copy_texture_to_texture(
                        self.compositor.output_texture(), shared_tex, comp_w, comp_h, 1,
                    );
                }
                Self::update_workspace_preview(
                    &mut native_enc,
                    self.compositor.output_texture(),
                    self.preview_textures[self.write_surface_index].as_ref(),
                    self.preview_pipeline.as_ref(),
                    self.preview_sampler.as_ref(),
                );
            }
        }

        // Signal frame completion + commit
        let native_event = self.native_event.as_ref().unwrap();
        native_enc.signal_event(native_event);
        self.native_signal_value = native_event.current_value();
        native_enc.commit();
        let _comp_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // Surface tracking — skipped in export mode (no surface cycling needed).
        if !export_mode {
            self.surface_signal_values[self.write_surface_index] =
                self.native_signal_value;
            self.last_write_surface = self.write_surface_index;
            self.write_surface_index = (self.write_surface_index + 1)
                % crate::shared_texture::SURFACE_COUNT;
        }

        // Update shared output view for UI thread
        let (comp_w, comp_h) = self.compositor.dimensions();
        let _ = (comp_w, comp_h); // used in profiling block below; suppress lint in non-profiling builds

        // Periodic perf dump (profiling builds only)
        #[cfg(feature = "profiling")]
        {
            let _total_ms = _t_frame.elapsed().as_secs_f64() * 1000.0;
            if frame_count > 0 && frame_count.is_multiple_of(60) {
                log::warn!(
                    "[PERF/NATIVE] frame={} clips={} render={}x{} out={}x{} | gen={:.1}ms desc={:.1}ms \
                     comp={:.1}ms poll={:.1}ms | total={:.1}ms ({:.0}fps)",
                    frame_count,
                    tick_result.ready_clips.len(),
                    comp_w,
                    comp_h,
                    self.output_w,
                    self.output_h,
                    _gen_ms,
                    _desc_ms,
                    _comp_ms,
                    _poll_ms,
                    _total_ms,
                    1000.0 / _total_ms.max(0.001),
                );
            }
        }

        // Update shared dimensions (always output dims, not render dims).
        let (old_w, old_h) = self.shared_output.get_dimensions();
        if old_w != self.output_w || old_h != self.output_h {
            self.shared_output.set_dimensions(self.output_w, self.output_h);
        }

        // GPU profiler (if active): store poll timing
        #[cfg(feature = "profiling")]
        {
            self.gpu_poll_ms = _poll_ms;
        }
    }

    /// Resize compositor, generators, and IOSurface bridge.
    ///
    /// `width` / `height` are the **output** dimensions (what the UI and IOSurface see).
    /// `render_scale` ∈ (0, 1] controls the internal render resolution:
    ///   - 1.0 → render at output resolution, upscaling disabled.
    ///   - 0.75 / 0.5 → render at 75% / 50%, MetalFX Spatial upscales back to output
    ///     (FSR 1.0 used as fallback if MetalFX is unavailable).
    pub fn resize(
        &mut self,
        engine: &mut PlaybackEngine,
        width: u32,
        height: u32,
        render_scale: f32,
    ) {
        let scale = render_scale.clamp(0.25, 1.0);
        let render_w = ((width  as f32) * scale).round().max(1.0) as u32;
        let render_h = ((height as f32) * scale).round().max(1.0) as u32;

        self.output_w = width;
        self.output_h = height;

        #[cfg(target_os = "macos")]
        let native_device = self.native_device.as_ref()
            .expect("native device required for resize");

        // Compositor renders at render resolution (may be smaller than output).
        #[cfg(target_os = "macos")]
        self.compositor.resize(native_device, render_w, render_h);

        // Resize generator renderer via engine downcast (at render resolution).
        let (renderers, _) = engine.split_renderer_project();
        for renderer in renderers.iter_mut() {
            if let Some(gen_renderer) =
                renderer.as_any_mut().downcast_mut::<GeneratorRenderer>()
            {
                gen_renderer.resize_gpu(render_w, render_h, width, height);
                break;
            }
        }

        // Init / resize upscaler when render_scale < 1.0.
        // Prefer MetalFX Spatial (ML-based, faster, better quality on Apple Silicon).
        // Fall back to FSR 1.0 if MetalFX is unavailable (older hardware).
        #[cfg(target_os = "macos")]
        if scale < 1.0 {
            // Try MetalFX first.
            if manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler::is_available(native_device) {
                if let Some(ref mut mfx) = self.metalfx {
                    mfx.resize(native_device, render_w, render_h, width, height);
                } else {
                    self.metalfx = manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler::new(
                        native_device, render_w, render_h, width, height,
                    );
                }
                self.fsr1 = None; // MetalFX takes over
                eprintln!(
                    "[Upscaler] MetalFX Spatial: {}x{} → {}x{} ({:.0}% render scale)",
                    render_w, render_h, width, height, scale * 100.0,
                );
            } else {
                // MetalFX not available — use FSR 1.0.
                self.metalfx = None;
                if let Some(ref mut fsr) = self.fsr1 {
                    fsr.resize(native_device, render_w, render_h, width, height);
                } else {
                    self.fsr1 = Some(manifold_renderer::fsr1::Fsr1Upscaler::new(
                        native_device, render_w, render_h, width, height,
                    ));
                }
                eprintln!(
                    "[Upscaler] FSR 1.0: {}x{} → {}x{} ({:.0}% render scale)",
                    render_w, render_h, width, height, scale * 100.0,
                );
            }
        } else {
            if self.metalfx.is_some() || self.fsr1.is_some() {
                eprintln!("[Upscaler] Disabled — rendering at native {}x{}", width, height);
            }
            self.metalfx = None;
            self.fsr1 = None;
        }

        // IOSurface bridge always at output resolution.
        #[cfg(target_os = "macos")]
        if let Some(ref bridge) = self.shared_bridge {
            bridge.resize(width, height);
            self.shared_textures = std::array::from_fn(|i| {
                Some(unsafe { bridge.import_texture_native(native_device, i) })
            });
            self.write_surface_index = 0;
            self.surface_signal_values = [0; crate::shared_texture::SURFACE_COUNT];
            self.shared_generation = bridge.generation();
        }

        // UI thread reads output dimensions.
        self.shared_output.set_dimensions(width, height);
    }

    #[cfg(target_os = "macos")]
    pub fn resize_workspace_preview(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);

        let Some(ref bridge) = self.preview_bridge else { return };
        if bridge.width() == width && bridge.height() == height {
            return;
        }

        let native_device = self.native_device.as_ref()
            .expect("native device required for workspace preview resize");
        bridge.resize(width, height);
        self.preview_textures = std::array::from_fn(|i| {
            Some(unsafe { bridge.import_texture_native(native_device, i) })
        });
        self.preview_generation = bridge.generation();
    }

    /// Get current output dimensions (= IOSurface / UI dimensions).
    /// When FSR is active these differ from `self.compositor.dimensions()`.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.output_w, self.output_h)
    }

    #[cfg(target_os = "macos")]
    fn update_workspace_preview(
        native_enc: &mut manifold_gpu::GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: Option<&manifold_gpu::GpuTexture>,
        pipeline: Option<&manifold_gpu::GpuRenderPipeline>,
        sampler: Option<&manifold_gpu::GpuSampler>,
    ) {
        let Some(target) = target else {
            return;
        };

        if target.width == source.width && target.height == source.height {
            native_enc.copy_texture_to_texture(source, target, target.width, target.height, 1);
            return;
        }

        let (Some(pipeline), Some(sampler)) = (pipeline, sampler) else {
            return;
        };

        native_enc.draw_fullscreen(
            pipeline,
            target,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
            ],
            true,
            true,
            "Workspace Preview Blit",
        );
    }

    /// Clean up per-owner effect state for stopped clips.
    /// Called after render_content() to release GPU resources for clips
    /// that stopped this tick, preventing unbounded GPU memory growth.
    pub fn cleanup_stopped_clips(&mut self, stopped_clip_ids: &[manifold_core::ClipId]) {
        for clip_id in stopped_clip_ids {
            self.compositor.cleanup_clip_owner(clip_id.as_str());
        }
    }

    /// Clear all temporal effect state (feedback textures, bloom state, etc.).
    /// Called on project load to prevent stale GPU state from bleeding across projects.
    pub fn clear_all_effect_state(&mut self) {
        self.compositor.clear_all_effect_state();
    }

    /// Block until all in-flight background work in effect processors completes.
    /// Called after each export frame so async pipelines (GPU readback → CPU worker
    /// → result) resolve deterministically before the frame is encoded.
    /// Affected effects: BlobTracking, WireframeDepth, DepthOfField (depth mode).
    pub fn flush_all_background_work(&mut self) {
        self.compositor.flush_all_background_work();
    }

    /// Block until the last render's GPU command buffer has completed.
    /// Must be called before reading the output texture on a different queue.
    #[cfg(target_os = "macos")]
    pub fn wait_for_render_complete(&self) {
        if let Some(ref event) = self.native_event {
            event.wait_until_done(self.native_signal_value);
        }
    }

    /// Export output texture (post-tonemap, post-effects).
    pub fn export_output_texture(&self) -> &manifold_gpu::GpuTexture {
        self.compositor.output_texture()
    }

    /// LED tap texture: pre-tonemap composite captured when led_exit_index == 0.
    /// Returns the tap if available, otherwise falls back to the final output.
    pub fn led_source_texture(&self) -> &manifold_gpu::GpuTexture {
        self.compositor.led_tap_texture()
            .unwrap_or_else(|| self.compositor.output_texture())
    }

    /// Run the PQ encoder on the final compositor output for HDR export.
    /// Returns the PQ-encoded texture.
    /// Lazily creates the PQ encoder pipeline on first call.
    pub fn pq_encode_for_export(
        &mut self,
        paper_white_nits: f32,
        max_nits: f32,
    ) -> &manifold_gpu::GpuTexture {
        let native_device = self.native_device.as_ref()
            .expect("native device required for PQ encoding");
        let (w, h) = self.compositor.dimensions();

        // Lazy init PQ encoder
        if self.pq_encoder.is_none() {
            self.pq_encoder =
                Some(manifold_renderer::pq_encoder::PqEncoder::new(
                    native_device, w, h,
                ));
            log::info!("[ContentPipeline] Created PQ encoder {}x{}", w, h);
        }
        let pq = self.pq_encoder.as_ref().unwrap();

        // Resize if needed
        if pq.output.width != w || pq.output.height != h {
            self.pq_encoder.as_mut().unwrap().resize(native_device, w, h);
        }

        // Encode: take the final compositor output (post-tonemap, post-effects)
        // and apply the ST.2084 PQ transfer function.
        let source = self.compositor.output_texture();
        let mut enc = native_device.create_encoder("PQ Encode");
        {
            let mut gpu_enc = GpuEncoder::new(&mut enc, native_device);
            self.pq_encoder.as_ref().unwrap().encode(
                &mut gpu_enc,
                source,
                paper_white_nits,
                max_nits,
            );
        }
        // Signal the same event so wait_for_render_complete covers PQ output.
        if let Some(ref event) = self.native_event {
            enc.signal_event(event);
            self.native_signal_value = event.current_value();
        }
        enc.commit();

        &self.pq_encoder.as_ref().unwrap().output.texture
    }

    /// Duration of the last GPU poll (wait for completion) in milliseconds.
    /// Only available with the `profiling` feature.
    #[cfg(feature = "profiling")]
    pub fn last_gpu_poll_ms(&self) -> f64 {
        self.gpu_poll_ms
    }

    /// Per-pass GPU timing results from the last frame.
    /// Only available with the `profiling` feature.
    #[cfg(feature = "profiling")]
    pub fn last_gpu_pass_results(
        &self,
    ) -> &[manifold_renderer::gpu_profiler::GpuPassTiming] {
        &self.last_gpu_pass_results
    }

    /// GPU adapter name from the profiler. Returns "Unknown" if profiler not available.
    #[cfg(feature = "profiling")]
    pub fn gpu_adapter_name(&self) -> &str {
        self.gpu_profiler
            .as_ref()
            .map_or("Unknown", |p| p.adapter_name())
    }

    /// Profiler buffer readback overhead in ms.
    #[cfg(feature = "profiling")]
    pub fn profiler_overhead_ms(&self) -> f64 {
        self.gpu_profiler
            .as_ref()
            .map_or(0.0, |p| p.last_readback_overhead_ms())
    }
}
