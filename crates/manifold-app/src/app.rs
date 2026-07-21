use manifold_ui::{AudioSetupAction, ModulationAction, ProjectAction};
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Bpm, LayerId};
use manifold_editing::service::EditingService;
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

/// Re-export UIState as the selection state.
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
    // MasterOpacity/LedBrightness/LayerOpacity/Macro/Param migrated to the
    // unified scrub gesture (`ui_bridge::scrub::ResolvedScrub`, P-I / D4).
    // Trim (modulation trim-range handle trio, driver / audio-mod / Ableton,
    // BUG-246) migrated to the unified scrub gesture
    // (`ui_bridge::scrub::ResolvedScrub::Trim`, P-I / D4).
    // AudioGain (layer-header audio-gain trio) migrated to the unified scrub
    // gesture (`ui_bridge::scrub::ResolvedScrub::LayerAudioGain`, P-I / D4).
    // EnvelopeTarget (orange handle / `Target*` trio) and EnvelopeDecay
    // (`EnvDecay*` trio) migrated to the unified scrub gesture
    // (`ui_bridge::scrub::ResolvedScrub::{EnvelopeTarget,EnvDecay}`, P-I / D4).
    // AudioModShape (drawer shaping-slider `AudioModShape*` trio) migrated to the
    // unified scrub gesture (`ui_bridge::scrub::ResolvedScrub::AudioModShape`,
    // P-I / D4) — whole-shape restore preserved.
    // AudioModStepAmount (drawer Step-amount slider `AudioModStepAmount*` trio)
    // migrated to the unified scrub gesture
    // (`ui_bridge::scrub::ResolvedScrub::AudioModStepAmount`, P-I / D4) — whole
    // `TriggerAction` baseline, wrap-preserving restore.
    // AudioTriggerShape (layer clip-trigger shaping-slider `AudioTriggerShape*`
    // trio) migrated to the unified scrub gesture
    // (`ui_bridge::scrub::ResolvedScrub::AudioTriggerShape`, P-I / D4) —
    // whole-shape restore preserved.
    /// An Audio Setup send-gain label drag (`AudioSendGainDrag*` trio).
    AudioSendGain {
        send_id: manifold_core::AudioSendId,
        db: f32,
    },
    /// An Audio Setup band-divider drag (`AudioCrossover*` trio).
    AudioCrossover {
        low_hz: f32,
        mid_hz: f32,
    },
    // RelightParam (3D-Shading knob trio) migrated to the unified scrub gesture
    // (`ui_bridge::scrub::ResolvedScrub::RelightParam`, P-I / D4).
    /// An Ableton macro trim-bar drag (`AbletonMacroTrim*` trio).
    AbletonMacroTrim {
        slot_idx: usize,
        min: f32,
        max: f32,
    },
    /// A graph-editor mapping-sidebar range drag (`EffectMappingRange*` trio,
    /// BUG-262). Unlike the cluster-C families this one dispatches through
    /// `app_render`'s `pending_actions` loop, not the inspector: the commit
    /// reads the new range back via `watched_reshape`, so an unguarded
    /// mid-drag snapshot swap reverts the def and the commit sees old == new —
    /// no undo entry. Restores the in-flight range through the same
    /// `build_mapping_command` write `preview_mapping` lands each tick.
    MappingRange {
        target: manifold_core::GraphTarget,
        param_id: String,
        min: f32,
        max: f32,
    },
    /// A graph-editor mapping-sidebar affine (scale/offset) drag
    /// (`EffectMappingAffine*` trio, BUG-262). Same restore path as
    /// `MappingRange`, writing the binding's scale/offset.
    MappingAffine {
        target: manifold_core::GraphTarget,
        param_id: String,
        scale: f32,
        offset: f32,
    },
    /// A timeline-marker drag (BUG-280). Marker drag is driven by
    /// `ViewportDrag::MarkerDrag`, outside `InteractionOverlay`'s `DragMode`,
    /// so a mid-gesture content-thread snapshot would revert `marker.beat`.
    /// Restores the in-flight beat through the same `timeline` write the live
    /// `MarkerDragMoved` arm uses.
    Marker {
        marker_id: manifold_core::MarkerId,
        beat: f32,
    },
}

impl ActiveInspectorDrag {
    /// Write the dragged value back into the project after snapshot acceptance.
    pub(crate) fn apply(&self, project: &mut manifold_core::project::Project) {
        match self {
            // Every restore below writes through the SAME store the family's
            // live `*Changed` arm writes, so a mid-drag snapshot swap can't
            // revert the in-flight value (undo audit 2026-07-19, cluster C).
            Self::AudioSendGain { send_id, db } => {
                if let Some(s) = project.audio_setup.find_send_mut(send_id) {
                    s.gain_db = *db;
                }
            }
            Self::AudioCrossover { low_hz, mid_hz } => {
                project.audio_setup.low_hz = *low_hz;
                project.audio_setup.mid_hz = *mid_hz;
            }
            Self::AbletonMacroTrim { slot_idx, min, max } => {
                if let Some(m) = project
                    .settings
                    .macro_bank
                    .slots
                    .get_mut(*slot_idx)
                    .and_then(|s| s.ableton_mapping.as_mut())
                {
                    m.range_min = *min;
                    m.range_max = *max;
                }
            }
            // The two mapping families restore through the SAME command
            // `preview_mapping` executes each `*Changed` tick — build the
            // reshape edit and run it on the project so a mid-drag snapshot
            // swap can't revert the def value the commit reads back via
            // `watched_reshape` (BUG-262, undo audit 2026-07-19 cluster C).
            Self::MappingRange {
                target,
                param_id,
                min,
                max,
            } => {
                let seed = crate::app_render::seed_def_for_project(project, target);
                let edit = manifold_editing::commands::effects::BindingMappingEdit {
                    min: Some(*min),
                    max: Some(*max),
                    ..Default::default()
                };
                crate::app_render::build_mapping_command(target, param_id, edit, seed)
                    .execute(project);
            }
            Self::MappingAffine {
                target,
                param_id,
                scale,
                offset,
            } => {
                let seed = crate::app_render::seed_def_for_project(project, target);
                let edit = manifold_editing::commands::effects::BindingMappingEdit {
                    scale: Some(*scale),
                    offset: Some(*offset),
                    ..Default::default()
                };
                crate::app_render::build_mapping_command(target, param_id, edit, seed)
                    .execute(project);
            }
            Self::Marker { marker_id, beat } => {
                if let Some(marker) = project.timeline.find_marker_mut(marker_id) {
                    marker.beat = manifold_core::Beats::from_f32(*beat);
                }
                project.timeline.sort_markers();
            }
        }
    }
}

pub struct Application {
    // GPU
    pub(crate) gpu: Option<GpuContext>,

    /// Lazily-created fallback GPU device for import-graph validation
    /// (`import_model_file`'s trial `PresetRuntime` build) when `gpu` is
    /// still `None` — i.e. a model is dropped before `resumed()` has run.
    /// IMPORT_RESPONSIVENESS_DESIGN.md D2/P2: validation always reuses ONE
    /// device, never a fresh `GpuDevice::new()` per import; when the app's
    /// real device already exists (the normal case) that one is used
    /// instead and this field stays `None` for the process's lifetime.
    pub(crate) import_validation_device: Option<std::sync::Arc<manifold_gpu::GpuDevice>>,

    // Windows
    pub(crate) window_registry: WindowRegistry,
    pub(crate) primary_window_id: Option<WindowId>,

    /// Native menu bar (File / Edit / View …). Built once in `resumed`; kept
    /// alive here for the process lifetime. `None` until the window is up.
    pub(crate) app_menu: Option<crate::menu::AppMenu>,
    /// Set when `MANIFOLD ▸ Settings…` fires; consumed by the UI to open the
    /// floating settings popup.
    pub(crate) pending_open_settings: bool,

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

    /// Debounced background autosave (GIG_RESILIENCE_DESIGN §6). Ticked from
    /// `tick_and_render` after the state drain, editor mode only — perform
    /// mode returns before the tick, which parks the timer (D5).
    pub(crate) autosave: crate::autosave::AutosaveState,
    /// Set at startup when the previous session exited uncleanly (sentinel
    /// left behind — see `main.rs`). Consumed once to show the crash notice
    /// on an early editor frame.
    pub(crate) show_crash_notice: bool,

    /// Completion channel for the in-flight background video-import probe
    /// (BUG-133): `import_video_files`' worker thread sends the probe-failure
    /// messages (codec/container the decoder couldn't open — e.g. `.webm`
    /// without VP8/VP9, patchy `.avi`) once it finishes the batch. Polled by
    /// `tick_import_failures` (same drain site as `tick_autosave`) and
    /// surfaced via the existing `alerts::error` blocking-dialog path —
    /// never a log-only failure. `None` while no import is in flight.
    pub(crate) import_failures_rx: Option<std::sync::mpsc::Receiver<Vec<String>>>,

    /// IMPORT_RESPONSIVENESS_DESIGN.md D3: the background model-import
    /// worker's progress channel — one long-lived pair for the process's
    /// lifetime (unlike `import_failures_rx`, which is created fresh per
    /// batch), since imports queue rather than run in parallel (see
    /// `import_queue` below). Drained once per frame by
    /// `drain_import_progress` (`app_render.rs`, same site as
    /// `tick_import_failures`).
    pub(crate) import_progress_tx: crossbeam_channel::Sender<crate::import_worker::ImportProgress>,
    pub(crate) import_progress_rx: crossbeam_channel::Receiver<crate::import_worker::ImportProgress>,
    /// True while a background import worker thread is in flight — gates
    /// whether `import_model_file` spawns immediately or enqueues.
    pub(crate) import_worker_busy: bool,
    /// FIFO queue of drops that arrived while a worker was already running
    /// (D3: "concurrent drops queue... one worker at a time"). Drained by
    /// `drain_import_progress` immediately after each `Done`/`Failed`.
    pub(crate) import_queue: std::collections::VecDeque<crate::import_worker::ImportRequest>,

    /// Gig-resilience breadcrumb sidecar (GIG_RESILIENCE_DESIGN §5.1), phase
    /// P2. Cadence gate — pure logic, ticked from the content-state drain in
    /// both editor and perform mode (unlike autosave, NOT parked in perform
    /// mode: the breadcrumb is exactly what a live show needs).
    pub(crate) breadcrumb_cadence: crate::breadcrumb::BreadcrumbCadence,
    /// Background breadcrumb writer thread handle. `None` only if the thread
    /// failed to spawn (degrades to "no breadcrumb this session," never a
    /// crash).
    pub(crate) breadcrumb_writer: Option<crate::breadcrumb::BreadcrumbWriter>,
    /// Set once by `--resume` CLI parsing in `main.rs`, consumed by
    /// `Application::resumed()` after the content thread + GPU are up.
    pub(crate) resume_breadcrumb_path: Option<std::path::PathBuf>,
    /// Set by `Application::boot_resume`, consumed by perform-mode entry
    /// (`perform_mode/lifecycle.rs`) to pick the output window's display.
    pub(crate) pending_resume: Option<crate::breadcrumb::PendingResume>,

    // Selection
    pub(crate) selection: SelectionState,
    pub(crate) active_layer_id: Option<LayerId>,
    /// In-flight scrub-gesture snapshots (eight slider/trim/audio snapshots +
    /// `active_inspector_drag`), regrouped off the field list into one struct
    /// (UI_FUNNEL_DECOMPOSITION P-B, D3; `ui_bridge::scrub::ScrubState`). Threaded
    /// into `dispatch` as `ctx.scrub`. Interim shape — P-I replaces it with the
    /// addressed gesture engine.
    pub(crate) scrub: crate::ui_bridge::ScrubState,
    /// User param-binding mapping range drag snapshot `(min, max)` for
    /// undo. Captured on `EffectMappingRangeSnapshot`, committed as one
    /// `EditUserParamBindingCommand` on `EffectMappingRangeCommit`.
    pub(crate) mapping_range_snapshot: Option<(f32, f32)>,
    /// User param-binding scale/offset drag snapshot `(scale, offset)` for
    /// undo. Captured on `EffectMappingAffineSnapshot`, committed as one
    /// `EditUserParamBindingCommand` on `EffectMappingAffineCommit`.
    pub(crate) mapping_affine_snapshot: Option<(f32, f32)>,

    /// A node-face scrub session currently rerouted through a card binding's
    /// write-back path (`PARAM_TWO_WAY_BINDING_DESIGN.md` D1). `None` when no
    /// bound-param drag is in flight. Set at the first `SetGraphNodeParam` on
    /// a bound `(node_id, param_name)`; cleared on the matching
    /// `EndGraphNodeParamScrub` (one undo-worthy `ChangeGraphParamCommand`
    /// per whole drag, not one per pointer-move).
    pub(crate) bound_node_param_drag: Option<crate::app_render::BoundNodeParamDrag>,

    /// BUG-282: an ordinary (unbound) node-face param/vec scrub session,
    /// mirroring `bound_node_param_drag` for the un-rerouted path — set at
    /// the first `SetGraphNodeParam`/`SetOuterParam`-family move, cleared on
    /// the matching `EndGraphNodeParamScrub` (one undo-worthy
    /// `SetGraphNodeParamCommand` per whole drag, not one per pointer-move).
    pub(crate) unbound_node_param_drag: Option<crate::app_render::UnboundNodeParamDrag>,

    // Effect clipboard (Unity: static EffectClipboard singleton, Rust: instance)
    pub(crate) effect_clipboard: manifold_editing::clipboard::EffectClipboard,

    /// D4 (docs/TIMELINE_INGEST_DESIGN.md): the general pasteboard's
    /// `changeCount` at the moment the app last copied clips internally.
    /// `None` until the first internal copy — the D4 arbitration treats
    /// that the same as "internal clipboard empty" (Finder file always
    /// wins over a clipboard that was never populated).
    #[cfg(target_os = "macos")]
    pub(crate) internal_clipboard_change_count: Option<i64>,

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
    /// IOSurface bridge for the graph editor's node-output preview pane.
    /// Fixed small size; the content thread downscales the captured node
    /// texture into it and the editor sidebar samples the front buffer.
    #[cfg(target_os = "macos")]
    pub(crate) node_preview_texture_bridge:
        Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// UI-side textures imported from the node-output preview IOSurfaces.
    #[cfg(target_os = "macos")]
    pub(crate) ui_node_preview_textures:
        [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// IOSurface bridge for the per-node thumbnail atlas (one big texture packed
    /// as a cell grid). The editor present pass samples cells onto canvas nodes.
    #[cfg(target_os = "macos")]
    pub(crate) node_atlas_texture_bridge:
        Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// UI-side textures imported from the thumbnail-atlas IOSurfaces.
    #[cfg(target_os = "macos")]
    pub(crate) ui_node_atlas_textures:
        [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// Last atlas visible-node set sent to the content thread — dedups the
    /// `SetNodeAtlasVisible` command so it sends only when the visible scope
    /// (or topology) changes, not every frame. Empty = atlas off / editor closed.
    pub(crate) last_atlas_visible_sent: Vec<manifold_core::NodeId>,
    /// Single shared IOSurface + the UI-side texture imported from it for the
    /// clip-thumbnail atlas (§24 5c, BUG-119: one surface, no rotation — see
    /// `crate::shared_texture::SharedAtlasSurface`).
    #[cfg(target_os = "macos")]
    pub(crate) clip_atlas_surface: Option<Arc<crate::shared_texture::SharedAtlasSurface>>,
    #[cfg(target_os = "macos")]
    pub(crate) ui_clip_atlas_texture: Option<manifold_gpu::GpuTexture>,
    /// Last clip-thumbnail visible set sent — dedups `SetClipAtlasVisible`.
    pub(crate) last_clip_atlas_visible_sent: Vec<manifold_core::ClipId>,
    /// Blits clip-thumbnail atlas cells into clip bodies (§24 5c), 4b′ slot.
    pub(crate) clip_thumb_gpu: Option<manifold_renderer::clip_thumb_gpu::ClipThumbGpu>,
    /// Reused per-frame scratch for the thumbnail quad list — no per-frame heap on
    /// the render hot path.
    pub(crate) clip_thumb_quad_scratch: Vec<manifold_renderer::clip_thumb_gpu::ThumbQuad>,
    pub(crate) blit_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    pub(crate) blit_sampler: Option<manifold_gpu::GpuSampler>,
    /// Built once (construction walks the whole ~185-primitive inventory);
    /// shared by every `ViewportSession::open`/`sync_def` call (P5c,
    /// `docs/REALTIME_3D_DESIGN.md`) so opening/re-syncing the 3D viewport
    /// never rebuilds the registry per frame — `Arc` because
    /// `ViewportSession` doesn't need ownership, just a shared borrow that
    /// outlives any one call.
    pub(crate) primitive_registry: std::sync::Arc<manifold_renderer::node_graph::PrimitiveRegistry>,
    /// Audio Setup spectrogram waterfall renderer + its target texture, created
    /// lazily on the UI device when the scope opens and rebuilt if the column
    /// bin count changes. `spectrogram_num_bins` tracks the built layout.
    pub(crate) spectrogram: Option<manifold_spectral::Spectrogram>,
    /// The scope's render target, wrapped as a `Local` TexturePane so the blit
    /// goes through the unified texture-in-UI path.
    #[cfg(target_os = "macos")]
    pub(crate) spectrogram_pane: Option<crate::texture_pane::TexturePane>,
    pub(crate) spectrogram_num_bins: usize,
    /// VQT columns drained from content snapshots but not yet fed to the
    /// waterfall. Snapshots carry the columns produced since the last snapshot;
    /// accumulating here (rather than reading `content_state.spectrogram_columns`
    /// directly) makes the feed consume-once: columns from every drained snapshot
    /// are kept (the drain loop discards all but the latest snapshot), and the
    /// render path clears this after pushing so no column is ever pushed twice.
    /// Without it the waterfall double-pushes on UI frames with no new snapshot
    /// and drops columns on frames that drain several — visible as juddery,
    /// "jelly" scrolling and smeared startup columns.
    pub(crate) pending_spectrogram_columns: Vec<f32>,
    /// Per-column overlay records staged in lockstep with
    /// `pending_spectrogram_columns` — one [`manifold_spectral::ScopeColumn`]
    /// (centroid traces + onset tick lanes) per column.
    pub(crate) pending_spectrogram_scalars: Vec<manifold_spectral::ScopeColumn>,
    /// Physical-pixel size of the scope render target, tracked so it is rebuilt
    /// when the (resizable) Audio Setup modal changes the on-screen scope size —
    /// keeps the waterfall crisp instead of upscaling a fixed small texture.
    #[cfg(target_os = "macos")]
    pub(crate) spectrogram_tex_dims: (u32, u32),
    /// Last spectrogram-send selection pushed to the content thread, so the
    /// `SetSpectrogramSend` command only fires on change.
    pub(crate) spectrogram_send_sent: Option<manifold_core::AudioSendId>,
    pub(crate) atlas_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    pub(crate) atlas_sampler: Option<manifold_gpu::GpuSampler>,
    /// Samples one atlas cell (UV sub-rect via inline `Bytes` uniform) onto a
    /// node's body in the editor present pass. The per-node thumbnail blit.
    pub(crate) node_thumb_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Linear + clamp-to-edge sampler for the per-node thumbnail blit. The UI
    /// `atlas_sampler` is Nearest (crisp glyphs), which made the thumbnail's
    /// downscaled cell read as hard blocky pixels when upscaled into the node
    /// body — this samples it smoothly instead. Clamp + a half-texel UV inset
    /// (see app_render) keep linear taps from bleeding the neighbouring cell.
    pub(crate) thumb_sampler: Option<manifold_gpu::GpuSampler>,
    pub(crate) ui_renderer: Option<UIRenderer>,
    pub(crate) ui_cache_manager: Option<manifold_renderer::ui_cache_manager::UICacheManager>,
    /// GPU-completion fence shared by every UI immediate-draw vertex ring
    /// (`ui_renderer`, `layer_bitmap_gpu`, `clip_content_gpu`,
    /// `clip_thumb_gpu`) — gates ring-slot reuse against in-flight command
    /// buffers so a GPU backlog (e.g. heavy GLB scene renders) can't let a
    /// later frame overwrite a slot an earlier, still-queued frame is about
    /// to read. `advance()`d once per frame in `tick_and_render`, committed
    /// (marking retirement) right after `present_all_windows`.
    pub(crate) ui_frame_fence: Option<Arc<manifold_gpu::FrameFence>>,
    /// Count of admission-control redraw skips (`present_all_windows`
    /// re-presenting the cached offscreen instead of encoding a new frame
    /// because `ui_frame_fence.lag()` is too high). Rate-limits the
    /// `[frame-fence]` skip log the same way ring-owner stall logging does.
    pub(crate) ui_frame_fence_skip_events: u64,
    /// Per-layer grid bitmaps (grid lines + top separator) plus the lane / stem /
    /// overview / collapsed-group bitmaps — all drawn around the GPU clip passes.
    /// The grid (per-layer indices) draws BEFORE the clips so opaque bodies occlude
    /// it; the panels (1000/1001/1002/2000+) are separate regions whose z-order vs
    /// clips is moot. One instance since §24 5b retired the per-layer "front" buffer.
    pub(crate) layer_bitmap_gpu: Option<manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu>,
    /// Per-clip waveform textures, drawn INSIDE the audio-clip bodies after the
    /// body pass (§24 5b) — the waveform is part of the clip on the GPU, no longer
    /// a layer-wide CPU bitmap laid over the bodies.
    pub(crate) clip_content_gpu: Option<manifold_renderer::clip_content_gpu::ClipContentGpu>,
    /// Reused per-frame scratch for the GPU clip pass — visible clip rects from
    /// the viewport, and the resolved draw list. Kept on the struct so the clip
    /// pass allocates nothing on the render hot path.
    pub(crate) clip_rect_scratch: Vec<manifold_ui::panels::viewport::ClipScreenRect>,
    pub(crate) clip_body_scratch: Vec<manifold_renderer::clip_draw::ClipBody>,
    /// Reused scratch for the per-frame timeline-marker overlay lines (beat,
    /// colour). Filled by `viewport::timeline_overlays`; keeps the overlay pass
    /// allocation-free on the render hot path.
    pub(crate) timeline_marker_scratch: Vec<(f32, manifold_ui::node::Color32)>,
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
    /// Cached UI-local translation of `content_state.active_graph_snapshot`
    /// (Phase 8: the canvas reads `manifold_ui::graph_view`, so the app
    /// translates the renderer snapshot at the boundary). Re-derived only when
    /// the source `Arc` changes (`Arc::ptr_eq`), so an unchanged frame pays
    /// nothing; the effect path mints a fresh snapshot each frame and so
    /// re-translates, matching the canvas's own per-frame `set_snapshot`. Holds
    /// the source `Arc` alongside the translation purely for that identity check.
    pub(crate) editor_ui_graph: Option<(
        std::sync::Arc<manifold_renderer::node_graph::GraphSnapshot>,
        std::sync::Arc<manifold_ui::graph_view::GraphSnapshot>,
    )>,
    /// Right-sidebar checkbox panel for V2 user-exposed parameters.
    /// Shares the editor window with `graph_canvas`.
    pub(crate) graph_editor_panel: manifold_ui::panels::graph_editor::GraphEditorPanel,
    /// UI-side mirror of the node-output preview's auto-gain toggle. On by
    /// default; the editor's preview pane flips it and sends
    /// `ContentCommand::SetNodePreviewNormalize` to the content thread. Drives
    /// the toggle checkmark each `configure`.
    pub(crate) node_preview_normalize: bool,
    // The editor's right lane is now the WHOLE inspector column
    // (`Workspace.ui_root.inspector`), not a single watched card. See
    // `docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` (Change 3). The former
    // `editor_card` / `editor_card_intents` / `editor_card_config_hash` fields
    // were deleted with that swap.
    /// Node-intent dispatch for the editor's right-sidebar inspector
    /// (`graph_editor_panel`) — typed to its own `GraphEditCommand` vocabulary
    /// (the generic `IntentRegistry<A>` from Phase 6.1). Discrete sidebar clicks
    /// fold through this registry exactly like every chrome panel, replacing the
    /// panel's old per-row click loop (Phase 6.2). Repopulated from the panel's
    /// after the on-node-params migration this registry carries a single intent —
    /// the node-output "Smart preview" toggle — resolved each editor frame that
    /// has events. Every param control now lives on the node face and dispatches
    /// through the canvas.
    pub(crate) editor_sidebar_intents:
        manifold_ui::intent::IntentRegistry<manifold_ui::GraphEditCommand>,
    /// Tree id of the "Smart preview" toggle button captured during the last
    /// editor render (the button is drawn in the left preview pane by
    /// `GraphEditorPanel::render_smart_preview_toggle`). The input pass registers
    /// the toggle's `SetNodePreviewNormalize` intent on it. `None` when the toggle
    /// wasn't drawn (a non-image node fills the pane with its value inspector).
    pub(crate) editor_smart_preview_toggle_id: Option<manifold_ui::node::NodeId>,
    /// Sideways mapping drawer for the editor card's Author-context rows. Same
    /// `MappingPopover` the canvas uses for on-node rows, but anchored beside the
    /// left-lane card row and opened by its right-edge chevron. Edits emit the
    /// existing `EffectMapping*` actions, dispatched against the editor's
    /// `watched_graph_target` (by effect id) like the canvas popover.
    pub(crate) editor_mapping_popover: crate::mapping_popover::MappingPopover,
    /// Graph-editor node clipboard: the copied nodes + the wires among them,
    /// captured at the source scope. Cmd+C fills it, Cmd+V pastes it (offset,
    /// fresh ids). `None` until the first copy. Plain owned data — no identity
    /// tie to the source, so it survives edits to the original.
    pub(crate) graph_node_clipboard: Option<(
        Vec<manifold_core::effect_graph_def::EffectGraphNode>,
        Vec<manifold_core::effect_graph_def::EffectGraphWire>,
    )>,
    /// Built-once list of atoms shown in the spawn popup (node browser).
    /// The palette column is gone; this still feeds the popup's Node mode.
    pub(crate) palette_atoms_cache: Vec<manifold_ui::panels::graph_palette::GraphPaletteAtom>,
    /// Last node-preview selection sent to the content thread, to suppress
    /// duplicate `SetGraphPreviewNode` commands each frame. `None` = nothing
    /// previewed (editor closed or multi/zero selection).
    pub(crate) last_preview_node: Option<manifold_core::NodeId>,
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
    /// Per-frame UI profiler (main-window breakdown). No-op unless
    /// `MANIFOLD_UI_FRAME_PROFILE=1`. See `ui_frame_profile`.
    pub(crate) ui_profile: crate::ui_frame_profile::UiFrameProfile,
    /// Cached transport display strings (avoids per-frame format! allocations).
    pub(crate) transport_cache: crate::ui_bridge::TransportDisplayCache,

    // Input state for winit → UIInputSystem translation
    pub(crate) cursor_pos: Vec2,
    pub(crate) mouse_pressed: bool,
    pub(crate) modifiers: Modifiers,
    pub(crate) time_since_start: f32,

    // Finder file-drag hover (BUG-028): the live pointer position during an
    // OS drag, read via drag_interpose — see that module for why cursor_pos
    // alone can't answer "where is the file being dropped."
    pub(crate) drag_tracker: crate::drag_hover::DragHoverTracker,

    // Cursor feedback — tracks current cursor shape for interaction hints.
    // From Unity Cursors.cs: SetMove, SetBlocked, SetResizeHorizontal, SetDefault.
    pub(crate) cursor_manager: CursorManager,

    // Video/timeline split handle drag state.
    // From Unity PanelResizeHandle.cs — drag to resize video vs timeline proportion.
    pub(crate) split_dragging: bool,
    pub(crate) split_was_hovered: bool,
    /// P2 "panel-split snap-back" (D15): double-click-to-default timers for
    /// the two draggable splits, mirroring `output_last_click`'s pattern
    /// below — both handles are checked (and their drags started) BEFORE
    /// the press ever reaches the generic `UIEvent`/`Gesture::DoubleClick`
    /// pipeline (see `primary_mouse_input`), so a plain timestamp check here
    /// is the same shape as the output window's raw double-click, not a
    /// new mechanism.
    pub(crate) split_handle_last_click: Option<std::time::Instant>,
    pub(crate) inspector_handle_last_click: Option<std::time::Instant>,
    /// Double-click detect for the Audio Setup dock resize handle (D1) —
    /// double-click snaps the width back to its default, same shape as the
    /// inspector/split handles above.
    pub(crate) audio_setup_handle_last_click: Option<std::time::Instant>,
    /// Double-click detect for the Scene Setup dock resize handle — mirror
    /// of `audio_setup_handle_last_click` (SCENE_SETUP_PANEL_DESIGN D2).
    pub(crate) scene_setup_handle_last_click: Option<std::time::Instant>,

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
        let (import_progress_tx, import_progress_rx) = crossbeam_channel::unbounded();

        Self {
            gpu: None,
            import_validation_device: None,
            import_progress_tx,
            import_progress_rx,
            import_worker_busy: false,
            import_queue: std::collections::VecDeque::new(),
            window_registry: WindowRegistry::new(),
            primary_window_id: None,
            app_menu: None,
            pending_open_settings: false,
            content_tx: None,
            state_rx: None,
            content_thread_handle: None,
            content_state: ContentState::default(),
            local_project: default_project,
            last_snapshot_arc: None,
            suppress_snapshot_until: 0,
            suppress_snapshot_set_at: 0,
            autosave: crate::autosave::AutosaveState::new(),
            show_crash_notice: false,
            import_failures_rx: None,
            breadcrumb_cadence: crate::breadcrumb::BreadcrumbCadence::new(),
            breadcrumb_writer: Some(crate::breadcrumb::BreadcrumbWriter::spawn()),
            resume_breadcrumb_path: None,
            pending_resume: None,
            selection: UIState::new(),
            active_layer_id: None,
            scrub: crate::ui_bridge::ScrubState::default(),
            mapping_range_snapshot: None,
            mapping_affine_snapshot: None,
            bound_node_param_drag: None,
            unbound_node_param_drag: None,
            effect_clipboard: manifold_editing::clipboard::EffectClipboard::new(),
            #[cfg(target_os = "macos")]
            internal_clipboard_change_count: None,
            content_pipeline_output: None,
            last_preview_node: None,
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
            #[cfg(target_os = "macos")]
            node_preview_texture_bridge: None,
            #[cfg(target_os = "macos")]
            ui_node_preview_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            node_atlas_texture_bridge: None,
            #[cfg(target_os = "macos")]
            ui_node_atlas_textures: [None, None, None],
            last_atlas_visible_sent: Vec::new(),
            #[cfg(target_os = "macos")]
            clip_atlas_surface: None,
            #[cfg(target_os = "macos")]
            ui_clip_atlas_texture: None,
            last_clip_atlas_visible_sent: Vec::new(),
            clip_thumb_gpu: None,
            clip_thumb_quad_scratch: Vec::new(),
            blit_pipeline: None,
            blit_sampler: None,
            primitive_registry: std::sync::Arc::new(
                manifold_renderer::node_graph::PrimitiveRegistry::with_builtin(),
            ),
            spectrogram: None,
            #[cfg(target_os = "macos")]
            spectrogram_pane: None,
            spectrogram_num_bins: 0,
            pending_spectrogram_columns: Vec::new(),
            pending_spectrogram_scalars: Vec::new(),
            #[cfg(target_os = "macos")]
            spectrogram_tex_dims: (0, 0),
            spectrogram_send_sent: None,
            atlas_pipeline: None,
            atlas_sampler: None,
            node_thumb_pipeline: None,
            thumb_sampler: None,
            ui_renderer: None,
            ui_cache_manager: None,
            ui_frame_fence: None,
            ui_frame_fence_skip_events: 0,
            layer_bitmap_gpu: None,
            clip_content_gpu: None,
            clip_rect_scratch: Vec::new(),
            clip_body_scratch: Vec::new(),
            timeline_marker_scratch: Vec::new(),
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
            editor_ui_graph: None,
            graph_editor_panel: manifold_ui::panels::graph_editor::GraphEditorPanel::new(),
            node_preview_normalize: false,
            editor_sidebar_intents: manifold_ui::intent::IntentRegistry::new(),
            editor_smart_preview_toggle_id: None,
            editor_mapping_popover: crate::mapping_popover::MappingPopover::new(),
            graph_node_clipboard: None,
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
            watched_graph_target: None,
            watched_catalog_default: None,
            // UI frame rate: uncapped (120fps target, vsync limits actual present).
            // Content thread has its own timer at project FPS — fully decoupled.
            frame_timer: FrameTimer::new(120.0),
            ui_profile: crate::ui_frame_profile::UiFrameProfile::new(),
            frame_count: 0,
            transport_cache: crate::ui_bridge::TransportDisplayCache::new(),
            cursor_pos: Vec2::ZERO,
            drag_tracker: crate::drag_hover::DragHoverTracker::default(),
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
            split_handle_last_click: None,
            inspector_handle_last_click: None,
            audio_setup_handle_last_click: None,
            scene_setup_handle_last_click: None,
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

    /// The GPU device import-graph validation (`import_model_file`) builds
    /// its trial `PresetRuntime` against. IMPORT_RESPONSIVENESS_DESIGN.md
    /// D2/P2: never a fresh `GpuDevice` per import. Reuses the app's real
    /// UI-side device (`self.gpu`, set by `resumed()`) whenever it exists;
    /// falls back to ONE lazily-created device cached on `self` — created
    /// at most once per process — for the window where a model can be
    /// dropped before `resumed()` has run (proved reachable by BUG-219's
    /// P1 harness).
    pub(crate) fn validation_gpu_device(&mut self) -> std::sync::Arc<manifold_gpu::GpuDevice> {
        match self.gpu.as_ref() {
            Some(ctx) => ctx.device.clone(),
            None => self
                .import_validation_device
                .get_or_insert_with(|| std::sync::Arc::new(manifold_gpu::GpuDevice::new()))
                .clone(),
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
    pub(crate) fn update_cursor_for_position(&mut self) {
        // Priority 1: Active drag — cursor set by InteractionOverlay
        // (overlay calls host.set_cursor() during drag, so we just skip here)
        {
            use manifold_ui::interaction_overlay::DragMode;
            match self.overlay.drag_mode() {
                DragMode::Move
                | DragMode::TrimLeft
                | DragMode::TrimRight
                | DragMode::RegionSelect
                | DragMode::AutomationPoint
                | DragMode::AutomationSegmentBend
                | DragMode::AutomationSegmentDrag
                | DragMode::AutomationMarquee
                | DragMode::AutomationGroupMove
                | DragMode::AutomationDraw => return,
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

        // Priority 2b: Audio Setup dock resize edge hover (D1)
        if self.ws.ui_root.audio_setup_resize_dragging
            || self.ws.ui_root.is_near_audio_setup_edge(self.cursor_pos)
        {
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
            if self.ws.ui_root.audio_setup_resize_dragging {
                self.ws.ui_root.set_audio_setup_handle_drag();
            } else {
                self.ws.ui_root.set_audio_setup_handle_hover();
            }
            return;
        }
        self.ws.ui_root.set_audio_setup_handle_idle();

        // Priority 2c: Scene Setup dock resize edge hover — mirror of Audio
        // Setup above (SCENE_SETUP_PANEL_DESIGN D2).
        if self.ws.ui_root.scene_setup_resize_dragging
            || self.ws.ui_root.is_near_scene_setup_edge(self.cursor_pos)
        {
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
            if self.ws.ui_root.scene_setup_resize_dragging {
                self.ws.ui_root.set_scene_setup_handle_drag();
            } else {
                self.ws.ui_root.set_scene_setup_handle_hover();
            }
            return;
        }
        self.ws.ui_root.set_scene_setup_handle_idle();

        // Priority 2d (UX-P2, D3a of SCENE_PANEL_UX_DESIGN.md) — the Scene
        // Setup drag-armable value-cell cursor lookup — is DELETED
        // (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1d): Modifier, the
        // last family with a bespoke delta-drag value cell, converted onto
        // the card row's own slider track, so `value_cell_at` has no
        // producer left anywhere in the panel.

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

    /// Handle committed text input value.
    pub(crate) fn handle_text_input_commit(
        &mut self,
        field: crate::text_input::TextInputField,
        text: &str,
    ) {
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
            TextInputField::InspectorParam => {
                if let Some(ctx) = self.text_input.inspector_param.take() {
                    // Lenient parse: keep only the numeric head so a value typed
                    // with a unit suffix (e.g. an angle "45°") still commits.
                    let cleaned: String = text
                        .trim()
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
                        .collect();
                    if let Ok(parsed) = cleaned.parse::<f32>() {
                        // PARAM_RANGE_CONTRACT_DESIGN.md D3: `ctx.min`/`ctx.max` are
                        // the param's display hint (default slider travel), not a
                        // restriction — a typed value is free to exceed it, exactly
                        // like a card remap or modulation. No clamp here.
                        let mut v = parsed;
                        if ctx.whole_numbers {
                            v = v.round();
                        }
                        // Mirror the slider drag as one undoable step by driving
                        // the SAME scrub wire (P-I / D4, the D8 direct-set
                        // shape): Begin captures the old base as the undo
                        // baseline, Move sets the new value live, Commit builds
                        // the one undo entry (old base → typed value).
                        let content_tx = self.content_tx.as_ref().unwrap().clone();
                        use manifold_ui::panels::{PanelAction, ScrubPhase, ScrubValue, ValueRef};
                        for act in [
                            PanelAction::Scrub(
                                ValueRef::Param(ctx.target.clone(), ctx.param_id.clone()),
                                ScrubPhase::Begin,
                            ),
                            PanelAction::Scrub(
                                ValueRef::Param(ctx.target.clone(), ctx.param_id.clone()),
                                ScrubPhase::Move(ScrubValue::Scalar(v)),
                            ),
                            PanelAction::Scrub(
                                ValueRef::Param(ctx.target, ctx.param_id.clone()),
                                ScrubPhase::Commit,
                            ),
                        ] {
                            let mut dctx = crate::ui_bridge::DispatchCtx {
                                project: &mut self.local_project,
                                content_tx: &content_tx,
                                content_state: &self.content_state,
                                ui: &mut self.ws.ui_root,
                                selection: &mut self.selection,
                                active_layer: &mut self.active_layer_id,
                                user_prefs: &mut self.user_prefs,
                                editor_target: None,
                                scrub: &mut self.scrub,
                            };
                            let _ = crate::ui_bridge::dispatch(&act, &mut dctx);
                        }
                        self.needs_rebuild = true;
                    }
                }
            }
            TextInputField::SceneNumericParam(node_doc_id) => {
                // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P4, D8/D10. Lenient
                // parse (same convention as InspectorParam): keep only the
                // numeric head so a value typed with a trailing unit (e.g.
                // "45°") still commits. NO clamp — PARAM_RANGE_CONTRACT P1.
                if let Some(ctx) = self.text_input.scene_numeric_param.take()
                    && let Some(parsed) = crate::text_input::parse_lenient_numeric(text)
                {
                    // D10: the panel boundary is the ONLY place a
                    // degrees-typed value converts to the radians the
                    // graph stores.
                    let value = crate::text_input::scene_numeric_commit_value(parsed, ctx.degrees);
                    let content_tx = self.content_tx.as_ref().unwrap().clone();
                    use manifold_ui::panels::PanelAction;
                    // ONE dispatch = ONE undo unit — the exact write the
                    // dock's own drag/steppers already use
                    // (`SceneSetupParamChanged`), so type-in is not a
                    // second mutation path.
                    let act = PanelAction::Project(ProjectAction::SceneSetupParamChanged(
                        ctx.layer_id,
                        ctx.scope_path,
                        node_doc_id,
                        ctx.param_id,
                        value,
                    ));
                    let mut dctx = crate::ui_bridge::DispatchCtx {
                        project: &mut self.local_project,
                        content_tx: &content_tx,
                        content_state: &self.content_state,
                        ui: &mut self.ws.ui_root,
                        selection: &mut self.selection,
                        active_layer: &mut self.active_layer_id,
                        user_prefs: &mut self.user_prefs,
                        editor_target: None,
                        scrub: &mut self.scrub,
                    };
                    let _ = crate::ui_bridge::dispatch(&act, &mut dctx);
                    self.needs_rebuild = true;
                }
            }
            TextInputField::AudioSendGainParam => {
                // P4 audio-dock sibling — same lenient-parse, no-clamp,
                // one-dispatch shape as `SceneNumericParam` above.
                if let Some(ctx) = self.text_input.audio_send_gain_param.take()
                    && let Some(parsed) = crate::text_input::parse_lenient_numeric(text)
                {
                    let content_tx = self.content_tx.as_ref().unwrap().clone();
                    use manifold_ui::panels::PanelAction;
                    let act = PanelAction::AudioSetup(AudioSetupAction::AudioSendGainSetTyped(ctx.send_id, parsed));
                    let mut dctx = crate::ui_bridge::DispatchCtx {
                        project: &mut self.local_project,
                        content_tx: &content_tx,
                        content_state: &self.content_state,
                        ui: &mut self.ws.ui_root,
                        selection: &mut self.selection,
                        active_layer: &mut self.active_layer_id,
                        user_prefs: &mut self.user_prefs,
                        editor_target: None,
                        scrub: &mut self.scrub,
                    };
                    let _ = crate::ui_bridge::dispatch(&act, &mut dctx);
                    self.needs_rebuild = true;
                }
            }
            TextInputField::DriverFreePeriod => {
                if let Some(ctx) = self.text_input.driver_free_period.take() {
                    // Lenient parse: keep the numeric head (so "3 b" still works).
                    let cleaned: String = text
                        .trim()
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
                        .collect();
                    // Only a positive period is meaningful; a zero/garbage entry
                    // leaves the driver in its prior mode (no command issued).
                    if let Ok(parsed) = cleaned.parse::<f32>()
                        && parsed > 0.0
                    {
                        let content_tx = self.content_tx.as_ref().unwrap().clone();
                        use manifold_ui::panels::{DriverConfigAction, PanelAction};
                        let act = PanelAction::Modulation(ModulationAction::DriverConfig(
                            ctx.target,
                            ctx.param_id.clone(),
                            DriverConfigAction::SetFreePeriod(parsed),
                        ));
                        let mut dctx = crate::ui_bridge::DispatchCtx {
                            project: &mut self.local_project,
                            content_tx: &content_tx,
                            content_state: &self.content_state,
                            ui: &mut self.ws.ui_root,
                            selection: &mut self.selection,
                            active_layer: &mut self.active_layer_id,
                            user_prefs: &mut self.user_prefs,
                            editor_target: None,
                            scrub: &mut self.scrub,
                        };
                        let _ = crate::ui_bridge::dispatch(&act, &mut dctx);
                        self.needs_rebuild = true;
                    }
                }
            }
            TextInputField::LayerName => {
                // Resolved by id, not a baked-in index (BUG-031): a layer-list
                // change between the double-click and this commit can't rename
                // the wrong row.
                if let Some(layer_id) = self.text_input.layer_id.take()
                    && let Some((_, layer)) =
                        self.local_project.timeline.find_layer_by_id(layer_id.as_str())
                {
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
                    // Resolve effect instance and read its param (by card
                    // index) ONCE from the live manifest — both the clamp
                    // range and the stable id come off the same lookup, no
                    // registry consultation (P5 registry containment).
                    let effect_info = match tab {
                        manifold_ui::InspectorTab::Master => self
                            .local_project
                            .settings
                            .master_effects
                            .get(effect_idx)
                            .map(|fx| {
                                let param = fx.params.iter().nth(param_idx);
                                let old = param
                                    .map(|p| if fx.base_tracked { p.base } else { p.value })
                                    .unwrap_or(0.0);
                                // PARAM_RANGE_CONTRACT_DESIGN.md D3: `p.spec.min`/`max`
                                // are a display hint, not a restriction — a typed
                                // value is free to exceed it. No clamp here.
                                let new_val = parsed;
                                let param_id = param.map(|p| p.id().to_string());
                                (fx.id.clone(), old, new_val, param_id)
                            }),
                        manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Group => self
                            .active_layer_id
                            .as_ref()
                            .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                            .and_then(|(_, l)| l.effects.as_ref())
                            .and_then(|e| e.get(effect_idx))
                            .map(|fx| {
                                let param = fx.params.iter().nth(param_idx);
                                let old = param
                                    .map(|p| if fx.base_tracked { p.base } else { p.value })
                                    .unwrap_or(0.0);
                                // PARAM_RANGE_CONTRACT_DESIGN.md D3: `p.spec.min`/`max`
                                // are a display hint, not a restriction — a typed
                                // value is free to exceed it. No clamp here.
                                let new_val = parsed;
                                let param_id = param.map(|p| p.id().to_string());
                                (fx.id.clone(), old, new_val, param_id)
                            }),
                        manifold_ui::InspectorTab::Clip => self
                            .selection
                            .primary_selected_clip_id
                            .as_ref()
                            .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                            .and_then(|c| c.effects.get(effect_idx))
                            .map(|fx| {
                                let param = fx.params.iter().nth(param_idx);
                                let old = param
                                    .map(|p| if fx.base_tracked { p.base } else { p.value })
                                    .unwrap_or(0.0);
                                // PARAM_RANGE_CONTRACT_DESIGN.md D3: `p.spec.min`/`max`
                                // are a display hint, not a restriction — a typed
                                // value is free to exceed it. No clamp here.
                                let new_val = parsed;
                                let param_id = param.map(|p| p.id().to_string());
                                (fx.id.clone(), old, new_val, param_id)
                            }),
                    };
                    if let Some((effect_id, old_val, new_val, param_id)) = effect_info
                        && (old_val - new_val).abs() > f32::EPSILON
                        && let Some(param_id) = param_id
                    {
                        let cmd =
                            manifold_editing::commands::effects::ChangeGraphParamCommand::new(
                                manifold_core::GraphTarget::Effect(effect_id),
                                param_id,
                                old_val,
                                new_val,
                            );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
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
                    && let Some(gp) = layer.gen_params()
                {
                    // Read the param (by card index) ONCE from the live
                    // manifest — both the clamp range and the stable id
                    // come off the same lookup, no registry consultation
                    // (P5 registry containment).
                    let param = gp.params.iter().nth(param_idx);
                    let old_val = param
                        .map(|p| if gp.base_tracked { p.base } else { p.value })
                        .unwrap_or(0.0);
                    // PARAM_RANGE_CONTRACT_DESIGN.md D3: `p.spec.min`/`max` are a
                    // display hint, not a restriction — a typed value is free to
                    // exceed it. No clamp here.
                    let new_val = parsed;
                    let param_id = param.map(|p| p.id().to_string());
                    if (old_val - new_val).abs() > f32::EPSILON
                        && let Some(param_id) = param_id
                    {
                        let lid = layer.layer_id.clone();
                        let cmd =
                            manifold_editing::commands::effects::ChangeGraphParamCommand::new(
                                manifold_core::GraphTarget::Generator(lid),
                                param_id,
                                old_val,
                                new_val,
                            );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
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
                        manifold_core::preset_definition_registry::try_get(gen_type)
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
                        manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Group => self
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
                            manifold_ui::InspectorTab::Layer
                            | manifold_ui::InspectorTab::Group
                            | manifold_ui::InspectorTab::Clip => {
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
            TextInputField::AudioSendLabel => {
                if let Some(send_id) = self.text_input.audio_send_id.take() {
                    let new_label = text.trim().to_string();
                    let old_label = self
                        .local_project
                        .audio_setup
                        .find_send(&send_id)
                        .map(|s| s.label.clone())
                        .unwrap_or_default();
                    if !new_label.is_empty() && old_label != new_label {
                        let cmd =
                            manifold_editing::commands::audio_setup::RenameAudioSendCommand::new(
                                send_id, old_label, new_label,
                            );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                }
                self.needs_rebuild = true;
            }
            // ── Graph-editor fields ──────────────────────────────────────
            // The graph canvas renders from `content_state.active_graph_snapshot`
            // (the content thread owns the authoritative graph), so these
            // dispatch to the content thread only — no local_project execute.
            TextInputField::GraphGroupRename(group_node_id) => {
                let new_handle = text.trim().to_string();
                if !new_handle.is_empty()
                    && let (Some(target), Some(default)) = (
                        self.watched_graph_target.clone(),
                        self.watched_catalog_default.clone(),
                    )
                {
                    let scope = self
                        .graph_canvas
                        .as_ref()
                        .map(|c| c.scope_path().to_vec())
                        .unwrap_or_default();
                    let cmd = manifold_editing::commands::graph::RenameGroupCommand::new(
                        target,
                        scope,
                        group_node_id,
                        new_handle,
                        default,
                    );
                    self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                }
            }
            // ── Scene Setup panel (SCENE_SETUP_PANEL_DESIGN.md P2) ───────
            // The panel addresses the layer directly (no graph editor needs
            // to be open — it's a fourth surface, not a canvas view), so this
            // resolves its own catalog default via `generator_catalog_default`
            // instead of `watched_graph_target`/`watched_catalog_default`.
            TextInputField::SceneObjectRename(group_node_id) => {
                // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D6/P3: dispatches
                // RenameSceneObjectCommand now, not RenameGroupCommand — it
                // extends the same walk (group handle + card-section sweep)
                // but ALSO keeps the object's own `node.scene_object` handle
                // in sync (D6's single-writer-of-both posture) and degrades
                // cleanly for an ungrouped hand-built object. `group_node_id`
                // is unchanged from before: `SceneVm`'s
                // `SceneObjectVm::Known::group_node_id` already resolves to
                // the object_k wire's producer post-P1/P2 (D12), so this is
                // exactly the id `RenameSceneObjectCommand` addresses by.
                let new_handle = text.trim().to_string();
                if !new_handle.is_empty()
                    && let Some(layer_id) = self.text_input.scene_object_layer_id.take()
                {
                    if let Some(default) =
                        crate::ui_bridge::generator_catalog_default(&self.local_project, &layer_id)
                    {
                        let target = manifold_core::GraphTarget::Generator(layer_id);
                        let cmd = manifold_editing::commands::graph::RenameSceneObjectCommand::new(
                            target,
                            Vec::new(),
                            group_node_id,
                            new_handle,
                            default,
                        );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                    self.needs_rebuild = true;
                }
            }
            TextInputField::SceneLightRename(light_node_id) => {
                // SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D6/P5: a light's name
                // is a plain `SetNodeHandleCommand` write — no group sweep
                // (a light is never wrapped in a group, unlike an object).
                let new_handle = text.trim().to_string();
                if !new_handle.is_empty()
                    && let Some(layer_id) = self.text_input.scene_object_layer_id.take()
                {
                    if let Some(default) =
                        crate::ui_bridge::generator_catalog_default(&self.local_project, &layer_id)
                    {
                        let target = manifold_core::GraphTarget::Generator(layer_id);
                        let cmd = manifold_editing::commands::graph::SetNodeHandleCommand::new(
                            target,
                            Vec::new(),
                            light_node_id,
                            new_handle,
                            default,
                        );
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(&mut self.local_project);
                        self.send_content_cmd(ContentCommand::Execute(boxed));
                    }
                    self.needs_rebuild = true;
                }
            }
            TextInputField::GraphStringParam(node_id) => {
                if let (Some(param_name), Some(target), Some(default)) = (
                    self.text_input.graph_param_name.take(),
                    self.watched_graph_target.clone(),
                    self.watched_catalog_default.clone(),
                ) {
                    let scope = self
                        .graph_canvas
                        .as_ref()
                        .map(|c| c.scope_path().to_vec())
                        .unwrap_or_default();
                    let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                        target,
                        node_id,
                        param_name,
                        manifold_core::effect_graph_def::SerializedParamValue::String {
                            value: text.to_string(),
                        },
                        default,
                    )
                    .with_scope(scope);
                    self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                }
            }
            TextInputField::GraphWgsl(node_id) => {
                if let (Some(target), Some(default)) = (
                    self.watched_graph_target.clone(),
                    self.watched_catalog_default.clone(),
                ) {
                    let scope = self
                        .graph_canvas
                        .as_ref()
                        .map(|c| c.scope_path().to_vec())
                        .unwrap_or_default();
                    let cmd = manifold_editing::commands::graph::SetWgslSourceCommand::new(
                        target,
                        node_id,
                        text.to_string(),
                        default,
                    )
                    .with_scope(scope);
                    self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                }
            }
            TextInputField::GraphNumericParam(node_id) => {
                if let Some(ctx) = self.text_input.graph_numeric_param.take() {
                    // Lenient parse, same convention as InspectorParam: keep
                    // only the numeric head so a value typed with a unit
                    // suffix still commits.
                    let cleaned: String = text
                        .trim()
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
                        .collect();
                    if let Ok(parsed) = cleaned.parse::<f32>() {
                        let mut v = parsed.clamp(ctx.min, ctx.max);
                        if ctx.whole_numbers {
                            v = v.round();
                        }
                        if let Some(outer_param_id) = ctx.outer_param_id {
                            // D4/D6 parity: a group-face mirror row writes
                            // through the outer card param's own path — the
                            // SAME dispatch `GraphEditCommand::SetOuterParam`
                            // uses (app_render.rs), never `SetGraphNodeParam`
                            // on the inner node.
                            if let Some(target) = self.watched_graph_target.as_ref() {
                                use manifold_ui::panels::{PanelAction, ScrubPhase, ScrubValue, ValueRef};
                                let gpt = match target {
                                    manifold_core::GraphTarget::Effect(_) => {
                                        manifold_ui::panels::GraphParamTarget::Effect(0)
                                    }
                                    manifold_core::GraphTarget::Generator(_) => {
                                        manifold_ui::panels::GraphParamTarget::Generator
                                    }
                                };
                                let action = PanelAction::Scrub(
                                    ValueRef::Param(
                                        gpt,
                                        manifold_core::effects::ParamId::from(outer_param_id),
                                    ),
                                    ScrubPhase::Move(ScrubValue::Scalar(v)),
                                );
                                let content_tx = self.content_tx.as_ref().unwrap();
                                let editor_target = self.watched_graph_target.as_ref();
                                let mut dctx = crate::ui_bridge::DispatchCtx {
                                    project: &mut self.local_project,
                                    content_tx,
                                    content_state: &self.content_state,
                                    ui: &mut self.ws.ui_root,
                                    selection: &mut self.selection,
                                    active_layer: &mut self.active_layer_id,
                                    user_prefs: &mut self.user_prefs,
                                    editor_target,
                                    scrub: &mut self.scrub,
                                };
                                let _ = crate::ui_bridge::dispatch(&action, &mut dctx);
                            }
                        } else if let (Some(target), Some(default)) = (
                            self.watched_graph_target.clone(),
                            self.watched_catalog_default.clone(),
                        ) {
                            let scope = self
                                .graph_canvas
                                .as_ref()
                                .map(|c| c.scope_path().to_vec())
                                .unwrap_or_default();
                            let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                                target,
                                node_id,
                                ctx.param_name,
                                manifold_core::effect_graph_def::SerializedParamValue::Float {
                                    value: v,
                                },
                                default,
                            )
                            .with_scope(scope);
                            self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                        }
                        self.needs_rebuild = true;
                    }
                }
            }
            TextInputField::GraphNodeSearch => {
                // Live-filtered while typing (see the editor key handler); the
                // final query persists as the canvas highlight. Nothing to
                // commit to the model.
            }
            TextInputField::GraphTableCell => {
                if let Some(edit) = self.text_input.graph_table_edit.take()
                    && let Ok(v) = text.trim().parse::<f32>()
                    && let (Some(target), Some(default)) = (
                        self.watched_graph_target.clone(),
                        self.watched_catalog_default.clone(),
                    )
                {
                    let mut rows = edit.rows;
                    if let Some(cell) = rows.get_mut(edit.row).and_then(|r| r.get_mut(edit.col)) {
                        // No-op edits skip the command so an accidental click +
                        // Enter doesn't push an empty undo step.
                        if (*cell - v).abs() > f32::EPSILON {
                            *cell = v;
                            let scope = self
                                .graph_canvas
                                .as_ref()
                                .map(|c| c.scope_path().to_vec())
                                .unwrap_or_default();
                            let cmd =
                                manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                                    target,
                                    edit.node_id,
                                    edit.param_name,
                                    manifold_core::effect_graph_def::SerializedParamValue::Table {
                                        rows,
                                    },
                                    default,
                                )
                                .with_scope(scope);
                            self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                }
            }
            TextInputField::SavePresetName => {
                // Save to Library / Save to Project (PRESET_LIBRARY_DESIGN
                // D4, P3). The ctx carries the target's CURRENT effective
                // definition, already resolved (and values-snapshotted) at
                // the point the prompt opened — this arm only mints the name
                // and writes/upserts it.
                if let Some(ctx) = self.text_input.save_preset.take() {
                    let typed = text.trim();
                    if typed.is_empty() {
                        log::warn!("[preset] Save to {:?} cancelled: empty name", ctx.destination);
                    } else {
                        match ctx.destination {
                            crate::text_input::SavePresetDestination::Library => {
                                let lib = crate::user_library::UserLibrary::new();
                                match lib.save(ctx.kind, typed, &ctx.def) {
                                    Ok(id) => {
                                        log::info!(
                                            "[preset] saved '{}' to the user library",
                                            id.as_str()
                                        );
                                        // PRESET_LIBRARY_DESIGN P6/D7: the ONLY
                                        // render — save-time, once, here. The
                                        // browser only ever reads this PNG back
                                        // off disk (never renders).
                                        if let Some(gpu) = self.gpu.as_ref() {
                                            let png_path =
                                                lib.thumbnail_path(ctx.kind, id.as_str());
                                            if let Err(e) =
                                                manifold_renderer::preset_thumbnail::render_preset_thumbnail_to_file(
                                                    &gpu.device,
                                                    ctx.kind,
                                                    &ctx.def,
                                                    manifold_renderer::preset_thumbnail::THUMBNAIL_SIZE,
                                                    &png_path,
                                                )
                                            {
                                                log::error!(
                                                    "[preset] thumbnail render failed for '{}': {e}",
                                                    id.as_str()
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => log::error!("[preset] Save to Library failed: {e}"),
                                }
                            }
                            crate::text_input::SavePresetDestination::Project => {
                                let mut def = ctx.def;
                                if let Some(meta) = def.preset_metadata.as_mut() {
                                    meta.display_name = typed.to_string();
                                }
                                let cmd =
                                    manifold_editing::commands::preset::SaveToProjectCommand::new(
                                        ctx.kind, def,
                                    );
                                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                    Box::new(cmd);
                                boxed.execute(&mut self.local_project);
                                self.send_content_cmd(ContentCommand::Execute(boxed));
                            }
                        }
                    }
                }
                self.needs_rebuild = true;
            }
            TextInputField::RenamePreset => {
                // Browser management-menu Rename commit (PRESET_LIBRARY_DESIGN
                // P5, D6). MyLibrary is a plain file rewrite (no undo, matches
                // `UserLibrary::rename`'s existing non-undoable precedent —
                // Push to Library is the same shape); Project routes through
                // the undoable `RenameEmbeddedPresetCommand`, same
                // execute-locally-then-send-to-content-thread pattern as
                // `SavePresetName`'s Project-destination arm above.
                if let Some(ctx) = self.text_input.rename_preset.take() {
                    let typed = text.trim();
                    if typed.is_empty() {
                        log::warn!("[preset] Rename cancelled: empty name");
                    } else {
                        use manifold_ui::panels::picker_core::Source;
                        match ctx.source {
                            Source::MyLibrary => {
                                let lib = crate::user_library::UserLibrary::new();
                                if let Err(e) = lib.rename(ctx.kind, &ctx.id, typed) {
                                    log::error!("[preset] rename failed: {e}");
                                }
                            }
                            Source::Project => {
                                let cmd = manifold_editing::commands::preset::RenameEmbeddedPresetCommand::new(
                                    ctx.id,
                                    typed.to_string(),
                                );
                                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                    Box::new(cmd);
                                boxed.execute(&mut self.local_project);
                                self.send_content_cmd(ContentCommand::Execute(boxed));
                            }
                            Source::Factory => {
                                log::error!("[preset] rename requested for a Factory id — unreachable");
                            }
                        }
                    }
                }
                self.needs_rebuild = true;
            }
        }
    }

    // tick_and_render() and present_all_windows() moved to app_render.rs

    /// Convert a winit key to a manifold_ui Key.
    pub(crate) fn convert_key(logical_key: &Key) -> Option<manifold_ui::input::Key> {
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

        // Native menu bar. Built + attached once the app/window exists (macOS
        // `init_for_nsapp` needs a live `NSApplication`). Clicks are drained in
        // the render loop and routed through the normal `PanelAction` dispatch.
        let mut app_menu = crate::menu::AppMenu::new();
        app_menu.init_platform();
        // Populate the File → Open Recent submenu from the persisted list.
        app_menu.set_recent_projects(&self.project_io.recent_projects());
        self.app_menu = Some(app_menu);

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
            let native_device = std::sync::Arc::new(manifold_gpu::GpuDevice::new());

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
            // BUG-028: winit never surfaces a live pointer position during a
            // Finder file drag. Install the draggingUpdated:/performDragOperation:
            // interposition on the window's delegate so drop targeting can
            // read the real position — see drag_interpose.rs for why.
            crate::drag_interpose::install(&window);

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

            // Per-node thumbnail pipeline — samples one atlas cell (a UV
            // sub-rect, passed as inline bytes `cell = vec4(u0, v0, du, dv)`) and
            // draws it into the node's body viewport in the editor present pass.
            let thumb_shader = r#"
@group(0) @binding(0) var t_atlas: texture_2d<f32>;
@group(0) @binding(1) var s_atlas: sampler;
struct Cell { rect: vec4<f32> };
@group(0) @binding(2) var<uniform> cell: Cell;
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
    let uv = cell.rect.xy + in.uv * cell.rect.zw;
    return textureSample(t_atlas, s_atlas, uv);
}
"#;
            self.node_thumb_pipeline = Some(native_device.create_render_pipeline(
                thumb_shader,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
                None,
                "Node Thumbnail Pipeline",
            ));
            self.thumb_sampler = Some(native_device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
                address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
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

            // Layer grid bitmaps + lane/stem/overview/group panels (one instance),
            // and per-clip waveform textures, drawn around the GPU clip passes
            // (§24 5b).
            self.layer_bitmap_gpu = Some(manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu::new(
                &native_device,
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            ));
            self.clip_content_gpu = Some(manifold_renderer::clip_content_gpu::ClipContentGpu::new(
                &native_device,
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            ));
            self.clip_thumb_gpu = Some(manifold_renderer::clip_thumb_gpu::ClipThumbGpu::new(
                &native_device,
                manifold_gpu::GpuTextureFormat::Bgra8Unorm,
            ));

            // GPU-completion fence for the four UI immediate-draw vertex
            // rings above: gates ring-slot reuse under GPU backlog instead
            // of trusting ring depth alone (BUG: GLB timeline flicker under
            // heavy scenes). See `ui_frame_fence` field doc.
            let ui_frame_fence = Arc::new(manifold_gpu::FrameFence::new());
            self.ui_renderer
                .as_mut()
                .expect("ui_renderer just constructed above")
                .set_frame_fence(ui_frame_fence.clone());
            self.layer_bitmap_gpu
                .as_mut()
                .expect("layer_bitmap_gpu just constructed above")
                .set_frame_fence(ui_frame_fence.clone());
            self.clip_content_gpu
                .as_mut()
                .expect("clip_content_gpu just constructed above")
                .set_frame_fence(ui_frame_fence.clone());
            self.clip_thumb_gpu
                .as_mut()
                .expect("clip_thumb_gpu just constructed above")
                .set_frame_fence(ui_frame_fence.clone());
            self.ui_frame_fence = Some(ui_frame_fence);

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

                // Node-output preview bridge — fixed small size (the editor
                // pane is small and the content thread downscales into it).
                let node_bridge =
                    Arc::new(crate::shared_texture::SharedTextureBridge::new(480, 270));
                let node_textures: [manifold_gpu::GpuTexture;
                    crate::shared_texture::SURFACE_COUNT] = std::array::from_fn(|i| unsafe {
                    node_bridge.import_texture_native(&gpu.device, i)
                });
                self.ui_node_preview_textures = node_textures.map(Some);
                self.node_preview_texture_bridge = Some(Arc::clone(&node_bridge));

                // Per-node thumbnail atlas bridge — one big cell-grid texture of
                // 16:9 cells (not square), so thumbnails keep a video aspect.
                let atlas_bridge = Arc::new(crate::shared_texture::SharedTextureBridge::new(
                    crate::content_pipeline::ATLAS_W,
                    crate::content_pipeline::ATLAS_H,
                ));
                let atlas_textures: [manifold_gpu::GpuTexture;
                    crate::shared_texture::SURFACE_COUNT] = std::array::from_fn(|i| unsafe {
                    atlas_bridge.import_texture_native(&gpu.device, i)
                });
                self.ui_node_atlas_textures = atlas_textures.map(Some);
                self.node_atlas_texture_bridge = Some(Arc::clone(&atlas_bridge));

                // Clip-thumbnail FILMSTRIP atlas surface (§24 5c-2, BUG-119) — its own
                // smaller cell-grid geometry (many narrow bar cells), independent
                // lifecycle (always-on in the timeline vs editor-only). A SINGLE
                // shared IOSurface, not a triple-buffer bridge: the atlas is
                // persistent, slowly-changing data with no "stale frame" to solve,
                // and the old ring's periodic publish-with-clear was itself the bug
                // (BUG_BACKLOG.md BUG-119 — it could clear the surface the UI thread
                // was concurrently sampling).
                let clip_atlas_surface = Arc::new(crate::shared_texture::SharedAtlasSurface::new(
                    crate::content_pipeline::CLIP_ATLAS_W,
                    crate::content_pipeline::CLIP_ATLAS_H,
                ));
                let ui_clip_atlas_texture =
                    unsafe { clip_atlas_surface.import_texture_native(&gpu.device) };
                self.ui_clip_atlas_texture = Some(ui_clip_atlas_texture);
                self.clip_atlas_surface = Some(Arc::clone(&clip_atlas_surface));
            }

            // Create native Metal device BEFORE renderers so they can build native pipelines.
            // This gives the content thread its OWN MTLCommandQueue, completely separate
            // from the UI thread's queue. Metal interleaves GPU work from both queues,
            // preventing the content thread from starving UI submissions.
            let native_device = Arc::new(manifold_gpu::GpuDevice::new());
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
                    Arc::clone(&native_device),
                    output_w,
                    output_h,
                    manifold_gpu::GpuTextureFormat::Rgba16Float,
                    8,
                )),
                #[cfg(not(target_os = "macos"))]
                Box::new(StubRenderer::new_video()),
                // Static-image clips. Claims clips with an `image_path`;
                // GeneratorRenderer explicitly excludes those so dispatch
                // is order-independent, but image stays ahead of it anyway.
                #[cfg(target_os = "macos")]
                Box::new(manifold_media::image_renderer::ImageRenderer::new(
                    Arc::clone(&native_device),
                    output_w,
                    output_h,
                )),
                Box::new(GeneratorRenderer::new(
                    Arc::clone(&native_device),
                    output_w,
                    output_h,
                    gen_format,
                    8,
                )),
            ];
            let mut engine = PlaybackEngine::new(renderers);
            engine.initialize(self.local_project.clone());
            // The live-clip sink for MIDI phantom clips AND live audio triggers.
            // Without it both `tick_midi_input` and `tick_audio_triggers` bail at
            // their `live_clip_manager.is_none()` guard, so nothing ever fires.
            engine.set_live_clip_manager(
                manifold_playback::live_clip_manager::LiveClipManager::new(),
            );

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
            // Content-side import of the node-output preview IOSurfaces.
            #[cfg(target_os = "macos")]
            if let Some(ref bridge) = self.node_preview_texture_bridge {
                let native_dev = content_pipeline.native_device().unwrap();
                let node_textures: [manifold_gpu::GpuTexture;
                    crate::shared_texture::SURFACE_COUNT] =
                    std::array::from_fn(|i| unsafe { bridge.import_texture_native(native_dev, i) });
                content_pipeline.set_node_preview_textures(node_textures, Arc::clone(bridge));
            }
            // Content-side import of the thumbnail-atlas IOSurfaces.
            #[cfg(target_os = "macos")]
            if let Some(ref bridge) = self.node_atlas_texture_bridge {
                let native_dev = content_pipeline.native_device().unwrap();
                let atlas_textures: [manifold_gpu::GpuTexture;
                    crate::shared_texture::SURFACE_COUNT] =
                    std::array::from_fn(|i| unsafe { bridge.import_texture_native(native_dev, i) });
                content_pipeline.set_node_atlas_textures(atlas_textures, Arc::clone(bridge));
            }
            // Content-side import of the single shared clip-thumbnail atlas
            // surface (§24 5c, BUG-119), plus its one-time init clear — Metal
            // doesn't zero-init, and this is the ONLY clear the atlas ever gets:
            // every cell blit after this uses `LoadAction::Load` (see
            // `fill_clip_atlas`/`restore_clip_atlas` in content_pipeline.rs). The
            // clear is waited-out synchronously so the surface is guaranteed
            // transparent before either thread's first real frame.
            #[cfg(target_os = "macos")]
            if let Some(ref surface) = self.clip_atlas_surface {
                let native_dev = content_pipeline.native_device().unwrap();
                let clip_atlas_tex = unsafe { surface.import_texture_native(native_dev) };
                let mut clear_enc = native_dev.create_encoder("Clip Atlas Init Clear");
                clear_enc.clear_texture(&clip_atlas_tex, 0.0, 0.0, 0.0, 0.0);
                clear_enc.commit_and_wait_completed();
                content_pipeline.set_clip_atlas_texture(clip_atlas_tex, Arc::clone(surface));
            }
            self.content_pipeline_output = Some(content_pipeline.shared_output());

            let audio_layer_playback =
                match manifold_playback::audio_layer_playback::AudioLayerPlayback::new() {
                    Ok(p) => Some(p),
                    Err(e) => {
                        log::warn!("[AudioLayer] Failed to initialize audio-layer playback: {e}");
                        None
                    }
                };

            let mut midi_input = manifold_playback::midi_input::MidiInputController::new();
            midi_input.start();

            let content_thread = crate::content_thread::ContentThread {
                engine,
                editing_service: EditingService::new(),
                content_pipeline,
                audio_layer_playback,
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
                tempo_recorder: manifold_playback::tempo_recorder::TempoRecorder::new(),
                link_beat_offset: f64::NAN,
                led_controller: None,
                still_export: None,
                cached_midi_device_names: Vec::new(),
                last_midi_device_scan_time: manifold_core::Seconds(-10.0),
                cached_project_snapshot: None,
                watched_graph_target: None,
                preview_graph_node: None,
                node_preview_normalize: false,
                cached_graph_snapshot: None,
                mod_scratch: crate::content_state::ModulationSnapshot::empty(),
                audio_mod_runtime: crate::audio_mod_runtime::AudioModRuntime::default(),
                cached_midi_clock_position: Arc::from(""),
                cached_midi_clock_device: Arc::from(""),
                cached_perc_message: Arc::from(""),
                last_sent_midi_device_names: Arc::from([]),
                // No project is live at construction, so no forks yet.
                embedded_presets_fingerprint: 0,
                pending_undo_redo_event: None,
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

            // Step 10: start the preset hot-reload watcher alongside the
            // content thread. Detached background thread, off the render
            // and content tick paths — editing a preset `.json` on disk
            // refreshes the catalog + registry and rebuilds live chains
            // without a restart. Idempotent; a no-op if already started.
            manifold_renderer::preset_loader::start_preset_watcher();
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
            &self.content_state.automation_latched_params,
        );

        // `--resume` boot fast path (GIG_RESILIENCE_DESIGN §5.2): content
        // thread + GPU are up, so this is the earliest point that can load a
        // project and seek/play it. Output-window creation + perform-mode
        // entry happen on the next `about_to_wait` via the flag this sets
        // (`handle_perform_mode_pending` already runs unconditionally there).
        if let Some(path) = self.resume_breadcrumb_path.take() {
            self.boot_resume(&path);
        }

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
                    self.editor_resized(window_id, size);
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
                    self.editor_scale_factor_changed(window_id, scale_factor);
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

            // ── Pointer input → window input owner (window_input.rs) ──────
            WindowEvent::CursorMoved { position, .. } => {
                self.input_cursor_moved(window_id, is_primary, is_graph_editor, position);
            }

            WindowEvent::MouseInput { button, state, .. } => {
                self.input_mouse_input(window_id, is_primary, is_graph_editor, button, state);
            }

            // ── Mouse wheel (scroll / zoom) ──────────────────────────
            WindowEvent::MouseWheel { delta, .. } => {
                self.input_mouse_wheel(window_id, is_primary, is_graph_editor, delta);
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
                self.input_keyboard(is_primary, is_graph_editor, logical_key);
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
                } else if crate::project_io::is_supported_midi_extension(&path)
                    || crate::project_io::is_supported_audio_extension(&path)
                {
                    // MIDI + audio files → route through ProjectIOService.
                    // winit's file-drop carries no coordinates during a
                    // Finder drag, so resolve the drop target from the live
                    // drag position (drag_interpose) when available, falling
                    // back to the last tracked cursor position otherwise:
                    // over an existing audio lane → the file joins it; over
                    // empty timeline → a new lane. Beat comes from the x when
                    // inside the tracks area.
                    let pos = self.drag_tracker.drop_position().unwrap_or(self.cursor_pos);
                    let vp = &self.ws.ui_root.viewport;
                    let in_tracks = vp.get_tracks_rect().contains(pos);
                    let drop_beat = if in_tracks {
                        vp.pixel_to_beat(pos.x).as_f32().max(0.0)
                    } else {
                        self.content_state.current_beat.as_f32()
                    };
                    let join_audio_layer = if in_tracks {
                        vp.layer_at_y(pos.y)
                            .and_then(|i| self.local_project.timeline.layers.get(i))
                            .filter(|l| l.is_audio())
                            .map(|l| l.layer_id.clone())
                    } else {
                        None
                    };
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
                        join_audio_layer,
                        &mut self.local_project,
                        spb,
                    );
                    self.apply_project_io_action(action);
                } else if crate::project_io::is_supported_image_extension(&path) {
                    // Still images → an image clip, Video layers only. Resolve
                    // the drop beat and target layer from the live drag
                    // position when available (drag_interpose), falling back
                    // to the last tracked cursor position (winit's file-drop
                    // carries no coordinates of its own).
                    let pos = self.drag_tracker.drop_position().unwrap_or(self.cursor_pos);
                    let (drop_beat, layer_under_cursor) = {
                        let vp = &self.ws.ui_root.viewport;
                        let in_tracks = vp.get_tracks_rect().contains(pos);
                        let beat = if in_tracks {
                            vp.pixel_to_beat(pos.x).as_f32().max(0.0)
                        } else {
                            self.content_state.current_beat.as_f32()
                        };
                        let under = if in_tracks { vp.layer_at_y(pos.y) } else { None };
                        (beat, under)
                    };
                    self.import_image_file(&path, drop_beat, layer_under_cursor);
                } else if ext == "glb"
                    || ext == "gltf"
                    || crate::blender_import::is_blender_convertible_extension(&ext)
                {
                    // 3D models → a new generator layer whose graph renders the
                    // model, plus a default clip so it plays immediately.
                    // FBX/.obj/.dae route through `import_model_file`'s
                    // Blender-conversion seam first (IMPORT_ANYTHING_WAVE_DESIGN.md
                    // Lane W3) — MANIFOLD is glTF-only internally.
                    // Resolve the drop beat and target layer from the live
                    // drag position when available (drag_interpose), falling
                    // back to the last tracked cursor position (winit's
                    // file-drop carries no coordinates of its own).
                    let pos = self.drag_tracker.drop_position().unwrap_or(self.cursor_pos);
                    let (drop_beat, layer_under_cursor) = {
                        let vp = &self.ws.ui_root.viewport;
                        let in_tracks = vp.get_tracks_rect().contains(pos);
                        let beat = if in_tracks {
                            vp.pixel_to_beat(pos.x).as_f32().max(0.0)
                        } else {
                            self.content_state.current_beat.as_f32()
                        };
                        let under = if in_tracks { vp.layer_at_y(pos.y) } else { None };
                        (beat, under)
                    };
                    self.import_model_file(&path, drop_beat, layer_under_cursor);
                } else if ext == "json" || ext == "manifold" {
                    // Project files → load project
                    self.open_project_from_path(path.clone());
                } else {
                    log::debug!("Unrecognized file type dropped: {}", path.to_string_lossy());
                }
                // Drop consumed — stop reporting a drag position/hover state
                // regardless of which branch above ran.
                self.drag_tracker.on_drag_ended();
            }
            WindowEvent::HoveredFile(path) => {
                self.drag_tracker.on_hovered_file(path);
                // Live-gate diagnostic (BUG-028): confirms the tracker sees
                // the hover and, on the next log line from a DroppedFile,
                // whether drag_interpose supplied a live position or the
                // drop fell back to cursor_pos.
                log::debug!(
                    "File hovering: {:?} (tracker active={}, {} file(s) hovered)",
                    self.drag_tracker.hovered_files(),
                    self.drag_tracker.is_active(),
                    self.drag_tracker.hovered_files().len(),
                );
            }
            WindowEvent::HoveredFileCancelled => {
                log::debug!("File hover cancelled");
                self.drag_tracker.on_drag_ended();
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
        self.clip_content_gpu = None;
        self.clip_thumb_gpu = None;
        self.ui_renderer = None;
        self.blit_pipeline = None;
        self.blit_sampler = None;
        self.atlas_pipeline = None;
        self.atlas_sampler = None;
        self.node_thumb_pipeline = None;
        self.thumb_sampler = None;
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
