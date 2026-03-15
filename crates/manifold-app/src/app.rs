use std::collections::HashSet;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_core::project::Project;
use manifold_core::types::{BlendMode, GeneratorType, LayerType, PlaybackState};
use manifold_core::layer::Layer;
use manifold_core::clip::TimelineClip;
use manifold_core::generator::GeneratorParamState;
use manifold_editing::service::EditingService;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::StubRenderer;
use manifold_renderer::blit::BlitPipeline;
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::{CompositeClipDescriptor, LayerCompositor};
use manifold_renderer::surface::SurfaceWrapper;
use manifold_renderer::ui_renderer::UIRenderer;

use manifold_ui::input::{Modifiers, PointerAction};
use manifold_ui::node::Vec2;

use crate::frame_timer::FrameTimer;
use crate::ui_root::UIRoot;
use crate::window_registry::{WindowRegistry, WindowRole, WindowState};

/// Selection state for the timeline viewport.
pub struct SelectionState {
    pub selected_clip_ids: HashSet<String>,
    pub primary_clip_id: Option<String>,
    pub insert_cursor_beat: Option<f32>,
    pub insert_cursor_layer: Option<usize>,
    pub version: u64,
}

impl SelectionState {
    pub fn new() -> Self {
        Self {
            selected_clip_ids: HashSet::new(),
            primary_clip_id: None,
            insert_cursor_beat: None,
            insert_cursor_layer: None,
            version: 0,
        }
    }

    /// Clear all selection state, bump version.
    pub fn clear(&mut self) {
        self.selected_clip_ids.clear();
        self.primary_clip_id = None;
        self.version += 1;
    }

    /// Select a single clip (replaces existing selection).
    pub fn select_single(&mut self, clip_id: String) {
        self.selected_clip_ids.clear();
        self.selected_clip_ids.insert(clip_id.clone());
        self.primary_clip_id = Some(clip_id);
        self.version += 1;
    }

    /// Toggle a clip in the selection (Ctrl+Click).
    pub fn toggle(&mut self, clip_id: String) {
        if self.selected_clip_ids.contains(&clip_id) {
            self.selected_clip_ids.remove(&clip_id);
            if self.primary_clip_id.as_ref() == Some(&clip_id) {
                self.primary_clip_id = self.selected_clip_ids.iter().next().cloned();
            }
        } else {
            self.selected_clip_ids.insert(clip_id.clone());
            self.primary_clip_id = Some(clip_id);
        }
        self.version += 1;
    }

    /// Set insert cursor position.
    pub fn set_insert_cursor(&mut self, beat: f32, layer: usize) {
        self.insert_cursor_beat = Some(beat);
        self.insert_cursor_layer = Some(layer);
        self.version += 1;
    }
}

/// Active drag mode for timeline clip interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipDragMode {
    None,
    Move,
    TrimLeft,
    TrimRight,
    RegionSelect,
}

/// Snapshot of a clip's state at drag start (for undo).
#[derive(Debug, Clone)]
pub struct ClipDragSnapshot {
    pub clip_id: String,
    pub original_start_beat: f32,
    pub original_layer_index: i32,
}

/// State for an active clip drag operation.
pub struct ClipDragState {
    pub mode: ClipDragMode,
    pub anchor_clip_id: String,
    pub anchor_beat: f32,
    pub snapshots: Vec<ClipDragSnapshot>,
    // For trim:
    pub trim_old_start: f32,
    pub trim_old_duration: f32,
    pub trim_old_in_point: f32,
    // For region select:
    pub region_anchor_beat: f32,
    pub region_anchor_layer: usize,
}

impl ClipDragState {
    pub fn new() -> Self {
        Self {
            mode: ClipDragMode::None,
            anchor_clip_id: String::new(),
            anchor_beat: 0.0,
            snapshots: Vec::new(),
            trim_old_start: 0.0,
            trim_old_duration: 0.0,
            trim_old_in_point: 0.0,
            region_anchor_beat: 0.0,
            region_anchor_layer: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.mode != ClipDragMode::None
    }
}

pub struct Application {
    // GPU
    gpu: Option<GpuContext>,

    // Windows
    window_registry: WindowRegistry,
    primary_window_id: Option<WindowId>,

    // Engine
    engine: PlaybackEngine,
    editing_service: EditingService,

    // Selection + drag state
    selection: SelectionState,
    clip_drag: ClipDragState,
    active_layer_index: Option<usize>,
    drag_snapshot: Option<f32>,

    // Rendering
    generator_renderer: Option<GeneratorRenderer>,
    compositor: Option<Box<dyn Compositor>>,
    blit_pipeline: Option<BlitPipeline>,
    ui_renderer: Option<UIRenderer>,
    surface_format: wgpu::TextureFormat,

    // UI
    ui_root: UIRoot,

    // Frame timing
    frame_timer: FrameTimer,
    frame_count: u64,

    // Lifecycle tracking for generator renderer sync
    prev_active_clip_ids: HashSet<String>,

    // Input state for winit → UIInputSystem translation
    cursor_pos: Vec2,
    mouse_pressed: bool,
    modifiers: Modifiers,
    time_since_start: f32,

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
            editing_service: EditingService::new(),
            selection: SelectionState::new(),
            clip_drag: ClipDragState::new(),
            active_layer_index: None,
            drag_snapshot: None,
            generator_renderer: None,
            compositor: None,
            blit_pipeline: None,
            ui_renderer: None,
            surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            ui_root: UIRoot::new(),
            frame_timer: FrameTimer::new(60.0),
            frame_count: 0,
            prev_active_clip_ids: HashSet::with_capacity(16),
            cursor_pos: Vec2::ZERO,
            mouse_pressed: false,
            modifiers: Modifiers {
                shift: false,
                ctrl: false,
                alt: false,
                command: false,
            },
            time_since_start: 0.0,
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
        self.time_since_start = realtime as f32;

        // 1. Process UI events and dispatch actions
        let actions = self.ui_root.process_events();
        let mut needs_structural_sync = false;
        let prev_active_layer = self.active_layer_index;
        for action in &actions {
            let result = crate::ui_bridge::dispatch(
                action,
                &mut self.engine,
                &mut self.editing_service,
                &mut self.ui_root,
                &mut self.selection,
                &mut self.clip_drag,
                &mut self.active_layer_index,
                &mut self.drag_snapshot,
            );
            if result.structural_change {
                needs_structural_sync = true;
            }
        }
        if needs_structural_sync {
            crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.engine);
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index);
        } else if self.active_layer_index != prev_active_layer {
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index);
        }

        // 2. Push engine state to UI panels
        crate::ui_bridge::push_state(&mut self.ui_root, &self.engine, self.active_layer_index, &self.selection);
        self.ui_root.update();

        // 3. Tick the engine
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

        // 4. Sync generator renderer lifecycle with engine
        let mut current_active: HashSet<String> = HashSet::with_capacity(tick_result.ready_clips.len());
        for clip in &tick_result.ready_clips {
            if clip.is_generator() {
                current_active.insert(clip.id.clone());

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

        for old_id in &self.prev_active_clip_ids {
            if !current_active.contains(old_id) {
                gen_renderer.stop_clip(old_id);
            }
        }
        self.prev_active_clip_ids = current_active;

        // 5. Render all generators
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

        // 6. Build clip descriptors for compositor
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
                    effects: &clip.effects,
                    effect_groups: clip.effect_groups.as_deref().unwrap_or(&[]),
                });
            }
        }

        // 7. Build layer descriptors for compositor
        let empty_effects: Vec<manifold_core::effects::EffectInstance> = Vec::new();
        let empty_groups: Vec<manifold_core::effects::EffectGroup> = Vec::new();
        let layer_descs: Vec<CompositeLayerDescriptor> = layers.iter().map(|layer| {
            CompositeLayerDescriptor {
                layer_index: layer.index,
                blend_mode: layer.default_blend_mode,
                opacity: layer.opacity,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                effects: layer.effects.as_deref().unwrap_or(&empty_effects),
                effect_groups: layer.effect_groups.as_deref().unwrap_or(&empty_groups),
            }
        }).collect();

        // 8. Composite
        let compositor = match &mut self.compositor {
            Some(c) => c,
            None => return,
        };

        let project = self.engine.project();
        let master_effects = project.map_or(&empty_effects[..], |p| &p.settings.master_effects);
        let master_effect_groups = project
            .and_then(|p| p.settings.master_effect_groups.as_deref())
            .unwrap_or(&empty_groups);

        let frame = CompositorFrame {
            time: self.engine.current_time(),
            beat: self.engine.current_beat(),
            dt: dt as f32,
            frame_count: self.frame_count,
            compositor_dirty: tick_result.compositor_dirty,
            clips: &clip_descs,
            layers: &layer_descs,
            master_effects,
            master_effect_groups,
        };

        let output_view = compositor.render(&gpu.device, &gpu.queue, &mut encoder, &frame);

        // 8. Submit generator + compositor work
        let output_view_ptr: *const wgpu::TextureView = output_view;
        gpu.queue.submit(std::iter::once(encoder.finish()));

        // 9. Present to all windows via blit + UI overlay
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
            let is_workspace = Some(window_id) == self.primary_window_id;

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

            let surface_w = ws.surface.width;
            let surface_h = ws.surface.height;
            let scale = ws.surface.scale_factor;

            let mut encoder =
                gpu.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Blit Encoder"),
                    });

            blit.blit(&gpu.device, &mut encoder, compositor_output, &surface_view);

            // Draw UI overlay on workspace window using the UITree
            // Pass logical pixel dimensions — the tree is built in logical coords
            if is_workspace {
                if let Some(ui) = &mut self.ui_renderer {
                    let logical_w = (surface_w as f64 / scale) as u32;
                    let logical_h = (surface_h as f64 / scale) as u32;
                    ui.render_tree(&self.ui_root.tree);
                    ui.render(
                        &gpu.device,
                        &gpu.queue,
                        &mut encoder,
                        &surface_view,
                        logical_w,
                        logical_h,
                        scale,
                    );
                }
            }

            gpu.queue.submit(std::iter::once(encoder.finish()));
            surface_texture.present();
        }
    }

    /// Convert a winit key to a manifold_ui Key.
    fn convert_key(logical_key: &Key) -> Option<manifold_ui::input::Key> {
        match logical_key {
            Key::Named(named) => match named {
                NamedKey::Space => Some(manifold_ui::input::Key::Space),
                NamedKey::Enter => Some(manifold_ui::input::Key::Enter),
                NamedKey::Escape => Some(manifold_ui::input::Key::Escape),
                NamedKey::Backspace => Some(manifold_ui::input::Key::Backspace),
                NamedKey::Delete => Some(manifold_ui::input::Key::Delete),
                NamedKey::Tab => Some(manifold_ui::input::Key::Tab),
                NamedKey::ArrowLeft => Some(manifold_ui::input::Key::Left),
                NamedKey::ArrowRight => Some(manifold_ui::input::Key::Right),
                NamedKey::ArrowUp => Some(manifold_ui::input::Key::Up),
                NamedKey::ArrowDown => Some(manifold_ui::input::Key::Down),
                NamedKey::Home => Some(manifold_ui::input::Key::Home),
                NamedKey::End => Some(manifold_ui::input::Key::End),
                NamedKey::F1 => Some(manifold_ui::input::Key::F1),
                NamedKey::F2 => Some(manifold_ui::input::Key::F2),
                NamedKey::F3 => Some(manifold_ui::input::Key::F3),
                NamedKey::F4 => Some(manifold_ui::input::Key::F4),
                NamedKey::F5 => Some(manifold_ui::input::Key::F5),
                NamedKey::F6 => Some(manifold_ui::input::Key::F6),
                NamedKey::F7 => Some(manifold_ui::input::Key::F7),
                NamedKey::F8 => Some(manifold_ui::input::Key::F8),
                NamedKey::F9 => Some(manifold_ui::input::Key::F9),
                NamedKey::F10 => Some(manifold_ui::input::Key::F10),
                NamedKey::F11 => Some(manifold_ui::input::Key::F11),
                NamedKey::F12 => Some(manifold_ui::input::Key::F12),
                _ => None,
            },
            Key::Character(c) => {
                let ch = c.chars().next()?;
                match ch.to_ascii_lowercase() {
                    'a' => Some(manifold_ui::input::Key::A),
                    'b' => Some(manifold_ui::input::Key::B),
                    'c' => Some(manifold_ui::input::Key::C),
                    'd' => Some(manifold_ui::input::Key::D),
                    'e' => Some(manifold_ui::input::Key::E),
                    'f' => Some(manifold_ui::input::Key::F),
                    'g' => Some(manifold_ui::input::Key::G),
                    'h' => Some(manifold_ui::input::Key::H),
                    'i' => Some(manifold_ui::input::Key::I),
                    'j' => Some(manifold_ui::input::Key::J),
                    'k' => Some(manifold_ui::input::Key::K),
                    'l' => Some(manifold_ui::input::Key::L),
                    'm' => Some(manifold_ui::input::Key::M),
                    'n' => Some(manifold_ui::input::Key::N),
                    'o' => Some(manifold_ui::input::Key::O),
                    'p' => Some(manifold_ui::input::Key::P),
                    'q' => Some(manifold_ui::input::Key::Q),
                    'r' => Some(manifold_ui::input::Key::R),
                    's' => Some(manifold_ui::input::Key::S),
                    't' => Some(manifold_ui::input::Key::T),
                    'u' => Some(manifold_ui::input::Key::U),
                    'v' => Some(manifold_ui::input::Key::V),
                    'w' => Some(manifold_ui::input::Key::W),
                    'x' => Some(manifold_ui::input::Key::X),
                    'y' => Some(manifold_ui::input::Key::Y),
                    'z' => Some(manifold_ui::input::Key::Z),
                    '0' => Some(manifold_ui::input::Key::Num0),
                    '1' => Some(manifold_ui::input::Key::Num1),
                    '2' => Some(manifold_ui::input::Key::Num2),
                    '3' => Some(manifold_ui::input::Key::Num3),
                    '4' => Some(manifold_ui::input::Key::Num4),
                    '5' => Some(manifold_ui::input::Key::Num5),
                    '6' => Some(manifold_ui::input::Key::Num6),
                    '7' => Some(manifold_ui::input::Key::Num7),
                    '8' => Some(manifold_ui::input::Key::Num8),
                    '9' => Some(manifold_ui::input::Key::Num9),
                    '-' => Some(manifold_ui::input::Key::Minus),
                    '+' | '=' => Some(manifold_ui::input::Key::Plus),
                    '.' => Some(manifold_ui::input::Key::Period),
                    ',' => Some(manifold_ui::input::Key::Comma),
                    '/' => Some(manifold_ui::input::Key::Slash),
                    _ => None,
                }
            }
            _ => None,
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
                            trace: wgpu::Trace::Off,
                            ..Default::default()
                        },
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

            // Store surface format for UI renderer
            self.surface_format = format;

            // Create blit pipeline
            self.blit_pipeline = Some(BlitPipeline::new(&device, format));

            // Create UI renderer (renders directly to surface in surface format)
            self.ui_renderer = Some(UIRenderer::new(&device, &queue, format));

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

        // Build UI at initial window size (logical pixels)
        let logical_w = size.width as f32 / scale as f32;
        let logical_h = size.height as f32 / scale as f32;
        self.ui_root.resize(logical_w, logical_h);

        // Push initial project data (layers, tracks) and rebuild
        crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.engine);
        crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index);

        self.initialized = true;

        log::info!(
            "Initialized. UI built at {:.0}x{:.0}. Press Space=play/pause, O=output window",
            logical_w,
            logical_h,
        );
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let is_primary = Some(window_id) == self.primary_window_id;

        match event {
            WindowEvent::CloseRequested => {
                if is_primary {
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

                        // Rebuild UI on primary window resize
                        if is_primary {
                            let logical_w = size.width as f32 / scale as f32;
                            let logical_h = size.height as f32 / scale as f32;
                            self.ui_root.resize(logical_w, logical_h);
                        }
                    }
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = &self.gpu {
                    if let Some(ws) = self.window_registry.get_mut(&window_id) {
                        let size = ws.window.inner_size();
                        ws.surface
                            .resize(&gpu.device, size.width, size.height, scale_factor);

                        if is_primary {
                            let logical_w = size.width as f32 / scale_factor as f32;
                            let logical_h = size.height as f32 / scale_factor as f32;
                            self.ui_root.resize(logical_w, logical_h);
                        }
                    }
                }
            }

            // ── Pointer input → UIInputSystem ──────────────────────
            WindowEvent::CursorMoved { position, .. } => {
                if is_primary {
                    // Convert to logical pixels
                    let scale = self.window_registry.get(&window_id)
                        .map(|ws| ws.window.scale_factor())
                        .unwrap_or(1.0);
                    self.cursor_pos = Vec2::new(
                        position.x as f32 / scale as f32,
                        position.y as f32 / scale as f32,
                    );
                    self.ui_root.pointer_event(
                        self.cursor_pos,
                        PointerAction::Move,
                        self.time_since_start,
                    );
                }
            }

            WindowEvent::MouseInput { button, state, .. } => {
                if is_primary {
                    match button {
                        MouseButton::Left => {
                            match state {
                                ElementState::Pressed => {
                                    self.mouse_pressed = true;
                                    self.ui_root.pointer_event(
                                        self.cursor_pos,
                                        PointerAction::Down,
                                        self.time_since_start,
                                    );
                                }
                                ElementState::Released => {
                                    self.mouse_pressed = false;
                                    self.ui_root.pointer_event(
                                        self.cursor_pos,
                                        PointerAction::Up,
                                        self.time_since_start,
                                    );
                                }
                            }
                        }
                        MouseButton::Right => {
                            if state == ElementState::Pressed {
                                self.ui_root.right_click(self.cursor_pos);
                            }
                        }
                        _ => {}
                    }
                }
            }

            // ── Modifier tracking ──────────────────────────────────
            WindowEvent::ModifiersChanged(mods) => {
                let state = mods.state();
                self.modifiers = Modifiers {
                    shift: state.shift_key(),
                    ctrl: state.control_key(),
                    alt: state.alt_key(),
                    command: state.super_key(),
                };
                self.ui_root.input.set_modifiers(self.modifiers);
            }

            // ── Keyboard input ─────────────────────────────────────
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                // App-level shortcuts (handled before UI forwarding)
                let mut consumed = false;
                if is_primary {
                    match &logical_key {
                        Key::Named(NamedKey::Space) => {
                            if self.engine.is_playing() {
                                self.engine.set_state(PlaybackState::Paused);
                            } else {
                                self.engine.set_state(PlaybackState::Playing);
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::Escape) => {
                            // Clear selection first; if already clear, stop
                            if !self.selection.selected_clip_ids.is_empty() {
                                self.selection.clear();
                            } else {
                                self.engine.set_state(PlaybackState::Stopped);
                                self.engine.seek_to(0.0);
                            }
                            consumed = true;
                        }
                        // ── Undo/Redo ──
                        Key::Character(ref c) if c.as_str() == "z" && self.modifiers.command => {
                            if self.modifiers.shift {
                                crate::ui_bridge::redo(&mut self.engine, &mut self.editing_service);
                            } else {
                                crate::ui_bridge::undo(&mut self.engine, &mut self.editing_service);
                            }
                            consumed = true;
                        }
                        Key::Character(ref c) if c.as_str() == "y" && self.modifiers.command => {
                            crate::ui_bridge::redo(&mut self.engine, &mut self.editing_service);
                            consumed = true;
                        }
                        // ── File ──
                        Key::Character(ref c) if c.as_str() == "s" && self.modifiers.command => {
                            log::info!("Save project (Cmd+S) — not yet implemented");
                            consumed = true;
                        }
                        // ── Delete selected clips ──
                        Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace)
                            if !self.modifiers.command =>
                        {
                            if !self.selection.selected_clip_ids.is_empty() {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = EditingService::delete_clips(project, &ids);
                                    self.editing_service.execute_batch(commands, "Delete clips".into(), project);
                                }
                                self.selection.clear();
                            }
                            consumed = true;
                        }
                        // ── Split at playhead ──
                        Key::Character(ref c) if c.as_str() == "s" && !self.modifiers.command => {
                            let beat = self.engine.current_beat();
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                if let Some(project) = self.engine.project_mut() {
                                    let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
                                    for id in &ids {
                                        if let Some(cmd) = EditingService::split_clip_at_beat(project, id, beat) {
                                            commands.push(cmd);
                                        }
                                    }
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Split clips".into(), project);
                                    }
                                }
                            }
                            consumed = true;
                        }
                        // ── Extend / Shrink by grid step ──
                        Key::Character(ref c) if c.as_str() == "e" && !self.modifiers.command => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                let step = self.ui_root.viewport.grid_step();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = if self.modifiers.shift {
                                        EditingService::shrink_clips_by_grid(project, &ids, step)
                                    } else {
                                        EditingService::extend_clips_by_grid(project, &ids, step)
                                    };
                                    if !commands.is_empty() {
                                        let desc = if self.modifiers.shift { "Shrink clips" } else { "Extend clips" };
                                        self.editing_service.execute_batch(commands, desc.into(), project);
                                    }
                                }
                            }
                            consumed = true;
                        }
                        // ── Toggle mute on selected clips ──
                        Key::Character(ref c) if c.as_str() == "0" && !self.modifiers.command => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if let Some(project) = self.engine.project_mut() {
                                for id in &ids {
                                    if let Some(clip) = project.timeline.find_clip_by_id_mut(id) {
                                        clip.is_muted = !clip.is_muted;
                                    }
                                }
                            }
                            consumed = true;
                        }
                        // ── Select all (Cmd+A) ──
                        Key::Character(ref c) if c.as_str() == "a" && self.modifiers.command => {
                            if let Some(project) = self.engine.project() {
                                self.selection.selected_clip_ids.clear();
                                for layer in &project.timeline.layers {
                                    for clip in &layer.clips {
                                        self.selection.selected_clip_ids.insert(clip.id.clone());
                                    }
                                }
                                self.selection.primary_clip_id = self.selection.selected_clip_ids.iter().next().cloned();
                                self.selection.version += 1;
                            }
                            consumed = true;
                        }
                        // ── Copy (Cmd+C) ──
                        Key::Character(ref c) if c.as_str() == "c" && self.modifiers.command => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                if let Some(project) = self.engine.project() {
                                    self.editing_service.copy_clips(project, &ids);
                                }
                            }
                            consumed = true;
                        }
                        // ── Cut (Cmd+X) ──
                        Key::Character(ref c) if c.as_str() == "x" && self.modifiers.command => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                if let Some(project) = self.engine.project_mut() {
                                    self.editing_service.copy_clips(project, &ids);
                                    let commands = EditingService::delete_clips(project, &ids);
                                    self.editing_service.execute_batch(commands, "Cut clips".into(), project);
                                }
                                self.selection.clear();
                            }
                            consumed = true;
                        }
                        // ── Paste (Cmd+V) ──
                        Key::Character(ref c) if c.as_str() == "v" && self.modifiers.command => {
                            let target_beat = self.selection.insert_cursor_beat
                                .unwrap_or(self.engine.current_beat());
                            let target_layer = self.selection.insert_cursor_layer
                                .or(self.active_layer_index)
                                .unwrap_or(0) as i32;
                            if let Some(project) = self.engine.project_mut() {
                                let result = self.editing_service.paste_clips(project, target_beat, target_layer);
                                if !result.commands.is_empty() {
                                    self.editing_service.execute_batch(result.commands, "Paste clips".into(), project);
                                    // Select pasted clips
                                    self.selection.selected_clip_ids.clear();
                                    for id in result.pasted_clip_ids {
                                        self.selection.selected_clip_ids.insert(id);
                                    }
                                    self.selection.primary_clip_id = self.selection.selected_clip_ids.iter().next().cloned();
                                    self.selection.version += 1;
                                }
                            }
                            consumed = true;
                        }
                        // ── Duplicate (Cmd+D) ──
                        Key::Character(ref c) if c.as_str() == "d" && self.modifiers.command => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                // Use a default region spanning the selected clips
                                let region = manifold_core::selection::SelectionRegion::default();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = EditingService::duplicate_clips(project, &ids, &region);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Duplicate clips".into(), project);
                                    }
                                }
                            }
                            consumed = true;
                        }
                        // ── Arrow keys — nudge clips or seek ──
                        Key::Named(NamedKey::ArrowLeft) if !self.modifiers.command => {
                            let step = if self.modifiers.shift { 0.0625 } else { 0.25 };
                            if !self.selection.selected_clip_ids.is_empty() {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = EditingService::nudge_clips(project, &ids, -step);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Nudge clips left".into(), project);
                                    }
                                }
                            } else {
                                let beat = (self.engine.current_beat() - step).max(0.0);
                                if let Some(p) = self.engine.project() {
                                    let time = beat * (60.0 / p.settings.bpm);
                                    self.engine.seek_to(time);
                                }
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowRight) if !self.modifiers.command => {
                            let step = if self.modifiers.shift { 0.0625 } else { 0.25 };
                            if !self.selection.selected_clip_ids.is_empty() {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = EditingService::nudge_clips(project, &ids, step);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Nudge clips right".into(), project);
                                    }
                                }
                            } else {
                                let beat = self.engine.current_beat() + step;
                                if let Some(p) = self.engine.project() {
                                    let time = beat * (60.0 / p.settings.bpm);
                                    self.engine.seek_to(time);
                                }
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowUp) if !self.modifiers.command => {
                            if let Some(idx) = self.active_layer_index {
                                if idx > 0 {
                                    self.active_layer_index = Some(idx - 1);
                                }
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowDown) if !self.modifiers.command => {
                            if let Some(idx) = self.active_layer_index {
                                let count = self.engine.project().map_or(0, |p| p.timeline.layers.len());
                                if idx + 1 < count {
                                    self.active_layer_index = Some(idx + 1);
                                }
                            }
                            consumed = true;
                        }
                        // ── I — set/clear export in point ──
                        Key::Character(ref c) if c.as_str() == "i" && !self.modifiers.command => {
                            let beat = self.engine.current_beat();
                            if let Some(project) = self.engine.project_mut() {
                                if self.modifiers.alt {
                                    project.timeline.export_in_beat = 0.0;
                                    project.timeline.export_range_enabled = false;
                                } else {
                                    project.timeline.export_in_beat = beat;
                                    project.timeline.export_range_enabled = true;
                                }
                            }
                            consumed = true;
                        }
                        // ── Home/End — seek to start/end ──
                        Key::Named(NamedKey::Home) => {
                            self.engine.seek_to(0.0);
                            consumed = true;
                        }
                        Key::Named(NamedKey::End) => {
                            if let Some(project) = self.engine.project() {
                                let max_beat = project.timeline.layers.iter()
                                    .flat_map(|l| l.clips.iter())
                                    .map(|c| c.start_beat + c.duration_beats)
                                    .fold(0.0_f32, f32::max);
                                let time = max_beat * (60.0 / project.settings.bpm);
                                self.engine.seek_to(time);
                            }
                            consumed = true;
                        }
                        _ => {}
                    }
                }

                // Forward to UI input system (unless consumed by app shortcut)
                if is_primary && !consumed {
                    if let Some(ui_key) = Self::convert_key(&logical_key) {
                        self.ui_root.key_event(ui_key, self.modifiers);
                    }
                }

                // App-level shortcuts (output window management)
                match &logical_key {
                    Key::Character(ref c) if c.as_str() == "o" || c.as_str() == "O" => {
                        let count = self.window_registry.len();
                        self.open_output_window(event_loop, &format!("Output {}", count), None);
                    }
                    Key::Named(NamedKey::Escape) => {
                        if !is_primary {
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
