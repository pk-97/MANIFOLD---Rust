# Clip Thumbnails (SOTA §24 5c) — Design

Generator/video clips on the timeline should show what they *look like*, not a flat
colour. This is the implementation contract for that.

## The core insight

A clip's true picture — generator + its effects, warmed-up and live-accurate —
**already exists on the content thread every frame the clip is under the playhead.**
`GeneratorRenderer::render_all` renders each active generator clip to a per-clip
texture (`get_clip_texture(clip_id)`); `VideoRenderer` decodes each active video
clip's current frame the same way; the compositor then applies clip/layer effects.

So the cheapest *and truest* thumbnail is a **snapshot of that live output**: when a
clip is active, downscale its post-effect texture into a thumbnail cell and cache it
by `ClipId`. No second render pipeline, no synthetic-time injection, no warm-up
problem (the live render is already warm), effects included for free.

The alternative — rendering a *parked* clip at an arbitrary time — is invasive:
`render_all` is driven by the live `active_clips` set, stateful generators (fluid
sims, feedback) need many warm-up frames to look right, and you'd pay that for every
off-screen clip. We do **not** do that as the foundation. It returns only as a
bounded **on-demand fill** (Phase 2) for clips the operator hasn't played yet.

## Transport — clone the node-thumbnail atlas

The graph editor already solves "content thread renders small previews → UI thread
draws them", cross-thread and cross-GPU-device, via a packed atlas shared over an
IOSurface bridge. We clone it for clips:

- **Atlas:** one `SharedTextureBridge` (IOSurface triple-buffer, `SURFACE_COUNT`
  slots) holding a `GRID×GRID` cell grid of 16:9 cells (mirrors `content_pipeline`'s
  `ATLAS_*`; start at 8×8 = 64 cells, 256×144 each). The UI imports the three
  IOSurfaces as device textures (`import_texture_native`).
- **Content → UI layout:** `clip_atlas_layout: Vec<(ClipId, u32 cell)>` in
  `ContentState`, mirroring `node_atlas_layout`. The UI looks a visible clip up in
  it and blits that cell.
- **UI → content visible set:** a `SetClipAtlasVisible(Vec<ClipId>)` command, deduped
  like `SetNodeAtlasVisible` (send only when the visible scope changes). Tells the
  content thread which clips currently want a thumbnail, so it only keeps/refreshes
  those cells.

The clip atlas is **independent** of the node atlas (different lifecycle: clip
thumbnails are always-on in the timeline; node thumbnails only while the editor is
open). Separate bridge, separate layout, same mechanics.

## Snapshot point (what we copy)

We want generator + clip effects + layer effects — the clip's real contribution.
- **Single-clip layer:** the compositor's per-layer output (`layer_outputs_scratch`)
  is exactly generator → clip fx → layer fx. Snapshot that.
- **Multi-clip layer:** the per-layer buffer composites *all* the layer's clips, so
  it can't isolate one. Fall back to the per-clip source texture
  (`get_clip_texture`) — generator (+ clip fx if applied upstream), no layer fx.
  Acceptable: a representative still, refined later if needed.

The copy is a downscale blit into the clip's atlas cell (letterboxed to the cell's
16:9, via `atlas_cell_viewport`). One blit per refresh.

## When we snapshot (and how often)

- **First sight:** when a visible clip is active and has no cached cell yet → snapshot.
- **Refresh:** re-snapshot a cached clip at most every N frames (e.g. ~once/sec) so a
  thumbnail of an animated generator stays roughly current without per-frame cost.
  Audio-reactive/animated content is inherently a *representative* still — we don't
  chase it every frame.
- **Bounded:** at most `K` snapshots per frame (a small queue). A downscale blit is
  cheap (≈ one quad), so `K` can be generous, but the cap guarantees the live frame
  budget is never threatened.

## Persistence + eviction (the cache)

The cache must outlive a clip going inactive (so a clip keeps its thumbnail when the
playhead moves on) but stay bounded.

- Cache keyed by `ClipId`. Cells are a fixed pool (`GRID²`).
- A clip in the **current visible set** holds its cell.
- Spare cells fill from a small **LRU** of recently-visible-but-now-offscreen clips,
  so scrolling back doesn't always re-snapshot.
- When visible thumbnail-needing clips exceed `GRID²`, the furthest-from-view clips
  fall back to plain body colour (graceful — no crash, no churn). The grid is sized
  so this is rare at normal zoom; `log()` if we ever truncate.

Keyed by `ClipId`, not cell index or position — same discipline as the waveform pool
and the effect-chain pools (a reorder/edit keeps the clip's thumbnail).

## UI — drawing the thumbnail

Thumbnails draw in the **same 4b′ slot** as audio waveforms (over the GPU body,
under the names), in `app_render.rs`. A clip is exactly one of:
- **audio** → waveform texture (`clip_content_gpu`, shipped in 5b),
- **generator/video** → thumbnail cell blit (this feature),
- **other / no thumbnail yet** → nothing (the gradient body shows).

The blit samples the clip's atlas cell (letterbox-corrected for project aspect, as
the node-thumbnail draw already does) into the clip's interior rect, scissor-clipped,
respecting the rounded corners the same way the waveform does. Generator vs video is
irrelevant to the UI — both are just a cell in the same atlas.

## Phases

- **P1 (foundation) — SHIPPED.** clip atlas + bridge; content-side snapshot of live
  generator **and** video clip output into cells; `SetClipAtlasVisible` + layout;
  UI blit. Truthful thumbnails for every clip the operator has played — a complete,
  shippable system for a rehearsed show. (Source: RAW clip output.)
- **P2a (with-effects) — SHIPPED.** The snapshot prefers each clip's POST-EFFECT
  output. `LayerCompositor` exposes `clip_post_fx_texture(clip_id)` — the per-layer
  output for a SINGLE-clip layer (that clip's full look: generator/video + layer
  effects). The snapshot pass moved to AFTER the compositor render so the post-fx
  textures exist; multi-clip layers fall back to the raw clip texture (a clip can't
  be isolated there). For a clip with no effects, raw == post-fx, so most clips are
  unchanged.
- **P2c (cold-start, GENERATORS) — SHIPPED.** A parked generator clip (no live
  source, no atlas cell) is rendered into an ISOLATED thumbnail instance:
  `GeneratorRenderer::render_clip_thumbnail` creates a `ThumbGen` (a separate
  `Box<PresetRuntime>` + a 256×144 `RenderTarget`, pooled by `ClipId`, NOT the live
  per-layer `layer_generators` — so a parked-clip thumbnail can never corrupt an
  active clip's state on the same layer). It renders the clip's authored/base params
  (`gp.param_values`) at `time=0`, `beat=clip.start_beat`, `anim_progress=0` — NO
  modulation / override-graph / warm-up (none of which are computed off the
  playhead), so the thumbnail is the generator's *default* look; the live snapshot
  replaces it the moment the clip plays. The content thread renders ≤1 such
  thumbnail/frame (instance creation is the cost; fills in gradually), evicts
  instances for non-visible clips (`evict_thumb_gens`), and the snapshot source
  becomes: compositor post-fx > live raw (`get_clip_texture`) > cold-start
  (`thumb_texture`). `find_parked_generator_clip` locates the clip; `clip_atlas`'s
  `contains` skips already-celled clips.
- **P2b (video posters) — SHIPPED.** A parked video clip shows a poster (its first
  decoded frame) via an ISOLATED async decode. `VideoRenderer::request_clip_poster`
  registers the clip in a SEPARATE `poster_clips` map (never composited, never
  advanced by `pre_render`'s playback loop), acquires a render target, and submits
  Open+Prepare decode jobs. The async results route via `clip_state_mut` (active
  first, then poster) in `process_decode_results`; once decoded, `poster_texture(id)`
  returns the frame. Snapshot source order: compositor post-fx > live
  `get_clip_texture` > generator cold-start `thumb_texture` > video `poster_texture`.
  - **Isolation is by a PREFIXED key (`\u{1}poster\u{1}<id>` — the critical fix from
    review).** A poster's decoder handle, `poster_clips` entry, and render target all
    live under the prefixed key, so they are FULLY independent of the same clip's
    active-playback decoder/entry. Even if a clip is parked-then-played, an in-flight
    poster decode result (keyed by the prefix) can NEVER land in the active clip's
    texture — without this, a poster frame could overwrite the live frame and a
    parked clip's data would reach the live output. No `start_clip` change needed.
  - **Lifecycle:** `evict_posters` drops a poster when its clip leaves the visible
    set OR becomes active (closes the isolated decoder + returns the RT). `resize`
    drops all posters (they re-decode at the new size). `release_all` tears posters
    down (Close + RT release) alongside active clips. The content thread requests ≤1
    new poster/frame (skip if the clip has a cell, a live frame, or `has_poster`).
  - Poster = the first frame (Prepare), not a seek to a representative time — a
    reasonable default; a representative-frame seek is future polish. A poster that
    permanently fails to decode shows body colour (no retry loop) and is reclaimed
    when its clip leaves view.

## Cost

- **Per snapshot:** a downscale blit (~one quad) — tens of µs. Refreshes are rate-
  limited (~1/sec/clip) and capped per frame, so the steady-state cost is negligible.
- **P2 standalone renders:** bounded queue; the expensive case (stateful generators ×
  warm-up frames) is rate-limited so it can never blow the frame budget — cold
  thumbnails fill in over a second or two.
- **Memory:** the atlas is one texture (8×8×256×144 ≈ 9 MB per IOSurface slot ×3).
  No per-clip textures — everything packs into the fixed atlas. Bounded by design.
- **Video posters:** seek+decode one frame (~5–50 ms, codec, off-thread via the
  existing scheduler), once per clip, then cached.

## GPU-init + cross-thread hardening (do not remove)

Two non-obvious correctness details, both verified by an adversarial review of the
content-thread path:

- **Clear the persistent atlas on create + force-propagate.** Metal does NOT
  zero-initialise textures. The first cell blit uses `Load`, and unwritten cells
  are sampled by the UI, so a fresh atlas must be `clear_texture`'d to transparent
  on creation. On creation we also set `propagate = SURFACE_COUNT` so the cleared
  atlas is copied to ALL rotating IOSurface slots over the next frames — the UI can
  never sample an uninitialised surface before the first real snapshot. (The
  node-thumbnail atlas clears the same way; the clip atlas must too.)
- **The layout↔surface skew is benign.** `clip_atlas_layout` is published to the UI
  in the `ContentState` snapshot, while the atlas surface is published in the GPU
  completion handler (`publish_front`) — so for one frame the UI may hold a new
  layout while `front_index()` still points at the previous surface. This is the
  same soft race the node atlas accepts, and it is harmless here because **cell
  assignments are stable** (keyed by `ClipId`, cells persist): the worst case is a
  one-frame-stale thumbnail in a cell that still belongs to the right clip — never
  garbage (the clear guarantees that) and never the wrong clip. We deliberately do
  NOT restructure to publish the layout atomically inside the bridge — it would
  diverge from the node-atlas pattern for no visible benefit.

## Status + honest gaps

**All phases shipped.** Every generator clip shows its content (live / with-effects /
cold-start default-look), and every video clip shows its content (live frame /
parked poster). Remaining items are *polish*, not gaps:

- A thumbnail is a **still**; animated/audio-reactive content is frozen at snapshot
  time. By design — a live mini-render per clip is not affordable at timeline scale.
- A **cold-start generator** thumbnail is the generator's DEFAULT look (base params,
  no modulation / override-graph / warm-up — none are computed off the playhead); the
  live snapshot replaces it the moment the clip plays. A **video poster** is the
  clip's FIRST frame, not a seek to a representative time. Both are reasonable
  defaults; representative-frame / modulated cold-start renders are future polish.
- Atlas cell count caps simultaneous thumbnails; past it, distant clips show body
  colour. Sized so it's rare at working zoom.
- The cross-thread visual (IOSurface handoff) is **not headless-verifiable** — it
  needs eyeballing on the running app. Everything else (UI blit, cache logic, the
  content-thread paths) is verified by headless PNG / unit tests / adversarial review
  / the workspace sweep.

---

# Filmstrip evolution (§24 5c-2) — design

The shipped system above puts **one** thumbnail cell per clip and stretches it across
the whole clip body, and (for playing generators) re-snapshots it on a ~1.5 s timer.
That has three problems we want to remove:

1. **It stretches.** A 32-bar clip body is enormous; one cropped cell across it shows
   a thin horizontal slice and wastes the width.
2. **The timed refresh is neither/nor.** Re-snapshotting a playing generator every 90
   frames is too slow to read as animation, too fast to read as a stable icon — it
   just *jumps*.
3. **The cold-start / first-frame source is the worst frame.** A generator's `t=0`
   default look (or a video's frame 0) is exactly the empty, pre-warm-up frame.

The evolution: **a temporal filmstrip per clip** — N cells across the clip's duration,
one per bar, tiled across the body. Resolume uses a single static thumbnail; Resolve
uses a filmstrip plus a lockable poster frame and hover-scrub. We take the Resolve
model because our clips have real width on a wide, beat-ruled timeline, and a filmstrip
reads the clip's *arc* at a glance. This section supersedes the single-cell display and
the timed-refresh capture; it keeps the shipped substrate (IOSurface bridge, persistent
atlas, `ClipAtlasCache`, the rounded-rect masked blit, the source-preference chain).

## The unifying insight — the playhead sweep *is* the recording

A generator is only truthfully renderable **while it plays** (warm, modulated, audio-
reactive — none of which is computed off the playhead; re-rendering a parked clip at a
synthetic time is the invasive path the shipped doc already rejected). But while it
plays, the playhead crosses *every bar* of the clip. So we don't render off-playhead at
all — we **record the live output into the bar-cell the playhead is currently in**. The
strip fills itself as you rehearse, each cell warm and real for that bar. The only cell
that updates live is the one under the playhead; the rest is static history. That reads
as "the playhead is painting the strip," which is intuitive, not jumpy.

Video is the deterministic case: there's nothing to keep warm, so video fills by
**background seek-decode** at each bar's timestamp (no play required). Same visual, same
draw path, same storage — only the *fill source* differs by clip kind. That symmetry is
the whole point: one model for both.

## Cell grid — bar-indexed, adaptive, bounded

- **Base resolution = one cell per bar.** Cell boundaries are real bar lines (via the
  tempo map / `time_sig_numerator` — `crates/manifold-core/src/settings.rs`), so the
  strip lines up with the timeline's existing beat/bar markers. "What this clip looks
  like each bar." Variable tempo / meter is handled because we index by the bar grid,
  not a fixed beat count.
- **Bounded per clip.** A clip stores at most `M_MAX` cells (start at 64). Past that,
  drop to one cell per 2 bars, then 4, … (power-of-two grouping) so a 200-bar clip
  never stores 200 cells. Cell count per clip is `min(bars, M_MAX)` at the chosen
  grouping.
- **Display adapts to width; storage does not.** Zoom only re-tiles — never re-renders
  or re-captures. If the body is wider than `bars × min_readable_px`, cells grow (mild
  intra-bar stretch is acceptable — it's still "this bar looked like this"). If
  narrower, sub-sample the stored cells. This decoupling is the property that makes
  zoom free.

## Capture

The capture pass already runs after the compositor render (`content_pipeline.rs`,
"Clip thumbnail atlas snapshot"), with the current playhead beat in scope
(`engine.current_beat_f64()`) and the source-preference chain built
(post-fx → live → poster/cold-start). What changes is *which cell* and *when*:

- **Generators (playing):** for each active visible clip, compute the current bar
  `b = floor((beat − clip.start_beat) / beats_per_bar)` at the clip's grouping; if `b`
  differs from the clip's last-captured bar, blit the live source into that bar's cell.
  At 120 BPM a 4/4 bar is ~120 frames, so each cell captures roughly once per bar
  crossing — a handful of blits per frame across all playing clips, trivially within
  the existing `MAX_SNAPSHOTS_PER_FRAME` cap. Seeking/looping fills cells out of order
  or overwrites — both correct, since each cell always holds the live frame *for its
  own bar*.
- **Video (parked or playing):** background seek-decode. For each bar without a cell,
  submit a `DecodeJob::Seek` to the bar's video-time on an **isolated decoder** (the
  prefixed-key poster isolation from P2b generalizes to a strip), grab the frame, blit
  it into the bar's cell. Bounded queue, prioritize visible clips, persist so it's
  one-time. Industry practice (and ours, eventually) is **keyframe-fast-seek** — seek
  to the nearest preceding keyframe without decoding forward to the exact frame: I-
  frames are self-contained, fast, and a thumbnail doesn't care about ±a few frames.
  Our seek is a native FFI (`VideoDecoder_SeekTo(handle, seconds)` in `decoder.rs`,
  which reports the landed PTS via `frame_time()`); a cheap keyframe-only mode is a
  **native-decoder addition**, not free today. Until it lands, treat video strip fill
  as "one seek+decode per cell, off-thread, bounded, persisted."
- **The "best frame" heuristic is gone.** With a strip, each cell is simply that bar —
  no peak-energy selection needed. Simpler than the shipped single-still.

## Storage & transport — confirmed low-risk

The audit settled the part I was least sure of: **atlas cells are interchangeable and
drawn one quad per cell** (`atlas_cell_full(cell)` computes a viewport from a flat cell
index; the UI blit in `app_render.rs` Pass 4b″ already draws an arbitrary cell per
clip). So a filmstrip needs **no rectangle packer** — a clip holds a *list* of cell
indices (one per bar), and we draw M quads at consecutive x-offsets across the body.
The cells need not be contiguous in the atlas.

Concrete changes, all bounded extensions of shipped code:
- **`ClipAtlasCache`: `ClipId → Vec<cell>`** instead of `ClipId → cell`. `get_or_alloc`
  takes a bar index; LRU evicts a *whole clip's* cell-list when off-screen and the pool
  is full. Layout published as `Vec<(ClipId, bar, cell)>`.
- **Bigger pool + smaller cells + 8-bit.** Recommended start: **128×72, RGBA8, 512
  cells** (≈19 MB/surface, ~57 MB across the triple buffer + persistent copy ≈ 75 MB).
  At ~30 visible clips × ~16 visible bars that's ~480 cells — fits; distant clips past
  the cap fall back to a single still or the type poster (graceful, as today). This
  needs a **format parameter on `SharedTextureBridge`** (currently hardcoded
  `Rgba16Float`/8 BPP) and on the persistent atlas — a contained change to one file.
  Thumbnails are SDR previews, so RGBA8 is correct and halves bandwidth.
- **Copy cost.** The shipped path copies the whole persistent atlas → rotating surface
  on any change (one `draw_fullscreen`) and propagates over `SURFACE_COUNT` frames. A
  bigger atlas makes that copy bigger but it's still one blit; **dirty-cell copy** (copy
  only changed cells) is an available optimization, not required for correctness. The
  filmstrip atlas is mostly static history (only the playhead cell churns), so the
  steady-state copy is cheap regardless.

## Persistence — sidecar cache (decided)

Filmstrips persist to a **sidecar cache, not the project file.** Rationale: regenerable,
keeps `.manifold` files small and portable (video strips are many frames), and a stale/
corrupt cache simply rebuilds. **Location:** the app cache dir keyed by project id (not
next to the user's project — no folder clutter). **Format:** a few packed atlas images
on disk + a small JSON index `clip_id → { hash, bar→frame }`. **Invalidation key:**
generator type + `param_values` for generators; file path + mtime + in/out points for
video. Edit a generator's params → hash changes → only that clip's strip regenerates;
everything else loads instantly on project open. **Eviction:** cap total disk size, LRU
by clip. Net effect: open a rehearsed project and the strips are simply *there*.

## Fill states (graceful, never blank, never lying)

- **Bar never recorded → generator-type poster tint, or flat layer colour.** Render
  each generator preset once, warmed, at default params → a per-*type* poster (≈20
  presets, one-time, cacheable/shippable; far cheaper than the per-*clip* cold-start the
  shipped doc does). A never-played generator clip shows its type's poster, not an empty
  `t=0` frame. Real captures overwrite poster cells bar-by-bar as you play.
- **Early generator bars are honestly empty** (the sim is still warming up) — and that's
  *useful*: you can see "nothing happens until bar 3." We do not fake-fill them.
- **Dark thumbnails keep identity.** A mostly-black generator (fluid sim) erases the
  layer colour-coding the operator reads the timeline by. Gate on luminance/variance, or
  tint toward the layer colour, so a dark strip never reads as a broken/empty clip.

## Performance budget & guarantees (non-negotiable)

- Hard cap captures/frame (reuse `MAX_SNAPSHOTS_PER_FRAME`); video decode off-thread on
  a bounded queue. The 60 fps content tick is never threatened (hot-path discipline).
- Capture scope = **visible + small margin**; clips playing far off-screen are not
  recorded (viewport-driven cache, as today). Trade-off noted: you won't capture a clip
  you play while scrolled away until you scroll near it.
- **Downsample properly.** Source is up to 3456×2234 → 128×72; a single bilinear tap
  aliases. Use a mip/box downsample.

## Honest asymmetries (so they don't surprise)

- **Video strip = raw decode (no effects); generator strip = with layer fx** (it's the
  live composited output). Fixable later by running decoded video frames through the
  layer chain in a one-off pass; not worth it initially.
- **Multi-clip layers** can't isolate one clip's post-fx (the compositor blends all) →
  raw fallback, same limit as the shipped P2a.

## Usability layer (shares the same storage)

The filmstrip cells *are* the hover-scrub frames, so these come almost for free once the
strip exists:
- **Hover-scrub** the strip → preview the bar under the cursor (Resolve/Premiere
  standard). Big win.
- **Click a cell → seek the playhead to that bar.**
Both are deferred polish, but the storage is designed so they need no new capture path —
flag them so we don't architect them out.

## Phased rollout

1. **Display:** draw M bar-cells tiled across the body (adaptive/bounded), replacing the
   single stretched cell. Cache + layout become list-per-clip. (Biggest visible change;
   uses existing capture to fill cell 0 first, then more.)
2. **Generator capture-on-play by bar** + drop the timed refresh. Add the luminance gate
   and the per-type poster fallback.
3. **Video background seek-decode per bar** (isolated decoder, bounded queue).
4. **Sidecar persistence** (load-on-open, hash-invalidate).
5. **Polish:** keyframe-fast-seek native mode; hover-scrub; click-to-seek; RGBA8 bridge
   format + cell-size/pool tuning; optional dirty-cell copy.

## What this supersedes

- Single stretched cell per clip → bar-indexed filmstrip.
- Timed `REFRESH_INTERVAL` re-snapshot of playing clips → capture-on-bar-crossing.
- Per-clip cold-start `render_clip_thumbnail` (parked generator default-look) → per-type
  poster fallback (cheaper) + real bars filled on play. (The isolated `ThumbGen`
  instance machinery can be retired once the poster path lands.)
- First-frame-only video poster → seek-per-bar strip (the poster becomes "cell 0").
