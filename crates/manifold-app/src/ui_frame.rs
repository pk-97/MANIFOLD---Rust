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

use manifold_gpu::{GpuDevice, GpuRenderPipeline, GpuSampler, GpuTexture};
use manifold_renderer::ui_cache_manager::UICacheManager;
use manifold_renderer::ui_renderer::UIRenderer;

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
