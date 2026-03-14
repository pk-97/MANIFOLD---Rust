use std::collections::HashSet;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_core::project::Project;
use manifold_core::types::{BlendMode, GeneratorType, LayerType, PlaybackState};
use manifold_core::layer::Layer;
use manifold_core::clip::TimelineClip;
use manifold_core::generator::GeneratorParamState;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::StubRenderer;
use manifold_renderer::blit::BlitPipeline;
use manifold_renderer::compositor::{Compositor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::{CompositeClipDescriptor, LayerCompositor};
use manifold_renderer::surface::SurfaceWrapper;

use crate::frame_timer::FrameTimer;
use crate::window_registry::{WindowRegistry, WindowRole, WindowState};

pub struct Application {
    // GPU
    gpu: Option<GpuContext>,

    // Windows
    window_registry: WindowRegistry,
    primary_window_id: Option<WindowId>,

    // Engine
    engine: PlaybackEngine,

    // Rendering
    generator_renderer: Option<GeneratorRenderer>,
    compositor: Option<Box<dyn Compositor>>,
    blit_pipeline: Option<BlitPipeline>,

    // Frame timing
    frame_timer: FrameTimer,
    frame_count: u64,

    // Lifecycle tracking for generator renderer sync
    prev_active_clip_ids: HashSet<String>,

    // State
    initialized: bool,
}

impl Application {
    pub fn new() -> Self {
        // Create engine with stub renderers for lifecycle tracking
        let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
            Box::new(StubRenderer::new_video()),
            Box::new(StubRenderer::new_generator()),
        ];
        let mut engine = PlaybackEngine::new(renderers);

        // Create test project with a Plasma generator clip
        let project = Self::create_test_project();
        engine.initialize(project);
        engine.set_state(PlaybackState::Playing);

        Self {
            gpu: None,
            window_registry: WindowRegistry::new(),
            primary_window_id: None,
            engine,
            generator_renderer: None,
            compositor: None,
            blit_pipeline: None,
            frame_timer: FrameTimer::new(60.0),
            frame_count: 0,
            prev_active_clip_ids: HashSet::with_capacity(16),
            initialized: false,
        }
    }

    fn create_test_project() -> Project {
        let mut project = Project::default();
        project.settings.bpm = 120.0;
        project.settings.time_signature_numerator = 4;

        // Generator layer with Plasma clip
        let mut layer = Layer::new("Plasma".to_string(), LayerType::Generator, 0);
        layer.gen_params = Some(GeneratorParamState {
            generator_type: GeneratorType::Plasma,
            param_values: vec![
                0.0, // PATTERN
                0.5, // COMPLEXITY
                0.5, // CONTRAST
                1.0, // SPEED
                1.0, // SCALE
                0.0, // SNAP
            ],
            base_param_values: None,
            drivers: None,
            envelopes: None,
            legacy_param_version: None,
        });

        let clip = TimelineClip::new_generator(GeneratorType::Plasma, 0, 0.0, 10000.0);
        layer.add_clip(clip);
        project.timeline.layers.push(layer);

        project
    }

    fn open_output_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        name: &str,
        display_index: Option<usize>,
    ) {
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        let mut attrs = winit::window::Window::default_attributes()
            .with_title(format!("MANIFOLD - {}", name))
            .with_inner_size(winit::dpi::LogicalSize::new(960u32, 540u32));

        if let Some(idx) = display_index {
            if let Some(monitor) = event_loop.available_monitors().nth(idx) {
                let pos = monitor.position();
                attrs = attrs.with_position(winit::dpi::Position::Physical(
                    winit::dpi::PhysicalPosition::new(pos.x, pos.y),
                ));
            }
        }

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("Failed to create output window: {e}");
                return;
            }
        };

        let size = window.inner_size();
        let scale = window.scale_factor();

        let surface = SurfaceWrapper::new(
            &gpu.instance,
            &gpu.adapter,
            &gpu.device,
            window.clone(),
            size.width,
            size.height,
            scale,
            wgpu::PresentMode::Fifo,
        );

        let id = window.id();
        let state = WindowState {
            window,
            surface,
            role: WindowRole::Output {
                name: name.to_string(),
            },
            display_index,
        };

        self.window_registry.add(id, state);
        log::info!("Opened output window: {name}");
    }

    fn tick_and_render(&mut self) {
        let dt = self.frame_timer.consume_tick();
        let realtime = self.frame_timer.realtime_since_start();

        // 1. Tick the engine
        let ctx = TickContext {
            dt_seconds: dt,
            realtime_now: realtime,
            pre_render_dt: dt as f32,
            frame_count: self.frame_count as i32,
        };
        let tick_result = self.engine.tick(ctx);

        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        let gen_renderer = match &mut self.generator_renderer {
            Some(g) => g,
            None => return,
        };

        // 2. Sync generator renderer lifecycle with engine
        // Build current active set from ready_clips
        let mut current_active: HashSet<String> = HashSet::with_capacity(tick_result.ready_clips.len());
        for clip in &tick_result.ready_clips {
            if clip.is_generator() {
                current_active.insert(clip.id.clone());

                // Start new clips
                if !gen_renderer.is_active(&clip.id) {
                    gen_renderer.start_clip(
                        &gpu.device,
                        &clip.id,
                        clip.generator_type,
                        clip.layer_index,
                    );
                }
            }
        }

        // Stop clips that are no longer active
        for old_id in &self.prev_active_clip_ids {
            if !current_active.contains(old_id) {
                gen_renderer.stop_clip(old_id);
            }
        }
        self.prev_active_clip_ids = current_active;

        // 3. Render all generators
        let layers = self
            .engine
            .project()
            .map(|p| p.timeline.layers.as_slice())
            .unwrap_or(&[]);

        let mut encoder =
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Frame Encoder"),
                });

        gen_renderer.render_all(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            self.engine.current_time(),
            self.engine.current_beat(),
            dt as f32,
            layers,
        );

        // 4. Build clip descriptors for compositor
        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());

        for clip in &tick_result.ready_clips {
            if let Some(view) = gen_renderer.get_clip_texture_view(&clip.id) {
                let layer = layers.get(clip.layer_index as usize);
                clip_descs.push(CompositeClipDescriptor {
                    clip_id: &clip.id,
                    texture_view: view,
                    layer_index: clip.layer_index,
                    blend_mode: layer.map_or(BlendMode::Normal, |l| l.default_blend_mode),
                    opacity: layer.map_or(1.0, |l| l.opacity),
                    translate_x: clip.translate_x,
                    translate_y: clip.translate_y,
                    scale: clip.scale,
                    rotation: clip.rotation,
                    invert_colors: clip.invert_colors,
                });
            }
        }

        // 5. Composite
        let compositor = match &mut self.compositor {
            Some(c) => c,
            None => return,
        };

        let frame = CompositorFrame {
            time: self.engine.current_time(),
            beat: self.engine.current_beat(),
            dt: dt as f32,
            frame_count: self.frame_count,
            compositor_dirty: tick_result.compositor_dirty,
            clips: &clip_descs,
        };

        let output_view = compositor.render(&gpu.device, &gpu.queue, &mut encoder, &frame);

        // 6. Submit generator + compositor work
        // We need to get the output view pointer before submitting, but the borrow
        // from compositor.render() returns a reference into the compositor.
        // We must submit, then blit in separate encoders per window.
        // Actually — we can get a raw pointer to avoid the borrow issue.
        // Safer approach: submit this encoder, then blit in present_all_windows.
        let output_view_ptr: *const wgpu::TextureView = output_view;
        gpu.queue.submit(std::iter::once(encoder.finish()));

        // 7. Present to all windows via blit
        // SAFETY: output_view points into self.compositor's RenderTarget which
        // is not modified between here and the blit calls.
        let output_view_ref = unsafe { &*output_view_ptr };
        self.present_all_windows(output_view_ref);

        self.frame_count += 1;
    }

    fn present_all_windows(&mut self, compositor_output: &wgpu::TextureView) {
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };
        let blit = match &self.blit_pipeline {
            Some(b) => b,
            None => return,
        };

        let window_ids: Vec<WindowId> = self.window_registry.iter().map(|(id, _)| *id).collect();

        for window_id in window_ids {
            let ws = match self.window_registry.get_mut(&window_id) {
                Some(ws) => ws,
                None => continue,
            };

            let surface_texture = match ws.surface.get_current_texture() {
                Ok(t) => t,
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    ws.surface.resize(
                        &gpu.device,
                        ws.surface.width,
                        ws.surface.height,
                        ws.surface.scale_factor,
                    );
                    continue;
                }
                Err(e) => {
                    log::error!("Surface error: {e}");
                    continue;
                }
            };

            let surface_view = surface_texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder =
                gpu.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Blit Encoder"),
                    });

            blit.blit(&gpu.device, &mut encoder, compositor_output, &surface_view);

            gpu.queue.submit(std::iter::once(encoder.finish()));
            surface_texture.present();
        }
    }
}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.initialized {
            return;
        }

        log::info!("Creating primary window...");

        let attrs = winit::window::Window::default_attributes()
            .with_title("MANIFOLD")
            .with_inner_size(winit::dpi::LogicalSize::new(1280u32, 720u32));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("Failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let scale = window.scale_factor();

        // Create GPU context with primary window's surface for adapter compatibility
        let gpu = {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                ..Default::default()
            });

            let surface = instance
                .create_surface(window.clone())
                .expect("Failed to create surface");

            let gpu = pollster::block_on(async {
                let adapter = instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        compatible_surface: Some(&surface),
                        force_fallback_adapter: false,
                    })
                    .await
                    .expect("No suitable GPU adapter");

                log::info!("GPU: {}", adapter.get_info().name);

                let (device, queue) = adapter
                    .request_device(
                        &wgpu::DeviceDescriptor {
                            label: Some("MANIFOLD Device"),
                            required_features: wgpu::Features::empty(),
                            required_limits: wgpu::Limits::default(),
                            memory_hints: wgpu::MemoryHints::Performance,
                        },
                        None,
                    )
                    .await
                    .expect("Failed to create device");

                (instance, adapter, device, queue, surface)
            });

            let (instance, adapter, device, queue, surface) = gpu;

            // Configure surface
            let caps = surface.get_capabilities(&adapter);
            let format = caps
                .formats
                .iter()
                .find(|f| f.is_srgb())
                .copied()
                .unwrap_or(caps.formats[0]);

            let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                caps.present_modes[0]
            };

            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: size.width.max(1),
                height: size.height.max(1),
                present_mode,
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };
            surface.configure(&device, &config);

            let surface_wrapper = SurfaceWrapper {
                surface,
                config,
                width: size.width,
                height: size.height,
                scale_factor: scale,
            };

            // Register primary window
            let wid = window.id();
            self.primary_window_id = Some(wid);
            self.window_registry.add(
                wid,
                WindowState {
                    window,
                    surface: surface_wrapper,
                    role: WindowRole::Workspace,
                    display_index: None,
                },
            );

            // Create blit pipeline
            self.blit_pipeline = Some(BlitPipeline::new(&device, format));

            // Create generator renderer and compositor
            let output_w = 1920u32;
            let output_h = 1080u32;
            let compositor_format = wgpu::TextureFormat::Rgba16Float;

            self.generator_renderer = Some(GeneratorRenderer::new(
                &device,
                output_w,
                output_h,
                compositor_format,
                8,
            ));

            self.compositor = Some(Box::new(LayerCompositor::new(&device, output_w, output_h)));

            GpuContext {
                instance,
                adapter,
                device,
                queue,
            }
        };

        self.gpu = Some(gpu);
        self.initialized = true;

        log::info!(
            "Initialized. Press Space=play/pause, O=output window, Escape=close output"
        );
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                if Some(window_id) == self.primary_window_id {
                    event_loop.exit();
                } else {
                    self.window_registry.remove(&window_id);
                    log::info!("Closed output window");
                }
            }

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &self.gpu {
                    if let Some(ws) = self.window_registry.get_mut(&window_id) {
                        let scale = ws.window.scale_factor();
                        ws.surface
                            .resize(&gpu.device, size.width, size.height, scale);
                    }
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = &self.gpu {
                    if let Some(ws) = self.window_registry.get_mut(&window_id) {
                        let size = ws.window.inner_size();
                        ws.surface
                            .resize(&gpu.device, size.width, size.height, scale_factor);
                    }
                }
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match logical_key {
                Key::Named(NamedKey::Space) => {
                    if self.engine.is_playing() {
                        self.engine.set_state(PlaybackState::Paused);
                        log::info!("Paused");
                    } else {
                        self.engine.set_state(PlaybackState::Playing);
                        log::info!("Playing");
                    }
                }
                Key::Character(ref c) if c.as_str() == "o" || c.as_str() == "O" => {
                    let count = self.window_registry.len();
                    self.open_output_window(event_loop, &format!("Output {}", count), None);
                }
                Key::Named(NamedKey::Escape) => {
                    if Some(window_id) != self.primary_window_id {
                        self.window_registry.remove(&window_id);
                        log::info!("Closed output window");
                    }
                }
                _ => {}
            },

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if !self.initialized {
            return;
        }

        if self.frame_timer.should_tick() {
            self.tick_and_render();
        }

        for window in self
            .window_registry
            .window_arcs()
            .cloned()
            .collect::<Vec<_>>()
        {
            window.request_redraw();
        }
    }
}
