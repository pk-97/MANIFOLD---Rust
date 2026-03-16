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

use manifold_ui::cursors::{CursorManager, TimelineCursor};
use manifold_ui::input::{Modifiers, PointerAction};
use manifold_ui::node::Vec2;
use manifold_ui::panels::PanelAction;
use manifold_ui::ui_state::UIState;

use crate::frame_timer::FrameTimer;
use crate::ui_root::UIRoot;
use crate::window_registry::{WindowRegistry, WindowRole, WindowState};

/// Re-export UIState as the selection state (replaces the old SelectionState).
/// UIState is the 1:1 port of Unity's UIState.cs with proper Ableton semantics:
/// - SelectionVersion for dirty-checking
/// - Layer selection (single/toggle/range)
/// - Region (SetRegion clears clips; SetRegionFromClipBounds preserves them)
/// - Insert cursor clears everything (Ableton behavior)
/// - IsLayerActive unified check across 4 interaction paths
pub type SelectionState = UIState;

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
/// From Unity InteractionOverlay drag fields.
pub struct ClipDragState {
    pub mode: ClipDragMode,
    pub anchor_clip_id: String,
    pub anchor_beat: f32,
    pub snapshots: Vec<ClipDragSnapshot>,
    // For move — cross-layer tracking (from Unity InteractionOverlay):
    pub drag_start_layer_index: usize,
    pub drag_selection_min_layer: usize,
    pub drag_selection_max_layer: usize,
    pub drag_offset_beats: f32,       // mouse beat - clip start beat at drag begin
    pub drag_layer_blocked: bool,     // true when video↔generator mismatch
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
            drag_start_layer_index: 0,
            drag_selection_min_layer: 0,
            drag_selection_max_layer: 0,
            drag_offset_beats: 0.0,
            drag_layer_blocked: false,
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

    // Cursor feedback — tracks current cursor shape for interaction hints.
    // From Unity Cursors.cs: SetMove, SetBlocked, SetResizeHorizontal, SetDefault.
    cursor_manager: CursorManager,

    // Video/timeline split handle drag state.
    // From Unity PanelResizeHandle.cs — drag to resize video vs timeline proportion.
    split_dragging: bool,

    // File I/O
    current_project_path: Option<std::path::PathBuf>,

    // Text input
    text_input: crate::text_input::TextInputState,

    // Transport controller — sync management, BPM editing, playback actions
    transport_controller: manifold_playback::transport_controller::TransportController,

    // Panel focus — set when user clicks in inspector area, cleared on timeline click.
    // Matches Unity's InputHandler.inspectorHasFocus for context-sensitive routing
    // of Ctrl+C/X/V, Delete, Ctrl+G shortcuts to effect vs. clip operations.
    inspector_has_focus: bool,

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
            selection: UIState::new(),
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
            cursor_manager: CursorManager::new(),
            split_dragging: false,
            current_project_path: None,
            text_input: crate::text_input::TextInputState::new(),
            transport_controller: manifold_playback::transport_controller::TransportController::new(),
            inspector_has_focus: false,
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
    /// Determine the correct cursor icon based on current interaction state.
    /// From Unity: InteractionOverlay sets Move/Blocked during drag,
    /// PanelResizeHandle sets ResizeHorizontal/ResizeVertical on hover,
    /// Cursors.SetDefault() on drag end and pointer exit.
    fn update_cursor_for_position(&mut self) {
        // Priority 1: Active drag — cursor follows drag mode
        if self.clip_drag.mode == ClipDragMode::Move {
            if self.clip_drag.drag_layer_blocked {
                self.cursor_manager.set(TimelineCursor::Blocked);
            } else {
                self.cursor_manager.set(TimelineCursor::Move);
            }
            return;
        }
        if self.clip_drag.mode == ClipDragMode::TrimLeft || self.clip_drag.mode == ClipDragMode::TrimRight {
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
            return;
        }

        // Priority 2: Inspector resize edge hover
        if self.ui_root.inspector_resize_dragging || self.ui_root.is_near_inspector_edge(self.cursor_pos) {
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
            return;
        }

        // Priority 3: Video/timeline split handle hover
        // (The split handle is at the top edge of the timeline body)
        let timeline_body = self.ui_root.layout.timeline_body();
        let split_handle_y = timeline_body.y;
        let split_handle_height = 6.0; // UIConstants.InspectorResizeHandleWidth equivalent
        if self.cursor_pos.y >= split_handle_y - split_handle_height
            && self.cursor_pos.y <= split_handle_y + split_handle_height
            && self.cursor_pos.x >= timeline_body.x
            && self.cursor_pos.x <= timeline_body.x + timeline_body.width
        {
            self.cursor_manager.set(TimelineCursor::ResizeVertical);
            return;
        }

        // Priority 4: Clip trim handle hover
        let tracks_rect = self.ui_root.viewport.tracks_rect();
        if tracks_rect.contains(self.cursor_pos) {
            if let Some(hit) = self.ui_root.viewport.hit_test_clip(self.cursor_pos) {
                match hit.region {
                    manifold_ui::panels::HitRegion::TrimLeft | manifold_ui::panels::HitRegion::TrimRight => {
                        self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Default: standard arrow
        self.cursor_manager.set_default();
    }

    fn navigate_cursor(&mut self, direction: manifold_ui::cursor_nav::Direction) {
        use manifold_ui::cursor_nav::{navigate_cursor, NavResult, NavLayerInfo, NavClipInfo};

        let current_beat = self.selection.insert_cursor_beat.unwrap_or(self.engine.current_beat());
        let current_layer = self.selection.insert_cursor_layer_index
            .or(self.active_layer_index)
            .unwrap_or(0);
        let grid_interval = self.ui_root.viewport.grid_step();

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
                // Find the clip's layer for proper UIState selection
                let li = self.engine.project()
                    .and_then(|p| p.timeline.layers.iter().enumerate()
                        .find_map(|(i, l)| l.clips.iter().any(|c| c.id == clip_id).then_some(i)))
                    .unwrap_or(0);
                self.selection.select_clip(clip_id, li);
                self.active_layer_index = Some(li);
                self.needs_rebuild = true;
            }
            NavResult::SetCursor { beat, layer } => {
                self.selection.set_insert_cursor(beat, layer);
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
                if let Ok(new_bpm) = text.parse::<f32>() {
                    let new_bpm = new_bpm.clamp(20.0, 300.0);
                    if let Some(project) = self.engine.project_mut() {
                        let old_bpm = project.settings.bpm;
                        // Unity: skip if approximately equal
                        if (old_bpm - new_bpm).abs() >= 0.01 {
                            let cmd = manifold_editing::commands::settings::ChangeBpmCommand::new(
                                old_bpm, new_bpm,
                            );
                            self.editing_service.execute(Box::new(cmd), project);
                        }
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
                    self.selection.clear_selection();
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
        let prev_sel_version = self.selection.selection_version;
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
                    self.selection.clear_selection();
                    self.active_layer_index = Some(0);
                    self.current_project_path = None;
                    needs_structural_sync = true;
                    continue;
                }
                // Transport controller actions — intercept here for Application-level access
                PanelAction::CycleClockAuthority => {
                    self.transport_controller.cycle_authority(&mut self.engine);
                    continue;
                }
                PanelAction::ToggleLink => {
                    self.transport_controller.toggle_link(&mut self.engine);
                    continue;
                }
                PanelAction::ToggleMidiClock => {
                    self.transport_controller.toggle_midi_clock(&mut self.engine);
                    continue;
                }
                PanelAction::ToggleSyncOutput => {
                    self.transport_controller.toggle_sync_output(&mut self.engine);
                    continue;
                }
                PanelAction::ResetBpm => {
                    manifold_playback::transport_controller::TransportController::reset_bpm(
                        &mut self.engine, &mut self.editing_service,
                    );
                    self.needs_rebuild = true;
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
        if self.selection.selection_version != prev_sel_version && !needs_structural_sync {
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
        // 2a. Per-frame drag polling with auto-scroll.
        // From Unity InteractionOverlay.PollMoveDrag (lines 116-124):
        // When mouse is stationary at viewport edge during Move drag,
        // OnDrag stops firing but auto-scroll must continue.
        if self.clip_drag.mode == ClipDragMode::Move && !self.clip_drag.snapshots.is_empty() {
            let tracks_rect = self.ui_root.viewport.tracks_rect();
            if tracks_rect.width > 0.0 {
                // From Unity WorkspaceController.cs lines 58-60:
                // DragEdgeScrollZonePx = 72, DragEdgeScrollSpeedPxPerSec = 900
                let edge_zone_px = 72.0;
                let scroll_speed_px_per_sec = 900.0;
                let ppb = self.ui_root.viewport.pixels_per_beat();
                let dt = self.frame_timer.last_dt() as f32;
                let scroll_speed_beats = (scroll_speed_px_per_sec * dt) / ppb;

                let local_x = self.cursor_pos.x - tracks_rect.x;
                if local_x > tracks_rect.width - edge_zone_px {
                    // Near right edge — scroll right, speed proportional to proximity
                    let factor = 1.0 - (tracks_rect.width - local_x) / edge_zone_px;
                    let new_scroll = self.ui_root.viewport.scroll_x_beats() + scroll_speed_beats * factor;
                    self.ui_root.viewport.set_scroll(new_scroll, self.ui_root.viewport.scroll_y_px());
                    needs_structural_sync = true;
                } else if local_x < edge_zone_px && local_x >= 0.0 {
                    // Near left edge — scroll left
                    let factor = 1.0 - local_x / edge_zone_px;
                    let new_scroll = (self.ui_root.viewport.scroll_x_beats() - scroll_speed_beats * factor).max(0.0);
                    self.ui_root.viewport.set_scroll(new_scroll, self.ui_root.viewport.scroll_y_px());
                    needs_structural_sync = true;
                }
            }
        }

        // 2b. Auto-scroll check for playback (BEFORE build so rebuild includes new scroll)
        let scroll_changed = crate::ui_bridge::check_auto_scroll(&mut self.ui_root, &self.engine);

        // 3. Rebuild if needed
        if self.needs_rebuild || scroll_changed || needs_structural_sync {
            self.needs_rebuild = false;
            self.ui_root.build();
        }

        // 4. Push engine state to UI panels (AFTER build so new nodes get state)
        crate::ui_bridge::push_state(
            &mut self.ui_root,
            &self.engine,
            self.active_layer_index,
            &self.selection,
            self.editing_service.is_dirty(),
            self.current_project_path.as_deref(),
        );

        // 5. Push performance metrics to HUD
        if self.ui_root.perf_hud.is_visible() {
            let bpm = self.engine.project().map(|p| p.settings.bpm).unwrap_or(120.0);
            let clock_source = self.engine.project()
                .map(|p| p.settings.clock_authority.display_name().to_string())
                .unwrap_or_else(|| "Internal".to_string());
            self.ui_root.perf_hud.set_metrics(manifold_ui::panels::perf_hud::PerfMetrics {
                fps: self.frame_timer.current_fps() as f32,
                frame_time_ms: (self.frame_timer.last_dt() * 1000.0) as f32,
                active_clips: 0, // TODO: wire from tick_result
                preparing_clips: 0,
                current_beat: self.engine.current_beat(),
                current_time_secs: self.engine.current_time(),
                bpm,
                clock_source,
                is_playing: self.engine.is_playing(),
                data_version: self.editing_service.data_version(),
            });
        }

        // 6. Lightweight update (playhead, insert cursor, layer selection, HUD values)
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

            if is_workspace {
                // Blit compositor output into the video preview area only (not fullscreen)
                let video_rect = self.ui_root.layout.video_area();
                let sf = scale as f32;
                // Clear surface first (black background for areas outside video)
                {
                    let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Clear Surface"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &surface_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                blit.blit_to_rect(
                    &gpu.device, &mut encoder, compositor_output, &surface_view,
                    video_rect.x * sf, video_rect.y * sf,
                    video_rect.width * sf, video_rect.height * sf,
                );
            } else {
                // Output windows: fullscreen blit
                blit.blit(&gpu.device, &mut encoder, compositor_output, &surface_view);
            }

            // Draw UI overlay on workspace window using the UITree
            // Pass logical pixel dimensions — the tree is built in logical coords
            if is_workspace {
                if let Some(ui) = &mut self.ui_renderer {
                    let logical_w = (surface_w as f64 / scale) as u32;
                    let logical_h = (surface_h as f64 / scale) as u32;
                    // When dropdown is open, pass its bounds so base text behind
                    // it is hidden (prevents text bleed-through).
                    if self.ui_root.dropdown.is_open() {
                        let start = Some(self.ui_root.dropdown.first_node());
                        let bounds = Some(self.ui_root.dropdown.container_bounds());
                        ui.render_tree_with_overlay(&self.ui_root.tree, start, bounds);
                    } else {
                        ui.render_tree(&self.ui_root.tree);
                    }
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

                    // Split handle drag takes highest priority
                    // From Unity PanelResizeHandle.OnDrag
                    if self.split_dragging {
                        self.ui_root.layout.update_split_from_drag(self.cursor_pos.y);
                        self.cursor_manager.set(TimelineCursor::ResizeVertical);
                        self.needs_rebuild = true;
                    }
                    // Inspector resize drag takes next priority
                    else if self.ui_root.inspector_resize_dragging {
                        self.ui_root.update_inspector_resize(self.cursor_pos.x);
                        self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
                    } else {
                        self.ui_root.pointer_event(
                            self.cursor_pos,
                            PointerAction::Move,
                            self.time_since_start,
                        );

                        // Update cursor based on current interaction state.
                        // From Unity: Cursors.SetMove/SetBlocked/SetResizeHorizontal/SetDefault
                        self.update_cursor_for_position();
                    }

                    // Apply cursor to window if changed
                    if self.cursor_manager.needs_update() {
                        if let Some(ws) = self.window_registry.get(&window_id) {
                            let icon = match self.cursor_manager.pending_cursor_icon() {
                                TimelineCursor::Default => winit::window::CursorIcon::Default,
                                TimelineCursor::ResizeHorizontal => winit::window::CursorIcon::ColResize,
                                TimelineCursor::ResizeVertical => winit::window::CursorIcon::RowResize,
                                TimelineCursor::Move => winit::window::CursorIcon::Move,
                                TimelineCursor::Blocked => winit::window::CursorIcon::NotAllowed,
                            };
                            ws.window.set_cursor(icon);
                            self.cursor_manager.mark_applied();
                        }
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

                                    // Track which panel has focus for context-sensitive shortcuts.
                                    // Matches Unity's InputHandler.inspectorHasFocus.
                                    let inspector_rect = self.ui_root.layout.inspector();
                                    let timeline_rect = self.ui_root.layout.timeline_tracks();
                                    if inspector_rect.contains(self.cursor_pos) {
                                        self.inspector_has_focus = true;
                                    } else if timeline_rect.contains(self.cursor_pos) {
                                        self.inspector_has_focus = false;
                                    }

                                    // If a dropdown is open and the click lands outside it,
                                    // dismiss the dropdown and consume the event so that the
                                    // background node never receives a PointerDown (prevents
                                    // phantom pressed_id on the node behind the dropdown).
                                    if self.ui_root.dropdown.is_open()
                                        && !self.ui_root.dropdown.contains_point(self.cursor_pos)
                                    {
                                        self.ui_root.dropdown.close(&mut self.ui_root.tree);
                                        // Click is consumed by dismiss — do not forward.
                                    } else if self.ui_root.layout.is_near_split_handle(self.cursor_pos) {
                                        // Begin video/timeline split drag.
                                        // From Unity PanelResizeHandle.OnPointerDown.
                                        self.split_dragging = true;
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
                                    if self.split_dragging {
                                        // End video/timeline split drag.
                                        // From Unity PanelResizeHandle.OnPointerUp.
                                        self.split_dragging = false;
                                        self.cursor_manager.set_default();
                                    } else if self.ui_root.inspector_resize_dragging {
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
                    // ── Shortcut dispatch ──
                    // Follows Unity InputHandler.HandleKeyboardInput() control flow exactly.
                    // Modifier matching is EXACT (matches Unity's ShortcutRegistry.WasPressed):
                    //   is_none()          = no modifiers held
                    //   is_command_only()   = only Cmd (macOS) / Ctrl (Windows)
                    //   is_shift_only()     = only Shift
                    //   is_alt_only()       = only Alt
                    //   is_command_shift()  = only Cmd+Shift
                    let m = self.modifiers;
                    match &logical_key {

                        // ── Backtick — toggle performance HUD ──
                        // From Unity InputHandler — toggles PerformanceHUDPanel visibility.
                        Key::Character(ref c) if c.as_str() == "`" && m.is_none() => {
                            self.ui_root.perf_hud.toggle();
                            self.needs_rebuild = true;
                            consumed = true;
                        }

                        // ── Escape — 4-level priority chain (Unity InputHandler line 224-232) ──
                        Key::Named(NamedKey::Escape) => {
                            // Level 1: dismiss context menu / dropdown
                            if self.ui_root.dropdown.is_open() {
                                self.ui_root.dropdown.close(&mut self.ui_root.tree);
                            }
                            // Level 2: monitor output active → no-op
                            else if self.window_registry.has_output_window() {
                                // Matches Unity: if (host.IsMonitorOutputActive) return;
                            }
                            // Level 3: inspector has focus → clear effect selection + clear focus
                            else if self.inspector_has_focus {
                                // Future: clear effect selection when effect system is implemented
                                self.inspector_has_focus = false;
                            }
                            // Level 4: clear all selection + insert cursor
                            else {
                                self.selection.clear_selection();
                            }
                            consumed = true;
                        }

                        // ── Undo: Cmd+Shift+Z ──
                        Key::Character(ref c) if c.as_str() == "z" && m.is_command_shift() => {
                            crate::ui_bridge::redo(&mut self.engine, &mut self.editing_service);
                            self.engine.mark_compositor_dirty(self.frame_timer.realtime_since_start());
                            self.needs_structural_sync = true;
                            consumed = true;
                        }
                        // ── Undo: Cmd+Z ──
                        Key::Character(ref c) if c.as_str() == "z" && m.is_command_only() => {
                            crate::ui_bridge::undo(&mut self.engine, &mut self.editing_service);
                            self.engine.mark_compositor_dirty(self.frame_timer.realtime_since_start());
                            self.needs_structural_sync = true;
                            consumed = true;
                        }
                        // ── Redo: Cmd+Y ──
                        Key::Character(ref c) if c.as_str() == "y" && m.is_command_only() => {
                            crate::ui_bridge::redo(&mut self.engine, &mut self.editing_service);
                            self.engine.mark_compositor_dirty(self.frame_timer.realtime_since_start());
                            self.needs_structural_sync = true;
                            consumed = true;
                        }

                        // ── Save: Cmd+S ──
                        Key::Character(ref c) if c.as_str() == "s" && m.is_command_only() => {
                            self.save_project();
                            consumed = true;
                        }
                        // ── Open: Cmd+O ──
                        Key::Character(ref c) if c.as_str() == "o" && m.is_command_only() => {
                            self.open_project();
                            consumed = true;
                        }
                        // ── New: Cmd+N ──
                        Key::Character(ref c) if c.as_str() == "n" && m.is_command_only() => {
                            let project = Self::create_default_project();
                            self.engine.initialize(project);
                            self.editing_service.set_project();
                            self.selection.clear_selection();
                            self.active_layer_index = Some(0);
                            self.needs_rebuild = true;
                            log::info!("New project created");
                            consumed = true;
                        }

                        // ── Select all: Cmd+A ──
                        Key::Character(ref c) if c.as_str() == "a" && m.is_command_only() => {
                            if let Some(project) = self.engine.project() {
                                // Clear everything first, then add all clips
                                self.selection.clear_selection();
                                for layer in &project.timeline.layers {
                                    for clip in &layer.clips {
                                        self.selection.selected_clip_ids.insert(clip.id.clone());
                                    }
                                }
                                self.selection.primary_selected_clip_id = self.selection.selected_clip_ids.iter().next().cloned();
                                self.selection.selection_version += 1;
                            }
                            self.needs_structural_sync = true;
                            consumed = true;
                        }

                        // ── Copy: Cmd+C (context-sensitive: effects vs clips) ──
                        Key::Character(ref c) if c.as_str() == "c" && m.is_command_only() => {
                            if self.inspector_has_focus {
                                // Future: effect copy when effect system is implemented
                                log::debug!("Inspector focused — effect copy (stub)");
                            } else {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if !ids.is_empty() {
                                    if let Some(project) = self.engine.project() {
                                        self.editing_service.copy_clips(project, &ids);
                                    }
                                }
                            }
                            consumed = true;
                        }
                        // ── Cut: Cmd+X (context-sensitive: effects vs clips) ──
                        Key::Character(ref c) if c.as_str() == "x" && m.is_command_only() => {
                            if self.inspector_has_focus {
                                // Future: effect cut when effect system is implemented
                                log::debug!("Inspector focused — effect cut (stub)");
                            } else {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if !ids.is_empty() {
                                    if let Some(project) = self.engine.project_mut() {
                                        self.editing_service.copy_clips(project, &ids);
                                        // Region-aware cut (Fix #6)
                                        let spb = 60.0 / project.settings.bpm;
                                        let region = if self.selection.has_region() {
                                            Some(self.selection.get_region().clone())
                                        } else {
                                            None
                                        };
                                        let commands = EditingService::delete_clips(
                                            project, &ids, region.as_ref(), spb,
                                        );
                                        self.editing_service.execute_batch(commands, "Cut clips".into(), project);
                                    }
                                    self.selection.clear_selection();
                                }
                            }
                            consumed = true;
                        }
                        // ── Paste: Cmd+V (context-sensitive: effects vs clips) ──
                        Key::Character(ref c) if c.as_str() == "v" && m.is_command_only() => {
                            if self.inspector_has_focus {
                                // Future: effect paste when effect system is implemented
                                log::debug!("Inspector focused — effect paste (stub)");
                            } else {
                                let target_beat = self.selection.insert_cursor_beat
                                    .unwrap_or(self.engine.current_beat());
                                let target_layer = self.selection.insert_cursor_layer_index
                                    .or(self.active_layer_index)
                                    .unwrap_or(0) as i32;
                                if let Some(project) = self.engine.project_mut() {
                                    let spb = 60.0 / project.settings.bpm;
                                    let result = self.editing_service.paste_clips(project, target_beat, target_layer, spb);
                                    if !result.commands.is_empty() {
                                        self.editing_service.execute_batch(result.commands, "Paste clips".into(), project);
                                        // Select all pasted clips and create region (Fix #5)
                                        // From Unity EditingService.PasteClips (line 660-667)
                                        self.selection.clear_selection();
                                        for id in result.pasted_clip_ids {
                                            self.selection.selected_clip_ids.insert(id);
                                        }
                                        self.selection.primary_selected_clip_id = self.selection.selected_clip_ids.iter().next().cloned();
                                        self.selection.selection_version += 1;
                                        // Update region to encompass pasted clips
                                        crate::ui_bridge::update_region_from_clip_selection_inline(
                                            &mut self.selection, project);
                                    }
                                }
                            }
                            consumed = true;
                        }

                        // ── Duplicate: Cmd+D ──
                        // From Unity EditingService.DuplicateSelectedClips (line 767-778):
                        // After duplicate, select the new clips and update region.
                        Key::Character(ref c) if c.as_str() == "d" && m.is_command_only() => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                if let Some(project) = self.engine.project_mut() {
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
                                    // Count clips before to identify new ones after
                                    let before_ids: std::collections::HashSet<String> = project.timeline.layers.iter()
                                        .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
                                        .collect();

                                    let commands = EditingService::duplicate_clips(project, &ids, &region);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Duplicate clips".into(), project);

                                        // Find newly created clips (IDs that didn't exist before)
                                        let new_ids: Vec<String> = project.timeline.layers.iter()
                                            .flat_map(|l| l.clips.iter()
                                                .filter(|c| !before_ids.contains(&c.id))
                                                .map(|c| c.id.clone()))
                                            .collect();

                                        // Select the duplicates (Fix #4)
                                        self.selection.clear_selection();
                                        for id in &new_ids {
                                            self.selection.selected_clip_ids.insert(id.clone());
                                        }
                                        self.selection.primary_selected_clip_id = new_ids.first().cloned();
                                        self.selection.selection_version += 1;

                                        // Update region to encompass duplicates
                                        crate::ui_bridge::update_region_from_clip_selection_inline(
                                            &mut self.selection, project);
                                    }
                                }
                            }
                            self.needs_structural_sync = true;
                            consumed = true;
                        }

                        // ── Ungroup: Cmd+Shift+G (context-sensitive) ──
                        Key::Character(ref c) if c.as_str() == "g" && m.is_command_shift() => {
                            if self.inspector_has_focus {
                                // Future: effect ungroup when effect system is implemented
                                log::debug!("Inspector focused — effect ungroup (stub)");
                            }
                            consumed = true;
                        }
                        // ── Group: Cmd+G (context-sensitive) ──
                        Key::Character(ref c) if c.as_str() == "g" && m.is_command_only() => {
                            if self.inspector_has_focus {
                                // Future: effect group when effect system is implemented
                                log::debug!("Inspector focused — effect group (stub)");
                            } else {
                                // Future: group selected layers
                                log::debug!("Group selected layers (stub)");
                            }
                            consumed = true;
                        }

                        // ── Delete/Backspace (context-sensitive: effects → layers → clips) ──
                        Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace)
                            if m.is_none() =>
                        {
                            // Priority 1: inspector focused → delete effects
                            if self.inspector_has_focus {
                                // Future: effect delete when effect system is implemented
                                log::debug!("Inspector focused — effect delete (stub)");
                            }
                            // Priority 2: active layer selected, no clips → delete layer
                            else if self.selection.selected_clip_ids.is_empty() {
                                if let Some(idx) = self.active_layer_index {
                                    if let Some(project) = self.engine.project_mut() {
                                        if project.timeline.layers.len() > 1 {
                                            if let Some(layer) = project.timeline.layers.get(idx) {
                                                let layer_clone = layer.clone();
                                                let cmd = DeleteLayerCommand::new(layer_clone, idx);
                                                self.editing_service.execute(Box::new(cmd), project);
                                                let new_count = project.timeline.layers.len();
                                                if idx >= new_count {
                                                    self.active_layer_index = Some(new_count.saturating_sub(1));
                                                }
                                                self.needs_rebuild = true;
                                            }
                                        }
                                    }
                                }
                            }
                            // Priority 3: delete selected clips (region-aware)
                            // From Unity EditingService.DeleteSelectedClips: if region active,
                            // split at boundaries and delete interior only.
                            else {
                                let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                                if let Some(project) = self.engine.project_mut() {
                                    let spb = 60.0 / project.settings.bpm;
                                    // Pass the region if active (Fix #6)
                                    let region = if self.selection.has_region() {
                                        Some(self.selection.get_region().clone())
                                    } else {
                                        None
                                    };
                                    let commands = EditingService::delete_clips(
                                        project, &ids, region.as_ref(), spb,
                                    );
                                    self.editing_service.execute_batch(commands, "Delete clips".into(), project);
                                }
                                self.selection.clear_selection();
                            }
                            consumed = true;
                        }

                        // ── Space — Play/Pause (seek to insert cursor if paused) ──
                        Key::Named(NamedKey::Space) if m.is_none() => {
                            if self.engine.is_playing() {
                                self.engine.pause();
                            } else {
                                // Unity: if paused and insert cursor exists, seek to cursor beat first
                                // Use beat_to_timeline_time (goes through tempo map) not simple BPM division
                                if let Some(cursor_beat) = self.selection.insert_cursor_beat {
                                    let time = self.engine.beat_to_timeline_time(cursor_beat);
                                    self.engine.seek_to(time);
                                }
                                self.engine.play();
                            }
                            consumed = true;
                        }

                        // ── Home — seek to start ──
                        Key::Named(NamedKey::Home) if m.is_none() => {
                            self.engine.seek_to(0.0);
                            consumed = true;
                        }
                        // ── End — seek to end of last clip ──
                        Key::Named(NamedKey::End) if m.is_none() => {
                            if let Some(project) = self.engine.project() {
                                let mut max_beat: f32 = 0.0;
                                for layer in &project.timeline.layers {
                                    for clip in &layer.clips {
                                        let end = clip.start_beat + clip.duration_beats;
                                        if end > max_beat { max_beat = end; }
                                    }
                                }
                                let time = max_beat * (60.0 / project.settings.bpm);
                                self.engine.seek_to(time);
                            }
                            consumed = true;
                        }

                        // ── S — split at playhead (bare S only, no modifiers) ──
                        Key::Character(ref c) if c.as_str() == "s" && m.is_none() => {
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

                        // ── Shift+E — shrink by grid step (check before bare E) ──
                        // winit reports Shift+E as "E" (uppercase) on most platforms
                        Key::Character(ref c) if (c.as_str() == "E" || c.as_str() == "e") && m.is_shift_only() => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                let step = self.ui_root.viewport.grid_step();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = EditingService::shrink_clips_by_grid(project, &ids, step);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Shrink clips".into(), project);
                                    }
                                }
                            }
                            consumed = true;
                        }
                        // ── E — extend by grid step (bare E only) ──
                        Key::Character(ref c) if c.as_str() == "e" && m.is_none() => {
                            let ids: Vec<String> = self.selection.selected_clip_ids.iter().cloned().collect();
                            if !ids.is_empty() {
                                let step = self.ui_root.viewport.grid_step();
                                if let Some(project) = self.engine.project_mut() {
                                    let commands = EditingService::extend_clips_by_grid(project, &ids, step);
                                    if !commands.is_empty() {
                                        self.editing_service.execute_batch(commands, "Extend clips".into(), project);
                                    }
                                }
                            }
                            consumed = true;
                        }

                        // ── F — zoom to fit (bare F only) ──
                        Key::Character(ref c) if c.as_str() == "f" && m.is_none() => {
                            if let Some(project) = self.engine.project() {
                                let mut min_beat = f32::MAX;
                                let mut max_beat = f32::MIN;
                                let mut has_clips = false;
                                for layer in &project.timeline.layers {
                                    for clip in &layer.clips {
                                        min_beat = min_beat.min(clip.start_beat);
                                        max_beat = max_beat.max(clip.start_beat + clip.duration_beats);
                                        has_clips = true;
                                    }
                                }
                                if has_clips {
                                    let margin = 0.1 * (max_beat - min_beat).max(1.0);
                                    let range = max_beat - min_beat + 2.0 * margin;
                                    let tracks_w = self.ui_root.layout.timeline_tracks().width;
                                    let required_ppb = tracks_w / range;
                                    let levels = &manifold_ui::color::ZOOM_LEVELS;
                                    let mut best_idx = 0;
                                    let mut best_diff = f32::MAX;
                                    for (i, &lvl) in levels.iter().enumerate() {
                                        let diff = (lvl - required_ppb).abs();
                                        if diff < best_diff {
                                            best_diff = diff;
                                            best_idx = i;
                                        }
                                    }
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

                        // ── 0 — toggle mute (bare 0 only) ──
                        Key::Character(ref c) if c.as_str() == "0" && m.is_none() => {
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

                        // ── Arrow keys — nudge clips when selected, navigate cursor otherwise ──
                        // Left/Right with Shift: fine nudge (1/16 beat)
                        // Left/Right without Shift: grid step nudge
                        // Up/Down: navigate layers (no-op with clips selected in Unity)
                        Key::Named(NamedKey::ArrowLeft) if !m.command && !m.alt => {
                            let grid = self.ui_root.viewport.grid_step();
                            let step = if m.shift { 1.0 / 16.0 } else { grid };
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
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Left);
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowRight) if !m.command && !m.alt => {
                            let grid = self.ui_root.viewport.grid_step();
                            let step = if m.shift { 1.0 / 16.0 } else { grid };
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
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Right);
                            }
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowUp) if m.is_none() => {
                            if self.selection.selected_clip_ids.is_empty() {
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Up);
                            }
                            // Unity: Up/Down with clips selected = no-op
                            consumed = true;
                        }
                        Key::Named(NamedKey::ArrowDown) if m.is_none() => {
                            if self.selection.selected_clip_ids.is_empty() {
                                self.navigate_cursor(manifold_ui::cursor_nav::Direction::Down);
                            }
                            // Unity: Up/Down with clips selected = no-op
                            consumed = true;
                        }

                        // ── Export markers: Alt variants first (exact match) ──
                        Key::Character(ref c) if c.as_str() == "i" && m.is_alt_only() => {
                            if let Some(project) = self.engine.project_mut() {
                                project.timeline.export_in_beat = 0.0;
                                project.timeline.export_range_enabled = false;
                            }
                            consumed = true;
                        }
                        Key::Character(ref c) if c.as_str() == "o" && m.is_alt_only() => {
                            if let Some(project) = self.engine.project_mut() {
                                project.timeline.export_out_beat = 0.0;
                                project.timeline.export_range_enabled = false;
                            }
                            consumed = true;
                        }
                        // ── I — set export in point (bare I only) ──
                        Key::Character(ref c) if c.as_str() == "i" && m.is_none() => {
                            let beat = self.engine.current_beat();
                            if let Some(project) = self.engine.project_mut() {
                                project.timeline.export_in_beat = beat;
                                project.timeline.export_range_enabled = true;
                            }
                            consumed = true;
                        }
                        // ── O — set export out point (bare O only) ──
                        Key::Character(ref c) if c.as_str() == "o" && m.is_none() => {
                            let beat = self.engine.current_beat();
                            if let Some(project) = self.engine.project_mut() {
                                project.timeline.export_out_beat = beat;
                                project.timeline.export_range_enabled = true;
                            }
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

            // File drag-drop support.
            // From Unity FileDragDrop.cs — polls for OS-level file drops.
            // In winit, this is event-driven instead of polled.
            WindowEvent::DroppedFile(path) => {
                let path_str = path.to_string_lossy().to_string();
                let ext = path.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                match ext.as_str() {
                    // Video files → import as video clip on active layer
                    "mp4" | "mov" | "avi" | "mkv" | "webm" => {
                        log::info!("Video file dropped: {}", path_str);
                        // Future: create video clip on active layer at cursor position
                    }
                    // Project files → load project
                    "json" | "manifold" => {
                        log::info!("Project file dropped: {}", path_str);
                        let load_path = path.clone();
                        match manifold_io::loader::load_project(&load_path) {
                            Ok(project) => {
                                self.engine.initialize(project);
                                self.editing_service.set_project();
                                self.selection.clear_selection();
                                self.active_layer_index = Some(0);
                                self.current_project_path = Some(load_path);
                                self.needs_structural_sync = true;
                                self.needs_rebuild = true;
                                log::info!("Loaded project from drop");
                            }
                            Err(e) => log::error!("Failed to load dropped project: {e}"),
                        }
                    }
                    // Audio files → import as audio lane
                    "wav" | "mp3" | "flac" | "aiff" | "ogg" => {
                        log::info!("Audio file dropped: {} (audio import not yet implemented)", path_str);
                    }
                    _ => {
                        log::debug!("Unrecognized file type dropped: {}", path_str);
                    }
                }
            }
            WindowEvent::HoveredFile(path) => {
                log::debug!("File hovering: {}", path.to_string_lossy());
                // Future: show drop preview (highlight target layer/position)
            }
            WindowEvent::HoveredFileCancelled => {
                log::debug!("File hover cancelled");
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
