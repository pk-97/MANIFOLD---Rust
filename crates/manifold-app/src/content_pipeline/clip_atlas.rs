//! Clip-thumbnail atlas helpers: cell geometry, request validation, parked-clip
//! lookup, and the fill and restore passes that build the filmstrip atlas.

use super::*;

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

/// Validate UI thumbnail requests against the current project before allocating
/// cache entries or rendering previews. The UI may hold an older snapshot.
pub(super) fn retain_thumbnail_requests(
    visible: &mut Vec<ClipId>,
    project: Option<&manifold_core::project::Project>,
) {
    visible.retain(|id| {
        project.is_some_and(|p| {
            p.timeline.layers.iter().any(|layer| {
                layer.layer_type.supports_clip_thumbnails()
                    && layer.clips.iter().any(|clip| clip.id == *id)
            })
        })
    });
}

/// section 24 5c cold-start: locate a PARKED generator clip by id — its layer (must be a
/// generator), clip index, and clip-start `(time, beat)` for a thumbnail render.
/// `None` if not found or the layer isn't a generator (video posters are separate).
pub(super) fn find_parked_generator_clip<'a>(
    layers: &'a [manifold_core::layer::Layer],
    clip_id: &str,
) -> Option<(&'a manifold_core::layer::Layer, u32, f64, f64)> {
    for layer in layers {
        if !layer.hosts_generator() || layer.gen_params().is_none() {
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

/// section 24 5c P2b: locate a PARKED video clip by id — returns its `video_clip_id`
/// (the file reference), for a one-shot poster decode. `None` if not found or not
/// a video clip.
pub(super) fn find_parked_video_clip<'a>(
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
pub(super) fn thumb_source_shader_readable(label: &str, tex: &manifold_gpu::GpuTexture) -> bool {
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
/// surface (section 24 5c, BUG-119 root fix). `sources` maps each *active* clip
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
#[allow(clippy::too_many_arguments)]
pub(super) fn fill_clip_atlas(
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
        enc.draw(
            &manifold_gpu::DrawPass::color(persistent_tex, manifold_gpu::GpuLoadAction::Load)
                .viewport(clip_atlas_cell_full(*cell as usize)),
            &[manifold_gpu::Draw::fullscreen(
                cell_blit,
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
            )],
            "Clip Thumbnail Atlas Cell",
        );
    }
    true
}

/// Restore cached filmstrip cells into the shared clip atlas surface (section 24 5c-2
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
#[allow(clippy::too_many_arguments)]
pub(super) fn restore_clip_atlas(
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
        enc.draw(
            &manifold_gpu::DrawPass::color(pt, manifold_gpu::GpuLoadAction::Load)
                .viewport(clip_atlas_cell_full(atlas_cell as usize)),
            &[manifold_gpu::Draw::fullscreen(
                raw,
                &[
                    manifold_gpu::GpuBinding::Texture {
                        binding: 0,
                        texture: st,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 1,
                        sampler,
                    },
                ],
            )],
            "Clip Thumb Restore Cell",
        );
        *pending_layout_out = Some((cache_obj.layout(), CLIP_ATLAS_LAYOUT_UNSTAMPED));
        return true;
    }
    false
}
