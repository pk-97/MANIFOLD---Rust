#![allow(dead_code)]
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
/// Both threads share a single wgpu Device, so TextureViews created by
/// the content thread are directly usable by the UI thread — zero copy.
pub struct SharedOutputView {
    view: RwLock<Option<wgpu::TextureView>>,
    dimensions: RwLock<(u32, u32)>,
}

impl SharedOutputView {
    pub fn new() -> Self {
        Self {
            view: RwLock::new(None),
            dimensions: RwLock::new((1920, 1080)),
        }
    }

    /// Read the current front buffer view (called by UI thread).
    pub fn get_view(&self) -> Option<wgpu::TextureView> {
        self.view.read().clone()
    }

    /// Update the front buffer view (called by content thread after swap).
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
/// IOSurface double-buffering for zero-copy cross-device sharing with the UI thread.
pub struct ContentPipeline {
    compositor: Box<dyn Compositor>,
    /// EDR headroom from the display (1.0 = SDR, e.g. 2.0 = 2x SDR white).
    /// Used to compute max_display_nits for tonemapping.
    pub edr_headroom: f64,
    /// PQ encoder for HDR export. Lazily created on first HDR export frame.
    pq_encoder: Option<manifold_renderer::pq_encoder::PqEncoder>,
    /// Content frame rate tracking (for separate cadence mode).
    content_interval_secs: f64,
    last_content_time: f64,
    /// Shared output view for cross-thread access (fallback for non-macOS).
    shared_output: Arc<SharedOutputView>,
    /// Double-buffered IOSurface textures on the content device (native GpuTexture).
    /// Content writes to shared_textures[write_surface_index]; UI reads the other surface.
    #[cfg(target_os = "macos")]
    shared_textures: [Option<manifold_gpu::GpuTexture>; 2],
    /// Which surface we're writing to THIS frame (0 or 1).
    #[cfg(target_os = "macos")]
    write_surface_index: usize,
    /// IOSurface bridge for cross-device sharing.
    #[cfg(target_os = "macos")]
    shared_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// Last seen bridge generation — used to detect resize and re-import.
    #[cfg(target_os = "macos")]
    shared_generation: u64,
    /// Which IOSurface the PREVIOUS frame wrote to (published after fence ready).
    #[cfg(target_os = "macos")]
    last_write_surface: usize,
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
            content_interval_secs: 1.0 / 60.0,
            last_content_time: 0.0,
            shared_output: shared,
            #[cfg(target_os = "macos")]
            shared_textures: [None, None],
            #[cfg(target_os = "macos")]
            write_surface_index: 0,
            #[cfg(target_os = "macos")]
            shared_bridge: None,
            #[cfg(target_os = "macos")]
            shared_generation: 0,
            #[cfg(target_os = "macos")]
            last_write_surface: 0,
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
        }
    }

    /// Initialize the native Metal GPU device and event.
    /// Called once at startup after the content pipeline is created.
    #[cfg(target_os = "macos")]
    pub fn init_native_gpu(&mut self) {
        let device = manifold_gpu::GpuDevice::new();
        let event = device.create_event();
        self.native_device = Some(device);
        self.native_event = Some(event);
    }

    /// Set a pre-created native GPU device (transfers ownership).
    /// Used when the device must exist before the content pipeline (e.g. for
    /// compositor native pipeline creation).
    #[cfg(target_os = "macos")]
    pub fn set_native_gpu(&mut self, device: manifold_gpu::GpuDevice) {
        let event = device.create_event();
        self.native_device = Some(device);
        self.native_event = Some(event);
    }

    /// Reference to the native GPU device (if initialized).
    #[cfg(target_os = "macos")]
    pub fn native_device(&self) -> Option<&manifold_gpu::GpuDevice> {
        self.native_device.as_ref()
    }

    /// Set the double-buffered IOSurface shared textures (native GpuTexture) and bridge.
    /// Called during init after the bridge imports both textures on the content device.
    #[cfg(target_os = "macos")]
    pub fn set_shared_textures(
        &mut self,
        tex_a: manifold_gpu::GpuTexture,
        tex_b: manifold_gpu::GpuTexture,
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.shared_textures = [Some(tex_a), Some(tex_b)];
        self.shared_bridge = Some(bridge);
    }

    /// Get a clone of the shared output handle. The UI thread holds this
    /// to read the front buffer view and dimensions.
    pub fn shared_output(&self) -> Arc<SharedOutputView> {
        Arc::clone(&self.shared_output)
    }

    /// Wait for the previous frame's GPU work to finish (if pending).
    /// Called at the START of each frame before encoding new work.
    /// Publishes the completed IOSurface so the UI can read it.
    fn wait_previous_frame(&mut self) {
        // ── Native Metal path: GpuEvent sync ────────────────────────────
        // Direct GPU counter read via MTLSharedEvent — microsecond latency.
        if let Some(ref native_event) = self.native_event
            && self.native_signal_value > 0
        {
            while !native_event.is_done(self.native_signal_value) {
                std::thread::yield_now();
            }
            if let Some(ref bridge) = self.shared_bridge {
                bridge.publish_front(self.last_write_surface as u32);
            }
        }
    }

    /// Render all generators and composite, then submit asynchronously.
    ///
    /// Uses native Metal encoding on macOS via manifold-gpu.
    /// IOSurface double-buffering for zero-copy cross-device sharing.
    pub fn render_content(
        &mut self,
        gpu: &manifold_renderer::gpu::GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
    ) {
        let _t_frame = std::time::Instant::now();

        // Wait for PREVIOUS frame's GPU work before encoding this frame.
        // If the GPU finished quickly (likely), this returns immediately.
        let _poll_start = std::time::Instant::now();
        self.wait_previous_frame();
        let _poll_ms = _poll_start.elapsed().as_secs_f64() * 1000.0;

        // Extract timing values before split borrow
        let time = engine.current_time();
        let beat = engine.current_beat();

        // === NATIVE METAL PATH ===
        // When manifold-gpu is initialized, use raw Metal encoding.
        // Zero wgpu on the content hot path — no "(wgpu internal) Signal".
        #[cfg(target_os = "macos")]
        if self.native_device.is_some() {
            self.render_content_native(
                gpu, engine, tick_result, dt, frame_count,
                time, beat, _t_frame, _poll_ms,
            );
        }

        // Non-macOS: not yet supported (native Metal path required).
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (gpu, engine, tick_result, dt, frame_count, time, beat);
            eprintln!("[ContentPipeline] Non-macOS render path not available");
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
        _t_frame: std::time::Instant,
        _poll_ms: f64,
    ) {
        let native_device = self.native_device.as_ref().unwrap();

        // Split borrow: get renderers + project from engine simultaneously.
        let (renderers, project) = engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // ── Create native Metal encoder for ALL GPU work ──────────────
        let _t0 = std::time::Instant::now();
        let mut native_enc = native_device.create_encoder("Frame");

        // Generators render via native encoder (no wgpu encoder needed)
        {
            let mut gpu_gen = GpuEncoder::new(
                &mut native_enc,
                native_device,
            );

            for renderer in renderers.iter_mut() {
                if let Some(gen_renderer) =
                    renderer.as_any_mut().downcast_mut::<GeneratorRenderer>()
                {
                    gen_renderer.render_all(
                        &mut gpu_gen, time, beat, dt as f32, layers,
                    );
                    break;
                }
            }
        }
        let _gen_ms = _t0.elapsed().as_secs_f64() * 1000.0;

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
                    // VideoRenderer still returns &wgpu::Texture — skip for now.
                    // TODO: Port VideoRenderer to manifold-gpu types.
                    let _ = t;
                    return None;
                }
                None
            });
            if let Some(texture) = clip_texture {
                let clip_li = project
                    .and_then(|p| p.timeline.layer_index_for_id(&clip.layer_id))
                    .unwrap_or(0);
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
                    invert_colors: clip.invert_colors,
                    effects: &clip.effects,
                    effect_groups: clip
                        .effect_groups
                        .as_deref()
                        .unwrap_or(&[]),
                });
            }
        }

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
        };
        let _desc_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // ── Compositor (same native encoder) ─────────────────────────
        let _t0 = std::time::Instant::now();
        {
            let mut gpu_comp = GpuEncoder::new(
                &mut native_enc,
                native_device,
            );

            let _compositor_tex =
                self.compositor.render(&mut gpu_comp, &frame);
        }

        // IOSurface copy via native blit
        {
            let (comp_w, comp_h) = self.compositor.dimensions();
            if let Some(ref shared_tex) =
                self.shared_textures[self.write_surface_index]
                && shared_tex.width == comp_w
                && shared_tex.height == comp_h
            {
                // Compositor output is already a native GpuTexture.
                // Copy directly via the native encoder.
                let src = self.compositor.output_texture();
                native_enc.copy_texture_to_texture(
                    src,
                    shared_tex,
                    comp_w,
                    comp_h,
                    1,
                );
            }
        }

        // Signal frame completion + commit
        let native_event = self.native_event.as_ref().unwrap();
        native_enc.signal_event(native_event);
        self.native_signal_value = native_event.current_value();
        native_enc.commit();
        let _comp_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // Surface swap
        {
            self.last_write_surface = self.write_surface_index;
            self.write_surface_index = 1 - self.write_surface_index;
        }

        // Update shared output view for UI thread
        let (comp_w, comp_h) = self.compositor.dimensions();

        // Periodic perf dump
        let _total_ms = _t_frame.elapsed().as_secs_f64() * 1000.0;
        if frame_count > 0 && frame_count.is_multiple_of(60) {
            eprintln!(
                "[PERF/NATIVE] frame={} clips={} res={}x{} | gen={:.1}ms desc={:.1}ms \
                 comp={:.1}ms poll={:.1}ms | total={:.1}ms ({:.0}fps)",
                frame_count,
                tick_result.ready_clips.len(),
                comp_w,
                comp_h,
                _gen_ms,
                _desc_ms,
                _comp_ms,
                _poll_ms,
                _total_ms,
                1000.0 / _total_ms.max(0.001),
            );
        }

        // Update shared dimensions
        let (old_w, old_h) = self.shared_output.get_dimensions();
        if old_w != comp_w || old_h != comp_h {
            self.shared_output.set_dimensions(comp_w, comp_h);
        }

        // GPU profiler (if active): store poll timing
        #[cfg(feature = "profiling")]
        {
            self.gpu_poll_ms = _poll_ms;
        }
    }

    /// Whether it's time for a content frame (for separate cadence mode).
    pub fn should_render_content(&self, realtime_now: f64) -> bool {
        realtime_now - self.last_content_time >= self.content_interval_secs
    }

    /// Mark that a content frame was rendered at the given time.
    pub fn mark_content_rendered(&mut self, realtime_now: f64) {
        self.last_content_time = realtime_now;
    }

    /// Set content rendering frame rate (independent of UI refresh rate).
    #[allow(dead_code)]
    pub fn set_content_fps(&mut self, fps: f64) {
        self.content_interval_secs = 1.0 / fps.max(1.0);
    }

    /// Resize compositor, generators, and IOSurface bridge to new project resolution.
    pub fn resize(&mut self, engine: &mut PlaybackEngine, width: u32, height: u32) {
        #[cfg(target_os = "macos")]
        let native_device = self.native_device.as_ref()
            .expect("native device required for resize");
        #[cfg(target_os = "macos")]
        self.compositor.resize(native_device, width, height);

        // Resize generator renderer via engine downcast
        let (renderers, _) = engine.split_renderer_project();
        for renderer in renderers.iter_mut() {
            if let Some(gen_renderer) =
                renderer.as_any_mut().downcast_mut::<GeneratorRenderer>()
            {
                gen_renderer.resize_gpu(width, height);
                break;
            }
        }

        // Resize IOSurface bridge and re-import both content textures
        #[cfg(target_os = "macos")]
        if let Some(ref bridge) = self.shared_bridge {
            bridge.resize(width, height);
            self.shared_textures = [
                Some(unsafe { bridge.import_texture_native(native_device, 0) }),
                Some(unsafe { bridge.import_texture_native(native_device, 1) }),
            ];
            self.write_surface_index = 0;
            self.shared_generation = bridge.generation();
        }

        // Update shared dimensions for UI thread
        self.shared_output.set_dimensions(width, height);
    }

    /// Get current compositor output dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        self.compositor.dimensions()
    }

    /// Clean up per-owner effect state for stopped clips.
    /// Called after render_content() to release GPU resources for clips
    /// that stopped this tick, preventing unbounded GPU memory growth.
    pub fn cleanup_stopped_clips(&mut self, stopped_clip_ids: &[manifold_core::ClipId]) {
        for clip_id in stopped_clip_ids {
            self.compositor.cleanup_clip_owner(clip_id.as_str());
        }
    }

    /// Pre-tonemap HDR output for export pipeline.
    #[allow(dead_code)]
    pub fn pre_tonemap_output(&self) -> &manifold_gpu::GpuTexture {
        self.compositor.pre_tonemap_output()
    }

    /// Export output texture (post-tonemap, post-effects).
    pub fn export_output_texture(&self) -> &manifold_gpu::GpuTexture {
        self.compositor.output_texture()
    }

    /// Compositor output texture (post-tonemap). Used by LED output.
    pub fn compositor_output_texture(&self) -> &manifold_gpu::GpuTexture {
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
