use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::layer::Layer;
use manifold_editing::service::EditingService;
use manifold_playback::audio_sync::{ImportedAudioSyncController, PreloadedAudioData};
use manifold_playback::audio_decoder::DecodedAudio;
use manifold_playback::percussion_orchestrator::PercussionImportOrchestrator;
use manifold_playback::engine::PlaybackEngine;
use manifold_playback::renderer::StubRenderer;
use manifold_renderer::blit::BlitPipeline;
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::LayerCompositor;
use manifold_renderer::surface::SurfaceWrapper;
use manifold_renderer::ui_renderer::{TextMode, UIRenderer};

use manifold_ui::cursors::{CursorManager, TimelineCursor};
use manifold_ui::input::{Modifiers, PointerAction};
use manifold_ui::node::Vec2;
use manifold_ui::panels::PanelAction;
use manifold_ui::ui_state::UIState;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
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
pub(crate) struct PendingAudioLoadResult {
    pub preloaded: PreloadedAudioData,
    pub waveform: Option<DecodedAudio>,
}

pub struct Application {
    // GPU
    gpu: Option<GpuContext>,

    // Windows
    window_registry: WindowRegistry,
    primary_window_id: Option<WindowId>,

    // Content thread communication
    content_tx: Option<crossbeam_channel::Sender<ContentCommand>>,
    state_rx: Option<crossbeam_channel::Receiver<ContentState>>,
    content_thread_handle: Option<std::thread::JoinHandle<()>>,

    /// Latest state snapshot from the content thread.
    content_state: ContentState,

    /// Local project snapshot for UI reads. Updated from content thread
    /// when data_version changes. During drag, snapshots are deferred.
    local_project: Project,

    /// After a local project load (open/new), suppress content thread snapshots
    /// until its data_version exceeds this value. Prevents the old project from
    /// overwriting the locally-loaded new project before the content thread
    /// processes the LoadProject command.
    suppress_snapshot_until: u64,

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

    // Effect clipboard (Unity: static EffectClipboard singleton, Rust: instance)
    effect_clipboard: manifold_editing::clipboard::EffectClipboard,

    // Rendering
    /// Shared reference to the content pipeline's output dimensions.
    content_pipeline_output: Option<Arc<crate::content_pipeline::SharedOutputView>>,
    /// IOSurface bridge for cross-device texture sharing (macOS).
    /// Content device writes compositor output to the IOSurface; UI device reads it.
    #[cfg(target_os = "macos")]
    shared_texture_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// UI-side wgpu::Texture imported from the IOSurface. Same GPU memory as
    /// the content-side texture — zero copy.
    #[cfg(target_os = "macos")]
    ui_shared_texture: Option<wgpu::Texture>,
    #[cfg(target_os = "macos")]
    ui_shared_view: Option<wgpu::TextureView>,
    /// Last seen bridge generation — detects resize (not per-frame).
    #[cfg(target_os = "macos")]
    last_bridge_generation: u64,
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

    // Pending audio load — receives results from background decode thread.
    // Unity loads audio async via coroutines; we use std::thread + mpsc channel.
    // Waveform data stays on UI thread; preloaded audio data is forwarded to content thread.
    pending_audio_load: Option<std::sync::mpsc::Receiver<PendingAudioLoadResult>>,

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
    /// Last data_version seen from content thread. When content_state.data_version
    /// is newer, accept the project snapshot (unless drag is in progress).
    last_accepted_data_version: u64,
}

impl Application {
    pub fn new() -> Self {
        let default_project = Self::create_default_project();

        Self {
            gpu: None,
            window_registry: WindowRegistry::new(),
            primary_window_id: None,
            content_tx: None,
            state_rx: None,
            content_thread_handle: None,
            content_state: ContentState::default(),
            local_project: default_project,
            suppress_snapshot_until: 0,
            selection: UIState::new(),
            active_layer_index: None,
            slider_snapshot: None,
            trim_snapshot: None,
            adsr_snapshot: None,
            target_snapshot: None,
            effect_clipboard: manifold_editing::clipboard::EffectClipboard::new(),
            content_pipeline_output: None,
            #[cfg(target_os = "macos")]
            shared_texture_bridge: None,
            #[cfg(target_os = "macos")]
            ui_shared_texture: None,
            #[cfg(target_os = "macos")]
            ui_shared_view: None,
            #[cfg(target_os = "macos")]
            last_bridge_generation: 0,
            blit_pipeline: None,
            output_blit_pipeline: None,
            output_blit_format: None,
            ui_renderer: None,
            layer_bitmap_gpu: None,
            surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            ui_root: UIRoot::new(),
            // UI frame rate: uncapped (120fps target, vsync limits actual present).
            // Content thread has its own timer at project FPS — fully decoupled.
            frame_timer: FrameTimer::new(120.0),
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
            pending_audio_load: None,
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
            last_accepted_data_version: 0,
        }
    }

    /// Send a command to the content thread (no-op if not yet spawned).
    fn send_content_cmd(&self, cmd: ContentCommand) {
        if let Some(ref tx) = self.content_tx {
            let _ = tx.try_send(cmd);
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

        let current_beat = self.selection.insert_cursor_beat.unwrap_or(self.content_state.current_beat);
        let current_layer = self.selection.insert_cursor_layer_index
            .or(self.active_layer_index)
            .unwrap_or(0);
        let grid_interval = self.ui_root.viewport.grid_step();

        // Build layer info for navigation (skip collapsed layers)
        let layers: Vec<NavLayerInfo> = Some(&self.local_project)
            .map(|p| p.timeline.layers.iter().enumerate().map(|(i, l)| {
                NavLayerInfo {
                    index: i,
                    height: if l.is_collapsed { 0.0 } else { 140.0 },
                }
            }).collect())
            .unwrap_or_default();

        // Build clip info for auto-select
        let clips: Vec<NavClipInfo> = Some(&self.local_project)
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
                let li = Some(&self.local_project)
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
                    if let Some(project) = Some(&mut self.local_project) {
                        let old_bpm = project.settings.bpm;
                        // Unity: skip if approximately equal
                        if (old_bpm - new_bpm).abs() >= 0.01 {
                            let cmd = manifold_editing::commands::settings::ChangeBpmCommand::new(
                                old_bpm, new_bpm,
                            );
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); self.send_content_cmd(ContentCommand::Execute(boxed)); }
                        }
                    }
                    self.needs_rebuild = true;
                }
            }
            TextInputField::Fps => {
                if let Ok(fps) = text.parse::<f32>() {
                    let fps = fps.clamp(1.0, 240.0);
                    if let Some(project) = Some(&mut self.local_project) {
                        let cmd = manifold_editing::commands::settings::ChangeFrameRateCommand::new(
                            project.settings.frame_rate, fps,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); self.send_content_cmd(ContentCommand::Execute(boxed)); }
                    }
                    // Content thread renders at project FPS; UI always runs at display rate.
                    self.send_content_cmd(ContentCommand::SetFrameRate(fps as f64));
                    self.needs_rebuild = true;
                }
            }
            TextInputField::LayerName(idx) => {
                if let Some(project) = Some(&mut self.local_project) {
                    if let Some(layer) = project.timeline.layers.get_mut(idx) {
                        layer.name = text.to_string();
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::ClipBpm => {
                // Unity: ClipInspector.OnBitmapBpmCommit
                // "auto" or empty → 0 (use project BPM), otherwise parse + clamp [20, 300]
                let trimmed = text.trim();
                let new_bpm = if trimmed.is_empty()
                    || trimmed.eq_ignore_ascii_case("auto")
                {
                    0.0
                } else if let Ok(v) = trimmed.parse::<f32>() {
                    if v > 0.0 { v.clamp(20.0, 300.0) } else { 0.0 }
                } else {
                    return; // parse failed — silent no-op (matches Unity)
                };
                if let Some(clip_id) = &self.selection.primary_selected_clip_id {
                    let clip_id = clip_id.clone();
                    if let Some(project) = Some(&mut self.local_project) {
                        let old_bpm = project.timeline.find_clip_by_id(&clip_id)
                            .map(|c| c.recorded_bpm)
                            .unwrap_or(0.0);
                        if (old_bpm - new_bpm).abs() >= 0.01 {
                            let cmd = manifold_editing::commands::clip::ChangeClipRecordedBpmCommand::new(
                                clip_id, old_bpm, new_bpm,
                            );
                            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); self.send_content_cmd(ContentCommand::Execute(boxed)); }
                        }
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::EffectParam(_, _) => {
                // TODO: parse float, clamp to param range, execute ChangeEffectParamCommand
                log::debug!("Effect param text input: {}", text);
            }
            TextInputField::GenParam(_) => {
                // TODO: parse float, clamp to param range, execute ChangeGenParamCommand
                log::debug!("Gen param text input: {}", text);
            }
            TextInputField::GroupRename(_) => {
                // TODO: execute RenameGroupCommand
                log::debug!("Group rename: {}", text);
            }
            TextInputField::SearchFilter => {
                // Update browser popup filter — no undo command
                self.ui_root.browser_popup.set_filter(text.trim().to_string());
                self.needs_rebuild = true;
            }
        }
    }

    // ── Project I/O — delegates to ProjectIOService ────────────────────

    /// Save. Delegates to ProjectIOService.save_project.
    fn save_project(&mut self) {
        let current_time = self.content_state.current_time;
        let current_path = self.current_project_path.clone();
        // Save the local project snapshot (best effort — authoritative is on content thread)
        self.local_project.saved_playhead_time = current_time;
        if let Some(path) = current_path.as_deref() {
            match manifold_io::saver::save_project(&mut self.local_project, path, None, false) {
                Ok(()) => {
                    self.send_content_cmd(ContentCommand::SetProject);
                    log::info!("[ProjectIO] Saved to {}", path.display());
                }
                Err(e) => log::error!("[ProjectIO] Save failed: {e}"),
            }
        } else {
            self.save_project_as();
        }
    }

    /// Save As. Delegates to ProjectIOService.save_project_as.
    fn save_project_as(&mut self) {
        self.send_content_cmd(ContentCommand::PauseRendering);
        let current_time = self.content_state.current_time;
        self.local_project.saved_playhead_time = current_time;
        let action = self.project_io.save_project_as(
            &mut self.local_project,
            current_time,
            &mut EditingService::new(), // placeholder — mark clean via content thread
            &mut self.user_prefs,
        );
        self.send_content_cmd(ContentCommand::ResumeRendering);
        self.apply_project_io_action(action);
    }

    /// Open. Delegates to ProjectIOService.open_project.
    fn open_project(&mut self) {
        self.send_content_cmd(ContentCommand::PauseRendering);
        let action = self.project_io.open_project(&mut self.user_prefs);
        self.send_content_cmd(ContentCommand::ResumeRendering);
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
            // Audio reset sent to content thread
            self.send_content_cmd(ContentCommand::ResetAudio);
            self.ui_root.waveform_lane.clear_audio();
            self.ui_root.layout.waveform_lane_visible = false;
            self.pending_audio_load = None;

            // Apply saved layout before initializing
            self.ui_root.apply_project_layout(&project.settings);
            let saved_time = project.saved_playhead_time;

            // Update local_project BEFORE sending to content thread so UI
            // can rebuild the timeline in this same frame.
            self.local_project = project.clone();
            // Suppress content thread snapshots until it processes the LoadProject
            // command (which will bump data_version above current).
            self.suppress_snapshot_until = self.content_state.data_version + 1;

            self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));

            // Restore playhead position
            if saved_time > 0.0 {
                self.send_content_cmd(ContentCommand::SeekTo(saved_time));
            }

            // Resize compositor + generators to project resolution
            {
                let w = self.local_project.settings.output_width.max(1) as u32;
                let h = self.local_project.settings.output_height.max(1) as u32;
                self.send_content_cmd(ContentCommand::ResizeContent(w, h));
                log::info!("[ProjectIO] GPU resize sent: {}x{}", w, h);
            }

            // Spawn background audio loading (audio decode on background thread,
            // result forwarded to content thread via AudioLoaded command)
            let mut audio_path_for_load: Option<(String, f32)> = None;
            if let Some(ref perc) = self.local_project.percussion_import {
                if let Some(ref audio_path) = perc.audio_path {
                    if !audio_path.is_empty() {
                        audio_path_for_load = Some((audio_path.clone(), perc.audio_start_beat));
                        self.ui_root.layout.waveform_lane_visible = true;
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
                        log::info!("[Audio] decode (background): {:.1}ms", t_audio.elapsed().as_secs_f64() * 1000.0);

                        // Extract waveform PCM from kira's already-decoded frames (no second decode).
                        // Unity does the same: decode once, then AudioClip.GetData() for waveform.
                        let waveform = Some(DecodedAudio::from_static_sound_data(&preloaded.sound_data));

                        let _ = tx.send(PendingAudioLoadResult { preloaded, waveform });
                    })
                    .expect("Failed to spawn audio load thread");
            }

            self.send_content_cmd(ContentCommand::SetProject);
            self.selection.clear_selection();
            self.active_layer_index = Some(0);
            self.needs_rebuild = true;

            // Content thread renders at project FPS; UI always runs at display rate.
            // Don't sync UI frame timer to project FPS — that couples UI to render cadence.

            log::info!("[ProjectIO] load sync: {:.1}ms (audio continues in background)", t_total.elapsed().as_secs_f64() * 1000.0);
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

                // Send loaded audio to content thread
                self.send_content_cmd(ContentCommand::AudioLoaded {
                    preloaded: result.preloaded,
                    waveform: None,
                });

                if let Some(decoded) = result.waveform {
                    self.ui_root.waveform_lane.set_audio_data(
                        &decoded.samples,
                        decoded.channels,
                        decoded.sample_rate,
                    );
                    self.ui_root.layout.waveform_lane_visible = true;
                    log::info!("[Waveform] Decoded audio for waveform display");
                }

                log::info!("[Audio] background load applied to UI thread");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_audio_load = None;
            }
        }
    }

    /// Open a decorated HDR output window (default size = project resolution).
    /// Resizable — content always renders at project resolution with letterbox/pillarbox.
    /// Native title bar allows drag-to-monitor and macOS fullscreen (green button).
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

        // Log all available monitors
        for (i, m) in monitors.iter().enumerate() {
            let s = m.size();
            let p = m.position();
            let sf = m.scale_factor();
            let n = m.name().unwrap_or_else(|| "?".to_string());
            log::debug!(
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

        log::debug!(
            "[OutputWindow] Target '{}': logical={:.0}x{:.0} at ({:.0},{:.0}), physical={}x{}, scale={:.2}",
            mon_name, logical_w, logical_h, logical_x, logical_y,
            mon_phys_size.width, mon_phys_size.height, scale_factor
        );

        // Default size = project resolution. Resizable + native fullscreen supported.
        // Content always renders at project resolution with letterbox/pillarbox to fit.
        let (proj_w, proj_h) = (
            self.local_project.settings.output_width.max(1) as f64,
            self.local_project.settings.output_height.max(1) as f64,
        );
        let center_x = logical_x + (logical_w - proj_w) * 0.5;
        let center_y = logical_y + (logical_h - proj_h) * 0.5;

        let attrs = winit::window::Window::default_attributes()
            .with_title(format!("MANIFOLD - {}", name))
            .with_position(winit::dpi::Position::Logical(
                winit::dpi::LogicalPosition::new(center_x, center_y),
            ))
            .with_inner_size(winit::dpi::LogicalSize::new(proj_w, proj_h));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("[OutputWindow] Failed to create window: {e}");
                return;
            }
        };

        // Window is interactive — user can drag to any monitor and use native fullscreen.

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
            wgpu::PresentMode::AutoNoVsync,
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
        let _dt = self.frame_timer.consume_tick();
        let realtime = self.frame_timer.realtime_since_start();
        self.time_since_start = realtime as f32;

        // Content rendering now runs on dedicated thread — no cadence check needed here.

        // 1. Drain state from content thread
        if let Some(ref rx) = self.state_rx {
            // Drain all pending states, keep the latest
            while let Ok(state) = rx.try_recv() {
                // Accept project snapshot if data_version changed (unless drag in progress)
                if let Some(snapshot) = state.project_snapshot {
                    let drag_active = self.overlay.drag_mode() != manifold_ui::interaction_overlay::DragMode::None;
                    // Suppress snapshots until content thread catches up after a local project load.
                    let suppressed = state.data_version < self.suppress_snapshot_until;

                    // Inspector drags (slider/trim/target/ADSR) are safe to accept
                    // snapshots through — handle_drag() writes the dragged value back
                    // to local_project in the same tick (via dispatch()), so the
                    // snapshot value is immediately overwritten. Accepting snapshots
                    // during inspector drag lets modulation-driven slider animations
                    // continue for non-dragged params.
                    //
                    // Overlay drags (clip move/trim in viewport) write clip positions
                    // directly via the host — those would be overwritten by the
                    // snapshot, so we still suppress for overlay drags.
                    if !drag_active && !suppressed {
                        let version_changed = state.data_version != self.content_state.data_version;
                        self.local_project = *snapshot;
                        // Clear suppression once we've accepted a post-load snapshot
                        self.suppress_snapshot_until = 0;
                        // Only trigger structural sync when data_version changed
                        // (editing commands, undo/redo). Modulation-only snapshots
                        // just update param_values — push_state() syncs sliders
                        // every frame without needing a structural rebuild.
                        if version_changed {
                            self.needs_structural_sync = true;
                            self.needs_rebuild = true;
                        }
                    }
                }
                self.content_state = ContentState {
                    project_snapshot: None, // consumed above
                    ..state
                };
            }
        }

        // 1b. Poll for completed background audio load (waveform stays on UI thread)
        self.poll_pending_audio_load();

        // 1d. Percussion import runs on content thread — read status from content_state.
        let was_importing = false; // previous frame state not tracked here
        let is_importing = self.content_state.percussion_importing;

        // 1e. Sync percussion pipeline status to header panel
        // Port of Unity WorkspaceController.RefreshPercussionImportStatusLabel
        {
            let msg = self.content_state.percussion_status_message.clone();
            let progress = self.content_state.percussion_progress;
            let show = self.content_state.percussion_show_progress && !msg.is_empty();
            self.ui_root.header.set_import_status(
                &mut self.ui_root.tree,
                &msg,
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

        // 1f. Sync stem mute/solo state from content thread to UI panels.
        // Port of Unity: WorkspaceController.OnStemMuteToggled/OnStemSoloToggled refreshing button visuals.
        {
            for i in 0..manifold_playback::stem_audio::STEM_COUNT {
                self.ui_root.stem_lanes.set_mute_state(i, self.content_state.stem_muted[i]);
                self.ui_root.stem_lanes.set_solo_state(i, self.content_state.stem_soloed[i]);
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
                let content_tx = self.content_tx.as_ref().unwrap();
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.local_project,
                    content_tx,
                    &self.content_state,
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
                PanelAction::BrowserSearchClicked => {
                    let r = self.ui_root.browser_popup.search_bar_rect(&self.ui_root.tree);
                    self.text_input.begin(
                        crate::text_input::TextInputField::SearchFilter,
                        &self.ui_root.browser_popup.current_filter,
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                PanelAction::BpmFieldClicked => {
                    let bpm = Some(&self.local_project).map_or(120.0, |p| p.settings.bpm);
                    let r = self.ui_root.tree.get_bounds(
                        self.ui_root.transport.bpm_field_id() as u32,
                    );
                    self.text_input.begin(
                        crate::text_input::TextInputField::Bpm,
                        &format!("{:.1}", bpm),
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        14.0,
                    );
                    continue;
                }
                PanelAction::FpsFieldClicked => {
                    let fps = Some(&self.local_project).map_or(60.0, |p| p.settings.frame_rate);
                    let r = self.ui_root.tree.get_bounds(
                        self.ui_root.footer.fps_field_id() as u32,
                    );
                    self.text_input.begin(
                        crate::text_input::TextInputField::Fps,
                        &format!("{:.0}", fps),
                        crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                        11.0,
                    );
                    continue;
                }
                PanelAction::LayerDoubleClicked(idx) => {
                    // Open text input for layer rename
                    {
                        let project = &self.local_project;
                        if let Some(layer) = project.timeline.layers.get(*idx) {
                            let nid = self.ui_root.layer_headers.name_node_id(*idx);
                            let r = if nid >= 0 {
                                self.ui_root.tree.get_bounds(nid as u32)
                            } else {
                                manifold_ui::node::Rect::new(100.0, 100.0, 120.0, 20.0)
                            };
                            self.text_input.begin(
                                crate::text_input::TextInputField::LayerName(*idx),
                                &layer.name,
                                crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                                11.0,
                            );
                        }
                    }
                    continue;
                }
                PanelAction::ClipBpmClicked => {
                    // Open text input for clip recorded BPM editing.
                    // Unity: ClipInspector.OnBitmapBpmClicked → BitmapTextInput.BeginEdit
                    if let Some(clip_id) = &self.selection.primary_selected_clip_id {
                        let bpm_text = Some(&self.local_project)
                            .and_then(|p| {
                                p.timeline.layers.iter()
                                    .flat_map(|l| l.clips.iter())
                                    .find(|c| c.id == *clip_id)
                            })
                            .map(|c| {
                                if c.recorded_bpm > 0.0 {
                                    format!("{:.1}", c.recorded_bpm)
                                } else {
                                    "Auto".to_string()
                                }
                            })
                            .unwrap_or_else(|| "Auto".to_string());
                        let r = self.ui_root.inspector.clip_chrome_mut()
                            .bpm_button_rect(&self.ui_root.tree);
                        self.text_input.begin(
                            crate::text_input::TextInputField::ClipBpm,
                            &bpm_text,
                            crate::text_input::AnchorRect::new(r.x, r.y, r.width, r.height),
                            10.0,
                        );
                    }
                    continue;
                }
                PanelAction::NewProject => {
                    let project = Self::create_default_project();
                    self.local_project = project.clone();
                    self.suppress_snapshot_until = self.content_state.data_version + 1;
                    self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));
                    self.send_content_cmd(ContentCommand::SetProject);
                    self.selection.clear_selection();
                    self.active_layer_index = Some(0);
                    self.current_project_path = None;
                    needs_structural_sync = true;
                    continue;
                }
                // Transport controller actions — intercept here for Application-level access
                PanelAction::CycleClockAuthority => {
                    self.send_content_cmd(ContentCommand::CycleClockAuthority);
                    continue;
                }
                PanelAction::ToggleLink => {
                    self.send_content_cmd(ContentCommand::ToggleLink);
                    continue;
                }
                PanelAction::ToggleMidiClock => {
                    self.send_content_cmd(ContentCommand::ToggleMidiClock);
                    continue;
                }
                PanelAction::ToggleSyncOutput => {
                    self.send_content_cmd(ContentCommand::ToggleSyncOutput);
                    continue;
                }
                PanelAction::ResetBpm => {
                    self.send_content_cmd(ContentCommand::ResetBpm);
                    self.needs_rebuild = true;
                    continue;
                }
                _ => {}
            }
            let content_tx = self.content_tx.as_ref().unwrap();
            let result = crate::ui_bridge::dispatch(
                action,
                &mut self.local_project,
                content_tx,
                &self.content_state,
                &mut self.ui_root,
                &mut self.selection,
                &mut self.active_layer_index,
                &mut self.slider_snapshot,
                &mut self.trim_snapshot,
                &mut self.adsr_snapshot,
                &mut self.target_snapshot,
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
            let dims = Some(&self.local_project).map(|p| {
                (p.settings.output_width.max(1) as u32, p.settings.output_height.max(1) as u32)
            });
            if let Some((w, h)) = dims {
                self.send_content_cmd(ContentCommand::ResizeContent(w, h));
                log::info!("Resolution changed to {}x{}", w, h);
            }
        }

        // Selection version change → sync inspector so it shows the newly selected clip
        if self.selection.selection_version != prev_sel_version && !needs_structural_sync {
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.local_project, self.active_layer_index, &self.selection);
            needs_structural_sync = true;
        }

        if needs_structural_sync {
            crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.local_project, self.active_layer_index);
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.local_project, self.active_layer_index, &self.selection);
        } else if self.active_layer_index != prev_active_layer {
            crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.local_project, self.active_layer_index, &self.selection);
            needs_structural_sync = true; // Inspector content changed — needs rebuild
        }
        // 2a. Per-frame drag polling with auto-scroll.
        // InteractionOverlay.PollMoveDrag — continues edge auto-scroll when mouse is stationary.
        {
            use manifold_ui::interaction_overlay::DragMode;
            if self.overlay.drag_mode() == DragMode::Move {
                let content_tx = self.content_tx.as_ref().unwrap();
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.local_project,
                    content_tx,
                    &self.content_state,
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
        let auto_scroll_changed = crate::ui_bridge::check_auto_scroll(&mut self.ui_root, &self.content_state, &self.local_project);
        let overlay_changed = self.ui_root.overlay_dirty;
        self.ui_root.overlay_dirty = false;
        let scroll_changed = auto_scroll_changed || self.needs_scroll_rebuild || overlay_changed;
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
                // Re-apply effect card selection visuals after rebuild —
                // structural changes recreate cards with is_selected=false.
                self.ui_root.inspector.apply_selection_visuals(&mut self.ui_root.tree);
            }
        } else if scroll_changed {
            self.ui_root.rebuild_scroll_panels();
        }

        // 4. Push engine state to UI panels (AFTER build so new nodes get state)
        crate::ui_bridge::push_state(
            &mut self.ui_root,
            &self.local_project,
            &self.content_state,
            self.active_layer_index,
            &self.selection,
            self.content_state.editing_is_dirty,
            self.current_project_path.as_deref(),
        );

        // 4b. Sync clip positions from live project model every frame.
        // During drag, the InteractionOverlay mutates clip data directly in the
        // project model, but the viewport's clips_by_layer cache is only refreshed
        // via sync_project_data() (structural sync). This per-frame sync ensures
        // bitmap renderers see mutated clip positions and repaint during drag.
        // Cost: iterates layers+clips, but the bitmap fingerprint skips repaint
        // when nothing changed (cheap no-op outside of drag).
        crate::ui_bridge::sync_clip_positions(&mut self.ui_root, &self.local_project);

        // 5. Push performance metrics to HUD
        if self.ui_root.perf_hud.is_visible() {
            let bpm = Some(&self.local_project).map(|p| p.settings.bpm).unwrap_or(120.0);
            let clock_source = Some(&self.local_project)
                .map(|p| p.settings.clock_authority.display_name().to_string())
                .unwrap_or_else(|| "Internal".to_string());
            self.ui_root.perf_hud.set_metrics(manifold_ui::panels::perf_hud::PerfMetrics {
                ui_fps: self.frame_timer.current_fps() as f32,
                ui_frame_time_ms: (self.frame_timer.last_dt() * 1000.0) as f32,
                render_fps: self.content_state.content_fps,
                render_frame_time_ms: self.content_state.content_frame_time_ms,
                active_clips: 0, // TODO: wire from tick_result
                preparing_clips: 0,
                current_beat: self.content_state.current_beat,
                current_time_secs: self.content_state.current_time,
                bpm,
                clock_source,
                is_playing: self.content_state.is_playing,
                data_version: self.content_state.data_version,
            });
        }

        // 6. Lightweight update (playhead, insert cursor, layer selection, HUD values)
        self.ui_root.update();

        // 6a. Update waveform lane overlay (position + playhead for dirty-checking)
        {
            let playhead_beat = self.content_state.current_beat;
            let scroll_x = self.ui_root.viewport.scroll_x_beats() * self.ui_root.viewport.pixels_per_beat();
            let wf = &mut self.ui_root.waveform_lane;
            if wf.is_ready() {
                // Get start beat and duration from project percussion import state
                let (start_beat, duration_beats) = if let Some(proj) = Some(&self.local_project) {
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

            // 6a-ii. Update stem lane overlay (same position/scroll as master).
            if self.ui_root.stem_lanes.is_expanded() {
                let start_beat = self.local_project.percussion_import
                    .as_ref()
                    .map_or(0.0, |perc| perc.audio_start_beat);
                let bpm = self.local_project.settings.bpm.max(1.0);
                let mapper = self.ui_root.viewport.mapper();
                self.ui_root.stem_lanes.update_overlay(
                    start_beat,
                    playhead_beat,
                    scroll_x,
                    bpm,
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

            // 6e. Repaint + upload stem lanes if dirty
            if self.ui_root.stem_lanes.is_expanded() {
                let sl_rect = self.ui_root.viewport.stem_lanes_rect();
                if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                    let sl = &mut self.ui_root.stem_lanes;
                    let mapper = self.ui_root.viewport.mapper();
                    if sl.buffer_width != sl_rect.width as usize {
                        sl.dirty = true;
                    }
                    if sl.dirty {
                        sl.repaint(sl_rect.width as usize, mapper);
                        if sl.buffer_width > 0 && sl.buffer_height > 0 && !sl.pixel_buffer.is_empty() {
                            bitmap_gpu.upload_layer(
                                &gpu.device, &gpu.queue,
                                1001, &sl.pixel_buffer,
                                sl.buffer_width as u32, sl.buffer_height as u32,
                            );
                        }
                    }
                }
            }
        }

        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        // Present using IOSurface shared texture (dual device, zero GPU copy).
        // The content thread writes to the IOSurface-backed texture on its device;
        // the UI device reads the same GPU memory via its own imported texture.
        #[cfg(target_os = "macos")]
        {
            // Detect bridge resize (generation changed) and re-import UI texture.
            if let Some(ref bridge) = self.shared_texture_bridge {
                let gen = bridge.generation();
                if gen != self.last_bridge_generation {
                    self.last_bridge_generation = gen;
                    let ui_tex = unsafe { bridge.import_texture(&gpu.device) };
                    self.ui_shared_view = Some(ui_tex.create_view(&wgpu::TextureViewDescriptor::default()));
                    self.ui_shared_texture = Some(ui_tex);
                    log::info!("[UI] re-imported IOSurface texture after resize (gen={})", gen);
                }
            }
            let view = self.ui_shared_view.clone();
            if let Some(ref v) = view {
                self.present_all_windows(v);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Fallback: single-device SharedOutputView (non-macOS)
            let compositor_view = self.content_pipeline_output.as_ref()
                .and_then(|shared| shared.get_view());
            if let Some(ref view) = compositor_view {
                self.present_all_windows(view);
            }
        }

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
        let (comp_w, comp_h) = self.content_pipeline_output.as_ref()
            .map(|p| p.get_dimensions())
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
                // Output windows: project resolution centered with letterbox/pillarbox.
                // Clear to black first (bars around content when window != project aspect).
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
                // Skip overlay nodes — perf HUD renders after bitmaps (Pass 3b),
                // popups render in Pass 4. Perf HUD comes first in tree order, so
                // skipping from its first_node also skips popup nodes (correct).
                // Uses TextMode::Main so base UI text goes to the main TextRenderer's
                // own vertex buffer, isolated from the overlay TextRenderer.
                if let Some(ui) = &mut self.ui_renderer {
                    let skip_from = if self.ui_root.perf_hud.is_visible() {
                        Some(self.ui_root.perf_hud.first_node())
                    } else if self.ui_root.dropdown.is_open() {
                        Some(self.ui_root.dropdown.first_node())
                    } else if self.ui_root.browser_popup.is_open() {
                        Some(self.ui_root.browser_popup.first_node())
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

                    // Add stem lanes rect (texture at reserved index 1001)
                    if self.ui_root.stem_lanes.is_expanded() {
                        let sl_rect = self.ui_root.viewport.stem_lanes_rect();
                        if sl_rect.width > 0.0 && sl_rect.height > 0.0 {
                            rects.push((1001, sl_rect));
                        }
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

                // Pass 3b: Perf HUD — renders on top of bitmaps and playhead.
                // Uses its own overlay pass so it's not covered by layer textures.
                if self.ui_root.perf_hud.is_visible() {
                    if let Some(ui) = &mut self.ui_renderer {
                        // Render only perf HUD nodes (from first_node up to dropdown/browser start)
                        let hud_start = self.ui_root.perf_hud.first_node();
                        let hud_end = if self.ui_root.dropdown.is_open() {
                            self.ui_root.dropdown.first_node()
                        } else if self.ui_root.browser_popup.is_open() {
                            self.ui_root.browser_popup.first_node()
                        } else {
                            usize::MAX
                        };
                        ui.render_overlay_range(&self.ui_root.tree, hud_start, hud_end);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }
                }

                // Pass 4: Overlay popups — render ON TOP of layer bitmaps and playhead.
                // Uses TextMode::Overlay so popup text goes to a separate TextRenderer
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
                } else if self.ui_root.browser_popup.is_open() {
                    if let Some(ui) = &mut self.ui_renderer {
                        let start = self.ui_root.browser_popup.first_node();
                        ui.render_overlay(&self.ui_root.tree, start);
                        ui.render(
                            &gpu.device, &gpu.queue, &mut encoder, &surface_view,
                            logical_w, logical_h, scale, TextMode::Overlay,
                        );
                    }
                }

                // Pass 5: Text input overlay — renders on top of everything.
                // Uses immediate-mode draw_rect + draw_text (no UITree nodes needed).
                if self.text_input.active {
                    if let Some(ui) = &mut self.ui_renderer {
                        render_text_input_overlay(&self.text_input, &self.frame_timer, ui);
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

            GpuContext {
                instance,
                adapter,
                device,
                queue,
            }
        };

        // Spawn content thread with its OWN GPU device (separate queue for isolation).
        // Compositor output is shared via IOSurface — zero copy, GPU-to-GPU.
        {
            let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<ContentCommand>(64);
            let (state_tx, state_rx) = crossbeam_channel::bounded::<ContentState>(4);

            // Create a secondary GPU device for the content thread.
            // Same adapter, independent queue — heavy compute can't block UI rendering.
            let content_gpu = pollster::block_on(gpu.create_secondary_device("Content Device"));

            let output_w = self.local_project.settings.output_width.max(1) as u32;
            let output_h = self.local_project.settings.output_height.max(1) as u32;
            let compositor_format = wgpu::TextureFormat::Rgba16Float;

            // Create IOSurface bridge for cross-device texture sharing.
            // Both devices get their own MTLTexture backed by the same IOSurface memory.
            #[cfg(target_os = "macos")]
            {
                let bridge = crate::shared_texture::SharedTextureBridge::new(
                    output_w, output_h,
                );
                let bridge = Arc::new(bridge);
                // Import the IOSurface texture on the UI device
                let ui_tex = unsafe { bridge.import_texture(&gpu.device) };
                self.ui_shared_view = Some(ui_tex.create_view(&wgpu::TextureViewDescriptor::default()));
                self.ui_shared_texture = Some(ui_tex);
                self.shared_texture_bridge = Some(Arc::clone(&bridge));
            }

            let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
                Box::new(StubRenderer::new_video()),
                Box::new(GeneratorRenderer::new(
                    Arc::clone(&content_gpu.device),
                    output_w,
                    output_h,
                    compositor_format,
                    8,
                )),
            ];
            let mut engine = PlaybackEngine::new(renderers);
            engine.initialize(self.local_project.clone());

            let mut content_pipeline = crate::content_pipeline::ContentPipeline::new(
                Box::new(LayerCompositor::new(&content_gpu.device, &content_gpu.queue, output_w, output_h)),
            );
            // Give the content pipeline the IOSurface bridge so it can copy output + signal.
            #[cfg(target_os = "macos")]
            if let Some(ref bridge) = self.shared_texture_bridge {
                let content_tex = unsafe { bridge.import_texture(&*content_gpu.device) };
                content_pipeline.set_shared_texture(content_tex, Arc::clone(bridge));
            }
            self.content_pipeline_output = Some(content_pipeline.shared_output());

            let audio_sync = match ImportedAudioSyncController::new() {
                Ok(ctrl) => Some(ctrl),
                Err(e) => {
                    log::warn!("[Audio] Failed to initialize audio sync: {}", e);
                    None
                }
            };

            let stem_audio = match manifold_playback::stem_audio::StemAudioController::new() {
                Ok(ctrl) => Some(ctrl),
                Err(e) => {
                    log::warn!("[StemAudio] Failed to initialize stem audio controller: {}", e);
                    None
                }
            };

            let mut midi_input = manifold_playback::midi_input::MidiInputController::new();
            midi_input.start();

            let content_thread = crate::content_thread::ContentThread {
                engine,
                editing_service: EditingService::new(),
                content_pipeline,
                audio_sync,
                stem_audio,
                percussion_orchestrator: PercussionImportOrchestrator::new(
                    None,
                    std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
                        .unwrap_or_default(),
                ),
                transport_controller: manifold_playback::transport_controller::TransportController::new(),
                gpu: GpuContext {
                    instance: gpu.instance.clone(),
                    adapter: gpu.adapter.clone(),
                    device: content_gpu.device,
                    queue: content_gpu.queue,
                },
                frame_count: 0,
                time_since_start: 0.0,
                last_data_version: 0,
                midi_input,
                clip_launcher: manifold_playback::clip_launcher::ClipLauncher::new(),
                rendering_paused: false,
                timer: crate::frame_timer::FrameTimer::new(
                    self.local_project.settings.frame_rate as f64,
                ),
                sync_arbiter: manifold_playback::sync::SyncArbiter::new(),
                osc_receiver: manifold_playback::osc_receiver::OscReceiver::new(),
                osc_sync: manifold_playback::osc_sync::OscSyncController::new(),
                osc_sender: manifold_playback::osc_sender::OscPositionSender::new(),
            };

            let handle = std::thread::Builder::new()
                .name("content-thread".into())
                .spawn(move || {
                    content_thread.run(cmd_rx, state_tx);
                })
                .expect("Failed to spawn content thread");

            self.content_tx = Some(cmd_tx);
            self.state_rx = Some(state_rx);
            self.content_thread_handle = Some(handle);
            log::info!("[ContentThread] spawned (dual device + IOSurface bridge)");
        }

        self.gpu = Some(gpu);

        // Pass detected display resolutions to UI
        self.ui_root.set_display_resolutions(self.display_resolutions.clone());

        // Build UI at initial window size (logical pixels)
        let logical_w = size.width as f32 / scale as f32;
        let logical_h = size.height as f32 / scale as f32;
        self.ui_root.resize(logical_w, logical_h);

        // Push initial project data (layers, tracks) and rebuild
        crate::ui_bridge::sync_project_data(&mut self.ui_root, &self.local_project, self.active_layer_index);
        crate::ui_bridge::sync_inspector_data(&mut self.ui_root, &self.local_project, self.active_layer_index, &self.selection);

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
                    // Shut down content thread
                    if let Some(tx) = self.content_tx.take() {
                        let _ = tx.send(ContentCommand::Shutdown);
                    }
                    if let Some(handle) = self.content_thread_handle.take() {
                        let _ = handle.join();
                        log::info!("[ContentThread] joined");
                    }
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
                        if let Some(content_tx) = self.content_tx.as_ref() {
                            let content_tx = content_tx;
                            let mut host = crate::editing_host::AppEditingHost::new(
                                &mut self.local_project,
                                content_tx,
                                &self.content_state,
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
                                        if let Some(project) = Some(&mut self.local_project) {
                                            project.settings.timeline_height_percent =
                                                self.ui_root.layout.timeline_split_ratio;
                                        }
                                    } else if self.ui_root.inspector_resize_dragging {
                                        // Persist to ProjectSettings (Unity WorkspaceController line 528)
                                        if let Some(project) = Some(&mut self.local_project) {
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
                let data_version_before = self.content_state.data_version;
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
                                // Cmd+A / Ctrl+A → select all
                                if c == "a" && self.modifiers.command {
                                    self.text_input.select_all_text();
                                } else {
                                    for ch in c.chars() {
                                        self.text_input.insert_char(ch);
                                    }
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
                            project: &mut self.local_project,
                            content_tx: self.content_tx.as_ref().unwrap(),
                            content_state: &self.content_state,
                            ui_root: &mut self.ui_root,
                            selection: &mut self.selection,
                            active_layer: &mut self.active_layer_index,
                            needs_rebuild: &mut self.needs_rebuild,
                            needs_structural_sync: &mut self.needs_structural_sync,
                            needs_scroll_rebuild: &mut self.needs_scroll_rebuild,
                            current_project_path: &self.current_project_path,
                            has_output_window: self.window_registry.has_output_window(),
                            pending_close_output: &mut self.pending_close_output,
                            effect_clipboard: &mut self.effect_clipboard,
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
                            self.local_project = project.clone();
                            self.suppress_snapshot_until = self.content_state.data_version + 1;
                            self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));
                            self.send_content_cmd(ContentCommand::SetProject);
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
                if self.content_state.data_version != data_version_before {
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
                    let drop_beat = self.content_state.current_beat;
                    let drop_layer = self.active_layer_index.unwrap_or(0) as i32;
                    let spb = manifold_core::tempo::TempoMapConverter::seconds_per_beat_from_bpm(
                        Some(&self.local_project).map(|p| p.settings.bpm).unwrap_or(120.0),
                    );
                    if let Some(project) = Some(&mut self.local_project) {
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

// ── Text input overlay rendering (free function to avoid borrow conflicts) ──

/// Render the text input overlay using immediate-mode draw calls.
fn render_text_input_overlay(
    ti: &crate::text_input::TextInputState,
    timer: &crate::frame_timer::FrameTimer,
    ui: &mut UIRenderer,
) {
    use crate::text_input::*;

    let a = &ti.anchor;
    let fs = ti.font_size;
    let pad_h = TEXT_INPUT_PAD_H;
    let pad_v = TEXT_INPUT_PAD_V;

    let bg_x = a.x;
    let bg_y = a.y;
    let bg_w = a.width.max(40.0);
    let bg_h = a.height.max(fs + pad_v * 2.0);

    ui.draw_bordered_rect(
        bg_x, bg_y, bg_w, bg_h,
        TEXT_INPUT_BG,
        3.0,
        1.0,
        [0.35, 0.45, 0.7, 0.8],
    );

    // Selection highlight (when select_all)
    if ti.select_all && !ti.text.is_empty() {
        let text_w = ti.text.len() as f32 * fs * 0.6;
        ui.draw_rect(
            bg_x + pad_h, bg_y + pad_v,
            text_w.min(bg_w - pad_h * 2.0), bg_h - pad_v * 2.0,
            TEXT_INPUT_SELECT_BG,
        );
    }

    // Text
    let text_x = bg_x + pad_h;
    let text_y = bg_y + pad_v;
    ui.draw_text(text_x, text_y, &ti.text, fs, TEXT_INPUT_FG);

    // Blinking cursor
    if !ti.select_all {
        let elapsed = timer.realtime_since_start();
        let blink_on = (elapsed / TEXT_INPUT_BLINK_PERIOD) as u64 % 2 == 0;
        if blink_on {
            let chars_before = ti.text[..ti.cursor].chars().count();
            let cursor_x = text_x + chars_before as f32 * fs * 0.6;
            ui.draw_rect(
                cursor_x, bg_y + pad_v,
                TEXT_INPUT_CURSOR_W, bg_h - pad_v * 2.0,
                TEXT_INPUT_CURSOR,
            );
        }
    }
}
