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
//! ── Deviations from the design doc's §4 committed signatures
//! (`UI_HARNESS_UNIFICATION_DESIGN.md` §4), found at VERIFY-AT-IMPL and
//! reported to the orchestrator rather than silently absorbed — see the P1
//! phase report for the full escalation writeup:
//!
//! 1. `apply_ui_frame_invalidations`'s `cache` parameter is
//!    `Option<&mut UICacheManager>`, not `&mut UICacheManager` as drafted.
//!    `tick_and_render` runs every UI tick regardless of GPU/window
//!    readiness (no `self.gpu`-gated early return upstream of the
//!    invalidation block, unlike `present_all_windows`), and
//!    `self.ui_cache_manager` is genuinely `None` before GPU init
//!    completes. The original code reflects this: `ui_root.build()` /
//!    `rebuild_scroll_panels()` always run; only the `cache.invalidate_*`
//!    calls are individually wrapped in `if let Some(cm) = ...`
//!    (app_render.rs:960-964, 2827, 2843, 2849, pre-extraction). A
//!    non-Option `cache` would force either skipping `ui_root.build()`
//!    when the cache doesn't exist yet (a real behavior change — the tree
//!    wouldn't be built pre-GPU-init) or a duplicated inline fallback at
//!    the call site (reintroducing the exact drift D3 exists to kill).
//! 2. Both functions take `ui_root: &mut UIRoot`, not `&UIRoot`. Each
//!    performs a mutation `UIRoot` itself owns: `apply_ui_frame_invalidations`
//!    calls `ui_root.build()` / `rebuild_scroll_panels()`;
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
//!    after consuming it (not documented as a write-back field in §4 — only
//!    `needs_rebuild` was). The live App builds `signals` fresh every tick
//!    so this is a no-op for it either way; the P0 harness reuses one
//!    `UiFrameSignals` across many simulated frames, and without the clear
//!    a stale `true` would replay `invalidate_inspector()` on every later
//!    frame until the next real scroll. Harmless to pixels (repainting
//!    already-correct content is a no-op visually) but not a faithful
//!    one-shot signal, so it's cleared like `needs_rebuild` is.
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
//! §4 step 2) — additional deviations found at VERIFY-AT-IMPL:
//!
//! 6. Profiling is split by value, not deleted. The async **GPU-time sink**
//!    IS preserved: it's threaded in as `MainUiPassInputs::gpu_sink`
//!    (`Option<(Arc<AtomicU64>, Arc<AtomicU64>)>`, fed by
//!    `UiFrameProfile::gpu_sink()`; `None` headless — §3 input presence) and
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
//!    `#[cfg(not(target_os = "macos"))]` arm. Input-presence branching (§3),
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
use manifold_renderer::clip_content_gpu::ClipContentGpu;
use manifold_renderer::clip_draw::ClipBody;
use manifold_renderer::clip_thumb_gpu::{ClipThumbGpu, ThumbQuad};
use manifold_renderer::layer_bitmap_gpu::LayerBitmapGpu;
use manifold_renderer::ui_cache_manager::UICacheManager;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_spectral::{ScopeColumn, Spectrogram, SpectrogramConfig};
use manifold_ui::node::{Color32, Vec2};
use manifold_ui::panels::viewport::{AutomationLaneScreen, ClipScreenRect, TimelineOverlays};

use crate::texture_pane::TexturePane;
use crate::ui_root::{ScrollDirty, UIRoot};

/// The per-frame UI dirty signals the invalidation decision reads. Filled by
/// the caller from its own state (the live App from `self.needs_rebuild`
/// etc.; the harness from the gesture it just applied).
#[derive(Default)]
pub(crate) struct UiFrameSignals {
    pub needs_rebuild: bool,
    pub needs_structural_sync: bool,
    pub scroll_dirty: ScrollDirty,
    pub scrolled_in_place: bool, // the scroll-in-place path (app_render.rs, pre-extraction :960-964)
}

/// Owns the invalidate/rebuild decision block. Reads `ui_root`'s drag guards
/// and applies `build()` / `rebuild_scroll_panels()` and the matching
/// `UICacheManager::invalidate_*` exactly as `tick_and_render` did before
/// extraction. THE single place these decisions live.
/// Precedent: app_render.rs (pre-extraction) :2819-2852 + :960-964, moved,
/// not rewritten.
///
/// `signals` is `&mut` because the block WRITES BACK: it clears
/// `needs_rebuild` when it rebuilds, and deliberately KEEPS it set when an
/// active inspector/layer drag defers the rebuild to the next frame. The
/// live caller copies the residual flags back into its own state after the
/// call; the harness carries them to its next frame.
///
/// `cache` is `Option<&mut UICacheManager>` — see module doc deviation #1.
pub(crate) fn apply_ui_frame_invalidations(
    ui_root: &mut UIRoot,
    mut cache: Option<&mut UICacheManager>,
    signals: &mut UiFrameSignals,
) {
    // An in-place inspector scroll (wheel in window_input, or a scrollbar
    // drag handled inside process_events) offset the content nodes without a
    // rebuild — re-render just the inspector's atlas slot. A full rebuild
    // later this frame (needs_rebuild → invalidate_all) supersedes it
    // harmlessly. One-shot, like `needs_rebuild`: cleared here so a caller
    // that reuses the same `UiFrameSignals` across frames (the P0 harness)
    // doesn't replay a stale scroll on every later frame — the live App
    // doesn't need this (it builds `signals` fresh every tick) but the
    // harness does, and the field means the same thing for both.
    if signals.scrolled_in_place {
        signals.scrolled_in_place = false;
        if let Some(cm) = &mut cache {
            cm.invalidate_inspector();
        }
    }

    // GUARD: If the inspector has an active drag (slider being dragged),
    // defer the rebuild to prevent node destruction mid-drag which causes
    // snap-back.
    let inspector_dragging = ui_root.inspector.is_dragging();
    let layer_dragging = ui_root.layer_headers.is_dragging();
    if signals.needs_rebuild || signals.needs_structural_sync {
        if inspector_dragging {
            // Defer — keep needs_rebuild set so it fires after drag ends.
            // But still rebuild scroll panels if needed (they're separate
            // from the inspector).
            if signals.scroll_dirty.any() {
                ui_root.rebuild_scroll_panels(signals.scroll_dirty);
                if let Some(cm) = &mut cache {
                    cm.invalidate_scroll_panels();
                }
            }
        } else if layer_dragging {
            // Defer — rebuilding scroll panels while a layer drag is active
            // would destroy the node IDs that handle_drag / handle_drag_end
            // depend on.
        } else {
            signals.needs_rebuild = false;
            ui_root.build();
            // Re-apply effect card selection visuals after rebuild —
            // structural changes recreate cards with is_selected=false.
            ui_root.inspector.apply_selection_visuals(&mut ui_root.tree);
            if let Some(cm) = &mut cache {
                cm.invalidate_all();
            }
        }
    } else if signals.scroll_dirty.any() && !layer_dragging {
        ui_root.rebuild_scroll_panels(signals.scroll_dirty);
        if let Some(cm) = &mut cache {
            cm.invalidate_scroll_panels();
        }
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
) {
    // ── Panel cache update: paint only dirty panels into the persistent
    // atlas via LoadOp::Load. The cache manager doesn't own the UITree, so
    // this loop is the sole place panel-range dirty-flag clearing happens —
    // narrowed so out-of-sub-region panel dirt isn't silently erased before
    // the cache's fallback-to-full-render can fire (BUG-015).
    let panel_infos = ui_root.panel_cache_info();
    let (_, rendered_ranges) =
        cache.render_dirty_panels(device, ui_renderer, &ui_root.tree, &panel_infos);
    for (start, end) in &rendered_ranges {
        ui_root.tree.clear_dirty_range(*start, *end);
    }

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
    if let Some(compositor_tex) = video {
        let (comp_w, comp_h) = video_source_dims;
        let source_aspect = if comp_h > 0.0 { comp_w / comp_h } else { 0.0 };
        let video_rect = ui_root.layout.video_area();
        let rect_x = video_rect.x * sf;
        let rect_y = video_rect.y * sf;
        let rect_w = video_rect.width * sf;
        let rect_h = video_rect.height * sf;

        if rect_w > 0.0 && rect_h > 0.0 && source_aspect > 0.0 {
            let rect_aspect = rect_w / rect_h;
            let (fit_w, fit_h) = if source_aspect > rect_aspect {
                (rect_w, rect_w / source_aspect)
            } else {
                (rect_h * source_aspect, rect_h)
            };
            let fit_x = rect_x + (rect_w - fit_w) * 0.5;
            let fit_y = rect_y + (rect_h - fit_h) * 0.5;

            encoder.draw_fullscreen_viewport(
                blit_pipeline,
                offscreen,
                &[
                    manifold_gpu::GpuBinding::Texture { binding: 0, texture: compositor_tex },
                    manifold_gpu::GpuBinding::Sampler { binding: 1, sampler: blit_sampler },
                ],
                (fit_x, fit_y, fit_w, fit_h),
                manifold_gpu::GpuLoadAction::Load,
                "Blit Compositor",
            );
        }
    }

    encoder.commit();
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
    /// P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item 5):
    /// the band a currently-open fire-mode drawer is reading, if any — `Some`
    /// dims the spectrum outside it, `None` (no drawer open, or Full band)
    /// dims nothing. Caller-resolved from `UIRoot::open_fire_mode_drawer_band`.
    pub band_dim: Option<manifold_ui::types::AudioBand>,
}

/// Pass 4b″ clip-thumbnail render inputs: the persistent renderer, the
/// content-thread atlas texture, and this frame's resolved quad list —
/// atlas and quads are caller-resolved (content-thread atlas live;
/// `thumbs::make_test_atlas`/`build_quads` headless via `--thumbs`; both
/// absent → `None`, §3).
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
/// (`HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` §3): the live app fills what it
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
    pub automation_lanes: &'a [AutomationLaneScreen],
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
    // (§3 input presence, not caller identity).
    pub gpu_sink: Option<(std::sync::Arc<std::sync::atomic::AtomicU64>, std::sync::Arc<std::sync::atomic::AtomicU64>)>,
}

/// Owns the full main-window immediate-pass assembly (Passes 4a→5 + the VQT
/// waterfall + the overlay-region dirty-clear) on its own encoder, called
/// AFTER [`composite_main_ui_frame`]. The single owner of pass order and
/// per-pass render-call choice, called by the live app
/// (`present_all_windows`) and the headless harness (`render_ui_to_png`,
/// `script.rs`'s `Runner`). Branches only on input presence (an
/// absent/empty field on `inputs`), never on caller identity
/// (`HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` §3): every skip here is a skip
/// the live app itself takes on a frame whose own input happens to be
/// absent. Commits its own encoder (mirrors `composite_main_ui_frame`'s
/// internal commit) — the caller creates no encoder of its own for these
/// passes.
/// Precedent: `present_all_windows` (pre-extraction) :3920-4695; deletes the
/// harness's `draw_immediate_passes` and its overlay pass (BUG-097, closed
/// by construction — see `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` §4 step
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
        automation_lanes,
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
            manifold_renderer::clip_draw::emit_clips(&mut scissor, clip_bodies);
        }
        if ui_renderer.prepare(device, logical_w, logical_h, scale) {
            ui_renderer.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
        }
    }

    // Pass 4b': Per-clip waveform textures, painted INSIDE the audio-clip
    // bodies (§24 5b). Reuses the visible-clip list resolved in 4b; only audio
    // clips with a decoded waveform cost anything, and a still timeline
    // re-uploads nothing (textures are cached per clip).
    if let Some(content_gpu) = clip_content_gpu.as_mut()
        && !clip_rects.is_empty()
    {
        content_gpu.render(device, &mut encoder, offscreen, logical_w, logical_h, sf, tracks, clip_rects);
    }

    // Pass 4b″: Clip thumbnails (§24 5c) — blit each visible generator/video
    // clip's atlas cell (published by the content thread, or a labeled test
    // fixture headless) into its body, after the waveform pass, centre-cropped
    // to the body aspect and masked to the rounded shape. Atlas + quads are
    // caller-resolved (§3); this pass only decides whether to draw.
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

    // Clip name labels (§24 5b) — on top of the bodies + waveforms, at
    // BASE depth (under the dropdown/modal overlays). Reuses the visible
    // clip list resolved for the Pass 4b body emission this frame.
    manifold_renderer::clip_draw::emit_clip_names(ui_renderer, clip_rects, tracks);

    // Automation lane strips (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7) —
    // on top of the clip names, same overlay pass. Empty whenever
    // automation mode is off (the caller never populated any lanes this
    // frame), so this is a no-op cost in the common case.
    manifold_renderer::automation_lane_draw::emit_automation_lanes(ui_renderer, automation_lanes, tracks);

    // Playhead — a red line spanning ruler + tracks, capped by a downward
    // triangle head at the top of the ruler (§24 5e). The head is the
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

    // Horizontal scrollbar (§24 5e): a slim track + draggable thumb in the
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

    // Flush all overlay commands
    if ui_renderer.prepare(device, logical_w, logical_h, scale) {
        ui_renderer.render(&mut encoder, offscreen, manifold_gpu::GpuLoadAction::Load);
    }

    // Audio Setup spectrogram waterfall. Drawn AFTER the overlay flush so it
    // lands on top of the modal's scope-area background (the modal is an
    // overlay, drawn into `offscreen` just above). Render the live VQT
    // columns into a UI-device texture, then blit it into the panel's
    // reserved scope rect via the unified TexturePane path.
    //
    // Suppressed while a dropdown is open: dropdowns (device / channel) are
    // overlays that expand down over the scope, and this blit lands on top of
    // them — so painting the waterfall would hide the open list. The scope
    // briefly shows its dark background instead, and returns when it closes.
    if ui_root.audio_setup_panel.is_open()
        && !ui_root.dropdown.is_open()
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

    // ── Commit offscreen render ──
    // Capture the offscreen buffer's TRUE GPU execution time async — tells the
    // profiler whether next_drawable's block is our own GPU work (heavy
    // offscreen) or external starvation (content thread hogging the shared
    // GPU). `Some` only when the live app's frame profiler is enabled; `None`
    // headless (§3 input presence). Precedent: `present_all_windows`
    // (pre-extraction) :4671-4676, which timed this same "Frame" encoder.
    if let Some((sink_us, sink_n)) = gpu_sink {
        encoder.add_gpu_time_handler(move |secs| {
            sink_us.fetch_add((secs * 1_000_000.0) as u64, std::sync::atomic::Ordering::Relaxed);
            sink_n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
    encoder.commit();

    // The overlay-region dirty-clear (Panel ranges are already cleared per
    // rendered range by `composite_main_ui_frame`; the 7 panels contiguously
    // tile [0, overlay_region_start), so this and that together clear every
    // non-overlay node — BUG-015) now runs INSIDE
    // `tree_passes::render_tree_overlay_passes`, above, before this commit —
    // see that module's doc deviation 1 for why the reordering relative to
    // the commit is safe.
}
