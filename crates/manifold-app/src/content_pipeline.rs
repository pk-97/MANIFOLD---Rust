#![allow(dead_code)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::RwLock;

use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_core::types::BlendMode;
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::layer_compositor::CompositeClipDescriptor;
#[cfg(target_os = "macos")]
use manifold_media::video_renderer::VideoRenderer;
#[cfg(not(target_os = "macos"))]
use manifold_renderer::render_target::RenderTarget;
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

/// Output format for double-buffered compositor output (non-macOS fallback).
/// Matches compositor's tonemap output format.
#[cfg(not(target_os = "macos"))]
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Self-contained content rendering pipeline.
///
/// Owns the compositor and orchestrates GPU rendering of generators + compositing.
/// The PlaybackEngine (which owns GeneratorRenderer) is borrowed for each frame.
///
/// Double-buffered output: content writes to back buffer, swaps on completion.
/// UI always reads from the stable front buffer via SharedOutputView (zero copy —
/// both threads share the same wgpu Device).
pub struct ContentPipeline {
    compositor: Box<dyn Compositor>,
    /// EDR headroom from the display (1.0 = SDR, e.g. 2.0 = 2x SDR white).
    /// Used to compute max_display_nits for tonemapping.
    pub edr_headroom: f64,
    /// PQ encoder for HDR export. Lazily created on first HDR export frame.
    pq_encoder: Option<manifold_renderer::pq_encoder::PqEncoder>,
    /// Double-buffered output textures. UI reads front, content writes to back.
    /// NOT used on macOS (IOSurface path bypasses double-buffering).
    #[cfg(not(target_os = "macos"))]
    output_buffers: Option<[RenderTarget; 2]>,
    /// Which buffer is the front (0 or 1). Back is always `1 - front_index`.
    #[cfg(not(target_os = "macos"))]
    front_index: usize,
    /// Content frame rate tracking (for separate cadence mode).
    content_interval_secs: f64,
    last_content_time: f64,
    /// Shared output view for cross-thread access (fallback for non-macOS).
    shared_output: Arc<SharedOutputView>,
    /// Double-buffered IOSurface textures on the content device. Content writes
    /// to shared_textures[write_surface_index]; UI reads the other surface.
    #[cfg(target_os = "macos")]
    shared_textures: [Option<wgpu::Texture>; 2],
    /// Which surface we're writing to THIS frame (0 or 1).
    #[cfg(target_os = "macos")]
    write_surface_index: usize,
    /// IOSurface bridge for cross-device sharing.
    #[cfg(target_os = "macos")]
    shared_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// Last seen bridge generation — used to detect resize and re-import.
    #[cfg(target_os = "macos")]
    shared_generation: u64,
    /// GPU fence: tiny buffer copied at end of each frame. When map_async
    /// callback fires, we know the GPU finished this frame's work.
    fence_buffer: Option<wgpu::Buffer>,
    fence_staging: Option<wgpu::Buffer>,
    /// Set to true by map_async callback when GPU finishes the frame.
    fence_ready: Arc<AtomicBool>,
    /// True when a frame has been submitted but fence hasn't been checked yet.
    fence_pending: bool,
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
    /// HAL context for zero-overhead GPU encoding (macOS + hal-encoding feature).
    hal_ctx: Option<manifold_renderer::hal_context::HalContext>,
    /// Signal value from the previous frame's MTLSharedEvent.
    /// Checked by wait_previous_frame() to detect GPU completion without device.poll().
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    hal_signal_value: u64,
}

impl ContentPipeline {
    pub fn new(
        compositor: Box<dyn Compositor>,
        hal_ctx: Option<manifold_renderer::hal_context::HalContext>,
    ) -> Self {
        let shared = Arc::new(SharedOutputView::new());
        Self {
            compositor,
            edr_headroom: 1.0,
            pq_encoder: None,
            #[cfg(not(target_os = "macos"))]
            output_buffers: None,
            #[cfg(not(target_os = "macos"))]
            front_index: 0,
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
            fence_buffer: None,
            fence_staging: None,
            fence_ready: Arc::new(AtomicBool::new(false)),
            fence_pending: false,
            #[cfg(target_os = "macos")]
            last_write_surface: 0,
            #[cfg(feature = "profiling")]
            gpu_poll_ms: 0.0,
            #[cfg(feature = "profiling")]
            gpu_profiler: None,
            #[cfg(feature = "profiling")]
            last_gpu_pass_results: Vec::new(),
            hal_ctx,
            #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
            hal_signal_value: 0,
        }
    }

    /// Set the double-buffered IOSurface shared textures and bridge.
    /// Called during init after the bridge imports both textures on the content device.
    #[cfg(target_os = "macos")]
    pub fn set_shared_textures(
        &mut self,
        tex_a: wgpu::Texture,
        tex_b: wgpu::Texture,
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

    /// Lazily create the double-buffer pair at compositor dimensions.
    /// Only used on non-macOS (macOS uses IOSurface zero-copy path).
    #[cfg(not(target_os = "macos"))]
    fn ensure_output_buffers(&mut self, device: &wgpu::Device) {
        if self.output_buffers.is_some() {
            return;
        }
        let (w, h) = self.compositor.dimensions();
        self.output_buffers = Some([
            RenderTarget::new(device, w, h, OUTPUT_FORMAT, "ContentOutput_Front"),
            RenderTarget::new(device, w, h, OUTPUT_FORMAT, "ContentOutput_Back"),
        ]);
        self.front_index = 0;
    }

    /// Lazily create the GPU fence buffers used for async frame completion.
    fn ensure_fence_buffers(&mut self, device: &wgpu::Device) {
        if self.fence_buffer.is_some() {
            return;
        }
        self.fence_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frame Fence"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        // Staging buffer with sentinel value — copied to fence at end of each frame.
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Frame Fence Staging"),
            size: 4,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::MAP_WRITE,
            mapped_at_creation: true,
        });
        // Write sentinel so the copy is always valid.
        staging.slice(..).get_mapped_range_mut()[..4]
            .copy_from_slice(&0xDEADBEEFu32.to_ne_bytes());
        staging.unmap();
        self.fence_staging = Some(staging);
    }

    /// Wait for the previous frame's GPU work to finish (if pending).
    /// Called at the START of each frame before encoding new work.
    /// Publishes the completed IOSurface so the UI can read it.
    fn wait_previous_frame(&mut self, device: &wgpu::Device) {
        // ── HAL path: MTLSharedEvent sync ────────────────────────────────
        // signaled_value() is a direct GPU counter read — microsecond latency
        // vs device.poll() which goes through wgpu's internal machinery and
        // wgpu-hal's 1ms sleep loop.
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if let Some(ref hal_ctx) = self.hal_ctx {
            if self.hal_signal_value > 0 {
                while !hal_ctx.is_frame_done(self.hal_signal_value) {
                    std::thread::yield_now();
                }
                // Publish the completed IOSurface for the UI thread.
                if let Some(ref bridge) = self.shared_bridge {
                    bridge.publish_front(self.last_write_surface as u32);
                }
            }
            // Non-blocking poll for wgpu bookkeeping (readback map_async
            // callbacks, internal resource reclamation).
            let _ = device.poll(wgpu::PollType::Poll);
            return;
        }

        // ── Non-hal fallback: map_async + device.poll fence ──────────────
        if !self.fence_pending {
            return;
        }

        // Non-blocking check first — if the GPU finished between frames, no wait.
        if !self.fence_ready.load(Ordering::Acquire) {
            let _ = device.poll(wgpu::PollType::Poll);
        }

        // If still not ready, the GPU is truly behind — must block.
        if !self.fence_ready.load(Ordering::Acquire) {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }

        // Fence is ready — unmap and reset.
        if let Some(ref fence) = self.fence_buffer {
            fence.unmap();
        }
        self.fence_ready.store(false, Ordering::Release);
        self.fence_pending = false;

        // Publish the completed surface for the UI to read.
        #[cfg(target_os = "macos")]
        if let Some(ref bridge) = self.shared_bridge {
            bridge.publish_front(self.last_write_surface as u32);
        }
    }

    /// Render all generators and composite, then submit asynchronously.
    ///
    /// Uses double-buffered output: content writes to one surface while
    /// the UI reads the other. A fence buffer detects GPU completion so
    /// we only block if the GPU is truly behind (2 frames in flight).
    pub fn render_content(
        &mut self,
        gpu: &GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
    ) {
        let _t_frame = std::time::Instant::now();

        self.ensure_fence_buffers(&gpu.device);

        // Wait for PREVIOUS frame's GPU work before encoding this frame.
        // If the GPU finished quickly (likely), this returns immediately.
        let _poll_start = std::time::Instant::now();
        self.wait_previous_frame(&gpu.device);
        let _poll_ms = _poll_start.elapsed().as_secs_f64() * 1000.0;

        #[cfg(not(target_os = "macos"))]
        self.ensure_output_buffers(&gpu.device);

        // Extract timing values before split borrow
        let time = engine.current_time();
        let beat = engine.current_beat();

        // === HAL THREE-ENCODER SPLIT ===
        // When hal-encoding is active, split the frame into 3 separate encoders:
        // 1. wgpu encoder → generators → submit
        // 2. hal encoder → compositor → hal submit
        // 3. wgpu encoder → IOSurface copy + fence → submit
        // Metal command queue guarantees in-order execution.
        #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
        if self.hal_ctx.is_some() {
            self.render_content_hal(gpu, engine, tick_result, dt, frame_count,
                                    time, beat, _t_frame, _poll_ms);
            return;
        }

        let mut wgpu_encoder =
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Frame Encoder"),
                });

        let mut gpu_enc = GpuEncoder::new(
            &gpu.device, &gpu.queue, &mut wgpu_encoder, None,
        );

        // Split borrow: get renderers + project from engine simultaneously.
        let (renderers, project) = engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // GPU profiler: begin frame and lazily create profiler
        #[cfg(feature = "profiling")]
        {
            if self.gpu_profiler.is_none() {
                self.gpu_profiler =
                    manifold_renderer::gpu_profiler::GpuProfiler::new(&gpu.device, &gpu.queue, &gpu.adapter);
            }
            if let Some(ref mut profiler) = self.gpu_profiler {
                profiler.begin_frame();
            }
        }

        let _t0 = std::time::Instant::now();
        // Render generators via downcast (GPU rendering needs queue + encoder)
        for renderer in renderers.iter_mut() {
            if let Some(gen_renderer) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                #[cfg(feature = "profiling")]
                let gpu_prof = self.gpu_profiler.as_ref();
                #[cfg(not(feature = "profiling"))]
                let gpu_prof: Option<&manifold_renderer::gpu_profiler::GpuProfiler> = None;
                gen_renderer.render_all(
                    &mut gpu_enc, time, beat, dt as f32, layers, gpu_prof,
                );
                break;
            }
        }

        let _gen_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        let _t0 = std::time::Instant::now();
        // Build clip descriptors for compositor
        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());

        for clip in &tick_result.ready_clips {
            let clip_textures = renderers.iter().find_map(|r| {
                if let Some(gen_r) = r.as_any().downcast_ref::<GeneratorRenderer>()
                    && let (Some(v), Some(t)) = (
                        gen_r.get_clip_texture_view(&clip.id),
                        gen_r.get_clip_texture(&clip.id),
                    )
                {
                    return Some((v, t));
                }
                #[cfg(target_os = "macos")]
                if let Some(vid_r) = r.as_any().downcast_ref::<VideoRenderer>()
                    && let (Some(v), Some(t)) = (
                        vid_r.get_clip_texture_view(&clip.id),
                        vid_r.get_clip_texture(&clip.id),
                    )
                {
                    return Some((v, t));
                }
                None
            });
            if let Some((view, texture)) = clip_textures {
                let clip_li = project.and_then(|p| p.timeline.layer_index_for_id(&clip.layer_id))
                    .unwrap_or(0);
                let layer = layers.get(clip_li);
                clip_descs.push(CompositeClipDescriptor {
                    clip_id: &clip.id,
                    texture_view: view,
                    texture,
                    layer_index: clip_li as i32,
                    blend_mode: layer.map_or(BlendMode::Normal, |l| l.default_blend_mode),
                    opacity: layer.map_or(1.0, |l| l.opacity),
                    translate_x: clip.translate_x,
                    translate_y: clip.translate_y,
                    scale: clip.scale,
                    rotation: clip.rotation,
                    invert_colors: clip.invert_colors,
                    effects: &clip.effects,
                    effect_groups: clip.effect_groups.as_deref().unwrap_or(&[]),
                });
            }
        }

        // Build layer descriptors for compositor
        // Use static empty slices instead of per-frame Vec allocations.
        let empty_effects: &[EffectInstance] = &[];
        let empty_groups: &[EffectGroup] = &[];
        let layer_descs: Vec<CompositeLayerDescriptor> = layers.iter().map(|layer| {
            CompositeLayerDescriptor {
                layer_index: layer.index,
                layer_id: layer.layer_id.clone(),
                blend_mode: layer.default_blend_mode,
                opacity: layer.opacity,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                effects: layer.effects.as_deref().unwrap_or(empty_effects),
                effect_groups: layer.effect_groups.as_deref().unwrap_or(empty_groups),
            }
        }).collect();

        // Composite
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
            tonemap: TonemapSettings {
                exposure: 1.0,
                hdr_output_enabled: self.edr_headroom > 1.0,
                paper_white_nits: 200.0,
                // Dynamic max nits from actual display EDR headroom.
                // headroom=1.0 (SDR) → 200 nits, headroom=2.0 → 400 nits, etc.
                // Unity: MonitorOutput.cs reads HDROutputSettings.maxFullFrameToneMapLuminance.
                max_display_nits: (200.0 * self.edr_headroom as f32).min(10000.0),
            },
        };

        let _desc_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        let _t0 = std::time::Instant::now();
        // Render compositor (records into encoder, returns view into tonemap output)
        #[cfg(feature = "profiling")]
        let gpu_prof = self.gpu_profiler.as_ref();
        #[cfg(not(feature = "profiling"))]
        let gpu_prof: Option<&manifold_renderer::gpu_profiler::GpuProfiler> = None;
        let _compositor_view = self.compositor.render(
            &mut gpu_enc, &frame, gpu_prof,
        );
        let _comp_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        let (comp_w, comp_h) = self.compositor.dimensions();

        // Copy compositor output to the appropriate destination.
        // macOS: IOSurface shared texture (double-buffered, write to current surface).
        // Other: double-buffered output textures (UI reads via SharedOutputView).
        #[cfg(target_os = "macos")]
        {
            if let Some(ref shared_tex) = self.shared_textures[self.write_surface_index]
                && shared_tex.width() == comp_w && shared_tex.height() == comp_h {
                    gpu_enc.encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: self.compositor.output_texture(),
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: shared_tex,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: comp_w,
                            height: comp_h,
                            depth_or_array_layers: 1,
                        },
                    );
                }
        }

        #[cfg(not(target_os = "macos"))]
        {
            let back_index = 1 - self.front_index;
            let bufs = self.output_buffers.as_ref().unwrap();
            let copy_size = wgpu::Extent3d {
                width: comp_w.min(bufs[back_index].width),
                height: comp_h.min(bufs[back_index].height),
                depth_or_array_layers: 1,
            };
            gpu_enc.encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: self.compositor.output_texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &bufs[back_index].texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                copy_size,
            );
        }

        // GPU profiler: resolve timestamp queries before submission
        #[cfg(feature = "profiling")]
        if let Some(ref profiler) = self.gpu_profiler {
            profiler.resolve(gpu_enc.encoder);
        }

        // Append fence copy at the end of the encoder — when the GPU executes
        // this copy, we know all preceding work is done.
        if let (Some(staging), Some(fence)) =
            (&self.fence_staging, &self.fence_buffer)
        {
            gpu_enc.encoder.copy_buffer_to_buffer(staging, 0, fence, 0, 4);
        }

        // Drop GpuEncoder to release mutable borrow on wgpu_encoder before finish().
        #[allow(clippy::drop_non_drop)]
        drop(gpu_enc);

        // Submit all GPU work — single command buffer containing both wgpu and
        // hal-encoded (via as_hal_mut) work in correct interleaved order.
        let _t0 = std::time::Instant::now();
        gpu.queue.submit(std::iter::once(wgpu_encoder.finish()));
        let _submit_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // Start async fence: map_async fires when GPU finishes this frame.
        // We check fence_ready at the START of the NEXT frame — if the GPU
        // finished between frames (likely), no blocking occurs at all.
        if let Some(ref fence) = self.fence_buffer {
            let ready = Arc::clone(&self.fence_ready);
            fence.slice(..).map_async(wgpu::MapMode::Read, move |_| {
                ready.store(true, Ordering::Release);
            });
            self.fence_pending = true;
        }

        // Remember which surface we wrote to so we can publish it next frame.
        #[cfg(target_os = "macos")]
        {
            self.last_write_surface = self.write_surface_index;
            self.write_surface_index = 1 - self.write_surface_index;
        }

        // GPU profiler: read PREVIOUS frame's results (deferred readback).
        // The fence check at frame start guaranteed the previous frame completed.
        #[cfg(feature = "profiling")]
        {
            self.gpu_poll_ms = _poll_ms;
            self.last_gpu_pass_results = self.gpu_profiler
                .as_ref()
                .map_or_else(Vec::new, |p| p.read_results(&gpu.device));
        }

        // Periodic stderr dump — independent of profiling feature
        let _total_ms = _t_frame.elapsed().as_secs_f64() * 1000.0;
        let (comp_w, comp_h) = self.compositor.dimensions();
        if frame_count > 0 && frame_count.is_multiple_of(60) {
            eprintln!(
                "[PERF] frame={} clips={} res={}x{} | gen={:.1}ms desc={:.1}ms comp={:.1}ms \
                 submit={:.1}ms poll={:.1}ms | total={:.1}ms ({:.0}fps)",
                frame_count,
                tick_result.ready_clips.len(),
                comp_w, comp_h,
                _gen_ms, _desc_ms, _comp_ms, _submit_ms, _poll_ms, _total_ms,
                1000.0 / _total_ms.max(0.001),
            );
        }

        // Swap + update shared output view (non-macOS fallback path)
        #[cfg(not(target_os = "macos"))]
        {
            let back_index = 1 - self.front_index;
            self.front_index = back_index;
            let bufs = self.output_buffers.as_ref().unwrap();
            let front_view = bufs[self.front_index].texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.shared_output.set_view(front_view);
        }

        // Update shared dimensions for UI aspect ratio (only when changed).
        let (old_w, old_h) = self.shared_output.get_dimensions();
        if old_w != comp_w || old_h != comp_h {
            self.shared_output.set_dimensions(comp_w, comp_h);
        }
    }

    /// HAL three-encoder split: generators (wgpu) → compositor (hal) → copy+fence (wgpu).
    /// Called when hal-encoding feature is active. Metal command queue guarantees
    /// in-order execution across all three submissions.
    #[cfg(all(target_os = "macos", feature = "hal-encoding"))]
    #[allow(clippy::too_many_arguments)]
    fn render_content_hal(
        &mut self,
        gpu: &GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
        time: f32,
        beat: f32,
        _t_frame: std::time::Instant,
        _poll_ms: f64,
    ) {
        use wgpu::hal::CommandEncoder as HalCmdEnc;

        let hal_ctx = self.hal_ctx.as_ref().unwrap();

        // Split borrow: get renderers + project from engine simultaneously.
        let (renderers, project) = engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // ── Encoder 1: generators (wgpu) ─────────────────────────────────
        let _t0 = std::time::Instant::now();
        let mut gen_encoder = gpu.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("Generator Encoder") },
        );
        {
            let mut gpu_gen = GpuEncoder::new(
                &gpu.device, &gpu.queue, &mut gen_encoder, None,
            );
            for renderer in renderers.iter_mut() {
                if let Some(gen_renderer) =
                    renderer.as_any_mut().downcast_mut::<GeneratorRenderer>()
                {
                    gen_renderer.render_all(
                        &mut gpu_gen, time, beat, dt as f32, layers, None,
                    );
                    break;
                }
            }
        }
        gpu.queue.submit(std::iter::once(gen_encoder.finish()));
        let _gen_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // ── Build clip + layer descriptors (CPU only) ────────────────────
        let _t0 = std::time::Instant::now();
        let empty_effects: &[EffectInstance] = &[];
        let empty_groups: &[EffectGroup] = &[];

        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());
        for clip in &tick_result.ready_clips {
            let clip_textures = renderers.iter().find_map(|r| {
                if let Some(gen_r) = r.as_any().downcast_ref::<GeneratorRenderer>()
                    && let (Some(v), Some(t)) = (
                        gen_r.get_clip_texture_view(&clip.id),
                        gen_r.get_clip_texture(&clip.id),
                    )
                {
                    return Some((v, t));
                }
                #[cfg(target_os = "macos")]
                if let Some(vid_r) = r.as_any().downcast_ref::<VideoRenderer>()
                    && let (Some(v), Some(t)) = (
                        vid_r.get_clip_texture_view(&clip.id),
                        vid_r.get_clip_texture(&clip.id),
                    )
                {
                    return Some((v, t));
                }
                None
            });
            if let Some((view, texture)) = clip_textures {
                let clip_li = project
                    .and_then(|p| p.timeline.layer_index_for_id(&clip.layer_id))
                    .unwrap_or(0);
                let layer = layers.get(clip_li);
                clip_descs.push(CompositeClipDescriptor {
                    clip_id: &clip.id,
                    texture_view: view,
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
                    effect_groups: clip.effect_groups.as_deref().unwrap_or(&[]),
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

        // ── Encoder 2: compositor (hal) ──────────────────────────────────
        let _t0 = std::time::Instant::now();
        let mut hal_enc = hal_ctx.create_command_encoder();
        unsafe {
            hal_enc
                .begin_encoding(Some("Compositor"))
                .expect("hal begin_encoding failed");
        }

        // Auxiliary wgpu encoder — handles wgpu-tracked operations during
        // hal compositing (readback copy_texture_to_buffer, etc.). Submitted
        // AFTER the hal encoder so Metal in-order execution ensures hal
        // writes complete before readback copies run. This allows map_async
        // to fire correctly (wgpu tracks this encoder's submissions).
        let mut aux_enc = gpu.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("HAL Compositor (aux wgpu)"),
            },
        );
        {
            let mut gpu_comp = GpuEncoder::new(
                &gpu.device, &gpu.queue, &mut aux_enc, Some(hal_ctx),
            );
            gpu_comp.hal_enc =
                Some(&mut hal_enc as *mut manifold_renderer::hal_context::MetalCommandEncoder);

            // Compositor render — render/compute passes go to hal encoder,
            // wgpu-tracked operations (readbacks) go to aux_enc.
            let _compositor_view =
                self.compositor.render(&mut gpu_comp, &frame, None);
        }

        // IOSurface copy via hal — merged into compositor encoder.
        // Replaces the old wgpu encoder 3 copy. Same hal blit pattern as
        // layer_compositor.rs effect_chain→tonemap copy.
        #[cfg(target_os = "macos")]
        {
            let (comp_w, comp_h) = self.compositor.dimensions();
            if let Some(ref shared_tex) =
                self.shared_textures[self.write_surface_index]
                && shared_tex.width() == comp_w
                && shared_tex.height() == comp_h
            {
                type MetalApi = wgpu::hal::api::Metal;
                use wgpu::hal::CommandEncoder as HalCopyEnc;
                let src_tex_ptr = {
                    let g = unsafe {
                        self.compositor.output_texture().as_hal::<MetalApi>()
                    }
                    .expect("compositor output not Metal");
                    &*g as *const _
                };
                let dst_tex_ptr = {
                    let g = unsafe { shared_tex.as_hal::<MetalApi>() }
                        .expect("shared tex not Metal");
                    &*g as *const _
                };
                unsafe {
                    hal_enc.copy_texture_to_texture(
                        &*src_tex_ptr,
                        wgpu::wgt::TextureUses::COPY_SRC,
                        &*dst_tex_ptr,
                        std::iter::once(wgpu::hal::TextureCopy {
                            src_base: wgpu::hal::TextureCopyBase {
                                mip_level: 0,
                                array_layer: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::hal::FormatAspects::COLOR,
                            },
                            dst_base: wgpu::hal::TextureCopyBase {
                                mip_level: 0,
                                array_layer: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::hal::FormatAspects::COLOR,
                            },
                            size: wgpu::hal::CopyExtent {
                                width: comp_w,
                                height: comp_h,
                                depth: 1,
                            },
                        }),
                    );
                }
            }
        }

        let hal_cmd_buf = unsafe {
            hal_enc
                .end_encoding()
                .expect("hal end_encoding failed")
        };
        unsafe {
            hal_ctx.submit(&[&hal_cmd_buf]);
        }
        // Submit aux wgpu encoder — readback copies + any queue.write_buffer
        // staging. Metal in-order queue guarantees hal work completes first.
        gpu.queue.submit(std::iter::once(aux_enc.finish()));

        // Signal frame completion via MTLSharedEvent. The lightweight signal
        // command buffer is committed after all other submissions — Metal
        // in-order execution guarantees it fires after compositor + copy +
        // readbacks all complete.
        unsafe { hal_ctx.signal_frame_completion(); }
        self.hal_signal_value = hal_ctx.current_signal_value();
        let _comp_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // Surface swap
        #[cfg(target_os = "macos")]
        {
            self.last_write_surface = self.write_surface_index;
            self.write_surface_index = 1 - self.write_surface_index;
        }

        // Periodic perf dump
        let (comp_w, comp_h) = self.compositor.dimensions();
        let _total_ms = _t_frame.elapsed().as_secs_f64() * 1000.0;
        if frame_count > 0 && frame_count.is_multiple_of(60) {
            eprintln!(
                "[PERF/HAL] frame={} clips={} res={}x{} | gen={:.1}ms desc={:.1}ms \
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
    }

    /// The stable output texture view. UI reads this for blitting.
    /// Returns None only before the first render.
    /// Only used on non-macOS (macOS reads via IOSurface).
    #[cfg(not(target_os = "macos"))]
    pub fn output_view(&self) -> Option<&wgpu::TextureView> {
        self.output_buffers.as_ref().map(|bufs| &bufs[self.front_index].view)
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

    /// Resize compositor, generators, and output buffers to new project resolution.
    pub fn resize(&mut self, device: &wgpu::Device, engine: &mut PlaybackEngine, width: u32, height: u32) {
        self.compositor.resize(device, width, height);
        // Resize generator renderer via engine downcast
        let (renderers, _) = engine.split_renderer_project();
        for renderer in renderers.iter_mut() {
            if let Some(gen_renderer) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                gen_renderer.resize_gpu(width, height);
                break;
            }
        }
        // Recreate output buffers at new dimensions (non-macOS only)
        #[cfg(not(target_os = "macos"))]
        if self.output_buffers.is_some() {
            self.output_buffers = Some([
                RenderTarget::new(device, width, height, OUTPUT_FORMAT, "ContentOutput_Front"),
                RenderTarget::new(device, width, height, OUTPUT_FORMAT, "ContentOutput_Back"),
            ]);
            self.front_index = 0;
        }
        // Resize IOSurface bridge and re-import both content textures
        #[cfg(target_os = "macos")]
        if let Some(ref bridge) = self.shared_bridge {
            bridge.resize(width, height);
            self.shared_textures = [
                Some(unsafe { bridge.import_texture(device, 0) }),
                Some(unsafe { bridge.import_texture(device, 1) }),
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
    pub fn pre_tonemap_output(&self) -> &wgpu::TextureView {
        self.compositor.pre_tonemap_output()
    }

    /// Underlying GPU texture of the compositor output for SDR export.
    /// Used to extract the raw Metal texture pointer via `as_hal`.
    pub fn export_output_texture(&self) -> &wgpu::Texture {
        self.compositor.output_texture()
    }

    /// Compositor output view (post-tonemap). Used by LED output to blit
    /// through the edge-extend shader. The texture has TEXTURE_BINDING usage.
    pub fn compositor_output_view(&self) -> &wgpu::TextureView {
        self.compositor.output_view()
    }

    /// LED tap view: pre-tonemap composite captured when led_exit_index == 0.
    /// Returns the tap if available, otherwise falls back to the final output.
    pub fn led_source_view(&self) -> &wgpu::TextureView {
        self.compositor.led_tap_view().unwrap_or_else(|| self.compositor.output_view())
    }

    /// Run the PQ encoder on the final compositor output for HDR export.
    /// Returns the PQ-encoded texture for the Metal encoder.
    /// Lazily creates the PQ encoder pipeline on first call.
    pub fn pq_encode_for_export(
        &mut self,
        gpu: &manifold_renderer::gpu::GpuContext,
        paper_white_nits: f32,
        max_nits: f32,
    ) -> &wgpu::Texture {
        let (w, h) = self.compositor.dimensions();

        // Lazy init PQ encoder
        if self.pq_encoder.is_none() {
            self.pq_encoder =
                Some(manifold_renderer::pq_encoder::PqEncoder::new(&gpu.device, w, h));
            log::info!("[ContentPipeline] Created PQ encoder {}x{}", w, h);
        }
        let pq = self.pq_encoder.as_ref().unwrap();

        // Resize if needed
        if pq.output.width != w || pq.output.height != h {
            self.pq_encoder.as_mut().unwrap().resize(&gpu.device, w, h);
        }

        // Encode: take the final compositor output (post-tonemap, post-effects)
        // and apply the ST.2084 PQ transfer function.
        let edr_view = self.compositor.output_view();
        let mut encoder =
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("PQ Encode"),
                });
        self.pq_encoder.as_ref().unwrap().encode(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            edr_view,
            paper_white_nits,
            max_nits,
        );
        gpu.queue.submit(std::iter::once(encoder.finish()));

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
    pub fn last_gpu_pass_results(&self) -> &[manifold_renderer::gpu_profiler::GpuPassTiming] {
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
