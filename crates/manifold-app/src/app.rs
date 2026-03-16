use std::collections::HashSet;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_core::project::Project;
use manifold_core::types::{BlendMode, LayerType};
use manifold_core::layer::Layer;
use manifold_editing::commands::layer::DeleteLayerCommand;
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
use manifold_ui::panels::PanelAction;

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
        self.insert_cursor_beat = None;
        self.insert_cursor_layer = None;
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

    // File I/O
    current_project_path: Option<std::path::PathBuf>,

    // Text input
    text_input: crate::text_input::TextInputState,

    // State
    initialized: bool,
    needs_rebuild: bool,
    /// Set by keyboard shortcuts that mutate project data (undo, delete, etc.).
    /// Consumed by tick_and_render to trigger sync_project_data + rebuild.
    needs_structural_sync: bool,
}

impl Application {
    pub fn new() -> Self {
        // Create engine with stub renderers for lifecycle tracking
        let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
            Box::new(StubRenderer::new_video()),
            Box::new(StubRenderer::new_generator()),
        ];
        let mut engine = PlaybackEngine::new(renderers);

        // Create default project with one empty video layer
        let project = Self::create_default_project();
        engine.initialize(project);

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
            current_project_path: None,
            text_input: crate::text_input::TextInputState::new(),
            initialized: false,
            needs_rebuild: false,
            needs_structural_sync: false,
        }
    }

    fn create_default_project() -> Project {
        let mut project = Project::default();
        project.settings.bpm = 120.0;
        project.settings.time_signature_numerator = 4;

        // One empty video layer (matches Unity startup behavior)
        let layer = Layer::new("Layer 1".to_string(), LayerType::Video, 0);
        project.timeline.layers.push(layer);

        project
    }

    /// Navigate the insert cursor using the cursor_nav module.
    /// Handles Left/Right/Up/Down with auto-select and collapsed-layer skipping.
    fn navigate_cursor(&mut self, direction: manifold_ui::cursor_nav::Direction) {
        use manifold_ui::cursor_nav::{navigate_cursor, NavResult, NavLayerInfo, NavClipInfo};

        let current_beat = self.selection.insert_cursor_beat.unwrap_or(self.engine.current_beat());
        let current_layer = self.selection.insert_cursor_layer
            .or(self.active_layer_index)
            .unwrap_or(0);
        let grid_interval = 0.25; // default 16th note grid

        // Build layer info for navigation (skip collapsed layers)
        let layers: Vec<NavLayerInfo> = self.engine.project()
            .map(|p| p.timeline.layers.iter().enumerate().map(|(i, l)| {
                NavLayerInfo {
                    index: i,
                    height: if l.is_collapsed { 0.0 } else { 140.0 },
                }
            }).collect())
            .unwrap_or_default();

        // Build clip info for auto-select
        let clips: Vec<NavClipInfo> = self.engine.project()
            .map(|p| p.timeline.layers.iter().enumerate().flat_map(|(li, l)| {
                l.clips.iter().map(move |c| NavClipInfo {
                    clip_id: c.id.clone(),
                    layer_index: li,
                    start_beat: c.start_beat,
                    end_beat: c.start_beat + c.duration_beats,
                })
            }).collect())
            .unwrap_or_default();

        match navigate_cursor(
            direction, current_beat, current_layer, grid_interval,
            self.modifiers.shift, &layers, &clips,
        ) {
            NavResult::SelectClip(clip_id) => {
                self.selection.clear();
                self.selection.selected_clip_ids.insert(clip_id.clone());
                self.selection.primary_clip_id = Some(clip_id);
                self.needs_rebuild = true;
            }
            NavResult::SetCursor { beat, layer } => {
                self.selection.clear();
                self.selection.insert_cursor_beat = Some(beat);
                self.selection.insert_cursor_layer = Some(layer);
                self.active_layer_index = Some(layer);
                self.needs_rebuild = true;
            }
            NavResult::NoChange => {}
        }
    }

    /// Handle committed text input value.
    fn handle_text_input_commit(&mut self, field: crate::text_input::TextInputField, text: &str) {
        use crate::text_input::TextInputField;
        match field {
            TextInputField::Bpm => {
                if let Ok(bpm) = text.parse::<f32>() {
                    let bpm = bpm.clamp(20.0, 300.0);
                    if let Some(project) = self.engine.project_mut() {
                        let cmd = manifold_editing::commands::settings::ChangeBpmCommand::new(
                            project.settings.bpm, bpm,
                        );
                        self.editing_service.execute(Box::new(cmd), project);
                    }
                    self.needs_rebuild = true;
                }
            }
            TextInputField::Fps => {
                if let Ok(fps) = text.parse::<f32>() {
                    let fps = fps.clamp(1.0, 240.0);
                    if let Some(project) = self.engine.project_mut() {
                        let cmd = manifold_editing::commands::settings::ChangeFrameRateCommand::new(
                            project.settings.frame_rate, fps,
                        );
                        self.editing_service.execute(Box::new(cmd), project);
                    }
                    self.needs_rebuild = true;
                }
            }
            TextInputField::LayerName(idx) => {
                if let Some(project) = self.engine.project_mut() {
                    if let Some(layer) = project.timeline.layers.get_mut(idx) {
                        layer.name = text.to_string();
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::ClipBpm => {
                log::info!("Clip BPM input: {} (not yet wired)", text);
            }
        }
    }

    /// Save the current project. If no path exists, triggers Save As.
    fn save_project(&mut self) {
        if let Some(path) = &self.current_project_path {
            if let Some(project) = self.engine.project() {
                match manifold_io::saver::save_project(project, path) {
                    Ok(()) => {
                        self.editing_service.mark_clean();
                        log::info!("Saved to {}", path.display());
                    }
                    Err(e) => log::error!("Save failed: {e}"),
                }
            }
        } else {
            self.save_project_as();
        }
    }

    /// Save As — open native save dialog.
    fn save_project_as(&mut self) {
        let dialog = rfd::FileDialog::new()
            .set_title("Save Project")
            .add_filter("MANIFOLD Project", &["json", "manifold"])
            .set_file_name("project.json");

        if let Some(path) = dialog.save_file() {
            self.current_project_path = Some(path.clone());
            if let Some(project) = self.engine.project() {
                match manifold_io::saver::save_project(project, &path) {
                    Ok(()) => {
                        self.editing_service.mark_clean();
                        log::info!("Saved to {}", path.display());
                    }
                    Err(e) => log::error!("Save failed: {e}"),
                }
            }
        }
    }

    /// Open — native file dialog + load.
    fn open_project(&mut self) {
        let dialog = rfd::FileDialog::new()
            .set_title("Open Project")
            .add_filter("MANIFOLD Project", &["json", "manifold"]);

        if let Some(path) = dialog.pick_file() {
            match manifold_io::loader::load_project(&path) {
                Ok(project) => {
                    self.engine.initialize(project);
                    self.editing_service.set_project();
                    self.selection.clear();
                    self.active_layer_index = Some(0);
                    self.current_project_path = Some(path.clone());
                    self.needs_rebuild = true;
                    log::info!("Opened {}", path.display());
                }
                Err(e) => log::error!("Open failed: {e}"),
            }
        }
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

        // 1. Tick the engine FIRST so this frame's UI sees the advanced time
        let ctx = TickContext {
            dt_seconds: dt,
            realtime_now: realtime,
            pre_render_dt: dt as f32,
            frame_count: self.frame_count as i32,
        };
        let tick_result = self.engine.tick(ctx);

        // 2. Process UI events and dispatch actions
        let actions = self.ui_root.process_events();
        // Consume deferred structural sync flag (set by keyboard shortcuts)
        let mut needs_structural_sync = self.needs_structural_sync;
        self.needs_structural_sync = false;
        let prev_active_layer = self.active_layer_index;
        let prev_sel_version = self.selection.version;
        for action in &actions {
            // Intercept actions that need Application-level access
            match action {
                PanelAction::SaveProject => { self.save_project(); continue; }
                PanelAction::SaveProjectAs => { self.save_project_as(); continue; }
                PanelAction::OpenProject => { self.open_project(); needs_structural_sync = true; continue; }
                PanelAction::BpmFieldClicked => {
                    let bpm = self.engine.project().map_or(120.0, |p| p.settings.bpm);
                    self.text_input.begin(
                        crate::text_input::TextInputField::Bpm,
                        &format!("{:.1}", bpm),
                    );
                    continue;
                }
                PanelAction::FpsFieldClicked => {
                    let fps = self.engine.project().map_or(60.0, |p| p.settings.frame_rate);
                    self.text_input.begin(
                        crate::text_input::TextInputField::Fps,
                        &format!("{:.0}", fps),
                    );
                    continue;
                }
                PanelAction::NewProject => {
                    let project = Self::create_default_project();
                    self.engine.initialize(project);
                    self.editing_service.set_project();
                    self.selection.clear();
                    self.active_layer_index = Some(0);
                    self.current_project_path = None;
                    needs_structural_sync = true;
                    continue;
                }
                _ => {}
            }
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
        // Selection version change → sync inspector so it shows the newly selected clip
        if self.selection.version != prev_sel_version && !needs_structural_sync {
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index);
            needs_structural_sync = true;
        }

        if needs_structural_sync {
            crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.engine, self.active_layer_index);
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index);
        } else if self.active_layer_index != prev_active_layer {
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index);
            needs_structural_sync = true; // Inspector content changed — needs rebuild
        }
        // 2. Auto-scroll check (BEFORE build so rebuild includes new scroll)
        let scroll_changed = crate::ui_bridge::check_auto_scroll(&mut self.ui_root, &self.engine);

        // 3. Rebuild if needed
        if self.needs_rebuild || scroll_changed || needs_structural_sync {
            self.needs_rebuild = false;
            self.ui_root.build();
        }

        // 4. Push engine state to UI panels (AFTER build so new nodes get state)
        crate::ui_bridge::push_state(&mut self.ui_root, &self.engine, self.active_layer_index, &self.selection);

        // 5. Lightweight update (playhead, insert cursor, layer selection)
        self.ui_root.update();

        // tick_result was computed at the top of tick_and_render (engine ticked first)

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
        crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.engine, self.active_layer_index);
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

                    // Inspector resize drag takes priority
                    if self.ui_root.inspector_resize_dragging {
                        self.ui_root.update_inspector_resize(self.cursor_pos.x);
                    } else {
                        self.ui_root.pointer_event(
                            self.cursor_pos,
                            PointerAction::Move,
                            self.time_since_start,
                        );
                    }
                }
            }

            WindowEvent::MouseInput { button, state, .. } => {
                if is_primary {
                    match button {
                        MouseButton::Left => {
                            match state {
                                ElementState::Pressed => {
                                    self.mouse_pressed = true;
                                    // If a dropdown is open and the click lands outside it,
                                    // dismiss the dropdown and consume the event so that the
                                    // background node never receives a PointerDown (prevents
                                    // phantom pressed_id on the node behind the dropdown).
                                    if self.ui_root.dropdown.is_open()
                                        && !self.ui_root.dropdown.contains_point(self.cursor_pos)
                                    {
                                        self.ui_root.dropdown.close(&mut self.ui_root.tree);
                                        // Click is consumed by dismiss — do not forward.
                                    } else if self.ui_root.is_near_inspector_edge(self.cursor_pos) {
                                        self.ui_root.begin_inspector_resize(self.cursor_pos.x);
                                    } else {
                                        self.ui_root.pointer_event(
                                            self.cursor_pos,
                                            PointerAction::Down,
                                            self.time_since_start,
                                        );
                                    }
                                }
                                ElementState::Released => {
                                    self.mouse_pressed = false;
                                    if self.ui_root.inspector_resize_dragging {
                                        self.ui_root.end_inspector_resize();
                                    } else {
                                        self.ui_root.pointer_event(
                                            self.cursor_pos,
                                            PointerAction::Up,
                                            self.time_since_start,
                                        );
                                    }
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

            // ── Mouse wheel (scroll / zoom) ──────────────────────────
            WindowEvent::MouseWheel { delta, .. } => {
                if is_primary {
                    let (dx, dy) = match delta {
                        winit::event::MouseScrollDelta::LineDelta(x, y) => (x * 20.0, y * 20.0),
                        winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
                    };

                    let pos = self.cursor_pos;
                    let inspector_rect = self.ui_root.layout.inspector();
                    let tracks_rect = self.ui_root.layout.timeline_tracks();

                    if inspector_rect.contains(pos) {
                        // Scroll the inspector panel
                        self.ui_root.inspector.handle_scroll(dy);
                        self.needs_rebuild = true;
                    } else if tracks_rect.contains(pos) {
                        if self.modifiers.alt {
                            // Alt + scroll Y → zoom (step through zoom levels)
                            let anchor_beat = self.ui_root.viewport.pixel_to_beat(pos.x);
                            let current_ppb = self.ui_root.viewport.pixels_per_beat();
                            let levels = &manifold_ui::color::ZOOM_LEVELS;
                            let current_idx = levels.iter()
                                .position(|&l| (l - current_ppb).abs() < 0.01)
                                .unwrap_or_else(|| {
                                    levels.iter().enumerate()
                                        .min_by(|(_, a), (_, b)| {
                                            (*a - current_ppb).abs().partial_cmp(&(*b - current_ppb).abs()).unwrap()
                                        })
                                        .map(|(i, _)| i)
                                        .unwrap_or(0)
                                });
                            let new_idx = if dy > 0.0 {
                                current_idx.saturating_add(1).min(levels.len() - 1)
                            } else {
                                current_idx.saturating_sub(1)
                            };
                            if new_idx != current_idx {
                                let new_ppb = levels[new_idx];
                                // Anchor: keep the beat under cursor at the same screen X
                                let new_scroll = anchor_beat - (pos.x - tracks_rect.x) / new_ppb;
                                self.ui_root.viewport.set_zoom(new_ppb);
                                self.ui_root.viewport.set_scroll(
                                    new_scroll.max(0.0),
                                    self.ui_root.viewport.scroll_y_px(),
                                );
                                self.needs_rebuild = true;
                            }
                        } else if self.modifiers.shift {
                            // Shift + scroll Y → horizontal pan
                            let ppb = self.ui_root.viewport.pixels_per_beat();
                            let beat_delta = dy * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                            let new_x = (self.ui_root.viewport.scroll_x_beats() - beat_delta).max(0.0);
                            self.ui_root.viewport.set_scroll(
                                new_x,
                                self.ui_root.viewport.scroll_y_px(),
                            );
                            self.needs_rebuild = true;
                        } else {
                            // Plain scroll → vertical track scroll
                            let new_y = (self.ui_root.viewport.scroll_y_px() - dy).max(0.0);
                            self.ui_root.viewport.set_scroll(
                                self.ui_root.viewport.scroll_x_beats(),
                                new_y,
                            );
                            // Sync layer headers with viewport vertical scroll
                            self.ui_root.layer_headers.set_scroll_y(
                                self.ui_root.viewport.scroll_y_px(),
                            );
                            self.needs_rebuild = true;
                        }
                        // Native horizontal scroll (trackpad two-finger swipe)
                        if dx.abs() > 0.01 && !self.modifiers.alt {
                            let ppb = self.ui_root.viewport.pixels_per_beat();
                            let beat_delta = dx * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                            let new_x = (self.ui_root.viewport.scroll_x_beats() - beat_delta).max(0.0);
                            self.ui_root.viewport.set_scroll(
                                new_x,
                                self.ui_root.viewport.scroll_y_px(),
                            );
                            self.needs_rebuild = true;
                        }
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
                let data_version_before = self.editing_service.data_version();
                if is_primary {
                    // Text input mode: intercept all keys for text editing
                    if self.text_input.active {
                        match &logical_key {
                            Key::Named(NamedKey::Escape) => {
                                self.text_input.cancel();
                                consumed = true;
                            }
                            Key::Named(NamedKey::Enter) => {
                                let (field, text) = self.text_input.commit();
                                self.handle_text_input_commit(field, &text);
                                consumed = true;
                            }
                            Key::Named(NamedKey::Backspace) => {
                                self.text_input.backspace();
                                consumed = true;
                            }
                            Key::Named(NamedKey::Delete) => {
                                self.text_input.delete();
                                consumed = true;
                            }
                            Key::Named(NamedKey::ArrowLeft) => {
                                self.text_input.move_left();
                                consumed = true;
                            }
                            Key::Named(NamedKey::ArrowRight) => {
                                self.text_input.move_right();
                                consumed = true;
                            }
                            Key::Character(ref c) => {
                                for ch in c.chars() {
                                    self.text_input.insert_char(ch);
                                }
                                consumed = true;
                            }
                            _ => { consumed = true; } // Suppress all other keys
                        }
                        // Skip normal shortcut processing when text input consumed the key
                        if consumed {
                            return;
                        }
                    }
                    match &logical_key {
                        Key::Named(NamedKey::Space) => {
                            if self.engine.is_playing() {
                                self.engine.pause();
                            } else {
                                self.engine.play();
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::Escape) => {
                            // 4-level priority chain (from INTERACTION_CONTRACT.md §28)
                            if self.ui_root.dropdown.is_open() {
                                // Level 1: dismiss dropdown/context menu
                                self.ui_root.dropdown.close(&mut self.ui_root.tree);
                            } else if !self.selection.selected_clip_ids.is_empty() {
                                // Level 4: clear all selection + insert cursor
                                self.selection.clear();
                            }
                            // Note: Escape never stops playback (contract says clear only)
                            consumed = true;
                        }
                        // ── Undo/Redo ──
                        Key::Character(ref c) if c.as_str() == "z" && self.modifiers.command => {
                            if self.modifiers.shift {
                                crate::ui_bridge::redo(&mut self.engine, &mut self.editing_service);
                            } else {
                                crate::ui_bridge::undo(&mut self.engine, &mut self.editing_service);
                            }
                            self.engine.mark_compositor_dirty(self.frame_timer.realtime_since_start());
                            self.needs_structural_sync = true;
                            consumed = true;
                        }
                        Key::Character(ref c) if c.as_str() == "y" && self.modifiers.command => {
                            crate::ui_bridge::redo(&mut self.engine, &mut self.editing_service);
                            self.engine.mark_compositor_dirty(self.frame_timer.realtime_since_start());
                            self.needs_structural_sync = true;
                            consumed = true;
                        }
                        // ── File ──
                        Key::Character(ref c) if c.as_str() == "s" && self.modifiers.command => {
                            self.save_project();
                            consumed = true;
                        }
                        Key::Character(ref c) if c.as_str() == "o" && self.modifiers.command => {
                            self.open_project();
                            consumed = true;
                        }
                        Key::Character(ref c) if c.as_str() == "n" && self.modifiers.command => {
                            // New project — reset to empty default
                            let project = Self::create_default_project();
                            self.engine.initialize(project);
                            self.editing_service.set_project();
                            self.selection.clear();
                            self.active_layer_index = Some(0);
                            self.needs_rebuild = true;
                            log::info!("New project created");
                            consumed = true;
                        }
                        // ── Delete selected clips ──
                        Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace)
                            if !self.modifiers.command =>
                        {
                            if !self.selection.selected_clip_ids.is_empty() {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = EditingService::delete_clips(project, &ids, None, 0.0);
                                    self.editing_service.execute_batch(commands, "Delete clips".into(), project);
                                }
                                self.selection.clear();
                            } else if let Some(idx) = self.active_layer_index {
                                // No clips selected — delete the active layer (if >1 layer)
                                if let Some(project) = self.engine.project_mut() {
                                    if project.timeline.layers.len() > 1 {
                                        if let Some(layer) = project.timeline.layers.get(idx) {
                                            let layer_clone = layer.clone();
                                            let cmd = DeleteLayerCommand::new(layer_clone, idx);
                                            self.editing_service.execute(Box::new(cmd), project);
                                            // Fix active_layer if out of bounds
                                            let new_count = project.timeline.layers.len();
                                            if idx >= new_count {
                                                self.active_layer_index = Some(new_count.saturating_sub(1));
                                            }
                                            self.needs_rebuild = true;
                                        }
                                    }
                                }
                            }
                            consumed = true;
                        }
                        // ── Split at playhead ──
                        Key::Character(ref c) if c.as_str() == "s" && !self.modifiers.command => {
                            let beat = self.engine.current_beat();
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                if let Some(project) = self.engine.project_mut() {
                                    let spb = 60.0 / project.settings.bpm;
                                    let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
                                    for id in &ids {
                                        if let Some(cmd) = EditingService::split_clip_at_beat(project, id, beat, spb) {
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
                            self.needs_structural_sync = true;
                            self.needs_rebuild = true;
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
                            self.needs_structural_sync = true;
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
                                    let commands = EditingService::delete_clips(project, &ids, None, 0.0);
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
                                let spb = 60.0 / project.settings.bpm;
                                let result = self.editing_service.paste_clips(project, target_beat, target_layer, spb);
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
                                if let Some(project) = self.engine.project_mut() {
                                    // Calculate span of selected clips for offset
                                    let mut min_beat = f32::MAX;
                                    let mut max_beat = f32::MIN;
                                    for layer in &project.timeline.layers {
                                        for clip in &layer.clips {
                                            if ids.contains(&clip.id) {
                                                min_beat = min_beat.min(clip.start_beat);
                                                max_beat = max_beat.max(clip.start_beat + clip.duration_beats);
                                            }
                                        }
                                    }
                                    let mut region = manifold_core::selection::SelectionRegion::default();
                                    if max_beat > min_beat {
                                        region.is_active = true;
                                        region.start_beat = min_beat;
                                        region.end_beat = max_beat;
                                    }
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
                                    let spb = 60.0 / project.settings.bpm;
                                    let commands = EditingService::nudge_clips(project, &ids, -step, spb);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Nudge clips left".into(), project);
                                    }
                                }
                            } else {
                                // Navigate insert cursor left (Ableton behavior)
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Left);
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowRight) if !self.modifiers.command => {
                            let step = if self.modifiers.shift { 0.0625 } else { 0.25 };
                            if !self.selection.selected_clip_ids.is_empty() {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if let Some(project) = self.engine.project_mut() {
                                    let spb = 60.0 / project.settings.bpm;
                                    let commands = EditingService::nudge_clips(project, &ids, step, spb);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Nudge clips right".into(), project);
                                    }
                                }
                            } else {
                                // Navigate insert cursor right (Ableton behavior)
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Right);
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowUp) if !self.modifiers.command => {
                            if self.selection.selected_clip_ids.is_empty() {
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Up);
                            } else if let Some(idx) = self.active_layer_index {
                                if idx > 0 {
                                    self.active_layer_index = Some(idx - 1);
                                }
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowDown) if !self.modifiers.command => {
                            if self.selection.selected_clip_ids.is_empty() {
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Down);
                            } else if let Some(idx) = self.active_layer_index {
                                let count = self.engine.project().map_or(0, |p| p.timeline.layers.len());
                                if idx + 1 < count {
                                    self.active_layer_index = Some(idx + 1);
                                }
                            }
                            consumed = true;
                        }
                        // ── F — zoom to fit all clips ──
                        Key::Character(ref c) if c.as_str() == "f" && !self.modifiers.command => {
                            if let Some(project) = self.engine.project() {
                                let clips = project.timeline.layers.iter()
                                    .flat_map(|l| l.clips.iter());
                                let mut min_beat = f32::MAX;
                                let mut max_beat = f32::MIN;
                                let mut has_clips = false;
                                for c in clips {
                                    min_beat = min_beat.min(c.start_beat);
                                    max_beat = max_beat.max(c.start_beat + c.duration_beats);
                                    has_clips = true;
                                }
                                if has_clips {
                                    let margin = 0.1 * (max_beat - min_beat).max(1.0);
                                    let range = max_beat - min_beat + 2.0 * margin;
                                    let tracks_w = self.ui_root.layout.timeline_tracks().width;
                                    let required_ppb = tracks_w / range;
                                    // Find closest zoom level
                                    let levels = &manifold_ui::color::ZOOM_LEVELS;
                                    let best_idx = levels.iter().enumerate()
                                        .min_by(|(_, a), (_, b)| {
                                            (*a - required_ppb).abs().partial_cmp(&(*b - required_ppb).abs()).unwrap()
                                        })
                                        .map(|(i, _)| i)
                                        .unwrap_or(0);
                                    let ppb = levels[best_idx];
                                    self.ui_root.viewport.set_zoom(ppb);
                                    self.ui_root.viewport.set_scroll(
                                        (min_beat - margin).max(0.0),
                                        0.0,
                                    );
                                    self.needs_rebuild = true;
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
                        // ── O — set/clear export out point ──
                        Key::Character(ref c) if c.as_str() == "o" && !self.modifiers.command => {
                            let beat = self.engine.current_beat();
                            if let Some(project) = self.engine.project_mut() {
                                if self.modifiers.alt {
                                    project.timeline.export_out_beat = 0.0;
                                    project.timeline.export_range_enabled = false;
                                } else {
                                    project.timeline.export_out_beat = beat;
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
                        // ── Backtick — toggle performance HUD ──
                        Key::Character(ref c) if c.as_str() == "`" && !self.modifiers.command => {
                            log::info!("Toggle performance HUD");
                            consumed = true;
                        }
                        _ => {}
                    }
                }

                // If any keyboard shortcut mutated project data, trigger structural sync
                if self.editing_service.data_version() != data_version_before {
                    self.needs_structural_sync = true;
                    self.needs_rebuild = true;
                }

                // Forward to UI input system (unless consumed by app shortcut)
                if is_primary && !consumed {
                    if let Some(ui_key) = Self::convert_key(&logical_key) {
                        self.ui_root.key_event(ui_key, self.modifiers);
                    }
                }

                // Output window management (only when key wasn't consumed by app shortcuts)
                if !consumed {
                    match &logical_key {
                        Key::Named(NamedKey::Escape) => {
                            if !is_primary {
                                self.window_registry.remove(&window_id);
                                log::info!("Closed output window");
                            }
                        }
                        _ => {}
                    }
                }
            }

            // ── Focus loss → cancel in-progress drags ──────────────
            WindowEvent::Focused(false) => {
                // Synthesize a PointerUp to cancel any drag that was in
                // progress when the user alt-tabbed away. Without this the
                // drag state stays active forever because no real PointerUp
                // is delivered while the window is out of focus.
                // Matches Unity OnApplicationFocus(false) in UIBitmapRoot.cs.
                if is_primary {
                    log::debug!("Window lost focus — synthesizing PointerUp to cancel drag");
                    self.ui_root.pointer_event(
                        self.cursor_pos,
                        PointerAction::Up,
                        self.time_since_start,
                    );
                    self.mouse_pressed = false;
                    if self.ui_root.inspector_resize_dragging {
                        self.ui_root.end_inspector_resize();
                    }
                }
            }

            WindowEvent::Focused(true) => {
                // No action needed on focus gain.
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
