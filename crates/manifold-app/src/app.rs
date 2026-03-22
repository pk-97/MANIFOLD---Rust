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
use manifold_renderer::ui_renderer::UIRenderer;

use manifold_ui::cursors::{CursorManager, TimelineCursor};
use manifold_ui::input::{Modifiers, PointerAction};
use manifold_ui::node::Vec2;
use manifold_ui::ui_state::UIState;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::frame_timer::FrameTimer;
use crate::project_io::ProjectIOService;
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
    pub(crate) gpu: Option<GpuContext>,

    // Windows
    pub(crate) window_registry: WindowRegistry,
    pub(crate) primary_window_id: Option<WindowId>,

    // Content thread communication
    pub(crate) content_tx: Option<crossbeam_channel::Sender<ContentCommand>>,
    pub(crate) state_rx: Option<crossbeam_channel::Receiver<ContentState>>,
    pub(crate) content_thread_handle: Option<std::thread::JoinHandle<()>>,

    /// Latest state snapshot from the content thread.
    pub(crate) content_state: ContentState,

    /// Local project snapshot for UI reads. Updated from content thread
    /// when data_version changes. During drag, snapshots are deferred.
    pub(crate) local_project: Project,

    /// After a local project load (open/new), suppress content thread snapshots
    /// until its data_version exceeds this value. Prevents the old project from
    /// overwriting the locally-loaded new project before the content thread
    /// processes the LoadProject command.
    pub(crate) suppress_snapshot_until: u64,

    // Selection
    pub(crate) selection: SelectionState,
    pub(crate) active_layer_index: Option<usize>,
    /// Slider drag snapshot for undo (opacity, slip, etc.). Stores the old value
    /// on snapshot, committed on release. NOT related to clip drag state.
    pub(crate) slider_snapshot: Option<f32>,
    /// Trim drag snapshot (min, max) for undo. Unity: onTrimSnapshot/onTrimCommit.
    pub(crate) trim_snapshot: Option<(f32, f32)>,
    /// ADSR drag snapshot (attack, decay, sustain, release) for undo.
    pub(crate) adsr_snapshot: Option<(f32, f32, f32, f32)>,
    /// Envelope target drag snapshot for undo.
    pub(crate) target_snapshot: Option<f32>,

    // Effect clipboard (Unity: static EffectClipboard singleton, Rust: instance)
    pub(crate) effect_clipboard: manifold_editing::clipboard::EffectClipboard,

    // Rendering
    /// Shared reference to the content pipeline's output dimensions.
    pub(crate) content_pipeline_output: Option<Arc<crate::content_pipeline::SharedOutputView>>,
    /// IOSurface bridge for cross-device texture sharing (macOS).
    /// Content device writes compositor output to the IOSurface; UI device reads it.
    #[cfg(target_os = "macos")]
    pub(crate) shared_texture_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// UI-side wgpu::Texture imported from the IOSurface. Same GPU memory as
    /// the content-side texture — zero copy.
    #[cfg(target_os = "macos")]
    pub(crate) ui_shared_texture: Option<wgpu::Texture>,
    #[cfg(target_os = "macos")]
    pub(crate) ui_shared_view: Option<wgpu::TextureView>,
    /// Last seen bridge generation — detects resize (not per-frame).
    #[cfg(target_os = "macos")]
    pub(crate) last_bridge_generation: u64,
    pub(crate) blit_pipeline: Option<BlitPipeline>,
    pub(crate) output_blit_pipeline: Option<BlitPipeline>,
    pub(crate) output_blit_format: Option<wgpu::TextureFormat>,
    pub(crate) ui_renderer: Option<UIRenderer>,
    pub(crate) layer_bitmap_gpu: Option<manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu>,
    pub(crate) surface_format: wgpu::TextureFormat,

    // UI
    pub(crate) ui_root: UIRoot,

    // Frame timing
    pub(crate) frame_timer: FrameTimer,
    pub(crate) frame_count: u64,

    // Input state for winit → UIInputSystem translation
    pub(crate) cursor_pos: Vec2,
    pub(crate) mouse_pressed: bool,
    pub(crate) modifiers: Modifiers,
    pub(crate) time_since_start: f32,

    // Cursor feedback — tracks current cursor shape for interaction hints.
    // From Unity Cursors.cs: SetMove, SetBlocked, SetResizeHorizontal, SetDefault.
    pub(crate) cursor_manager: CursorManager,

    // Video/timeline split handle drag state.
    // From Unity PanelResizeHandle.cs — drag to resize video vs timeline proportion.
    pub(crate) split_dragging: bool,
    pub(crate) split_was_hovered: bool,

    // File I/O
    pub(crate) current_project_path: Option<std::path::PathBuf>,
    pub(crate) user_prefs: UserPrefs,
    pub(crate) project_io: ProjectIOService,

    // Text input
    pub(crate) text_input: crate::text_input::TextInputState,

    // Pending audio load — receives results from background decode thread.
    // Unity loads audio async via coroutines; we use std::thread + mpsc channel.
    // Waveform data stays on UI thread; preloaded audio data is forwarded to content thread.
    pub(crate) pending_audio_load: Option<std::sync::mpsc::Receiver<PendingAudioLoadResult>>,

    // Keyboard/zoom handler — port of Unity InputHandler.cs
    // Owns inspector_has_focus (panel focus for context-sensitive routing).
    pub(crate) input_handler: crate::input_handler::InputHandler,

    // Interaction overlay — port of Unity InteractionOverlay.cs
    // Owns all drag state. Lives on Application (not UIRoot) so we can
    // split-borrow it alongside ui_root.viewport and create AppEditingHost.
    pub(crate) overlay: manifold_ui::interaction_overlay::InteractionOverlay,

    // Pre-drag split commands — persists between AppEditingHost instances.
    // Unity stores these on InteractionOverlay; Rust stores them here because
    // the overlay can't depend on manifold-editing Command types.
    // Populated by split_clips_for_region_move, prepended on commit_command_batch.
    pub(crate) pre_drag_commands: Vec<Box<dyn manifold_editing::command::Command>>,

    // Detected display resolutions: (width, height, label).
    // Populated from winit monitors at startup. Matches Unity Footer.CollectDisplayResolutions.
    pub(crate) display_resolutions: Vec<(u32, u32, String)>,

    // State
    pub(crate) initialized: bool,
    pub(crate) pending_toggle_output: bool,
    pub(crate) pending_close_output: bool,
    pub(crate) needs_rebuild: bool,
    /// Set by scroll/zoom events that only affect viewport + layer_headers.
    /// Uses the partial rebuild path (rebuild_scroll_panels) instead of full build.
    pub(crate) needs_scroll_rebuild: bool,
    /// Set by keyboard shortcuts that mutate project data (undo, delete, etc.).
    /// Consumed by tick_and_render to trigger sync_project_data + rebuild.
    pub(crate) needs_structural_sync: bool,
    /// Last data_version seen from content thread. When content_state.data_version
    /// is newer, accept the project snapshot (unless drag is in progress).
    #[allow(dead_code)]
    pub(crate) last_accepted_data_version: u64,
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
    pub(crate) fn send_content_cmd(&self, cmd: ContentCommand) {
        if let Some(ref tx) = self.content_tx {
            let _ = tx.try_send(cmd);
        }
    }

    pub(crate) fn create_default_project() -> Project {
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
        if tracks_rect.contains(self.cursor_pos)
            && let Some(hit) = self.ui_root.viewport.hit_test_clip(self.cursor_pos) {
                match hit.region {
                    manifold_ui::panels::HitRegion::TrimLeft | manifold_ui::panels::HitRegion::TrimRight => {
                        self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
                        return;
                    }
                    _ => {}
                }
            }

        // Default: standard arrow
        self.cursor_manager.set_default();
    }

    #[allow(dead_code)]
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
                if let Some(layer) = self.local_project.timeline.layers.get(idx) {
                    let old_name = layer.name.clone();
                    let new_name = text.to_string();
                    if old_name != new_name {
                        let cmd = manifold_editing::commands::layer::RenameLayerCommand::new(
                            idx, old_name, new_name,
                        );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
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
            TextInputField::EffectParam(effect_idx, param_idx) => {
                if let Ok(parsed) = text.parse::<f32>() {
                    let tab = self.ui_root.inspector.last_effect_tab();
                    // Resolve effect instance to get type + old value
                    let effect_info = match tab {
                        manifold_ui::InspectorTab::Master => {
                            self.local_project.settings.master_effects.get(effect_idx)
                                .map(|fx| (fx.effect_type, fx.get_base_param(param_idx)))
                        }
                        manifold_ui::InspectorTab::Layer => {
                            self.active_layer_index
                                .and_then(|li| self.local_project.timeline.layers.get(li))
                                .and_then(|l| l.effects.as_ref())
                                .and_then(|e| e.get(effect_idx))
                                .map(|fx| (fx.effect_type, fx.get_base_param(param_idx)))
                        }
                        manifold_ui::InspectorTab::Clip => {
                            self.selection.primary_selected_clip_id.as_ref()
                                .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                                .and_then(|c| c.effects.get(effect_idx))
                                .map(|fx| (fx.effect_type, fx.get_base_param(param_idx)))
                        }
                    };
                    if let Some((effect_type, old_val)) = effect_info {
                        // Clamp to param range from registry
                        let new_val = if let Some(def) = manifold_core::effect_definition_registry::try_get(effect_type) {
                            if let Some(pd) = def.param_defs.get(param_idx) {
                                parsed.clamp(pd.min, pd.max)
                            } else { parsed }
                        } else { parsed };
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let target = match tab {
                                manifold_ui::InspectorTab::Master => manifold_editing::commands::effect_target::EffectTarget::Master,
                                manifold_ui::InspectorTab::Layer => manifold_editing::commands::effect_target::EffectTarget::Layer {
                                    layer_index: self.active_layer_index.unwrap_or(0),
                                },
                                manifold_ui::InspectorTab::Clip => {
                                    if let Some(cid) = self.selection.primary_selected_clip_id.clone() {
                                        manifold_editing::commands::effect_target::EffectTarget::Clip { clip_id: cid }
                                    } else {
                                        manifold_editing::commands::effect_target::EffectTarget::Layer {
                                            layer_index: self.active_layer_index.unwrap_or(0),
                                        }
                                    }
                                }
                            };
                            let cmd = manifold_editing::commands::effects::ChangeEffectParamCommand::new(
                                target, effect_idx, param_idx, old_val, new_val,
                            );
                            if let Some(project) = Some(&mut self.local_project) {
                                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                                boxed.execute(project);
                                self.send_content_cmd(ContentCommand::Execute(boxed));
                            }
                        }
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::GenParam(param_idx) => {
                if let Ok(parsed) = text.parse::<f32>()
                    && let Some(layer_idx) = self.active_layer_index
                        && let Some(layer) = self.local_project.timeline.layers.get(layer_idx) {
                            let gen_type = layer.generator_type();
                            // Clamp to param range from generator registry
                            let new_val = if let Some(def) = manifold_core::generator_definition_registry::try_get(gen_type) {
                                if let Some(pd) = def.param_defs.get(param_idx) {
                                    parsed.clamp(pd.min, pd.max)
                                } else { parsed }
                            } else { parsed };
                            if let Some(gp) = &layer.gen_params {
                                let base = gp.base_param_values.as_ref().unwrap_or(&gp.param_values);
                                let old_val = base.get(param_idx).copied().unwrap_or(0.0);
                                if (old_val - new_val).abs() > f32::EPSILON {
                                    let mut old_params = base.clone();
                                    let mut new_params = base.clone();
                                    if param_idx < new_params.len() {
                                        new_params[param_idx] = new_val;
                                    }
                                    if param_idx < old_params.len() {
                                        old_params[param_idx] = old_val;
                                    }
                                    let cmd = manifold_editing::commands::settings::ChangeGeneratorParamsCommand::new(
                                        layer_idx, old_params, new_params,
                                    );
                                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                                    boxed.execute(&mut self.local_project);
                                    self.send_content_cmd(ContentCommand::Execute(boxed));
                                }
                            }
                        }
                self.needs_rebuild = true;
            }
            TextInputField::GroupRename(group_idx) => {
                let new_name = text.trim().to_string();
                if !new_name.is_empty() {
                    let tab = self.ui_root.inspector.last_effect_tab();
                    // Find the group by index
                    let group_info = match tab {
                        manifold_ui::InspectorTab::Master => {
                            self.local_project.settings.master_effect_groups.as_ref()
                                .and_then(|groups| groups.get(group_idx))
                                .map(|g| (g.id.clone(), g.name.clone()))
                        }
                        manifold_ui::InspectorTab::Layer => {
                            self.active_layer_index
                                .and_then(|li| self.local_project.timeline.layers.get(li))
                                .and_then(|l| l.effect_groups.as_ref())
                                .and_then(|g| g.get(group_idx))
                                .map(|g| (g.id.clone(), g.name.clone()))
                        }
                        manifold_ui::InspectorTab::Clip => {
                            self.selection.primary_selected_clip_id.as_ref()
                                .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                                .and_then(|c| c.effect_groups.as_ref())
                                .and_then(|g| g.get(group_idx))
                                .map(|g| (g.id.clone(), g.name.clone()))
                        }
                    };
                    if let Some((group_id, old_name)) = group_info
                        && old_name != new_name {
                            let target = match tab {
                                manifold_ui::InspectorTab::Master => manifold_editing::commands::effect_target::EffectTarget::Master,
                                manifold_ui::InspectorTab::Layer => manifold_editing::commands::effect_target::EffectTarget::Layer {
                                    layer_index: self.active_layer_index.unwrap_or(0),
                                },
                                manifold_ui::InspectorTab::Clip => {
                                    if let Some(cid) = self.selection.primary_selected_clip_id.clone() {
                                        manifold_editing::commands::effect_target::EffectTarget::Clip { clip_id: cid }
                                    } else {
                                        manifold_editing::commands::effect_target::EffectTarget::Layer {
                                            layer_index: self.active_layer_index.unwrap_or(0),
                                        }
                                    }
                                }
                            };
                            let cmd = manifold_editing::commands::effect_groups::RenameGroupCommand::new(
                                target, group_id, old_name, new_name,
                            );
                            let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                            boxed.execute(&mut self.local_project);
                            self.send_content_cmd(ContentCommand::Execute(boxed));
                        }
                }
                self.needs_rebuild = true;
            }
            TextInputField::SearchFilter => {
                // Update browser popup filter — no undo command
                self.ui_root.browser_popup.set_filter(text.trim().to_string());
                self.needs_rebuild = true;
            }
        }
    }

    // tick_and_render() and present_all_windows() moved to app_render.rs


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
                let content_tex = unsafe { bridge.import_texture(&content_gpu.device) };
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
                tempo_recorder: manifold_playback::tempo_recorder::TempoRecorder::new(),
                link_beat_offset: f64::NAN,
                #[cfg(feature = "profiling")]
                profiler: None,
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
                if let Some(gpu) = &self.gpu
                    && let Some(ws) = self.window_registry.get_mut(&window_id) {
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

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = &self.gpu
                    && let Some(ws) = self.window_registry.get_mut(&window_id) {
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
                    if self.cursor_manager.needs_update()
                        && let Some(ws) = self.window_registry.get(&window_id) {
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
                                            (*a - current_ppb).abs().partial_cmp(&(*b - current_ppb).abs()).unwrap_or(std::cmp::Ordering::Equal)
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
                            Key::Character(c) => {
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
                        Key::Character(c) if c.as_str() == "s" && m.is_command_only() => {
                            self.save_project();
                            consumed = true;
                        }
                        // ── Open: Cmd+O ──
                        Key::Character(c) if c.as_str() == "o" && m.is_command_only() => {
                            self.open_project();
                            consumed = true;
                        }
                        // ── New: Cmd+N ──
                        Key::Character(c) if c.as_str() == "n" && m.is_command_only() => {
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
                if is_primary && !consumed
                    && let Some(ui_key) = Self::convert_key(&logical_key) {
                        self.ui_root.key_event(ui_key, self.modifiers);
                    }

                // Output window management (only when key wasn't consumed by app shortcuts)
                if !consumed
                    && let Key::Named(NamedKey::Escape) = &logical_key
                        && !is_primary {
                            self.window_registry.remove(&window_id);
                            log::info!("Closed output window");
                        }
            }

            // ── Cursor left window → cancel in-progress drags ────────
            WindowEvent::CursorLeft { .. } => {
                if is_primary {
                    if self.mouse_pressed {
                        log::debug!("Cursor left window — synthesizing PointerUp to cancel drag");
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
                    // Clear clip hover so bitmap doesn't stay painted in hover state
                    if self.selection.hovered_clip_id.is_some() {
                        self.selection.hovered_clip_id = None;
                        self.needs_scroll_rebuild = true;
                    }
                }
            }
            WindowEvent::CursorEntered { .. } => {}

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
                    // Clear clip hover so bitmap doesn't stay painted in hover state
                    if self.selection.hovered_clip_id.is_some() {
                        self.selection.hovered_clip_id = None;
                        self.needs_scroll_rebuild = true;
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
                            std::slice::from_ref(&path),
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

// render_text_input_overlay() moved to app_render.rs

impl Drop for Application {
    fn drop(&mut self) {
        // Ensure the content thread is shut down even on abnormal exit (panic, etc.).
        // Normal exit already handles this in WindowEvent::CloseRequested, but if the
        // Application is dropped without that event, the content thread would leak.
        if let Some(tx) = self.content_tx.take() {
            let _ = tx.send(ContentCommand::Shutdown);
        }
        if let Some(handle) = self.content_thread_handle.take() {
            let _ = handle.join();
            log::info!("[Application::Drop] content thread joined");
        }
    }
}
