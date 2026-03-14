use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_core::project::Project;
use manifold_core::types::PlaybackState;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::StubRenderer;
use manifold_renderer::blit::BlitPipeline;
use manifold_renderer::compositor::{ClearColorCompositor, Compositor, CompositorFrame};
use manifold_renderer::gpu::GpuContext;
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
    compositor: Option<Box<dyn Compositor>>,
    blit_pipeline: Option<BlitPipeline>,

    // Frame timing
    frame_timer: FrameTimer,
    frame_count: u64,

    // State
    initialized: bool,
}

impl Application {
    pub fn new() -> Self {
        // Create engine with a stub renderer (no GPU renderers yet)
        let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
            Box::new(StubRenderer::new_video()),
        ];
        let mut engine = PlaybackEngine::new(renderers);

        // Load a default project so the engine has something to tick
        let mut project = Project::default();
        project.settings.bpm = 120.0;
        project.settings.time_signature_numerator = 4;
        engine.initialize(project);
        engine.set_state(PlaybackState::Playing);

        Self {
            gpu: None,
            window_registry: WindowRegistry::new(),
            primary_window_id: None,
            engine,
            compositor: None,
            blit_pipeline: None,
            frame_timer: FrameTimer::new(60.0),
            frame_count: 0,
            initialized: false,
        }
    }

    fn open_output_window(&mut self, event_loop: &ActiveEventLoop, name: &str, display_index: Option<usize>) {
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        let mut attrs = winit::window::Window::default_attributes()
            .with_title(format!("MANIFOLD - {}", name))
            .with_inner_size(winit::dpi::LogicalSize::new(960u32, 540u32));

        // Position on target display if specified
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
            wgpu::PresentMode::Fifo, // vsync for output windows
        );

        let id = window.id();
        let state = WindowState {
            window,
            surface,
            role: WindowRole::Output { name: name.to_string() },
            display_index,
        };

        self.window_registry.add(id, state);
        log::info!("Opened output window: {name}");
    }

    fn tick_and_render(&mut self) {
        let dt = self.frame_timer.consume_tick();
        let realtime = self.frame_timer.realtime_since_start();

        // Tick the engine
        let ctx = TickContext {
            dt_seconds: dt,
            realtime_now: realtime,
            pre_render_dt: dt as f32,
            frame_count: self.frame_count as i32,
        };
        let tick_result = self.engine.tick(ctx);

        // Render compositor
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };
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
        };

        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Frame Encoder"),
        });

        compositor.render(&gpu.device, &gpu.queue, &mut encoder, &frame);

        gpu.queue.submit(std::iter::once(encoder.finish()));

        // Present to all windows
        self.present_all_windows();

        self.frame_count += 1;
    }

    fn present_all_windows(&mut self) {
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };
        let _blit = match &self.blit_pipeline {
            Some(b) => b,
            None => return,
        };
        let _compositor = match &self.compositor {
            Some(c) => c,
            None => return,
        };

        // We need to re-render the compositor output via blit for each window.
        // The compositor already rendered into its internal target.
        // We need to get the output view again — use a separate render pass per window.

        // Collect window IDs to avoid borrow issues
        let window_ids: Vec<WindowId> = self.window_registry.iter()
            .map(|(id, _)| *id)
            .collect();

        for window_id in window_ids {
            let ws = match self.window_registry.get_mut(&window_id) {
                Some(ws) => ws,
                None => continue,
            };

            let surface_texture = match ws.surface.get_current_texture() {
                Ok(t) => t,
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    ws.surface.resize(&gpu.device, ws.surface.width, ws.surface.height, ws.surface.scale_factor);
                    continue;
                }
                Err(e) => {
                    log::error!("Surface error: {e}");
                    continue;
                }
            };

            let surface_view = surface_texture.texture.create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blit Encoder"),
            });

            // Get compositor output — re-render a quick pass to get the view
            // Since ClearColorCompositor alternates ping/pong, we need to read the last-rendered target.
            // The compositor's render() returns a reference, but we can't hold it across the borrow.
            // Instead, blit from the compositor's current output by rendering again with same frame.
            // Better approach: just clear directly to the surface for now, and refactor in Phase 4.

            // For Phase 3: render a clear color directly to each surface
            let hue = (self.engine.current_beat() * 0.05) % 1.0;
            let color = manifold_core::color::Color::hsv_to_rgb(hue, 0.7, 0.9);

            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Surface Clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: color.r as f64,
                                g: color.g as f64,
                                b: color.b as f64,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }

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

        // Create GPU context with primary window's surface for compatibility
        let gpu = {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                ..Default::default()
            });

            let surface = instance.create_surface(window.clone()).expect("Failed to create surface");

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
                    .request_device(&wgpu::DeviceDescriptor {
                        label: Some("MANIFOLD Device"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::default(),
                        memory_hints: wgpu::MemoryHints::Performance,
                    }, None)
                    .await
                    .expect("Failed to create device");

                (instance, adapter, device, queue, surface)
            });

            let (instance, adapter, device, queue, surface) = gpu;

            // Configure the surface
            let caps = surface.get_capabilities(&adapter);
            let format = caps.formats.iter()
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
            self.window_registry.add(wid, WindowState {
                window,
                surface: surface_wrapper,
                role: WindowRole::Workspace,
                display_index: None,
            });

            // Create blit pipeline
            self.blit_pipeline = Some(BlitPipeline::new(&device, format));

            // Create compositor
            let output_w = 1920u32;
            let output_h = 1080u32;
            self.compositor = Some(Box::new(ClearColorCompositor::new(&device, output_w, output_h)));

            GpuContext { instance, adapter, device, queue }
        };

        self.gpu = Some(gpu);
        self.initialized = true;

        log::info!("Initialized. Press Space=play/pause, O=output window, Escape=close output");
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
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
                        ws.surface.resize(&gpu.device, size.width, size.height, scale);
                    }
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = &self.gpu {
                    if let Some(ws) = self.window_registry.get_mut(&window_id) {
                        let size = ws.window.inner_size();
                        ws.surface.resize(&gpu.device, size.width, size.height, scale_factor);
                    }
                }
            }

            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } => {
                match logical_key {
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
                        self.open_output_window(
                            event_loop,
                            &format!("Output {}", count),
                            None,
                        );
                    }
                    Key::Named(NamedKey::Escape) => {
                        // Close output windows only (not workspace)
                        if Some(window_id) != self.primary_window_id {
                            self.window_registry.remove(&window_id);
                            log::info!("Closed output window");
                        }
                    }
                    _ => {}
                }
            }

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

        // Always request redraw for continuous rendering
        for window in self.window_registry.window_arcs().cloned().collect::<Vec<_>>() {
            window.request_redraw();
        }
    }
}
