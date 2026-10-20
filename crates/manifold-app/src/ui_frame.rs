//! The shared per-frame UI seam (`UI_HARNESS_UNIFICATION_DESIGN.md` D3, P1).
//!
//! Two functions extracted VERBATIM (moved, not rewritten) from
//! `app_render.rs`'s `tick_and_render` and `present_all_windows`: the
//! invalidation/rebuild decision block, and the dirty-panel atlas composite.
//! Both the live App and the headless harness call these — the app and the
//! harness now literally run the same code, which is the structural
//! faithfulness proof the design replaces the old red-bracket model with.
//!
//! App-internal module (no new crate, no new dependency, no thread-residency
//! change) — everything here is `pub(crate)`.
//!
//! ── Deviations from the design doc's section 4 committed signatures
//! (`UI_HARNESS_UNIFICATION_DESIGN.md` section 4), found at VERIFY-AT-IMPL and
//! reported to the orchestrator rather than silently absorbed — see the P1
//! phase report for the full escalation writeup:
//!
//! 1. `apply_ui_frame_invalidations`'s `cache` parameter is
//!    `Option<&mut UICacheManager>`, not `&mut UICacheManager` as drafted.
//!    `tick_and_render` runs every UI tick regardless of GPU/window
//!    readiness (no `self.gpu`-gated early return upstream of the
//!    invalidation block, unlike `present_all_windows`), and
//!    `self.ui_cache_manager` is genuinely `None` before GPU init
//!    completes. The original code reflects this: `ui_root.build()` and the
//!    region rebuilds always run; only the `cache.invalidate_*`
//!    calls are individually wrapped in `if let Some(cm) = ...`
//!    (app_render.rs:960-964, 2827, 2843, 2849, pre-extraction). A
//!    non-Option `cache` would force either skipping `ui_root.build()`
//!    when the cache doesn't exist yet (a real behavior change — the tree
//!    wouldn't be built pre-GPU-init) or a duplicated inline fallback at
//!    the call site (reintroducing the exact drift D3 exists to kill).
//! 2. Both functions take `ui_root: &mut UIRoot`, not `&UIRoot`. Each
//!    performs a mutation `UIRoot` itself owns: `apply_ui_frame_invalidations`
//!    calls `ui_root.build()` and the region rebuilds;
//!    `composite_main_ui_frame` clears the dirty ranges `render_dirty_panels`
//!    just painted via `ui_root.tree.clear_dirty_range(..)` (the cache
//!    manager doesn't own the tree — this loop is the sole place panel-range
//!    dirty-flag clearing happens, per the BUG-015 comment at the
//!    precedent site). `clear_dirty_range` requires `&mut self` on
//!    `UITree` — an immutable `ui_root` cannot compile against this call.
//! 3. `composite_main_ui_frame` gained seven parameters the doc's 6-param
//!    signature omitted: `atlas_pipeline: &GpuRenderPipeline`,
//!    `atlas_sampler: &GpuSampler`, `blit_pipeline: &GpuRenderPipeline`,
//!    `blit_sampler: &GpuSampler`, `scale_factor: f64`,
//!    `video_source_dims: (f32, f32)`. The atlas blit and video-band blit
//!    passes (present_all_windows' old Pass 2 / Pass 3) draw with cached
//!    `GpuRenderPipeline`/`GpuSampler` objects that live on `Application`
//!    (`self.atlas_pipeline` etc., app.rs:276-310) — built once at GPU
//!    init, never reachable from `device`, `ui_renderer`, `cache`,
//!    `ui_root`, `offscreen`, or `video`. Recreating a pipeline per frame
//!    would violate the hot-path discipline (CLAUDE.md), so there's no
//!    alternative to threading them through. `scale_factor` is needed for
//!    the video-band rect math (`video_rect * sf`); it isn't reachable
//!    either — `UICacheManager::scale_factor` is a private field with no
//!    getter. `video_source_dims` preserves the original aspect-ratio
//!    source exactly: the pre-extraction code reads
//!    `self.content_pipeline_output.get_dimensions()` (falling back to
//!    1920×1080), NOT `compositor_tex.width/height` — the two are expected
//!    to agree in the overwhelming common case, but only the original
//!    source preserves behavior through a hypothetical resize-race
//!    divergence between the two, so it's threaded through rather than
//!    substituted. All seven are cached/computed once per frame by the
//!    caller exactly as before; nothing here recreates GPU state.
//! 4. `apply_ui_frame_invalidations` clears `signals.scrolled_in_place`
//!    after consuming it, so a harness that reuses one `UiFrameSignals`
//!    across frames never replays the inspector slot invalidation.
//! 5. `composite_main_ui_frame` bundles `render_dirty_panels` (previously
//!    called unconditionally, before `present_all_windows`'s fast-path
//!    check) into a function now called only from the non-fast-path branch.
//!    Proven behavior-preserving, not just convenient: `tick_and_render`
//!    sets `self.ws.offscreen_dirty = true` whenever ANY panel node is dirty
//!    (`has_dirty_in_range(0, panel_end)`) — the exact condition
//!    `render_dirty_panels` needs to have anything to paint — so
//!    `render_dirty_panels` was already a provable no-op on every frame the
//!    fast path takes. Deferring its call site changes no pixel; it does
//!    drop the `present.panel_cache` profiler timer's coverage of fast-path
//!    frames (cosmetic — those frames had nothing to time anyway).
//!
//! ── `render_main_ui_passes` (P2, `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md`
//! section 4 step 2) — additional deviations found at VERIFY-AT-IMPL:
//!
//! 6. Profiling is split by value, not deleted. The async **GPU-time sink**
//!    IS preserved: it's threaded in as `MainUiPassInputs::gpu_sink`
//!    (`Option<(Arc<AtomicU64>, Arc<AtomicU64>)>`, fed by
//!    `UiFrameProfile::gpu_sink()`; `None` headless — section 3 input presence) and
//!    the seam attaches `encoder.add_gpu_time_handler(...)` before its
//!    commit, so the "our GPU work vs. content-thread starvation" attribution
//!    the frame-pacing investigations depend on is unchanged. What DID
//!    coalesce is the fine per-pass CPU breakdown: the caller now wraps the
//!    whole seam in one `present.main_ui_passes` timer instead of the seven
//!    labels (`pass4a_grid`, `pass4b_clip_bodies`, `pass4b_waveforms`,
//!    `pass4b_thumbnails`, `pass4c_panels`, `pass5_overlay`, `commit`), and
//!    the per-frame integer counts (clips/thumbnails drawn) are gone —
//!    interleaving seven `Instant` cursors + count calls back through the
//!    shared signature would clutter the seam for sub-microsecond CPU-enqueue
//!    numbers that were never the bottleneck. Profiler-only, no pixel changes
//!    — same class as deviation #5 above.
//! 7. `LandingFlash` (the `landing_flash` field's element type) is a NEW
//!    type alias, not a reused upstream name — `InteractionOverlay::
//!    landing_flash()` returns a bare `Option<(f32, Beats, usize, usize)>`
//!    tuple, no named type exists upstream. Aliased here so the seam's
//!    struct field reads as intentional rather than an anonymous 4-tuple;
//!    the shape is unchanged from what `landing_flash()` already returns.
//! 8. `render_main_ui_passes` itself carries NO `#[cfg(target_os =
//!    "macos")]` anywhere in its body, even though two of the passes it
//!    owns (clip thumbnails, the VQT waterfall) are mac-only in the live
//!    app today. The mac-only-ness lives entirely in WHICH
//!    `Application` fields exist to resolve `inputs.thumb`/`inputs.vqt`
//!    (`clip_atlas_surface`, `spectrogram_pane`, etc., all
//!    `#[cfg(target_os = "macos")]` on `Application` — see `app.rs`) — the
//!    seam only ever sees `Option<ThumbPass>`/`Option<&mut VqtPassState>`,
//!    already `None` on a non-mac build via the caller's own
//!    `#[cfg(not(target_os = "macos"))]` arm. Input-presence branching (section 3),
//!    not caller-identity — the seam doesn't know or care which platform
//!    left the input `None`.
//! 9. The live caller gates the ENTIRE `render_main_ui_passes` call on
//!    `(self.ui_renderer, self.blit_pipeline, self.blit_sampler)` all being
//!    `Some` (mirroring `composite_main_ui_frame`'s existing call-site
//!    pattern). Pre-extraction, several passes (4a grid, 4b′ waveforms, 4c
//!    panels, the VQT waterfall) did NOT individually check
//!    `self.ui_renderer`'s presence — only pass 4b (clip bodies) and pass 5
//!    did. Verified behavior-preserving, not a silent narrowing: `app.rs`
//!    sets `ui_renderer`/`layer_bitmap_gpu`/`clip_content_gpu`/
//!    `clip_thumb_gpu`/`blit_pipeline`/`blit_sampler` together in ONE
//!    GPU-init block (:1865-1993) and clears them together in ONE teardown
//!    block (:2888-2893) — they are never independently `Some`/`None` of
//!    each other in any reachable state, so gating the whole call on
//!    `ui_renderer`'s presence produces the identical pass-execution set as
//!    the old independent per-pass gates on every frame that can actually
//!    occur.

use manifold_core::Beats;
use manifold_gpu::{GpuDevice, GpuRenderPipeline, GpuSampler, GpuTexture};
use manifold_ui_paint::clip_content_gpu::ClipContentGpu;
use manifold_ui_paint::clip_draw::ClipBody;
use manifold_ui_paint::clip_thumb_gpu::{ClipThumbGpu, ThumbQuad};
use manifold_ui_paint::layer_bitmap_gpu::LayerBitmapGpu;
use manifold_ui_paint::ui_cache_manager::UICacheManager;
use manifold_ui_paint::ui_renderer::UIRenderer;
use manifold_spectral::{ScopeColumn, Spectrogram, SpectrogramConfig};
use manifold_ui::node::{Color32, Vec2};
use manifold_ui::panels::viewport::{ClipScreenRect, TimelineOverlays};

use crate::frame_plan::{MainRegion, RegionMask, ViewportMoves};
use crate::texture_pane::TexturePane;
use crate::ui_root::UIRoot;
use manifold_core::work_count::{record_work, WorkKind};

/// What the executor applies to the tree this frame, read from the plan.
#[derive(Default)]
pub(crate) struct UiFrameSignals {
    pub scrolled_in_place: bool,
    /// The regions the frame plan rebuilds, each alone in its own arena (D7).
    pub rebuild: RegionMask,
    /// The regions the frame plan moves in place, and by how much.
    pub translate: RegionMask,
    /// The regions whose panel lays out again in place at a new size.
    pub resize: RegionMask,
    /// The regions whose own view moved without a rebuild. The tree half
    /// moves the audio dock body to its scroll offset here.
    pub in_place: RegionMask,
    /// Which timeline view axes moved: layer headers and viewport update in
    /// place, or rebuild when a count no longer matches.
    pub viewport: ViewportMoves,
    /// A region rect moved or resized: the pixels it left behind belong to
    /// no slot, so the atlas clears and every slot repaints.
    pub rects_moved: bool,
}

/// The executor's tree half (UI_INVALIDATION_DESIGN.md D6, D7): each planned
/// region rebuilt alone with its atlas slot invalidated, each translated
/// region moved in place, the timeline scroll applied in place. THE single
/// place these decisions are applied; the only caller of `UIRoot::build`,
/// `clear_region` and `translate_region` in the main window. No drag defers
/// a rebuild (D18): panels hold drag targets by row, index or `WidgetId` and
/// re-resolve them after a rebuild.
///
/// `cache` is `Option<&mut UICacheManager>` — see module doc deviation #1.
/// Returns whether any part of the tree was rebuilt, moved or scrolled.
pub(crate) fn apply_ui_frame_invalidations(
    ui_root: &mut UIRoot,
    mut cache: Option<&mut UICacheManager>,
    signals: &mut UiFrameSignals,
) -> bool {
    // An in-place inspector scroll offset the content nodes without a
    // rebuild: repaint just the inspector's atlas slot. One-shot, so a caller
    // that reuses `UiFrameSignals` across frames (the harness) never replays it.
    let inspector_scrolled = std::mem::take(&mut signals.scrolled_in_place);
    if inspector_scrolled
        && let Some(cm) = &mut cache
        && let Some(slot) = MainRegion::Inspector.panel_slot()
    {
        cm.invalidate_slot(slot);
    }
    if !ui_root.is_built() {
        ui_root.build();
        if let Some(cm) = &mut cache {
            cm.invalidate_all();
        }
        settle_ghosts(ui_root);
        return true;
    }
    if signals.rects_moved
        && let Some(cm) = cache.take()
    {
        // Every slot is invalid now; the region work below needs no slot.
        cm.invalidate_all();
    }
    let mut regions = signals.rebuild;
    let moves = signals.viewport;
    let mut scrolled = false;
    if moves.any() {
        if ui_root.scroll_in_place(moves, regions) {
            scrolled = true;
            if let Some(cm) = &mut cache {
                for region in [MainRegion::LayerHeaders, MainRegion::Viewport] {
                    if let Some(slot) = region.panel_slot() {
                        cm.invalidate_slot(slot);
                    }
                }
            }
        } else {
            regions = regions.union(UIRoot::scroll_rebuild_regions(moves));
        }
    }
    if signals.in_place.contains(MainRegion::AudioDock) && !regions.contains(MainRegion::AudioDock) {
        if ui_root.audio_setup_panel.scroll_in_place(&mut ui_root.tree) {
            scrolled = true;
            record_work(WorkKind::InPlaceUpdate);
            if let Some(cm) = &mut cache
                && let Some(slot) = MainRegion::AudioDock.panel_slot()
            {
                cm.invalidate_slot(slot);
            }
        } else {
            regions.insert(MainRegion::AudioDock);
        }
    }
    // A region rebuilt this frame is built at its new rect already.
    let mut translate = signals.translate;
    let mut resize = signals.resize;
    for r in regions.iter() {
        translate.remove(r);
        resize.remove(r);
    }
    let mut moved = translate_regions(ui_root, cache.as_deref_mut(), translate);
    for region in resize.iter() {
        if ui_root.follow_resize(region) {
            record_work(WorkKind::InPlaceUpdate);
            moved = true;
            if let Some(cm) = &mut cache
                && let Some(slot) = region.panel_slot()
            {
                cm.invalidate_slot(slot);
            }
        } else {
            regions.insert(region);
        }
    }
    let rebuilt = rebuild_regions(ui_root, cache, regions) || scrolled || moved;
    // Nodes that moved in place slid under a still cursor; no rebuild
    // cleared their hover, so resolve it again where the pointer is.
    let cards_moved = ui_root.inspector.take_moved_in_place() | ui_root.scene_setup_panel.take_moved_in_place();
    if inspector_scrolled || scrolled || moved || cards_moved {
        ui_root.input.refresh_hover(&mut ui_root.tree);
    }
    settle_ghosts(ui_root);
    rebuilt
}

/// Rebuild each region of `regions` alone, in build order: clear its arena,
/// build it again, invalidate its atlas slot (D7). Every other region keeps
/// its nodes, ids and pixels. Returns whether anything rebuilt.
fn rebuild_regions(ui_root: &mut UIRoot, mut cache: Option<&mut UICacheManager>, regions: RegionMask) -> bool {
    if regions.is_empty() {
        return false;
    }
    // Hover may point into a rebuilt region.
    ui_root.input.invalidate_hover();
    for region in regions.iter() {
        // The ghost follows the drag, not the plan: see `settle_ghosts`.
        if region == MainRegion::Ghosts {
            continue;
        }
        record_work(WorkKind::RegionRebuild);
        ui_root.tree.clear_region(region.id());
        ui_root.build_region(region);
        if let Some(slot) = region.panel_slot()
            && let Some(cm) = &mut cache
        {
            cm.invalidate_slot(slot);
        }
    }
    if regions.contains(MainRegion::Inspector) {
        // A rebuilt inspector recreates its cards with is_selected=false, and
        // undimmed under a live card drag.
        ui_root.inspector.apply_selection_visuals(&mut ui_root.tree);
    }
    true
}

/// The inspector projection onto the graph editor's own column, the same on
/// the window's open and on every main-window inspector sync. The column
/// hosts no scene panel (SCENE_SURFACES D16); its card lane rebuilds on the
/// projection, in the editor's own frame (D17).
pub(crate) fn project_editor_inspector(
    ed: &mut UIRoot,
    project: &manifold_core::project::Project,
    active_layer: Option<usize>,
    selection: &crate::app::SelectionState,
    latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    led_preview: Option<&crate::content_state::LedPreview>,
) {
    crate::ui_bridge::sync_inspector_data(ed, project, active_layer, selection, latched, led_preview);
    ed.inspector.drop_scene_tab();
    ed.scene_setup_panel.close();
    ed.note_inspector_projected();
}

/// The Ghosts region (D7) holds only the live card drag's ghost: the drag
/// start mints it, the drag moves it in place, and this clears it once the
/// drag ends or refills it when it holds anything else (a ghost from an
/// earlier drag in the same frame). No work while a drag lives or none does.
/// It has no atlas slot.
pub(crate) fn settle_ghosts(ui_root: &mut UIRoot) {
    let ghosts = MainRegion::Ghosts.id();
    let live = ui_root.inspector.card_drag_ghost_root();
    let len = ui_root.tree.region_len(ghosts);
    match live {
        None if len == 0 => return,
        Some(root) if len > 0 && ui_root.tree.id_in(ghosts, 0) == root => return,
        _ => {}
    }
    ui_root.tree.clear_region(ghosts);
    if live.is_some() {
        ui_root.build_region(MainRegion::Ghosts);
    }
}

/// Move each translated region's nodes by the plan's delta, let its panel
/// take the new rect, and repaint its atlas slot (D7): a region whose rect
/// only moved never rebuilds.
fn translate_regions(
    ui_root: &mut UIRoot,
    mut cache: Option<&mut UICacheManager>,
    regions: RegionMask,
) -> bool {
    for region in regions.iter() {
        // The exact move, not the plan's pixel-quantized one: a fractional
        // layout must land where a fresh build would put it.
        let (dx, dy) = ui_root.follow_translate(region);
        record_work(WorkKind::RegionTranslate);
        ui_root.tree.translate_region(region.id(), dx, dy);
        if let Some(slot) = region.panel_slot()
            && let Some(cm) = &mut cache
        {
            cm.invalidate_slot(slot);
        }
    }
    !regions.is_empty()
}

/// What moved this frame outside the plan: an in-place inspector scroll.
pub(crate) struct ProjectionChanges {
    pub scrolled_in_place: bool,
}

/// The `push_state` inputs that are not project, selection or active layer.
pub(crate) struct PushInputs<'a> {
    pub content_state: &'a crate::content_state::ContentState,
    pub is_dirty: bool,
    pub project_path: Option<&'a std::path::Path>,
    pub transport_cache: &'a mut crate::ui_bridge::TransportDisplayCache,
}

/// This frame's planner inputs (D6), read from the UI's own state.
pub(crate) fn gather_frame_inputs(
    planner: &mut crate::frame_plan::FramePlanner,
    ui_root: &UIRoot,
    selection: &mut manifold_ui::UIState,
    active_layer: &Option<manifold_core::LayerId>,
    edits: &crate::gesture_preview::UiEdits,
    content_state: &crate::content_state::ContentState,
    chrome: crate::frame_plan::ChromeSource<'_>,
) -> crate::frame_plan::FrameInputs {
    crate::frame_plan::FrameInputs {
        parts: edits.stamps.parts(),
        preview: edits.preview.version(),
        selection_identity: selection.selection_identity_version(),
        selection_visual: selection.selection_visual_version(),
        cursor: selection.cursor_version(),
        active_layer: planner.active_layer_version(active_layer),
        automation_view: selection.automation_view(),
        lane_heights: selection.lane_heights_view(),
        chrome: crate::frame_plan::ChromeInputs {
            export_open: ui_root.export_progress.is_open(),
            ..planner.chrome_inputs(content_state, &chrome)
        },
        rects: ui_root.region_rects(),
        scroll_x_px: ui_root.viewport.beat_to_pixel(Beats::ZERO).round() as i32,
        scroll_y_px: ui_root.viewport.scroll_y_px().round() as i32,
        zoom_bits: ui_root.viewport.pixels_per_beat().to_bits(),
        scroll_x_bits: ui_root.viewport.scroll_x_beats().as_f32().to_bits(),
        scroll_y_bits: ui_root.viewport.scroll_y_px().to_bits(),
        playhead_px: ui_root.viewport.playhead_pixel().map_or(i32::MIN, |px| px.round() as i32),
        // The docked Scene body is inspector-region content (SCENE_SURFACES
        // D16). Both counters only grow, so their sum moves when either does.
        inspector_view: ui_root.inspector.view_version().wrapping_add(ui_root.scene_setup_panel.view_version()),
        scene_selection: ui_root.scene_setup_panel.selection_version(),
        setup_check: planner.setup_check_key(
            chrome.project,
            [active_layer.as_ref(), ui_root.scene_setup_panel.gizmo_layer()],
            edits.stamps.parts(),
        ),
        inspector_reveal: ui_root.inspector_reveal_version(),
        layer_reveal: ui_root.layer_reveal_version(),
        dock_view: ui_root.audio_setup_panel.view_version(),
        dock_scroll_px: ui_root.audio_setup_panel.scroll_px().round() as i32,
        overlays: ui_root.overlays_version(),
        overlay_open: ui_root.any_overlay_open(),
        audio_directory: content_state.audio_directory_version,
        values: content_state.values_seq,
        runtime: content_state.runtime,
        driver_period_key: content_state.bpm.to_bits(),
        led_enabled: content_state.led_enabled,
        gizmo_frame: crate::physics_overlay::newest_frame(
            &content_state.physics_overlay_frames,
            ui_root.scene_setup_panel.gizmo_layer(),
        ),
        clock_digits: crate::frame_plan::clock_digits(
            content_state,
            chrome.project.settings.time_signature_numerator,
        ),
        ..Default::default()
    }
}

/// Apply one frame plan, then the per-frame work that sits around it in the
/// App's order: the projections the plan asks for (each at most once), the
/// editor window's inspector mirror, `between` (the caller's drag polling;
/// returns whether a clip move or trim drag is live), the tree work, the four
/// `push_state` pieces, and the drag-time clip and lane re-projection.
/// `redraw` is set when the frame needs the offscreen repainted (D8).
/// Called by the live App (`tick_and_render`) and the script runner.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_frame_plan<P: std::borrow::Borrow<manifold_core::project::Project>>(
    ui_root: &mut UIRoot,
    editor_ui_root: Option<&mut UIRoot>,
    mut cache: Option<&mut UICacheManager>,
    project_mut: &mut P,
    selection: &mut manifold_ui::UIState,
    active_layer: &mut Option<manifold_core::LayerId>,
    redraw: &mut bool,
    plan: &crate::frame_plan::FramePlan,
    changes: ProjectionChanges,
    push: PushInputs<'_>,
    between: impl FnOnce(
        &mut UIRoot,
        &mut P,
        &mut manifold_ui::UIState,
        &mut Option<manifold_core::LayerId>,
        &mut bool,
    ) -> bool,
) {
    use crate::frame_plan::Projection;
    let project: &manifold_core::project::Project = (*project_mut).borrow();
    let projections = plan.projections;
    let latched = &push.content_state.automation_latched_params;
    let led_preview = push.content_state.led_preview.as_ref();
    let active_idx = |project: &manifold_core::project::Project, id: &Option<manifold_core::LayerId>| {
        id.as_ref().and_then(|id| project.timeline.find_layer_index_by_id(id))
    };

    let project_sync = projections.contains(Projection::ProjectSync);
    let inspector_sync = project_sync || projections.contains(Projection::InspectorSync);
    if projections.contains(Projection::PresetList) {
        ui_root.sync_embedded_presets(project);
    }
    // The content clock's bpm, before the inspector projection reads it. A
    // moved frame-aligned period is rewritten in place after the build
    // (D20), never a structural sync.
    let mut driver_periods_moved = false;
    let mut editor_ui_root = editor_ui_root;
    if projections.contains(Projection::DriverReadout)
        && push.content_state.bpm.is_finite()
        && push.content_state.bpm > 0.0
    {
        let bpm = manifold_core::Bpm(push.content_state.bpm as f32);
        driver_periods_moved = ui_root.sync_driver_bpm(project, bpm);
        if let Some(ed) = editor_ui_root.as_deref_mut() {
            ed.sync_driver_bpm(project, bpm);
        }
    }
    if project_sync {
        let idx = active_idx(project, active_layer);
        crate::ui_bridge::sync_automation_lane_order(project, selection);
        crate::ui_bridge::sync_project_data(ui_root, project, idx, selection);
    }
    if projections.contains(Projection::TrackLayout) && !project_sync {
        crate::ui_bridge::sync_track_layout(ui_root, project, selection);
    }
    if inspector_sync {
        let idx = active_idx(project, active_layer);
        ui_root.chain_failures = push.content_state.chain_failures.clone();
        crate::ui_bridge::sync_inspector_data(ui_root, project, idx, selection, latched, led_preview);
    }
    // A marker drag moved the dragged marker; the viewport lays the new list
    // out in place.
    if projections.contains(Projection::MarkerPreview) && !project_sync {
        crate::ui_bridge::sync_markers(ui_root, project);
    }
    // The dock's device query keys on its own inputs, not on every inspector
    // sync (P4c).
    if projections.contains(Projection::AudioDevices) {
        crate::ui_bridge::sync_audio_dock(ui_root, project);
    }
    // Mirror the inspector sync onto the editor window's own inspector so its
    // column stays in lockstep with the main one; reconfigure (which resets
    // transient card state) fires only when it does for the main window.
    if inspector_sync && let Some(ed) = editor_ui_root.as_deref_mut() {
        let idx = active_idx(project, active_layer);
        ed.chain_failures = push.content_state.chain_failures.clone();
        project_editor_inspector(ed, project, idx, selection, latched, led_preview);
    }

    let clip_drag = between(ui_root, project_mut, selection, active_layer, redraw);
    let project: &manifold_core::project::Project = (*project_mut).borrow();

    // The plan's regions rebuild alone (D7); an inspector projection
    // rewrote the inspector's lists, so its region rebuilds with it.
    let mut rebuild = plan.rebuild;
    if inspector_sync {
        rebuild.insert(MainRegion::Inspector);
    }
    let mut signals = UiFrameSignals {
        scrolled_in_place: changes.scrolled_in_place,
        rebuild,
        translate: plan.translate,
        resize: plan.resize,
        in_place: plan.in_place,
        viewport: plan.viewport,
        rects_moved: plan.rects_moved,
    };
    let rebuilt = apply_ui_frame_invalidations(ui_root, cache.as_deref_mut(), &mut signals);
    *redraw |= plan.redraw;

    // Push engine state AFTER build so new nodes get state. Each push runs on
    // its own inputs, or when a projection or rebuild replaced the nodes and
    // lists it writes.
    let idx = active_idx(project, active_layer);
    let replaced = rebuilt || inspector_sync;
    crate::ui_bridge::drain_events(ui_root, push.content_state);
    if replaced || projections.contains(Projection::ChromePush) {
        crate::ui_bridge::push_chrome(
            ui_root,
            project,
            push.content_state,
            selection,
            push.is_dirty,
            push.project_path,
            push.transport_cache,
        );
    }
    if replaced || projections.contains(Projection::SelectionPush) {
        crate::ui_bridge::push_selection(ui_root, project, idx, selection);
    }
    // The clock writes its label in place; a chrome layout below re-reads
    // the same text.
    if replaced || projections.contains(Projection::ChromePush) || projections.contains(Projection::ClockPush) {
        crate::ui_bridge::push_clock(ui_root, project, push.content_state, push.transport_cache);
    }
    // The bars show what the two pushes above wrote; they lay out only then.
    if replaced
        || [MainRegion::Transport, MainRegion::Header, MainRegion::Footer]
            .into_iter()
            .any(|r| plan.in_place.contains(r))
    {
        // A bar resized to a new shape cannot lay out in place: it rebuilds,
        // from the values the pushes just wrote.
        let stale = ui_root.lay_out_chrome();
        rebuild_regions(ui_root, cache, stale);
    }
    crate::ui_bridge::push_frame(ui_root, project, push.content_state, idx);
    let live = crate::ui_bridge::LiveGroups {
        values: replaced || projections.contains(Projection::LiveValues),
        shapes: replaced
            || projections.contains(Projection::LiveShapes)
            || projections.contains(Projection::LiveRanges),
    };
    if live.any() {
        crate::ui_bridge::push_live(ui_root, project, push.content_state, idx, live);
    }
    // The inspector projection already built the eyes from the project.
    if projections.contains(Projection::SceneEyes) && !inspector_sync {
        crate::ui_bridge::sync_scene_outliner_eyes(ui_root, project);
    }
    if driver_periods_moved {
        crate::ui_bridge::sync_driver_readouts(ui_root, project, idx);
        if let Some(ed) = editor_ui_root {
            crate::ui_bridge::sync_driver_readouts(ed, project, idx);
        }
    }

    // Re-project clips and automation lanes only when their source moved: a
    // committed edit, a drag preview, or a live clip drag.
    if project_sync
        || clip_drag
        || projections.contains(Projection::ClipPositions)
        || projections.contains(Projection::ClipPreview)
    {
        crate::ui_bridge::sync_clip_positions(ui_root, project);
    }
    if project_sync
        || projections.contains(Projection::LaneGeometry)
        || projections.contains(Projection::LanePreview)
    {
        crate::ui_bridge::sync_automation_lane_geometry(
            ui_root,
            project,
            selection.automation_mode_visible(),
            selection.chosen_automation_params(),
            selection.automation_lane_order(),
            selection.hidden_automation_lanes(),
            selection.pinned_automation_lanes(),
        );
    }
}

/// Clear the dirt of every region the atlas just painted. A painted range is
/// the region's whole panel range (the incremental path returns it whole
/// only when no dirt lies outside its sub-regions, BUG-015), and every other
/// node of the arena is a region root, which paints nothing
/// (`atlas_ranges_cover_every_region_node`). Clearing only the panel range
/// left the roots dirty forever, so the idle main window recomposited every
/// frame (BUG-5y9y4 (region roots stay dirty)).
pub(crate) fn clear_painted_dirt(ui_root: &mut UIRoot, painted: &[manifold_ui_paint::ui_cache_manager::RenderedRange]) {
    for &(region, _, _) in painted {
        ui_root.tree.clear_dirty_range(region, 0, usize::MAX);
    }
}

/// Composites the main-window UI for one frame into `offscreen`:
/// `render_dirty_panels` (atlas, LoadOp::Load) + clear-to-black +
/// full-atlas blit + optional video-band blit. Does NOT acquire or present a
/// drawable, and does NOT draw the timeline-track passes (grid bitmaps, clip
/// bodies, waveforms, overlays) — those stay in `present_all_windows`,
/// unchanged, on their own encoder after this call returns.
/// `video` is the compositor output for the video band; `None` in the
/// harness. `atlas_pipeline`/`atlas_sampler`/`blit_pipeline`/`blit_sampler`/
/// `scale_factor` are the cached GPU resources the blit passes need — see
/// module doc deviation #3.
/// Precedent: `present_all_windows` (pre-extraction) :3890-4064 minus the
/// fast path (:3951-3998) and the drawable tail.
#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_main_ui_frame(
    device: &GpuDevice,
    ui_renderer: &mut UIRenderer,
    cache: &mut UICacheManager,
    ui_root: &mut UIRoot,
    offscreen: &GpuTexture,
    atlas_pipeline: &GpuRenderPipeline,
    atlas_sampler: &GpuSampler,
    blit_pipeline: &GpuRenderPipeline,
    blit_sampler: &GpuSampler,
    scale_factor: f64,
    video: Option<&GpuTexture>,
    video_source_dims: (f32, f32),
    gizmos: Option<MainGizmos<'_>>,
) {
    // ── Panel cache update: paint only dirty panels into the persistent
    // atlas via LoadOp::Load. The cache manager doesn't own the UITree, so
    // this is the sole place atlas dirt is cleared.
    let mut subs = std::mem::take(&mut ui_root.panel_subs_scratch);
    let mut panels = manifold_core::scratch::recycle_vec(std::mem::take(&mut ui_root.panel_info_scratch));
    ui_root.panel_cache_info_into(&mut subs, &mut panels);
    let (_, rendered_ranges) =
        cache.render_dirty_panels(device, ui_renderer, &ui_root.tree, &panels);
    clear_painted_dirt(ui_root, rendered_ranges);
    ui_root.panel_info_scratch = manifold_core::scratch::recycle_vec(panels);
    ui_root.panel_subs_scratch = subs;

    let sf = scale_factor as f32;
    let mut encoder = device.create_encoder("Frame");

    // Pass 1: Clear to black.
    encoder.clear_texture(offscreen, 0.0, 0.0, 0.0, 1.0);

    // Pass 2: Atlas blit fullscreen (UI panels onto black).
    if let Some(atlas) = cache.atlas_texture() {
        encoder.draw_fullscreen(
            atlas_pipeline,
            offscreen,
            &[
                manifold_gpu::GpuBinding::Texture { binding: 0, texture: atlas },
                manifold_gpu::GpuBinding::Sampler { binding: 1, sampler: atlas_sampler },
            ],
            false,
            true,
            "Atlas Blit",
        );
    }

    // Pass 3: Blit compositor output ON TOP of atlas in video area
    // (aspect-fit). Compositor replaces whatever is in the video rect
    // (opaque, no blend). `None` in the harness (D8 gap #2).
    let fit = video_fit_rect(ui_root.layout.video_area(), video_source_dims);
    if let Some(compositor_tex) = video
        && let Some(fit) = fit
    {
        encoder.draw_fullscreen_viewport(
            blit_pipeline,
            offscreen,
            &[
                manifold_gpu::GpuBinding::Texture { binding: 0, texture: compositor_tex },
                manifold_gpu::GpuBinding::Sampler { binding: 1, sampler: blit_sampler },
            ],
            (fit.x * sf, fit.y * sf, fit.width * sf, fit.height * sf),
            manifold_gpu::GpuLoadAction::Load,
            "Blit Compositor",
        );
    }

    // Pass 3b: the Scene panel's gizmos over the show picture, through the
    // camera that rendered it (REALTIME_3D_DESIGN.md D7a). Editor chrome only.
    if let Some(gizmos) = gizmos
        && video.is_some()
        && let Some(fit) = fit
    {
        // The selected object's outline under the lines (D-102).
        if let Some((outline, mask)) = gizmos.outline {
            outline.draw(&mut encoder, offscreen, mask, (fit.x * sf, fit.y * sf, fit.width * sf, fit.height * sf), sf);
        }
        ui_renderer.begin_frame();
        ui_renderer.push_immediate_clip(fit.x, fit.y, fit.width, fit.height);
        let view = gizmos.view;
        crate::scene_viewport::draw_world_lines(ui_renderer, fit, &view.camera, view.width, view.height, gizmos.lines);
        ui_renderer.pop_immediate_clip();
        let (logical_w, logical_h) = (
            (offscreen.width as f64 / scale_factor) as u32,
            (offscreen.height as f64 / scale_factor) as u32,
        );
        if ui_renderer.prepare(device, logical_w, logical_h, scale_factor) {
            ui_renderer.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
        }
    }

    encoder.commit();
}

/// The Scene panel's gizmo overlay for one main-window frame: the preview
/// host's composed lines and the camera and output size of the frame shown.
pub(crate) struct MainGizmos<'a> {
    pub lines: &'a [manifold_nodes_scene::node_graph::viewport_overlay::WorldLine],
    pub view: crate::scene_gizmo::GizmoView,
    /// The selection outline pass and the shown picture's mask (D-102).
    pub outline: Option<(&'a crate::selection_outline::SelectionOutline, &'a manifold_gpu::GpuTexture)>,
}

/// Where the show picture sits inside `area`: aspect-fit and centred, in
/// logical units. `None` for an empty area or source.
pub(crate) fn video_fit_rect(area: manifold_ui::Rect, (source_w, source_h): (f32, f32)) -> Option<manifold_ui::Rect> {
    let source_aspect = if source_h > 0.0 { source_w / source_h } else { 0.0 };
    if area.width <= 0.0 || area.height <= 0.0 || source_aspect <= 0.0 {
        return None;
    }
    let (fit_w, fit_h) = if source_aspect > area.width / area.height {
        (area.width, area.width / source_aspect)
    } else {
        (area.height * source_aspect, area.height)
    };
    Some(manifold_ui::Rect::new(area.x + (area.width - fit_w) * 0.5, area.y + (area.height - fit_h) * 0.5, fit_w, fit_h))
}

/// The `Application` field bundle the VQT (Audio Setup spectrogram
/// waterfall) pass reads/mutates. All six `&mut` fields are per-Application
/// persistent state (created lazily, rebuilt on bin-count/size change) —
/// bundled here so the seam can own the pass without borrowing
/// `Application` itself. `content_*` are the content-thread-published
/// scalars the pass only reads (`ContentState::spectrogram_num_bins` etc.);
/// `scope_cursor_y` is resolved caller-side from the live-only
/// `Application::scope_hover_uv()` (`-1.0` sentinel = not hovering — the
/// live app's own not-hovering case, not a harness stand-in).
/// Precedent: `app_render.rs` (pre-extraction) :4518-4662.
pub(crate) struct VqtPassState<'a> {
    pub spectrogram: &'a mut Option<Spectrogram>,
    pub spectrogram_pane: &'a mut Option<TexturePane>,
    pub spectrogram_num_bins: &'a mut usize,
    pub spectrogram_tex_dims: &'a mut (u32, u32),
    pub pending_spectrogram_columns: &'a mut Vec<f32>,
    pub pending_spectrogram_scalars: &'a mut Vec<ScopeColumn>,
    pub content_num_bins: usize,
    pub content_fmin: f32,
    pub content_fmax: f32,
    pub content_low_hz: f32,
    pub content_mid_hz: f32,
    pub scope_cursor_y: f32,
    /// P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` section 7.2 item 5):
    /// the band a currently-open fire-mode drawer is reading, if any — `Some`
    /// dims the spectrum outside it, `None` (no drawer open, or Full band)
    /// dims nothing. Caller-resolved from `UIRoot::open_fire_mode_drawer_band`.
    pub band_dim: Option<manifold_ui::types::AudioBand>,
}

/// Pass 4b″ clip-thumbnail render inputs: the persistent renderer, the
/// content-thread atlas texture, and this frame's resolved quad list —
/// atlas and quads are caller-resolved (content-thread atlas live;
/// `thumbs::make_test_atlas`/`build_quads` headless via `--thumbs`; both
/// absent → `None`, section 3).
pub(crate) struct ThumbPass<'a> {
    pub gpu: &'a mut ClipThumbGpu,
    pub atlas: &'a GpuTexture,
    pub quads: &'a [ThumbQuad],
}

/// A landing-flash-in-progress: `(progress 0..1, beat, min_layer,
/// max_layer)` — the exact shape `InteractionOverlay::landing_flash()`
/// returns (see module doc deviation #7).
pub(crate) type LandingFlash = (f32, Beats, usize, usize);

/// Caller-resolved per-pass data for [`render_main_ui_passes`]. Every
/// `Option`/empty field is a legitimate "input absent"
/// (`HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` section 3): the live app fills what it
/// has this frame; the harness fills the subset it can resolve headless and
/// leaves the rest `None`/empty. A pass whose input is absent skips itself
/// — the live app skips the same pass on a frame whose own input happens to
/// be absent (no open modal → no overlay pass, etc.).
pub(crate) struct MainUiPassInputs<'a> {
    // Pass 4a grid + 4c lane/overview/collapsed bitmaps.
    pub layer_bitmap_gpu: Option<&'a mut LayerBitmapGpu>,
    // Pass 4b clip bodies — resolved WITH live drag lift/settle/ghost/
    // split-flick (harness: plain rects, no drag). `clip_rects` carries the
    // SAME drag-adjusted rects reused by waveforms/thumbnails/names so the
    // whole clip moves together.
    pub clip_bodies: &'a [ClipBody],
    pub clip_rects: &'a [ClipScreenRect],
    // Pass 4b' waveforms.
    pub clip_content_gpu: Option<&'a mut ClipContentGpu>,
    // Pass 4b" thumbnails.
    pub thumb: Option<ThumbPass<'a>>,
    // Pass 5 timeline overlays + names + lanes + playhead + scrollbar.
    pub timeline_overlays: TimelineOverlays,
    pub markers: &'a [(f32, Color32)],
    pub landing_flash: Option<LandingFlash>,
    /// `ContentState::automation_latched_params`; lane geometry itself is
    /// read from `ui_root.viewport`'s cache.
    pub automation_latched: &'a [(manifold_core::EffectId, manifold_core::effects::ParamId)],
    pub automation_selection: Option<&'a manifold_ui::UIState>,
    pub cursor_pos: Vec2, // scrollbar hover
    // Pass 5 text-input overlay (card-drag ghost + overlay_draw come off
    // ui_root).
    pub text_input: &'a crate::text_input::TextInputState,
    pub frame_timer: &'a crate::frame_timer::FrameTimer,
    // VQT waterfall — live-only spectrogram state bundled; None headless.
    pub vqt: Option<&'a mut VqtPassState<'a>>,
    // Shared blit resources (VQT blit).
    pub blit_pipeline: &'a GpuRenderPipeline,
    pub blit_sampler: &'a GpuSampler,
    // Async GPU-time sink for the offscreen "Frame" buffer, attached to this
    // seam's command-buffer completion handler before commit. `Some` only when
    // the live app's `MANIFOLD_UI_FRAME_PROFILE` profiler is enabled
    // (`UiFrameProfile::gpu_sink()`); `None` headless — no perf HUD there
    // (section 3 input presence, not caller identity).
    pub gpu_sink: Option<(std::sync::Arc<std::sync::atomic::AtomicU64>, std::sync::Arc<std::sync::atomic::AtomicU64>)>,
}

/// Owns the full main-window immediate-pass assembly (Passes 4a→5 + the VQT
/// waterfall + the overlay-region dirty-clear) on its own encoder, called
/// AFTER [`composite_main_ui_frame`]. The single owner of pass order and
/// per-pass render-call choice, called by the live app
/// (`present_all_windows`) and the headless harness (`render_ui_to_png`,
/// `script.rs`'s `Runner`). Branches only on input presence (an
/// absent/empty field on `inputs`), never on caller identity
/// (`HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` section 3): every skip here is a skip
/// the live app itself takes on a frame whose own input happens to be
/// absent. Commits its own encoder (mirrors `composite_main_ui_frame`'s
/// internal commit) — the caller creates no encoder of its own for these
/// passes.
/// Precedent: `present_all_windows` (pre-extraction) :3920-4695; deletes the
/// harness's `draw_immediate_passes` and its overlay pass (BUG-097, closed
/// by construction — see `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` section 4 step
/// 1, not point-fixed).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_main_ui_passes(
    device: &GpuDevice,
    ui_renderer: &mut UIRenderer,
    ui_root: &mut UIRoot,
    offscreen: &GpuTexture,
    logical_w: u32,
    logical_h: u32,
    scale: f64,
    inputs: MainUiPassInputs<'_>,
) {
    let MainUiPassInputs {
        mut layer_bitmap_gpu,
        clip_bodies,
        clip_rects,
        mut clip_content_gpu,
        thumb,
        timeline_overlays,
        markers,
        landing_flash,
        automation_latched,
        automation_selection,
        cursor_pos,
        text_input,
        frame_timer,
        vqt,
        blit_pipeline,
        blit_sampler,
        gpu_sink,
    } = inputs;

    let sf = scale as f32;
    let mut encoder = device.create_encoder("Frame");

    // Pass 4a: Per-layer grid bitmaps (grid + top separator), under the clip
    // bodies — opaque bodies occlude the grid, it shows through the gaps.
    let layer_rects = ui_root.viewport.layer_bitmap_rects();
    if let Some(bg_gpu) = layer_bitmap_gpu.as_mut()
        && !layer_rects.is_empty()
    {
        bg_gpu.render_layers(device, &mut encoder, offscreen, logical_w, logical_h, &layer_rects);
    }

    // Pass 4b: GPU clip bodies — rounded gradient tiles with a lift-on-select
    // shadow, in their own UIRenderer prepare/render cycle (reusing the shared
    // SDF rect pipeline). `clip_bodies` is the caller's resolved visible-clip
    // list, so only on-screen clips cost anything.
    let tracks = ui_root.viewport.get_tracks_rect();
    if !clip_bodies.is_empty() {
        {
            // D7 lane-content choke point — see
            // `docs/TIMELINE_INTERACTION_P1_SPEC.md` and
            // `UIRenderer::lane_content_scissor`'s doc comment.
            let mut scissor = ui_renderer.lane_content_scissor(tracks);
            manifold_ui_paint::clip_draw::emit_clips(&mut scissor, clip_bodies);
        }
        if ui_renderer.prepare(device, logical_w, logical_h, scale) {
            ui_renderer.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
        }
    }

    // Pass 4b': Per-clip waveform textures, painted INSIDE the audio-clip
    // bodies (section 24 5b). Reuses the visible-clip list resolved in 4b; only audio
    // clips with a decoded waveform cost anything, and a still timeline
    // re-uploads nothing (textures are cached per clip).
    if let Some(content_gpu) = clip_content_gpu.as_mut()
        && !clip_rects.is_empty()
    {
        content_gpu.render(device, &mut encoder, offscreen, logical_w, logical_h, sf, tracks, clip_rects);
    }

    // Pass 4b″: Clip thumbnails (section 24 5c) — blit each visible generator/video
    // clip's atlas cell (published by the content thread, or a labeled test
    // fixture headless) into its body, after the waveform pass, centre-cropped
    // to the body aspect and masked to the rounded shape. Atlas + quads are
    // caller-resolved (section 3); this pass only decides whether to draw.
    if let Some(thumb) = thumb
        && !thumb.quads.is_empty()
    {
        thumb.gpu.render(device, &mut encoder, offscreen, logical_w, logical_h, sf, tracks, thumb.atlas, thumb.quads);
    }

    // Pass 4c: The lane / stem / overview / collapsed-group panel bitmaps.
    // These are separate regions (below / beside the tracks, or collapsed group
    // rows that carry no clips), so their z-order vs the clips is moot — they
    // ride the same single layer-bitmap instance as the grid, indices 1000+.
    if let Some(bitmap_gpu) = layer_bitmap_gpu.as_mut() {
        let mut rects: Vec<(usize, manifold_ui::node::Rect)> = Vec::new();

        let ov_rect = ui_root.viewport.overview_rect();
        if ov_rect.width > 0.0 && ov_rect.height > 0.0 {
            rects.push((1002, ov_rect));
        }

        // Collapsed group bitmaps
        rects.extend(ui_root.viewport.collapsed_group_rects());

        // LED composite preview band on the DMX lane's generator card
        // (LED_STRIPS_DESIGN MVP-P4) — index 1003, uploaded when the card's
        // dirty flag fires. Absent/hidden → no quad.
        if let Some(rect) = ui_root.inspector.led_preview_rect(&ui_root.tree) {
            rects.push((manifold_ui::panels::param_card::LED_BAND_BITMAP_INDEX, rect));
        }

        if !rects.is_empty() {
            bitmap_gpu.render_layers(device, &mut encoder, offscreen, logical_w, logical_h, &rects);
        }
    }

    // Pass 5: Overlay UI (playhead, HUD, dropdowns, text). Its own cycle —
    // begin_frame here resets the text pool that the Pass 4b clip cycle did
    // not touch, isolating the two prepare/render cycles cleanly.
    ui_renderer.begin_frame();

    // Region / cursor / markers, scissored to the tracks rect, UNDER the
    // clip names (bottom→top: region, cursor, markers — matches the old
    // bitmap paint order). All sit on top of the clip bodies + waveforms.
    // D7 lane-content choke point (see above).
    {
        let mut scissor = ui_renderer.lane_content_scissor(tracks);
        if let Some((r, c)) = timeline_overlays.region {
            scissor.draw_rect(r.x, r.y, r.width, r.height, c);
        }
        if let Some((r, c)) = timeline_overlays.cursor {
            scissor.draw_rect(r.x, r.y, r.width, r.height, c);
        }
        for (x, c) in markers {
            scissor.draw_rect(*x, tracks.y, 1.0, tracks.height, *c);
        }
        // D15 "landing-line flash" — a brief vertical line at the beat a
        // move-drag settled to, spanning the dragged selection's layer
        // range; drawn through the D7 lane-content scissor.
        if let Some((progress, beat, min_layer, max_layer)) = landing_flash {
            let x = ui_root.viewport.beat_to_pixel(beat);
            let y0 = ui_root.viewport.track_y(min_layer);
            let y1 = ui_root.viewport.track_y(max_layer) + ui_root.viewport.track_height(max_layer);
            if y1 > y0 {
                let alpha = ((1.0 - progress) * 230.0).round().clamp(0.0, 255.0) as u8;
                let c = manifold_ui::color::with_alpha(manifold_ui::color::INSERT_CURSOR_BLUE, alpha);
                scissor.draw_rect(x - 1.0, y0, 2.0, y1 - y0, c);
            }
        }
    }

    // Clip name labels (section 24 5b) — on top of the bodies + waveforms, at
    // BASE depth (under the dropdown/modal overlays). Reuses the visible
    // clip list resolved for the Pass 4b body emission this frame.
    manifold_ui_paint::clip_draw::emit_clip_names(ui_renderer, clip_rects, tracks);

    // Automation lane strips (P4, `docs/AUTOMATION_LANES_DESIGN.md` section 7) —
    // on top of the clip names, same overlay pass. Empty whenever
    // automation mode is off (the caller never populated any lanes this
    // frame), so this is a no-op cost in the common case.
    manifold_ui_paint::automation_lane_draw::emit_automation_lanes(
        ui_renderer,
        ui_root.tree.measurer(),
        ui_root.viewport.automation_lane_screens(),
        tracks,
        automation_selection,
        automation_latched,
    );

    // Playhead — a red line spanning ruler + tracks, capped by a downward
    // triangle head at the top of the ruler (section 24 5e). The head is the
    // single dominant "now" marker so it never competes with the blue
    // insert cursor for "where am I".
    if let Some(px) = ui_root.viewport.playhead_pixel() {
        let ruler = ui_root.viewport.ruler_rect();
        let tr = ui_root.viewport.get_tracks_rect();
        let top = ruler.y;
        let height = (tr.y + tr.height) - top;
        ui_renderer.draw_rect(
            px - 1.0,
            top,
            manifold_ui::color::PLAYHEAD_WIDTH,
            height,
            manifold_ui::color::PLAYHEAD_RED,
        );
        let s = manifold_ui::color::PLAYHEAD_HEAD_SIZE;
        ui_renderer.draw_icon(
            manifold_ui::icons::Icon::Playhead.id(),
            px - s * 0.5,
            top,
            s,
            s,
            manifold_ui::color::PLAYHEAD_RED,
            None,
        );
    }

    // Horizontal scrollbar (section 24 5e): a slim track + draggable thumb in the
    // reserved strip below the tracks. Geometry comes from the viewport —
    // the same source the drag hit-test uses — so the drawn thumb and the
    // grabbable region can't drift. Drawn here as GPU rects, like the
    // playhead. Hidden (None) when the whole timeline fits.
    if let Some((track, thumb)) = ui_root.viewport.scrollbar_h_layout() {
        ui_renderer.draw_rect(
            track.x,
            track.y,
            track.width,
            track.height,
            manifold_ui::color::SCROLLBAR_TRACK_C32,
        );
        let active = ui_root.viewport.scrollbar_h_dragging() || thumb.contains(cursor_pos);
        let thumb_color = if active {
            manifold_ui::color::SCROLLBAR_THUMB_HOVER_C32
        } else {
            manifold_ui::color::SCROLLBAR_THUMB_C32
        };
        let radius = (thumb.height * 0.5).min(thumb.width * 0.5);
        ui_renderer.draw_rounded_rect(thumb.x, thumb.y, thumb.width, thumb.height, thumb_color, radius);
    }

    // Tree-overlay pass (`EDITOR_WINDOW_UNIFICATION_DESIGN.md` D1, P1): browser-
    // popup thumbnail registration, the overlay_draw loop, and the TOOLTIP
    // tier (card-drag ghost + text-input overlay), shared with the
    // graph-editor window — see `tree_passes.rs`. `text_input` is `Some`
    // only when the active session belongs to THIS window (D4) — the
    // `.active` gate that used to live here now lives inside
    // `is_owned_by_main`.
    crate::tree_passes::render_tree_overlay_passes(
        device,
        ui_renderer,
        ui_root,
        logical_w,
        logical_h,
        crate::tree_passes::TreeOverlayInputs {
            text_input: text_input.is_owned_by_main().then_some(text_input),
            frame_timer,
        },
    );

    // Audio Setup spectrogram waterfall — live content of a docked,
    // base-layer panel. Blitted here, after the panel atlas and BEFORE the
    // popup flush below, so every popup layer (dropdowns, the browser,
    // menus, tooltips) covers it the same way it covers the panel itself.
    // Render the live VQT columns into a UI-device texture, then blit it
    // into the panel's reserved scope rect.
    if ui_root.audio_setup_panel.is_open()
        && let Some(vqt) = vqt
        && vqt.content_num_bins > 0
        && let Some(rect) = ui_root.audio_setup_panel.scope_rect()
    {
        let num_bins = vqt.content_num_bins;
        let cfg = SpectrogramConfig::default();

        // Render the waterfall at the scope's physical-pixel size so it stays
        // crisp at any (resizable) modal size — the shader supersamples the
        // column ring directly to display resolution. Clamped to bound VRAM.
        let tex_w = ((rect.width * sf).round() as u32).clamp(256, 4096);
        let tex_h = ((rect.height * sf).round() as u32).clamp(128, 4096);

        // (Re)create the renderer if the bin count or the on-screen width
        // changed. The ring is sized to the texture's pixel width so each
        // column owns one pixel (crisp 1:1 sweep, no resampling).
        let cur_cols = vqt.spectrogram.as_ref().map(|s| s.num_cols());
        if cur_cols != Some(tex_w as usize) || *vqt.spectrogram_num_bins != num_bins {
            // Drop buffered columns — chunking them at the new `num_bins`
            // would misalign, and a width change resets the sweep anyway.
            // The overlay records must drop WITH them: they pair 1:1 by
            // position, so clearing one side leaves stale records pairing
            // with fresh columns and the overlay slides out of time
            // against the waterfall until the backlog flushes.
            vqt.pending_spectrogram_columns.clear();
            vqt.pending_spectrogram_scalars.clear();
            *vqt.spectrogram = Some(Spectrogram::new(
                device,
                num_bins,
                tex_w as usize,
                manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                cfg.db_min,
                cfg.db_max,
                cfg.tilt_slope,
            ));
            *vqt.spectrogram_num_bins = num_bins;
        }
        // (Re)create the target pane when the scope's pixel size changes
        // (modal resize) or it doesn't exist yet.
        if vqt.spectrogram_pane.is_none() || *vqt.spectrogram_tex_dims != (tex_w, tex_h) {
            let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: tex_w,
                height: tex_h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label: "Audio Scope",
                mip_levels: 1,
            });
            *vqt.spectrogram_pane = Some(TexturePane::local(tex));
            *vqt.spectrogram_tex_dims = (tex_w, tex_h);
        }

        // Cursor frequency-line position (uv.y), resolved caller-side.
        // Negative = not hovering.
        let scope_cursor_y = vqt.scope_cursor_y;
        if let (Some(spectrogram), Some(pane)) = (vqt.spectrogram.as_mut(), vqt.spectrogram_pane.as_mut())
            && let Some(target) = pane.local_target().cloned()
        {
            // Feed new columns (post-gain magnitudes from the worker), each
            // exactly once, then clear — see `pending_spectrogram_columns`.
            // The overlay records ride in lockstep (one ScopeColumn per
            // column); a column with no matching record (shouldn't happen)
            // gets the hide sentinel.
            let mut scalars = vqt.pending_spectrogram_scalars.iter();
            for col in vqt.pending_spectrogram_columns.chunks_exact(num_bins) {
                let record = scalars.next().copied().unwrap_or(ScopeColumn::EMPTY);
                spectrogram.push_column(col, record);
            }
            vqt.pending_spectrogram_columns.clear();
            vqt.pending_spectrogram_scalars.clear();
            // Band dividers at the editable low/mid + mid/high crossovers (the
            // modulation's Low/Mid/High splits), as normalised y on the log
            // axis. Drag-retuned live via the Audio Setup scope.
            let (fmin, fmax) = (vqt.content_fmin, vqt.content_fmax);
            let y_of = |f: f32| {
                if fmin > 0.0 && fmax > fmin {
                    (f / fmin).log2() / (fmax / fmin).log2()
                } else {
                    -1.0
                }
            };
            // Octave span of the displayed range — drives the pink tilt
            // (centred on the geometric-mean freq). 0 disables it.
            let freq_log_ratio = if fmin > 0.0 && fmax > fmin { (fmax / fmin).log2() } else { 0.0 };
            let lo_yfb = y_of(vqt.content_low_hz);
            let hi_yfb = y_of(vqt.content_mid_hz);
            // Which divider the cursor is over, for the grip-handle hover
            // glow. Use the panel's OWN hit-test (same px tolerance the grab
            // uses) so the glow and the grab zone match exactly — converting
            // the cursor's uv.y back to a screen y. Off-scope cursor (< 0)
            // maps far away → no hover.
            let cursor_screen_y = if scope_cursor_y < 0.0 { -1.0e9 } else { rect.y + scope_cursor_y * rect.height };
            let hovered_divider = ui_root.audio_setup_panel.divider_hover_index(cursor_screen_y);
            // P7 band dimming: the KEPT range in the same [lo_yfb, hi_yfb]
            // normalised space the dividers already use — Low/Mid/High slice
            // the same two crossovers differently; Full (or no drawer open)
            // dims nothing.
            let dim_range = match vqt.band_dim {
                Some(manifold_ui::types::AudioBand::Low) => Some((0.0, lo_yfb)),
                Some(manifold_ui::types::AudioBand::Mid) => Some((lo_yfb, hi_yfb)),
                Some(manifold_ui::types::AudioBand::High) => Some((hi_yfb, 1.0)),
                Some(manifold_ui::types::AudioBand::Full) | None => None,
            };
            spectrogram.render(
                &mut encoder,
                &target,
                [lo_yfb, hi_yfb],
                freq_log_ratio,
                scope_cursor_y,
                hovered_divider,
                dim_range,
            );

            // Blit through the unified TexturePane path (logical rect + scale).
            crate::texture_pane::blit_texture_pane(
                pane,
                device,
                &mut encoder,
                blit_pipeline,
                blit_sampler,
                offscreen,
                (rect.x, rect.y, rect.width, rect.height),
                sf,
                "Audio Scope Blit",
            );
        }
    }

    // Flush all overlay commands
    if ui_renderer.prepare(device, logical_w, logical_h, scale) {
        ui_renderer.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
    }

    // ── Commit offscreen render ──
    // Capture the offscreen buffer's TRUE GPU execution time async — tells the
    // profiler whether next_drawable's block is our own GPU work (heavy
    // offscreen) or external starvation (content thread hogging the shared
    // GPU). `Some` only when the live app's frame profiler is enabled; `None`
    // headless (section 3 input presence). Precedent: `present_all_windows`
    // (pre-extraction) :4671-4676, which timed this same "Frame" encoder.
    if let Some((sink_us, sink_n)) = gpu_sink {
        encoder.add_gpu_time_handler(move |secs| {
            sink_us.fetch_add((secs * 1_000_000.0) as u64, std::sync::atomic::Ordering::Relaxed);
            sink_n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
    encoder.commit();

    // The Overlays and Ghosts arenas' dirty-clear (the atlas regions are
    // cleared per rendered range by `composite_main_ui_frame` — BUG-015) runs INSIDE
    // `tree_passes::render_tree_overlay_passes`, above, before this commit —
    // see that module's doc deviation 1 for why the reordering relative to
    // the commit is safe.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BUG-5y9y4 (region roots stay dirty): a region's root sits outside its
    /// panel's range, so clearing only the painted ranges left it dirty and
    /// the idle main window recomposited every frame. Clearing what a slot
    /// painted clears its whole arena.
    #[test]
    fn a_painted_frame_leaves_no_atlas_dirt() {
        let mut ui = UIRoot::new();
        ui.resize(1280.0, 800.0);
        assert!(ui.atlas_dirty(), "a fresh build is dirty");
        let (mut subs, mut infos) = (Vec::new(), Vec::new());
        ui.panel_cache_info_into(&mut subs, &mut infos);
        let painted: Vec<_> = infos.iter().map(|p| (p.region, p.start, p.end)).collect();
        clear_painted_dirt(&mut ui, &painted);
        assert!(!ui.atlas_dirty(), "an idle frame after a paint has nothing to repaint");
    }

    /// INV-5 (only the frame planner builds): outside the frame seam
    /// (`ui_frame.rs`), the editor window's seam (`editor_frame.rs`) and
    /// `UIRoot::build` itself, no production code builds, clears, moves or
    /// invalidates a region. Test items and the ui-snapshot harness are
    /// skipped.
    #[test]
    fn only_the_frame_planner_builds() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let calls = [
            "ui_root.build()",
            "ui.build()",
            "self.build()",
            ".build_region(",
            ".clear_region(",
            ".translate_region(",
            ".invalidate_slot(",
        ];
        let mut offenders = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
                if path.is_dir() {
                    if rel != "ui_snapshot" && !rel.ends_with("/tests") && rel != "tests" {
                        stack.push(path);
                    }
                    continue;
                }
                if !rel.ends_with(".rs")
                    || rel.ends_with("tests.rs")
                    || matches!(rel.as_str(), "ui_frame.rs" | "editor_frame.rs")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                if text.lines().any(|l| l.trim() == "#![cfg(test)]") {
                    continue;
                }
                for (line_no, line) in production_lines(&text) {
                    let code = line.split("//").next().unwrap_or("");
                    for call in calls {
                        // `UIRoot::build` delegates to `build_region`.
                        let delegate = rel == "ui_root/mod.rs" && call == ".build_region(";
                        if code.contains(call) && !delegate {
                            offenders.push(format!("{rel}:{line_no}: {}", line.trim()));
                        }
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "region work outside the frame planner:\n{}", offenders.join("\n"));
    }

    /// The lines of `text` outside any item gated on `test` (a
    /// `#[cfg(test)]` or `#[cfg(any(test, ..))]` attribute and the braces of
    /// the item it gates).
    fn production_lines(text: &str) -> Vec<(usize, &str)> {
        let mut out = Vec::new();
        let mut pending = false;
        let mut depth = 0i32;
        let mut opened = false;
        for (i, line) in text.lines().enumerate() {
            let t = line.trim();
            if depth == 0 && !pending && (t.starts_with("#[cfg(test)]") || t.starts_with("#[cfg(any(test")) {
                pending = true;
                continue;
            }
            if pending {
                for c in line.chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if opened && depth <= 0 || !opened && t.ends_with(';') {
                    pending = false;
                    opened = false;
                    depth = 0;
                }
                continue;
            }
            out.push((i + 1, line));
        }
        out
    }
}
