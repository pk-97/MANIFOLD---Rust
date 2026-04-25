use parking_lot::RwLock;
use std::sync::Arc;

use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::types::BlendMode;
#[cfg(target_os = "macos")]
use manifold_media::video_renderer::VideoRenderer;
use manifold_playback::engine::{PlaybackEngine, TickResult};
use manifold_renderer::compositor::{CompositeLayerDescriptor, Compositor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::layer_compositor::CompositeClipDescriptor;
use manifold_renderer::tonemap::TonemapSettings;

/// Thread-safe shared output dimensions. The content thread writes new
/// dimensions after resize; the UI thread reads them for aspect ratio.
pub struct SharedOutputView {
    dimensions: RwLock<(u32, u32)>,
}

impl SharedOutputView {
    pub fn new() -> Self {
        Self {
            dimensions: RwLock::new((1920, 1080)),
        }
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
/// Combined with separate Metal command queues (content + UI),
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
    /// Full output dimensions (what the drawable and UI see).
    /// May differ from compositor dimensions when FSR is active.
    output_w: u32,
    output_h: u32,
    /// Direct-present output surface (CAMetalLayer on the output window).
    /// Content thread acquires drawables and presents in its own command buffer.
    /// None when no output window is open.
    #[cfg(target_os = "macos")]
    output_surface: Option<manifold_gpu::GpuSurface>,
    /// When true, skip next_drawable() during display retarget to avoid
    /// blocking the content thread for up to 1s on a transitioning display.
    #[cfg(target_os = "macos")]
    output_present_suspended: bool,
    /// Blit pipeline for output present (passthrough + sampler).
    #[cfg(target_os = "macos")]
    output_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Sampler for output present blit.
    #[cfg(target_os = "macos")]
    output_sampler: Option<manifold_gpu::GpuSampler>,
    /// Triple-buffered IOSurface textures for the workspace preview.
    #[cfg(target_os = "macos")]
    preview_textures: [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// Which preview surface we're writing to THIS frame (0, 1, or 2).
    #[cfg(target_os = "macos")]
    write_surface_index: usize,
    /// IOSurface bridge for the workspace preview path.
    #[cfg(target_os = "macos")]
    preview_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// Last seen preview bridge generation.
    #[cfg(target_os = "macos")]
    preview_generation: u64,
    /// Per-surface signal values — tracks the GpuEvent signal value from the last
    /// frame that wrote to each surface. Before writing to surface S, we wait for
    /// surface_signal_values[S] to complete (the frame that last used it).
    #[cfg(target_os = "macos")]
    surface_signal_values: [u64; crate::shared_texture::SURFACE_COUNT],
    /// Duration of the last GPU fence wait in milliseconds.
    /// Non-zero means the GPU was still working when the content thread woke up.
    /// Exposed unconditionally for the performance overlay.
    last_fence_wait_ms: f64,
    /// Duration of the last GPU poll (wait for completion) in milliseconds.
    /// Captured inside render_content(), read by the profiler.
    #[cfg(feature = "profiling")]
    gpu_poll_ms: f64,
    /// Native Metal GPU device from manifold-gpu (macOS only).
    /// Owns GpuDevice (Metal device + command queue) for native encoding.
    #[cfg(target_os = "macos")]
    native_device: Option<manifold_gpu::GpuDevice>,
    /// Native Metal shared event for frame completion (macOS only).
    #[cfg(target_os = "macos")]
    native_event: Option<manifold_gpu::GpuEvent>,
    /// Kernel-notified GPU fence waiter — replaces busy-spin polling.
    /// Registered before each frame to wake the content thread via condvar
    /// when the GPU finishes with the target surface.
    #[cfg(target_os = "macos")]
    fence_waiter: Option<manifold_gpu::GpuFenceWaiter>,
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
    /// Active live recording session. `Some` while recording, `None` otherwise.
    /// Managed by ContentThread via `set_recording_session` / `take_recording_session`.
    #[cfg(target_os = "macos")]
    pub(crate) recording_session: Option<manifold_recording::LiveRecordingSession>,

    /// Current LED grid dimensions (strip_count, leds_per_strip). Used to size
    /// the per-layer LED composite buffer at native LED resolution. Updated by
    /// ContentThread when the LED controller is initialized; defaults to the
    /// LedSettings defaults when no controller is active.
    led_grid_size: (u32, u32),
}

impl ContentPipeline {
    pub fn new(compositor: Box<dyn Compositor>) -> Self {
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
            output_surface: None,
            #[cfg(target_os = "macos")]
            output_present_suspended: false,
            #[cfg(target_os = "macos")]
            output_pipeline: None,
            #[cfg(target_os = "macos")]
            output_sampler: None,
            #[cfg(target_os = "macos")]
            preview_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            write_surface_index: 0,
            #[cfg(target_os = "macos")]
            preview_bridge: None,
            #[cfg(target_os = "macos")]
            preview_generation: 0,
            #[cfg(target_os = "macos")]
            surface_signal_values: [0; crate::shared_texture::SURFACE_COUNT],
            last_fence_wait_ms: 0.0,
            #[cfg(feature = "profiling")]
            gpu_poll_ms: 0.0,
            #[cfg(target_os = "macos")]
            native_device: None,
            #[cfg(target_os = "macos")]
            native_event: None,
            #[cfg(target_os = "macos")]
            fence_waiter: None,
            #[cfg(target_os = "macos")]
            native_signal_value: 0,
            #[cfg(target_os = "macos")]
            texture_pool: None,
            #[cfg(target_os = "macos")]
            preview_pipeline: None,
            #[cfg(target_os = "macos")]
            preview_sampler: None,
            #[cfg(target_os = "macos")]
            recording_session: None,
            led_grid_size: (
                manifold_led::DEFAULT_STRIP_COUNT,
                manifold_led::DEFAULT_LEDS_PER_STRIP,
            ),
        }
    }

    /// Update the LED grid dimensions used to size the LED composite buffer.
    /// Called by ContentThread when the LED controller is initialized so the
    /// compositor renders the LED path at native LED resolution.
    pub fn set_led_grid_size(&mut self, strip_count: u32, leds_per_strip: u32) {
        self.led_grid_size = (strip_count.max(1), leds_per_strip.max(1));
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
        self.fence_waiter = Some(manifold_gpu::GpuFenceWaiter::new());
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

    /// Duration the content thread blocked waiting for a GPU surface to become
    /// available (ms). Non-zero means the GPU was still processing a frame from
    /// 2 frames ago when the content thread woke up — a sign of GPU saturation.
    pub fn last_fence_wait_ms(&self) -> f64 {
        self.last_fence_wait_ms
    }

    // ── Surface readiness (GPU fence notification) ──────────────────────

    /// Check if the surface is ready (GPU already finished, or no pending work).
    #[cfg(target_os = "macos")]
    pub fn is_surface_ready(&self) -> bool {
        let pending = self.surface_signal_values[self.write_surface_index];
        if pending == 0 {
            return true;
        }
        self.native_event.as_ref().is_none_or(|e| e.is_done(pending))
    }

    /// Register a GPU notification for when the current surface becomes
    /// available. When the GPU signals, `SurfaceReady` is sent through
    /// `cmd_tx` to wake the content thread's `recv()`.
    ///
    /// Returns `true` if a wait is needed (notification registered),
    /// `false` if the surface is already ready.
    #[cfg(target_os = "macos")]
    pub fn register_surface_notify(
        &self,
        cmd_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    ) -> bool {
        let pending = self.surface_signal_values[self.write_surface_index];
        if pending == 0 {
            return false;
        }
        if let (Some(event), Some(waiter)) =
            (&self.native_event, &self.fence_waiter)
        {
            if event.is_done(pending) {
                return false;
            }
            let tx = cmd_tx.clone();
            waiter.register(event, pending, move || {
                let _ = tx.send(
                    crate::content_command::ContentCommand::SurfaceReady,
                );
            });
            true
        } else {
            false
        }
    }

    /// Handle GPU timeout — clear stale signal to prevent infinite blocking.
    #[cfg(target_os = "macos")]
    pub fn handle_surface_timeout(&mut self) {
        let idx = self.write_surface_index;
        let pending = self.surface_signal_values[idx];
        let signaled = self.native_event.as_ref().map_or(0, |e| e.signaled_value());
        log::error!(
            "[ContentPipeline] GPU timeout waiting for surface {} \
             (signal={}, signaled={})",
            idx,
            pending,
            signaled,
        );
        self.surface_signal_values[idx] = 0;
    }

    /// Set the last fence wait duration (called from content thread).
    pub fn set_last_fence_wait_ms(&mut self, ms: f64) {
        self.last_fence_wait_ms = ms;
    }

    /// Current output resolution (post-upscale).
    pub fn output_dimensions(&self) -> (u32, u32) {
        (self.output_w, self.output_h)
    }

    /// Attach an output surface for direct-to-drawable presentation.
    /// Creates the blit pipeline and sampler lazily.
    #[cfg(target_os = "macos")]
    pub fn set_output_surface(&mut self, surface: manifold_gpu::GpuSurface) {
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);
        surface.set_maximum_drawable_count(3);
        surface.set_presents_with_transaction(false);
        if self.output_pipeline.is_none()
            && let Some(ref device) = self.native_device
        {
            let shader = r#"
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
            self.output_pipeline = Some(device.create_render_pipeline(
                shader,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                None,
                "Output Present Blit",
            ));
            self.output_sampler =
                Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                    min_filter: manifold_gpu::GpuFilterMode::Linear,
                    mag_filter: manifold_gpu::GpuFilterMode::Linear,
                    ..Default::default()
                }));
        }
        self.output_surface = Some(surface);
        log::info!("[ContentPipeline] Output surface attached — direct present");
    }

    /// Resize the output surface drawable (fullscreen toggle).
    #[cfg(target_os = "macos")]
    pub fn resize_output_surface(&mut self, width: u32, height: u32) {
        if let Some(ref mut surface) = self.output_surface {
            surface.resize(width, height);
        }
    }

    /// Suspend or resume direct present to the output drawable.
    #[cfg(target_os = "macos")]
    pub fn set_output_present_suspended(&mut self, suspended: bool) {
        self.output_present_suspended = suspended;
    }

    /// Detach the output surface (output window closed).
    #[cfg(target_os = "macos")]
    pub fn clear_output_surface(&mut self) {
        self.output_surface = None;
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
        data_version: u64,
    ) {
        let _t_frame = std::time::Instant::now();

        // Surface wait is now handled by the content thread main loop
        // (wait_for_surface_draining_commands) which keeps processing commands
        // instead of busy-spinning. Export mode waits via wait_for_gpu_idle().
        let _poll_ms = self.last_fence_wait_ms;

        // Extract timing values before split borrow
        let time = engine.current_time();
        let beat = engine.current_beat();
        let time_f64 = engine.current_time_double();
        let beat_f64 = engine.current_beat_f64();

        // === NATIVE METAL PATH ===
        // When manifold-gpu is initialized, use raw Metal encoding.
        // Native Metal encoding path.
        #[cfg(target_os = "macos")]
        if self.native_device.is_some() {
            self.render_content_native(
                gpu,
                engine,
                tick_result,
                dt,
                frame_count,
                time.as_f32(),
                beat.as_f32(),
                time_f64,
                beat_f64,
                _t_frame,
                _poll_ms,
                export_mode,
                data_version,
            );
        }

        // Non-macOS: not yet supported (native Metal path required).
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (
                gpu,
                engine,
                tick_result,
                dt,
                frame_count,
                time,
                beat,
                time_f64,
                beat_f64,
            );
            log::warn!("[ContentPipeline] Non-macOS render path not available");
        }
    }

    /// Native Metal render path.
    ///
    /// Uses manifold_gpu::GpuDevice + GpuEncoder for ALL encoding.
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
        data_version: u64,
    ) {
        let native_device = self.native_device.as_ref().unwrap();
        let texture_pool = self.texture_pool.as_ref();

        // Split borrow: get renderers + project from engine simultaneously.
        let (renderers, project) = engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // ── Generators (separate CB, committed first) ─────────────────
        // Generators must commit before the compositor because the parallel
        // compositor path creates per-layer CBs that are also committed.
        // Metal executes CBs in commit order, so committing generators first
        // guarantees their texture writes are visible to the per-layer CBs.
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
                            &mut gpu_gen,
                            time_f64,
                            beat_f64,
                            dt as f32,
                            layers,
                            data_version,
                        );
                        break;
                    }
                }
            }
            gen_enc.commit();
        }
        let _gen_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // ── Compositor CB (+ direct present, preview, recording) ────
        let mut native_enc = native_device.create_encoder("Compositor");

        // ── Build clip + layer descriptors (CPU only) ────────────────
        let _t0 = std::time::Instant::now();
        let empty_effects: &[EffectInstance] = &[];
        let empty_groups: &[EffectGroup] = &[];

        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());
        for entry in &tick_result.ready_clips {
            let clip_texture = renderers.iter().find_map(|r| {
                if let Some(gen_r) = r.as_any().downcast_ref::<GeneratorRenderer>()
                    && let Some(t) = gen_r.get_clip_texture(&entry.clip_id)
                {
                    return Some(t);
                }
                #[cfg(target_os = "macos")]
                if let Some(vid_r) = r.as_any().downcast_ref::<VideoRenderer>()
                    && let Some(t) = vid_r.get_clip_texture(&entry.clip_id)
                {
                    return Some(t);
                }
                None
            });
            if let Some(texture) = clip_texture {
                let layer = layers.get(entry.layer_index as usize);
                clip_descs.push(CompositeClipDescriptor {
                    clip_id: &entry.clip_id,
                    texture,
                    layer_index: entry.layer_index,
                    blend_mode: layer.map_or(BlendMode::Normal, |l| l.default_blend_mode),
                    opacity: layer.map_or(1.0, |l| l.opacity),
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
                layer_id: &layer.layer_id,
                blend_mode: layer.default_blend_mode,
                opacity: layer.opacity,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                blit_to_led: layer.blit_to_led,
                effects: layer.effects.as_deref().unwrap_or(empty_effects),
                effect_groups: layer.effect_groups.as_deref().unwrap_or(empty_groups),
                parent_layer_id: layer.parent_layer_id.as_ref(),
                is_group: layer.is_group(),
            })
            .collect();

        let master_effects = project.map_or(empty_effects, |p| &p.settings.master_effects);
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
            led_composite_size: self.led_grid_size,
            tonemap: TonemapSettings {
                exposure: 1.0,
                hdr_output_enabled: self.edr_headroom > 1.0,
                paper_white_nits: 200.0,
                max_display_nits: (200.0 * self.edr_headroom as f32).min(10000.0),
                curve: project.map_or(manifold_core::TonemapCurve::AcesNarkowicz, |p| {
                    p.settings.tonemap_curve
                }),
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

            let _compositor_tex = self.compositor.render(&mut gpu_comp, &frame);
        }

        // Upscale (render-res → output-res), direct present, and workspace preview.
        // MetalFX preferred; FSR 1.0 as fallback; direct blit when scale = 1.0.
        // Skipped in export mode (export reads output_texture directly).
        if !export_mode {
            // Resolve the final output texture (post-upscale or raw compositor).
            let final_output: &manifold_gpu::GpuTexture;
            if let Some(ref mfx) = self.metalfx {
                {
                    let mut gpu_upscale = if let Some(pool) = texture_pool {
                        GpuEncoder::with_pool(&mut native_enc, native_device, pool)
                    } else {
                        GpuEncoder::new(&mut native_enc, native_device)
                    };
                    mfx.upscale(&mut gpu_upscale, self.compositor.output_texture(), 0.35);
                }
                final_output = &mfx.output.texture;
            } else if let Some(ref fsr) = self.fsr1 {
                {
                    let mut gpu_fsr = if let Some(pool) = texture_pool {
                        GpuEncoder::with_pool(&mut native_enc, native_device, pool)
                    } else {
                        GpuEncoder::new(&mut native_enc, native_device)
                    };
                    fsr.upscale(&mut gpu_fsr, self.compositor.output_texture(), 0.35);
                }
                final_output = &fsr.output.texture;
            } else {
                final_output = self.compositor.output_texture();
            }

            // ── Direct present to output drawable ───────────────────
            // Acquire drawable, blit final output, schedule present — all in
            // the same command buffer. displaySyncEnabled on the CAMetalLayer
            // handles vsync-aligned delivery. No CVDisplayLink, no IOSurface.
            if let Some(ref surface) = self.output_surface
                && !self.output_present_suspended
                && let Some(ref pipeline) = self.output_pipeline
                && let Some(ref sampler) = self.output_sampler
                && let Some(drawable) = surface.next_drawable()
            {
                let target = drawable.gpu_texture(
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                );
                let draw_w = surface.width as f32;
                let draw_h = surface.height as f32;
                let source_aspect = self.output_w as f32 / self.output_h as f32;
                let draw_aspect = draw_w / draw_h;
                let (fit_w, fit_h) = if source_aspect > draw_aspect {
                    (draw_w, draw_w / source_aspect)
                } else {
                    (draw_h * source_aspect, draw_h)
                };
                let fit_x = (draw_w - fit_w) * 0.5;
                let fit_y = (draw_h - fit_h) * 0.5;
                native_enc.draw_fullscreen_viewport(
                    pipeline,
                    &target,
                    &[
                        manifold_gpu::GpuBinding::Texture {
                            binding: 0,
                            texture: final_output,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 1,
                            sampler,
                        },
                    ],
                    (fit_x, fit_y, fit_w, fit_h),
                    manifold_gpu::GpuLoadAction::Clear,
                    "Output Present",
                );
                native_enc.present_drawable(&drawable);
            }

            // ── Workspace preview (downscaled IOSurface) ────────────
            Self::update_workspace_preview(
                &mut native_enc,
                final_output,
                self.preview_textures[self.write_surface_index].as_ref(),
                self.preview_pipeline.as_ref(),
                self.preview_sampler.as_ref(),
            );
        }

        // ── Live recording capture ──────────────────────────────────
        // Format-convert the upscaled output (Rgba16Float → sRGB Bgra8Unorm)
        // into a recording pool texture. Compute dispatch in the SAME command
        // buffer — the recording thread has zero GPU work.
        let recording_fence = if !export_mode {
            if let Some(ref mut session) = self.recording_session {
                if let Some((tex_idx, pool_slot, fence)) = session.acquire_texture() {
                    let src = if let Some(ref mfx) = self.metalfx {
                        &mfx.output.texture
                    } else if let Some(ref fsr) = self.fsr1 {
                        &fsr.output.texture
                    } else {
                        self.compositor.output_texture()
                    };
                    let dst = session.pool_texture(tex_idx);
                    // Compute dispatch: Rgba16Float → sRGB Bgra8Unorm.
                    // Uses the native GpuEncoder directly (same command buffer).
                    session.encode_format_conversion(&mut native_enc, src, dst);
                    session.submit_frame(pool_slot, fence.clone());
                    Some(fence)
                } else {
                    session.record_dropped_frame();
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Register GPU completion handler to publish the preview front buffer
        // the instant the GPU finishes — decoupled from the content thread's
        // sleep/wake cycle. Output presentation is handled by presentDrawable
        // on the same command buffer (no IOSurface needed).
        if !export_mode {
            let write_idx = self.write_surface_index as u32;
            let preview = self.preview_bridge.clone();
            native_enc.add_completed_handler(move || {
                if let Some(ref b) = preview {
                    b.publish_front(write_idx);
                }
                // Signal recording thread that the GPU blit is complete.
                if let Some(ref fence) = recording_fence {
                    fence.signal();
                }
            });
        }

        // Signal frame completion + commit
        let native_event = self.native_event.as_ref().unwrap();
        native_enc.signal_event(native_event);
        self.native_signal_value = native_event.current_value();
        native_enc.add_completed_handler_with_status("Compositor");
        native_enc.commit();
        let _comp_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // Preview surface tracking — skipped in export mode (no surface cycling).
        if !export_mode {
            self.surface_signal_values[self.write_surface_index] = self.native_signal_value;
            self.write_surface_index =
                (self.write_surface_index + 1) % crate::shared_texture::SURFACE_COUNT;
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
            self.shared_output
                .set_dimensions(self.output_w, self.output_h);
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
        let render_w = ((width as f32) * scale).round().max(1.0) as u32;
        let render_h = ((height as f32) * scale).round().max(1.0) as u32;

        self.output_w = width;
        self.output_h = height;

        #[cfg(target_os = "macos")]
        let native_device = self
            .native_device
            .as_ref()
            .expect("native device required for resize");

        // Compositor renders at render resolution (may be smaller than output).
        #[cfg(target_os = "macos")]
        self.compositor.resize(native_device, render_w, render_h);

        // Resize generator renderer via engine downcast (at render resolution).
        let (renderers, _) = engine.split_renderer_project();
        for renderer in renderers.iter_mut() {
            if let Some(gen_renderer) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
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
            if manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler::is_available(
                native_device,
            ) {
                if let Some(ref mut mfx) = self.metalfx {
                    mfx.resize(native_device, render_w, render_h, width, height);
                } else {
                    self.metalfx =
                        manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler::new(
                            native_device,
                            render_w,
                            render_h,
                            width,
                            height,
                        );
                }
                self.fsr1 = None; // MetalFX takes over
                eprintln!(
                    "[Upscaler] MetalFX Spatial: {}x{} → {}x{} ({:.0}% render scale)",
                    render_w,
                    render_h,
                    width,
                    height,
                    scale * 100.0,
                );
            } else {
                // MetalFX not available — use FSR 1.0.
                self.metalfx = None;
                if let Some(ref mut fsr) = self.fsr1 {
                    fsr.resize(native_device, render_w, render_h, width, height);
                } else {
                    self.fsr1 = Some(manifold_renderer::fsr1::Fsr1Upscaler::new(
                        native_device,
                        render_w,
                        render_h,
                        width,
                        height,
                    ));
                }
                eprintln!(
                    "[Upscaler] FSR 1.0: {}x{} → {}x{} ({:.0}% render scale)",
                    render_w,
                    render_h,
                    width,
                    height,
                    scale * 100.0,
                );
            }
        } else {
            if self.metalfx.is_some() || self.fsr1.is_some() {
                eprintln!(
                    "[Upscaler] Disabled — rendering at native {}x{}",
                    width, height
                );
            }
            self.metalfx = None;
            self.fsr1 = None;
        }

        // Reset preview surface tracking after resolution change.
        #[cfg(target_os = "macos")]
        {
            self.write_surface_index = 0;
            self.surface_signal_values = [0; crate::shared_texture::SURFACE_COUNT];
        }

        // UI thread reads output dimensions.
        self.shared_output.set_dimensions(width, height);
    }

    #[cfg(target_os = "macos")]
    pub fn resize_workspace_preview(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);

        let Some(ref bridge) = self.preview_bridge else {
            return;
        };
        if bridge.width() == width && bridge.height() == height {
            return;
        }

        let native_device = self
            .native_device
            .as_ref()
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
    ///
    /// Uses the fence waiter's kernel notification when available (zero CPU),
    /// falling back to the polling path if the fence waiter isn't initialized.
    #[cfg(target_os = "macos")]
    pub fn wait_for_render_complete(&self) {
        if let Some(ref event) = self.native_event {
            if event.is_done(self.native_signal_value) {
                return;
            }
            // Use kernel notification via fence waiter (zero CPU, zero allocation).
            if let Some(ref waiter) = self.fence_waiter {
                let thread = std::thread::current();
                waiter.register(event, self.native_signal_value, move || {
                    thread.unpark();
                });
                std::thread::park_timeout(std::time::Duration::from_secs(5));
                return;
            }
            // Fallback: polling (should not be reached in normal operation).
            event.wait_until_done(self.native_signal_value);
        }
    }

    /// Export output texture (post-tonemap, post-effects).
    pub fn export_output_texture(&self) -> &manifold_gpu::GpuTexture {
        self.compositor.output_texture()
    }

    /// LED source texture. Returns `Some` only when at least one layer is flagged
    /// `blit_to_led` and has active clips this frame — the LED composite carries
    /// just those layers, post-tonemap + post-master-FX. Returns `None` when no
    /// layer is routed to LEDs; callers should blackout in that case.
    pub fn led_source_texture(&self) -> Option<&manifold_gpu::GpuTexture> {
        self.compositor.led_composite_texture()
    }

    /// Run the PQ encoder on the final compositor output for HDR export.
    /// Returns the PQ-encoded texture.
    /// Lazily creates the PQ encoder pipeline on first call.
    pub fn pq_encode_for_export(
        &mut self,
        paper_white_nits: f32,
        max_nits: f32,
    ) -> &manifold_gpu::GpuTexture {
        let native_device = self
            .native_device
            .as_ref()
            .expect("native device required for PQ encoding");
        let (w, h) = self.compositor.dimensions();

        // Lazy init PQ encoder
        if self.pq_encoder.is_none() {
            self.pq_encoder = Some(manifold_renderer::pq_encoder::PqEncoder::new(
                native_device,
                w,
                h,
            ));
            log::info!("[ContentPipeline] Created PQ encoder {}x{}", w, h);
        }
        let pq = self.pq_encoder.as_ref().unwrap();

        // Resize if needed
        if pq.output.width != w || pq.output.height != h {
            self.pq_encoder
                .as_mut()
                .unwrap()
                .resize(native_device, w, h);
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
}
