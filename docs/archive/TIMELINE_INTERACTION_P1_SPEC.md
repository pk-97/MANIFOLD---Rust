<!-- index: Timeline interaction P1 — one selection authority (clips vs time-range enum), one clip-zone geometry source (trim handles usable at every zoom), previews that show the committed result (per-frame snap+clamp, Escape cancels), selection survives mutation. Sequel to TIMELINE_LAYOUT_P0_SPEC; runs BEFORE UI_CRAFT_AND_MOTION_PLAN in the UI lane. -->

# Timeline Interaction P1 — one geometry, one selection, previews that tell the truth

**Status: SHIPPED 2026-07-05 @ `62a0f01e` (merge of `wave/timeline-fixes`: TimelineSelection enum, S5 overlap-enforcement root fix, edge autoscroll/snap, keyboard layer). Landing-flash RE-HOOKED 2026-07-07 (timeline-ux pass): fires at the Move-commit drag end, unit-tested; keep/kill/timing rides Peter's feel-pass list (`docs/TIMELINE_UX_AUDIT_2026-07-07.md` §3.1) with the rest of the still-owed running-app feel-pass. S1 multi-select chrome re-verified headless 2026-07-07 (driven render: per-clip borders, no region band). Approved Peter 2026-07-04 — scope confirmed: structural fixes as
must-ship, behavior contract as the checklist; "Nice let's build that doc".**
**Prerequisites: TIMELINE_LAYOUT_P0 (shipped 2026-07-04 — single Y source).
Blocks: UI_CRAFT_AND_MOTION_PLAN (same files; motion must not animate lying
previews or misaligned chrome).**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 + §8 first.
Anchors below are a 2026-07-04 snapshot — re-verify before each phase.**

The governing insight: P0 killed the header/lane detach class by deleting the
second copy of Y. The interaction layer has the same disease in three more
places — **selection has two authorities** (a clip-id set and a region flag
that gestures update inconsistently), **clip geometry has two authorities**
(the painter's rects and the hit-tester's private trim math), and **the drag
preview is a third authority on where a clip will land** (snap applies at
release, not during the gesture). Every bug Peter filed on 2026-07-04 is two
of those authorities disagreeing. The fix is the P0 move again: delete the
extra authority, don't sync it harder.

Companion docs: `TIMELINE_LAYOUT_P0_SPEC.md` (the pattern this doc extends to
X/selection/preview) · `TIMELINE_UI_REDESIGN.md` (visual contract — untouched
here) · `UI_CRAFT_AND_MOTION_PLAN.md` (runs after; consumes this doc's stable
chrome) · `docs/HEADLESS_UI_HARNESS.md` (the PNG evidence mechanism).

## 1. Symptoms (Peter, in-app, 2026-07-04)

- **S1** — shift-click multi-select draws misaligned chrome: light inner
  borders with gaps, a dark overlay band behind some clips, one clip in the
  range not highlighted.
- **S2** — a clip can apparently be dragged before beat 0:00, rendering on top
  of the layer-header column.
- **S3** — selecting 4 contiguous clips + Cmd+D places the copies with a gap
  after the originals instead of flush.
- **S4** — edge-drag to trim "doesn't work unless I change zoom level".
- **S5** — moving a multi-clip selection drops selection state on some clips.

## 2. Audit — what exists (verified 2026-07-04)

Extend, don't redesign. Every piece below is live code; the design reuses it.

| Piece | Where | State |
|---|---|---|
| Clip body geometry, single-source | `crates/manifold-ui/src/panels/viewport.rs:549` `visible_clip_rects` — doc-comment guarantees painter and hit-tester share body rects | GOOD for bodies; trim zones NOT included |
| Trim-handle hit zones | `crates/manifold-ui/src/clip_hit_tester.rs:34-39` (`MAX_TRIM_HANDLE_PX = 8.0`, `TRIM_HANDLE_RATIO = 0.15`), `:104-110` (`trim_w < 2.0` → no trim at all) | **S4 root**: clips narrower than ~13px on screen have NO trim zone; 2–3px zones until ~53px. Private math, second geometry authority |
| Clip selection state | `crates/manifold-ui/src/ui_state.rs:14` (`selected_clip_ids: HashSet<ClipId>`) | Authority #1 |
| Region selection state | `ui_state.rs:224, 257, 273-300` (`selection_region` + `is_active`) | Authority #2 — the flag outlives the gesture |
| Shift-click = REGION gesture | `crates/manifold-app/src/ui_bridge/editing.rs:43-51` (`select_region_to_with_project`, Unity port) | **S1 + S3 root**: shift-click activates the region; cmd-click toggles the id set (`editing.rs:52-57`) and back-syncs the region. Two gestures, two authorities |
| Region overlay drawing | `viewport.rs:621-629` (`timeline_overlays` — full track-height rect) vs per-clip selected recolor (`viewport.rs:132`, `sync_selection` `:816-827`) | Overlay band + per-clip chrome painted from different sources = S1's visual mess (hypothesis; pinned in P1.0) |
| Cmd+D | `crates/manifold-app/src/input_handler.rs:152` → `input_host.rs:571-604` → `EditingService::duplicate_clips` `crates/manifold-editing/src/service.rs:680` | `region.is_active` → duplicates **all clips overlapping the region**, offset = **region duration** (`:688-717`); else offset = clips' own span (`:719+`). Copies re-selected by before/after id diff (`input_host.rs:605+`) — that part is right |
| Move drag | `crates/manifold-ui/src/interaction_overlay.rs:679-775` (`handle_move_drag` — per-frame live mutation via host, clamp at `:764-769`), snap only at release (`finalize_move_snap` `:1045-1078`), selection untouched at end (`:562-563`) | Preview ≠ committed result: snap jumps at mouse-up. Clamp exists on this path — S2's renderer is NOT identified yet (see P1.0) |
| MoveClipCommand | `crates/manifold-editing/src/commands/clip.rs:37-47` | Preserves clip ids across move — not the S5 mechanism |
| Overlap enforcement | `clip.rs:279-334` (`AddClipCommand` overlap actions: tail clips created with NEW ids) | **S5 suspect**: a move that lands on another clip splits it; recreated ids fall out of `selected_clip_ids`. Confirm in P1.0 |
| Lane scissor | `viewport.rs:608` ("The caller scissors to the tracks rect") | Opt-in per call site — the S2 paint-over-header enabler |
| Escape during timeline drag | `crates/manifold-app/src/window_input.rs:1061-1552` (all Escape sites are popovers/pickers/output-window) | **Absent** |
| Drag threshold | `crates/manifold-ui/src/drag.rs` (`DragController` — lifecycle only, no threshold), overlay begin paths | **Absent** (⚠ VERIFY-AT-IMPL: `rg -n "threshold" crates/manifold-ui/src crates/manifold-app/src/window_input.rs` — re-check before building P1.4) |
| Edge autoscroll during drag | no hits for drag-edge scroll in `viewport.rs` / `interaction_overlay.rs` (playhead follow exists separately) | **Absent** |
| Undo selection restore | `crates/manifold-editing/src/undo.rs` — zero selection hits | **Absent** (deferred, §8) |
| Alt-drag duplicate seam | `crates/manifold-ui/src/timeline_editing_host.rs:213-214`, `crates/manifold-app/src/editing_host.rs:526-531` | Exists; one undo entry with the move — keep |
| One-undo-per-gesture | `interaction_overlay.rs:562` commits the engine batch at drag end | Exists — protect with a test, don't rebuild |

## 3. Decisions

**D1 — one selection authority: an enum, not a flag.**
`UIState` replaces the pair (`selected_clip_ids`, `selection_region.is_active`)
with a single owned value:

```rust
// crates/manifold-ui/src/ui_state.rs
pub enum TimelineSelection {
    None,
    /// Clip selection — a set of whole clips. Cmd+D offsets by the set's span.
    Clips { ids: HashSet<ClipId>, anchor: Option<ClipId> },
    /// Time-range selection — a beat × layer region. Cmd+D duplicates the range.
    TimeRange(SelectionRegion),
}
```

Exactly one kind is active at any moment; every gesture states which kind it
produces (D2). The `SelectionRegion` struct itself survives (the overlay and
region commands consume it); what dies is `is_active` as an independent flag
that gestures forget to clear. Derived conveniences (`region bounds of a clip
selection` for scroll-into-view etc.) are computed on demand, never stored.
**Rejected: keeping both states and syncing more aggressively** — copies that
must be kept in sync are the mechanism of this bug class (P0, verbatim).

**D2 — gesture → selection-kind mapping (Ableton semantics, Peter's muscle
memory is the spec).**

| Gesture | Produces |
|---|---|
| Click on clip | `Clips{[id]}`, anchor = id |
| Shift+click on clip | `Clips` — anchor's layer-contiguous range extended to the clicked clip (**changed**: today this is a region gesture, `editing.rs:43-51` — that Unity port is S1+S3's root and is deleted) |
| Cmd/Ctrl+click on clip | `Clips` — toggle membership, anchor moves to clicked |
| Rubber-band drag on empty lane | `TimeRange` |
| Shift+click on empty lane | `TimeRange` — extend region from anchor (today's `TrackClicked` shift path, kept) |
| Click on empty lane | `None` (+ insert cursor, unchanged) |
| Mouse-down on an already-selected clip | selection unchanged until mouse-up-without-drag — grab-any-member group move (B4) |

**Rejected: keeping shift-click = region** because it's literally the reported
bug; **rejected: unifying clips and range into one representation** (Ableton
keeps them distinct because Cmd+D, delete, and render-in-place mean different
things on each — so do we).

**D3 — Cmd+D honors the selection kind.** `Clips` → duplicate exactly the
selected ids, offset by the selection's own span (`service.rs:719+` path,
which already does this); copies become the new `Clips` selection (readback at
`input_host.rs:605+` kept). `TimeRange` → today's region mode (`service.rs:
688-717`), region shifts forward (`service.rs:1280`), unchanged. The mode
picker is the enum variant — the `region.is_active` test at `service.rs:688`
becomes a typed match. **Rejected: grid-rounding the region offset** — symptom
patch; the gap came from the wrong mode firing, not the wrong rounding.

**D4 — clip zones join the single geometry source.** The trim math leaves the
hit tester and lands beside the body rects it already shares:

```rust
// crates/manifold-ui/src/panels/viewport.rs — beside ClipScreenRect
pub struct ClipZones {
    pub body: Rect,        // == ClipScreenRect.rect
    pub trim_left: Rect,   // may extend OUTSIDE body into empty lane space
    pub trim_right: Rect,
    pub label: Rect,       // name strip; selection chrome insets derive from body
}
/// neighbor_gaps: px of empty lane left/right of this clip (0.0 when abutting).
pub fn clip_zones(rect: &ClipScreenRect, neighbor_gaps: (f32, f32)) -> ClipZones
```

Zone rule (decided here so the executor transcribes): inner trim width =
`min(8.0, body.width / 3.0).max(2.0)`; each handle additionally extends
**outward** by `min(4.0, neighbor_gap)` px into empty lane space; when two
clips abut, the shared boundary splits 50/50 between the left clip's right
handle and the right clip's left handle. The `trim_w < 2.0 → no trim` disable
(`clip_hit_tester.rs:105-107`) is **deleted** — with the outward extension a
1px clip still has two grabbable 4px handles. `ClipHitTester` and the cursor
choice consume `clip_zones`; the cursor can no longer disagree with the hit
test because neither owns private math. **Rejected: tuning the constants
inside the hit tester** — keeps the second authority alive.

**D5 — the preview IS the committed result.** `handle_move_drag` applies
snap-and-clamp per frame — the same functions `finalize_move_snap` uses today
— so the clip on screen sits where it will land, always. `finalize_move_snap`
(`interaction_overlay.rs:1045`) then has nothing left to do that the last
frame didn't already show; it reduces to committing the batch. Trim paths get
the same treatment (their clamps at `:777-865` already run per-frame — verify,
don't rebuild). Escape cancels an in-flight gesture by restoring
`drag_snapshots` through the same host batch that `on_end_drag` commits —
cancel is "restore and close batch", never "commit then undo". **The
plausible-wrong architecture, forbidden by name: clamping/snapping in the
painter.** A painter that "fixes" the position visually while the model holds
a different value is a fourth authority and this doc's cardinal sin.

**D6 — selection survives every mutation, structurally.** Any command that
recreates a clip under a new id (today: the overlap-split path,
`clip.rs:279-334`; audit for others in P1.0) reports an `old_id → new_id`
mapping. The batch execution path in the UI-side host applies the mapping to
`TimelineSelection::Clips` once, centrally — no per-call-site remembering.
Duplicate's select-the-copies readback stays. **Rejected: pruning missing ids
from the selection on sync** — that codifies the loss instead of fixing it.

**D7 — the scissor is structural.** Lane content (clip bodies, waveforms,
overlays, drag chrome) draws through one choke point that sets the tracks-rect
scissor; the "caller scissors" contract (`viewport.rs:608`) inverts. A draw
call cannot opt out, so nothing the timeline paints can land on the header
column again — whatever S2's renderer turns out to be, its pixels die at the
boundary, and the P0.0-style evidence run pins the actual beat-clamp bug
separately.

**D8 — a click is not a drag.** A pointer-down becomes a gesture only after
4px of travel (`DragSession.delta()`, `drag.rs:43`, already carries this);
below that it's a click on release. Kills accidental micro-moves of clips.

## 4. Behavior contract

Each row is small once §3 lands; each names its phase. This table is the
"general user expectations" half of the doc — DAW-standard behaviors Peter
called for on 2026-07-04.

| # | Expectation | Phase |
|---|---|---|
| B1 | Trim handles grabbable at every zoom (D4 zone rule) | P1.1 |
| B2 | Cursor affordance == hit zone, always (both read `clip_zones`) | P1.1 |
| B3 | Shift-click extends clip selection; rubber-band selects time-range (D2) | P1.3 |
| B4 | Mouse-down on selected clip defers deselect to mouse-up → grab-any-member group move | P1.3 |
| B5 | Cmd+D on clips lands copies flush after the selection's span (D3) | P1.3 |
| B6 | Selection survives move/duplicate/overlap-split (D6) | P1.3 |
| B7 | Preview position == landed position, snap included (D5) | P1.4 |
| B8 | Escape cancels an in-flight drag/trim, restoring the pre-gesture state | P1.4 |
| B9 | One gesture == one undo entry (protect the existing batch with a test) | P1.4 |
| B10 | 4px drag threshold; a click never moves a clip (D8) | P1.4 |
| B11 | Edge autoscroll: dragging/trimming near the viewport edge scrolls the timeline | P1.5 |
| B12 | Snap targets include clip edges and markers, not just the grid; Cmd held mid-drag bypasses snap | P1.5 |
| B13 | Numeric readout while dragging/trimming: position + length in bars.beats | P1.5 |
| B14 | Keyboard nudge: arrows move selection by grid step; Cmd+E split at playhead; zoom-to-selection | P1.6 |

## 5. Phasing

One phase = one session; every phase ends committable and gate-passing.
Escalation rule (all phases): a moved/missing anchor, a consumer that can't
adopt the new shape cleanly, or any borrow/threading wall → STOP, write
file:line + the smallest unblocking question. Never adapt, shim, or keep the
old path alive alongside the new one.

### P1.0 — Evidence deck (repro before surgery)

- **Entry state:** clean main; `docs/HEADLESS_UI_HARNESS.md` read;
  `cargo run --bin ui-snap -- --help` (or the harness's current entry — read
  the doc, don't guess) works.
- **Read-back first:** this doc §1–§3; P0 spec's P0.0 phase (the pattern);
  restate the five symptoms and which authority-pair each maps to.
- **Deliverables:** (a) headless PNG before-set: shift-click range on 4 clips,
  cmd-click multi-select, region overlay + clip selection together (S1);
  4-clip Cmd+D result (S3); narrow-clip lane at far zoom (S4 geometry, even
  though hit zones aren't visible in PNGs, the scene anchors the after-set).
  (b) Instrumented repro for S5: `println!` the selection set and all clip ids
  before/after a multi-clip move that lands on another clip vs. onto empty
  space — pin whether the overlap-split (`clip.rs:279-334`) is the id churn.
  (c) Instrumented repro for S2: log `start_beat` per frame during a drag
  toward beat 0 — pin which path renders the clip over the header (screen
  ghost? unclamped intermediate? scissor-less overlay?). (d) One paragraph per
  symptom in the phase notes: mechanism CONFIRMED or hypothesis revised.
- **Gate:** PNGs committed; S5 and S2 mechanism paragraphs written with
  file:line. *Negative:* no product code changed —
  `git diff --stat` shows only new PNGs/notes/println (printlns reverted
  before commit).
- **Forbidden moves:** fixing anything in this phase; claiming a repro that
  wasn't run (if a state can't be reproduced headless, say so — Peter runs
  the in-app repro from your exact steps, per `assume-latest-build-run`).
- **Test scope:** none beyond building the harness.

### P1.1 — Geometry authority: `clip_zones` (D4, D7 · B1, B2)

- **Entry state:** re-verify `viewport.rs:549`, `clip_hit_tester.rs:34-110`,
  `viewport.rs:608` anchors.
- **Read-back:** D4 + D7; the P0 doctrine paragraph ("remove the state, not
  the misalignment"); restate the zone rule numbers.
- **Deliverables:** `ClipZones` + `clip_zones()` beside `ClipScreenRect`;
  `ClipHitTester` rewritten to consume it (seam brief: old
  `MAX_TRIM_HANDLE_PX`/`TRIM_HANDLE_RATIO`/`trim_w` math deleted FIRST,
  compiler drives the migration); cursor selection reads the same zones;
  the lane-content scissor choke point (D7) with the `viewport.rs:608`
  "caller scissors" comment updated to state the new contract.
- **Gate:** *Positive:* hit-tester unit tests — 1px-wide clip has two
  grabbable handles via outward extension; abutting clips split the boundary
  50/50; 100px clip gets 8px inner handles; cursor-vs-hit-test agreement test
  over a zoom sweep. P1.0 body-geometry PNGs byte-identical (zones change hit
  behavior, not painted bodies). *Negative:* `rg -n "TRIM_HANDLE_RATIO|MAX_TRIM_HANDLE_PX" crates/` → zero hits;
  `rg -n "scissor" crates/manifold-ui/src/panels/viewport.rs` shows only the
  choke point.
- **Forbidden moves:** keeping the hit tester's old math as a fallback;
  zoom-level special cases; "improving" clip visuals while in the file.
- **Test scope:** `-p manifold-ui --lib`. Clippy workspace.

### P1.2 — Selection model swap (D1, mechanical)

- **Entry state:** re-verify `ui_state.rs:14/224/257`; run the inventory:
  `rg -n "selection_region|selected_clip_ids" crates/ --glob "*.rs"` — the
  2026-07-04 count is ~40 sites across `manifold-ui` (`ui_state.rs`,
  `interaction_overlay.rs`, `panels/viewport.rs`) and `manifold-app`
  (`ui_bridge/*`, `input_host.rs`, `state_sync.rs`). If the fresh count
  differs materially, list the new sites before touching anything.
- **Read-back:** D1; the may/must-escalate line; restate: this phase is
  behavior-preserving — gestures still produce what they produce today
  (including shift-click = region, until P1.3).
- **Deliverables:** `TimelineSelection` enum replaces the pair; compiler-driven
  (delete the old fields FIRST); panel caches (`viewport.rs:812-827`,
  `state_sync.rs:468-475`) read the enum; sync helpers
  (`update_region_from_clip_selection*`) become enum transitions or die.
- **Gate:** *Positive:* `-p manifold-ui --lib` + `-p manifold-app` focused
  tests green; P1.0 PNG scenes regenerate identical (behavior-preserving).
  *Negative:* `rg -n "is_active" crates/manifold-ui/src/ui_state.rs` → zero
  hits; `rg -n "selected_clip_ids" crates/manifold-app/src/ui_bridge/` → only
  enum-mediated reads (paste the survivors into the notes).
- **Forbidden moves:** keeping `is_active` as a derived-but-stored bool
  (adapter); changing any gesture semantics "while we're here" — that's P1.3.
- **Test scope:** `-p manifold-ui --lib`, `-p manifold-app --lib`. Clippy.

### P1.3 — Selection semantics (D2, D3, D6 · B3–B6)

- **Entry state:** P1.2 merged; re-verify `editing.rs:43-57`,
  `input_host.rs:571-604`, `service.rs:680-717`, and the P1.0 S5 mechanism
  paragraph.
- **Read-back:** the D2 gesture table verbatim; the S5 mechanism found in
  P1.0; restate which `service.rs` branch each Cmd+D mode uses.
- **Deliverables:** shift-click → contiguous clip range (the
  `select_region_to_with_project` call at `editing.rs:46` replaced);
  mouse-down-on-selected defers collapse to mouse-up (B4); Cmd+D matches on
  the enum (D3); id-mapping report from recreating commands + central
  selection remap in the batch path (D6 — shaped by what P1.0 found).
- **Gate:** *Positive:* unit tests — shift-click range across a gap selects
  only whole clips; Cmd+D on 4 contiguous clips lands copies flush (assert
  new start == old span end); Cmd+D on a rubber-band region preserves today's
  region behavior; multi-clip move onto an occupied lane keeps the moved
  clips selected (the S5 repro, now a test). After-PNGs for S1/S3 scenes.
  *Negative:* `rg -n "select_region_to_with_project" crates/manifold-app/src/ui_bridge/editing.rs`
  → zero hits on the clip-click path (the empty-lane shift path keeps it).
- **Forbidden moves:** fixing S3 by changing the region-mode offset math;
  duplicating all-clips-overlapping-region under a `Clips` selection
  (the mode leak this phase exists to kill); silent prune of missing ids.
- **Test scope:** `-p manifold-ui --lib`, `-p manifold-app --lib`. Clippy.

### P1.4 — Gesture integrity (D5, D8 · B7–B10)

- **Entry state:** re-verify `interaction_overlay.rs:679-775/1045-1078`,
  `:562-563`; the P1.0 S2 mechanism paragraph.
- **Read-back:** D5 including the forbidden painter-clamp; restate where snap
  lives today (release-only) and where it moves (per-frame).
- **Deliverables:** per-frame snap+clamp in `handle_move_drag` (shared with
  finalize — one function, two callers, then finalize reduces to
  batch-commit); Escape → restore `drag_snapshots` + close batch; 4px
  threshold at gesture start; the S2 fix at whatever site P1.0 pinned (with
  D7's scissor already making the header-overpaint impossible); one-undo-per-
  gesture regression test around the existing batch.
- **Gate:** *Positive:* tests — preview position after N synthetic pointer
  moves == committed position (snap on and off); Escape mid-drag restores
  byte-identical project state; sub-4px press-release moves nothing; a full
  drag produces exactly one undo entry; drag-toward-zero clamps at beat 0 in
  every frame's model state. After-PNG for the S2 scene.
  *Negative:* `rg -n "finalize_move_snap" crates/manifold-ui/src/interaction_overlay.rs`
  → the reduced form only (paste it); no `.max(Beats::ZERO)` outside the one
  shared clamp site on the move path.
- **Forbidden moves:** painter-side clamping (named in D5); Escape as
  commit-then-undo; a second snap implementation "just for preview".
- **Test scope:** `-p manifold-ui --lib`. Clippy. This phase touches the
  gesture path end-to-end — Peter does an in-app feel pass before merge
  (drag, trim, Escape, snap on/off), per `assume-latest-build-run`.

### P1.5 — Drag ergonomics (B11–B13)

- **Entry state:** P1.4 merged (autoscroll moves a live gesture — it needs
  truthful previews first).
- **Read-back:** B11–B13 rows; the hot-path rule (autoscroll runs per-frame —
  no allocations).
- **Deliverables:** edge autoscroll during move/trim/rubber-band (constant
  scroll rate scaled by edge proximity, both axes; reuses the viewport's
  single scroll owner from P0); snap-target set extended to clip edges +
  markers with Cmd-held bypass; numeric readout (bars.beats position + length)
  on the dragged/trimmed clip — plain text chrome, styling deferred to
  UI_CRAFT_AND_MOTION.
- **Gate:** *Positive:* autoscroll unit test (pointer parked at edge advances
  scroll and the gesture's beat mapping stays consistent); snap-to-clip-edge
  test (drag lands flush against a neighbor with snap on, Cmd bypasses);
  headless PNG showing the readout during a synthetic drag. *Negative:*
  no per-frame allocation on the autoscroll path (`rg -n "Vec::new|to_vec|format!"`
  over the new autoscroll fn → zero hits; the readout may format, once per
  change, not per frame — state the mechanism in notes).
- **Forbidden moves:** autoscroll by synthesizing scroll-wheel events;
  a second scroll offset (P0's corpse — do not exhume).
- **Test scope:** `-p manifold-ui --lib`. Clippy.

### P1.6 — Keyboard layer (B14) — Peter-paired

- **Entry state:** P1.3 merged (nudge operates on the selection enum).
- **Read-back:** D2 table; the binding table below is a DEFAULT drawn from
  Ableton — Peter vetoes/amends in the same session ("your reflexes are the
  spec").
- **Deliverables:** arrow-key nudge of `Clips` selection by one grid step
  (Shift+arrow = 1 beat, Cmd+arrow = fine/1 tick — default, Peter calibrates);
  up/down moves selection across layers; Cmd+E splits selected clips at the
  playhead; Z zoom-to-selection (clips or range), Shift+Z zoom-back. All
  through existing commands — nudge is a move command batch, split reuses the
  split command (⚠ VERIFY-AT-IMPL: split command exists —
  `rg -n "Split" crates/manifold-editing/src/commands/clip.rs`).
- **Gate:** *Positive:* nudge test (selection moves exactly one grid step,
  one undo entry per press); split-at-playhead test; zoom-to-selection frames
  the selection with margin (assert visible beat range). *Negative:* no new
  key handling outside the timeline's focused-input path (no global grabs).
- **Forbidden moves:** inventing a keymap system (bind in the existing input
  path); shipping bindings Peter hasn't seen.
- **Test scope:** `-p manifold-ui --lib`, `-p manifold-app --lib`. **Final
  phase of the pass → full `cargo test --workspace` + clippy workspace here.**

## 6. Decided — do not reopen

1. Selection = one enum (`None`/`Clips`/`TimeRange`); `is_active` dies; no
   stored derived region for clip selections.
2. Shift-click on a clip = clip-range selection, NOT region (Unity port
   deleted). Rubber-band/empty-lane shift = time-range.
3. Cmd+D mode is picked by selection kind, not by a flag's memory of an old
   gesture. Clips-mode duplicates exactly the selected ids, flush.
4. Clip zones (trim/body/label) live in the viewport geometry source; the hit
   tester and cursor consume, never compute. Zone rule: `min(8, w/3).max(2)`
   inner + `min(4, gap)` outward; abutting boundary splits 50/50; no
   narrow-clip disable.
5. Preview == committed result: snap+clamp per-frame; finalize = commit only;
   Escape = restore-and-close-batch. Never clamp in the painter.
6. Commands that recreate clip ids report the mapping; selection remaps
   centrally. Never prune-on-sync.
7. Scissor at one choke point; lane draws cannot opt out.
8. 4px drag threshold.
9. Motion/styling of any chrome this doc adds → UI_CRAFT_AND_MOTION_PLAN,
   not here.

## 7. Deferred (with revival triggers)

- **Undo/redo restores selection.** Needs the undo stack (content thread) to
  carry UI-side selection snapshots across the thread seam — a real design,
  not a rider. Revive if selection still vanishes on undo during dogfooding
  after D6 lands.
- **Option-drag duplicate polish.** The seam exists
  (`timeline_editing_host.rs:213`); audit its live wiring during P1.4's feel
  pass — promote to a B-row only if broken.
- **Configurable keybindings.** P1.6 hardcodes the vetted table. Revive with
  the settings-surface work.
- **Marker/automation selection joining the enum.** Markers keep their own
  list (`viewport.rs:178`) for now. Revive when automation-lane editing lands
  (AUTOMATION_LANES design).
- **Rubber-band across collapsed groups / group-row semantics.** Out of scope;
  revive with the session-grid ↔ timeline unification work.
