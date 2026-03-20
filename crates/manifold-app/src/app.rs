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
use manifold_playback::audio_sync::{ImportedAudioSyncController, PreloadedAudioData};
use manifold_playback::audio_decoder::DecodedAudio;
use manifold_playback::percussion_orchestrator::PercussionImportOrchestrator;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::StubRenderer;
use manifold_renderer::blit::BlitPipeline;
use manifold_renderer::compositor::{Compositor, CompositeLayerDescriptor, CompositorFrame};
use manifold_renderer::tonemap::TonemapSettings;
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::{CompositeClipDescriptor, LayerCompositor};
use manifold_renderer::surface::SurfaceWrapper;
use manifold_renderer::ui_renderer::{TextMode, UIRenderer};

use manifold_ui::cursors::{CursorManager, TimelineCursor};
use manifold_ui::input::{Modifiers, PointerAction};
use manifold_ui::node::Vec2;
use manifold_ui::panels::PanelAction;
use manifold_ui::ui_state::UIState;

use crate::frame_timer::FrameTimer;
use crate::project_io::{ProjectIOService, ProjectIOAction};
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;
use crate::window_registry::{WindowRegistry, WindowRole, WindowState};

/// Re-export UIState as the selection state (replaces the old SelectionState).
/// UIState is the 1:1 port of Unity's UIState.cs with proper Ableton semantics:
/// - SelectionVersion for dirty-checking
/// - Layer selection (single/toggle/range)
/// - Region (SetRegion clears clips; SetRegionFromClipBounds preserves them)
/// - Insert cursor clears everything (Ableton behavior)
/// - IsLayerActive unified check across 4 interaction paths
pub type SelectionState = UIState;

// ClipDragMode, ClipDragSnapshot, ClipDragState — REMOVED.
// All drag state now lives in InteractionOverlay (interaction_overlay.rs).

/// Result from background audio loading thread.
/// Contains pre-decoded audio for both kira playback and waveform visualization.
struct PendingAudioLoadResult {
    preloaded: PreloadedAudioData,
    waveform: Option<DecodedAudio>,
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

    // Selection
    selection: SelectionState,
    active_layer_index: Option<usize>,
    /// Slider drag snapshot for undo (opacity, slip, etc.). Stores the old value
    /// on snapshot, committed on release. NOT related to clip drag state.
    slider_snapshot: Option<f32>,
    /// Trim drag snapshot (min, max) for undo. Unity: onTrimSnapshot/onTrimCommit.
    trim_snapshot: Option<(f32, f32)>,
    /// ADSR drag snapshot (attack, decay, sustain, release) for undo.
    adsr_snapshot: Option<(f32, f32, f32, f32)>,
    /// Envelope target drag snapshot for undo.
    target_snapshot: Option<f32>,

    // Rendering
    compositor: Option<Box<dyn Compositor>>,
    blit_pipeline: Option<BlitPipeline>,
    output_blit_pipeline: Option<BlitPipeline>,
    output_blit_format: Option<wgpu::TextureFormat>,
    ui_renderer: Option<UIRenderer>,
    layer_bitmap_gpu: Option<manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu>,
    surface_format: wgpu::TextureFormat,

    // UI
    ui_root: UIRoot,

    // Frame timing
    frame_timer: FrameTimer,
    frame_count: u64,

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
    split_was_hovered: bool,

    // File I/O
    current_project_path: Option<std::path::PathBuf>,
    user_prefs: UserPrefs,
    project_io: ProjectIOService,

    // Text input
    text_input: crate::text_input::TextInputState,

    // Audio sync — imported audio playback synced to timeline.
    // Port of Unity ImportedAudioSyncController (owned by WorkspaceController).
    audio_sync: Option<ImportedAudioSyncController>,

    // Pending audio load — receives results from background decode thread.
    // Unity loads audio async via coroutines; we use std::thread + mpsc channel.
    pending_audio_load: Option<std::sync::mpsc::Receiver<PendingAudioLoadResult>>,

    // Percussion import orchestrator — central state machine for audio analysis pipeline.
    // Port of Unity PercussionImportOrchestrator (owned by WorkspaceController).
    percussion_orchestrator: PercussionImportOrchestrator,

    // Transport controller — sync management, BPM editing, playback actions
    transport_controller: manifold_playback::transport_controller::TransportController,

    // Keyboard/zoom handler — port of Unity InputHandler.cs
    // Owns inspector_has_focus (panel focus for context-sensitive routing).
    input_handler: crate::input_handler::InputHandler,

    // Interaction overlay — port of Unity InteractionOverlay.cs
    // Owns all drag state. Lives on Application (not UIRoot) so we can
    // split-borrow it alongside ui_root.viewport and create AppEditingHost.
    overlay: manifold_ui::interaction_overlay::InteractionOverlay,

    // Pre-drag split commands — persists between AppEditingHost instances.
    // Unity stores these on InteractionOverlay; Rust stores them here because
    // the overlay can't depend on manifold-editing Command types.
    // Populated by split_clips_for_region_move, prepended on commit_command_batch.
    pre_drag_commands: Vec<Box<dyn manifold_editing::command::Command>>,

    // Detected display resolutions: (width, height, label).
    // Populated from winit monitors at startup. Matches Unity Footer.CollectDisplayResolutions.
    display_resolutions: Vec<(u32, u32, String)>,

    // State
    initialized: bool,
    pending_toggle_output: bool,
    pending_close_output: bool,
    needs_rebuild: bool,
    /// Set by scroll/zoom events that only affect viewport + layer_headers.
    /// Uses the partial rebuild path (rebuild_scroll_panels) instead of full build.
    needs_scroll_rebuild: bool,
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
            active_layer_index: None,
            slider_snapshot: None,
            trim_snapshot: None,
            adsr_snapshot: None,
            target_snapshot: None,
            compositor: None,
            blit_pipeline: None,
            output_blit_pipeline: None,
            output_blit_format: None,
            ui_renderer: None,
            layer_bitmap_gpu: None,
            surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            ui_root: UIRoot::new(),
            frame_timer: FrameTimer::new(60.0),
            frame_count: 0,
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
            split_was_hovered: false,
            current_project_path: None,
            project_io: {
                let prefs = UserPrefs::load();
                ProjectIOService::new(&prefs)
            },
            user_prefs: UserPrefs::load(),
            text_input: crate::text_input::TextInputState::new(),
            audio_sync: match ImportedAudioSyncController::new() {
                Ok(ctrl) => Some(ctrl),
                Err(e) => {
                    log::warn!("[Audio] Failed to initialize audio sync: {}", e);
                    None
                }
            },
            pending_audio_load: None,
            percussion_orchestrator: PercussionImportOrchestrator::new(
                None,
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
                    .unwrap_or_default(),
            ),
            transport_controller: manifold_playback::transport_controller::TransportController::new(),
            input_handler: crate::input_handler::InputHandler::new(),
            overlay: manifold_ui::interaction_overlay::InteractionOverlay::new(
                manifold_ui::color::CLIP_VERTICAL_PAD,
            ),
            pre_drag_commands: Vec::new(),
            display_resolutions: Vec::new(),
            initialized: false,
            pending_toggle_output: false,
            pending_close_output: false,
            needs_rebuild: false,
            needs_scroll_rebuild: false,
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
        // Priority 1: Active drag — cursor set by InteractionOverlay
        // (overlay calls host.set_cursor() during drag, so we just skip here)
        {
            use manifold_ui::interaction_overlay::DragMode;
            match self.overlay.drag_mode() {
                DragMode::Move | DragMode::TrimLeft | DragMode::TrimRight | DragMode::RegionSelect => return,
                DragMode::None => {}
            }
        }

        // Priority 2: Inspector resize edge hover
        if self.ui_root.inspector_resize_dragging || self.ui_root.is_near_inspector_edge(self.cursor_pos) {
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
            return;
        }

        // Priority 3: Video/timeline split handle hover
        // Use the same hit test as click detection (layout.split_handle rect).
        let near_split = self.split_dragging || self.ui_root.layout.is_near_split_handle(self.cursor_pos);
        if near_split {
            if !self.split_dragging {
                self.ui_root.set_split_handle_hover();
            }
            self.cursor_manager.set(TimelineCursor::ResizeVertical);
            self.split_was_hovered = true;
            return;
        } else if self.split_was_hovered && !self.split_dragging {
            self.ui_root.set_split_handle_idle();
            self.split_was_hovered = false;
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

    // ── Project I/O — delegates to ProjectIOService ────────────────────

    /// Save. Delegates to ProjectIOService.save_project.
    fn save_project(&mut self) {
        let current_time = self.engine.current_time();
        let current_path = self.current_project_path.clone();
        if let Some(project) = self.engine.project_mut() {
            let action = self.project_io.save_project(
                project,
                current_path.as_deref(),
                current_time,
                &mut self.editing_service,
                &mut self.user_prefs,
            );
            self.apply_project_io_action(action);
        }
    }

    /// Save As. Delegates to ProjectIOService.save_project_as.
    fn save_project_as(&mut self) {
        let current_time = self.engine.current_time();
        if let Some(project) = self.engine.project_mut() {
            let action = self.project_io.save_project_as(
                project,
                current_time,
                &mut self.editing_service,
                &mut self.user_prefs,
            );
            self.apply_project_io_action(action);
        }
    }

    /// Open. Delegates to ProjectIOService.open_project.
    fn open_project(&mut self) {
        let action = self.project_io.open_project(&mut self.user_prefs);
        self.apply_project_io_action(action);
    }

    /// Open Recent. Delegates to ProjectIOService.open_recent_project.
    fn open_recent_project(&mut self) {
        let action = self.project_io.open_recent_project(&mut self.user_prefs);
        self.apply_project_io_action(action);
    }

    /// Shared project-load logic — called by open, open recent, and file drop.
    /// Delegates load+persist to ProjectIOService, then handles GPU/audio side-effects.
    fn open_project_from_path(&mut self, path: std::path::PathBuf) {
        let action = self.project_io.open_project_from_path(&path, &mut self.user_prefs);
        self.apply_project_io_action(action);
    }

    /// Apply a ProjectIOAction returned by ProjectIOService.
    /// Handles all side-effects that require Application-owned state:
    /// engine init, GPU resize, audio loading, selection reset, etc.
    fn apply_project_io_action(&mut self, action: ProjectIOAction) {
        // Apply loaded project (replaces host.PrepareForProjectSwitch + ApplyProject + OnProjectOpened)
        if let Some(project) = action.apply_project {
            let t_total = std::time::Instant::now();

            // PrepareForProjectSwitch — clean up previous audio/waveform state
            // Unity: WorkspaceController.ProjectIO.cs PrepareForProjectSwitch()
            if let Some(ref mut audio_sync) = self.audio_sync {
                audio_sync.reset_audio();
            }
            self.ui_root.waveform_lane.clear_audio();
            self.ui_root.layout.waveform_lane_visible = false;
            self.pending_audio_load = None;

            // Apply saved layout before initializing
            self.ui_root.apply_project_layout(&project.settings);
            let saved_time = project.saved_playhead_time;

            let t1 = std::time::Instant::now();
            self.engine.initialize(project);
            eprintln!("[PROJECT LOAD] engine.initialize: {:.1}ms", t1.elapsed().as_secs_f64() * 1000.0);

            // Restore playhead position
            if saved_time > 0.0 {
                self.engine.seek_to(saved_time);
            }

            // Resize compositor + generators to project resolution
            if let Some(proj) = self.engine.project() {
                let w = proj.settings.output_width.max(1) as u32;
                let h = proj.settings.output_height.max(1) as u32;
                if let Some(gpu) = &self.gpu {
                    let t2 = std::time::Instant::now();
                    if let Some(compositor) = &mut self.compositor {
                        compositor.resize(&gpu.device, w, h);
                    }
                    let (renderers, _) = self.engine.split_renderer_project();
                    for renderer in renderers.iter_mut() {
                        if let Some(gen) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                            gen.resize_gpu(w, h);
                            break;
                        }
                    }
                    eprintln!("[PROJECT LOAD] GPU resize: {:.1}ms ({}x{})", t2.elapsed().as_secs_f64() * 1000.0, w, h);
                }
            }

            // Spawn background audio loading
            let mut audio_path_for_load: Option<(String, f32)> = None;
            if self.audio_sync.is_some() {
                if let Some(proj) = self.engine.project() {
                    if let Some(ref perc) = proj.percussion_import {
                        if let Some(ref audio_path) = perc.audio_path {
                            if !audio_path.is_empty() {
                                audio_path_for_load = Some((audio_path.clone(), perc.audio_start_beat));
                                // Show the waveform lane immediately — audio decode
                                // will populate it once the background thread finishes.
                                self.ui_root.layout.waveform_lane_visible = true;
                            }
                        }
                    }
                }
            }

            if let Some((audio_path, start_beat)) = audio_path_for_load {
                let (tx, rx) = std::sync::mpsc::channel();
                self.pending_audio_load = Some(rx);

                std::thread::Builder::new()
                    .name("audio-load".into())
                    .spawn(move || {
                        let t_audio = std::time::Instant::now();
                        let preloaded = match manifold_playback::audio_sync::preload_audio(&audio_path, start_beat) {
                            Ok(p) => p,
                            Err(e) => {
                                log::warn!("[ProjectIO] Background audio load failed: {}", e);
                                return;
                            }
                        };
                        eprintln!("[PROJECT LOAD] audio decode (background): {:.1}ms", t_audio.elapsed().as_secs_f64() * 1000.0);

                        // Extract waveform PCM from kira's already-decoded frames (no second decode).
                        // Unity does the same: decode once, then AudioClip.GetData() for waveform.
                        let t_wave = std::time::Instant::now();
                        let waveform = Some(DecodedAudio::from_static_sound_data(&preloaded.sound_data));
                        eprintln!("[PROJECT LOAD] waveform extract from kira frames: {:.1}ms", t_wave.elapsed().as_secs_f64() * 1000.0);

                        let _ = tx.send(PendingAudioLoadResult { preloaded, waveform });
                    })
                    .expect("Failed to spawn audio load thread");
            }

            self.editing_service.set_project();
            self.selection.clear_selection();
            self.active_layer_index = Some(0);
            self.needs_rebuild = true;

            eprintln!("[PROJECT LOAD] total sync time: {:.1}ms (audio loading continues in background)", t_total.elapsed().as_secs_f64() * 1000.0);
        }

        // Set project path
        if let Some(path) = action.set_project_path {
            self.current_project_path = if path.as_os_str().is_empty() {
                None
            } else {
                Some(path)
            };
        }

        // Structural sync
        if action.needs_structural_sync {
            self.needs_structural_sync = true;
        }

        // Clip sync
        if action.needs_clip_sync {
            self.needs_rebuild = true;
        }
    }

    /// Poll for completed background audio load and apply results.
    /// Called each frame from tick_and_render.
    fn poll_pending_audio_load(&mut self) {
        let rx = match self.pending_audio_load.as_ref() {
            Some(rx) => rx,
            None => return,
        };

        match rx.try_recv() {
            Ok(result) => {
                self.pending_audio_load = None;

                if let Some(ref mut audio_sync) = self.audio_sync {
                    if let Err(e) = audio_sync.apply_preloaded(result.preloaded) {
                        log::warn!("[ProjectIO] Failed to apply loaded audio: {}", e);
                    }
                }

                if let Some(decoded) = result.waveform {
                    self.ui_root.waveform_lane.set_audio_data(
                        &decoded.samples,
                        decoded.channels,
                        decoded.sample_rate,
                    );
                    self.ui_root.layout.waveform_lane_visible = true;
                    log::info!("[Waveform] Decoded audio for waveform display");
                }

                eprintln!("[PROJECT LOAD] audio load applied to main thread");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_audio_load = None;
            }
        }
    }

    /// Open an HDR output window on the specified (or non-primary) monitor.
    /// Uses borderless fullscreen to avoid macOS fullscreen transition stall.
    /// Surface is Rgba16Float — wgpu v28 Metal backend auto-enables EDR.
    /// Unity: NativeMonitorWindowController.cs + MonitorWindowPlugin.mm.
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

        // Resolve target monitor.
        // If display_index is given, use that. Otherwise pick first non-primary monitor.
        // If only one monitor, use it (user's primary display).
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        let target_monitor = if let Some(idx) = display_index {
            monitors.get(idx).cloned()
        } else {
            // Pick non-primary: primary is usually index 0
            if monitors.len() > 1 {
                Some(monitors[1].clone())
            } else {
                monitors.first().cloned()
            }
        };

        let monitor = match target_monitor {
            Some(m) => m,
            None => {
                log::error!("[OutputWindow] No monitors available");
                return;
            }
        };

        let mon_phys_size = monitor.size();
        let mon_pos = monitor.position();
        let scale_factor = monitor.scale_factor();
        let mon_name = monitor.name().unwrap_or_else(|| "Unknown".to_string());

        // Log all available monitors for debugging
        for (i, m) in monitors.iter().enumerate() {
            let s = m.size();
            let p = m.position();
            let sf = m.scale_factor();
            let n = m.name().unwrap_or_else(|| "?".to_string());
            eprintln!(
                "[OutputWindow] Monitor {}: '{}' physical={}x{} pos=({},{}) scale={:.2} logical={}x{}",
                i, n, s.width, s.height, p.x, p.y, sf,
                (s.width as f64 / sf) as u32, (s.height as f64 / sf) as u32
            );
        }

        // Use logical coordinates — macOS window placement uses logical (point) coords.
        // Physical pixels / scale_factor = logical points.
        let logical_w = mon_phys_size.width as f64 / scale_factor;
        let logical_h = mon_phys_size.height as f64 / scale_factor;
        let logical_x = mon_pos.x as f64 / scale_factor;
        let logical_y = mon_pos.y as f64 / scale_factor;

        eprintln!(
            "[OutputWindow] Target '{}': logical={:.0}x{:.0} at ({:.0},{:.0}), physical={}x{}, scale={:.2}",
            mon_name, logical_w, logical_h, logical_x, logical_y,
            mon_phys_size.width, mon_phys_size.height, scale_factor
        );

        // "Fake fullscreen" — borderless window sized and positioned in logical coords
        // to cover the target monitor exactly. Avoids macOS fullscreen spaces transition.
        let attrs = winit::window::Window::default_attributes()
            .with_title(format!("MANIFOLD - {}", name))
            .with_decorations(false)
            .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
            .with_position(winit::dpi::Position::Logical(
                winit::dpi::LogicalPosition::new(logical_x, logical_y),
            ))
            .with_inner_size(winit::dpi::LogicalSize::new(logical_w, logical_h));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("[OutputWindow] Failed to create window: {e}");
                return;
            }
        };

        // Make the output window click-through and non-focusable.
        // All mouse/keyboard input passes to the workspace window behind it.
        // Unity: MonitorWindowPlugin.mm sets NSWindow.ignoresMouseEvents = YES.
        if let Err(e) = window.set_cursor_hittest(false) {
            log::warn!("[OutputWindow] set_cursor_hittest(false) failed: {e} — window may steal input");
        }

        let size = window.inner_size();
        let scale = window.scale_factor();

        // Create HDR surface (Rgba16Float) — wgpu auto-enables EDR on Metal.
        // Linear HDR values pass through directly; macOS clips at display's physical max.
        let surface = SurfaceWrapper::new_hdr(
            &gpu.instance,
            &gpu.adapter,
            &gpu.device,
            window.clone(),
            size.width,
            size.height,
            scale,
            wgpu::PresentMode::Fifo,
        );

        let surface_format = surface.format();

        // Create a blit pipeline matching the output surface format.
        // The main workspace uses Bgra8UnormSrgb; the output window uses Rgba16Float.
        // Each needs its own pipeline for the target format.
        if self.output_blit_pipeline.is_none()
            || self.output_blit_format != Some(surface_format)
        {
            self.output_blit_pipeline = Some(BlitPipeline::new(&gpu.device, surface_format));
            self.output_blit_format = Some(surface_format);
            log::info!(
                "[OutputWindow] Created blit pipeline for {:?}",
                surface_format
            );
        }

        let id = window.id();
        let resolved_index = display_index.or_else(|| {
            if monitors.len() > 1 { Some(1) } else { Some(0) }
        });

        let state = WindowState {
            window,
            surface,
            role: WindowRole::Output {
                name: name.to_string(),
            },
            display_index: resolved_index,
        };

        self.window_registry.add(id, state);
        log::info!(
            "[OutputWindow] Opened '{}' on '{}' ({}x{}, {:?})",
            name, mon_name, size.width, size.height, surface_format
        );
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
            export_fixed_dt: 0.0, // non-zero only during video export (GAP-IO-4)
        };
        let tick_result = self.engine.tick(ctx);

        // 1b. Poll for completed background audio load (async decode from project open)
        self.poll_pending_audio_load();

        // 1c. Sync imported audio playback to timeline
        // Port of Unity WorkspaceController.LateUpdate → audioSyncController.UpdateSync
        if let Some(ref mut audio_sync) = self.audio_sync {
            audio_sync.update_sync(&mut self.engine);
        }

        // 1d. Tick percussion import orchestrator (poll-based state machine)
        let was_importing = self.percussion_orchestrator.is_import_in_progress();
        {
            let current_beat = self.engine.current_beat();
            if let Some(project) = self.engine.project_mut() {
                self.percussion_orchestrator.tick(
                    self.time_since_start,
                    project,
                    &mut self.editing_service,
                    current_beat,
                );
            }
        }
        let is_importing = self.percussion_orchestrator.is_import_in_progress();

        // 1e. Sync percussion pipeline status to header panel
        // Port of Unity WorkspaceController.RefreshPercussionImportStatusLabel
        {
            let msg = self.percussion_orchestrator.status_message();
            let progress = self.percussion_orchestrator.status_progress01();
            let show = self.percussion_orchestrator.show_progress_bar() && !msg.is_empty();
            self.ui_root.header.set_import_status(
                &mut self.ui_root.tree,
                msg,
                if progress < 0.0 { 0.0 } else { progress.clamp(0.0, 1.0) },
                show,
            );
            // Force UI rebuild while pipeline is running (progress bar updates)
            // and on completion (new clips/layers need to appear).
            if is_importing {
                self.needs_rebuild = true;
            }
            if was_importing && !is_importing {
                // Pipeline just finished — structural sync to pick up new clips/layers.
                self.needs_structural_sync = true;
                self.needs_rebuild = true;
            }
        }

        // 2. Process UI events and dispatch actions
        let mut actions = self.ui_root.process_events();

        // 2a. Route viewport tracks-area events through InteractionOverlay.
        // These events were stashed by process_events() because the overlay
        // needs &mut TimelineEditingHost which UIRoot can't provide.
        {
            let viewport_events = self.ui_root.drain_viewport_events();
            if !viewport_events.is_empty() {
                // Sync modifier state to overlay (Unity reads Keyboard.current inline)
                self.overlay.set_modifiers(self.modifiers);
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.engine,
                    &mut self.editing_service,
                    &mut self.cursor_manager,
                    &mut self.active_layer_index,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.needs_scroll_rebuild,
                    &mut self.pre_drag_commands,
                );
                for event in &viewport_events {
                    use manifold_ui::input::UIEvent;
                    match event {
                        UIEvent::Click { pos, modifiers, .. } => {
                            self.overlay.on_pointer_click(
                                *pos, modifiers.shift, modifiers.ctrl || modifiers.command,
                                1, false,
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::DoubleClick { pos, modifiers, .. } => {
                            self.overlay.on_pointer_click(
                                *pos, modifiers.shift, modifiers.ctrl || modifiers.command,
                                2, false,
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::RightClick { pos, .. } => {
                            self.overlay.on_pointer_click(
                                *pos, false, false,
                                1, true,
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::DragBegin { origin, .. } => {
                            self.overlay.on_begin_drag(
                                *origin, &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::Drag { pos, .. } => {
                            self.overlay.on_drag(
                                *pos, &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        UIEvent::DragEnd { .. } => {
                            self.overlay.on_end_drag(
                                &mut host, &mut self.selection, &self.ui_root.viewport,
                            );
                        }
                        _ => {}
                    }
                }

                // Drain actions generated by the host during overlay processing
                // (right-click context menus: ClipRightClicked, TrackRightClicked).
                actions.append(&mut host.pending_actions);
            }
        }

        // Overlay-generated right-click actions (TrackRightClicked, ClipRightClicked)
        // arrive AFTER process_events() has already run its try_open_dropdown pass.
        // Route them through the dropdown system now so context menus actually open.
        self.ui_root.intercept_overlay_actions(&mut actions);

        // Consume deferred structural sync flag (set by keyboard shortcuts)
        let mut needs_structural_sync = self.needs_structural_sync;
        self.needs_structural_sync = false;
        let mut needs_resolution_resize = false;
        let prev_active_layer = self.active_layer_index;
        let prev_sel_version = self.selection.selection_version;
        for action in &actions {
            // Intercept actions that need Application-level access
            match action {
                PanelAction::ToggleMonitor => { self.pending_toggle_output = true; continue; }
                PanelAction::SaveProject => { self.save_project(); continue; }
                PanelAction::SaveProjectAs => { self.save_project_as(); continue; }
                PanelAction::OpenProject => { self.open_project(); needs_structural_sync = true; continue; }
                PanelAction::OpenRecent => { self.open_recent_project(); needs_structural_sync = true; continue; }
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
                &mut self.active_layer_index,
                &mut self.slider_snapshot,
                &mut self.trim_snapshot,
                &mut self.adsr_snapshot,
                &mut self.target_snapshot,
                &mut self.percussion_orchestrator,
                &mut self.audio_sync,
                &mut self.user_prefs,
            );
            if result.structural_change {
                needs_structural_sync = true;
            }
            if result.resolution_changed {
                needs_resolution_resize = true;
            }
        }

        // Resize compositor + generator when resolution preset changes
        if needs_resolution_resize {
            let dims = self.engine.project().map(|p| {
                (p.settings.output_width.max(1) as u32, p.settings.output_height.max(1) as u32)
            });
            if let Some((w, h)) = dims {
                if let Some(gpu) = &self.gpu {
                    if let Some(compositor) = &mut self.compositor {
                        compositor.resize(&gpu.device, w, h);
                    }
                    // Resize generator renderer via engine downcast
                    let (renderers, _) = self.engine.split_renderer_project();
                    for renderer in renderers.iter_mut() {
                        if let Some(gen) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                            gen.resize_gpu(w, h);
                            break;
                        }
                    }
                }
                log::info!("Resolution changed to {}x{}", w, h);
            }
        }

        // Selection version change → sync inspector so it shows the newly selected clip
        if self.selection.selection_version != prev_sel_version && !needs_structural_sync {
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index, &self.selection);
            needs_structural_sync = true;
        }

        if needs_structural_sync {
            crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.engine, self.active_layer_index);
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index, &self.selection);
        } else if self.active_layer_index != prev_active_layer {
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index, &self.selection);
            needs_structural_sync = true; // Inspector content changed — needs rebuild
        }
        // 2a. Per-frame drag polling with auto-scroll.
        // InteractionOverlay.PollMoveDrag — continues edge auto-scroll when mouse is stationary.
        {
            use manifold_ui::interaction_overlay::DragMode;
            if self.overlay.drag_mode() == DragMode::Move {
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.engine,
                    &mut self.editing_service,
                    &mut self.cursor_manager,
                    &mut self.active_layer_index,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.needs_scroll_rebuild,
                    &mut self.pre_drag_commands,
                );
                self.overlay.poll_move_drag(
                    self.cursor_pos, &mut host, &mut self.selection, &self.ui_root.viewport,
                );
            }
        }
        // Legacy drag polling removed — overlay.poll_move_drag() handles it above.

        // 2b. Auto-scroll check for playback (BEFORE build so rebuild includes new scroll)
        let auto_scroll_changed = crate::ui_bridge::check_auto_scroll(&mut self.ui_root, &self.engine);
        let scroll_changed = auto_scroll_changed || self.needs_scroll_rebuild;
        self.needs_scroll_rebuild = false;

        // 3. Rebuild if needed
        // Full rebuild: structural changes, data mutations, or explicit needs_rebuild.
        // Partial rebuild: only scroll/zoom changed — rebuild viewport + layer_headers,
        // preserve transport, header, footer, inspector nodes.
        // From Unity: CheckScrollAndInvalidate only repaints affected layers.
        //
        // GUARD: If the inspector has an active drag (slider being dragged), defer
        // the rebuild to prevent node destruction mid-drag which causes snap-back.
        // Unity avoids this because rebuilds only happen on structural changes and
        // SyncValues() dirty-checks against the data model without rebuilding.
        let inspector_dragging = self.ui_root.inspector.is_dragging();
        if self.needs_rebuild || needs_structural_sync {
            if inspector_dragging {
                // Defer — keep needs_rebuild set so it fires after drag ends
                // But still rebuild scroll panels if needed (they're separate from inspector)
                if scroll_changed {
                    self.ui_root.rebuild_scroll_panels();
                }
            } else {
                self.needs_rebuild = false;
                self.ui_root.build();
            }
        } else if scroll_changed {
            self.ui_root.rebuild_scroll_panels();
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

        // 4b. Sync clip positions from live project model every frame.
        // During drag, the InteractionOverlay mutates clip data directly in the
        // project model, but the viewport's clips_by_layer cache is only refreshed
        // via sync_project_data() (structural sync). This per-frame sync ensures
        // bitmap renderers see mutated clip positions and repaint during drag.
        // Cost: iterates layers+clips, but the bitmap fingerprint skips repaint
        // when nothing changed (cheap no-op outside of drag).
        crate::ui_bridge::sync_clip_positions(&mut self.ui_root, &self.engine);

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

        // 6a. Update waveform lane overlay (position + playhead for dirty-checking)
        {
            let playhead_beat = self.engine.current_beat();
            let scroll_x = self.ui_root.viewport.scroll_x_beats() * self.ui_root.viewport.pixels_per_beat();
            let wf = &mut self.ui_root.waveform_lane;
            if wf.is_ready() {
                // Get start beat and duration from project percussion import state
                let (start_beat, duration_beats) = if let Some(proj) = self.engine.project() {
                    if let Some(ref perc) = proj.percussion_import {
                        let dur_sec = wf.clip_duration_seconds();
                        let bpm = proj.settings.bpm.max(1.0);
                        let dur_beats = dur_sec * bpm / 60.0;
                        (perc.audio_start_beat, dur_beats)
                    } else {
                        (0.0, 0.0)
                    }
                } else {
                    (0.0, 0.0)
                };
                let mapper = self.ui_root.viewport.mapper();
                wf.update_overlay(
                    start_beat,
                    duration_beats,
                    playhead_beat,
                    scroll_x,
                    self.ui_root.viewport.tracks_rect().width,
                    mapper,
                );
            }
        }

        // 6b. Repaint dirty layer bitmaps (CPU pixel painting).
        // Build BitmapRepaintState from current selection/hover.
        {
            let hovered = self.ui_root.viewport.hovered_clip_id().map(|s| s.to_string());
            let sel_region = self.ui_root.viewport.selection_region_ref().cloned();
            let has_region = sel_region.is_some();
            let insert_cursor_beat = self.ui_root.viewport.insert_cursor_beat();
            let insert_layer = self.selection.insert_cursor_layer_index;
            let has_insert = self.selection.has_insert_cursor();
            let ppb = self.ui_root.viewport.pixels_per_beat();
            let sel_ver = self.selection.selection_version;

            let state = manifold_ui::BitmapRepaintState {
                selection_version: sel_ver,
                is_selected: &|id: &str| self.selection.is_selected(id),
                hovered_clip_id: hovered.as_deref(),
                has_region,
                region: sel_region.as_ref(),
                has_insert_cursor: has_insert,
                insert_cursor_beat,
                insert_cursor_layer: insert_layer,
                pixels_per_beat: ppb,
            };
            self.ui_root.viewport.repaint_dirty_layers(&state);
        }

        // 6c. Upload dirty layer textures to GPU
        if let (Some(gpu), Some(bitmap_gpu)) = (&self.gpu, &mut self.layer_bitmap_gpu) {
            for (layer_idx, pixels, tw, th) in self.ui_root.viewport.dirty_layer_iter() {
                bitmap_gpu.upload_layer(
                    &gpu.device, &gpu.queue,
                    layer_idx, pixels, tw as u32, th as u32,
                );
            }

            // 6d. Repaint + upload waveform lane if dirty
            let wf_rect = self.ui_root.viewport.waveform_lane_rect();
            if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                let wf = &mut self.ui_root.waveform_lane;
                // Force dirty on resize
                if wf.buffer_width != wf_rect.width as usize {
                    wf.dirty = true;
                }
                if wf.dirty {
                    wf.repaint(wf_rect.width as usize);
                    // Upload after repaint
                    if wf.buffer_width > 0 && wf.buffer_height > 0 && !wf.pixel_buffer.is_empty() {
                        bitmap_gpu.upload_layer(
                            &gpu.device, &gpu.queue,
                            1000, &wf.pixel_buffer,
                            wf.buffer_width as u32, wf.buffer_height as u32,
                        );
                    }
                }
            }
        }

        // tick_result was computed at the top of tick_and_render (engine ticked first)

        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        // Extract timing values before split borrow
        let time = self.engine.current_time();
        let beat = self.engine.current_beat();

        let mut encoder =
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Frame Encoder"),
                });

        // Split borrow: get renderers + project from engine simultaneously.
        // Engine now owns the real GeneratorRenderer (replaced stub in init_gpu),
        // so clip lifecycle (start/stop) is handled by engine's sync_clips_to_time.
        let (renderers, project) = self.engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // 4. Render generators via downcast (GPU rendering needs queue + encoder)
        for renderer in renderers.iter_mut() {
            if let Some(gen) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                gen.render_all(&gpu.queue, &mut encoder, time, beat, dt as f32, layers);
                break;
            }
        }

        // 5. Build clip descriptors for compositor
        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());

        for clip in &tick_result.ready_clips {
            let texture_view = renderers.iter().find_map(|r| {
                r.as_any().downcast_ref::<GeneratorRenderer>()
                    .and_then(|gen| gen.get_clip_texture_view(&clip.id))
            });
            if let Some(view) = texture_view {
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

        // 6. Build layer descriptors for compositor
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

        // 7. Composite
        let compositor = match &mut self.compositor {
            Some(c) => c,
            None => return,
        };

        let master_effects = project.map_or(&empty_effects[..], |p| &p.settings.master_effects);
        let master_effect_groups = project
            .and_then(|p| p.settings.master_effect_groups.as_deref())
            .unwrap_or(&empty_groups);

        let frame = CompositorFrame {
            time,
            beat,
            dt: dt as f32,
            frame_count: self.frame_count,
            compositor_dirty: tick_result.compositor_dirty,
            clips: &clip_descs,
            layers: &layer_descs,
            master_effects,
            master_effect_groups,
            tonemap: TonemapSettings::default(),
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
        // Compositor aspect ratio for aspect-correct blitting (FitInParent)
        let (comp_w, comp_h) = self.compositor.as_ref()
            .map(|c| c.dimensions())
            .unwrap_or((1920, 1080));
        let source_aspect = comp_w as f32 / comp_h as f32;

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
                blit.blit_to_rect_fit(
                    &gpu.device, &mut encoder, compositor_output, &surface_view,
                    video_rect.x * sf, video_rect.y * sf,
                    video_rect.width * sf, video_rect.height * sf,
                    source_aspect,
                );
            } else {
                // Output windows: HDR blit with aspect-correct letterbox/pillarbox.
                // Uses the output-specific blit pipeline (Rgba16Float target format)
                // if available, otherwise falls back to the workspace blit pipeline.
                let output_blit = self.output_blit_pipeline.as_ref().unwrap_or(blit);
                {
                    let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Clear Output"),
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
                output_blit.blit_to_rect_fit(
                    &gpu.device, &mut encoder, compositor_output, &surface_view,
                    0.0, 0.0, surface_w as f32, surface_h as f32,
                    source_aspect,
                );
            }

            // Draw UI overlay on workspace window using the UITree
            // Pass logical pixel dimensions — the tree is built in logical coords
            if is_workspace {
                let logical_w = (surface_w as f64 / scale) as u32;
                let logical_h = (surface_h as f64 / scale) as u32;

                // Pass 1: UITree rects + text (track backgrounds, ruler, chrome panels).
                // When dropdown is open, skip overlay nodes — they render in Pass 4.
                // Uses TextMode::Main so base UI text goes to the main TextRenderer's
                // own vertex buffer, isolated from the overlay TextRenderer.
                if let Some(ui) = &mut self.ui_renderer {
                    let skip_from = if self.ui_root.dropdown.is_open() {
                        Some(self.ui_root.dropdown.first_node())
                    } else {
                        None
                    };
                    ui.render_tree(&self.ui_root.tree, skip_from);
                    ui.render(
                        &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                        logical_w, logical_h, scale, TextMode::Main,
                    );
                }

                // Pass 2: Layer bitmap textures + waveform lane (alpha-blend over track BGs)
                if let Some(bitmap_gpu) = &mut self.layer_bitmap_gpu {
                    let mut rects = self.ui_root.viewport.layer_bitmap_rects();

                    // Add waveform lane rect (texture at reserved index 1000)
                    let wf_rect = self.ui_root.viewport.waveform_lane_rect();
                    if wf_rect.width > 0.0 && wf_rect.height > 0.0 {
                        rects.push((1000, wf_rect));
                    }

                    if !rects.is_empty() {
                        bitmap_gpu.render_layers(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, &rects,
                        );
                    }
                }

                // Pass 3: Playhead track-area line ONLY (on top of bitmap textures).
                // TextMode::Skip — no text, no glyphon prepare(), no buffer mutation.
                if let Some(ui) = &mut self.ui_renderer {
                    if let Some(px) = self.ui_root.viewport.playhead_pixel() {
                        let tr = self.ui_root.viewport.get_tracks_rect();
                        ui.draw_rect(
                            px - 1.0, tr.y,
                            manifold_ui::color::PLAYHEAD_WIDTH, tr.height,
                            manifold_ui::color::PLAYHEAD_RED.to_f32(),
                        );
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Skip,
                        );
                    }
                }

                // Pass 4: Dropdown overlay — renders ON TOP of layer bitmaps and playhead.
                // Uses TextMode::Overlay so dropdown text goes to a separate TextRenderer
                // with its own vertex buffer, preventing corruption of Pass 1's text.
                if self.ui_root.dropdown.is_open() {
                    if let Some(ui) = &mut self.ui_renderer {
                        let start = self.ui_root.dropdown.first_node();
                        ui.render_overlay(&self.ui_root.tree, start);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }
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

        let fallback_size = winit::dpi::LogicalSize::new(1280u32, 720u32);
        let startup_size = event_loop
            .primary_monitor()
            .map(|monitor| monitor.size())
            .unwrap_or_else(|| fallback_size.to_physical(1.0));

        let attrs = winit::window::Window::default_attributes()
            .with_title("MANIFOLD")
            .with_inner_size(startup_size)
            .with_maximized(true);

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

        // Detect connected display resolutions (Unity: Footer.CollectDisplayResolutions).
        // Use the highest video mode resolution per monitor — this is the native panel
        // resolution, not the current macOS scaled resolution. Gives pixel-perfect output.
        self.display_resolutions.clear();
        for (i, monitor) in event_loop.available_monitors().enumerate() {
            // Find the native (highest) resolution from video modes
            let native_size = monitor.video_modes()
                .max_by_key(|vm| {
                    let s = vm.size();
                    (s.width as u64) * (s.height as u64)
                })
                .map(|vm| vm.size());

            let (w, h) = match native_size {
                Some(s) if s.width > 0 && s.height > 0 => (s.width, s.height),
                _ => {
                    // Fallback to monitor.size() (current scaled resolution)
                    let s = monitor.size();
                    (s.width, s.height)
                }
            };

            let scaled = monitor.size();
            let label = monitor.name().unwrap_or_else(|| format!("Display {}", i + 1));
            log::info!(
                "Detected monitor: {} native={}x{} scaled={}x{} scale={:.2}",
                label, w, h, scaled.width, scaled.height, monitor.scale_factor()
            );

            if w > 0 && h > 0 {
                self.display_resolutions.push((w, h, label));
            }
        }
        // Rename to "Display N" for consistent UI (Unity uses 1-indexed "Display N")
        for (i, entry) in self.display_resolutions.iter_mut().enumerate() {
            entry.2 = format!("Display {}", i + 1);
        }

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
                            required_limits: adapter.limits(),
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
            let device = Arc::new(device);
            let queue = Arc::new(queue);

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

            // Create layer bitmap GPU (textured quad pipeline for per-layer bitmaps)
            self.layer_bitmap_gpu = Some(manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu::new(&device, format));

            // Create generator renderer and compositor at project resolution
            let (output_w, output_h) = if let Some(project) = self.engine.project() {
                (project.settings.output_width.max(1) as u32, project.settings.output_height.max(1) as u32)
            } else {
                (1920u32, 1080u32)
            };
            let compositor_format = wgpu::TextureFormat::Rgba16Float;

            // Replace the generator stub with the real GeneratorRenderer
            self.engine.replace_renderer(1, Box::new(GeneratorRenderer::new(
                Arc::clone(&device),
                output_w,
                output_h,
                compositor_format,
                8,
            )));

            self.compositor = Some(Box::new(LayerCompositor::new(&device, &queue, output_w, output_h)));
            eprintln!("[GPU INIT] generator/compositor resolution: {}x{}", output_w, output_h);

            GpuContext {
                instance,
                adapter,
                device,
                queue,
            }
        };

        self.gpu = Some(gpu);

        // Pass detected display resolutions to UI
        self.ui_root.set_display_resolutions(self.display_resolutions.clone());

        // Build UI at initial window size (logical pixels)
        let logical_w = size.width as f32 / scale as f32;
        let logical_h = size.height as f32 / scale as f32;
        self.ui_root.resize(logical_w, logical_h);

        // Push initial project data (layers, tracks) and rebuild
        crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.engine, self.active_layer_index);
        crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.engine, self.active_layer_index, &self.selection);

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

                        // Route hover through InteractionOverlay (port of Unity OnPointerMove).
                        // This handles: CursorBeat/CursorLayerIndex tracking, per-layer bitmap
                        // invalidation on hover change, and cursor shape feedback.
                        {
                            let mut host = crate::editing_host::AppEditingHost::new(
                                &mut self.engine,
                                &mut self.editing_service,
                                &mut self.cursor_manager,
                                &mut self.active_layer_index,
                                &mut self.needs_rebuild,
                                &mut self.needs_structural_sync,
                                &mut self.needs_scroll_rebuild,
                                &mut self.pre_drag_commands,
                            );
                            self.overlay.on_pointer_move(
                                self.cursor_pos,
                                &mut host,
                                &mut self.selection,
                                &self.ui_root.viewport,
                            );
                        }

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
                                        self.input_handler.inspector_has_focus = true;
                                    } else if timeline_rect.contains(self.cursor_pos) {
                                        self.input_handler.inspector_has_focus = false;
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
                                        self.ui_root.set_split_handle_drag();
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
                                        self.ui_root.set_split_handle_idle();
                                        // Persist to ProjectSettings (Unity WorkspaceController line 591)
                                        if let Some(project) = self.engine.project_mut() {
                                            project.settings.timeline_height_percent =
                                                self.ui_root.layout.timeline_split_ratio;
                                        }
                                    } else if self.ui_root.inspector_resize_dragging {
                                        // Persist to ProjectSettings (Unity WorkspaceController line 528)
                                        if let Some(project) = self.engine.project_mut() {
                                            project.settings.inspector_width =
                                                self.ui_root.layout.inspector_width;
                                        }
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
                    // Convert line deltas (mouse wheel notches) to logical pixels.
                    // Each downstream consumer applies its own speed constant on top.
                    const LINE_DELTA_PX: f32 = 20.0;
                    let (dx, dy) = match delta {
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            (x * LINE_DELTA_PX, y * LINE_DELTA_PX)
                        }
                        winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
                    };

                    let pos = self.cursor_pos;
                    let inspector_rect = self.ui_root.layout.inspector();
                    let tracks_rect = self.ui_root.layout.timeline_tracks();

                    if inspector_rect.contains(pos) {
                        // Scroll the inspector panel — full rebuild (inspector is static)
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
                                // Zoom always requires rebuild (ppb changed)
                                self.ui_root.viewport.set_scroll(
                                    new_scroll.max(0.0),
                                    self.ui_root.viewport.scroll_y_px(),
                                );
                                self.needs_scroll_rebuild = true;
                            }
                        } else if self.modifiers.shift {
                            // Shift + scroll Y → horizontal pan
                            let ppb = self.ui_root.viewport.pixels_per_beat();
                            let beat_delta = dy * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                            let new_x = (self.ui_root.viewport.scroll_x_beats() - beat_delta).max(0.0);
                            if self.ui_root.viewport.set_scroll(
                                new_x,
                                self.ui_root.viewport.scroll_y_px(),
                            ) {
                                self.needs_scroll_rebuild = true;
                            }
                        } else {
                            // Plain scroll → vertical track scroll
                            let new_y = (self.ui_root.viewport.scroll_y_px() - dy).max(0.0);
                            if self.ui_root.viewport.set_scroll(
                                self.ui_root.viewport.scroll_x_beats(),
                                new_y,
                            ) {
                                // Sync layer headers with viewport vertical scroll
                                self.ui_root.layer_headers.set_scroll_y(
                                    self.ui_root.viewport.scroll_y_px(),
                                );
                                self.needs_scroll_rebuild = true;
                            }
                        }
                        // Native horizontal scroll (trackpad two-finger swipe)
                        if dx.abs() > 0.01 && !self.modifiers.alt {
                            let ppb = self.ui_root.viewport.pixels_per_beat();
                            let beat_delta = dx * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                            let new_x = (self.ui_root.viewport.scroll_x_beats() - beat_delta).max(0.0);
                            if self.ui_root.viewport.set_scroll(
                                new_x,
                                self.ui_root.viewport.scroll_y_px(),
                            ) {
                                self.needs_scroll_rebuild = true;
                            }
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
                    // ── Shortcut dispatch via InputHandler ──
                    // Port of Unity InputHandler.HandleKeyboardInput().
                    // All viewport access goes through the TimelineInputHost trait.
                    if !consumed {
                        let mut host = crate::input_host::AppInputHost {
                            engine: &mut self.engine,
                            editing: &mut self.editing_service,
                            ui_root: &mut self.ui_root,
                            selection: &mut self.selection,
                            active_layer: &mut self.active_layer_index,
                            needs_rebuild: &mut self.needs_rebuild,
                            needs_structural_sync: &mut self.needs_structural_sync,
                            needs_scroll_rebuild: &mut self.needs_scroll_rebuild,
                            current_project_path: &self.current_project_path,
                            has_output_window: self.window_registry.has_output_window(),
                            pending_close_output: &mut self.pending_close_output,
                        };
                        if self.input_handler.handle_keyboard_input(
                            &logical_key, self.modifiers,
                            &mut host,
                        ) {
                            consumed = true;
                        }
                    }

                    // File operations: Save/Open/New require rfd dialogs and window
                    // handles not available to AppInputHost. InputHandler returns false
                    // for these, so they fall through here.
                    if !consumed {
                    let m = self.modifiers;
                    match &logical_key {
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

                        _ => {}
                    }
                    } // end if !consumed (file operations)
                } // end if is_primary

                // All other shortcuts handled by InputHandler → AppInputHost.

                // (Legacy shortcut block deleted — was ~500 lines of duplicated dispatch.
                // All shortcuts now go through InputHandler → TimelineInputHost trait.
                // Only save/open/new remain as direct fallbacks above.)

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
                let ext = path.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                if crate::project_io::is_supported_video_extension(&path)
                    || crate::project_io::is_supported_midi_extension(&path)
                {
                    // Video/MIDI files → route through ProjectIOService.
                    // Drop at playhead beat on active layer (Unity ProcessDroppedFiles).
                    let drop_beat = self.engine.current_beat();
                    let drop_layer = self.active_layer_index.unwrap_or(0) as i32;
                    let spb = manifold_core::tempo::TempoMapConverter::seconds_per_beat_from_bpm(
                        self.engine.project().map(|p| p.settings.bpm).unwrap_or(120.0),
                    );
                    if let Some(project) = self.engine.project_mut() {
                        let action = self.project_io.process_dropped_files(
                            &[path.clone()],
                            drop_beat,
                            drop_layer,
                            project,
                            spb,
                        );
                        self.apply_project_io_action(action);
                    }
                } else if ext == "json" || ext == "manifold" {
                    // Project files → load project
                    self.open_project_from_path(path.clone());
                } else if matches!(ext.as_str(), "wav" | "mp3" | "flac" | "aiff" | "ogg") {
                    log::info!("Audio file dropped: {} (audio import not yet implemented)", path.to_string_lossy());
                } else {
                    log::debug!("Unrecognized file type dropped: {}", path.to_string_lossy());
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.initialized {
            return;
        }

        // Deferred output window toggle (needs ActiveEventLoop).
        // Close output window (Escape key or programmatic close)
        if self.pending_close_output {
            self.pending_close_output = false;
            let output_ids: Vec<_> = self.window_registry.iter()
                .filter(|(_, ws)| matches!(ws.role, WindowRole::Output { .. }))
                .map(|(id, _)| *id)
                .collect();
            let had_output = !output_ids.is_empty();
            for id in output_ids {
                self.window_registry.remove(&id);
            }
            if had_output {
                log::info!("[OutputWindow] Closed via Escape");
            }
        }

        // Toggle output window (UI button)
        if self.pending_toggle_output {
            self.pending_toggle_output = false;
            if self.window_registry.has_output_window() {
                self.pending_close_output = true; // will close next iteration
            } else {
                self.open_output_window(event_loop, "Output", None);
            }
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
