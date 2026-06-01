use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Bpm, LayerId};
use manifold_editing::service::EditingService;
use manifold_playback::audio_decoder::DecodedAudio;
use manifold_playback::audio_sync::{ImportedAudioSyncController, PreloadedAudioData};
use manifold_playback::engine::PlaybackEngine;
use manifold_playback::percussion_orchestrator::PercussionImportOrchestrator;
#[cfg(not(target_os = "macos"))]
use manifold_playback::renderer::StubRenderer;
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu::GpuContext;
use manifold_renderer::layer_compositor::LayerCompositor;
use manifold_renderer::ui_renderer::UIRenderer;

use manifold_ui::cursors::{CursorManager, TimelineCursor};
use manifold_ui::input::{Modifiers, PointerAction};
use manifold_ui::node::Vec2;
use manifold_ui::ui_state::UIState;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;

use crate::frame_timer::FrameTimer;
use crate::project_io::ProjectIOService;
use crate::user_prefs::UserPrefs;
use crate::window_registry::{WindowRegistry, WindowRole, WindowState};
use crate::workspace::{Workspace, WorkspaceKind};

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

/// Tracks which inspector field is actively being dragged by the user.
/// After snapshot acceptance, the dragged value is restored to prevent overwrite.
#[derive(Debug, Clone)]
pub(crate) enum ActiveInspectorDrag {
    MasterOpacity(f32),
    LedBrightness(f32),
    LayerOpacity {
        layer_id: LayerId,
        value: f32,
    },
    ClipSlip {
        clip_id: manifold_core::ClipId,
        value: f32,
    },
    ClipLoop {
        clip_id: manifold_core::ClipId,
        value: f32,
    },
    EffectParam {
        tab: manifold_ui::InspectorTab,
        layer_id: LayerId,
        effect_idx: usize,
        param_id: manifold_core::effects::ParamId,
        value: f32,
        clip_id: Option<manifold_core::ClipId>,
    },
    GenParam {
        layer_id: LayerId,
        param_id: manifold_core::effects::ParamId,
        value: f32,
    },
}

impl ActiveInspectorDrag {
    /// Write the dragged value back into the project after snapshot acceptance.
    pub(crate) fn apply(&self, project: &mut manifold_core::project::Project) {
        match self {
            Self::MasterOpacity(v) => {
                project.settings.master_opacity = *v;
            }
            Self::LedBrightness(v) => {
                project.settings.led_brightness = *v;
            }
            Self::LayerOpacity { layer_id, value } => {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id) {
                    layer.opacity = *value;
                }
            }
            Self::ClipSlip { clip_id, value } => {
                if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                    clip.in_point = manifold_core::Seconds::from_f32(*value);
                }
            }
            Self::ClipLoop { clip_id, value } => {
                if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
                    clip.loop_duration_beats = manifold_core::Beats::from_f32(*value);
                }
            }
            Self::EffectParam {
                tab,
                layer_id,
                effect_idx,
                param_id,
                value,
                clip_id,
            } => {
                let effects: Option<&mut Vec<manifold_core::effects::EffectInstance>> = match tab {
                    manifold_ui::InspectorTab::Master => Some(&mut project.settings.master_effects),
                    manifold_ui::InspectorTab::Layer => project
                        .timeline
                        .find_layer_by_id_mut(layer_id)
                        .and_then(|(_, l)| l.effects.as_mut()),
                    manifold_ui::InspectorTab::Clip => clip_id.as_ref().and_then(|cid| {
                        project
                            .timeline
                            .find_clip_by_id_mut(cid)
                            .map(|c| &mut c.effects)
                    }),
                };
                if let Some(effects) = effects
                    && let Some(effect) = effects.get_mut(*effect_idx)
                    && let Some(slot) = effect.param_id_to_value_index(param_id.as_ref())
                    && slot < effect.param_values.len()
                {
                    effect.param_values[slot].value = *value;
                }
            }
            Self::GenParam {
                layer_id,
                param_id,
                value,
            } => {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id)
                    && let Some(gp) = layer.gen_params_mut()
                    && let Some(slot) =
                        manifold_core::generator_definition_registry::param_id_to_index(
                            gp.generator_type(),
                            param_id.as_ref(),
                        )
                    && slot < gp.param_values.len()
                {
                    gp.param_values[slot].value = *value;
                }
            }
        }
    }
}

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

    /// Last received project snapshot Arc. Used to skip redundant deep clones
    /// when the content thread sends the same Arc for modulation-only frames.
    pub(crate) last_snapshot_arc: Option<std::sync::Arc<manifold_core::project::Project>>,

    /// After a local project load (open/new), suppress content thread snapshots
    /// until its data_version exceeds this value. Prevents the old project from
    /// overwriting the locally-loaded new project before the content thread
    /// processes the LoadProject command.
    pub(crate) suppress_snapshot_until: u64,
    /// Frame count when suppress_snapshot_until was set (for timeout).
    pub(crate) suppress_snapshot_set_at: u64,

    // Selection
    pub(crate) selection: SelectionState,
    pub(crate) active_layer_id: Option<LayerId>,
    /// Slider drag snapshot for undo (opacity, slip, etc.). Stores the old value
    /// on snapshot, committed on release. NOT related to clip drag state.
    pub(crate) slider_snapshot: Option<f32>,
    /// Trim drag snapshot (min, max) for undo. Unity: onTrimSnapshot/onTrimCommit.
    pub(crate) trim_snapshot: Option<(f32, f32)>,
    /// ADSR drag snapshot (attack, decay, sustain, release) for undo.
    pub(crate) adsr_snapshot: Option<(f32, f32, f32, f32)>,
    /// Envelope target drag snapshot for undo.
    pub(crate) target_snapshot: Option<f32>,
    /// Envelope range drag snapshot (min, max) for undo.
    pub(crate) range_snapshot: Option<(f32, f32)>,
    /// User param-binding mapping range drag snapshot `(min, max)` for
    /// undo. Captured on `EffectMappingRangeSnapshot`, committed as one
    /// `EditUserParamBindingCommand` on `EffectMappingRangeCommit`.
    pub(crate) mapping_range_snapshot: Option<(f32, f32)>,
    /// User param-binding scale/offset drag snapshot `(scale, offset)` for
    /// undo. Captured on `EffectMappingAffineSnapshot`, committed as one
    /// `EditUserParamBindingCommand` on `EffectMappingAffineCommit`.
    pub(crate) mapping_affine_snapshot: Option<(f32, f32)>,

    /// Active inspector drag — prevents snapshot from overwriting dragged field.
    pub(crate) active_inspector_drag: Option<ActiveInspectorDrag>,

    // Effect clipboard (Unity: static EffectClipboard singleton, Rust: instance)
    pub(crate) effect_clipboard: manifold_editing::clipboard::EffectClipboard,

    // Rendering
    /// Shared reference to the content pipeline's output dimensions.
    pub(crate) content_pipeline_output: Option<Arc<crate::content_pipeline::SharedOutputView>>,
    /// IOSurface bridge for cross-device texture sharing (macOS).
    /// Content device writes compositor output to the IOSurface; UI device reads it.
    #[cfg(target_os = "macos")]
    /// IOSurface bridge for the workspace preview texture.
    #[cfg(target_os = "macos")]
    pub(crate) preview_texture_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// UI-side textures imported from the workspace preview IOSurfaces.
    #[cfg(target_os = "macos")]
    pub(crate) ui_preview_textures:
        [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// Last seen preview bridge generation.
    #[cfg(target_os = "macos")]
    pub(crate) last_preview_bridge_generation: u64,
    /// Last workspace preview IOSurface front_index seen by the UI thread.
    #[cfg(target_os = "macos")]
    pub(crate) last_output_front_index: usize,
    /// Last requested workspace preview surface size.
    #[cfg(target_os = "macos")]
    pub(crate) workspace_preview_size: (u32, u32),
    pub(crate) blit_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    pub(crate) blit_sampler: Option<manifold_gpu::GpuSampler>,
    pub(crate) atlas_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    pub(crate) atlas_sampler: Option<manifold_gpu::GpuSampler>,
    pub(crate) ui_renderer: Option<UIRenderer>,
    pub(crate) ui_cache_manager: Option<manifold_renderer::ui_cache_manager::UICacheManager>,
    pub(crate) layer_bitmap_gpu: Option<manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu>,
    pub(crate) scale_factor: f64,
    /// True while a display retarget is in flight — skip all potentially-
    /// blocking surface operations (next_drawable, commit_and_wait_scheduled)
    /// until the UiDisplayLink confirms it's alive on the new display.
    /// Prevents hard locks when GPU surfaces target stale displays during
    /// transitions (e.g., MacBook → 4K TV at 120Hz).
    pub(crate) display_retarget_pending: bool,
    /// Safety deadline: if the display link never fires after a retarget
    /// (display disconnected entirely), clear the pending flag after 2s
    /// so the app doesn't stay frozen forever.
    pub(crate) display_retarget_deadline: Option<std::time::Instant>,
    /// macOS EDR headroom for the primary window (1.0 = SDR, >1.0 = HDR capable).
    /// Drives compositor tonemap (passthrough if > 1.0, ACES if ≤ 1.0).
    pub(crate) edr_headroom: f64,
    /// macOS EDR headroom for the output window. Drives the per-window
    /// tonemap blit (ACES if ≤ 1.0 SDR, passthrough if > 1.0 HDR).
    pub(crate) output_edr_headroom: f64,

    /// Main timeline workspace. Owns its `UIRoot`, offscreen render
    /// target, CVDisplayLink, and dirty/resize flags. See
    /// [`crate::workspace::Workspace`].
    pub(crate) ws: crate::workspace::Workspace,
    /// Optional secondary workspace hosting the node-graph editor.
    /// `None` until the user opens the editor window via Cmd+Shift+G
    /// (or, in the future, the per-effect-card cog icon).
    pub(crate) graph_editor: Option<crate::workspace::Workspace>,
    /// `WindowId` of the graph editor window when open. Paired with
    /// `graph_editor` — both are `Some` together or both `None`.
    pub(crate) graph_editor_window_id: Option<WindowId>,
    /// Remembered outer position + inner size of the editor window from
    /// its last close, so reopening lands where the user left it instead
    /// of winit's default cascade. `None` until the first close.
    pub(crate) graph_editor_geometry:
        Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)>,
    /// Read-only graph canvas hosted in the editor. `Some` while the
    /// editor window is open; cleared on close. Phase 4 seeds it with
    /// a hardcoded view of `NodeGraphTestFX`'s graph.
    pub(crate) graph_canvas: Option<crate::graph_canvas::GraphCanvas>,
    /// Right-sidebar checkbox panel for V2 user-exposed parameters.
    /// Shares the editor window with `graph_canvas`.
    pub(crate) graph_editor_panel: manifold_ui::panels::graph_editor::GraphEditorPanel,
    /// Left-lane card mirror inside the graph editor — the exposed-param
    /// reflection of the effect card, in the lane the node palette used to
    /// occupy. Configured each frame from the live snapshot.
    pub(crate) graph_card_mirror:
        manifold_ui::panels::graph_card_mirror::GraphCardMirrorPanel,
    /// Built-once list of atoms shown in the spawn popup (node browser).
    /// The palette column is gone; this still feeds the popup's Node mode.
    pub(crate) palette_atoms_cache: Vec<manifold_ui::panels::graph_palette::GraphPaletteAtom>,
    /// Identifies which `EffectInstance` the editor is currently
    /// configuring. Set when `OpenGraphEditor(ei)` fires; cleared on
    /// editor-window close. The editor's right-sidebar reads this to
    /// look up that effect's `user_param_bindings`.
    pub(crate) current_editor_target: Option<(
        manifold_editing::commands::effect_target::EffectTarget,
        usize,
    )>,
    /// What graph the editor canvas is open on. Set by `OpenGraphEditor`
    /// (Effect target) or `OpenGeneratorGraphEditor` (Generator target);
    /// cleared when the editor closes. Every graph mutation command
    /// dispatched from PanelAction handlers passes through this — one
    /// editor surface, one command set, two persistence destinations.
    pub(crate) watched_graph_target: Option<manifold_core::GraphTarget>,
    /// Catalog-default graph def for the watched target's type.
    /// Cached at editor-open time so the mutation commands have it
    /// available to lift `None` graphs on first edit. For effects this
    /// is the bundled effect preset; for generators it's the bundled
    /// generator preset.
    pub(crate) watched_catalog_default:
        Option<manifold_core::effect_graph_def::EffectGraphDef>,

    // Frame timing
    pub(crate) frame_timer: FrameTimer,
    pub(crate) frame_count: u64,
    /// Cached transport display strings (avoids per-frame format! allocations).
    pub(crate) transport_cache: crate::ui_bridge::TransportDisplayCache,

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

    // Output window double-click fullscreen toggle.
    // Double-click fullscreen toggle for the output window.
    pub(crate) output_last_click: Option<std::time::Instant>,
    /// Saved window frame before entering manual borderless fullscreen.
    /// `Some(...)` = currently fullscreen. `None` = windowed.
    pub(crate) output_saved_frame: Option<[f64; 4]>,

    // File I/O
    pub(crate) current_project_path: Option<std::path::PathBuf>,
    /// Last export path — remembered across exports so the file dialog
    /// opens at the previous directory with the previous filename.
    pub(crate) last_export_path: Option<std::path::PathBuf>,
    pub(crate) user_prefs: UserPrefs,
    pub(crate) project_io: ProjectIOService,

    // Text input
    pub(crate) text_input: crate::text_input::TextInputState,

    // Pending audio load — receives results from background decode thread.
    // Unity loads audio async via coroutines; we use std::thread + mpsc channel.
    // Waveform data stays on UI thread; preloaded audio data is forwarded to content thread.
    pub(crate) pending_audio_load: Option<std::sync::mpsc::Receiver<PendingAudioLoadResult>>,

    /// Tracks the audio path that has been loaded (or is being loaded) so we
    /// can detect when the content thread sets a *new* audio_path after a fresh
    /// percussion import and trigger background audio loading + waveform decode.
    pub(crate) loaded_audio_path: Option<String>,

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
    /// Per-layer bitmap invalidation targets from editing operations.
    /// Drained in app_render.rs to call invalidate() on targeted layers only.
    pub(crate) invalidate_layers: Vec<usize>,

    // Detected display resolutions: (width, height, label).
    // Populated from winit monitors at startup. Matches Unity Footer.CollectDisplayResolutions.
    pub(crate) display_resolutions: Vec<(u32, u32, String)>,

    // State
    pub(crate) initialized: bool,
    /// Set on CloseRequested — prevents about_to_wait from rendering after shutdown.
    pub(crate) shutting_down: bool,
    pub(crate) pending_toggle_output: bool,
    pub(crate) pending_close_output: bool,
    pub(crate) pending_export: bool,
    /// Set by Cmd+Shift+G — opens the graph editor window in the next
    /// `about_to_wait` (where `ActiveEventLoop` is in scope).
    pub(crate) pending_open_graph_editor: bool,
    /// Performance mode state — see `crate::perform_mode`.
    pub(crate) perform: crate::perform_mode::PerformModeState,
    pub(crate) needs_rebuild: bool,
    /// Fine-grained scroll dirty flags — tracks which axis changed to enable
    /// skipping layer header rebuild on horizontal-only scroll.
    pub(crate) scroll_dirty: crate::ui_root::ScrollDirty,
    /// Set by keyboard shortcuts that mutate project data (undo, delete, etc.).
    /// Consumed by tick_and_render to trigger sync_project_data + rebuild.
    pub(crate) needs_structural_sync: bool,
}

impl Application {
    #[cfg(target_os = "macos")]
    const WORKSPACE_PREVIEW_QUANTUM: u32 = 64;

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
            last_snapshot_arc: None,
            suppress_snapshot_until: 0,
            suppress_snapshot_set_at: 0,
            selection: UIState::new(),
            active_layer_id: None,
            slider_snapshot: None,
            trim_snapshot: None,
            adsr_snapshot: None,
            target_snapshot: None,
            range_snapshot: None,
            mapping_range_snapshot: None,
            mapping_affine_snapshot: None,
            active_inspector_drag: None,
            effect_clipboard: manifold_editing::clipboard::EffectClipboard::new(),
            content_pipeline_output: None,
            #[cfg(target_os = "macos")]
            #[cfg(target_os = "macos")]
            preview_texture_bridge: None,
            #[cfg(target_os = "macos")]
            #[cfg(target_os = "macos")]
            ui_preview_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            #[cfg(target_os = "macos")]
            last_preview_bridge_generation: 0,
            #[cfg(target_os = "macos")]
            last_output_front_index: usize::MAX,
            #[cfg(target_os = "macos")]
            workspace_preview_size: (1920, 1080),
            blit_pipeline: None,
            blit_sampler: None,
            atlas_pipeline: None,
            atlas_sampler: None,
            ui_renderer: None,
            ui_cache_manager: None,
            layer_bitmap_gpu: None,
            scale_factor: 1.0,
            display_retarget_pending: false,
            display_retarget_deadline: None,
            edr_headroom: 1.0,
            output_edr_headroom: 1.0,
            ws: Workspace::new(WorkspaceKind::Main),
            graph_editor: None,
            graph_editor_window_id: None,
            graph_editor_geometry: None,
            graph_canvas: None,
            graph_editor_panel: manifold_ui::panels::graph_editor::GraphEditorPanel::new(),
            graph_card_mirror:
                manifold_ui::panels::graph_card_mirror::GraphCardMirrorPanel::new(),
            palette_atoms_cache: {
                use manifold_renderer::node_graph::{Category, descriptor_for};
                let cat_of = |type_id: &str| {
                    descriptor_for(type_id)
                        .map(|d| d.category)
                        .unwrap_or(Category::Uncategorized)
                };
                let order = |c: Category| {
                    Category::ALL
                        .iter()
                        .position(|&x| x == c)
                        .unwrap_or(usize::MAX)
                };
                let mut atoms: Vec<_> = manifold_renderer::node_graph::palette_atoms()
                    .into_iter()
                    .map(|a| {
                        let category = cat_of(&a.type_id).label().to_string();
                        manifold_ui::panels::graph_palette::GraphPaletteAtom {
                            label: a.label,
                            type_id: a.type_id,
                            category,
                        }
                    })
                    .collect();
                // Group by the 19-category taxonomy (Color & Tone, Noise,
                // Distort & Warp, ...) in display order, then alphabetically,
                // instead of the coarse Atom / Driver split. graph_palette
                // renders one header per contiguous category run.
                atoms.sort_by(|a, b| {
                    order(cat_of(&a.type_id))
                        .cmp(&order(cat_of(&b.type_id)))
                        .then_with(|| a.label.cmp(&b.label))
                });
                atoms
            },
            current_editor_target: None,
            watched_graph_target: None,
            watched_catalog_default: None,
            // UI frame rate: uncapped (120fps target, vsync limits actual present).
            // Content thread has its own timer at project FPS — fully decoupled.
            frame_timer: FrameTimer::new(120.0),
            frame_count: 0,
            transport_cache: crate::ui_bridge::TransportDisplayCache::new(),
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
            output_last_click: None,
            output_saved_frame: None,
            current_project_path: None,
            last_export_path: None,
            project_io: {
                let prefs = UserPrefs::load();
                ProjectIOService::new(&prefs)
            },
            user_prefs: UserPrefs::load(),
            text_input: crate::text_input::TextInputState::new(),
            pending_audio_load: None,
            loaded_audio_path: None,
            input_handler: crate::input_handler::InputHandler::new(),
            overlay: manifold_ui::interaction_overlay::InteractionOverlay::new(
                manifold_ui::color::CLIP_VERTICAL_PAD,
            ),
            pre_drag_commands: Vec::new(),
            invalidate_layers: Vec::new(),
            display_resolutions: Vec::new(),
            initialized: false,
            shutting_down: false,
            pending_toggle_output: false,
            pending_export: false,
            pending_close_output: false,
            pending_open_graph_editor: false,
            perform: crate::perform_mode::PerformModeState::new(),
            needs_rebuild: false,
            scroll_dirty: crate::ui_root::ScrollDirty::default(),
            needs_structural_sync: false,
        }
    }

    #[cfg(target_os = "macos")]
    fn compute_workspace_preview_size(
        output_w: u32,
        output_h: u32,
        video_w_logical: f32,
        video_h_logical: f32,
        scale_factor: f64,
    ) -> (u32, u32) {
        let physical_w = (video_w_logical * scale_factor as f32).floor().max(1.0) as u32;
        let physical_h = (video_h_logical * scale_factor as f32).floor().max(1.0) as u32;
        let output_aspect = output_w.max(1) as f32 / output_h.max(1) as f32;
        let rect_aspect = physical_w as f32 / physical_h as f32;
        let quantum = Self::WORKSPACE_PREVIEW_QUANTUM;
        let align_dim = |value: u32| value.max(2) & !1;

        if output_aspect >= rect_aspect {
            let width = ((physical_w / quantum).max(1)) * quantum;
            let height = ((width as f32) / output_aspect).round().max(1.0) as u32;
            (align_dim(width), align_dim(height))
        } else {
            let height = ((physical_h / quantum).max(1)) * quantum;
            let width = ((height as f32) * output_aspect).round().max(1.0) as u32;
            (align_dim(width), align_dim(height))
        }
    }

    #[cfg(target_os = "macos")]
    fn current_workspace_preview_size(&self) -> (u32, u32) {
        let video_rect = self.ws.ui_root.layout.video_area();
        Self::compute_workspace_preview_size(
            self.local_project.settings.output_width.max(1) as u32,
            self.local_project.settings.output_height.max(1) as u32,
            video_rect.width.max(1.0),
            video_rect.height.max(1.0),
            self.scale_factor,
        )
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn sync_workspace_preview_size(&mut self) {
        let size = self.current_workspace_preview_size();
        if size == self.workspace_preview_size {
            return;
        }
        self.workspace_preview_size = size;
        self.send_content_cmd(ContentCommand::ResizeWorkspacePreview(size.0, size.1));
    }

    /// Send a command to the content thread (no-op if not yet spawned).
    pub(crate) fn send_content_cmd(&self, cmd: ContentCommand) {
        if let Some(ref tx) = self.content_tx {
            ContentCommand::send(tx, cmd);
        }
    }

    pub(crate) fn create_default_project() -> Project {
        let mut project = Project::default();
        project.settings.bpm = manifold_core::Bpm(120.0);
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
                DragMode::Move
                | DragMode::TrimLeft
                | DragMode::TrimRight
                | DragMode::RegionSelect => return,
                DragMode::None => {}
            }
        }

        // Priority 2: Inspector resize edge hover
        if self.ws.ui_root.inspector_resize_dragging
            || self.ws.ui_root.is_near_inspector_edge(self.cursor_pos)
        {
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
            if self.ws.ui_root.inspector_resize_dragging {
                self.ws.ui_root.set_inspector_handle_drag();
            } else {
                self.ws.ui_root.set_inspector_handle_hover();
            }
            return;
        }
        self.ws.ui_root.set_inspector_handle_idle();

        // Priority 3: Video/timeline split handle hover
        // Use the same hit test as click detection (layout.split_handle rect).
        let near_split =
            self.split_dragging || self.ws.ui_root.layout.is_near_split_handle(self.cursor_pos);
        if near_split {
            if !self.split_dragging {
                self.ws.ui_root.set_split_handle_hover();
            }
            self.cursor_manager.set(TimelineCursor::ResizeVertical);
            self.split_was_hovered = true;
            return;
        } else if self.split_was_hovered && !self.split_dragging {
            self.ws.ui_root.set_split_handle_idle();
            self.split_was_hovered = false;
        }

        // Priority 4: Clip trim handle hover
        let tracks_rect = self.ws.ui_root.viewport.tracks_rect();
        if tracks_rect.contains(self.cursor_pos)
            && let Some(hit) = self.ws.ui_root.viewport.hit_test_clip(self.cursor_pos)
        {
            match hit.region {
                manifold_ui::panels::HitRegion::TrimLeft
                | manifold_ui::panels::HitRegion::TrimRight => {
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
        use manifold_ui::cursor_nav::{NavClipInfo, NavLayerInfo, NavResult, navigate_cursor};

        let current_beat = self
            .selection
            .insert_cursor_beat
            .unwrap_or(self.content_state.current_beat)
            .as_f32();
        let active_idx = self
            .active_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        let insert_cursor_idx = self
            .selection
            .insert_cursor_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        let current_layer = insert_cursor_idx.or(active_idx).unwrap_or(0);
        let grid_interval = self.ws.ui_root.viewport.grid_step();

        // Build layer info for navigation (skip collapsed layers)
        let layers: Vec<NavLayerInfo> = Some(&self.local_project)
            .map(|p| {
                p.timeline
                    .layers
                    .iter()
                    .enumerate()
                    .map(|(i, l)| NavLayerInfo {
                        index: i,
                        height: if l.is_collapsed { 0.0 } else { 140.0 },
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Build clip info for auto-select
        let clips: Vec<NavClipInfo> = Some(&self.local_project)
            .map(|p| {
                p.timeline
                    .layers
                    .iter()
                    .enumerate()
                    .flat_map(|(li, l)| {
                        l.clips.iter().map(move |c| NavClipInfo {
                            clip_id: c.id.clone(),
                            layer_index: li,
                            start_beat: c.start_beat.as_f32(),
                            end_beat: (c.start_beat + c.duration_beats).as_f32(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        match navigate_cursor(
            direction,
            current_beat,
            current_layer,
            grid_interval,
            self.modifiers.shift,
            &layers,
            &clips,
        ) {
            NavResult::SelectClip(clip_id) => {
                // Find the clip's layer for proper UIState selection
                let li =
                    Some(&self.local_project)
                        .and_then(|p| {
                            p.timeline.layers.iter().enumerate().find_map(|(i, l)| {
                                l.clips.iter().any(|c| c.id == clip_id).then_some(i)
                            })
                        })
                        .unwrap_or(0);
                let lid = self
                    .local_project
                    .timeline
                    .layers
                    .get(li)
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default();
                self.selection.select_clip(clip_id, lid);
                self.active_layer_id = self
                    .local_project
                    .timeline
                    .layers
                    .get(li)
                    .map(|l| l.layer_id.clone());
                self.needs_rebuild = true;
            }
            NavResult::SetCursor { beat, layer } => {
                let lid = self
                    .local_project
                    .timeline
                    .layers
                    .get(layer)
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default();
                self.selection
                    .set_insert_cursor(manifold_core::Beats::from_f32(beat), lid);
                self.active_layer_id = self
                    .local_project
                    .timeline
                    .layers
                    .get(layer)
                    .map(|l| l.layer_id.clone());
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
                    let new_bpm = Bpm(new_bpm.clamp(20.0, 300.0));
                    if let Some(project) = Some(&mut self.local_project) {
                        let old_bpm = project.settings.bpm;
                        // Unity: skip if approximately equal
                        if (old_bpm.0 - new_bpm.0).abs() >= 0.01 {
                            // Must use with_tempo_map so the tempo map point at
                            // beat 0 is updated — sync_project_bpm_from_current_beat
                            // reads from the tempo map every tick and would revert
                            // settings.bpm back to the old map value otherwise.
                            let old_points = project.tempo_map.clone_points();
                            let cmd = manifold_editing::commands::settings::ChangeBpmCommand::with_tempo_map(
                                old_bpm, new_bpm,
                                manifold_core::types::TempoPointSource::Manual,
                                false,
                                old_points,
                            );
                            {
                                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                    Box::new(cmd);
                                boxed.execute(project);
                                self.send_content_cmd(ContentCommand::Execute(boxed));
                            }
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
                            project.settings.frame_rate,
                            fps,
                        );
                        {
                            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                Box::new(cmd);
                            boxed.execute(project);
                            self.send_content_cmd(ContentCommand::Execute(boxed));
                        }
                    }
                    // Content thread renders at project FPS; UI always runs at display rate.
                    self.send_content_cmd(ContentCommand::SetFrameRate(fps as f64));
                    self.needs_rebuild = true;
                }
            }
            TextInputField::LayerName(idx) => {
                if let Some(layer) = self.local_project.timeline.layers.get(idx) {
                    let layer_id = layer.layer_id.clone();
                    let old_name = layer.name.clone();
                    let new_name = text.to_string();
                    if old_name != new_name {
                        let cmd = manifold_editing::commands::layer::RenameLayerCommand::new(
                            layer_id, old_name, new_name,
                        );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
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
                let new_bpm = if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
                    0.0
                } else if let Ok(v) = trimmed.parse::<f32>() {
                    if v > 0.0 { v.clamp(20.0, 300.0) } else { 0.0 }
                } else {
                    return; // parse failed — silent no-op (matches Unity)
                };
                if let Some(clip_id) = &self.selection.primary_selected_clip_id {
                    let clip_id = clip_id.clone();
                    if let Some(project) = Some(&mut self.local_project) {
                        let old_bpm = project
                            .timeline
                            .find_clip_by_id(&clip_id)
                            .map(|c| c.recorded_bpm)
                            .unwrap_or(0.0);
                        if (old_bpm - new_bpm).abs() >= 0.01 {
                            let cmd =
                                manifold_editing::commands::clip::ChangeClipRecordedBpmCommand::new(
                                    clip_id, old_bpm, new_bpm,
                                );
                            {
                                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                    Box::new(cmd);
                                boxed.execute(project);
                                self.send_content_cmd(ContentCommand::Execute(boxed));
                            }
                        }
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::MacroLabel(idx) => {
                let new_label = text.trim().to_string();
                if let Some(slot) = self.local_project.settings.macro_bank.slots.get(idx) {
                    let old_label = slot.label.clone();
                    if old_label != new_label {
                        let cmd =
                            manifold_editing::commands::settings::RenameMacroLabelCommand::new(
                                idx, old_label, new_label,
                            );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::EffectParam(effect_idx, param_idx) => {
                if let Ok(parsed) = text.parse::<f32>() {
                    let tab = self.ws.ui_root.inspector.last_effect_tab();
                    // Resolve effect instance to get type + old value
                    let effect_info = match tab {
                        manifold_ui::InspectorTab::Master => self
                            .local_project
                            .settings
                            .master_effects
                            .get(effect_idx)
                            .map(|fx| (fx.effect_type(), fx.get_base_param(param_idx))),
                        manifold_ui::InspectorTab::Layer => self
                            .active_layer_id
                            .as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .and_then(|(_, l)| l.effects.as_ref())
                            .and_then(|e| e.get(effect_idx))
                            .map(|fx| (fx.effect_type(), fx.get_base_param(param_idx))),
                        manifold_ui::InspectorTab::Clip => self
                            .selection
                            .primary_selected_clip_id
                            .as_ref()
                            .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                            .and_then(|c| c.effects.get(effect_idx))
                            .map(|fx| (fx.effect_type(), fx.get_base_param(param_idx))),
                    };
                    if let Some((effect_type, old_val)) = effect_info {
                        // Clamp to param range from registry
                        let new_val = if let Some(def) =
                            manifold_core::effect_definition_registry::try_get(effect_type)
                        {
                            if let Some(pd) = def.param_defs.get(param_idx) {
                                parsed.clamp(pd.min, pd.max)
                            } else {
                                parsed
                            }
                        } else {
                            parsed
                        };
                        if (old_val - new_val).abs() > f32::EPSILON
                            && let Some(param_id) =
                                manifold_core::effect_definition_registry::param_index_to_id(
                                    effect_type,
                                    param_idx,
                                )
                        {
                            let target = match tab {
                                manifold_ui::InspectorTab::Master => {
                                    manifold_editing::commands::effect_target::EffectTarget::Master
                                }
                                manifold_ui::InspectorTab::Layer
                                | manifold_ui::InspectorTab::Clip => {
                                    let layer_id = self.active_layer_id.clone().unwrap_or_default();
                                    manifold_editing::commands::effect_target::EffectTarget::Layer {
                                        layer_id,
                                    }
                                }
                            };
                            let cmd =
                                manifold_editing::commands::effects::ChangeEffectParamCommand::new(
                                    target, effect_idx, param_id, old_val, new_val,
                                );
                            if let Some(project) = Some(&mut self.local_project) {
                                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                    Box::new(cmd);
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
                    && let Some(layer_idx) = self
                        .active_layer_id
                        .as_ref()
                        .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id))
                    && let Some(layer) = self.local_project.timeline.layers.get(layer_idx)
                {
                    let gen_type = layer.generator_type();
                    // Clamp to param range from generator registry
                    let new_val = if let Some(def) =
                        manifold_core::generator_definition_registry::try_get(gen_type)
                    {
                        if let Some(pd) = def.param_defs.get(param_idx) {
                            parsed.clamp(pd.min, pd.max)
                        } else {
                            parsed
                        }
                    } else {
                        parsed
                    };
                    if let Some(gp) = layer.gen_params() {
                        // Base-value snapshot as plain floats (base if present,
                        // else effective). `param_values` is `Vec<ParamSlot>`
                        // now, so fall back through the float projection.
                        let base: Vec<f32> = gp.snapshot_params();
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
                            let lid = layer.layer_id.clone();
                            let cmd = manifold_editing::commands::settings::ChangeGeneratorParamsCommand::new(
                                        lid, old_params, new_params,
                                    );
                            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                Box::new(cmd);
                            boxed.execute(&mut self.local_project);
                            self.send_content_cmd(ContentCommand::Execute(boxed));
                        }
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::GenStringParam(sp_idx) => {
                // Commit a generator string param change (e.g. text content).
                // Look up the string_param_def key from the active layer's generator def,
                // then find the active clip to get the old value.
                if let Some(layer_idx) = self
                    .active_layer_id
                    .as_ref()
                    .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id))
                    && let Some(layer) = self.local_project.timeline.layers.get(layer_idx)
                {
                    let gen_type = layer.generator_type();
                    if let Some(def) =
                        manifold_core::generator_definition_registry::try_get(gen_type)
                        && let Some(sp_def) = def.string_param_defs.get(sp_idx)
                    {
                        let key = sp_def.key.to_string();
                        let new_value: Option<String> = if text.is_empty() {
                            None
                        } else {
                            Some(text.to_string())
                        };

                        // Find clip: selected clip on this layer, or first clip
                        let clip = self
                            .selection
                            .primary_selected_clip_id
                            .as_ref()
                            .and_then(|sel_id| layer.clips.iter().find(|c| c.id == *sel_id))
                            .or_else(|| layer.clips.first());
                        let (clip_id, old_value) = clip
                            .map(|c| {
                                let old =
                                    c.string_params.as_ref().and_then(|m| m.get(&key)).cloned();
                                (c.id.clone(), old)
                            })
                            .unwrap_or_default();

                        if old_value != new_value {
                            let cmd =
                                manifold_editing::commands::clip::SetClipStringParamCommand::new(
                                    clip_id, key, old_value, new_value,
                                );
                            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                Box::new(cmd);
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
                    let tab = self.ws.ui_root.inspector.last_effect_tab();
                    // Find the group by index
                    let group_info = match tab {
                        manifold_ui::InspectorTab::Master => self
                            .local_project
                            .settings
                            .master_effect_groups
                            .as_ref()
                            .and_then(|groups| groups.get(group_idx))
                            .map(|g| (g.id.clone(), g.name.clone())),
                        manifold_ui::InspectorTab::Layer => self
                            .active_layer_id
                            .as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .and_then(|(_, l)| l.effect_groups.as_ref())
                            .and_then(|g| g.get(group_idx))
                            .map(|g| (g.id.clone(), g.name.clone())),
                        manifold_ui::InspectorTab::Clip => self
                            .selection
                            .primary_selected_clip_id
                            .as_ref()
                            .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                            .and_then(|c| c.effect_groups.as_ref())
                            .and_then(|g| g.get(group_idx))
                            .map(|g| (g.id.clone(), g.name.clone())),
                    };
                    if let Some((group_id, old_name)) = group_info
                        && old_name != new_name
                    {
                        let target = match tab {
                            manifold_ui::InspectorTab::Master => {
                                manifold_editing::commands::effect_target::EffectTarget::Master
                            }
                            manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Clip => {
                                let layer_id = self.active_layer_id.clone().unwrap_or_default();
                                manifold_editing::commands::effect_target::EffectTarget::Layer {
                                    layer_id,
                                }
                            }
                        };
                        let cmd =
                            manifold_editing::commands::effect_groups::RenameGroupCommand::new(
                                target, group_id, old_name, new_name,
                            );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::SearchFilter => {
                // Update browser popup filter — no undo command
                self.ws
                    .ui_root
                    .browser_popup
                    .set_filter(text.trim().to_string());
                self.needs_rebuild = true;
            }
            TextInputField::MarkerName => {
                if let Some(marker_id) = self.text_input.marker_id.take() {
                    let new_name = text.to_string();
                    let old_name = self
                        .local_project
                        .timeline
                        .find_marker(&marker_id)
                        .map(|m| m.name.clone())
                        .unwrap_or_default();
                    if old_name != new_name {
                        let cmd = manifold_editing::commands::marker::RenameMarkerCommand::new(
                            marker_id, old_name, new_name,
                        );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                }
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
            let native_size = monitor
                .video_modes()
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
            let label = monitor
                .name()
                .unwrap_or_else(|| format!("Display {}", i + 1));
            log::info!(
                "Detected monitor: {} native={}x{} scaled={}x{} scale={:.2}",
                label,
                w,
                h,
                scaled.width,
                scaled.height,
                monitor.scale_factor()
            );

            if w > 0 && h > 0 {
                self.display_resolutions.push((w, h, label));
            }
        }
        // Rename to "Display N" for consistent UI (Unity uses 1-indexed "Display N")
        for (i, entry) in self.display_resolutions.iter_mut().enumerate() {
            entry.2 = format!("Display {}", i + 1);
        }

        // Create native Metal GPU context
        let gpu = {
            let native_device = manifold_gpu::GpuDevice::new();

            // Create native Metal surface for the workspace window.
            // displaySyncEnabled = false: the CVDisplayLink handles vsync
            // pacing. With displaySync=true, nextDrawable() blocks until the
            // NEXT hardware vsync — adding a full frame of latency on top of
            // the CVDisplayLink's vsync signal. This doubles the effective
            // frame time (~9ms at 120Hz) and causes hard locks during display
            // transitions when vsync timing is disrupted.
            let surface = native_device.create_surface(
                &*window,
                size.width.max(1),
                size.height.max(1),
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
                false, // no display sync — CVDisplayLink is the pacer
            );
            // 3 drawables: CVDisplayLink is the pacer so nextDrawable should
            // not block. 3 ensures availability even with frame timing jitter.
            surface.set_maximum_drawable_count(3);
            // Don't batch presents into Core Animation transactions —
            // preserves the timing guarantees of the display link.
            surface.set_presents_with_transaction(false);
            // EDR: configure colorspace + query headroom
            surface.configure_edr();
            self.edr_headroom = crate::edr_surface::query_window_headroom(&window);
            crate::edr_surface::register_screen_change_observer();

            // Register primary window
            let wid = window.id();
            self.primary_window_id = Some(wid);
            // Clone Arc before moving into WindowState — needed for UiDisplayLink.
            #[cfg(target_os = "macos")]
            let window_arc = Arc::clone(&window);
            self.window_registry.add(
                wid,
                WindowState {
                    window,
                    surface: Some(surface),
                    role: WindowRole::Workspace,
                },
            );

            // Start CVDisplayLink for the MacBook display — vsync-aligned
            // render trigger replacing the free-running FrameTimer.
            #[cfg(target_os = "macos")]
            {
                self.ws.ui_display_link = Some(crate::display_link::UiDisplayLink::new(window_arc));
            }

            // Blit pipeline (composite output → drawable with aspect-fit viewport)
            // Fullscreen triangle from vertex_index
            // and atlas blit. Avoids vertex buffer / vertex descriptor path which
            // produces no visible output through the WGSL→MSL compilation pipeline.
            let blit_shader = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;
            self.blit_pipeline = Some(native_device.create_render_pipeline(
                blit_shader,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
                None,
                "Blit Pipeline",
            ));
            self.blit_sampler = Some(native_device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                ..Default::default()
            }));

            // Atlas blit pipeline (premultiplied alpha — One/OneMinusSrcAlpha)
            let atlas_shader = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;
            let premultiplied_blend = manifold_gpu::GpuBlendState {
                src_factor: manifold_gpu::GpuBlendFactor::One,
                dst_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
                operation: manifold_gpu::GpuBlendOp::Add,
                src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
                dst_alpha_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
                alpha_operation: manifold_gpu::GpuBlendOp::Add,
            };
            self.atlas_pipeline = Some(native_device.create_render_pipeline(
                atlas_shader,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
                Some(premultiplied_blend),
                "Atlas Blit Pipeline",
            ));
            self.atlas_sampler =
                Some(native_device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                    min_filter: manifold_gpu::GpuFilterMode::Nearest,
                    mag_filter: manifold_gpu::GpuFilterMode::Nearest,
                    ..Default::default()
                }));

            // Create UI renderer using native Metal
            self.ui_renderer = Some(UIRenderer::new(
                &native_device,
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            ));

            // Create panel cache system
            self.ui_cache_manager = Some(manifold_renderer::ui_cache_manager::UICacheManager::new(
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
                scale,
            ));

            // Create layer bitmap GPU
            self.layer_bitmap_gpu = Some(manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu::new(
                &native_device,
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            ));

            self.scale_factor = scale;

            GpuContext {
                device: native_device,
            }
        };

        // Create initial offscreen UI render target.
        self.resize_ui_offscreen(size.width.max(1), size.height.max(1));

        // Spawn content thread with its OWN GPU device (separate queue for isolation).
        // Compositor output is shared via IOSurface — zero copy, GPU-to-GPU.
        {
            let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
            let (state_tx, state_rx) = crossbeam_channel::unbounded::<ContentState>();

            // Content thread uses its own native Metal device from manifold-gpu.

            let output_w = self.local_project.settings.output_width.max(1) as u32;
            let output_h = self.local_project.settings.output_height.max(1) as u32;
            let initial_layout = manifold_ui::layout::ScreenLayout::new(
                size.width as f32 / scale as f32,
                size.height as f32 / scale as f32,
            );
            let initial_video_area = initial_layout.video_area();
            let initial_preview_size = Self::compute_workspace_preview_size(
                output_w,
                output_h,
                initial_video_area.width,
                initial_video_area.height,
                scale,
            );
            self.workspace_preview_size = initial_preview_size;

            // Create IOSurface bridge for workspace preview (downscaled).
            // The main output path uses direct-to-drawable present from the
            // content thread — no full-resolution IOSurface bridge needed.
            #[cfg(target_os = "macos")]
            {
                let preview_bridge = crate::shared_texture::SharedTextureBridge::new(
                    initial_preview_size.0,
                    initial_preview_size.1,
                );
                let preview_bridge = Arc::new(preview_bridge);
                let preview_textures: [manifold_gpu::GpuTexture;
                    crate::shared_texture::SURFACE_COUNT] = std::array::from_fn(|i| unsafe {
                    preview_bridge.import_texture_native(&gpu.device, i)
                });
                self.ui_preview_textures = preview_textures.map(Some);
                self.preview_texture_bridge = Some(Arc::clone(&preview_bridge));
            }

            // Create native Metal device BEFORE renderers so they can build native pipelines.
            // This gives the content thread its OWN MTLCommandQueue, completely separate
            // from the UI thread's queue. Metal interleaves GPU work from both queues,
            // preventing the content thread from starving UI submissions.
            let native_device = manifold_gpu::GpuDevice::new();
            // Load pipeline binary archive — subsequent pipeline creation calls
            // automatically use it for near-instant cache hits.
            if let Ok(home) = std::env::var("HOME") {
                let cache_dir =
                    std::path::PathBuf::from(home).join("Library/Caches/com.latentspace.manifold");
                std::fs::create_dir_all(&cache_dir).ok();
                native_device.load_pipeline_archive(&cache_dir.join("pipeline_cache.metallib"));
                native_device.load_msl_cache(&cache_dir.join("msl_cache"));
            }
            log::info!("[GPU] Content thread: native MTLCommandQueue (manifold-gpu)");

            let gen_format = manifold_gpu::GpuTextureFormat::Rgba16Float;

            let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
                #[cfg(target_os = "macos")]
                Box::new(manifold_media::video_renderer::VideoRenderer::new(
                    &native_device,
                    output_w,
                    output_h,
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    8,
                )),
                #[cfg(not(target_os = "macos"))]
                Box::new(StubRenderer::new_video()),
                Box::new(GeneratorRenderer::new(
                    &native_device,
                    output_w,
                    output_h,
                    gen_format,
                    8,
                )),
            ];
            let mut engine = PlaybackEngine::new(renderers);
            engine.initialize(self.local_project.clone());

            let mut content_pipeline = crate::content_pipeline::ContentPipeline::new(Box::new(
                LayerCompositor::new(&native_device, output_w, output_h),
            ));
            content_pipeline.edr_headroom = self.edr_headroom;
            // Save pipeline archive after all pipelines have been created.
            native_device.save_pipeline_archive();
            native_device.log_msl_cache_stats();
            // Set device-level capture scope so Xcode GPU frame capture
            // grabs command buffers from both content and UI threads.
            native_device.install_device_capture_scope();
            // Transfer native device ownership to content pipeline.
            content_pipeline.set_native_gpu(native_device);
            // Give the content pipeline preview IOSurface textures for the workspace.
            #[cfg(target_os = "macos")]
            if let Some(ref bridge) = self.preview_texture_bridge {
                let native_dev = content_pipeline.native_device().unwrap();
                let preview_textures: [manifold_gpu::GpuTexture;
                    crate::shared_texture::SURFACE_COUNT] =
                    std::array::from_fn(|i| unsafe { bridge.import_texture_native(native_dev, i) });
                content_pipeline.set_preview_textures(preview_textures, Arc::clone(bridge));
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
                    log::warn!(
                        "[StemAudio] Failed to initialize stem audio controller: {}",
                        e
                    );
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
                transport_controller:
                    manifold_playback::transport_controller::TransportController::new(),
                gpu: GpuContext::new(),
                frame_count: 0,
                time_since_start: manifold_core::Seconds::ZERO,
                last_data_version: 0,
                midi_input,
                clip_launcher: manifold_playback::clip_launcher::ClipLauncher::new(),
                rendering_paused: false,
                timer: crate::frame_timer::FrameTimer::new(
                    self.local_project.settings.frame_rate as f64,
                ),
                #[cfg(target_os = "macos")]
                sync_arbiter: manifold_playback::sync::SyncArbiter::new(),
                osc_receiver: manifold_playback::osc_receiver::OscReceiver::new(),
                osc_sync: manifold_playback::osc_sync::OscSyncController::new(),
                osc_sender: manifold_playback::osc_sender::OscPositionSender::new(),
                osc_param_router: manifold_playback::osc_param_router::OscParamRouter::new(),
                ableton_bridge: manifold_playback::ableton_bridge::AbletonBridge::new(),
                ableton_active_last_frame: false,
                tempo_recorder: manifold_playback::tempo_recorder::TempoRecorder::new(),
                link_beat_offset: f64::NAN,
                led_controller: None,
                cached_midi_device_names: Vec::new(),
                last_midi_device_scan_time: manifold_core::Seconds(-10.0),
                cached_project_snapshot: None,
                watched_graph_effect: None,
                watched_graph_generator_layer: None,
                cached_generator_graph_snapshot: None,
                mod_scratch: crate::content_state::ModulationSnapshot::empty(),
                cached_midi_clock_position: Arc::from(""),
                cached_midi_clock_device: Arc::from(""),
                cached_osc_timecode: Arc::from(""),
                cached_perc_message: Arc::from(""),
                last_sent_midi_device_names: Arc::from([]),
                #[cfg(feature = "profiling")]
                profiler: None,
            };

            let cmd_tx_clone = cmd_tx.clone();
            let handle = std::thread::Builder::new()
                .name("content-thread".into())
                .spawn(move || {
                    content_thread.run(cmd_tx_clone, cmd_rx, state_tx);
                })
                .expect("Failed to spawn content thread");

            self.content_tx = Some(cmd_tx);
            self.state_rx = Some(state_rx);
            self.content_thread_handle = Some(handle);
            log::info!("[ContentThread] spawned (dual device + triple-buffered IOSurface bridge)");
        }

        self.gpu = Some(gpu);

        // Pass detected display resolutions to UI
        self.ws
            .ui_root
            .set_display_resolutions(self.display_resolutions.clone());

        // Build UI at initial window size (logical pixels)
        let logical_w = size.width as f32 / scale as f32;
        let logical_h = size.height as f32 / scale as f32;
        self.ws.ui_root.resize(logical_w, logical_h);
        #[cfg(target_os = "macos")]
        self.sync_workspace_preview_size();

        // Push initial project data (layers, tracks) and rebuild
        let active_idx = self
            .active_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        crate::ui_bridge::sync_project_data(
            &mut self.ws.ui_root,
            &self.local_project,
            active_idx,
            &self.selection,
        );
        crate::ui_bridge::sync_inspector_data(
            &mut self.ws.ui_root,
            &self.local_project,
            active_idx,
            &self.selection,
        );

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

        let is_graph_editor = Some(window_id) == self.graph_editor_window_id;

        match event {
            WindowEvent::CloseRequested => {
                if is_graph_editor {
                    self.close_graph_editor();
                    return;
                }
                if is_primary {
                    self.shutting_down = true;

                    // Stop display links FIRST — their callbacks may call
                    // nextDrawable() or request_redraw(). CVDisplayLinkStop
                    // blocks until the in-flight callback finishes, so this
                    // must happen before we destroy windows or block on joins.
                    #[cfg(target_os = "macos")]
                    {
                        self.ws.ui_display_link = None;
                    }

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
                    #[cfg(target_os = "macos")]
                    {
                        self.send_content_cmd(
                            crate::content_command::ContentCommand::ClearOutputSurface,
                        );
                        self.output_saved_frame = None;
                    }
                    self.window_registry.remove(&window_id);
                    log::info!("Closed output window");
                    self.perform_on_output_window_closed();
                }
            }

            WindowEvent::Resized(size) => {
                if is_graph_editor {
                    if let Some(ws) = self.window_registry.get_mut(&window_id)
                        && let Some(surface) = &mut ws.surface
                    {
                        surface.resize(size.width.max(1), size.height.max(1));
                    }
                    self.resize_graph_editor_offscreen(size.width.max(1), size.height.max(1));
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.surface_resized_this_frame = true;
                        ed.offscreen_dirty = true;
                    }
                    return;
                }
                if let Some(ws) = self.window_registry.get_mut(&window_id) {
                    let scale = ws.window.scale_factor();
                    if is_primary {
                        if let Some(surface) = &mut ws.surface {
                            surface.resize(size.width, size.height);
                            self.resize_ui_offscreen(size.width, size.height);
                            // Skip drawable acquisition this frame — the
                            // drawable pool may be reconfiguring after
                            // set_drawable_size.
                            self.ws.surface_resized_this_frame = true;
                            self.ws.offscreen_dirty = true;
                        }
                        let logical_w = size.width as f32 / scale as f32;
                        let logical_h = size.height as f32 / scale as f32;
                        self.ws.ui_root.resize(logical_w, logical_h);
                    } else {
                        // Output window resized — update drawable.
                        self.send_content_cmd(ContentCommand::ResizeOutputSurface(
                            size.width.max(1),
                            size.height.max(1),
                        ));
                    }
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if is_graph_editor {
                    let mut new_size = (1u32, 1u32);
                    if let Some(ws) = self.window_registry.get_mut(&window_id)
                        && let Some(surface) = &mut ws.surface
                    {
                        let size = ws.window.inner_size();
                        new_size = (size.width.max(1), size.height.max(1));
                        surface.resize(new_size.0, new_size.1);
                    }
                    self.resize_graph_editor_offscreen(new_size.0, new_size.1);
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.surface_resized_this_frame = true;
                        ed.offscreen_dirty = true;
                    }
                    let _ = scale_factor;
                    return;
                }
                if let Some(ws) = self.window_registry.get_mut(&window_id) {
                    let size = ws.window.inner_size();
                    if is_primary {
                        if let Some(surface) = &mut ws.surface {
                            surface.resize(size.width, size.height);
                            self.resize_ui_offscreen(size.width, size.height);
                            self.ws.surface_resized_this_frame = true;
                            self.ws.offscreen_dirty = true;
                        }
                        let logical_w = size.width as f32 / scale_factor as f32;
                        let logical_h = size.height as f32 / scale_factor as f32;
                        self.ws.ui_root.resize(logical_w, logical_h);
                        self.scale_factor = scale_factor;
                    }
                }
            }

            // ── Pointer input → UIInputSystem ──────────────────────
            WindowEvent::CursorMoved { position, .. } => {
                if is_graph_editor {
                    let (scale, window_w, window_h) = self
                        .window_registry
                        .get(&window_id)
                        .map(|ws| {
                            let s = ws.window.scale_factor();
                            let sz = ws.window.inner_size();
                            (s, sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                        })
                        .unwrap_or((1.0, 1.0, 1.0));
                    let logical_x = position.x as f32 / scale as f32;
                    let logical_y = position.y as f32 / scale as f32;
                    let palette_width = manifold_ui::panels::graph_palette::PALETTE_WIDTH;
                    let sidebar_x = window_w - manifold_ui::panels::graph_editor::SIDEBAR_WIDTH;
                    // Canvas viewport matches the render-time slice
                    // (offset by palette_width); without this the
                    // canvas's `to_graph` would treat screen x=0 as the
                    // canvas left edge and node hit-tests would be off
                    // by `palette_width` to the left.
                    let viewport = crate::graph_canvas::Rect::new(
                        palette_width,
                        0.0,
                        (sidebar_x - palette_width).max(0.0),
                        window_h,
                    );
                    // Always update canvas cursor — graph-space coords
                    // need it even for clicks that land in the sidebar.
                    if let Some(canvas) = self.graph_canvas.as_mut() {
                        canvas.on_pointer_move(viewport, logical_x, logical_y);
                        // Drive the mapping popover's live range drag /
                        // handle hover. No-op when the popover is closed.
                        canvas.popover_on_move(logical_x, logical_y);
                    }
                    // Forward into the editor's UITree only when the
                    // cursor sits in either margin (palette on the left
                    // or expose-panel sidebar on the right). Move
                    // events from the canvas region would just cause
                    // spurious hover/exit on tree nodes — except when the
                    // node picker is open, which overlays the whole window
                    // and wants hover feedback on its cells everywhere.
                    let in_panel = logical_x < palette_width || logical_x >= sidebar_x;
                    let picker_open = self
                        .graph_editor
                        .as_ref()
                        .is_some_and(|ed| ed.ui_root.browser_popup.is_open());
                    if (in_panel || picker_open)
                        && let Some(ed) = self.graph_editor.as_mut()
                    {
                        ed.ui_root.input.process_pointer(
                            &mut ed.ui_root.tree,
                            Vec2::new(logical_x, logical_y),
                            manifold_ui::input::PointerAction::Move,
                            self.time_since_start,
                        );
                    }
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    return;
                }
                if is_primary {
                    // Convert to logical pixels
                    let scale = self
                        .window_registry
                        .get(&window_id)
                        .map(|ws| ws.window.scale_factor())
                        .unwrap_or(1.0);
                    self.cursor_pos = Vec2::new(
                        position.x as f32 / scale as f32,
                        position.y as f32 / scale as f32,
                    );

                    if self.perform_handle_cursor_moved(self.cursor_pos) {
                        return;
                    }

                    // Split handle drag takes highest priority
                    // From Unity PanelResizeHandle.OnDrag
                    if self.split_dragging {
                        self.ws
                            .ui_root
                            .layout
                            .update_split_from_drag(self.cursor_pos.y);
                        self.cursor_manager.set(TimelineCursor::ResizeVertical);
                        self.needs_rebuild = true;
                    }
                    // Inspector resize drag takes next priority
                    else if self.ws.ui_root.inspector_resize_dragging {
                        if self.ws.ui_root.update_inspector_resize(self.cursor_pos.x) {
                            self.needs_rebuild = true;
                        }
                        self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
                    } else {
                        self.ws.ui_root.pointer_event(
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
                                &mut self.active_layer_id,
                                &mut self.needs_rebuild,
                                &mut self.needs_structural_sync,
                                &mut self.scroll_dirty,
                                &mut self.invalidate_layers,
                                &mut self.pre_drag_commands,
                            );
                            self.overlay.on_pointer_move(
                                self.cursor_pos,
                                &mut host,
                                &mut self.selection,
                                &self.ws.ui_root.viewport,
                            );
                        }

                        // Update cursor based on current interaction state.
                        // From Unity: Cursors.SetMove/SetBlocked/SetResizeHorizontal/SetDefault
                        self.update_cursor_for_position();
                    }

                    // Apply cursor to window if changed
                    if self.cursor_manager.needs_update()
                        && let Some(ws) = self.window_registry.get(&window_id)
                    {
                        let icon = match self.cursor_manager.pending_cursor_icon() {
                            TimelineCursor::Default => winit::window::CursorIcon::Default,
                            TimelineCursor::ResizeHorizontal => {
                                winit::window::CursorIcon::ColResize
                            }
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
                if is_graph_editor {
                    // Node picker is modal: when open it claims every click in
                    // the editor window. Route the press/release straight into
                    // the editor UITree (which holds the popup's backdrop +
                    // cells) and bypass the canvas / popover / in_panel split.
                    // The resulting Click event is drained + dispatched to the
                    // popup in `present_*` / the editor drain loop.
                    let picker_open = self
                        .graph_editor
                        .as_ref()
                        .is_some_and(|ed| ed.ui_root.browser_popup.is_open());
                    if picker_open {
                        // Cursor in editor-window logical coords. The canvas
                        // tracks it on CursorMoved; read it out before the
                        // mutable editor borrow.
                        let (cx, cy) = self
                            .graph_canvas
                            .as_ref()
                            .map(|c| c.cursor())
                            .unwrap_or((0.0, 0.0));
                        if button == MouseButton::Left
                            && let Some(ed) = self.graph_editor.as_mut()
                        {
                            let action = match state {
                                ElementState::Pressed => {
                                    manifold_ui::input::PointerAction::Down
                                }
                                ElementState::Released => {
                                    manifold_ui::input::PointerAction::Up
                                }
                            };
                            ed.ui_root.input.process_pointer(
                                &mut ed.ui_root.tree,
                                Vec2::new(cx, cy),
                                action,
                                self.time_since_start,
                            );
                            ed.offscreen_dirty = true;
                        }
                        return;
                    }
                    if let Some(canvas) = self.graph_canvas.as_mut() {
                        let window_size = self
                            .window_registry
                            .get(&window_id)
                            .map(|ws| {
                                let s = ws.window.scale_factor();
                                let sz = ws.window.inner_size();
                                (sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                            })
                            .unwrap_or((1.0, 1.0));
                        let (cx, cy) = canvas.cursor();
                        let palette_width =
                            manifold_ui::panels::graph_palette::PALETTE_WIDTH;
                        let sidebar_x =
                            window_size.0 - manifold_ui::panels::graph_editor::SIDEBAR_WIDTH;
                        // The UITree spans the whole editor window — both
                        // the left palette and the right sidebar live in
                        // it. Route any click in either margin to it; the
                        // canvas only sees clicks in the center column.
                        let in_panel = cx < palette_width || cx >= sidebar_x;
                        // Canvas viewport matches the render-time slice:
                        // origin at palette_width, width is the remaining
                        // center column. Passing this (not the full window)
                        // is what makes `to_graph` translate cursor coords
                        // into the canvas's coordinate system correctly.
                        let viewport = crate::graph_canvas::Rect::new(
                            palette_width,
                            0.0,
                            (sidebar_x - palette_width).max(0.0),
                            window_size.1,
                        );
                        match (button, state) {
                            (MouseButton::Left, ElementState::Pressed) => {
                                // The mapping popover floats over the
                                // canvas, so it gets first crack at a
                                // left-press wherever it lands. If it
                                // consumes the click (handle/button/dead-
                                // space), the canvas underneath stays
                                // untouched; otherwise the popover closes
                                // and we fall through to the normal path.
                                if canvas.popover_open()
                                    && canvas.popover_on_left_press(cx, cy)
                                {
                                    // consumed by the popover
                                } else if in_panel {
                                    if let Some(ed) = self.graph_editor.as_mut() {
                                        ed.ui_root.input.process_pointer(
                                            &mut ed.ui_root.tree,
                                            Vec2::new(cx, cy),
                                            manifold_ui::input::PointerAction::Down,
                                            self.time_since_start,
                                        );
                                    }
                                } else {
                                    canvas.on_left_button_down(
                                        viewport,
                                        cx,
                                        cy,
                                        self.time_since_start,
                                    );
                                }
                            }
                            (MouseButton::Left, ElementState::Released) => {
                                // Commit any popover range drag first.
                                canvas.popover_on_left_release();
                                if in_panel {
                                    if let Some(ed) = self.graph_editor.as_mut() {
                                        ed.ui_root.input.process_pointer(
                                            &mut ed.ui_root.tree,
                                            Vec2::new(cx, cy),
                                            manifold_ui::input::PointerAction::Up,
                                            self.time_since_start,
                                        );
                                    }
                                } else {
                                    canvas.on_left_button_up(viewport, cx, cy);
                                }
                            }
                            (MouseButton::Right, ElementState::Pressed) => {
                                // Right-click an expanded param row that's
                                // exposed as a card binding → open the
                                // in-place mapping popover anchored on it.
                                // The canvas resolves the row + anchor; the
                                // app resolves the binding (needs the
                                // project + snapshot the canvas can't see).
                                if !in_panel
                                    && let Some((node_id, pi)) =
                                        canvas.on_right_button_down(viewport, cx, cy)
                                    && let Some(inner_param) = canvas.param_name_at(node_id, pi)
                                    && let Some((
                                        binding_id,
                                        label,
                                        min,
                                        max,
                                        invert,
                                        curve,
                                        range,
                                    )) = crate::app_render::resolve_canvas_binding(
                                        self.content_state.active_graph_snapshot.as_deref(),
                                        self.current_editor_target.as_ref(),
                                        &self.local_project,
                                        node_id,
                                        &inner_param,
                                    )
                                {
                                    canvas.open_mapping_popover(
                                        viewport, node_id, pi, binding_id, label, min, max,
                                        invert, curve, range,
                                    );
                                }
                            }
                            (MouseButton::Middle, ElementState::Pressed) => {
                                if !in_panel {
                                    canvas.on_pan_button_down(cx, cy);
                                }
                            }
                            (MouseButton::Middle, ElementState::Released) => {
                                if !in_panel {
                                    canvas.on_pan_button_up();
                                }
                            }
                            _ => {}
                        }
                        if let Some(ed) = self.graph_editor.as_mut() {
                            ed.offscreen_dirty = true;
                        }
                    }
                    return;
                }
                if is_primary && self.perform_handle_mouse_input(button, state) {
                    return;
                }
                if is_primary {
                    match button {
                        MouseButton::Left => {
                            match state {
                                ElementState::Pressed => {
                                    self.mouse_pressed = true;

                                    // Track which panel has focus for context-sensitive shortcuts.
                                    // Matches Unity's InputHandler.inspectorHasFocus.
                                    // Any click outside inspector clears focus and effect selection
                                    // — layer headers, timeline tracks, transport bar, etc.
                                    let inspector_rect = self.ws.ui_root.layout.inspector();
                                    let in_inspector = inspector_rect.contains(self.cursor_pos);
                                    if !in_inspector && self.input_handler.inspector_has_focus {
                                        let ui = &mut self.ws.ui_root;
                                        ui.inspector.clear_effect_selection(&mut ui.tree);
                                    }
                                    self.input_handler.inspector_has_focus = in_inspector;

                                    // If a dropdown is open and the click lands outside it,
                                    // dismiss the dropdown and consume the event so that the
                                    // background node never receives a PointerDown (prevents
                                    // phantom pressed_id on the node behind the dropdown).
                                    if self.ws.ui_root.dropdown.is_open()
                                        && !self.ws.ui_root.dropdown.contains_point(self.cursor_pos)
                                    {
                                        self.ws.ui_root.dropdown.close(&mut self.ws.ui_root.tree);
                                        // Click is consumed by dismiss — do not forward.
                                    } else if self
                                        .ws
                                        .ui_root
                                        .layout
                                        .is_near_split_handle(self.cursor_pos)
                                    {
                                        // Begin video/timeline split drag.
                                        // From Unity PanelResizeHandle.OnPointerDown.
                                        self.split_dragging = true;
                                        self.ws.ui_root.set_split_handle_drag();
                                    } else if self
                                        .ws
                                        .ui_root
                                        .is_near_inspector_edge(self.cursor_pos)
                                    {
                                        self.ws.ui_root.begin_inspector_resize(self.cursor_pos.x);
                                        self.ws.ui_root.set_inspector_handle_drag();
                                    } else {
                                        self.ws.ui_root.pointer_event(
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
                                        self.ws.ui_root.set_split_handle_idle();
                                        // Persist to ProjectSettings (Unity WorkspaceController line 591)
                                        if let Some(project) = Some(&mut self.local_project) {
                                            project.settings.timeline_height_percent =
                                                self.ws.ui_root.layout.timeline_split_ratio;
                                        }
                                    } else if self.ws.ui_root.inspector_resize_dragging {
                                        // Persist to ProjectSettings (Unity WorkspaceController line 528)
                                        if let Some(project) = Some(&mut self.local_project) {
                                            project.settings.inspector_width =
                                                self.ws.ui_root.layout.inspector_width;
                                        }
                                        self.ws.ui_root.end_inspector_resize();
                                    } else {
                                        self.ws.ui_root.pointer_event(
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
                                self.ws.ui_root.right_click(self.cursor_pos);
                            }
                        }
                        _ => {}
                    }
                } else if !is_primary
                    && button == MouseButton::Left
                    && state == ElementState::Pressed
                {
                    // Double-click on the output window toggles a dedicated
                    // borderless presentation window instead of mutating the
                    // existing titled window in place.
                    const DOUBLE_CLICK_MS: u128 = 300;
                    let now = std::time::Instant::now();
                    let is_double = self
                        .output_last_click
                        .map(|t| now.duration_since(t).as_millis() < DOUBLE_CLICK_MS)
                        .unwrap_or(false);

                    if is_double {
                        self.output_last_click = None;
                        // Toggle fullscreen by resizing the existing window in place.
                        // Do NOT destroy/recreate — that disrupts the CVDisplayLink
                        // cadence and tears the output.
                        if let Some(ws) = self.window_registry.get_mut(&window_id)
                            && let WindowRole::Output {
                                ref mut presentation,
                                ..
                            } = ws.role
                        {
                            let new_presentation = !*presentation;
                            *presentation = new_presentation;

                            if new_presentation {
                                // → Fullscreen: save frame, expand to monitor
                                let outer = ws.window.outer_position().unwrap_or_default();
                                let inner = ws.window.inner_size();
                                self.output_saved_frame = Some([
                                    outer.x as f64,
                                    outer.y as f64,
                                    inner.width as f64,
                                    inner.height as f64,
                                ]);
                                if let Some(monitor) = ws.window.current_monitor() {
                                    let mon_pos = monitor.position();
                                    let mon_size = monitor.size();
                                    let scale = monitor.scale_factor();
                                    let lw = mon_size.width as f64 / scale;
                                    let lh = mon_size.height as f64 / scale;
                                    let lx = mon_pos.x as f64 / scale;
                                    let ly = mon_pos.y as f64 / scale;
                                    ws.window.set_decorations(false);
                                    let _ = ws
                                        .window
                                        .request_inner_size(winit::dpi::LogicalSize::new(lw, lh));
                                    ws.window
                                        .set_outer_position(winit::dpi::LogicalPosition::new(
                                            lx, ly,
                                        ));
                                }
                                // Set window level above menu bar (NSMainMenuWindowLevel=24)
                                // so our borderless window covers it on this
                                // display only — no global setPresentationOptions.
                                #[cfg(target_os = "macos")]
                                crate::edr_surface::set_window_level(&ws.window, 25);
                            } else {
                                // → Windowed: restore saved frame + menu bar
                                ws.window.set_decorations(true);
                                if let Some(frame) = self.output_saved_frame.take() {
                                    ws.window.set_outer_position(
                                        winit::dpi::PhysicalPosition::new(frame[0], frame[1]),
                                    );
                                    let _ = ws.window.request_inner_size(
                                        winit::dpi::PhysicalSize::new(frame[2], frame[3]),
                                    );
                                }
                                // Restore NSNormalWindowLevel=0 so the menu
                                // bar is no longer covered.
                                #[cfg(target_os = "macos")]
                                crate::edr_surface::set_window_level(&ws.window, 0);
                            }

                            let new_size = ws.window.inner_size();
                            self.send_content_cmd(ContentCommand::ResizeOutputSurface(
                                new_size.width.max(1),
                                new_size.height.max(1),
                            ));
                        }
                    } else {
                        self.output_last_click = Some(now);
                    }
                }
            }

            // ── Mouse wheel (scroll / zoom) ──────────────────────────
            WindowEvent::MouseWheel { delta, .. } => {
                if is_graph_editor && let Some(canvas) = self.graph_canvas.as_mut() {
                    const LINE_DELTA_PX: f32 = 20.0;
                    let dy = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y * LINE_DELTA_PX,
                        winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                    };
                    let viewport = self
                        .window_registry
                        .get(&window_id)
                        .map(|ws| {
                            let s = ws.window.scale_factor();
                            let sz = ws.window.inner_size();
                            crate::graph_canvas::Rect::new(
                                0.0,
                                0.0,
                                sz.width as f32 / s as f32,
                                sz.height as f32 / s as f32,
                            )
                        })
                        .unwrap_or(crate::graph_canvas::Rect::new(0.0, 0.0, 1.0, 1.0));
                    canvas.on_scroll(viewport, dy);
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    return;
                }
                if is_primary && self.perform_handle_mouse_wheel() {
                    return;
                }
                if is_primary {
                    // When the dropdown is open, route scroll to the UIEvent
                    // pipeline so the dropdown can handle it.
                    if self.ws.ui_root.dropdown.is_open() {
                        const LINE_DELTA_PX: f32 = 20.0;
                        let (dx, dy) = match delta {
                            winit::event::MouseScrollDelta::LineDelta(x, y) => {
                                (x * LINE_DELTA_PX, y * LINE_DELTA_PX)
                            }
                            winit::event::MouseScrollDelta::PixelDelta(pos) => {
                                (pos.x as f32, pos.y as f32)
                            }
                        };
                        self.ws
                            .ui_root
                            .input
                            .process_scroll(self.cursor_pos, Vec2::new(dx, dy));
                        return;
                    }
                    // Convert line deltas (mouse wheel notches) to logical pixels.
                    // Each downstream consumer applies its own speed constant on top.
                    const LINE_DELTA_PX: f32 = 20.0;
                    let (dx, dy) = match delta {
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            (x * LINE_DELTA_PX, y * LINE_DELTA_PX)
                        }
                        winit::event::MouseScrollDelta::PixelDelta(pos) => {
                            (pos.x as f32, pos.y as f32)
                        }
                    };

                    let pos = self.cursor_pos;
                    let inspector_rect = self.ws.ui_root.layout.inspector();
                    let tracks_rect = self.ws.ui_root.layout.timeline_tracks();

                    if inspector_rect.contains(pos) {
                        // Scroll the inspector panel — full rebuild (inspector is static)
                        self.ws.ui_root.inspector.handle_scroll_at(dy, pos.x);
                        self.needs_rebuild = true;
                    } else if tracks_rect.contains(pos) {
                        if self.modifiers.alt {
                            // Alt + scroll Y → zoom (step through zoom levels)
                            // Always anchor on the playhead, not the mouse cursor.
                            let playhead_beat = self.content_state.current_beat.as_f32();
                            let current_ppb = self.ws.ui_root.viewport.pixels_per_beat();
                            let playhead_px = self
                                .ws
                                .ui_root
                                .viewport
                                .beat_to_pixel(manifold_core::Beats::from_f32(playhead_beat));
                            let anchor_x =
                                (playhead_px - tracks_rect.x).clamp(0.0, tracks_rect.width);
                            let levels = &manifold_ui::color::ZOOM_LEVELS;
                            let current_idx = levels
                                .iter()
                                .position(|&l| (l - current_ppb).abs() < 0.01)
                                .unwrap_or_else(|| {
                                    levels
                                        .iter()
                                        .enumerate()
                                        .min_by(|(_, a), (_, b)| {
                                            (*a - current_ppb)
                                                .abs()
                                                .partial_cmp(&(*b - current_ppb).abs())
                                                .unwrap_or(std::cmp::Ordering::Equal)
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
                                // Anchor: keep the playhead at the same screen X
                                let new_scroll = playhead_beat - anchor_x / new_ppb;
                                self.ws.ui_root.viewport.set_zoom(new_ppb);
                                self.ws.ui_root.viewport.set_scroll(
                                    new_scroll.max(0.0),
                                    self.ws.ui_root.viewport.scroll_y_px(),
                                );
                                self.scroll_dirty.zoom = true;
                            }
                        } else if self.modifiers.shift {
                            // Shift + scroll Y → horizontal pan
                            let ppb = self.ws.ui_root.viewport.pixels_per_beat();
                            let beat_delta = dy * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                            let new_x = (self.ws.ui_root.viewport.scroll_x_beats().as_f32()
                                - beat_delta)
                                .max(0.0);
                            if self
                                .ws
                                .ui_root
                                .viewport
                                .set_scroll(new_x, self.ws.ui_root.viewport.scroll_y_px())
                            {
                                self.scroll_dirty.scroll_x = true;
                            }
                        } else {
                            // Plain scroll → vertical track scroll
                            let new_y = (self.ws.ui_root.viewport.scroll_y_px() - dy).max(0.0);
                            if self.ws.ui_root.viewport.set_scroll(
                                self.ws.ui_root.viewport.scroll_x_beats().as_f32(),
                                new_y,
                            ) {
                                // Sync layer headers with viewport vertical scroll
                                self.ws
                                    .ui_root
                                    .layer_headers
                                    .set_scroll_y(self.ws.ui_root.viewport.scroll_y_px());
                                self.scroll_dirty.scroll_y = true;
                            }
                        }
                        // Native horizontal scroll (trackpad two-finger swipe)
                        if dx.abs() > 0.01 && !self.modifiers.alt {
                            let ppb = self.ws.ui_root.viewport.pixels_per_beat();
                            let beat_delta = dx * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                            let new_x = (self.ws.ui_root.viewport.scroll_x_beats().as_f32()
                                - beat_delta)
                                .max(0.0);
                            if self
                                .ws
                                .ui_root
                                .viewport
                                .set_scroll(new_x, self.ws.ui_root.viewport.scroll_y_px())
                            {
                                self.scroll_dirty.scroll_x = true;
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
                self.ws.ui_root.input.set_modifiers(self.modifiers);
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
                if is_primary && self.perform_handle_key(&logical_key) {
                    return;
                }
                // Editor window: node picker is open → keystrokes drive its
                // search field. Must run BEFORE the Delete/Backspace node-
                // delete arm below, or Backspace would delete a node instead
                // of editing the filter. Escape dismisses; chars/space type;
                // every transition sets offscreen_dirty so the editor
                // repaints (it has no per-frame repaint loop when idle).
                if is_graph_editor
                    && self
                        .graph_editor
                        .as_ref()
                        .is_some_and(|ed| ed.ui_root.browser_popup.is_open())
                {
                    use winit::keyboard::{Key, NamedKey};
                    let mut filter_changed = false;
                    match &logical_key {
                        Key::Named(NamedKey::Escape) => {
                            if let Some(ed) = self.graph_editor.as_mut() {
                                ed.ui_root.browser_popup.handle_escape();
                            }
                            self.text_input.cancel();
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.text_input.backspace();
                            filter_changed = true;
                        }
                        Key::Named(NamedKey::Space) => {
                            self.text_input.insert_char(' ');
                            filter_changed = true;
                        }
                        Key::Character(c) => {
                            for ch in c.chars() {
                                self.text_input.insert_char(ch);
                            }
                            filter_changed = true;
                        }
                        // Suppress every other key while the modal is up.
                        _ => {}
                    }
                    if filter_changed {
                        let filter = self.text_input.text.trim().to_string();
                        if let Some(ed) = self.graph_editor.as_mut() {
                            ed.ui_root.browser_popup.set_filter(filter);
                        }
                    }
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    return;
                }
                // Editor window: Delete/Backspace removes the currently
                // selected node. Has to happen before primary keyboard
                // routing because the editor window has its own focus
                // semantics.
                if is_graph_editor
                    && matches!(
                        logical_key,
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Delete)
                            | winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace)
                    )
                {
                    if let Some(canvas) = self.graph_canvas.as_mut() {
                        canvas.request_delete_selected();
                    }
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    return;
                }
                // Editor window: Cmd+Z / Cmd+Shift+Z route to the
                // content thread's undo stack so graph edits can be
                // reverted while the editor has focus. (The primary
                // window's InputHandler covers Cmd+Z when its focused
                // — but the editor has its own keyboard routing that
                // returns before InputHandler fires, so we wire the
                // shortcut here too.)
                if is_graph_editor
                    && self.modifiers.command
                    && let winit::keyboard::Key::Character(c) = &logical_key
                    && c.eq_ignore_ascii_case("z")
                {
                    if let Some(tx) = self.content_tx.as_ref() {
                        if self.modifiers.shift {
                            crate::ui_bridge::redo(tx);
                        } else {
                            crate::ui_bridge::undo(tx);
                        }
                    }
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.offscreen_dirty = true;
                    }
                    return;
                }
                // Cmd+Shift+G — open the node-graph editor window.
                // App-level shortcut, fires before text input or UI
                // forwarding so it's always available regardless of
                // focus.
                if is_primary
                    && self.modifiers.command
                    && self.modifiers.shift
                    && let winit::keyboard::Key::Character(c) = &logical_key
                    && c.eq_ignore_ascii_case("g")
                {
                    self.pending_open_graph_editor = true;
                    return;
                }
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
                                if self.text_input.multiline && self.modifiers.shift {
                                    self.text_input.insert_char('\n');
                                } else {
                                    let (field, text) = self.text_input.commit();
                                    self.handle_text_input_commit(field, &text);
                                }
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
                            Key::Named(NamedKey::Space) => {
                                self.text_input.insert_char(' ');
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
                            _ => {
                                consumed = true;
                            } // Suppress all other keys
                        }
                        // Reactive search: push filter on every keystroke
                        if consumed
                            && self.text_input.field
                                == crate::text_input::TextInputField::SearchFilter
                        {
                            self.ws
                                .ui_root
                                .browser_popup
                                .set_filter(self.text_input.text.trim().to_string());
                            self.needs_rebuild = true;
                        }
                        // Skip normal shortcut processing when text input consumed the key
                        if consumed {
                            return;
                        }
                    }
                    // ── Shortcut dispatch via InputHandler ──
                    // Port of Unity InputHandler.HandleKeyboardInput().
                    // All viewport access goes through the TimelineInputHost trait.
                    if !consumed && let Some(content_tx) = self.content_tx.as_ref() {
                        let mut host = crate::input_host::AppInputHost {
                            project: &mut self.local_project,
                            content_tx,
                            content_state: &self.content_state,
                            ui_root: &mut self.ws.ui_root,
                            selection: &mut self.selection,
                            active_layer: &mut self.active_layer_id,
                            needs_rebuild: &mut self.needs_rebuild,
                            needs_structural_sync: &mut self.needs_structural_sync,
                            scroll_dirty: &mut self.scroll_dirty,
                            current_project_path: &self.current_project_path,
                            has_output_window: self.window_registry.has_output_window(),
                            pending_close_output: &mut self.pending_close_output,
                            pending_export: &mut self.pending_export,
                            effect_clipboard: &mut self.effect_clipboard,
                        };
                        if self.input_handler.handle_keyboard_input(
                            &logical_key,
                            self.modifiers,
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
                            // ── Import Video: Cmd+I ──
                            Key::Character(c) if c.as_str() == "i" && m.is_command_only() => {
                                self.import_video_clip();
                                consumed = true;
                            }
                            // ── New: Cmd+N ──
                            Key::Character(c) if c.as_str() == "n" && m.is_command_only() => {
                                let project = Self::create_default_project();
                                self.local_project = project.clone();
                                self.suppress_snapshot_until = self.content_state.data_version + 1;
                                self.suppress_snapshot_set_at = self.frame_count;
                                self.send_content_cmd(ContentCommand::LoadProject(Box::new(
                                    project,
                                )));
                                self.send_content_cmd(ContentCommand::SetProject);
                                self.selection.clear_selection();
                                self.active_layer_id = self
                                    .local_project
                                    .timeline
                                    .layers
                                    .first()
                                    .map(|l| l.layer_id.clone());
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
                if is_primary
                    && !consumed
                    && let Some(ui_key) = Self::convert_key(&logical_key)
                {
                    self.ws.ui_root.key_event(ui_key, self.modifiers);
                }

                // Output window: Escape no longer closes it — during a live
                // show an accidental Escape would black out the audience.
                // Close via the UI Monitor button instead.
            }

            // ── Cursor left window → cancel in-progress drags ────────
            WindowEvent::CursorLeft { .. } => {
                if is_primary && self.perform_handle_cursor_left() {
                    return;
                }
                if is_primary {
                    if self.mouse_pressed {
                        log::debug!("Cursor left window — synthesizing PointerUp to cancel drag");
                        self.ws.ui_root.pointer_event(
                            self.cursor_pos,
                            PointerAction::Up,
                            self.time_since_start,
                        );
                        self.mouse_pressed = false;
                        if self.ws.ui_root.inspector_resize_dragging {
                            self.ws.ui_root.end_inspector_resize();
                        }
                    }
                    // Clear clip hover so bitmap doesn't stay painted in hover state
                    if self.selection.hovered_clip_id.is_some() {
                        self.selection.hovered_clip_id = None;
                        self.scroll_dirty.visual = true;
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
                    self.ws.ui_root.pointer_event(
                        self.cursor_pos,
                        PointerAction::Up,
                        self.time_since_start,
                    );
                    self.mouse_pressed = false;
                    if self.ws.ui_root.inspector_resize_dragging {
                        self.ws.ui_root.end_inspector_resize();
                    }
                    // Clear clip hover so bitmap doesn't stay painted in hover state
                    if self.selection.hovered_clip_id.is_some() {
                        self.selection.hovered_clip_id = None;
                        self.scroll_dirty.visual = true;
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
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                if crate::project_io::is_supported_video_extension(&path) {
                    // Video files → shared import path (same as Cmd+I)
                    self.import_video_files(std::slice::from_ref(&path));
                } else if crate::project_io::is_supported_midi_extension(&path) {
                    // MIDI files → route through ProjectIOService
                    let drop_beat = self.content_state.current_beat.as_f32();
                    let drop_layer = self
                        .active_layer_id
                        .as_ref()
                        .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id))
                        .unwrap_or(0) as i32;
                    let spb = manifold_core::tempo::TempoMapConverter::seconds_per_beat_from_bpm(
                        self.local_project.settings.bpm.0,
                    );
                    let action = self.project_io.process_dropped_files(
                        std::slice::from_ref(&path),
                        drop_beat,
                        drop_layer,
                        &mut self.local_project,
                        spb,
                    );
                    self.apply_project_io_action(action);
                } else if ext == "json" || ext == "manifold" {
                    // Project files → load project
                    self.open_project_from_path(path.clone());
                } else if matches!(ext.as_str(), "wav" | "mp3" | "flac" | "aiff" | "ogg") {
                    log::info!(
                        "Audio file dropped: {} (audio import not yet implemented)",
                        path.to_string_lossy()
                    );
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
        if !self.initialized || self.shutting_down {
            return;
        }

        // Deferred output window toggle (needs ActiveEventLoop).
        // Close output window (Escape key or programmatic close)
        if self.pending_close_output {
            self.pending_close_output = false;
            #[cfg(target_os = "macos")]
            {
                self.send_content_cmd(crate::content_command::ContentCommand::ClearOutputSurface);
                self.output_saved_frame = None;
            }
            let output_ids: Vec<_> = self
                .window_registry
                .iter()
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
                self.open_output_window(event_loop, "Output", None, false);
            }
        }

        // Open graph editor window (Cmd+Shift+G).
        if self.pending_open_graph_editor {
            self.pending_open_graph_editor = false;
            self.open_graph_editor(event_loop);
        }

        // Performance mode entry/exit (see crate::perform_mode::lifecycle).
        self.handle_perform_mode_pending(event_loop);

        // Check if a screen change notification fired — update EDR headroom
        // per-window and retarget CVDisplayLinks.
        if crate::edr_surface::edr_screen_changed() {
            let any_display_changed = self.update_edr_headroom();
            if any_display_changed {
                // A display link was retargeted to a new display. Skip all
                // potentially-blocking surface operations (next_drawable,
                // commit_and_wait_scheduled) until the display link confirms
                // it's alive on the new display. This prevents hard locks when
                // GPU surfaces target stale displays (e.g., MacBook → 4K TV).
                self.display_retarget_pending = true;
                self.display_retarget_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                self.send_content_cmd(ContentCommand::SetOutputPresentSuspended(true));
                log::info!(
                    "[Display] Retarget in flight — suspending surface ops \
                     until display link confirms"
                );
            }
        }

        // Event-driven transition exit: resume as soon as the UiDisplayLink
        // callback fires on the (potentially new) display, confirming it's alive.
        // Safety net: if the display link never fires (display disconnected
        // entirely), the 2s deadline clears the flag so we don't freeze forever.
        #[cfg(target_os = "macos")]
        if self.display_retarget_pending {
            let link_alive = self
                .ws
                .ui_display_link
                .as_ref()
                .is_some_and(|dl| dl.is_alive());
            let deadline_expired = self
                .display_retarget_deadline
                .is_some_and(|d| std::time::Instant::now() >= d);
            if link_alive || deadline_expired {
                log::info!(
                    "[Display] Retarget confirmed (link_alive={link_alive}, \
                     deadline_expired={deadline_expired}) — resuming surface ops"
                );
                self.display_retarget_pending = false;
                self.display_retarget_deadline = None;
                self.ws.offscreen_dirty = true;
                self.send_content_cmd(ContentCommand::SetOutputPresentSuspended(false));
            }
        }
        let in_display_transition = self.display_retarget_pending;

        // Render on CVDisplayLink vsync signal (macOS) or FrameTimer fallback.
        // CVDisplayLink aligns submission to the MacBook's actual vsync cadence,
        // eliminating event-loop jitter that caused near-miss frame drops.
        // During display transition: fall back to frame timer (display links
        // may be targeting dead displays and not firing).
        #[cfg(target_os = "macos")]
        let should_render = if in_display_transition {
            self.frame_timer.should_tick()
        } else {
            self.ws
                .ui_display_link
                .as_ref()
                .map_or(self.frame_timer.should_tick(), |dl| dl.vsync_ready())
        };
        #[cfg(not(target_os = "macos"))]
        let should_render = self.frame_timer.should_tick();

        if should_render && !in_display_transition {
            self.tick_and_render();
        }

        // Present output frame on the main thread (windowed mode only).
        // Output presentation is handled directly by the content thread's CB.
        // No presenter blit needed — the content thread presents to the
        // output drawable in its own command buffer.

        // Keep the event loop alive. On macOS the CVDisplayLink callback
        // calls request_redraw to wake us at each vsync. On other platforms
        // (or if the display link isn't started yet) we self-wake.
        #[cfg(not(target_os = "macos"))]
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

impl Application {
    /// Re-query EDR headroom for all windows' current screens.
    /// Called when NSNotification fires (window moved between displays
    /// or display parameters changed).
    /// Returns `true` if any CVDisplayLink was retargeted to a new display.
    fn update_edr_headroom(&mut self) -> bool {
        // Query headroom for primary window → drives compositor tonemap.
        if let Some(pid) = self.primary_window_id
            && let Some(ws) = self.window_registry.get(&pid)
        {
            let h = crate::edr_surface::query_window_headroom(&ws.window);
            if (h - self.edr_headroom).abs() > 0.01 {
                log::debug!("[EDR] Primary: {:.2}x → {:.2}x", self.edr_headroom, h);
                self.edr_headroom = h;
                self.send_content_cmd(ContentCommand::UpdateEdrHeadroom(h));
            }
        }

        // Query headroom for output window → update content thread.
        let output_window: Option<Arc<winit::window::Window>> = self
            .window_registry
            .iter()
            .find(|(_, ws)| matches!(ws.role, WindowRole::Output { .. }))
            .map(|(_, ws)| Arc::clone(&ws.window));

        if let Some(ref win) = output_window {
            let h = crate::edr_surface::query_window_headroom(win);
            if (h - self.output_edr_headroom).abs() > 0.01 {
                log::debug!(
                    "[EDR] Output: {:.2}x → {:.2}x (blit={})",
                    self.output_edr_headroom,
                    h,
                    if h > 1.0 {
                        "passthrough"
                    } else {
                        "ACES tonemap"
                    },
                );
                self.output_edr_headroom = h;
                // Update content thread — it owns the output surface and
                // needs the headroom for tonemapping.
                self.send_content_cmd(ContentCommand::UpdateEdrHeadroom(h));
            }
        }

        // Retarget CVDisplayLinks if windows moved to different displays.
        // Same NSNotification triggers this (screen change = display change).
        #[cfg(target_os = "macos")]
        {
            let mut any_changed = false;
            if let Some(pid) = self.primary_window_id
                && let Some(ws) = self.window_registry.get(&pid)
            {
                let win = &ws.window;
                if let Some(dl) = &mut self.ws.ui_display_link {
                    any_changed |= dl.retarget_if_needed(win);
                }
            }
            any_changed
        }
        #[cfg(not(target_os = "macos"))]
        false
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

        // Stop display links before dropping windows — their callbacks
        // call request_redraw() which deadlocks if the main thread is blocked.
        #[cfg(target_os = "macos")]
        {
            self.ws.ui_display_link = None;
        }

        // Drop GPU resources before the device and surfaces.
        // Field drop order is declaration order — gpu (device) drops before
        // window_registry (surfaces) and IOSurface textures, which can crash.
        // Explicitly clear them here so they're gone before implicit field drops.
        #[cfg(target_os = "macos")]
        {
            self.ui_preview_textures = [None, None, None];
        }
        self.layer_bitmap_gpu = None;
        self.ui_renderer = None;
        self.blit_pipeline = None;
        self.blit_sampler = None;
        self.atlas_pipeline = None;
        self.atlas_sampler = None;
        self.ws.ui_offscreen = None;
    }
}

impl Application {
    /// (Re)create the offscreen UI render target at the given surface dimensions.
    /// Called on surface resize / scale factor change.
    pub(crate) fn resize_ui_offscreen(&mut self, width: u32, height: u32) {
        let Some(gpu) = &self.gpu else { return };
        if width == 0 || height == 0 {
            return;
        }
        self.ws.ui_offscreen = Some(gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "UI Offscreen",
            mip_levels: 1,
        }));
    }

    /// (Re)create the graph editor's offscreen render target. Mirrors
    /// `resize_ui_offscreen` but writes to `self.graph_editor.ui_offscreen`.
    /// No-op when the editor isn't open.
    pub(crate) fn resize_graph_editor_offscreen(&mut self, width: u32, height: u32) {
        let Some(gpu) = &self.gpu else { return };
        let Some(ws) = self.graph_editor.as_mut() else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }
        ws.ui_offscreen = Some(gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "Graph Editor Offscreen",
            mip_levels: 1,
        }));
    }
}
