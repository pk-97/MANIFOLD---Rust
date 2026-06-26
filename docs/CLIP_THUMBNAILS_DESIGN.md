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
