# Timeline API — Phase 3 Design

**Status:** SHIPPED 2026-06-22 — as-built record (parseable status line added 2026-07-16; the banner below was invisible to the design status board).

> **SHIPPED 2026-06-22.** All six tasks landed behaviour-preserving, one commit
> each; manifold-ui 333 + manifold-app 103 + manifold-editing 59 tests green,
> clippy clean. This doc is the as-built record (the order/decisions below match
> what shipped, including the `trim.rs`-not-reused note in §3.3).

Sub-design-doc for **Phase 3** of the UI Architecture Overhaul
(`docs/UI_ARCHITECTURE_OVERHAUL.md` §5.3, §13). Scopes the six Phase-3 tasks
against the codebase as it actually stands today. Written after a full read of
the timeline surface; every claim below is backed by a `file:line`.

The timeline is the load-bearing performance surface — a real show is ~2,900
clips and a timing bug becomes the show. Phase 3 is **behaviour-preserving
structural cleanup**, not a rewrite. Each task lands as its own commit, gated by
the existing Unity-ported tests plus new ones, so any regression is bisectable.

---

## 1. What the timeline is made of today

| Concern | Type | File | Notes |
|---|---|---|---|
| Beat↔pixel + Y-layout | `CoordinateMapper` | `coordinate_mapper.rs` | The one clean shared seam. Owns zoom, scroll-x, and the per-layer Y arrays. Well-tested. |
| Pure-math clip hit-test | `ClipHitTester` | `clip_hit_tester.rs` | Stateless. Reads `ViewportClip`s + mapper. Well-tested. |
| Clip render+hit model | `ViewportClip` | `panels/viewport.rs` | Single storage (`clips_by_layer`); both the bitmap painter and the hit-tester read it. Already one source. |
| Interaction → engine seam | `TimelineEditingHost` | `timeline_editing_host.rs` | Trait the app implements; keeps `manifold-ui` off `manifold-editing`. |
| Selection / cursor / zoom | `UIState` | `ui_state.rs` | Persistent UI state read by render. **Also** holds transient drag/trim/scrub fields. |
| Clip drag / trim / region | `InteractionOverlay` | `interaction_overlay.rs` | Owns `drag_mode`, snapshots, region machinery. **Also** reads+writes `UIState`'s drag/trim fields. |
| Generic drag lifecycle | `DragController<T>` | `drag.rs` | Phase-1 substrate. The overlay does **not** use it yet. |
| Pure trim math | `compute_left_trim` / `compute_right_trim` | `trim.rs` | Phase-1 substrate. The overlay **open-codes** the same math instead of calling these. |
| The panel | `TimelineViewportPanel` | `panels/viewport.rs` (2,959 lines) | Coordinate, model, render, ruler-scrub, markers, overview minimap, collapsed-group bitmaps, scroll — all in one struct. |

### The four problems Phase 3 removes (named precisely)

1. **Y-layout is computed three times.** The per-layer height rule
   (collapsed/group/generator → height) lives in `CoordinateMapper::
   rebuild_y_layout` (`coordinate_mapper.rs:153-179`), is copied verbatim into
   the app's `TrackInfo` build (`state_sync.rs:891-925`), and is copied a third
   time as cumulative offsets in the viewport (`viewport.rs:431-437`,
   `track_y_offsets` / `total_tracks_height`). `track_y` / `track_height` /
   `layer_at_y` read the viewport's copy, not the mapper.
   *(Note: `layer_header` is already correct — `state_sync.rs:805-806` feeds its
   `y_offset`/`height` straight from `viewport.mapper()`. Only these two copies
   remain.)*

2. **The "MUST match" comment.** `viewport.rs:1205` and `layer_header.rs:1841`
   both call `layout.track_header_height()` for the header spacer, with a comment
   ordering them to stay identical. Already a shared value; the comment is a
   landmine where a type should be.

3. **Drag/trim/scrub has two owners.** Transient gesture state is split:
   `UIState` holds `is_dragging`, `drag_clip_id`, `drag_offset_beats`,
   `drag_start_beat`, `is_trimming`, `trim_clip_id`, `trim_original_*`,
   `is_scrubbing` (`ui_state.rs:45-64`); `InteractionOverlay` holds `drag_mode`,
   `drag_anchor_clip_id`, `drag_snapshots`, and **a second** `trim_clip_id`
   (`interaction_overlay.rs:117-129`). Begin-drag writes both. Two owners of one
   state is a bug farm.

4. **Markers exist three times.** A user marker is simultaneously: UITree nodes
   (`MarkerNodeGroup` — flag/outline/label, `viewport.rs:2160-2261`), a parallel
   positional hit-test list (`marker_flag_rects: Vec<(MarkerId, Rect)>`,
   scanned by `hit_test_marker_flag`, `viewport.rs:747-754`), and bitmap line
   data (`marker_line_cache`). The flag rect and the flag node are computed from
   the *same* `beat_to_pixel(marker.beat)` — but stored twice, so they can drift.

---

## 2. Target shape

Three ideas, each with exactly one owner. No new shared state, no new
`Arc<Mutex>`; the content thread still owns the `Project`, the UI still gets
snapshots. The `TimelineEditingHost` seam stays — it is the correct boundary.

```
crates/manifold-ui/src/panels/viewport/      (3.6 — the god-panel, split)
├── mod.rs          TimelineViewportPanel: fields, new(), Panel impl, public API
├── model.rs        TimelineItems: lanes + clips + markers as addressable items
├── coordinate.rs   the viewport's coordinate surface over CoordinateMapper
├── render.rs       build_*, repaint_*, scroll update-in-place, overview, groups
└── interaction.rs  ruler-scrub + marker-drag event handling (viewport-local)
```

- **3.2 / 3.5 — one model.** `model.rs` holds the lanes, clips, and markers as
  addressable items. Markers stop being three things: the hit-test reads the
  same item list the paint reads, via one `marker_flag_rect(...)` helper. The
  `marker_flag_rects` parallel `Vec` is deleted.
- **3.3 — one interaction owner.** A `TimelineInteraction` component owns *all*
  transient gesture state. The drag/trim/scrub fields leave `UIState` (which
  keeps only the persistent selection/cursor/zoom it is read for). The overlay's
  open-coded trim math is replaced by the `trim.rs` pure functions it should
  have used.
- **3.4 — one coordinate authority.** `CoordinateMapper` is the sole Y-layout
  source. The height rule becomes one function; `TrackInfo.height` and the
  viewport's `track_y_offsets` are deleted; `track_y`/`track_height`/`layer_at_y`
  read the mapper.

---

## 3. Task-by-task

### 3.4 — CoordinateMapper as the sole coordinate authority *(do first)*

Foundational and lowest-risk; de-risks everything Y-shaped that follows.

- Extract the height rule into one method:
  `CoordinateMapper::layer_height(layers: &[Layer], index: usize) -> f32`.
  `rebuild_y_layout` calls it in its loop. The rule now lives in one place.
- Delete `TrackInfo.height` (`viewport.rs:86`). `rebuild_mapper_layout` already
  runs before `set_tracks` (`state_sync.rs:796` then `:959`), so `set_tracks`
  reads `mapper.get_layer_height(i)` to size the bitmap renderers. The app stops
  computing height entirely (`state_sync.rs:891-925` deleted).
- Delete `track_y_offsets` and `total_tracks_height` (`viewport.rs:178-179`).
  - `track_y(i)` → `mapper.get_layer_y_offset(i) + tracks_rect.y - scroll_y_px`
  - `track_height(i)` → `mapper.get_layer_height(i)`
  - `layer_at_y(y)` → delegate to `mapper.get_layer_at_y(y - tracks_rect.y + scroll_y_px)`, guarded by the `tracks_rect` bounds check it already does.
  - scroll clamp uses `mapper.total_content_height()`.
- Replace the two "MUST match" comments with a one-line pointer to
  `track_header_height()` as the single source — the divergence the comment
  guarded against is now unrepresentable (nothing recomputes it).

*Done when:* the height rule exists once; `cargo test -p manifold-ui` green
(mapper + hit-tester + viewport coordinate roundtrip tests unchanged).

### 3.5 + 3.2 — One lane/clip/marker model; markers first-class

- Add `model.rs` with the item types the timeline addresses: lanes (the
  `TrackInfo` data minus `height`), clips (`ViewportClip`, moved here), markers.
  Re-export from `panels::viewport` so `clip_hit_tester` / `interaction_overlay`
  imports are unchanged.
- Markers: keep `markers: Vec<TimelineMarker>` as the **one** source. Derive the
  flag rect with a single pure helper
  `fn marker_flag_rect(beat, ruler_rect, mapper, scroll) -> Rect`, used by
  **both** `build_markers` (to position the node) and the hit-test. Delete
  `marker_flag_rects`. `hit_test_marker(pos)` recomputes from `markers` — the
  same iteration `build_markers` does, so paint and hit cannot disagree.
- This is the "addressable items drive both paint and hit-test from one source"
  deliverable: clips already satisfy it; markers now do too.

*Done when:* `hit_test_marker_flag`'s parallel `Vec` is gone; marker
click/drag/double-click/right-click still route the same `PanelAction`s
(`ui_bridge/marker.rs` unchanged); new unit test pins flag-rect == hit-rect.

### 3.3 — Fold drag/trim/scrub into one interaction owner

- New `TimelineInteraction` (rehoused `InteractionOverlay`) owns the full
  transient gesture state — the fields it already has **plus** the ones it
  currently reaches into `UIState` for (`drag_offset_beats`, `drag_start_beat`,
  `trim_original_*`). The duplicate `trim_clip_id` collapses to one.
- `UIState` keeps only persistent state it is *read* for: selection sets,
  `selection_version`, cursor, insert cursor, zoom index, marker selection. The
  `begin_drag`/`end_drag`/`begin_trim_*`/`end_trim` methods and their backing
  fields move onto the overlay. Of the twelve transient fields, only five are
  ever read (`drag_start_beat`, `drag_offset_beats`, `trim_original_*`) — they
  move; the other seven (`is_dragging`, `is_trimming`, `is_scrubbing`,
  `drag_clip_id`, `drag_start_layer_id`, `trim_from_left`, and a *duplicate*
  `trim_clip_id`) are write-only mirrors of `drag_mode` and are deleted outright.
- **Not** reusing `trim.rs` here. The overlay's right-trim delegates the
  source-length clamp to `host.get_max_duration_beats`, which knows about audio
  warp ratio and the video library; `trim::compute_right_trim` reimplements a
  naive `(len − in_point)/spb` that would regress audio/warped clips. (`trim.rs`
  is also currently unused; left in place, not wired in.) The fold preserves the
  overlay's correct, host-delegating math verbatim — only its backing fields move.
- The `TimelineEditingHost` trait is unchanged; only who-holds-the-fields moves.

*Done when:* no drag/trim/scrub field is written in two structs; the overlay's
controller tests + a new "begin-drag touches one owner" test pass; the app
compiles against the moved API.

### 3.6 — Split the viewport god-panel

Mechanical extraction into `panels/viewport/{mod,model,coordinate,render,
interaction}.rs`. `TimelineViewportPanel` stays one struct; its `impl` blocks
split across files (Rust allows impls for the same type in sibling modules of
one parent module). No behaviour change — the same methods, regrouped. `mod.rs`
re-exports the public types so every external `use` path is unchanged.

*Done when:* `viewport.rs` is a directory; each file owns one concern; full
`cargo test -p manifold-ui` and `cargo build -p manifold-app` green; `git`
shows moves, not rewrites.

---

## 4. Invariants this must not break

- **Y-alignment** between layer headers and tracks (the whole point of one
  authority) — covered by `CoordinateMapper` tests + the viewport coordinate
  roundtrip.
- **`sync_clips_to_time` / optimistic-echo** loop and `data_version` gating —
  untouched; Phase 3 is UI-side only.
- **No per-frame allocation** on the hot paths (hover, drag, scroll, repaint).
  Marker hit-test recompute is O(markers) on click only, not per frame.
- **`enforce_non_overlap`, region-partial split, cross-layer type-compat** drag
  rules — preserved verbatim; only their owning struct changes.

## 5. Order & verification

`3.4 → 3.5/3.2 → 3.3 → 3.6`, one commit each, each pushed.
Per-step: `cargo test -p manifold-ui` (mapper, hit-tester, trim, viewport,
new tests) + `cargo clippy -p manifold-ui -p manifold-app -- -D warnings` +
`cargo build -p manifold-app`. The full picture is behaviour-preserving, so the
existing Unity-ported tests are the safety net; new tests pin the specific
duplications being removed.
