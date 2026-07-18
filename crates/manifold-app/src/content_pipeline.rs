use parking_lot::RwLock;
use std::sync::Arc;

use ahash::AHashMap;
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::types::BlendMode;
use manifold_core::{ClipId, EffectId, LayerId, NodeId};
#[cfg(target_os = "macos")]
use manifold_media::video_renderer::VideoRenderer;
use manifold_playback::engine::{PlaybackEngine, TickResult};
use manifold_renderer::compositor::{CompositeLayerDescriptor, Compositor, CompositorFrame};
use manifold_renderer::generator_renderer::GeneratorRenderer;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::layer_compositor::CompositeClipDescriptor;
use manifold_renderer::tonemap::TonemapSettings;

/// Thread-safe shared output dimensions. The content thread writes new
/// dimensions after resize; the UI thread reads them for aspect ratio.
pub struct SharedOutputView {
    dimensions: RwLock<(u32, u32)>,
}

impl SharedOutputView {
    pub fn new() -> Self {
        Self {
            dimensions: RwLock::new((1920, 1080)),
        }
    }

    /// Update dimensions (called by content thread on resize).
    pub fn set_dimensions(&self, w: u32, h: u32) {
        *self.dimensions.write() = (w, h);
    }

    /// Get current output dimensions (called by UI thread for aspect ratio).
    pub fn get_dimensions(&self) -> (u32, u32) {
        *self.dimensions.read()
    }
}

/// Opaque occlusion: find every layer made invisible by a fully-opaque
/// layer above it, so the compositor can skip blending it into the
/// composite. Blend-skip ONLY — nothing upstream (generator render, sim
/// state, clip playback, per-layer effect chains) is affected by occlusion;
/// blend modes never interact with simulation or generator state.
///
/// The cutoff is the topmost top-level (non-group, no parent) layer that is
/// active this frame (has a ready clip), passes mute/solo, blends `Opaque`,
/// and sits at full opacity. `Opaque` (mode 6) replaces every pixel and
/// ignores the base regardless of alpha — even a transformed or effected
/// opaque layer writes its full texture — so everything below the cutoff
/// contributes nothing to the frame.
///
/// A layer below the cutoff is occluded only if its whole parent chain is
/// also below the cutoff (a child of a group ABOVE the cutoff still renders
/// into that group's composite). Opaque layers inside groups never act as
/// global occluders in v1 — their group may carry partial opacity or effects.
///
/// `out` receives the occluded layer indices (empty = no occluder). Layer
/// `index` equals its position in `timeline.layers`; index 0 is the top.
fn compute_occluded_layer_indices(
    layers: &[manifold_core::layer::Layer],
    ready_clips: &[manifold_playback::scheduler::ActiveClipRef],
    out: &mut Vec<i32>,
) {
    out.clear();
    // Audio layers have their own solo/mute bus (audible, not visual) — they must
    // never affect compositing. See docs/AUDIO_LAYER_DESIGN.md §5.
    let any_solo = layers.iter().filter(|l| !l.is_audio()).any(|l| l.is_solo);
    let mut cutoff: Option<i32> = None;
    for l in layers {
        if l.is_group() || l.parent_layer_id.is_some() {
            continue;
        }
        if l.is_muted || (any_solo && !l.is_solo) {
            continue;
        }
        if l.default_blend_mode != BlendMode::Opaque || l.opacity < 1.0 {
            continue;
        }
        if !ready_clips.iter().any(|c| c.layer_index == l.index) {
            continue;
        }
        cutoff = Some(l.index);
        break;
    }
    let Some(cut) = cutoff else {
        return;
    };
    for l in layers {
        if l.index <= cut {
            continue;
        }
        // Walk the parent chain: occluded only if every ancestor is below
        // the cutoff too. Chains are short (groups nest 1-2 deep); the
        // depth guard caps pathological/cyclic parent data.
        let mut above_cutoff_ancestor = false;
        let mut parent = l.parent_layer_id.as_ref();
        let mut depth = 0;
        while let Some(pid) = parent {
            depth += 1;
            if depth > 16 {
                break;
            }
            match layers.iter().find(|p| &p.layer_id == pid) {
                Some(p) if p.index <= cut => {
                    above_cutoff_ancestor = true;
                    break;
                }
                Some(p) => parent = p.parent_layer_id.as_ref(),
                None => break,
            }
        }
        if !above_cutoff_ancestor {
            out.push(l.index);
        }
    }
}

/// From the occluded set, pick the layers safe to skip RENDERING entirely
/// (generators + effect chains), not just their final blend. See
/// `ContentPipeline::render_skip_scratch` for the safety argument.
///
/// Conservative on purpose — the set only ever SHRINKS the work, and a
/// wrongly-kept layer just wastes perf while a wrongly-skipped one that feeds
/// LED would blank the wall. So we skip only plain **top-level leaf** layers
/// (no group, no parent) that are **not** LED-tapped, and we disable the whole
/// optimization while any authoring preview is open (previews consume the
/// hidden layer's output). Grouped layers and LED layers stay on the
/// blend-skip-only path. `out` is cleared and refilled; empty = skip nothing.
fn compute_render_skip_indices(
    layers: &[manifold_core::layer::Layer],
    occluded: &[i32],
    enabled: bool,
    preview_active: bool,
    out: &mut Vec<i32>,
) {
    out.clear();
    if !enabled || preview_active {
        return;
    }
    for &idx in occluded {
        // Skip only a plain top-level leaf that isn't feeding LED. Any layer
        // we can't positively classify as safe stays on the render path.
        let safe = layers.iter().find(|l| l.index == idx).is_some_and(|l| {
            !l.is_group() && l.parent_layer_id.is_none() && !l.blit_to_led
        });
        if safe {
            out.push(idx);
        }
    }
}

/// Self-contained content rendering pipeline.
///
/// Owns the compositor and orchestrates GPU rendering of generators + compositing.
/// Per-node thumbnail atlas geometry. The atlas is one `ATLAS_W`×`ATLAS_H`
/// texture packed as an `ATLAS_GRID`×`ATLAS_GRID` grid of **16:9** cells, so
/// each thumbnail keeps a video aspect instead of being squashed into a square
/// (the old 64²-square capture distorted every output twice — once into the
/// square cell, once stretching that square into the node body). Cells are
/// `ATLAS_CELL_W`×`ATLAS_CELL_H`; up to `ATLAS_GRID²` node thumbnails fit.
///
/// `8×8` × `256×144` → a `2048×1152` atlas (~9.4 MB `Rgba16Float` per surface,
/// authoring-only). Fewer but far larger cells than the old `16×16`×`64²`: a
/// single graph level rarely shows more than ~64 image nodes, and the extra
/// resolution is what makes a thumbnail readable at editor zoom.
pub const ATLAS_GRID: u32 = 8;
pub const ATLAS_CELL_W: u32 = 256;
pub const ATLAS_CELL_H: u32 = 144;
pub const ATLAS_W: u32 = ATLAS_GRID * ATLAS_CELL_W;
pub const ATLAS_H: u32 = ATLAS_GRID * ATLAS_CELL_H;
/// Max node thumbnails an atlas holds.
pub const ATLAS_CELLS: usize = (ATLAS_GRID * ATLAS_GRID) as usize;

/// The per-node atlas UV rect for one `cell` index, letterboxed to
/// `monitor_aspect` inside its 16:9 cell (BUG-034: this is the live
/// preview-inline math from `app_render.rs`'s node-output-preview block,
/// factored out so a headless harness can drive the same cell-picking math
/// a live render does, instead of only proving the per-node-texture path).
/// Returns `[u0, v0, u1, v1]` in the atlas's normalized UV space.
pub fn atlas_cell_uv(cell: u32, monitor_aspect: f32) -> [f32; 4] {
    let inv = 1.0 / ATLAS_GRID as f32;
    let half_tx = 0.5 / ATLAS_W as f32;
    let half_ty = 0.5 / ATLAS_H as f32;
    let cell_aspect = ATLAS_CELL_W as f32 / ATLAS_CELL_H as f32;
    let (content_w_frac, content_h_frac) = if monitor_aspect > cell_aspect {
        (1.0, cell_aspect / monitor_aspect)
    } else {
        (monitor_aspect / cell_aspect, 1.0)
    };
    let gx = (cell % ATLAS_GRID) as f32;
    let gy = (cell / ATLAS_GRID) as f32;
    let mut u0 = gx * inv + half_tx;
    let mut v0 = gy * inv + half_ty;
    let mut du = inv - 2.0 * half_tx;
    let mut dv = inv - 2.0 * half_ty;
    u0 += du * (1.0 - content_w_frac) * 0.5;
    v0 += dv * (1.0 - content_h_frac) * 0.5;
    du *= content_w_frac;
    dv *= content_h_frac;
    [u0, v0, u0 + du, v0 + dv]
}

#[cfg(test)]
mod atlas_cell_uv_tests {
    use super::*;

    #[test]
    fn square_monitor_matches_cell_aspect_letterboxing() {
        // monitor_aspect == cell_aspect: no letterboxing, full cell (minus
        // the half-texel inset) is used in both axes.
        let cell_aspect = ATLAS_CELL_W as f32 / ATLAS_CELL_H as f32;
        let uv = atlas_cell_uv(0, cell_aspect);
        let inv = 1.0 / ATLAS_GRID as f32;
        let half_tx = 0.5 / ATLAS_W as f32;
        let half_ty = 0.5 / ATLAS_H as f32;
        assert!((uv[0] - half_tx).abs() < 1e-6);
        assert!((uv[1] - half_ty).abs() < 1e-6);
        assert!((uv[2] - (inv - half_tx)).abs() < 1e-6);
        assert!((uv[3] - (inv - half_ty)).abs() < 1e-6);
    }

    #[test]
    fn cell_index_selects_correct_grid_position() {
        // Cell ATLAS_GRID (start of row 1) must land one grid step down in v,
        // at the same u as cell 0 — proves gx/gy decomposition, not just the
        // letterbox math.
        let cell_aspect = ATLAS_CELL_W as f32 / ATLAS_CELL_H as f32;
        let uv0 = atlas_cell_uv(0, cell_aspect);
        let uv1 = atlas_cell_uv(ATLAS_GRID, cell_aspect);
        let inv = 1.0 / ATLAS_GRID as f32;
        assert!((uv0[0] - uv1[0]).abs() < 1e-6, "same column -> same u0");
        assert!((uv1[1] - (uv0[1] + inv)).abs() < 1e-6, "one row down -> v0 + inv");
    }

    #[test]
    fn wide_monitor_letterboxes_vertically() {
        // monitor_aspect > cell_aspect: content is full-width, shrunk in v
        // and centered — the v span must be strictly smaller than a
        // non-letterboxed cell's, and vertically centered (equal top/bottom
        // margin from cell edges).
        let cell_aspect = ATLAS_CELL_W as f32 / ATLAS_CELL_H as f32;
        let uv = atlas_cell_uv(0, cell_aspect * 2.0);
        let inv = 1.0 / ATLAS_GRID as f32;
        let half_tx = 0.5 / ATLAS_W as f32;
        let half_ty = 0.5 / ATLAS_H as f32;
        // Full width (minus half-texel inset), same as the unletterboxed case.
        assert!((uv[0] - half_tx).abs() < 1e-6);
        assert!((uv[2] - (inv - half_tx)).abs() < 1e-6);
        // Shrunk + centered in v: top margin == bottom margin.
        let top_margin = uv[1] - half_ty;
        let bottom_margin = (inv - half_ty) - uv[3];
        assert!(top_margin > 1e-6, "must actually letterbox, not fill v");
        assert!((top_margin - bottom_margin).abs() < 1e-6, "must be centered");
    }
}

/// Clip-thumbnail **filmstrip** atlas geometry (§24 5c-2), decoupled from the
/// node atlas above. A non-square grid of small 16:9 cells; each cell holds one
/// bar (or bar-group) of one clip's filmstrip. Cells are interchangeable — a clip
/// holds a *list* of cell indices — so no rectangle packing is needed. Smaller
/// cells than the node atlas because each is drawn narrow (one bar wide) and many
/// are live at once. `32×8` × `256×144` → an `8192×1152` atlas (~75.5 MB
/// Rgba16Float, ONE `SharedAtlasSurface` — no triple-buffer copies (BUG-119:
/// the old design's persistent-texture-plus-3-rotating-IOSurfaces layout is
/// gone; content and UI import `GpuTexture`s backed by the same IOSurface).
/// 256 cells ≈ 30 visible clips × ~8 visible bars. Cells doubled from `128×72`
/// (2026-06-28) so thumbnails are sharp when upscaled into the tall timeline
/// lanes instead of blocky; format stays Rgba16Float, full cell count kept
/// (VRAM is not the constraint at this scale).
pub const CLIP_ATLAS_COLS: u32 = 32;
pub const CLIP_ATLAS_ROWS: u32 = 8;
pub const CLIP_ATLAS_CELL_W: u32 = 256;
pub const CLIP_ATLAS_CELL_H: u32 = 144;
pub const CLIP_ATLAS_W: u32 = CLIP_ATLAS_COLS * CLIP_ATLAS_CELL_W;
pub const CLIP_ATLAS_H: u32 = CLIP_ATLAS_ROWS * CLIP_ATLAS_CELL_H;
/// Max filmstrip cells the clip atlas holds (LRU-evicted by clip past this).
pub const CLIP_ATLAS_CELLS: usize = (CLIP_ATLAS_COLS * CLIP_ATLAS_ROWS) as usize;

/// Snapshot of `(published layout, clip→content-hash)` captured when a save
/// readback is submitted, consumed when it returns (§24 5c-2 P4).
type ClipAtlasPersistSnapshot = (Vec<(manifold_core::ClipId, u32, u32)>, AHashMap<String, u64>);

/// `(built layout, GPU signal value the pixels land on)` — see
/// `ContentPipeline::clip_atlas_pending_layout` and `CLIP_ATLAS_LAYOUT_UNSTAMPED`.
type ClipAtlasPendingLayout = (Vec<(manifold_core::ClipId, u32, u32)>, u64);

/// Sentinel `clip_atlas_pending_layout` signal value meaning "this layout was
/// built this frame but hasn't been stamped with a real GPU signal value yet"
/// (the stamp happens after `signal_event`/`commit`, later in the same tick —
/// see `render_content`). `is_done(u64::MAX)` can never spuriously resolve
/// true against a real signaled value, so a layout can never be promoted to
/// `last_clip_atlas_layout` while still carrying this sentinel.
const CLIP_ATLAS_LAYOUT_UNSTAMPED: u64 = u64::MAX;

/// Aspect-fit viewport `(x, y, w, h)` for atlas cell `i`, letterboxing a source
/// of `src_w`×`src_h` inside its 16:9 cell so the thumbnail keeps the source's
/// true aspect (the atlas is cleared transparent first, so the letterbox bars
/// read as the node's preview-screen background, not black bars). A 16:9 source
/// fills the cell exactly; anything else is centred and padded.
pub fn atlas_cell_viewport(i: usize, src_w: u32, src_h: u32) -> (f32, f32, f32, f32) {
    let gx = (i as u32 % ATLAS_GRID) as f32;
    let gy = (i as u32 / ATLAS_GRID) as f32;
    let cell_x = gx * ATLAS_CELL_W as f32;
    let cell_y = gy * ATLAS_CELL_H as f32;
    let cw = ATLAS_CELL_W as f32;
    let ch = ATLAS_CELL_H as f32;
    let src_aspect = (src_w.max(1) as f32) / (src_h.max(1) as f32);
    let cell_aspect = cw / ch;
    let (draw_w, draw_h) = if src_aspect > cell_aspect {
        (cw, cw / src_aspect)
    } else {
        (ch * src_aspect, ch)
    };
    (
        cell_x + (cw - draw_w) * 0.5,
        cell_y + (ch - draw_h) * 0.5,
        draw_w,
        draw_h,
    )
}

/// Full-cell viewport for clip-atlas filmstrip cell `i` (no letterbox). A clip
/// thumbnail fills the whole cell with the source (≈ project aspect ≈ the 16:9
/// cell), and the UI centre-crops each cell to the bar's on-screen sub-rect — the
/// DAW thumbnail look, no bars. Uses the non-square `CLIP_ATLAS_COLS×ROWS` grid.
fn clip_atlas_cell_full(i: usize) -> (f32, f32, f32, f32) {
    let gx = (i as u32 % CLIP_ATLAS_COLS) as f32;
    let gy = (i as u32 / CLIP_ATLAS_COLS) as f32;
    (
        gx * CLIP_ATLAS_CELL_W as f32,
        gy * CLIP_ATLAS_CELL_H as f32,
        CLIP_ATLAS_CELL_W as f32,
        CLIP_ATLAS_CELL_H as f32,
    )
}

/// §24 5c cold-start: locate a PARKED generator clip by id — its layer (must be a
/// generator), clip index, and clip-start `(time, beat)` for a thumbnail render.
/// `None` if not found or the layer isn't a generator (video posters are separate).
#[cfg(target_os = "macos")]
fn find_parked_generator_clip<'a>(
    layers: &'a [manifold_core::layer::Layer],
    clip_id: &str,
) -> Option<(&'a manifold_core::layer::Layer, u32, f64, f64)> {
    for layer in layers {
        if layer.gen_params().is_none() {
            continue;
        }
        for (ci, clip) in layer.clips.iter().enumerate() {
            if clip.id.as_str() == clip_id {
                return Some((layer, ci as u32, 0.0, clip.start_beat.as_f32() as f64));
            }
        }
    }
    None
}

/// §24 5c P2b: locate a PARKED video clip by id — returns its `video_clip_id`
/// (the file reference), for a one-shot poster decode. `None` if not found or not
/// a video clip.
#[cfg(target_os = "macos")]
fn find_parked_video_clip<'a>(
    layers: &'a [manifold_core::layer::Layer],
    clip_id: &str,
) -> Option<&'a str> {
    for layer in layers {
        if !layer.is_video() {
            continue;
        }
        for clip in &layer.clips {
            if clip.id.as_str() == clip_id && !clip.video_clip_id.is_empty() {
                return Some(&clip.video_clip_id);
            }
        }
    }
    None
}

/// Guard a thumbnail-capture source texture before binding it to the downsample
/// shader. A render-target-only producer output (no `SHADER_READ`) hard-crashes
/// AGX in `setVertexTexture` (nil-deref at `0x78`), so the capture must refuse
/// it — at worst a clip/node shows no thumbnail instead of taking the rig down.
/// Logs the offending label once per process so the producer can be fixed to
/// create its output with `SHADER_READ`. The dedup set is a process-local
/// diagnostic, not shared app state.
#[cfg(target_os = "macos")]
fn thumb_source_shader_readable(label: &str, tex: &manifold_gpu::GpuTexture) -> bool {
    if tex.is_shader_readable() {
        return true;
    }
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<ahash::AHashSet<String>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(ahash::AHashSet::new()));
    if let Ok(mut set) = warned.lock()
        && set.insert(label.to_string())
    {
        eprintln!(
            "[thumbnail] skipping capture of '{label}': source texture is render-target-only \
             (no SHADER_READ) and would crash setVertexTexture. Fix the producer to create its \
             output with SHADER_READ usage."
        );
    }
    false
}

/// Snapshot live clip output into the single shared clip-thumbnail atlas
/// surface (§24 5c, BUG-119 root fix). `sources` maps each *active* clip
/// (under the playhead this frame) to its just-rendered source texture. A clip
/// is (re)snapshot when it first appears or after `REFRESH_INTERVAL` frames,
/// at most `MAX_SNAPSHOTS_PER_FRAME` per frame, so cold thumbnails fill in
/// gradually and the live frame budget is never threatened. Cells persist
/// after a clip leaves the playhead (a parked clip keeps its thumbnail).
///
/// Every cell blit targets `persistent` directly with `LoadAction::Load` — no
/// clear, ever, after the one-time init clear at surface creation. There is no
/// separate "write surface" and no full-atlas publish copy: `persistent` IS
/// the surface the UI thread samples, so a completed cell blit is immediately
/// visible with no rotation or front-buffer flip. Returns true when at least
/// one cell was (re)painted this frame — the caller uses that to drive the
/// disk-save debounce, not any completion-publish step (there isn't one).
///
/// When a cell is newly allocated (or an eviction changes the layout), the
/// new layout is stashed in `pending_layout_out` rather than written straight
/// to the UI-visible layout — see `render_content`'s post-commit stamp +
/// promote-on-completion, which keeps layout from ever naming a cell before
/// its pixels have landed (item 3 of the BUG-119 fix).
///
/// A free function (not a `&mut self` method) so the caller can pass the
/// persistent atlas / cache / layout as disjoint field borrows alongside the
/// `&self.native_device` binding that `render()` holds for the whole frame.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn fill_clip_atlas(
    enc: &mut manifold_gpu::GpuEncoder,
    sources: &AHashMap<&str, &manifold_gpu::GpuTexture>,
    clip_meta: &AHashMap<&str, (f64, f64, f64)>,
    cell_override: &AHashMap<&str, u32>,
    current_beat: f64,
    beats_per_bar: f64,
    persistent: Option<&manifold_gpu::GpuTexture>,
    raw: Option<&manifold_gpu::GpuRenderPipeline>,
    downsample: Option<&manifold_gpu::GpuRenderPipeline>,
    sampler: Option<&manifold_gpu::GpuSampler>,
    visible: &[ClipId],
    cache: &mut crate::clip_atlas::ClipAtlasCache,
    last_snapshot: &mut AHashMap<ClipId, (u32, u64, f64)>,
    frame_counter: &mut u64,
    pending_layout_out: &mut Option<ClipAtlasPendingLayout>,
) -> bool {
    const MAX_SNAPSHOTS_PER_FRAME: usize = 4;
    // Throttled re-capture of the bar currently under the playhead, so a long bar's
    // cell reflects mid-bar content rather than only its downbeat. Only the single
    // playhead cell churns — the rest of the strip is static history.
    const REFRESH_INTERVAL: u64 = 90; // ~1.5 s at 60 fps

    let (Some(persistent_tex), Some(raw), Some(sampler)) = (persistent, raw, sampler) else {
        return false;
    };

    *frame_counter = frame_counter.wrapping_add(1);
    let frame = *frame_counter;

    // ── Phase 1: pick this frame's bar captures ──
    // Each visible clip with a live source captures the filmstrip cell under the
    // playhead (or cell 0 when parked — a representative still). A capture happens
    // on first sight of that cell, when the playhead crosses into a new cell, or on
    // the throttled refresh of the current cell.
    cache.begin_frame(visible);
    let mut to_blit: Vec<(u32, &manifold_gpu::GpuTexture)> = Vec::new();
    let mut budget = MAX_SNAPSHOTS_PER_FRAME;
    let mut allocated_new = false;
    for cid in visible {
        let Some(&tex) = sources.get(cid.as_str()) else {
            // Inactive clip with no live source: keeps its cached strip (persisted).
            continue;
        };
        // A render-target-only source can't be sampled — binding it crashes AGX.
        if !thumb_source_shader_readable(cid.as_str(), tex) {
            continue;
        }
        let (target_cell, is_active) = if let Some(&cell) = cell_override.get(cid.as_str()) {
            // Parked video filmstrip: the cell its settled poster frame is for.
            // Captured once (not active) so it never churns.
            (cell, false)
        } else {
            match clip_meta.get(cid.as_str()) {
                Some(&(start, dur, _)) if current_beat >= start && current_beat < start + dur => (
                    crate::clip_filmstrip::cell_index_at_beat(
                        current_beat,
                        start,
                        dur,
                        beats_per_bar,
                    ),
                    true,
                ),
                // Parked generator (playhead outside) or unknown beats: cell 0.
                _ => (0, false),
            }
        };
        let existing = cache.cell_for(cid, target_cell);
        let due = match last_snapshot.get(cid) {
            None => true,
            Some(&(last_cell, last_frame, last_beat)) => {
                last_cell != target_cell
                    || existing.is_none()
                    // Throttled re-capture only of the live playhead bar — and
                    // only if the transport actually MOVED since the last
                    // capture. A paused playhead re-captured identical pixels
                    // every 1.5s, and each capture re-armed the disk-save
                    // debounce: a perpetual 75MB readback loop while idle.
                    || (is_active
                        && frame.wrapping_sub(last_frame) >= REFRESH_INTERVAL
                        && current_beat != last_beat)
            }
        };
        if !due {
            continue;
        }
        if budget == 0 {
            break;
        }
        if let Some(cell) = cache.get_or_alloc(cid, target_cell) {
            last_snapshot.insert(cid.clone(), (target_cell, frame, current_beat));
            to_blit.push((cell, tex));
            if existing.is_none() {
                allocated_new = true; // new mapping (and any eviction it triggered)
            }
            budget -= 1;
        }
    }
    // The published layout changes only when a cell is (re)allocated or a clip is
    // evicted — both happen exclusively when `allocated_new`. Re-capturing an
    // existing cell leaves the layout untouched, so don't rebuild it then.
    // Stashed as UNSTAMPED — `render_content` fills in the real GPU signal
    // value right after this frame's `signal_event`/commit.
    if allocated_new {
        *pending_layout_out = Some((cache.layout(), CLIP_ATLAS_LAYOUT_UNSTAMPED));
    }

    if to_blit.is_empty() {
        return false;
    }

    // ── Phase 2: GPU blits, straight into the shared surface ──
    // Box-downsample the (often full-res) source into the cell; fall back to the
    // plain blit if the downsample pipeline isn't available. LoadAction::Load —
    // this is the only kind of write the atlas ever gets after its one-time
    // init clear; the UI thread may sample a cell mid-blit for one frame
    // (valid-old or valid-new pixels), which is the accepted trade replacing
    // the old triple-buffer ring (BUG-119).
    let cell_blit = downsample.unwrap_or(raw);
    for (cell, tex) in &to_blit {
        enc.draw_fullscreen_viewport(
            cell_blit,
            persistent_tex,
            &[
                manifold_gpu::GpuBinding::Texture { binding: 0, texture: tex },
                manifold_gpu::GpuBinding::Sampler { binding: 1, sampler },
            ],
            clip_atlas_cell_full(*cell as usize),
            manifold_gpu::GpuLoadAction::Load,
            "Clip Thumbnail Atlas Cell",
        );
    }
    true
}

/// Restore cached filmstrip cells into the shared clip atlas surface (§24 5c-2
/// P4). Requests missing strips from the disk cache, stashes finished loads,
/// and uploads **one** cell per frame via a reused RGBA8 staging texture
/// (`replaceRegion` is a CPU write, so a single upload+blit per frame avoids
/// any GPU read-after-write hazard; the strip fills in over ~1 s). Returns
/// true if a cell was restored. Best-effort: bad/short data is skipped and the
/// clip simply re-captures. Blits target `persistent` (the shared surface)
/// directly with `LoadAction::Load` — same no-clear contract as
/// `fill_clip_atlas`, and a newly allocated cell's layout goes through the
/// same UNSTAMPED → post-commit-stamped → promote-on-completion path (see
/// `render_content`), not straight to `last_clip_atlas_layout`.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn restore_clip_atlas(
    native_device: &manifold_gpu::GpuDevice,
    enc: &mut manifold_gpu::GpuEncoder,
    persistent: Option<&manifold_gpu::GpuTexture>,
    raw: Option<&manifold_gpu::GpuRenderPipeline>,
    sampler: Option<&manifold_gpu::GpuSampler>,
    visible: &[ClipId],
    clip_hashes: &AHashMap<String, u64>,
    cache_obj: &mut crate::clip_atlas::ClipAtlasCache,
    cache_disk: &mut crate::clip_thumb_cache::ClipThumbCache,
    pending_loads: &mut AHashMap<u64, crate::clip_thumb_cache::StripCells>,
    staging: &mut Option<manifold_gpu::GpuTexture>,
    pending_layout_out: &mut Option<ClipAtlasPendingLayout>,
) -> bool {
    let (Some(pt), Some(raw), Some(sampler)) = (persistent, raw, sampler) else {
        return false;
    };
    // Request a load for each visible clip that has no cells yet (once per session).
    for cid in visible {
        if cache_obj.contains_any(cid) {
            continue;
        }
        if let Some(&hash) = clip_hashes.get(cid.as_str())
            && hash != 0
        {
            cache_disk.request_load(hash);
        }
    }
    // Stash finished loads (the load may land a frame when its clip isn't visible).
    for strip in cache_disk.drain_loaded() {
        pending_loads.insert(strip.hash, strip.cells);
    }
    // Drop strips no visible clip wants any more, bounding memory.
    let wanted: ahash::AHashSet<u64> = visible
        .iter()
        .filter_map(|c| clip_hashes.get(c.as_str()).copied())
        .filter(|&h| h != 0)
        .collect();
    pending_loads.retain(|h, _| wanted.contains(h));
    if pending_loads.is_empty() {
        return false;
    }

    let (cw, ch) = (CLIP_ATLAS_CELL_W, CLIP_ATLAS_CELL_H);
    let cell_bytes = (cw * ch * 4) as usize;
    // Restore ONE cell this frame: pick a visible clip + a strip cell its cache lacks.
    for cid in visible {
        let Some(&hash) = clip_hashes.get(cid.as_str()) else {
            continue;
        };
        let Some(strip) = pending_loads.get(&hash) else {
            continue;
        };
        let Some((cell_idx, bytes)) = strip
            .iter()
            .find(|(idx, _)| cache_obj.cell_for(cid, *idx).is_none())
        else {
            continue;
        };
        if bytes.len() != cell_bytes {
            continue;
        }
        let Some(atlas_cell) = cache_obj.get_or_alloc(cid, *cell_idx) else {
            continue;
        };
        if staging.is_none() {
            *staging = Some(native_device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: cw,
                height: ch,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
                label: "Clip Thumb Restore Staging",
                mip_levels: 1,
            }));
        }
        let st = staging.as_ref().expect("staging just created");
        native_device.upload_texture(st, bytes);
        enc.draw_fullscreen_viewport(
            raw,
            pt,
            &[
                manifold_gpu::GpuBinding::Texture { binding: 0, texture: st },
                manifold_gpu::GpuBinding::Sampler { binding: 1, sampler },
            ],
            clip_atlas_cell_full(atlas_cell as usize),
            manifold_gpu::GpuLoadAction::Load,
            "Clip Thumb Restore Cell",
        );
        *pending_layout_out = Some((cache_obj.layout(), CLIP_ATLAS_LAYOUT_UNSTAMPED));
        return true;
    }
    false
}

/// The PlaybackEngine (which owns GeneratorRenderer) is borrowed for each frame.
///
/// On macOS, uses native Metal encoding via manifold-gpu.
/// IOSurface triple-buffering for zero-copy cross-device sharing with the UI thread.
/// Combined with separate Metal command queues (content + UI),
/// this allows 2 content frames in flight without starving the UI thread.
pub struct ContentPipeline {
    compositor: Box<dyn Compositor>,
    /// EDR headroom from the display (1.0 = SDR, e.g. 2.0 = 2x SDR white).
    /// Used to compute max_display_nits for tonemapping.
    pub edr_headroom: f64,
    /// PQ encoder for HDR export. Lazily created on first HDR export frame.
    pq_encoder: Option<manifold_renderer::pq_encoder::PqEncoder>,
    /// Reusable GPU→CPU readback for single-frame (still image) export.
    /// Holds the in-flight blit between `submit_still_readback` (one tick) and
    /// `take_still_readback` (the next). Idle except during a still capture.
    #[cfg(target_os = "macos")]
    still_readback: manifold_renderer::gpu_readback::ReadbackRequest,
    /// Shared output view for cross-thread access (fallback for non-macOS).
    shared_output: Arc<SharedOutputView>,
    /// MetalFX Spatial full-frame upscaler. Present only when render_scale < 1.0
    /// and MetalFX is supported (macOS 13+, Apple Silicon). Preferred over FSR.
    #[cfg(target_os = "macos")]
    metalfx: Option<manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler>,
    /// FSR 1.0 spatial upscaler. Present only when render_scale < 1.0
    /// AND MetalFX is not available. Fallback for older hardware.
    #[cfg(target_os = "macos")]
    fsr1: Option<manifold_renderer::fsr1::Fsr1Upscaler>,
    /// Full output dimensions (what the drawable and UI see).
    /// May differ from compositor dimensions when FSR is active.
    output_w: u32,
    output_h: u32,
    /// Direct-present output surface (CAMetalLayer on the output window).
    /// Content thread acquires drawables and presents in its own command buffer.
    /// None when no output window is open.
    #[cfg(target_os = "macos")]
    output_surface: Option<manifold_gpu::GpuSurface>,
    /// When true, skip next_drawable() during display retarget to avoid
    /// blocking the content thread for up to 1s on a transitioning display.
    #[cfg(target_os = "macos")]
    output_present_suspended: bool,
    /// Blit pipeline for output present (passthrough + sampler).
    #[cfg(target_os = "macos")]
    output_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Sampler for output present blit.
    #[cfg(target_os = "macos")]
    output_sampler: Option<manifold_gpu::GpuSampler>,
    /// Triple-buffered IOSurface textures for the workspace preview.
    #[cfg(target_os = "macos")]
    preview_textures: [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// Which preview surface we're writing to THIS frame (0, 1, or 2).
    #[cfg(target_os = "macos")]
    write_surface_index: usize,
    /// IOSurface bridge for the workspace preview path.
    #[cfg(target_os = "macos")]
    preview_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// Last seen preview bridge generation.
    #[cfg(target_os = "macos")]
    preview_generation: u64,
    /// Authoring-time node-output preview request `(watched effect, selected
    /// node)`, forwarded to the compositor each frame. `None` = no effect
    /// preview. Mutually exclusive with `node_preview_generator`.
    node_preview_request: Option<(EffectId, Option<NodeId>)>,
    /// Generator-side counterpart `(watched layer, selected node)`. Drives the
    /// per-node capture on the layer's generator `PresetRuntime`. `None` = no
    /// generator preview.
    node_preview_generator: Option<(LayerId, Option<NodeId>)>,
    /// One-shot "dump every output of this effect to disk" request `(effect,
    /// target dir)`. Consumed on the next render: the compositor captures the
    /// effect's node outputs, then they're read back and written as PNGs.
    pending_graph_dump: Option<(EffectId, std::path::PathBuf)>,
    /// Triple-buffered IOSurface textures for the node-output preview (the
    /// captured node texture, downscaled). Separate bridge from the workspace
    /// preview so the editor reads the node output independently.
    #[cfg(target_os = "macos")]
    node_preview_textures: [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// IOSurface bridge for the node-output preview path.
    #[cfg(target_os = "macos")]
    node_preview_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// The nodes the editor canvas can currently show — captured into the
    /// thumbnail atlas this frame. Set by the UI only while the graph editor is
    /// open; empty when closed, so a live show pays nothing. Only these nodes
    /// are dumped, so hidden / off-scope / collapsed-group nodes cost nothing
    /// (sub-change A).
    node_atlas_visible: Vec<NodeId>,
    /// Triple-buffered IOSurface textures for the per-node thumbnail atlas — one
    /// big texture packed as an `ATLAS_GRID`×`ATLAS_GRID` cell grid, each cell a
    /// node's downscaled output. The canvas samples one cell per node.
    #[cfg(target_os = "macos")]
    node_atlas_textures: [Option<manifold_gpu::GpuTexture>; crate::shared_texture::SURFACE_COUNT],
    /// IOSurface bridge for the thumbnail atlas path.
    #[cfg(target_os = "macos")]
    node_atlas_bridge: Option<Arc<crate::shared_texture::SharedTextureBridge>>,
    /// Published `(node_id, cell_index)` layout from the last atlas capture, so
    /// the UI knows which atlas cell holds each node's thumbnail. Empty when the
    /// atlas is off or nothing was captured.
    last_node_atlas_layout: Vec<(NodeId, u32)>,

    // ── Clip thumbnail atlas (§24 5c) ──────────────────────────────
    // BUG-119 root fix (2026-07-11): ONE IOSurface-backed texture shared by
    // both threads, replacing a private persistent texture plus a
    // `SharedTextureBridge` triple-buffer ring. Every cell blit uses
    // `LoadAction::Load`; the ring's periodic full-surface publish blit (the
    // ONLY `clear=true` write the atlas ever got) is gone, and with it the
    // race where a wrapped writer cleared the surface the UI was concurrently
    // sampling. `clip_atlas_persistent` IS the surface the UI reads — there
    // is no separate write target and no front-buffer flip to publish.
    /// Clips that currently want a timeline thumbnail (sent by the UI, deduped).
    /// Empty = no timeline visible, so the whole snapshot path is skipped.
    clip_atlas_visible: Vec<manifold_core::ClipId>,
    /// The single shared clip-atlas texture (content-side import), populated by
    /// `set_clip_atlas_texture` at content-thread init — already cleared once
    /// (transparent) by the caller before this is set; never cleared again.
    #[cfg(target_os = "macos")]
    clip_atlas_persistent: Option<manifold_gpu::GpuTexture>,
    /// Keeps the shared IOSurface alive on the content-thread side (the UI
    /// thread holds its own `Arc` clone in `Application`; either alone would
    /// keep the kernel object alive, but both sides holding a clone matches
    /// the existing preview/node-atlas bridge pattern and needs no reasoning
    /// about which thread outlives which).
    #[cfg(target_os = "macos")]
    clip_atlas_surface: Option<Arc<crate::shared_texture::SharedAtlasSurface>>,
    /// (ClipId, filmstrip cell) → atlas cell allocation + whole-clip LRU eviction.
    clip_atlas_cache: crate::clip_atlas::ClipAtlasCache,
    /// Per clip: the filmstrip cell last captured and the frame it was captured, so
    /// the playhead's current bar is captured on bar-crossing and refreshed at a
    /// throttled rate while it sits in a bar.
    // (cell, frame, beat-at-capture). The beat gates the throttled refresh:
    // a paused transport re-captures identical pixels forever otherwise, and
    // every capture re-arms the 5s disk-save debounce -> a perpetual 75MB
    // GPU->CPU readback loop on a completely static timeline (BUG-119 probe,
    // 2026-07-11: saves at frames 301/661/967 with nothing changing).
    clip_atlas_last_snapshot: AHashMap<manifold_core::ClipId, (u32, u64, f64)>,
    /// Monotonic fill-frame counter (refresh cadence for the playhead cell).
    clip_atlas_frame: u64,
    /// Published `(clip_id, filmstrip cell, atlas cell)` layout for the UI —
    /// only ever promoted from `clip_atlas_pending_layout` once its pixels are
    /// confirmed on the GPU timeline (never written directly by a capture
    /// pass). See `clip_atlas_pending_layout`.
    last_clip_atlas_layout: Vec<(manifold_core::ClipId, u32, u32)>,
    /// A layout built by `fill_clip_atlas`/`restore_clip_atlas` this frame
    /// (or an earlier one still awaiting GPU completion), paired with the GPU
    /// signal value that will be reached once its pixels have actually landed
    /// in the shared surface. `CLIP_ATLAS_LAYOUT_UNSTAMPED` means "built this
    /// frame, not yet stamped with a real signal value" — `render_content`
    /// fills that in right after `signal_event`/commit. Promoted to
    /// `last_clip_atlas_layout` on a later tick once `native_event.is_done`
    /// on the stamped value — so the UI can never learn about a cell before
    /// its blit is confirmed complete (layout never leads pixels; BUG-119
    /// item 3). Content-thread-owned; no lock needed.
    clip_atlas_pending_layout: Option<ClipAtlasPendingLayout>,
    /// §24 5c-2 P4: sidecar disk cache so filmstrips survive reload. `None` if no
    /// cache dir is available. All disk IO is on its own worker thread.
    clip_thumb_cache: Option<crate::clip_thumb_cache::ClipThumbCache>,
    /// Async RGBA8 readback of the persistent atlas for the debounced disk save.
    #[cfg(target_os = "macos")]
    clip_atlas_readback: manifold_renderer::gpu_readback::ReadbackRequest,
    /// Fill-frame at which a debounced save should fire (0 = none scheduled).
    clip_atlas_persist_due: u64,
    /// `(layout, clip→hash)` snapshot captured when the save readback was submitted,
    /// used to slice the atlas bytes when the readback returns a later tick.
    clip_atlas_persist_pending: Option<ClipAtlasPersistSnapshot>,
    /// Reused cell-sized RGBA8 staging texture for uploading cached cells back into
    /// the persistent atlas on restore. Lazily created.
    #[cfg(target_os = "macos")]
    clip_atlas_restore_staging: Option<manifold_gpu::GpuTexture>,
    /// Loaded strips waiting to be uploaded (keyed by content hash), kept until
    /// every cell is restored or the owning clips leave the visible set.
    clip_atlas_pending_loads: AHashMap<u64, crate::clip_thumb_cache::StripCells>,

    /// Per-surface signal values — tracks the GpuEvent signal value from the last
    /// frame that wrote to each surface. Before writing to surface S, we wait for
    /// surface_signal_values[S] to complete (the frame that last used it).
    #[cfg(target_os = "macos")]
    surface_signal_values: [u64; crate::shared_texture::SURFACE_COUNT],
    /// Duration of the last GPU fence wait in milliseconds.
    /// Non-zero means the GPU was still working when the content thread woke up.
    /// Exposed unconditionally for the performance overlay.
    last_fence_wait_ms: f64,
    /// Duration of the last GPU poll (wait for completion) in milliseconds.
    /// Captured inside render_content(), read by the profiler.
    #[cfg(feature = "profiling")]
    gpu_poll_ms: f64,
    /// Native Metal GPU device from manifold-gpu (macOS only).
    /// Shared handle (Metal device + command queue) for native encoding. An
    /// `Arc` — not an owned `GpuDevice` — so the renderers built against it
    /// before `ContentPipeline` reaches its final resting place (see
    /// `Application::resumed()` in app.rs) hold clones of the SAME
    /// allocation rather than a raw pointer that would dangle across the
    /// subsequent moves (BUG-054).
    #[cfg(target_os = "macos")]
    native_device: Option<std::sync::Arc<manifold_gpu::GpuDevice>>,
    /// Native Metal shared event for frame completion (macOS only).
    #[cfg(target_os = "macos")]
    native_event: Option<manifold_gpu::GpuEvent>,
    /// Kernel-notified GPU fence waiter — replaces busy-spin polling.
    /// Registered before each frame to wake the content thread via condvar
    /// when the GPU finishes with the target surface.
    #[cfg(target_os = "macos")]
    fence_waiter: Option<manifold_gpu::GpuFenceWaiter>,
    /// Signal value from the native event.
    #[cfg(target_os = "macos")]
    native_signal_value: u64,
    /// Texture pool backed by MTLHeap for zero-kernel-call allocation.
    #[cfg(target_os = "macos")]
    texture_pool: Option<manifold_gpu::TexturePool>,
    /// Downscale blit used for the workspace preview texture.
    #[cfg(target_os = "macos")]
    preview_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// §24 5c-2 P5: 4×4 box-filter downsample blit for clip-thumbnail capture.
    /// A full-res clip output (e.g. 3456px) into a 128px cell is a ~27× downscale;
    /// a single bilinear tap aliases badly, so the capture averages a tap grid.
    #[cfg(target_os = "macos")]
    clip_downsample_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Linear sampler for preview downscaling.
    #[cfg(target_os = "macos")]
    preview_sampler: Option<manifold_gpu::GpuSampler>,
    /// Scalar-field blit for the node-output preview: a fixed per-channel
    /// black-floor asinh lift (0 stays black, dark values raised), with no
    /// per-frame statistics so it never flickers. Used for density / mask /
    /// depth and as the safe default. The workspace preview keeps the plain
    /// `preview_pipeline`; only node previews use these.
    #[cfg(target_os = "macos")]
    node_preview_scalar_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Vector-field blit: the RG channels are read as a 2D vector and shown as
    /// the standard optical-flow colour wheel (direction → hue, magnitude →
    /// brightness). Used for force fields, flow, gradients, displacement.
    #[cfg(target_os = "macos")]
    node_preview_vector_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Signed-scalar blit: a diverging blue → black → red ramp centred at 0, so
    /// the sign reads at a glance. Used for SDFs, divergence, signed height.
    #[cfg(target_os = "macos")]
    node_preview_signed_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Normal-map blit: xyz in `[-1,1]` decoded to the familiar blue-dominant
    /// RGB. Used for surface normals / bump output.
    #[cfg(target_os = "macos")]
    node_preview_normal_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Depth blit: a perceptual near-far colour ramp (turbo-like) so subtle
    /// distance gradients read instead of washing to flat grey.
    #[cfg(target_os = "macos")]
    node_preview_depth_pipeline: Option<manifold_gpu::GpuRenderPipeline>,
    /// Live node-output preview info from the most recent render — the
    /// previewed node's id, whether it produced an image, and its scalar I/O.
    /// Pulled into [`ContentState`](crate::content_state::ContentState) each
    /// frame so the editor can show a value inspector for non-image nodes.
    last_node_preview_info: Option<crate::content_state::NodePreviewInfo>,
    /// Live (post-modulation) scalar param values for every node of the watched
    /// effect/generator this frame, keyed by stable `NodeId`. Pulled into
    /// [`ContentState`](crate::content_state::ContentState) so the editor canvas
    /// shows values that move under a card slider / driver / Ableton / envelope
    /// instead of the frozen authoring def. Empty whenever no editor is watching.
    last_live_node_params: manifold_renderer::node_graph::LiveNodeParams,
    /// Layer indices occluded this frame by a fully-opaque layer above them
    /// (opaque blend at full opacity replaces every pixel, so nothing below
    /// it can contribute). Occlusion elides ONLY the final blend dispatches
    /// into the composite — generators, sims, clip playback, and per-layer
    /// effect chains all keep running every frame, so no state anywhere
    /// depends on visibility. Recomputed every frame (opacity/blend are live
    /// performance surfaces); pre-allocated scratch, no per-frame allocation.
    occluded_layers_scratch: Vec<i32>,
    /// Subset of `occluded_layers_scratch` that is safe to skip RENDERING
    /// entirely this frame (not just blend): plain top-level leaf layers,
    /// not routed to LED, hidden behind a full-opacity `Opaque` layer. Their
    /// generators AND effect chains are skipped — the big perf win when rapid
    /// clips / audio triggers stack many layers under an opaque hit.
    ///
    /// Safety rests on the occluder gate (`Opaque` blend AT opacity 1.0): a
    /// skipped layer always resumes rendering BEFORE it can become visible
    /// again — either the occluder hard-cuts off (any state pop hides inside
    /// the same hard cut) or it fades, dropping below opacity 1.0, which
    /// un-occludes and restarts these layers while still hidden behind the
    /// near-opaque wall. So even stateful layers (feedback, sims) can be
    /// skipped without a visible discontinuity. LED-tapped layers and any
    /// authoring node-preview are excluded (they consume hidden output).
    /// Groups and grouped children are never skipped (only blend-skipped).
    /// Empty whenever the optimization is disabled or a preview is active.
    render_skip_scratch: Vec<i32>,
    /// Toggle for the occlusion render-skip optimization above. Read once at
    /// construction from `MANIFOLD_OCCLUSION_RENDER_SKIP` (default on; set to
    /// `0` to A/B the perf win against today's blend-skip-only behavior).
    occlusion_render_skip_enabled: bool,
    /// §8 D5: master/global effect chains have no owning layer, so their
    /// audio-trigger fires accumulate here instead of on a `GeneratorRenderer`
    /// layer state. Clip contribution is always 0 for master (no clip
    /// lifecycle); this counter alone is the master chain's effective
    /// `trigger_count`. Bumped by [`Self::apply_trigger_pulses`], read into
    /// `CompositorFrame.master_trigger_count` each frame.
    master_trigger_count: u32,
    /// Whether the node-output preview applies its smart (semantic) encoding.
    /// On by default; toggled from the editor's preview pane. Only affects the
    /// node preview pane, never the live render or workspace preview.
    node_preview_normalize: bool,
    /// Active live recording session. `Some` while recording, `None` otherwise.
    /// Managed by ContentThread via `set_recording_session` / `take_recording_session`.
    #[cfg(target_os = "macos")]
    pub(crate) recording_session: Option<manifold_recording::LiveRecordingSession>,

    /// Current LED grid dimensions (strip_count, leds_per_strip). Used to size
    /// the per-layer LED composite buffer at native LED resolution so each
    /// strip maps 1:1 to one column. Updated by ContentThread when the LED
    /// controller is initialized; defaults to LedSettings defaults otherwise.
    led_grid_size: (u32, u32),

    /// PERF_BUDGET_GATE_DESIGN P2 / D6: per-dispatch GPU timestamp sampler,
    /// created once (sized against the fixture's span count — see
    /// `set_profiling`'s doc) and re-attached to both the Generators and
    /// Compositor command buffers every profiled frame. `None` when
    /// profiling is off or unsupported on this device.
    #[cfg(target_os = "macos")]
    profiling_sampler: Option<manifold_gpu::GpuTimestampSampler>,
    /// Whether `--profile` mode is on this run. Forces `composite_serial`
    /// (via `compositor.set_force_serial`) and switches both command-buffer
    /// commits to the `_profiled` variant. Off by default — zero cost on the
    /// live path (no sampler attached, no extra wait).
    profiling_enabled: bool,
    /// Resolved [`manifold_gpu::GpuFrameProfile`]s from the last profiled
    /// frame: `(command_buffer_label, profile)` — `"Generators"` and
    /// `"Compositor"`, since D6's forced-serial mode still runs them as two
    /// separate command buffers (only the compositor's internal per-layer
    /// buffers collapse to one). Drained by [`Self::take_gpu_profiles`].
    #[cfg(target_os = "macos")]
    last_gpu_profiles: Vec<(&'static str, manifold_gpu::GpuFrameProfile)>,
}

/// The semantic node-preview render pipelines, borrowed as one bundle so the
/// preview blit takes a single argument instead of one `Option<&pipeline>` per
/// [`PreviewEncoding`](manifold_renderer::node_graph::PreviewEncoding). `raw`
/// is the plain blit used for `Color` and smart-off.
#[cfg(target_os = "macos")]
struct PreviewPipelines<'a> {
    scalar_lift: Option<&'a manifold_gpu::GpuRenderPipeline>,
    scalar_signed: Option<&'a manifold_gpu::GpuRenderPipeline>,
    vector: Option<&'a manifold_gpu::GpuRenderPipeline>,
    normal: Option<&'a manifold_gpu::GpuRenderPipeline>,
    depth: Option<&'a manifold_gpu::GpuRenderPipeline>,
    raw: Option<&'a manifold_gpu::GpuRenderPipeline>,
}

impl ContentPipeline {
    pub fn new(compositor: Box<dyn Compositor>) -> Self {
        let shared = Arc::new(SharedOutputView::new());
        Self {
            compositor,
            edr_headroom: 1.0,
            pq_encoder: None,
            #[cfg(target_os = "macos")]
            still_readback: manifold_renderer::gpu_readback::ReadbackRequest::new(),
            shared_output: shared,
            #[cfg(target_os = "macos")]
            metalfx: None,
            #[cfg(target_os = "macos")]
            fsr1: None,
            output_w: 1920,
            output_h: 1080,
            #[cfg(target_os = "macos")]
            output_surface: None,
            #[cfg(target_os = "macos")]
            output_present_suspended: false,
            #[cfg(target_os = "macos")]
            output_pipeline: None,
            #[cfg(target_os = "macos")]
            output_sampler: None,
            #[cfg(target_os = "macos")]
            preview_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            write_surface_index: 0,
            #[cfg(target_os = "macos")]
            preview_bridge: None,
            #[cfg(target_os = "macos")]
            preview_generation: 0,
            node_preview_request: None,
            node_preview_generator: None,
            last_node_preview_info: None,
            last_live_node_params: Vec::new(),
            occluded_layers_scratch: Vec::new(),
            render_skip_scratch: Vec::new(),
            occlusion_render_skip_enabled: std::env::var("MANIFOLD_OCCLUSION_RENDER_SKIP")
                .map(|v| v != "0")
                .unwrap_or(true),
            master_trigger_count: 0,
            pending_graph_dump: None,
            #[cfg(target_os = "macos")]
            node_preview_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            node_preview_bridge: None,
            node_atlas_visible: Vec::new(),
            #[cfg(target_os = "macos")]
            node_atlas_textures: [None, None, None],
            #[cfg(target_os = "macos")]
            node_atlas_bridge: None,
            last_node_atlas_layout: Vec::new(),
            clip_atlas_visible: Vec::new(),
            #[cfg(target_os = "macos")]
            clip_atlas_persistent: None,
            #[cfg(target_os = "macos")]
            clip_atlas_surface: None,
            clip_atlas_cache: crate::clip_atlas::ClipAtlasCache::new(CLIP_ATLAS_CELLS as u32),
            clip_atlas_last_snapshot: AHashMap::new(),
            clip_atlas_frame: 0,
            last_clip_atlas_layout: Vec::new(),
            clip_atlas_pending_layout: None,
            clip_thumb_cache: crate::clip_thumb_cache::ClipThumbCache::new(
                CLIP_ATLAS_CELL_W,
                CLIP_ATLAS_CELL_H,
            ),
            #[cfg(target_os = "macos")]
            clip_atlas_readback: manifold_renderer::gpu_readback::ReadbackRequest::new(),
            clip_atlas_persist_due: 0,
            clip_atlas_persist_pending: None,
            #[cfg(target_os = "macos")]
            clip_atlas_restore_staging: None,
            clip_atlas_pending_loads: AHashMap::new(),
            #[cfg(target_os = "macos")]
            surface_signal_values: [0; crate::shared_texture::SURFACE_COUNT],
            last_fence_wait_ms: 0.0,
            #[cfg(feature = "profiling")]
            gpu_poll_ms: 0.0,
            #[cfg(target_os = "macos")]
            native_device: None,
            #[cfg(target_os = "macos")]
            native_event: None,
            #[cfg(target_os = "macos")]
            fence_waiter: None,
            #[cfg(target_os = "macos")]
            native_signal_value: 0,
            #[cfg(target_os = "macos")]
            texture_pool: None,
            #[cfg(target_os = "macos")]
            preview_pipeline: None,
            #[cfg(target_os = "macos")]
            clip_downsample_pipeline: None,
            #[cfg(target_os = "macos")]
            preview_sampler: None,
            #[cfg(target_os = "macos")]
            node_preview_scalar_pipeline: None,
            #[cfg(target_os = "macos")]
            node_preview_vector_pipeline: None,
            #[cfg(target_os = "macos")]
            node_preview_signed_pipeline: None,
            #[cfg(target_os = "macos")]
            node_preview_normal_pipeline: None,
            #[cfg(target_os = "macos")]
            node_preview_depth_pipeline: None,
            node_preview_normalize: false,
            #[cfg(target_os = "macos")]
            recording_session: None,
            led_grid_size: (
                manifold_led::DEFAULT_STRIP_COUNT,
                manifold_led::DEFAULT_LEDS_PER_STRIP,
            ),
            #[cfg(target_os = "macos")]
            profiling_sampler: None,
            profiling_enabled: false,
            #[cfg(target_os = "macos")]
            last_gpu_profiles: Vec::new(),
        }
    }

    /// PERF_BUDGET_GATE_DESIGN P2 / D6: turn per-dispatch GPU attribution
    /// profiling on/off for this content pipeline's next frames. `max_spans`
    /// sizes the sampler (two counter samples per span) — the caller (the
    /// `--profile` xtask) verifies this against the fixture's actual span
    /// count and reports overflow rather than silently truncating (D6's
    /// capacity check). Forces `composite_serial` in the compositor (D6
    /// correction) and fans profiling out to every effect chain + generator
    /// via `Compositor::set_profiling` / `GeneratorRenderer::set_profiling`.
    /// Turning this off drops the sampler and un-forces serial compositing.
    #[cfg(all(target_os = "macos", feature = "perf-soak"))]
    pub fn set_profiling(&mut self, on: bool, max_spans: usize) {
        self.profiling_enabled = on;
        self.compositor.set_profiling(on);
        self.compositor.set_force_serial(on);
        self.profiling_sampler = if on {
            self.native_device
                .as_ref()
                .and_then(|d| d.create_timestamp_sampler(max_spans))
        } else {
            None
        };
    }

    /// Whether the sampler requested by [`Self::set_profiling`] was actually
    /// created — `false` means the device didn't support counter sampling at
    /// all (never silently downgrades to unprofiled; the caller must report
    /// this, not proceed as if profiling were on).
    #[cfg(all(target_os = "macos", feature = "perf-soak"))]
    pub fn profiling_sampler_ready(&self) -> bool {
        self.profiling_sampler.is_some()
    }

    /// This run's sampler capacity in spans (two samples per span) — the D6
    /// capacity check compares this against the fixture's actual per-frame
    /// span count. `None` if profiling isn't on / the sampler wasn't created.
    #[cfg(all(target_os = "macos", feature = "perf-soak"))]
    pub fn profiling_sampler_capacity(&self) -> Option<usize> {
        self.profiling_sampler.as_ref().map(|s| s.max_spans())
    }

    /// Drain the last profiled frame's resolved GPU command-buffer profiles
    /// (`"Generators"`, `"Compositor"`) — empty when profiling is off or no
    /// frame has run yet.
    #[cfg(all(target_os = "macos", feature = "perf-soak"))]
    pub fn take_gpu_profiles(&mut self) -> Vec<(&'static str, manifold_gpu::GpuFrameProfile)> {
        std::mem::take(&mut self.last_gpu_profiles)
    }

    /// Drain the compositor's owned chains' per-step CPU profiles from the
    /// last profiled frame (screen + LED + master effect chains). Generator
    /// profiles are separate — `GeneratorRenderer` lives on
    /// `PlaybackEngine::renderers`, not on `ContentPipeline`; the caller
    /// (the `--profile` xtask) drains those directly via
    /// `engine.renderers_mut()` + `as_any_mut().downcast_mut::<GeneratorRenderer>()`
    /// and combines both lists.
    #[cfg(feature = "perf-soak")]
    pub fn take_step_profiles(&mut self) -> Vec<manifold_renderer::node_graph::StepProfile> {
        self.compositor.take_step_profiles()
    }

    /// Update the LED grid dimensions. Called by ContentThread when the LED
    /// controller is initialized so the compositor sizes the per-layer LED
    /// composite to match the strip grid (1 column per strip).
    pub fn set_led_grid_size(&mut self, strip_count: u32, leds_per_strip: u32) {
        self.led_grid_size = (strip_count.max(1), leds_per_strip.max(1));
    }

    /// Initialize the native Metal GPU device, event, and texture pool.
    /// Called once at startup after the content pipeline is created.
    #[cfg(target_os = "macos")]
    /// Set a pre-created native GPU device (transfers ownership).
    /// Used when the device must exist before the content pipeline (e.g. for
    /// compositor native pipeline creation).
    #[cfg(target_os = "macos")]
    pub fn set_native_gpu(&mut self, device: std::sync::Arc<manifold_gpu::GpuDevice>) {
        let event = device.create_event();
        // 3 frames in flight (triple buffering).
        let pool = device.create_texture_pool(3);
        let preview_shader = r#"
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
        self.preview_pipeline = Some(device.create_render_pipeline(
            preview_shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Workspace Preview Blit",
        ));
        self.preview_sampler = Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            ..Default::default()
        }));

        // §24 5c-2 P5: box-filter downsample for clip-thumbnail capture (anti-alias
        // the big full-res→cell downscale). Reusable helper (unit-tested in
        // manifold-renderer).
        self.clip_downsample_pipeline =
            Some(manifold_renderer::clip_thumb_gpu::create_box_downsample_pipeline(
                &device,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                CLIP_ATLAS_CELL_W,
                CLIP_ATLAS_CELL_H,
            ));

        // Node-preview semantic encodings. Both share the fullscreen-triangle
        // vertex stage above and use NO per-frame statistics, so they're stable
        // across frames and outliers (a data-derived window flickers and
        // crushes to black). `asinh` is the shared lift curve: ~linear near 0
        // (faithful for small values), log-compressed far out (no clipping).
        // K = 0.05 (inv_k = 20) sets the linear→log knee around sim-scale data.
        let preview_vs = r#"
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
// asinh(x) = log(x + sqrt(x*x + 1)); arg is always > 0, valid for negative x.
fn asinh_approx(x: f32) -> f32 {
    return log(x + sqrt(x * x + 1.0));
}
"#;

        // Scalar lift: 0 stays black (clamp negatives), dark values raised.
        let scalar_shader = format!(
            "{preview_vs}
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
fn lift(c: f32, inv_k: f32, norm: f32) -> f32 {{
    return asinh_approx(max(c, 0.0) * inv_k) / norm;
}}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {{
    let c = textureSample(t_source, s_source, in.uv).rgb;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);     // so c = 1 maps to white
    let rgb = vec3<f32>(
        lift(c.r, inv_k, norm),
        lift(c.g, inv_k, norm),
        lift(c.b, inv_k, norm),
    );
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}}"
        );
        self.node_preview_scalar_pipeline = Some(device.create_render_pipeline(
            &scalar_shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Node Preview Scalar Lift",
        ));

        // Vector field: RG → 2D vector → optical-flow colour wheel
        // (direction = hue, magnitude = brightness). Zero vector → black.
        let vector_shader = format!(
            "{preview_vs}
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {{
    let p = abs(fract(vec3<f32>(h) + vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {{
    let v = textureSample(t_source, s_source, in.uv).rg;
    let two_pi = 6.2831853;
    let hue = atan2(v.g, v.r) / two_pi + 0.5;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);
    let val = clamp(asinh_approx(length(v) * inv_k) / norm, 0.0, 1.0);
    return vec4<f32>(hsv2rgb(hue, 1.0, val), 1.0);
}}"
        );
        self.node_preview_vector_pipeline = Some(device.create_render_pipeline(
            &vector_shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Node Preview Vector Wheel",
        ));

        // Signed scalar: diverging ramp centred at 0. Negative → blue, 0 →
        // black, positive → red, with the asinh lift on |value| so small
        // swings either side of zero are visible. No per-frame statistics.
        let signed_shader = format!(
            "{preview_vs}
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {{
    let c = textureSample(t_source, s_source, in.uv).r;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);
    let mag = clamp(asinh_approx(abs(c) * inv_k) / norm, 0.0, 1.0);
    let pos = vec3<f32>(0.9, 0.2, 0.15);   // warm red for c > 0
    let neg = vec3<f32>(0.15, 0.35, 0.95); // cool blue for c < 0
    let tint = select(neg, pos, c >= 0.0);
    return vec4<f32>(tint * mag, 1.0);
}}"
        );
        self.node_preview_signed_pipeline = Some(device.create_render_pipeline(
            &signed_shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Node Preview Signed Diverging",
        ));

        // Normal map: decode xyz from [-1,1] to the familiar blue-dominant RGB.
        // Tolerant of already-encoded [0,1] normals (re-normalising a unit-ish
        // vector is a no-op). No per-frame statistics.
        let normal_shader = format!(
            "{preview_vs}
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {{
    let raw = textureSample(t_source, s_source, in.uv).xyz;
    // Treat values already in [0,1] as encoded; those spanning negatives as
    // raw [-1,1]. Decode raw → [0,1] for display.
    let mn = min(min(raw.x, raw.y), raw.z);
    let n = select(raw, raw * 0.5 + vec3<f32>(0.5), mn < 0.0);
    return vec4<f32>(clamp(n, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}}"
        );
        self.node_preview_normal_pipeline = Some(device.create_render_pipeline(
            &normal_shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Node Preview Normal Decode",
        ));

        // Depth: a turbo-like near-far colour ramp. Depth is read from R; the
        // asinh lift spreads near values (where detail clusters) without a
        // per-frame window, so it never flickers as the scene changes.
        let depth_shader = format!(
            "{preview_vs}
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
// Compact turbo approximation: smooth blue→cyan→green→yellow→red.
fn turbo(t: f32) -> vec3<f32> {{
    let x = clamp(t, 0.0, 1.0);
    let r = clamp(1.5 - abs(2.0 * x - 1.5) * 2.0, 0.0, 1.0);
    let g = clamp(1.5 - abs(2.0 * x - 1.0) * 2.0, 0.0, 1.0);
    let b = clamp(1.5 - abs(2.0 * x - 0.5) * 2.0, 0.0, 1.0);
    return vec3<f32>(r, g, b);
}}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {{
    let d = textureSample(t_source, s_source, in.uv).r;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);
    let t = clamp(asinh_approx(max(d, 0.0) * inv_k) / norm, 0.0, 1.0);
    return vec4<f32>(turbo(t), 1.0);
}}"
        );
        self.node_preview_depth_pipeline = Some(device.create_render_pipeline(
            &depth_shader,
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
            "Node Preview Depth Ramp",
        ));

        self.native_device = Some(device);
        self.native_event = Some(event);
        self.fence_waiter = Some(manifold_gpu::GpuFenceWaiter::new());
        self.texture_pool = Some(pool);
    }

    /// Reference to the native GPU device (if initialized).
    #[cfg(target_os = "macos")]
    pub fn native_device(&self) -> Option<&manifold_gpu::GpuDevice> {
        self.native_device.as_deref()
    }

    /// Raw Metal device pointer for FFI interop (encoder sharing).
    #[cfg(target_os = "macos")]
    pub fn native_device_ptr(&self) -> Option<*mut std::ffi::c_void> {
        self.native_device.as_ref().map(|d| d.raw_device_ptr())
    }

    /// Duration the content thread blocked waiting for a GPU surface to become
    /// available (ms). Non-zero means the GPU was still processing a frame from
    /// 2 frames ago when the content thread woke up — a sign of GPU saturation.
    pub fn last_fence_wait_ms(&self) -> f64 {
        self.last_fence_wait_ms
    }

    // ── Surface readiness (GPU fence notification) ──────────────────────

    /// Check if the surface is ready (GPU already finished, or no pending work).
    #[cfg(target_os = "macos")]
    pub fn is_surface_ready(&self) -> bool {
        let pending = self.surface_signal_values[self.write_surface_index];
        if pending == 0 {
            return true;
        }
        self.native_event
            .as_ref()
            .is_none_or(|e| e.is_done(pending))
    }

    /// Register a GPU notification for when the current surface becomes
    /// available. When the GPU signals, `SurfaceReady` is sent through
    /// `cmd_tx` to wake the content thread's `recv()`.
    ///
    /// Returns `true` if a wait is needed (notification registered),
    /// `false` if the surface is already ready.
    #[cfg(target_os = "macos")]
    pub fn register_surface_notify(
        &self,
        cmd_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    ) -> bool {
        let pending = self.surface_signal_values[self.write_surface_index];
        if pending == 0 {
            return false;
        }
        if let (Some(event), Some(waiter)) = (&self.native_event, &self.fence_waiter) {
            if event.is_done(pending) {
                return false;
            }
            let tx = cmd_tx.clone();
            waiter.register(event, pending, move || {
                let _ = tx.send(crate::content_command::ContentCommand::SurfaceReady);
            });
            true
        } else {
            false
        }
    }

    /// Handle GPU timeout — clear stale signal to prevent infinite blocking.
    #[cfg(target_os = "macos")]
    pub fn handle_surface_timeout(&mut self) {
        let idx = self.write_surface_index;
        let pending = self.surface_signal_values[idx];
        let signaled = self.native_event.as_ref().map_or(0, |e| e.signaled_value());
        log::error!(
            "[ContentPipeline] GPU timeout waiting for surface {} \
             (signal={}, signaled={})",
            idx,
            pending,
            signaled,
        );
        self.surface_signal_values[idx] = 0;
    }

    /// Set the last fence wait duration (called from content thread).
    pub fn set_last_fence_wait_ms(&mut self, ms: f64) {
        self.last_fence_wait_ms = ms;
    }

    /// Current output resolution (post-upscale).
    pub fn output_dimensions(&self) -> (u32, u32) {
        (self.output_w, self.output_h)
    }

    /// Attach an output surface for direct-to-drawable presentation.
    /// Creates the blit pipeline and sampler lazily.
    #[cfg(target_os = "macos")]
    pub fn set_output_surface(&mut self, surface: manifold_gpu::GpuSurface) {
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);
        surface.set_maximum_drawable_count(3);
        surface.set_presents_with_transaction(false);
        if self.output_pipeline.is_none()
            && let Some(ref device) = self.native_device
        {
            let shader = r#"
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
            self.output_pipeline = Some(device.create_render_pipeline(
                shader,
                "vs_main",
                "fs_main",
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                None,
                "Output Present Blit",
            ));
            self.output_sampler = Some(device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                ..Default::default()
            }));
        }
        self.output_surface = Some(surface);
        log::info!("[ContentPipeline] Output surface attached — direct present");
    }

    /// Resize the output surface drawable (fullscreen toggle).
    #[cfg(target_os = "macos")]
    pub fn resize_output_surface(&mut self, width: u32, height: u32) {
        if let Some(ref mut surface) = self.output_surface {
            surface.resize(width, height);
        }
    }

    /// Suspend or resume direct present to the output drawable.
    #[cfg(target_os = "macos")]
    pub fn set_output_present_suspended(&mut self, suspended: bool) {
        self.output_present_suspended = suspended;
    }

    /// Detach the output surface (output window closed).
    #[cfg(target_os = "macos")]
    pub fn clear_output_surface(&mut self) {
        self.output_surface = None;
    }

    #[cfg(target_os = "macos")]
    pub fn set_preview_textures(
        &mut self,
        textures: [manifold_gpu::GpuTexture; crate::shared_texture::SURFACE_COUNT],
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.preview_textures = textures.map(Some);
        self.preview_bridge = Some(bridge);
    }

    /// Install the IOSurface textures + bridge for the node-output preview.
    /// Mirrors [`Self::set_preview_textures`] but feeds the graph editor's
    /// per-node preview pane instead of the workspace view.
    #[cfg(target_os = "macos")]
    pub fn set_node_preview_textures(
        &mut self,
        textures: [manifold_gpu::GpuTexture; crate::shared_texture::SURFACE_COUNT],
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.node_preview_textures = textures.map(Some);
        self.node_preview_bridge = Some(bridge);
    }

    /// Set (or clear) the node-output preview request `(watched effect,
    /// selected node)`. Forwarded to the compositor each frame; the chain
    /// holding the watched effect preserves the selected node's output.
    pub fn set_node_preview_request(&mut self, request: Option<(EffectId, Option<NodeId>)>) {
        self.node_preview_request = request;
    }

    /// Install the IOSurface textures + bridge for the per-node thumbnail atlas.
    #[cfg(target_os = "macos")]
    pub fn set_node_atlas_textures(
        &mut self,
        textures: [manifold_gpu::GpuTexture; crate::shared_texture::SURFACE_COUNT],
        bridge: Arc<crate::shared_texture::SharedTextureBridge>,
    ) {
        self.node_atlas_textures = textures.map(Some);
        self.node_atlas_bridge = Some(bridge);
    }

    /// Set the nodes the editor canvas can currently show, for per-node
    /// thumbnail capture. The UI sets this (deduped) only while the graph editor
    /// is open, and an empty vec when it closes — so a live show pays nothing.
    pub fn set_node_atlas_visible(&mut self, visible: Vec<NodeId>) {
        self.node_atlas_visible = visible;
    }

    /// The `(node_id, cell_index)` layout from the last atlas capture.
    pub fn node_atlas_layout(&self) -> &[(NodeId, u32)] {
        &self.last_node_atlas_layout
    }

    /// Install the content-side texture + keep-alive `Arc` for the single
    /// shared clip-thumbnail atlas surface (§24 5c, BUG-119). The caller must
    /// have already cleared `texture` once (Metal doesn't zero-init) — this
    /// is the atlas's ONE lifetime clear; every later write is `LoadAction::Load`.
    #[cfg(target_os = "macos")]
    pub fn set_clip_atlas_texture(
        &mut self,
        texture: manifold_gpu::GpuTexture,
        surface: Arc<crate::shared_texture::SharedAtlasSurface>,
    ) {
        self.clip_atlas_persistent = Some(texture);
        self.clip_atlas_surface = Some(surface);
    }

    /// Set the clips that currently want a timeline thumbnail (deduped by the UI).
    /// Empty = no timeline visible, so the snapshot path is skipped entirely.
    pub fn set_clip_atlas_visible(&mut self, visible: Vec<ClipId>) {
        self.clip_atlas_visible = visible;
    }

    /// The `(clip_id, filmstrip cell, atlas cell)` layout for the clip filmstrip atlas.
    pub fn clip_atlas_layout(&self) -> &[(ClipId, u32, u32)] {
        &self.last_clip_atlas_layout
    }


    /// Request a one-shot dump of every output of `effect_id` to `dir` on the
    /// next render (the watched effect's node outputs, read back to disk as
    /// 16-bit PNGs + a manifest).
    pub fn request_graph_dump(&mut self, effect_id: EffectId, dir: std::path::PathBuf) {
        self.pending_graph_dump = Some((effect_id, dir));
    }

    /// Generator-side counterpart of [`Self::set_node_preview_request`]:
    /// `(watched layer, selected node)`. Mutually exclusive with the effect
    /// request — the content thread sets at most one.
    pub fn set_node_preview_generator(&mut self, request: Option<(LayerId, Option<NodeId>)>) {
        self.node_preview_generator = request;
    }

    /// Get a clone of the shared output handle. The UI thread holds this
    /// to read the front buffer view and dimensions.
    pub fn shared_output(&self) -> Arc<SharedOutputView> {
        Arc::clone(&self.shared_output)
    }

    /// §8 P2: fold this tick's audio-trigger fires into the renderer's
    /// per-layer (or master) `audio_count`. `pulses` is
    /// `PlaybackEngine::take_trigger_pulses`'s output for this tick — pure
    /// bookkeeping, no GPU work. A `Some(layer_id)` pulse bumps that layer's
    /// `GeneratorRenderer` counter (a no-op if the layer's generator was
    /// deleted the same tick); `None` (D5: master/global chains have no
    /// layer) bumps `master_trigger_count`. Takes the counter by `&mut u32`
    /// (not `&mut self`) so the call site can hold it disjoint from other
    /// live borrows of `self` (e.g. `self.texture_pool.as_ref()`).
    fn apply_trigger_pulses(
        master_trigger_count: &mut u32,
        pulses: &[manifold_playback::modulation::TriggerPulse],
        renderers: &mut [Box<dyn manifold_playback::renderer::ClipRenderer>],
    ) {
        if pulses.is_empty() {
            return;
        }
        let mut gen_renderer = renderers
            .iter_mut()
            .find_map(|r| r.as_any_mut().downcast_mut::<GeneratorRenderer>());
        for pulse in pulses {
            match &pulse.layer_id {
                Some(layer_id) => {
                    if let Some(gr) = gen_renderer.as_deref_mut() {
                        gr.bump_audio_count(layer_id);
                    }
                }
                None => {
                    *master_trigger_count = master_trigger_count.wrapping_add(1);
                }
            }
        }
    }

    /// Render all generators and composite, then submit asynchronously.
    ///
    /// Uses native Metal encoding on macOS via manifold-gpu.
    /// IOSurface double-buffering for zero-copy cross-device sharing.
    ///
    /// When `export_mode` is true, skips IOSurface wait/blit/swap — the export
    /// pipeline reads directly from `export_output_texture()` and doesn't need
    /// the cross-device surface bridge.
    pub fn render_content(
        &mut self,
        gpu: &manifold_renderer::gpu::GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
        export_mode: bool,
        data_version: u64,
    ) {
        let _t_frame = std::time::Instant::now();

        // Surface wait is now handled by the content thread main loop
        // (wait_for_surface_draining_commands) which keeps processing commands
        // instead of busy-spinning. Export mode waits via wait_for_gpu_idle().
        let _poll_ms = self.last_fence_wait_ms;

        // Extract timing values before split borrow. Time/beat stay f64 from
        // the playback clock all the way to the GPU uniform boundary — no f32
        // round-trip — so beat phase stays exact over a long show.
        let time_f64 = engine.current_time_double();
        let beat_f64 = engine.current_beat_f64();

        // === NATIVE METAL PATH ===
        // When manifold-gpu is initialized, use raw Metal encoding.
        // Native Metal encoding path.
        #[cfg(target_os = "macos")]
        if self.native_device.is_some() {
            self.render_content_native(
                gpu,
                engine,
                tick_result,
                dt,
                frame_count,
                time_f64,
                beat_f64,
                _t_frame,
                _poll_ms,
                export_mode,
                data_version,
            );
        }

        // Non-macOS: not yet supported (native Metal path required).
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (
                gpu,
                engine,
                tick_result,
                dt,
                frame_count,
                time_f64,
                beat_f64,
            );
            log::warn!("[ContentPipeline] Non-macOS render path not available");
        }
    }

    /// Native Metal render path.
    ///
    /// Uses manifold_gpu::GpuDevice + GpuEncoder for ALL encoding.
    /// Generators/effects dispatch through the native encoder via GpuEncoder wrapper.
    #[cfg(target_os = "macos")]
    #[allow(clippy::too_many_arguments)]
    fn render_content_native(
        &mut self,
        _gpu: &manifold_renderer::gpu::GpuContext,
        engine: &mut PlaybackEngine,
        tick_result: &TickResult,
        dt: f64,
        frame_count: u64,
        time_f64: f64,
        beat_f64: f64,
        _t_frame: std::time::Instant,
        _poll_ms: f64,
        export_mode: bool,
        data_version: u64,
    ) {
        // One-shot graph dump: consume the request as a local so the borrow of
        // `self.pending_graph_dump` ends here. The compositor captures during
        // the frame; the readback runs after the compositor CB commits.
        let pending_dump = self.pending_graph_dump.take();
        let native_device = self.native_device.as_ref().unwrap();

        // Spike-triggered sub-phase trace (BUG-035): with MANIFOLD_RENDER_TRACE=1,
        // any content frame over 20ms prints a per-section breakdown to stderr.
        // Off: one bool check per section. The profiler's render_content_ms is a
        // single lump; this names which section a rare ~59ms spike lives in.
        struct RenderTrace {
            on: bool,
            last: std::time::Instant,
            marks: Vec<(&'static str, f64)>,
        }
        impl RenderTrace {
            #[inline]
            fn mark(&mut self, label: &'static str) {
                if self.on {
                    let now = std::time::Instant::now();
                    self.marks
                        .push((label, now.duration_since(self.last).as_secs_f64() * 1000.0));
                    self.last = now;
                }
            }
        }
        static TRACE_ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let mut rtrace = RenderTrace {
            on: *TRACE_ON.get_or_init(|| std::env::var_os("MANIFOLD_RENDER_TRACE").is_some()),
            last: std::time::Instant::now(),
            marks: Vec::new(),
        };
        // Xcode capture boundary: one content frame. The device capture scope
        // brackets everything committed below (generators + compositor) so the
        // camera button grabs the full content workload, not just the UI pass.
        native_device.capture_scope_begin();
        // Reset the node-preview inspector info; the active preview path below
        // repopulates it for this frame.
        self.last_node_preview_info = None;
        // Reset the editor canvas's live node-param values; the watched effect
        // or generator path below repopulates them post-render so the canvas
        // shows live (modulated) values, not the frozen authoring def. Stays
        // empty whenever no editor is watching — zero cost on the closed path.
        self.last_live_node_params.clear();
        // Whether a node-thumbnail capture block actually wrote the atlas this
        // frame. The triple-buffered atlas front is published ONLY when this is
        // set — publishing on every atlas-on frame (even ones whose
        // dump came back empty during setup lag or while the watched layer
        // wasn't rendering) flips the UI to a freshly-cleared, all-transparent
        // surface + empty layout, which is the editor's "strobe to black".
        let mut atlas_filled_this_frame = false;
        let texture_pool = self.texture_pool.as_ref();

        // §8 P2: drain this tick's audio-trigger fires (P1's evaluator
        // output) before the split borrow below — `take_trigger_pulses`
        // needs `&mut engine` in full, same as `split_renderer_project`.
        let trigger_pulses = engine.take_trigger_pulses();

        // Split borrow: get renderers + project from engine simultaneously.
        let (renderers, project) = engine.split_renderer_project();
        let layers = project.map(|p| p.timeline.layers.as_slice()).unwrap_or(&[]);

        // Fold this tick's fires into the renderer's per-layer/master
        // audio_count BEFORE generators render, so the same frame's
        // trigger_count already reflects the fire (no one-frame lag).
        Self::apply_trigger_pulses(
            &mut self.master_trigger_count,
            &trigger_pulses,
            renderers.as_mut_slice(),
        );

        // ── Generators (separate CB, committed first) ─────────────────
        // Generators must commit before the compositor because the parallel
        // compositor path creates per-layer CBs that are also committed.
        // Metal executes CBs in commit order, so committing generators first
        // guarantees their texture writes are visible to the per-layer CBs.
        let _t0 = std::time::Instant::now();

        // Advance the pool's frame counter — drives frame-stamped recycling.
        // Prune stale textures every 300 frames (~5s at 60fps) to free GPU memory
        // after resolution changes or project switches.
        if let Some(pool) = texture_pool {
            pool.begin_frame();
            if pool.current_frame() % 300 == 0 {
                // Lasting memory diagnostic: set MANIFOLD_POOL_STATS=1 to log the
                // per-resolution pool breakdown (dims/format/bytes/age) every ~5s
                // — run the Liveschool fixture with it to see what's dead and why.
                // Off by default (env read is cheap at this 300-frame cadence).
                if std::env::var_os("MANIFOLD_POOL_STATS").is_some() {
                    log::info!("{}", pool.report());
                }
                pool.prune_stale(300);
            }
        }

        rtrace.mark("pool_prune");

        // ── Opaque occlusion (blend-skip only) ────────────────────────
        // The topmost fully-opaque top-level layer replaces every pixel
        // beneath it (Opaque blend ignores the base), so the compositor
        // skips blending the layers below. Everything still RENDERS —
        // generators, sims, and effect chains advance normally.
        compute_occluded_layer_indices(
            layers,
            &tick_result.ready_clips,
            &mut self.occluded_layers_scratch,
        );
        // Render-skip: of the occluded layers, which are safe to not render at
        // all this frame (generators + effect chains), not just skip blending.
        // Disabled while a node preview is open — a previewed layer's hidden
        // output is still consumed by the editor.
        let preview_active = self.node_preview_request.is_some()
            || self.node_preview_generator.is_some()
            || !self.node_atlas_visible.is_empty();
        compute_render_skip_indices(
            layers,
            &self.occluded_layers_scratch,
            self.occlusion_render_skip_enabled,
            preview_active,
            &mut self.render_skip_scratch,
        );

        {
            let mut gen_enc = native_device.create_encoder("Generators");
            // PERF_BUDGET_GATE_DESIGN P2 / D6: attach the dispatch sampler to
            // this command buffer when a --profile run is active. Every
            // generator's executor was already scoped (`gen:{layer_id}`) at
            // chain-insertion time (`GeneratorRenderer::install_layer_generator`).
            if self.profiling_enabled
                && let Some(sampler) = self.profiling_sampler.clone()
            {
                gen_enc.enable_dispatch_profiling(sampler, native_device);
            }
            {
                let mut gpu_gen = if let Some(pool) = texture_pool {
                    GpuEncoder::with_pool(&mut gen_enc, native_device, pool)
                } else {
                    GpuEncoder::new(&mut gen_enc, native_device)
                };

                for renderer in renderers.iter_mut() {
                    if let Some(gen_renderer) =
                        renderer.as_any_mut().downcast_mut::<GeneratorRenderer>()
                    {
                        // Aim (or clear) the node-output preview before render,
                        // and enable the full-graph dump on the watched layer
                        // when the thumbnail atlas is on (so every node's output
                        // is captured this frame — the generator side of the
                        // effect compositor's dump).
                        match &self.node_preview_generator {
                            Some((layer_id, node_id)) => {
                                gen_renderer.set_preview_node(layer_id, node_id.as_ref());
                                gen_renderer.set_dump_visible(layer_id, &self.node_atlas_visible);
                            }
                            None => gen_renderer.clear_preview(),
                        }

                        gen_renderer.render_all(
                            &mut gpu_gen,
                            time_f64,
                            beat_f64,
                            dt as f32,
                            layers,
                            data_version,
                            &self.render_skip_scratch,
                        );
                        break;
                    }
                }
            }
            // Capture: downscale the watched generator's node output into the
            // node-preview surface (raw encoder, after the wrapper is dropped),
            // or clear to black when nothing was captured (non-texture node).
            #[cfg(target_os = "macos")]
            if let Some((layer_id, node_id_opt)) = &self.node_preview_generator {
                let gen_ref = renderers
                    .iter()
                    .find_map(|r| r.as_any().downcast_ref::<GeneratorRenderer>());
                let node_tex = gen_ref.and_then(|gr| gr.preview_texture(layer_id));
                let encoding = gen_ref
                    .map(|gr| gr.preview_encoding(layer_id))
                    .unwrap_or_default();
                // Value-inspector info for a non-image node: its live scalar I/O.
                if let Some(node_id) = node_id_opt {
                    let (inputs, outputs) = gen_ref
                        .map(|gr| gr.preview_scalar_io(layer_id))
                        .unwrap_or_default();
                    self.last_node_preview_info = Some(crate::content_state::NodePreviewInfo {
                        node_id: node_id.clone(),
                        has_image: node_tex.is_some(),
                        inputs,
                        outputs,
                    });
                }
                if let Some(node_tex) = node_tex {
                    Self::update_node_preview(
                        &mut gen_enc,
                        node_tex,
                        self.node_preview_textures[self.write_surface_index].as_ref(),
                        self.node_preview_normalize,
                        encoding,
                        &self.preview_pipelines(),
                        self.preview_sampler.as_ref(),
                    );
                } else if let Some(target) =
                    self.node_preview_textures[self.write_surface_index].as_ref()
                {
                    gen_enc.clear_texture(target, 0.0, 0.0, 0.0, 1.0);
                }
            }

            // ── Per-node thumbnail atlas (generator) ────────────────
            // Mirror of the effect-side atlas capture below, on the generator's
            // dump. Same cell packing; unifies the editor's behaviour across
            // effect and generator graphs.
            #[cfg(target_os = "macos")]
            {
                let mut gen_layout: Option<Vec<(NodeId, u32)>> = None;
                if !self.node_atlas_visible.is_empty()
                    && let Some((layer_id, _)) = self.node_preview_generator.clone()
                    && let (Some(atlas), Some(raw), Some(sampler)) = (
                        self.node_atlas_textures[self.write_surface_index].as_ref(),
                        self.preview_pipelines().raw,
                        self.preview_sampler.as_ref(),
                    )
                    && let Some(gr) = renderers
                        .iter()
                        .find_map(|r| r.as_any().downcast_ref::<GeneratorRenderer>())
                {
                    // Empty dump = the chain hasn't run with dump on yet (setup
                    // lag). Skip the clear+publish entirely so the UI keeps the
                    // last good atlas instead of flashing to a cleared surface.
                    let dump = gr.dump_textures(&layer_id);
                    if !dump.is_empty() {
                        let mut new_layout: Vec<(NodeId, u32)> = Vec::new();
                        gen_enc.clear_texture(atlas, 0.0, 0.0, 0.0, 0.0);
                        for (i, (name, _port, _type_id, tex)) in
                            dump.iter().enumerate().take(ATLAS_CELLS)
                        {
                            // A render-target-only node output can't be sampled —
                            // binding it crashes AGX. Skip its cell.
                            if !thumb_source_shader_readable(name.as_str(), tex) {
                                continue;
                            }
                            gen_enc.draw_fullscreen_viewport(
                                raw,
                                atlas,
                                &[
                                    manifold_gpu::GpuBinding::Texture {
                                        binding: 0,
                                        texture: tex,
                                    },
                                    manifold_gpu::GpuBinding::Sampler {
                                        binding: 1,
                                        sampler,
                                    },
                                ],
                                atlas_cell_viewport(i, tex.width, tex.height),
                                manifold_gpu::GpuLoadAction::Load,
                                "Node Thumbnail Atlas Cell (Generator)",
                            );
                            new_layout.push((NodeId::new(name.as_str()), i as u32));
                        }
                        gen_layout = Some(new_layout);
                        atlas_filled_this_frame = true;
                    }
                }
                if let Some(l) = gen_layout {
                    self.last_node_atlas_layout = l;
                }
            }
            if self.profiling_enabled {
                // D6: profiled commit waits synchronously so the resolved
                // spans are available before this frame's stats are read —
                // the same trade `freeze_profile.rs` makes; never on the
                // live path (profiling_enabled is always false there).
                let profile = gen_enc.commit_and_wait_profiled(native_device);
                self.last_gpu_profiles.push(("Generators", profile));
            } else {
                gen_enc.commit();
            }
        }
        // Tap the watched generator's live node-param values for the editor
        // canvas (post-render, so card / driver / Ableton / envelope writes are
        // already applied). Re-find the renderer here so the `node_preview_generator`
        // borrow from the capture block above is released. Cloning the LayerId is
        // an Arc bump. Only the watched layer is queried.
        if let Some((layer_id, _)) = self.node_preview_generator.clone()
            && let Some(gen_r) = renderers
                .iter()
                .find_map(|r| r.as_any().downcast_ref::<GeneratorRenderer>())
        {
            self.last_live_node_params = gen_r.live_node_params(&layer_id);
        }
        let _gen_ms = _t0.elapsed().as_secs_f64() * 1000.0;
        rtrace.mark("generators");

        // ── Compositor CB (+ direct present, preview, recording) ────
        let mut native_enc = native_device.create_encoder("Compositor");
        // PERF_BUDGET_GATE_DESIGN P2 / D6: same sampler, same command buffer
        // — the compositor was forced to `composite_serial` by
        // `set_profiling` so this IS the single shared compositor command
        // buffer D6 needs. Every chain's executor was already scoped
        // (`fx:{layer_id}`, `master`, `led:{...}`) at chain-insertion time.
        if self.profiling_enabled
            && let Some(sampler) = self.profiling_sampler.clone()
        {
            native_enc.enable_dispatch_profiling(sampler, native_device);
        }

        // ── Build clip + layer descriptors (CPU only) ────────────────
        let _t0 = std::time::Instant::now();
        let empty_effects: &[PresetInstance] = &[];
        let empty_groups: &[EffectGroup] = &[];

        let mut clip_descs: Vec<CompositeClipDescriptor> =
            Vec::with_capacity(tick_result.ready_clips.len());
        for entry in &tick_result.ready_clips {
            let clip_texture = renderers.iter().find_map(|r| {
                if let Some(gen_r) = r.as_any().downcast_ref::<GeneratorRenderer>()
                    && let Some(t) = gen_r.get_clip_texture(&entry.clip_id)
                {
                    return Some(t);
                }
                #[cfg(target_os = "macos")]
                if let Some(vid_r) = r.as_any().downcast_ref::<VideoRenderer>()
                    && let Some(t) = vid_r.get_clip_texture(&entry.clip_id)
                {
                    return Some(t);
                }
                #[cfg(target_os = "macos")]
                if let Some(img_r) = r
                    .as_any()
                    .downcast_ref::<manifold_media::image_renderer::ImageRenderer>()
                    && let Some(t) = img_r.get_clip_texture(&entry.clip_id)
                {
                    return Some(t);
                }
                None
            });
            if let Some(texture) = clip_texture {
                let layer = layers.get(entry.layer_index as usize);
                clip_descs.push(CompositeClipDescriptor {
                    clip_id: &entry.clip_id,
                    texture,
                    layer_index: entry.layer_index,
                    blend_mode: layer.map_or(BlendMode::Normal, |l| l.default_blend_mode),
                    opacity: layer.map_or(1.0, |l| l.opacity),
                    effects: &[],
                    effect_groups: &[],
                });
            }
        }

        // Sort clips descending by layer_index: higher index = bottom of timeline = rendered first
        // as base layer. This ordering is required by generate_layers' consecutive-run grouping.
        clip_descs.sort_unstable_by(|a, b| b.layer_index.cmp(&a.layer_index));

        // §8 D1/D5: each layer's effect chain gets its generator's effective
        // trigger_count (clip edge + audio fires) fed via `PresetContext` —
        // the same value the layer's own generator graph sees. `None` (no
        // GeneratorRenderer registered) reads as 0 for every layer, same as
        // a layer with no live generator.
        let gen_renderer_for_frame = renderers
            .iter()
            .find_map(|r| r.as_any().downcast_ref::<GeneratorRenderer>());

        let layer_descs: Vec<CompositeLayerDescriptor> = layers
            .iter()
            // Audio layers produce no visual output and must not enter the
            // compositor (their solo/mute is an audible bus). §5 of the design.
            .filter(|layer| !layer.is_audio())
            .map(|layer| CompositeLayerDescriptor {
                layer_index: layer.index,
                layer_id: &layer.layer_id,
                blend_mode: layer.default_blend_mode,
                opacity: layer.opacity,
                is_muted: layer.is_muted,
                is_solo: layer.is_solo,
                blit_to_led: layer.blit_to_led,
                effects: layer.effects.as_deref().unwrap_or(empty_effects),
                effect_groups: layer.effect_groups.as_deref().unwrap_or(empty_groups),
                parent_layer_id: layer.parent_layer_id.as_ref(),
                is_group: layer.is_group(),
                trigger_count: gen_renderer_for_frame
                    .map_or(0, |gr| gr.effective_trigger_count_for_layer(&layer.layer_id)),
            })
            .collect();

        let master_effects = project.map_or(empty_effects, |p| &p.settings.master_effects);
        let master_effect_groups = project
            .and_then(|p| p.settings.master_effect_groups.as_deref())
            .unwrap_or(empty_groups);
        let led_exit_index = project.map_or(-1, |p| p.settings.led_exit_index);

        let frame = CompositorFrame {
            time: time_f64,
            beat: beat_f64,
            dt: dt as f32,
            frame_count,
            compositor_dirty: tick_result.compositor_dirty,
            clips: &clip_descs,
            layers: &layer_descs,
            master_effects,
            master_effect_groups,
            // §8 D5: master has no layer, so its effective count is
            // audio-fires-only, accumulated across the session in
            // `self.master_trigger_count` (bumped in `apply_trigger_pulses`,
            // called earlier this frame before generators render).
            master_trigger_count: self.master_trigger_count,
            led_exit_index,
            led_composite_size: self.led_grid_size,
            tonemap: TonemapSettings {
                exposure: 1.0,
                hdr_output_enabled: self.edr_headroom > 1.0,
                paper_white_nits: 200.0,
                max_display_nits: (200.0 * self.edr_headroom as f32).min(10000.0),
                curve: project.map_or(manifold_core::TonemapCurve::AcesNarkowicz, |p| {
                    p.settings.tonemap_curve
                }),
            },
            output_width: self.output_w,
            output_height: self.output_h,
            occluded_layers: &self.occluded_layers_scratch,
            render_skip: &self.render_skip_scratch,
        };
        let _desc_ms = _t0.elapsed().as_secs_f64() * 1000.0;
        rtrace.mark("descriptors");

        // ── Compositor (same native encoder) ─────────────────────────
        let _t0 = std::time::Instant::now();
        {
            let mut gpu_comp = if let Some(pool) = texture_pool {
                GpuEncoder::with_pool(&mut native_enc, native_device, pool)
            } else {
                GpuEncoder::new(&mut native_enc, native_device)
            };

            // Forward the authoring-time node-output preview request so the
            // chain holding the watched effect preserves the selected node's
            // output this frame. Cheap clone; `None` clears (no preview).
            self.compositor
                .set_preview_request(self.node_preview_request.clone());
            // Enable a dump on the watched effect's chain this frame. The Cmd+D
            // one-shot dumps the whole graph; the thumbnail atlas dumps only the
            // canvas's visible nodes. Cmd+D takes precedence when both are
            // pending (both read the same captured per-node textures).
            let dump_request = if let Some((eid, _)) = pending_dump.as_ref() {
                Some(manifold_renderer::compositor::DumpRequest::All(eid.clone()))
            } else if !self.node_atlas_visible.is_empty() {
                self.node_preview_request.as_ref().map(|(e, _)| {
                    manifold_renderer::compositor::DumpRequest::Visible(
                        e.clone(),
                        self.node_atlas_visible.clone(),
                    )
                })
            } else {
                None
            };
            self.compositor.set_dump_request(dump_request);

            let _compositor_tex = self.compositor.render(&mut gpu_comp, &frame);
        }

        rtrace.mark("compositor_encode");

        // Promote a completed clip-atlas layout (BUG-119 item 3: layout never
        // leads pixels). `clip_atlas_pending_layout` may hold a layout from THIS
        // frame (still UNSTAMPED — stamped below, after this frame's own commit)
        // or an earlier frame's (already stamped with a real GPU signal value);
        // only the latter can ever satisfy `is_done`, so promotion never races
        // ahead of the blit it describes. Runs every tick, independent of
        // export/visibility — bookkeeping only, no GPU work.
        #[cfg(target_os = "macos")]
        if let Some((_, sig)) = self.clip_atlas_pending_layout.as_ref()
            && *sig != CLIP_ATLAS_LAYOUT_UNSTAMPED
            && self.native_event.as_ref().is_some_and(|e| e.is_done(*sig))
        {
            let (layout, _) = self.clip_atlas_pending_layout.take().unwrap();
            self.last_clip_atlas_layout = layout;
        }

        // ── Clip thumbnail atlas snapshot (§24 5c) ──────────────────
        // AFTER the compositor render so we can prefer each clip's POST-EFFECT
        // output (with-effects thumbnail). `frame` is no longer borrowing `self`
        // here, and `native_enc` is free again (its compositor wrapper dropped), so
        // the snapshot's disjoint field borrows coexist with the `native_device`
        // (= `&self.native_device`) binding. Single-clip-layer clips use the
        // compositor's post-fx output; everything else uses the raw clip texture
        // (`clip_descs`). Skipped in export / when no timeline thumbnails are shown.
        #[cfg(target_os = "macos")]
        if !export_mode && !self.clip_atlas_visible.is_empty() {
            // Pressure gate (BUG-119 item 4): non-zero means the content thread
            // blocked on the GPU fence last frame — the show is behind. Thumbnails
            // starve first: no cold-start renders, no filmstrip decode driving, no
            // capture/restore blits, no new save-readback submits. An
            // already-in-flight readback may still be drained below (a CPU memcpy,
            // no new GPU work).
            let pressured = self.last_fence_wait_ms > 0.1;

            // Filmstrip geometry inputs, shared by the parked-video driver and the
            // capture pass: each visible clip's (start_beat, duration_beats,
            // in_point seconds), plus bar length and seconds/beat. Bookkeeping-only
            // (no GPU work) — runs regardless of pressure.
            let beats_per_bar = project
                .map(|p| p.settings.time_signature_numerator as f64)
                .unwrap_or(4.0)
                .max(1.0);
            let secs_per_beat = project
                .map(|p| p.settings.seconds_per_beat() as f64)
                .unwrap_or(0.5)
                .max(1e-4);
            let visible_set: ahash::AHashSet<&str> =
                self.clip_atlas_visible.iter().map(|c| c.as_str()).collect();
            let mut clip_meta: AHashMap<&str, (f64, f64, f64)> =
                AHashMap::with_capacity(visible_set.len());
            // Per-clip content hash for the disk cache (key for save/restore).
            let mut clip_hashes: AHashMap<String, u64> =
                AHashMap::with_capacity(visible_set.len());
            for layer in layers {
                for clip in &layer.clips {
                    let id = clip.id.as_str();
                    if visible_set.contains(id) {
                        clip_meta.insert(
                            id,
                            (
                                clip.start_beat.as_f32() as f64,
                                clip.duration_beats.as_f32() as f64,
                                clip.in_point.as_f32() as f64,
                            ),
                        );
                        if self.clip_thumb_cache.is_some() {
                            clip_hashes.insert(
                                id.to_string(),
                                crate::clip_thumb_cache::clip_content_hash(clip, layer),
                            );
                        }
                    }
                }
            }

            // Cold-start (§24 5c P2c): render up to K PARKED generator clips'
            // thumbnails — visible clips with no live source this frame and no atlas
            // cell yet. Their default look (base params, no modulation/override/warm-
            // up) fills the gap until the clip first plays (then the live snapshot
            // replaces it). Bounded — instance creation is the cost; fills gradually.
            // Skipped entirely under pressure (BUG-119 item 4).
            if !pressured {
                const MAX_COLD_START_PER_FRAME: usize = 1;
                let mut budget = MAX_COLD_START_PER_FRAME;
                // Generators render through the renderer's GpuEncoder wrapper; wrap
                // native_enc for the thumbnail dispatches (dropped before the fill
                // reads the thumb textures on native_enc).
                let mut gpu_cold = if let Some(pool) = texture_pool {
                    GpuEncoder::with_pool(&mut native_enc, native_device, pool)
                } else {
                    GpuEncoder::new(&mut native_enc, native_device)
                };
                if let Some(gen_r) = renderers
                    .iter_mut()
                    .find_map(|r| r.as_any_mut().downcast_mut::<GeneratorRenderer>())
                {
                    for cid in &self.clip_atlas_visible {
                        if budget == 0 {
                            break;
                        }
                        let cid_str = cid.as_str();
                        // Skip clips that already have a cell, or are live (an
                        // active generator clip renders its own texture this frame —
                        // checked via gen_r, not clip_descs, which borrows renderers).
                        if self.clip_atlas_cache.contains_any(cid)
                            || gen_r.get_clip_texture(cid_str).is_some()
                        {
                            continue;
                        }
                        if let Some((layer, clip_index, time, beat)) =
                            find_parked_generator_clip(layers, cid_str)
                            && gen_r
                                .render_clip_thumbnail(
                                    &mut gpu_cold,
                                    cid_str,
                                    layer,
                                    clip_index,
                                    time,
                                    beat,
                                )
                                .is_some()
                        {
                            budget -= 1;
                        }
                    }
                    // Drop thumbnail instances for clips no longer visible.
                    gen_r.evict_thumb_gens(&self.clip_atlas_visible);
                }

                // P2b/5c-2: drive parked VIDEO clips' FILMSTRIP decode. Each clip's
                // isolated decoder walks its bars: request once (with per-bar source
                // times), then seek to the first not-yet-captured bar. The capture
                // pass blits the settled frame into that bar's cell, after which the
                // next frame here advances to the following bar. Bounded per frame so
                // the decode scheduler is never flooded. Never composited.
                if let Some(vid_r) = renderers
                    .iter_mut()
                    .find_map(|r| r.as_any_mut().downcast_mut::<VideoRenderer>())
                {
                    // A few more than the generator budget: a seek is far cheaper
                    // than a generator warm-up, and clips fill one bar per round-trip.
                    let mut vbudget = 3usize;
                    for cid in &self.clip_atlas_visible {
                        if vbudget == 0 {
                            break;
                        }
                        let cid_str = cid.as_str();
                        // Active (live) clips capture their playhead bar directly.
                        if vid_r.get_clip_texture(cid_str).is_some() {
                            continue;
                        }
                        let Some(&(start, dur, in_point)) = clip_meta.get(cid_str) else {
                            continue;
                        };
                        // First request: compute per-bar source times and open.
                        if !vid_r.has_poster(cid_str) {
                            let Some(video_clip_id) = find_parked_video_clip(layers, cid_str) else {
                                continue;
                            };
                            let count = crate::clip_filmstrip::cell_count(
                                crate::clip_filmstrip::clip_bar_count(dur, beats_per_bar),
                            );
                            let bar_times: Vec<f32> = (0..count)
                                .map(|c| {
                                    let (sb, _) = crate::clip_filmstrip::cell_beat_range(
                                        c,
                                        start,
                                        dur,
                                        beats_per_bar,
                                    );
                                    (in_point + (sb - start) * secs_per_beat).max(0.0) as f32
                                })
                                .collect();
                            if vid_r.request_clip_filmstrip(cid_str, video_clip_id, &bar_times) {
                                vbudget -= 1;
                            }
                            continue;
                        }
                        // Seek to the first bar without a captured cell yet.
                        let count = crate::clip_filmstrip::cell_count(
                            crate::clip_filmstrip::clip_bar_count(dur, beats_per_bar),
                        );
                        let Some(next) =
                            (0..count).find(|&b| self.clip_atlas_cache.cell_for(cid, b).is_none())
                        else {
                            continue; // every bar captured
                        };
                        // Already showing `next` (ready to capture) → leave it.
                        if vid_r.poster_target_bar(cid_str) == Some(next) {
                            continue;
                        }
                        if vid_r.poster_can_advance(cid_str) {
                            vid_r.advance_poster_to_bar(cid_str, next);
                            vbudget -= 1;
                        }
                    }
                    vid_r.evict_posters(&self.clip_atlas_visible);
                }
            }

            // Capture pass (sources, cell overrides, restore, fill) — the actual
            // thumbnail GPU work. Skipped entirely under pressure (BUG-119 item 4);
            // `published`/`restored` both default false, which also means the save
            // debounce below never arms on a pressured frame.
            let (published, restored) = if pressured {
                (false, false)
            } else {
                // Source per visible clip (built by iterating the visible set, NOT
                // clip_descs — clip_descs holds immutable borrows of `renderers` that
                // would block the cold-start's `&mut renderers`). Preference:
                //   1. compositor post-effect output (with-effects, single-clip layer),
                //   2. the live raw clip texture (active generator / video / image),
                //   3. the cold-start thumbnail (parked generator).
                let mut sources: AHashMap<&str, &manifold_gpu::GpuTexture> = AHashMap::new();
                for cid in &self.clip_atlas_visible {
                    let cid_str = cid.as_str();
                    if let Some(t) = self.compositor.clip_post_fx_texture(cid_str) {
                        sources.insert(cid_str, t);
                        continue;
                    }
                    let live = renderers.iter().find_map(|r| {
                        if let Some(g) = r.as_any().downcast_ref::<GeneratorRenderer>() {
                            if let Some(t) = g.get_clip_texture(cid_str) {
                                return Some(t);
                            }
                            if let Some(t) = g.thumb_texture(cid_str) {
                                return Some(t);
                            }
                        }
                        #[cfg(target_os = "macos")]
                        if let Some(v) = r.as_any().downcast_ref::<VideoRenderer>() {
                            if let Some(t) = v.get_clip_texture(cid_str) {
                                return Some(t);
                            }
                            // Parked filmstrip poster: only when a bar frame is settled
                            // (a seek has landed), so a mid-seek/stale frame is never
                            // captured into the wrong cell.
                            if v.poster_target_bar(cid_str).is_some()
                                && let Some(t) = v.poster_texture(cid_str)
                            {
                                return Some(t);
                            }
                        }
                        #[cfg(target_os = "macos")]
                        if let Some(i) = r
                            .as_any()
                            .downcast_ref::<manifold_media::image_renderer::ImageRenderer>()
                            && let Some(t) = i.get_clip_texture(cid_str)
                        {
                            return Some(t);
                        }
                        None
                    });
                    if let Some(t) = live {
                        sources.insert(cid_str, t);
                    }
                }
                // Per parked-video clip: the bar cell its *settled* poster frame
                // represents, so the capture writes it into the right cell rather than
                // the playhead-derived one. Active clips and generators have no override.
                let mut cell_override: AHashMap<&str, u32> = AHashMap::new();
                if let Some(vid_r) = renderers
                    .iter()
                    .find_map(|r| r.as_any().downcast_ref::<VideoRenderer>())
                {
                    for cid in &self.clip_atlas_visible {
                        if let Some(bar) = vid_r.poster_target_bar(cid.as_str()) {
                            cell_override.insert(cid.as_str(), bar);
                        }
                    }
                }
                // Restore cached cells (P4) BEFORE the capture, so a restored bar isn't
                // needlessly re-captured. One cell/frame, off the disk (worker thread).
                let restored = if let Some(cache_disk) = self.clip_thumb_cache.as_mut() {
                    restore_clip_atlas(
                        native_device,
                        &mut native_enc,
                        self.clip_atlas_persistent.as_ref(),
                        self.preview_pipeline.as_ref(),
                        self.preview_sampler.as_ref(),
                        &self.clip_atlas_visible,
                        &clip_hashes,
                        &mut self.clip_atlas_cache,
                        cache_disk,
                        &mut self.clip_atlas_pending_loads,
                        &mut self.clip_atlas_restore_staging,
                        &mut self.clip_atlas_pending_layout,
                    )
                } else {
                    false
                };

                let published = fill_clip_atlas(
                    &mut native_enc,
                    &sources,
                    &clip_meta,
                    &cell_override,
                    beat_f64,
                    beats_per_bar,
                    self.clip_atlas_persistent.as_ref(),
                    self.preview_pipeline.as_ref(),
                    self.clip_downsample_pipeline.as_ref(),
                    self.preview_sampler.as_ref(),
                    &self.clip_atlas_visible,
                    &mut self.clip_atlas_cache,
                    &mut self.clip_atlas_last_snapshot,
                    &mut self.clip_atlas_frame,
                    &mut self.clip_atlas_pending_layout,
                );
                (published, restored)
            };

            // Debounced disk SAVE (P4): poll a completed atlas readback → slice +
            // hand to the worker; otherwise schedule + submit a new readback once
            // captures have settled. All disk IO is off-thread; the readback is the
            // existing async pattern (submit one tick, read the next).
            if self.clip_thumb_cache.is_some() {
                const CLIP_ATLAS_SAVE_DEBOUNCE: u64 = 300; // ~5 s at 60 fps
                if self.clip_atlas_readback.is_pending() {
                    // try_read_packed() is a plain memcpy off the shared buffer
                    // (no per-pixel work) — the f16→u8 convert + per-cell slice
                    // that try_read() used to do inline here now runs on the
                    // clip-thumb disk worker thread via store_atlas(), off the
                    // content thread (BUG-035: that scalar conversion over the
                    // full 8192×1152 Rgba16Float atlas cost ~58ms/cycle here).
                    // Always allowed, even under pressure — draining an
                    // already-submitted readback does no new GPU work.
                    if let Some(bytes) = self.clip_atlas_readback.try_read_packed()
                        && let Some((layout_snap, hashes_snap)) =
                            self.clip_atlas_persist_pending.take()
                        && let Some(cache_disk) = self.clip_thumb_cache.as_ref()
                    {
                        cache_disk.store_atlas(
                            bytes,
                            CLIP_ATLAS_W,
                            layout_snap,
                            hashes_snap,
                            CLIP_ATLAS_COLS,
                        );
                    }
                } else if !pressured {
                    // Scheduling a new debounce and submitting a new 75MB readback
                    // are both "thumbnail GPU work" the pressure gate excludes.
                    if (published || restored) && self.clip_atlas_persist_due == 0 {
                        self.clip_atlas_persist_due =
                            self.clip_atlas_frame + CLIP_ATLAS_SAVE_DEBOUNCE;
                    }
                    if self.clip_atlas_persist_due != 0
                        && self.clip_atlas_frame >= self.clip_atlas_persist_due
                        && self.clip_atlas_persistent.is_some()
                        && !self.last_clip_atlas_layout.is_empty()
                    {
                        self.clip_atlas_persist_due = 0;
                        let mut gpu_rb = GpuEncoder::new(&mut native_enc, native_device);
                        if let Some(pt) = self.clip_atlas_persistent.as_ref() {
                            self.clip_atlas_readback.submit(
                                &mut gpu_rb,
                                pt,
                                CLIP_ATLAS_W,
                                CLIP_ATLAS_H,
                            );
                            self.clip_atlas_persist_pending =
                                Some((self.last_clip_atlas_layout.clone(), clip_hashes.clone()));
                        }
                    }
                }
            }
        }

        rtrace.mark("clip_atlas");

        // Upscale (render-res → output-res), direct present, and workspace preview.
        // MetalFX preferred; FSR 1.0 as fallback; direct blit when scale = 1.0.
        // Skipped in export mode (export reads output_texture directly).
        if !export_mode {
            // Resolve the final output texture (post-upscale or raw compositor).
            let final_output: &manifold_gpu::GpuTexture;
            if let Some(ref mfx) = self.metalfx {
                {
                    let mut gpu_upscale = if let Some(pool) = texture_pool {
                        GpuEncoder::with_pool(&mut native_enc, native_device, pool)
                    } else {
                        GpuEncoder::new(&mut native_enc, native_device)
                    };
                    mfx.upscale(&mut gpu_upscale, self.compositor.output_texture(), 0.35);
                }
                final_output = &mfx.output.texture;
            } else if let Some(ref fsr) = self.fsr1 {
                {
                    let mut gpu_fsr = if let Some(pool) = texture_pool {
                        GpuEncoder::with_pool(&mut native_enc, native_device, pool)
                    } else {
                        GpuEncoder::new(&mut native_enc, native_device)
                    };
                    fsr.upscale(&mut gpu_fsr, self.compositor.output_texture(), 0.35);
                }
                final_output = &fsr.output.texture;
            } else {
                final_output = self.compositor.output_texture();
            }

            // ── Direct present to output drawable ───────────────────
            // Acquire drawable, blit final output, schedule present — all in
            // the same command buffer. displaySyncEnabled on the CAMetalLayer
            // handles vsync-aligned delivery. No CVDisplayLink, no IOSurface.
            if let Some(ref surface) = self.output_surface
                && !self.output_present_suspended
                && let Some(ref pipeline) = self.output_pipeline
                && let Some(ref sampler) = self.output_sampler
                && let Some(drawable) = surface.next_drawable()
            {
                let target = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Rgba16Float);
                let draw_w = surface.width as f32;
                let draw_h = surface.height as f32;
                let source_aspect = self.output_w as f32 / self.output_h as f32;
                let draw_aspect = draw_w / draw_h;
                let (fit_w, fit_h) = if source_aspect > draw_aspect {
                    (draw_w, draw_w / source_aspect)
                } else {
                    (draw_h * source_aspect, draw_h)
                };
                let fit_x = (draw_w - fit_w) * 0.5;
                let fit_y = (draw_h - fit_h) * 0.5;
                native_enc.draw_fullscreen_viewport(
                    pipeline,
                    &target,
                    &[
                        manifold_gpu::GpuBinding::Texture {
                            binding: 0,
                            texture: final_output,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 1,
                            sampler,
                        },
                    ],
                    (fit_x, fit_y, fit_w, fit_h),
                    manifold_gpu::GpuLoadAction::Clear,
                    "Output Present",
                );
                native_enc.present_drawable(&drawable);
            }

            rtrace.mark("upscale_present");

            // ── Workspace preview (downscaled IOSurface) ────────────
            Self::update_workspace_preview(
                &mut native_enc,
                final_output,
                self.preview_textures[self.write_surface_index].as_ref(),
                self.preview_pipeline.as_ref(),
                self.preview_sampler.as_ref(),
            );

            // ── Node-output preview (downscaled IOSurface) ──────────
            // If a node is being previewed and its chain captured a texture
            // this frame, downscale it into the node-preview surface. Reuses
            // the workspace preview's downscale pipeline + sampler. When a
            // preview is requested but nothing was captured (the selected node
            // has no Texture2D output), clear the surface to black so the pane
            // reads as "no image output" rather than showing a stale frame.
            // Value-inspector info for a previewed effect node: its live scalar
            // I/O + whether it produced an image. Built whenever a node is
            // watched, image or not.
            if let Some((_, Some(node_id))) = &self.node_preview_request {
                let (inputs, outputs) = self.compositor.preview_scalar_io();
                self.last_node_preview_info = Some(crate::content_state::NodePreviewInfo {
                    node_id: node_id.clone(),
                    has_image: self.compositor.preview_texture().is_some(),
                    inputs,
                    outputs,
                });
            }
            // Live node-param values for the watched effect's canvas — collected
            // whenever an effect is watched, with or without a selected node, so
            // every on-face value reflects this frame's modulation. The
            // compositor resolves the watched effect from its own preview request.
            if self.node_preview_request.is_some() {
                self.last_live_node_params = self.compositor.live_node_params();
            }
            if let Some(node_tex) = self.compositor.preview_texture() {
                let encoding = self.compositor.preview_encoding();
                Self::update_node_preview(
                    &mut native_enc,
                    node_tex,
                    self.node_preview_textures[self.write_surface_index].as_ref(),
                    self.node_preview_normalize,
                    encoding,
                    &self.preview_pipelines(),
                    self.preview_sampler.as_ref(),
                );
            } else if self.node_preview_request.is_some()
                && let Some(target) = self.node_preview_textures[self.write_surface_index].as_ref()
            {
                native_enc.clear_texture(target, 0.0, 0.0, 0.0, 1.0);
            }

            // ── Per-node thumbnail atlas ────────────────────────────
            // While the editor is open, dump mode captured every watched-effect
            // node output this frame. Pack each into a cell of the atlas; the
            // canvas samples one cell per node. Authoring-only (gated on the UI
            // enabling it), so a live show pays nothing.
            // Gated on an effect being watched: a watched *generator* fills the
            // same atlas + layout in the generator block above, and the
            // compositor render runs later — so without this guard the empty
            // effect dump would clobber the generator's layout.
            #[cfg(target_os = "macos")]
            {
                let mut eff_layout: Option<Vec<(NodeId, u32)>> = None;
                if !self.node_atlas_visible.is_empty()
                    && self.node_preview_request.is_some()
                    && let (Some(atlas), Some(raw), Some(sampler)) = (
                        self.node_atlas_textures[self.write_surface_index].as_ref(),
                        self.preview_pipelines().raw,
                        self.preview_sampler.as_ref(),
                    )
                {
                    // Empty dump = the chain hasn't run with dump on yet (setup
                    // lag). Skip the clear+publish entirely so the UI keeps the
                    // last good atlas instead of flashing to a cleared surface.
                    let dump = self.compositor.dump_textures();
                    if !dump.is_empty() {
                        let mut new_layout: Vec<(NodeId, u32)> = Vec::new();
                        // Clear the whole atlas once, then Load-blit each cell so
                        // earlier cells survive.
                        native_enc.clear_texture(atlas, 0.0, 0.0, 0.0, 0.0);
                        for (i, (name, _port, _type_id, tex)) in
                            dump.iter().enumerate().take(ATLAS_CELLS)
                        {
                            // A render-target-only node output can't be sampled —
                            // binding it crashes AGX. Skip its cell.
                            if !thumb_source_shader_readable(name.as_str(), tex) {
                                continue;
                            }
                            native_enc.draw_fullscreen_viewport(
                                raw,
                                atlas,
                                &[
                                    manifold_gpu::GpuBinding::Texture {
                                        binding: 0,
                                        texture: tex,
                                    },
                                    manifold_gpu::GpuBinding::Sampler {
                                        binding: 1,
                                        sampler,
                                    },
                                ],
                                atlas_cell_viewport(i, tex.width, tex.height),
                                manifold_gpu::GpuLoadAction::Load,
                                "Node Thumbnail Atlas Cell",
                            );
                            new_layout.push((NodeId::new(name.as_str()), i as u32));
                        }
                        eff_layout = Some(new_layout);
                        atlas_filled_this_frame = true;
                    }
                }
                if let Some(l) = eff_layout {
                    self.last_node_atlas_layout = l;
                }
            }
        }

        // ── Live recording capture ──────────────────────────────────
        // Format-convert the upscaled output (Rgba16Float → sRGB Bgra8Unorm)
        // into a recording pool texture. Compute dispatch in the SAME command
        // buffer — the recording thread has zero GPU work.
        let recording_fence = if !export_mode {
            if let Some(ref mut session) = self.recording_session {
                if let Some((tex_idx, pool_slot, fence)) = session.acquire_texture() {
                    let src = if let Some(ref mfx) = self.metalfx {
                        &mfx.output.texture
                    } else if let Some(ref fsr) = self.fsr1 {
                        &fsr.output.texture
                    } else {
                        self.compositor.output_texture()
                    };
                    let dst = session.pool_texture(tex_idx);
                    // Compute dispatch: Rgba16Float → sRGB Bgra8Unorm.
                    // Uses the native GpuEncoder directly (same command buffer).
                    session.encode_format_conversion(&mut native_enc, src, dst);
                    session.submit_frame(pool_slot, fence.clone());
                    Some(fence)
                } else {
                    session.record_dropped_frame();
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Register GPU completion handler to publish the preview front buffer
        // the instant the GPU finishes — decoupled from the content thread's
        // sleep/wake cycle. Output presentation is handled by presentDrawable
        // on the same command buffer (no IOSurface needed).
        if !export_mode {
            let write_idx = self.write_surface_index as u32;
            let preview = self.preview_bridge.clone();
            // Publish the node preview when a preview (effect or generator) is
            // active this frame — otherwise leave the editor's front buffer.
            let node_preview =
                if self.node_preview_request.is_some() || self.node_preview_generator.is_some() {
                    self.node_preview_bridge.clone()
                } else {
                    None
                };
            // Publish the thumbnail atlas only on frames a capture block actually
            // wrote it — never on an enabled-but-empty frame, which would flip the
            // UI to a freshly-cleared (all-transparent) surface and strobe black.
            let node_atlas = if atlas_filled_this_frame {
                self.node_atlas_bridge.clone()
            } else {
                None
            };
            // No clip-atlas publish here (BUG-119): the shared surface IS what
            // the UI samples, so a completed cell blit is visible with no
            // front-buffer flip — see `set_clip_atlas_texture` and the
            // `clip_atlas_pending_layout` promotion in `render_content`.
            native_enc.add_completed_handler(move || {
                if let Some(ref b) = preview {
                    b.publish_front(write_idx);
                }
                if let Some(ref b) = node_preview {
                    b.publish_front(write_idx);
                }
                if let Some(ref b) = node_atlas {
                    b.publish_front(write_idx);
                }
                // Signal recording thread that the GPU blit is complete.
                if let Some(ref fence) = recording_fence {
                    fence.signal();
                }
            });
        }

        rtrace.mark("previews_recording");

        // Signal frame completion + commit
        let native_event = self.native_event.as_ref().unwrap();
        native_enc.signal_event(native_event);
        self.native_signal_value = native_event.current_value();
        // BUG-119 item 3: a clip-atlas layout built this frame (fill_clip_atlas /
        // restore_clip_atlas) was stashed UNSTAMPED, before this commit — and
        // therefore this frame's real signal value — existed. Stamp it now; the
        // promotion check at the top of the next tick's snapshot block only
        // promotes once `is_done(sig)`, i.e. once these blits actually landed.
        #[cfg(target_os = "macos")]
        if let Some((_, sig)) = self.clip_atlas_pending_layout.as_mut()
            && *sig == CLIP_ATLAS_LAYOUT_UNSTAMPED
        {
            *sig = self.native_signal_value;
        }
        native_enc.add_completed_handler_with_status("Compositor");
        if self.profiling_enabled {
            // D6: profiled commit waits synchronously (see the Generators CB
            // above) — never on the live path.
            let profile = native_enc.commit_and_wait_profiled(native_device);
            self.last_gpu_profiles.push(("Compositor", profile));
        } else {
            native_enc.commit();
        }
        native_device.capture_scope_end();
        let _comp_ms = _t0.elapsed().as_secs_f64() * 1000.0;
        rtrace.mark("commit");

        // One-shot graph dump readback. Runs AFTER the compositor CB commits so
        // the captured node textures hold this frame's writes; the readback
        // uses its own command buffers (one per texture) that the GPU runs
        // after the compositor's on the same queue.
        if let Some((_, dir)) = &pending_dump {
            let textures: Vec<crate::graph_dump::DumpTexture> = self
                .compositor
                .dump_textures()
                .into_iter()
                .map(|(name, port, type_id, texture)| crate::graph_dump::DumpTexture {
                    name,
                    port,
                    type_id,
                    texture,
                })
                .collect();
            if let Err(e) = crate::graph_dump::write_graph_dump(native_device, &textures, dir) {
                log::warn!("[graph-dump] write failed: {e}");
            }
            // Effect Array outputs (particle/instance buffers) → arrays.json.
            let arrays = self.compositor.dump_arrays();
            if let Err(e) = crate::graph_dump::write_array_dump(native_device, &arrays, dir) {
                log::warn!("[graph-dump] array write failed: {e}");
            }
        }

        // Preview surface tracking — skipped in export mode (no surface cycling).
        if !export_mode {
            self.surface_signal_values[self.write_surface_index] = self.native_signal_value;
            self.write_surface_index =
                (self.write_surface_index + 1) % crate::shared_texture::SURFACE_COUNT;
        }

        // Update shared output view for UI thread
        let (comp_w, comp_h) = self.compositor.dimensions();
        let _ = (comp_w, comp_h); // used in profiling block below; suppress lint in non-profiling builds

        // Periodic perf dump (profiling builds only)
        #[cfg(feature = "profiling")]
        {
            let _total_ms = _t_frame.elapsed().as_secs_f64() * 1000.0;
            if frame_count > 0 && frame_count.is_multiple_of(60) {
                log::warn!(
                    "[PERF/NATIVE] frame={} clips={} render={}x{} out={}x{} | gen={:.1}ms desc={:.1}ms \
                     comp={:.1}ms poll={:.1}ms | total={:.1}ms ({:.0}fps)",
                    frame_count,
                    tick_result.ready_clips.len(),
                    comp_w,
                    comp_h,
                    self.output_w,
                    self.output_h,
                    _gen_ms,
                    _desc_ms,
                    _comp_ms,
                    _poll_ms,
                    _total_ms,
                    1000.0 / _total_ms.max(0.001),
                );
            }
        }

        // Update shared dimensions (always output dims, not render dims).
        let (old_w, old_h) = self.shared_output.get_dimensions();
        if old_w != self.output_w || old_h != self.output_h {
            self.shared_output
                .set_dimensions(self.output_w, self.output_h);
        }

        // GPU profiler (if active): store poll timing
        #[cfg(feature = "profiling")]
        {
            self.gpu_poll_ms = _poll_ms;
        }

        rtrace.mark("tail");
        if rtrace.on {
            let total = _t_frame.elapsed().as_secs_f64() * 1000.0;
            if total > 20.0 {
                let breakdown: Vec<String> = rtrace
                    .marks
                    .iter()
                    .map(|(l, ms)| format!("{l}={ms:.1}"))
                    .collect();
                eprintln!(
                    "[RENDER_TRACE] frame={frame_count} total={total:.1}ms | {}",
                    breakdown.join(" ")
                );
            }
        }
    }

    /// Resize compositor, generators, and IOSurface bridge.
    ///
    /// `width` / `height` are the **output** dimensions (what the UI and IOSurface see).
    /// `render_scale` ∈ (0, 1] controls the internal render resolution:
    ///   - 1.0 → render at output resolution, upscaling disabled.
    ///   - 0.75 / 0.5 → render at 75% / 50%, MetalFX Spatial upscales back to output
    ///     (FSR 1.0 used as fallback if MetalFX is unavailable).
    pub fn resize(
        &mut self,
        engine: &mut PlaybackEngine,
        width: u32,
        height: u32,
        render_scale: f32,
    ) {
        let scale = render_scale.clamp(0.25, 1.0);
        let render_w = ((width as f32) * scale).round().max(1.0) as u32;
        let render_h = ((height as f32) * scale).round().max(1.0) as u32;

        self.output_w = width;
        self.output_h = height;

        // Reclaim old-resolution pool entries immediately on a canvas change.
        // Without this they can never be recycled (acquire keys on the new dims)
        // and only age out via the 300-frame prune_stale — dead 4K allocations
        // surviving up to ~10s. Keeps any entry already at the new render dims.
        if let Some(pool) = self.texture_pool.as_ref() {
            pool.evict_resolution_mismatch(render_w, render_h);
        }

        #[cfg(target_os = "macos")]
        let native_device = self
            .native_device
            .as_ref()
            .expect("native device required for resize");

        // Compositor renders at render resolution (may be smaller than output).
        #[cfg(target_os = "macos")]
        self.compositor.resize(native_device, render_w, render_h);

        // Resize clip renderers via engine downcast (at render resolution).
        // Generators re-allocate their GPU targets; the image renderer
        // re-decodes each still and re-fits it to the new canvas aspect so a
        // window/aspect change never stretches a static image.
        let (renderers, _) = engine.split_renderer_project();
        for renderer in renderers.iter_mut() {
            if let Some(gen_renderer) = renderer.as_any_mut().downcast_mut::<GeneratorRenderer>() {
                gen_renderer.resize_gpu(render_w, render_h, width, height);
                continue;
            }
            #[cfg(target_os = "macos")]
            if let Some(img_renderer) = renderer
                .as_any_mut()
                .downcast_mut::<manifold_media::image_renderer::ImageRenderer>()
            {
                use manifold_playback::renderer::ClipRenderer as _;
                img_renderer.resize(render_w as i32, render_h as i32);
            }
        }

        // Init / resize upscaler when render_scale < 1.0.
        // Prefer MetalFX Spatial (ML-based, faster, better quality on Apple Silicon).
        // Fall back to FSR 1.0 if MetalFX is unavailable (older hardware).
        #[cfg(target_os = "macos")]
        if scale < 1.0 {
            // Try MetalFX first.
            if manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler::is_available(
                native_device,
            ) {
                if let Some(ref mut mfx) = self.metalfx {
                    mfx.resize(native_device, render_w, render_h, width, height);
                } else {
                    self.metalfx =
                        manifold_renderer::metalfx_upscaler::MetalFxFullFrameUpscaler::new(
                            native_device,
                            render_w,
                            render_h,
                            width,
                            height,
                        );
                }
                self.fsr1 = None; // MetalFX takes over
                eprintln!(
                    "[Upscaler] MetalFX Spatial: {}x{} → {}x{} ({:.0}% render scale)",
                    render_w,
                    render_h,
                    width,
                    height,
                    scale * 100.0,
                );
            } else {
                // MetalFX not available — use FSR 1.0.
                self.metalfx = None;
                if let Some(ref mut fsr) = self.fsr1 {
                    fsr.resize(native_device, render_w, render_h, width, height);
                } else {
                    self.fsr1 = Some(manifold_renderer::fsr1::Fsr1Upscaler::new(
                        native_device,
                        render_w,
                        render_h,
                        width,
                        height,
                    ));
                }
                eprintln!(
                    "[Upscaler] FSR 1.0: {}x{} → {}x{} ({:.0}% render scale)",
                    render_w,
                    render_h,
                    width,
                    height,
                    scale * 100.0,
                );
            }
        } else {
            if self.metalfx.is_some() || self.fsr1.is_some() {
                eprintln!(
                    "[Upscaler] Disabled — rendering at native {}x{}",
                    width, height
                );
            }
            self.metalfx = None;
            self.fsr1 = None;
        }

        // Reset preview surface tracking after resolution change.
        #[cfg(target_os = "macos")]
        {
            self.write_surface_index = 0;
            self.surface_signal_values = [0; crate::shared_texture::SURFACE_COUNT];
        }

        // UI thread reads output dimensions.
        self.shared_output.set_dimensions(width, height);
    }

    #[cfg(target_os = "macos")]
    pub fn resize_workspace_preview(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);

        let Some(ref bridge) = self.preview_bridge else {
            return;
        };
        if bridge.width() == width && bridge.height() == height {
            return;
        }

        let native_device = self
            .native_device
            .as_ref()
            .expect("native device required for workspace preview resize");
        bridge.resize(width, height);
        self.preview_textures = std::array::from_fn(|i| {
            Some(unsafe { bridge.import_texture_native(native_device, i) })
        });
        self.preview_generation = bridge.generation();
    }

    /// Get current output dimensions (= IOSurface / UI dimensions).
    /// When FSR is active these differ from `self.compositor.dimensions()`.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.output_w, self.output_h)
    }

    #[cfg(target_os = "macos")]
    fn update_workspace_preview(
        native_enc: &mut manifold_gpu::GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: Option<&manifold_gpu::GpuTexture>,
        pipeline: Option<&manifold_gpu::GpuRenderPipeline>,
        sampler: Option<&manifold_gpu::GpuSampler>,
    ) {
        let Some(target) = target else {
            return;
        };

        if target.width == source.width && target.height == source.height {
            native_enc.copy_texture_to_texture(source, target, target.width, target.height, 1);
            return;
        }

        let (Some(pipeline), Some(sampler)) = (pipeline, sampler) else {
            return;
        };

        native_enc.draw_fullscreen(
            pipeline,
            target,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
            ],
            true,
            true,
            "Workspace Preview Blit",
        );
    }

    /// Downscale the previewed node's texture into the node-output preview
    /// surface. When `smart` is on (the default), the texture is rendered with
    /// the semantic `encoding` for that node — a vector field as an optical-flow
    /// colour wheel, a scalar field as a black-floor lift, a colour image raw —
    /// so dark/signed intermediates are legible without flicker. When off, it's
    /// the same raw blit the workspace preview uses. Only the node preview pane
    /// goes through here; the live render and workspace preview are untouched.
    #[cfg(target_os = "macos")]
    #[allow(clippy::too_many_arguments)]
    /// Borrow the semantic node-preview pipelines as one bundle, so the preview
    /// blit takes a single argument instead of one `Option<&pipeline>` per
    /// encoding. `None` on a platform / state where a pipeline wasn't built.
    #[cfg(target_os = "macos")]
    fn preview_pipelines(&self) -> PreviewPipelines<'_> {
        PreviewPipelines {
            scalar_lift: self.node_preview_scalar_pipeline.as_ref(),
            scalar_signed: self.node_preview_signed_pipeline.as_ref(),
            vector: self.node_preview_vector_pipeline.as_ref(),
            normal: self.node_preview_normal_pipeline.as_ref(),
            depth: self.node_preview_depth_pipeline.as_ref(),
            raw: self.preview_pipeline.as_ref(),
        }
    }

    fn update_node_preview(
        enc: &mut manifold_gpu::GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: Option<&manifold_gpu::GpuTexture>,
        smart: bool,
        encoding: manifold_renderer::node_graph::PreviewEncoding,
        pipelines: &PreviewPipelines<'_>,
        sampler: Option<&manifold_gpu::GpuSampler>,
    ) {
        use manifold_renderer::node_graph::PreviewEncoding;
        let Some(target) = target else {
            return;
        };
        // Pick the encoding pipeline. `Color` (and smart-off) fall through to
        // the raw blit; a missing pipeline also falls through.
        let pipeline = if smart {
            match encoding {
                PreviewEncoding::ScalarLift => pipelines.scalar_lift,
                PreviewEncoding::ScalarSigned => pipelines.scalar_signed,
                PreviewEncoding::VectorField => pipelines.vector,
                PreviewEncoding::Normal => pipelines.normal,
                PreviewEncoding::Depth => pipelines.depth,
                PreviewEncoding::Color => None,
            }
        } else {
            None
        };
        if let (Some(pipeline), Some(sampler)) = (pipeline, sampler) {
            enc.draw_fullscreen(
                pipeline,
                target,
                &[
                    manifold_gpu::GpuBinding::Texture {
                        binding: 0,
                        texture: source,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 1,
                        sampler,
                    },
                ],
                true,
                true,
                "Node Preview Encoding Blit",
            );
            return;
        }
        Self::update_workspace_preview(enc, source, Some(target), pipelines.raw, sampler);
    }

    /// Toggle auto-gain/normalization on the node-output preview. On by
    /// default; the editor's preview pane flips it. Node preview only.
    pub fn set_node_preview_normalize(&mut self, on: bool) {
        self.node_preview_normalize = on;
    }

    /// Live node-output preview info from the most recent render, for the
    /// editor's value inspector. `None` when no node is being previewed.
    pub fn node_preview_info(&self) -> Option<crate::content_state::NodePreviewInfo> {
        self.last_node_preview_info.clone()
    }

    /// Live (post-modulation) scalar param values for the watched effect/generator
    /// from the most recent render, keyed by stable `NodeId`. Empty when no editor
    /// is watching. The editor canvas overlays these onto its node faces so a
    /// driver / Ableton / envelope / card slider is seen moving the knob.
    pub fn live_node_params(&self) -> manifold_renderer::node_graph::LiveNodeParams {
        self.last_live_node_params.clone()
    }

    /// Clean up per-owner effect state for stopped clips.
    /// Called after render_content() to release GPU resources for clips
    /// that stopped this tick, preventing unbounded GPU memory growth.
    pub fn cleanup_stopped_clips(&mut self, stopped_clip_ids: &[manifold_core::ClipId]) {
        for clip_id in stopped_clip_ids {
            self.compositor.cleanup_clip_owner(clip_id.as_str());
        }
    }

    /// Clear all temporal effect state (feedback textures, bloom state, etc.).
    /// Called on project load to prevent stale GPU state from bleeding across projects.
    pub fn clear_all_effect_state(&mut self) {
        self.compositor.clear_all_effect_state();
    }

    /// Block until all in-flight background work in effect processors completes.
    /// Called after each export frame so async pipelines (GPU readback → CPU worker
    /// → result) resolve deterministically before the frame is encoded.
    /// Affected effects: BlobTracking, WireframeDepth, DepthOfField (depth mode).
    pub fn flush_all_background_work(&mut self) {
        self.compositor.flush_all_background_work();
    }

    /// Block until the last render's GPU command buffer has completed.
    /// Must be called before reading the output texture on a different queue.
    ///
    /// Uses the fence waiter's kernel notification when available (zero CPU),
    /// falling back to the polling path if the fence waiter isn't initialized.
    #[cfg(target_os = "macos")]
    pub fn wait_for_render_complete(&self) {
        if let Some(ref event) = self.native_event {
            if event.is_done(self.native_signal_value) {
                return;
            }
            // Use kernel notification via fence waiter (zero CPU, zero allocation).
            if let Some(ref waiter) = self.fence_waiter {
                let thread = std::thread::current();
                waiter.register(event, self.native_signal_value, move || {
                    thread.unpark();
                });
                std::thread::park_timeout(std::time::Duration::from_secs(5));
                return;
            }
            // Fallback: polling (should not be reached in normal operation).
            event.wait_until_done(self.native_signal_value);
        }
    }

    /// Export output texture (post-tonemap, post-effects).
    pub fn export_output_texture(&self) -> &manifold_gpu::GpuTexture {
        self.compositor.output_texture()
    }

    /// LED source texture. Returns `Some` only when at least one layer is flagged
    /// `blit_to_led` and has active clips this frame — the LED composite carries
    /// just those layers, post-tonemap + post-master-FX. Returns `None` when no
    /// layer is routed to LEDs; callers should blackout in that case.
    pub fn led_source_texture(&self) -> Option<&manifold_gpu::GpuTexture> {
        self.compositor.led_composite_texture()
    }

    /// Run the PQ encoder on the final compositor output for HDR export.
    /// Returns the PQ-encoded texture.
    /// Lazily creates the PQ encoder pipeline on first call.
    pub fn pq_encode_for_export(
        &mut self,
        paper_white_nits: f32,
        max_nits: f32,
    ) -> &manifold_gpu::GpuTexture {
        let native_device = self
            .native_device
            .as_ref()
            .expect("native device required for PQ encoding");
        let (w, h) = self.compositor.dimensions();

        // Lazy init PQ encoder
        if self.pq_encoder.is_none() {
            self.pq_encoder = Some(manifold_renderer::pq_encoder::PqEncoder::new(
                native_device,
                w,
                h,
            ));
            log::info!("[ContentPipeline] Created PQ encoder {}x{}", w, h);
        }
        let pq = self.pq_encoder.as_ref().unwrap();

        // Resize if needed
        if pq.output.width != w || pq.output.height != h {
            self.pq_encoder
                .as_mut()
                .unwrap()
                .resize(native_device, w, h);
        }

        // Encode: take the final compositor output (post-tonemap, post-effects)
        // and apply the ST.2084 PQ transfer function.
        let source = self.compositor.output_texture();
        let mut enc = native_device.create_encoder("PQ Encode");
        {
            let mut gpu_enc = GpuEncoder::new(&mut enc, native_device);
            self.pq_encoder.as_ref().unwrap().encode(
                &mut gpu_enc,
                source,
                paper_white_nits,
                max_nits,
            );
        }
        // Signal the same event so wait_for_render_complete covers PQ output.
        if let Some(ref event) = self.native_event {
            enc.signal_event(event);
            self.native_signal_value = event.current_value();
        }
        enc.commit();

        &self.pq_encoder.as_ref().unwrap().output.texture
    }

    /// Blit the final compositor output into the still-export readback buffer,
    /// returning the captured dimensions. Signals the render-complete event
    /// (like `pq_encode_for_export`) so `wait_for_render_complete` covers the
    /// copy. Must be called *after* `render_content` so the blit reads a fully
    /// written frame; pair with `take_still_readback` on the next tick.
    /// Mirrors the LED readback pattern (separate encoder, post-render texture).
    #[cfg(target_os = "macos")]
    pub fn submit_still_readback(&mut self) -> (u32, u32) {
        let native_device = self
            .native_device
            .as_ref()
            .expect("native device required for still readback");
        let (w, h) = self.compositor.dimensions();
        let source = self.compositor.output_texture();
        let mut enc = native_device.create_encoder("Still Readback");
        {
            let mut gpu_enc = GpuEncoder::new(&mut enc, native_device);
            self.still_readback.submit(&mut gpu_enc, source, w, h);
        }
        // Signal the same event so wait_for_render_complete covers the copy.
        if let Some(ref event) = self.native_event {
            enc.signal_event(event);
            self.native_signal_value = event.current_value();
        }
        enc.commit();
        (w, h)
    }

    /// If a still readback is in flight, wait for GPU completion and return the
    /// tightly-packed *linear* `Rgba16Float` pixels (8 bytes/px, stride =
    /// width × 8). Returns `None` when nothing is pending. Dimensions are
    /// whatever `submit_still_readback` captured — read them from its return
    /// value, not the live compositor. The caller applies the colour pipeline
    /// (rolloff + sRGB) in float off-thread; see
    /// `manifold_media::still_exporter::linear_f16_rgba_to_srgb8`.
    #[cfg(target_os = "macos")]
    pub fn take_still_readback(&mut self) -> Option<Vec<u8>> {
        if !self.still_readback.is_pending() {
            return None;
        }
        self.wait_for_render_complete();
        self.still_readback.try_read_packed()
    }

    /// Duration of the last GPU poll (wait for completion) in milliseconds.
    /// Only available with the `profiling` feature.
    #[cfg(feature = "profiling")]
    pub fn last_gpu_poll_ms(&self) -> f64 {
        self.gpu_poll_ms
    }

    /// Snapshot of the catalog-default graph for an effect type — the
    /// fallback path when an [`PresetInstance`] has no per-card
    /// override (`instance.graph` is `None`). Delegates to the
    /// compositor's `graph_snapshot_for`, which walks the live
    /// processors and returns the first matching one's
    /// `graph_snapshot()`. Per-card divergence is handled higher up in
    /// [`ContentThread::graph_snapshot`].
    pub fn graph_snapshot_for(
        &self,
        type_id: &manifold_core::PresetTypeId,
    ) -> Option<manifold_renderer::node_graph::GraphSnapshot> {
        self.compositor.graph_snapshot_for(type_id)
    }

    /// Outer-card → inner-node routings for the given effect type.
    /// Used to populate `GraphSnapshot::outer_routings` on the
    /// per-card (`from_def`) path, where the snapshot doesn't go
    /// through `graph_snapshot_for`.
    pub fn outer_routings_for(
        &self,
        type_id: &manifold_core::PresetTypeId,
    ) -> Vec<manifold_renderer::node_graph::OuterParamRouting> {
        self.compositor.outer_routings_for(type_id)
    }
}

#[cfg(test)]
mod occlusion_tests {
    use super::compute_occluded_layer_indices;
    use manifold_core::layer::Layer;
    use manifold_core::{BlendMode, LayerType};
    use manifold_playback::scheduler::ActiveClipRef;

    fn layer(index: i32, blend: BlendMode, opacity: f32) -> Layer {
        let mut l = Layer::new(format!("L{index}"), LayerType::Video, index);
        l.default_blend_mode = blend;
        l.opacity = opacity;
        l
    }

    fn clip(layer_index: i32) -> ActiveClipRef {
        ActiveClipRef {
            clip_id: format!("clip-{layer_index}").into(),
            layer_index,
            clip_index: 0,
            start_beat: manifold_core::Beats(0.0),
            duration_beats: manifold_core::Beats(4.0),
            is_looping: false,
            is_video: false,
        }
    }

    #[test]
    fn opaque_layer_occludes_everything_below() {
        let layers = vec![
            layer(0, BlendMode::Normal, 1.0),
            layer(1, BlendMode::Opaque, 1.0),
            layer(2, BlendMode::Additive, 1.0),
            layer(3, BlendMode::Normal, 1.0),
        ];
        let clips = vec![clip(0), clip(1), clip(2), clip(3)];
        let mut out = Vec::new();
        compute_occluded_layer_indices(&layers, &clips, &mut out);
        assert_eq!(out, vec![2, 3], "layers below the opaque cutoff are culled");
    }

    #[test]
    fn no_cull_when_opaque_is_faded_muted_or_clipless() {
        let mut out = Vec::new();
        // Partial opacity: a fade is in progress, everything below shows.
        let faded = vec![layer(0, BlendMode::Opaque, 0.99), layer(1, BlendMode::Normal, 1.0)];
        compute_occluded_layer_indices(&faded, &[clip(0), clip(1)], &mut out);
        assert!(out.is_empty(), "fading opaque layer must not occlude");
        // Muted opaque layer doesn't block.
        let mut muted = vec![layer(0, BlendMode::Opaque, 1.0), layer(1, BlendMode::Normal, 1.0)];
        muted[0].is_muted = true;
        compute_occluded_layer_indices(&muted, &[clip(0), clip(1)], &mut out);
        assert!(out.is_empty(), "muted opaque layer must not occlude");
        // Opaque layer with no ready clip this frame doesn't block.
        let idle = vec![layer(0, BlendMode::Opaque, 1.0), layer(1, BlendMode::Normal, 1.0)];
        compute_occluded_layer_indices(&idle, &[clip(1)], &mut out);
        assert!(out.is_empty(), "clipless opaque layer must not occlude");
    }

    #[test]
    fn solo_overrides_opaque_and_children_follow_their_group() {
        let mut out = Vec::new();
        // Solo elsewhere hides the opaque layer entirely.
        let mut soloed = vec![layer(0, BlendMode::Opaque, 1.0), layer(1, BlendMode::Normal, 1.0)];
        soloed[1].is_solo = true;
        compute_occluded_layer_indices(&soloed, &[clip(0), clip(1)], &mut out);
        assert!(out.is_empty(), "solo on another layer suppresses the occluder");
        // Parent-chain rule: a child whose group header sits ABOVE the
        // cutoff still renders into that group's composite; a plain layer
        // below the cutoff is culled.
        let mut group_header = layer(0, BlendMode::Normal, 1.0);
        group_header.layer_type = LayerType::Group;
        let occluder = layer(1, BlendMode::Opaque, 1.0);
        let mut child_of_top_group = layer(2, BlendMode::Normal, 1.0);
        child_of_top_group.parent_layer_id = Some(group_header.layer_id.clone());
        let plain_below = layer(3, BlendMode::Normal, 1.0);
        let layers = vec![group_header, occluder, child_of_top_group, plain_below];
        compute_occluded_layer_indices(
            &layers,
            &[clip(1), clip(2), clip(3)],
            &mut out,
        );
        assert!(
            !out.contains(&2),
            "child of a group above the cutoff keeps rendering"
        );
        assert!(out.contains(&3), "plain layer below the cutoff is culled");
    }
}

#[cfg(test)]
mod render_skip_tests {
    use super::{compute_occluded_layer_indices, compute_render_skip_indices};
    use manifold_core::layer::Layer;
    use manifold_core::{BlendMode, LayerType};
    use manifold_playback::scheduler::ActiveClipRef;

    fn layer(index: i32, blend: BlendMode, opacity: f32) -> Layer {
        let mut l = Layer::new(format!("L{index}"), LayerType::Video, index);
        l.default_blend_mode = blend;
        l.opacity = opacity;
        l
    }

    fn clip(layer_index: i32) -> ActiveClipRef {
        ActiveClipRef {
            clip_id: format!("clip-{layer_index}").into(),
            layer_index,
            clip_index: 0,
            start_beat: manifold_core::Beats(0.0),
            duration_beats: manifold_core::Beats(4.0),
            is_looping: false,
            is_video: false,
        }
    }

    /// The occluded set for the layer stack, for feeding render-skip.
    fn occluded(layers: &[Layer], clips: &[ActiveClipRef]) -> Vec<i32> {
        let mut o = Vec::new();
        compute_occluded_layer_indices(layers, clips, &mut o);
        o
    }

    #[test]
    fn plain_occluded_leaves_are_render_skipped() {
        let layers = vec![
            layer(0, BlendMode::Opaque, 1.0),
            layer(1, BlendMode::Normal, 1.0),
            layer(2, BlendMode::Additive, 1.0),
        ];
        let clips = vec![clip(0), clip(1), clip(2)];
        let occ = occluded(&layers, &clips);
        assert_eq!(occ, vec![1, 2], "both below the opaque cutoff are occluded");
        let mut skip = Vec::new();
        compute_render_skip_indices(&layers, &occ, true, false, &mut skip);
        assert_eq!(skip, vec![1, 2], "plain top-level leaves render-skip too");
    }

    #[test]
    fn led_tapped_occluded_layer_is_not_skipped() {
        let mut layers = vec![
            layer(0, BlendMode::Opaque, 1.0),
            layer(1, BlendMode::Normal, 1.0),
            layer(2, BlendMode::Normal, 1.0),
        ];
        layers[1].blit_to_led = true; // feeds the LED wall even while hidden
        let clips = vec![clip(0), clip(1), clip(2)];
        let occ = occluded(&layers, &clips);
        assert_eq!(occ, vec![1, 2], "occlusion still lists the LED layer");
        let mut skip = Vec::new();
        compute_render_skip_indices(&layers, &occ, true, false, &mut skip);
        assert_eq!(
            skip,
            vec![2],
            "the LED-tapped layer keeps rendering; only the plain leaf skips"
        );
    }

    #[test]
    fn grouped_child_below_cutoff_is_not_render_skipped() {
        // Occluder at top, then a group header below it with a child, plus a
        // plain leaf. The child is occluded but must not render-skip — only
        // top-level leaves do.
        let occluder = layer(0, BlendMode::Opaque, 1.0);
        let mut group_header = layer(1, BlendMode::Normal, 1.0);
        group_header.layer_type = LayerType::Group;
        let mut child = layer(2, BlendMode::Normal, 1.0);
        child.parent_layer_id = Some(group_header.layer_id.clone());
        let plain = layer(3, BlendMode::Normal, 1.0);
        let layers = vec![occluder, group_header, child, plain];
        let occ = occluded(&layers, &[clip(0), clip(2), clip(3)]);
        let mut skip = Vec::new();
        compute_render_skip_indices(&layers, &occ, true, false, &mut skip);
        assert!(!skip.contains(&1), "group header is never render-skipped");
        assert!(!skip.contains(&2), "grouped child is never render-skipped");
        assert!(skip.contains(&3), "plain top-level leaf still render-skips");
    }

    #[test]
    fn disabled_or_preview_active_skips_nothing() {
        let layers = vec![
            layer(0, BlendMode::Opaque, 1.0),
            layer(1, BlendMode::Normal, 1.0),
        ];
        let occ = occluded(&layers, &[clip(0), clip(1)]);
        assert_eq!(occ, vec![1]);
        let mut skip = Vec::new();
        compute_render_skip_indices(&layers, &occ, false, false, &mut skip);
        assert!(skip.is_empty(), "toggle off → render nothing skipped");
        compute_render_skip_indices(&layers, &occ, true, true, &mut skip);
        assert!(skip.is_empty(), "preview open → render nothing skipped");
    }
}
