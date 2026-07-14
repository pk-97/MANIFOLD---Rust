//! The shared tree-overlay pass (`EDITOR_WINDOW_UNIFICATION_DESIGN.md` D1,
//! P1) — the single owner of the tree-overlay pass sequence for EVERY
//! `UIRoot`-hosting window: `overlay_draw` loop (per-overlay
//! `Depth::OVERLAY.above(i)`, scrim-skip shadow hook, `render_sub_region`) →
//! TOOLTIP tier (card-drag ghost, text-input overlay) → browser-popup
//! thumbnail registration → overlay-region dirty-clear. Both the main window
//! (`ui_frame::render_main_ui_passes`) and the graph-editor window
//! (`editor_frame::composite_editor_frame`) call this instead of forking
//! their own copy — BUG-151 (the editor's node browser rendering cells but
//! no container fill/scrim) was what "twice, by hand" looked like: the
//! editor's flat `render_tree_range(0, usize::MAX)` root scan swept overlay
//! nodes up at CONTENT depth and never ran this pass at all.
//!
//! App-internal module (no new crate, no new dependency, no thread-residency
//! change) — everything here is `pub(crate)`.
//!
//! Moved VERBATIM from `ui_frame.rs:646-716` + `:889-891` (thumbnail
//! registration, overlay loop, TOOLTIP tier, dirty-clear) — not rewritten.
//! Enqueue-only into `ui_renderer` (the caller owns `begin_frame`, the
//! `prepare`/`render` flush, and the encoder) EXCEPT thumbnail registration,
//! which needs `device`. Precedent: the `ui_frame.rs`/`editor_frame.rs` seam
//! extractions themselves (`UI_HARNESS_UNIFICATION_DESIGN.md` P1/P3).
//!
//! ── Deviations from the design doc's §3 committed signature, found at
//! VERIFY-AT-IMPL:
//!
//! 1. `⚠ VERIFY-AT-IMPL (P1)` resolved as anticipated: the overlay-region
//!    dirty-clear (`ui_root.tree.clear_dirty_range(..)`) now runs INSIDE this
//!    function, before the caller's `prepare`/`render` flush and (on the main
//!    window) before the VQT waterfall — whereas pre-extraction it ran AFTER
//!    the encoder's `commit()`, at the very tail of `render_main_ui_passes`.
//!    This is not order-sensitive: `clear_dirty_range` only mutates
//!    `UITree`'s CPU-side dirty bookkeeping (which nodes still need a
//!    repaint) — it enqueues no GPU commands and reads no GPU state, so
//!    moving it earlier in the same synchronous call sequence changes no
//!    pixel. Verified by the P1 byte-identical main-window readback gate
//!    (I4).

use manifold_gpu::GpuDevice;
use manifold_renderer::ui_renderer::{Depth, UIRenderer};

use crate::ui_root::UIRoot;

/// Caller-resolved inputs. Every `Option` is input PRESENCE (the
/// `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` §3 caller test, extended across
/// windows by D4) — never caller identity. `text_input` is `Some` only when
/// the active session belongs to THIS window
/// (`TextInputState::is_owned_by_main`/`is_owned_by_editor`); this function
/// must never see a `WorkspaceKind`/`is_graph_editor` flag (D4's forbidden
/// move — the ownership decision happens at the caller, not here).
pub(crate) struct TreeOverlayInputs<'a> {
    pub text_input: Option<&'a crate::text_input::TextInputState>,
    pub frame_timer: &'a crate::frame_timer::FrameTimer,
}

/// The single owner of the tree-overlay pass sequence for EVERY
/// `UIRoot`-hosting window. See module doc for the full pass list and the
/// BUG-151 rationale.
pub(crate) fn render_tree_overlay_passes(
    device: &GpuDevice,
    ui_renderer: &mut UIRenderer,
    ui_root: &mut UIRoot,
    logical_w: u32,
    logical_h: u32,
    inputs: TreeOverlayInputs<'_>,
) {
    let TreeOverlayInputs { text_input, frame_timer } = inputs;

    // Browser popup thumbnails (PRESET_LIBRARY_DESIGN P6, D7): decode +
    // register every open item's PNG ONCE per distinct path (checked via
    // `has_image`, so a re-open in the same process is free too) — never
    // per frame, never a render. `device` is unconditional here (the caller
    // already guarantees GPU readiness to reach this seam at all).
    for path in ui_root.browser_popup.thumbnail_paths() {
        let handle = manifold_ui::node::texture_handle_for_key(path);
        if ui_renderer.has_image(handle) {
            continue;
        }
        match manifold_renderer::preset_thumbnail::decode_png_rgba8(std::path::Path::new(path)) {
            Ok((w, h, rgba)) => {
                ui_renderer.register_image(device, handle, w, h, &rgba);
            }
            Err(e) => log::error!("[preset-thumb] decode failed for {path}: {e}"),
        }
    }

    // Top-level overlays (perf HUD, dropdown, modals, browser popup) — built
    // by the overlay driver at the tail of the tree, drawn here in z-order.
    // One source (overlay_draw, recorded at build) for build and draw, so
    // they cannot drift. overlay_draw is in Z_ORDER (bottom→top), so each
    // overlay's depth is OVERLAY + its stack index: a later-opened overlay
    // (e.g. a dropdown over the Audio Setup modal) paints over an earlier
    // one, text included. Each range gets its own push/pop for scissor
    // isolation.
    for (i, &(start, end)) in ui_root.overlay_draw.iter().enumerate() {
        ui_renderer.push_depth(Depth::OVERLAY.above(i as i32));
        // Soft drop-shadow under the floating panel (§17). Drawn first so it
        // sits under the panel's own fill at this depth. Skip a leading
        // full-screen scrim (dim-modal backdrop) so the shadow lifts the
        // panel, not the whole screen.
        if start < end && manifold_ui::color::SHADOWS_ENABLED {
            let tree = &ui_root.tree;
            let mut r = tree.get_bounds(tree.id_at(start));
            if r.width >= logical_w as f32 - 1.0
                && r.height >= logical_h as f32 - 1.0
                && start + 1 < end
            {
                r = tree.get_bounds(tree.id_at(start + 1));
            }
            ui_renderer.draw_shadow(
                r.x,
                r.y + manifold_ui::color::SHADOW_OFFSET_Y,
                r.width,
                r.height,
                manifold_ui::color::POPUP_RADIUS,
                manifold_ui::color::SHADOW_BLUR,
                manifold_ui::color::SHADOW,
            );
        }
        // `render_sub_region`, not `render_tree_range`: each overlay's
        // `(start, end)` deliberately excludes its own
        // `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` region root (see
        // `UIRoot::build_overlays`'s doc comment — keeping `start` at the
        // first REAL node is what the shadow-peek above depends on), so a
        // root-scan would never find that region and would render nothing.
        // `render_sub_region`'s ancestor-aware flat scan picks up the
        // region's `CLIPS_CHILDREN` regardless. This is the BUG-097 call
        // choice — the sole owner now, so there is no parallel copy left to
        // pick the wrong one (this design's D2 closes BUG-151 the same way:
        // the editor stops rendering overlay nodes through its root scan at
        // all, rather than picking the wrong call by hand a second time).
        ui_renderer.render_sub_region(&ui_root.tree, start, end, false);
        ui_renderer.pop_depth();
    }

    // Effect card drag ghost + text input — TOOLTIP depth, above every
    // overlay.
    ui_renderer.push_depth(Depth::TOOLTIP);
    if let Some(start) = ui_root.inspector.card_drag_first_node() {
        ui_renderer.render_tree_range(&ui_root.tree, start, usize::MAX);
    }
    // Text input overlay — last, so it tops everything. `Some` only when the
    // caller resolved this window as the session's owner (D4) — the
    // `.active` check that used to gate this lives inside that resolution,
    // not here.
    if let Some(text_input) = text_input {
        crate::app_render::render_text_input_overlay(text_input, frame_timer, ui_renderer);
    }
    ui_renderer.pop_depth();

    // Clear the overlay region's dirty flags. Overlay nodes (HUD, playhead,
    // popups) live at `[overlay_region_start, count)` and are never in a
    // panel-cache's atlas ranges, so they would otherwise keep `has_dirty`
    // permanently true and defeat the idle fast path (main window) or just
    // accumulate uselessly (editor, which has no cache to defeat — D5). See
    // module doc deviation 1 for why this runs here rather than after the
    // caller's flush/commit.
    let overlay_start = ui_root.overlay_region_start;
    let node_count = ui_root.tree.count();
    ui_root.tree.clear_dirty_range(overlay_start, node_count);
}
