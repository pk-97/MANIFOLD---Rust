# Desired-State Reconciliation — one reconcile, one visibility predicate, edges as outputs

**Status:** IN PROGRESS — design approved by Peter's delegation 2026-08-20; P1–P4 not built · k3 (lead)
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

The engine keeps two models of truth with no rule about which facts live where. Some
facts are derived fresh every frame from the `Project` (stack position, blend, mute at
the compositor); others are captured at clip-start edges (renderer binding, generator
instance, audio schedule) or evaluated only when a caller remembers to mark a dirty
flag (membership while paused). Every bug in the class is a fact stored in the
edge-captured model that the UI can change without firing the edge. This design
deletes the split: **one reconciliation runs every tick in every transport state,
visibility is decided once per frame in one place, and edges are emitted by the
reconcile, never maintained by callers.**

Peter's directives, verbatim: "No stop gaps, let's see if we have everything first.
This is a good oppurtunity to upgrade these systems, simplify them, reduce complexity,
improve performance, and future proof these fundamental systems further." And: "If you
are happy with all of the decisions, designs, architectures, implementaton and
orchestration plans you have permission to implement this full upgrade in full end to
end as an automated orchestration session." The three decisions that were his (mute
temperature, sim-under-mute, scope) were delegated by that sentence and are D2/D4
below.

Companion docs: `CORE_ENGINE_MAP.md` (the map this redesign rewrites sections of —
section 5 (sync_clips_to_time — the sole authority) and the frame order in section 3
(The frame, end to end)); `EFFECT_CHAIN_LIFECYCLE.md` (chain pool survives unchanged —
chains key on LayerId, which this design relies on).

**For the instrument:** mute/unmute becomes instant and glitch-free in every transport
state — unmuting reveals a generator that kept cooking, never a reboot. Dragging a live
clip across layers rebinds the same frame instead of lying until the next rising edge.
The whole "paused means stale" category dies.

## 1. Audit — what exists (verified 2026-08-20, slot-4 worktree @ e085a2974)

| Piece | Where | State |
|---|---|---|
| Pure membership diff | `scheduler.rs:122` (`ClipScheduler::compute_sync`) | **Extend.** Pure, tested, zero-alloc; diffs on `clip_id` alone |
| Sole-authority sync | `engine.rs:1338` (`sync_clips_to_time`) | **Extend.** Idempotent; runs every tick only while playing (`engine.rs:859`) |
| Paused tick | `engine.rs:972` (`tick_non_playing`) | **Change.** Syncs only on `consume_sync_dirty` (`engine.rs:976`) |
| sync-dirty flag | `engine.rs:551,557`; writers: `content_commands.rs:263,281,315,370,734,745`, `live_clip_manager.rs:843` (+ trait decl `:31`, test impl `tests/live_clip.rs:83`) | **Delete.** Correctness must not depend on callers remembering |
| Mute/solo in membership | `timeline.rs:407-446` (`get_active_clips_at_beat_ref` filters layer mute, parent mute, solo, clip mute) | **Delete filters.** This is the double authority |
| Mute/solo at compositor | `layer_compositor.rs:263,1009,1059,1808,1851` (live descriptor checks) | **Extend.** Becomes the single visual authority, fed one computed flag |
| Mute at audio tap | `audio_layer_playback.rs:290` (`tap_hot` reads live flags) | **Keep as-is.** Already the model this design copies |
| Audio clip scheduling | `audio_layer_playback.rs:271` (`update` drives every clip under the playhead, voices keyed by ClipId, rebuilt on path change `:206`) | **Keep as-is.** Already reconciled, not edge-bound — proof the model works |
| Prewarm mute filter | `engine.rs:2551-2563` | **Delete.** Hot-muted clips prewarm like any active clip |
| Clip→renderer binding | `engine.rs:1122` (`start_clip`), `active_clip_renderers` map | **Extend.** Engine gains realized-layer tracking beside it |
| Clip→generator binding | `generator_renderer.rs:400-422` (`acquire_clip` early-returns for active ids) | **Keep.** Healed indirectly via stop+start, never directly |
| Generator rebuild sweep | `generator_renderer.rs:580` | **Keep.** Mid-flight graph edits already handled — NOT part of the bug class |
| Occlusion render-skip | `content_pipeline.rs:131` (`compute_render_skip_indices`); consumers `generator_renderer.rs:721`, `layer_compositor.rs:1088,1877` | **Extend.** Muted layers join the candidate set under the same safety filter |
| Layer descriptor | `compositor.rs:10-31` (`CompositeLayerDescriptor.is_muted/is_solo`) | **Change.** `is_muted`/`is_solo` replaced by one computed `hidden` flag |
| Mute/solo command path | `ui_bridge/layer.rs:29-123` → `ContentCommand::MutateProject` → `content_commands.rs:556-590` (marks compositor dirty only) | **Keep.** Correctness no longer depends on which dirty flag it marks |
| Clip descriptor | `layer_compositor.rs` (`CompositeClipDescriptor`) | **Change.** Gains `is_muted` (clip-level mute becomes presentational) |
| `ActiveClipRef` | `scheduler.rs` | **Change.** Gains `is_muted` (P2) and `layer_id` (P3) |

Re-derivation (run at execution; counts must match or stop and list new sites):
`rg -n "mark_sync_dirty|consume_sync_dirty|sync_dirty" crates/ --type rust` (13 sites
today) · `rg -n "get_active_clips_at_beat" crates/ --type rust` (callers beyond
`engine.rs:1343` unknown — ⚠ VERIFY-AT-IMPL) · `rg -n "is_muted" crates/manifold-playback/src/audio_mixdown.rs`
(export mixdown's mute handling — ⚠ VERIFY-AT-IMPL: it must keep reading live flags) ·
`rg -n "active_audio_clip_at" crates/manifold-core/src` (does audio clip membership read
clip mute — ⚠ VERIFY-AT-IMPL: audio stays untouched either way).

## 2. Decisions

**D1 — Reconcile every tick, every transport state; `sync_dirty` deleted.**
`tick_non_playing` calls `sync_clips_to_time()` unconditionally, matching
`tick_playing` (`engine.rs:859`). The flag, its 8 writers, the trait method, and both
`consume` sites are deleted compiler-first (delete the method, fix the red).
Rationale: a correctness-critical side effect that depends on every edit path
remembering to request it is unmaintainable by construction — the paused-mute stall
(BUG-gg64 (paused mute stall)) is that failure observed. Rejected: keep the flag as an
optimization — the work it skips (query + diff, scratch-based, zero-alloc) already runs
every playing tick at full project scale; a flag that saves nothing but can be
forgotten is pure risk. **Consequences, stated honestly:** a stopped transport with
the playhead over clips now activates them immediately (today that only happens after
a seek marks dirty). This is uniform paused-preview semantics — what you see stopped
is what play shows — and it rewrites `engine_tick_while_stopped_has_no_active_clips`
into its positive form. Rejected alternative shape: extract desired-state resolution
into a new pure module — `compute_sync` already IS the pure, tested unit; a new home
is migration cost for zero semantic gain.

**D2 — Mute/solo/clip-mute leave membership entirely (hot mute).**
The filters in `get_active_clips_at_beat_ref` (`timeline.rs:417-441`) and prewarm
(`engine.rs:2551-2563`) are deleted. Muted clips stay active: bound, scheduled,
modulating. Visibility becomes purely presentational (D3). Rationale: unmute must
never pay a decoder re-prepare + generator rebuild + recently-started gate on stage —
that is the black-frame-on-unmute class. The audio side already works exactly this way
(`audio_layer_playback.rs:290`). Rejected: cold mute (mute stays in membership, D1
alone fixes the stall) — smaller diff, but unmute reboots content mid-show and keeps
two authorities. **Consequences, stated honestly:** (a) muted video layers keep their
decoders resident AND running — ten muted 4K layers cost decode bandwidth with no GPU
relief beyond D4's render-skip; if profiling shows this, the revival trigger is the
Deferred entry for pausing muted players. (b) Mute/unmute no longer fires clip
stop/start edges: trigger counters don't increment on unmute, param-step actions don't
fire on mute, decay envelopes keep running (they already do — CORE_ENGINE_MAP.md
section 8 (Modulation pipeline): "Muted layers still modulate"). This is the correct
semantic — muting is not the clip ending — and it is a behavior change.

**D3 — Visibility is decided once per frame, in one place, as one flag.**
`CompositeLayerDescriptor.is_muted/is_solo` is replaced by `hidden: bool`, computed in
`content_pipeline.rs` at descriptor build (`content_pipeline.rs:2295-2315`) by one
predicate: `hidden = layer.is_muted || parent.is_muted || (any_solo &&
!layer.is_solo && !parent.is_solo)`. Every compositor check site
(`layer_compositor.rs:263,1009,1059,1808,1851`) reads `ld.hidden`; `any_solo`
disappears from the renderer crate. Occlusion (`compute_occluded_layer_indices`) and
render-skip (`compute_render_skip_indices`) take the hidden set as input — a muted
opaque layer no longer occludes the layers beneath it. Clip-level mute rides the same
frame: `ActiveClipRef.is_muted` → `CompositeClipDescriptor.is_muted`, checked at the
same generate/blend sites. Rationale: today three systems re-decide visibility and
disagree about timing; one predicate computed once per frame cannot disagree with
itself. Rejected: per-consumer checks reading live flags (that is today's disease,
renamed). Parent-group semantics are preserved by folding them into the predicate at
build time — today parent mute/solo reach visuals only through membership, so this is
also the only correct way to remove the membership filters.

**D4 — Muted layers join the occlusion render-skip, inheriting its exact semantics.**
The hidden set feeds `compute_render_skip_indices` (`content_pipeline.rs:131`) as
additional candidates under the SAME safety filter (top-level leaf, no group, not
LED-tapped, no preview open). Stateless generators then cost nothing while muted and
return perfectly evolved (they render from the time uniform). Stateful sims freeze
while muted — identical to an occluded layer today. Grouped and LED-tapped muted
layers keep rendering (blend-skip only), same as occluded ones. Rejected: any
mute-specific skip policy — invent nothing; the occlusion predicate already answered
"which hidden layers are safe to not render."

**D5 — Binding identity is (clip_id, layer_id); mismatch heals via existing
stop+start, same tick.**
The engine tracks the realized layer per active clip in
`active_clip_layers: AHashMap<ClipId, LayerId>` (maintained in `start_clip`/`stop_clip`
beside `active_clip_renderers` — same category of realized-state tracking, not a new
system). `ActiveClipRef` gains `layer_id`, populated where the layer is already in
hand (timeline query, live slots, session resolve). After the `compute_sync` diff,
`sync_clips_to_time` walks should-be-active entries that are already active; a
`layer_id` mismatch pushes the clip onto to_stop AND to_start this tick. The heal is
the existing machinery — `GeneratorRenderer::acquire_clip` rebinds on the fresh start
because the stop removed the id. Rationale: "clip on layer B" means "B's generator
through B's chain," and stop+start is the engine's only honest re-resolve. Rejected:
rebind-in-place (new machinery in every renderer for a rare gesture — Peter's call,
confirmed in discussion). Rejected: include loop-mode/source/trim in the identity —
deferred with trigger; those edits mid-flight are rarer than the drag and each has a
working retrigger path. **Consequences, stated honestly:** the drag of a live clip
rebinds with a one-frame clear + re-acquire (generator clips) or a re-seek (video) —
an accepted blink on an edit gesture, never a steady-state cost.

**D6 — No new abstractions.** Vetoed by name, for the executor who will reinvent them
at 2am: no event bus or observer pattern for dirty propagation; no generic "binding
registry" or invalidation framework; no rebind pathway beside stop/start; no
`mark_sync_dirty` reintroduction under another name; no caching of desired state
between ticks (recompute — the query is scratch-based and already runs every playing
tick). If a piece of this design can't name what it deletes, it doesn't ship.

## 3. The reconcile contract

After this design, `sync_clips_to_time` is: compute desired membership from (Project,
beat, live slots, session refs) → diff against realized (clip_id set + realized
layer) → heal via stop/start → emit edges (clip-edge layers, trigger counters) as
outputs. It runs every tick in every transport state, takes no dirty input, and is
safe to call redundantly (idempotent today, stays so). `mark_compositor_dirty` and the
paused-tick `filter_ready_clips` gating are untouched — they are UI-snapshot and
render-filter optimizations that never decided correctness of membership, and still
don't. `seek_active_clips` after paused syncs stays (scrub-while-stopped re-anchors
video). Export is unaffected: export ticks already sync every tick.

## 4. Invariants & enforcement

1. **Membership never reads mute/solo.** Enforcement: `rg "is_muted|is_solo" crates/manifold-core/src/timeline.rs` → zero hits inside `get_active_clips_at_beat_ref`; cure-test `muted_layer_clip_stays_active` (P2).
2. **Reconcile runs every tick in every state.** Enforcement: cure-test `stopped_engine_activates_clip_under_playhead` — stopped engine, no dirty calls, tick once, clip active (rewrite of `engine_tick_while_stopped_has_no_active_clips`, P1).
3. **Visibility is single-sourced.** Enforcement: `rg "any_solo|is_solo" crates/manifold-renderer/src/layer_compositor.rs` → zero hits; `rg "\.is_muted" crates/manifold-renderer/src/layer_compositor.rs` → zero hits outside the `hidden` field read (P2).
4. **An active clip's realized layer matches its project layer within one tick.** Enforcement: cure-test `drag_active_clip_across_layers_rebinds` (paused and playing variants, P3).
5. **No new shared state, no per-frame allocation on the reconcile path** (house rules). Enforcement: `rg "Arc<Mutex|Arc<RwLock" crates/manifold-playback/src` → zero new hits; reconcile uses existing scratch buffers only — verified by the P1 gate tests plus code review.

## 5. Phasing

### P1 — Reconcile every tick; delete `sync_dirty` (fixes the stall mechanism of BUG-gg64 (paused mute stall))

- **Entry state:** slot-4 worktree on `lane/reconcile-redesign`; audit anchors re-run
  (the section 1 (Audit) commands; 13 sync_dirty sites).
- **Read-back:** this doc's D1/D6 + CORE_ENGINE_MAP.md section 5 (sync_clips_to_time — the sole authority);
  restate: reconcile is unconditional; the flag is deleted compiler-first; no new
  call-request mechanism.
- **Deliverables:** `tick_non_playing` syncs unconditionally (`engine.rs:976` branch
  removed); `mark_sync_dirty`/`consume_sync_dirty`/`sync_clips_dirty` deleted
  (engine, `LiveClipHost` trait + impls, 7 `content_commands.rs` sites); the
  stopped-state test rewritten as `stopped_engine_activates_clip_under_playhead`.
- **Gate — positive:** `cargo test -p manifold-playback` green (all 8 `engine_tick`,
  19 `live_clip`, 5 `session_mode`); the rewritten cure-test fails on pre-P1 code
  (verify by stashing). **Negative:** `rg "sync_dirty" crates/ --type rust` → zero hits.
- **Content-thread work gate:** paused `MANIFOLD_RENDER_TRACE=1` run over the
  Liveschool fixture, 300 frames — no frame >20ms attributable to the sync
  (sync already costs this every playing frame; this proves paused parity).
- **Demo:** none — L1 (no observable surface beyond the cure-test).
- **Performer gesture:** pause mid-clip, mute a layer, unmute — visible state tracks
  without touching play.
- **Forbidden moves:** keeping the flag "as a hint"; gating the paused sync behind
  has_active_clips; touching mute semantics (that's P2).
- **Test scope:** `cargo nextest run -p manifold-playback`; clippy `-p manifold-playback -p manifold-app`.

### P2 — Hot mute: visibility predicate + membership filter removal (fixes the semantics of BUG-gg64 (paused mute stall))

- **Entry state:** P1 landed on the branch; `rg "sync_dirty" crates/` → zero.
- **Read-back:** D2/D3/D4 + audit; restate: mute leaves membership, one `hidden` flag,
  occlusion + render-skip consume it, parent-group semantics folded at build time.
- **Deliverables:** `hidden` predicate at `content_pipeline.rs:2295`; descriptor
  `is_muted`/`is_solo` → `hidden`; compositor check sites updated; occlusion input
  excludes hidden layers; render-skip candidates include hidden layers under the same
  safety filter; `timeline.rs:417-441` and `engine.rs:2551-2563` filters deleted;
  `ActiveClipRef.is_muted` + `CompositeClipDescriptor.is_muted` + clip-mute check at
  the same generate/blend sites; cure-tests `muted_layer_clip_stays_active`,
  `muted_layer_hidden_from_composite` (predicate unit test), `clip_muted_stays_active_hidden`,
  `muted_layer_does_not_occlude_below`.
- **Gate — positive:** `cargo test -p manifold-playback -p manifold-renderer -p manifold-app`
  green; new cure-tests fail on pre-P2 code. **Negative:** `rg "any_solo|is_solo"
  crates/manifold-renderer/src/layer_compositor.rs` → zero; `rg "is_muted"
  crates/manifold-core/src/timeline.rs` → zero inside the query fn.
- **Acceptance demo (L2, computed):** headless render of a two-layer paused scene via
  the headless harness → PNG; scripted region-mean probe: output mean with top layer
  muted equals output mean with top layer deleted (±1/255), and differs from unmuted.
  Exact command named at impl (harness invocation per `headless_harness.rs`).
- **Round-trip gate:** no serialized state touched — N/A.
- **Content-thread work gate:** none added beyond P1 (render-skip only removes work).
- **Performer gesture:** mute a layer mid-clip while playing, unmute two bars later —
  the generator reappears evolved, no reboot, no black frame.
- **Forbidden moves:** pausing muted video players (Deferred); a second visibility
  predicate anywhere; muting via membership "temporarily" for groups.
- **Test scope:** nextest `-p manifold-playback -p manifold-renderer -p manifold-app`;
  clippy same. GPU-proofs NOT required (no kernel, graph, or shared-WGSL change —
  predicate is CPU-side).

### P3 — Layer-aware binding identity (fixes BUG-2z07 (layer-drag staleness))

- **Entry state:** P2 landed; `rg "any_solo" crates/manifold-renderer/src/layer_compositor.rs` → zero.
- **Read-back:** D5/D6; restate: heal is stop+start only; identity is exactly
  (clip_id, layer_id) — no structural fields this phase.
- **Deliverables:** `ActiveClipRef.layer_id` (populated at timeline query, live-slot
  fill, session resolve — ⚠ VERIFY-AT-IMPL: `rg -n "ActiveClipRef {" crates/ --type rust`
  for all construction sites, incl. `scheduler.rs:216` test helpers);
  `engine.active_clip_layers` maintained in `start_clip`/`stop_clip`/`stop_all_clips`;
  the post-diff layer-mismatch walk in `sync_clips_to_time`; cure-test
  `drag_active_clip_across_layers_rebinds` (paused + playing: engine with
  StubRenderer, move the clip's layer via direct project mutation, tick, assert a
  stop+start pair fired for the clip and `start_clip` saw the new layer).
- **Gate — positive:** `cargo test -p manifold-playback` green; cure-test fails on
  pre-P3 code. **Negative:** none beyond suite (no symbol deletions this phase).
- **Demo:** none — L1.
- **Performer gesture:** drag a sounding/playing clip to another generator layer
  mid-bar — the new layer's content appears next frame.
- **Forbidden moves:** rebind-in-place; renderer-trait additions (`clip_layer_id`
  getters); touching loop-mode/source identity (Deferred).
- **Test scope:** nextest `-p manifold-playback`; clippy `-p manifold-playback`.

### P4 — Landing

- **Deliverables:** `scripts/landing_gate.py` green on the branch; merge
  `lane/reconcile-redesign` → main per `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol);
  this doc's Status header updated; BUG-2z07 (layer-drag staleness) and BUG-gg64
  (paused mute stall) closed; supersession sweep:
  `rg "sync_dirty|mark_sync_dirty" docs/` and `rg -i "reconcile|hot mute" docs/
  ~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/memory/` — fix or tombstone
  every stale hit; CORE_ENGINE_MAP.md sections 3 (frame order) and 5 (sync authority)
  updated; lifecycle call on this doc (cited contract vs archive).
- **Gate:** landing gate script exit 0; the two bug beads closed with the landing
  commit named.
- **Landing report:** committed file per DESIGN_DOC_STANDARD.md section 8.10 (the landing report is a committed file),
  with the P2 demo artifact, level reached per phase (P1 L1, P2 L2, P3 L1), and a
  ≤2-minute click-script for Peter.

## 6. Decided — do not reopen

1. Reconcile is unconditional; `sync_dirty` is deleted, not demoted (D1).
2. Mute/solo/clip-mute are presentational, never membership (D2).
3. Visibility is one `hidden` flag computed once per frame in `content_pipeline`;
   the renderer never recomputes solo logic (D3).
4. Muted layers inherit the occlusion render-skip predicate exactly (D4).
5. Binding identity is (clip_id, layer_id); heal is existing stop+start; no
   rebind-in-place, ever (D5).
6. No new abstractions — no event bus, no registry, no desired-state cache (D6).

## 7. Deferred

- **Pause muted video players** (decode relief for hot-muted 4K layers). Trigger:
  profiling shows muted-layer decode load in a real project; machinery exists
  (`pending_pauses`, transport pause/resume).
- **Structural fields in binding identity** (loop-mode, source, trim changes
  mid-flight). Trigger: a bug report of stale content after such an edit; each has a
  working retrigger path today.
- **Cold-mute option** (mute as full stop, for GPU relief). Trigger: Peter asks for
  it after living with hot mute; would be a new layer toggle, not a return of
  membership filtering.
- **UI preview/thumbnail refresh paths** (layer-header previews, node atlas) — a
  separate seam with its own caching; untouched here. Trigger: a stale-preview report
  that reproduces after this lands.
