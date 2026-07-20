# Chain Fusion — Cross-Card Freeze Compiler Design

**Status:** BUILT 2026-06-11 (`SEGMENT_CACHE`/`SegmentView` and
`StateStore::migrate_node` are live in-tree). **`docs/FREEZE_COMPILER_MAP.md` is the
authoritative current-state map — read it first.** v1 scope as built: adjacent-card
pointwise seams, single-input single-output cards, per-region gate on every cross-card region.
(Note 2026-07-16: the per-region *measurement* gate described here was later removed — the
fuse decision is structural now, no measurement; see the map's stale-docs list.)

**Branch:** `chain-fusion`. Campaign context: `project_graph_perf_campaign_2026_06`
(roadmap #3). Target: the ~8ms effect-chain slice of the Liveschool 4K frame — a layer
running ColorGrade → Kaleidoscope → Bloom pays a full-canvas write+read at every card seam
even when both sides of the seam are pointwise.

---

## 1. The load-bearing finding

The chain runtime does **not** need restructuring. A chain is already one `Graph` with one
`ExecutionPlan`: `PresetRuntime::try_build` (preset_runtime.rs) splices every card's
`EffectGraphDef` into a single Source → card₁ → card₂ → … → FinalOutput graph and compiles
one plan. The per-seam round-trip exists for exactly one reason: fusion runs **per card def**
— `fused_view_for(effective_def, base_view)` is called once per card inside the splice loop,
so `partition_regions` never sees two cards in one def. Each card's fused kernel ends at the
card's output texture; the next card's kernel re-reads it.

Consequence: cross-card fusion is "hand the existing region finder a bigger def", not a new
compiler tier. Everything downstream — classify, region union, convexity, codegen, retarget,
the per-region gate, the proof harness — operates on `EffectGraphDef` and is reused unchanged.

## 2. Where the cross-card def is assembled

Inside `try_build`, as a **segmentation pass** over `active_effects` before the splice loop.

A **segment** is a maximal run of ≥2 consecutive cards that are all fusion-eligible:

- has a `LoadedPresetView` (preflight already guarantees this);
- not the watched (`preview_effect`) card — editor == performance: the open card renders
  unfused via the existing mechanism, and it is a segment boundary so its neighbours can
  still fuse among themselves;
- not in any enabled group — `needs_mix` emits a Mix sub-graph around grouped cards
  regardless of the current `wet_dry` value (state-preservation across the 1.0 crossing),
  so *any* grouped card is a v1 boundary. This subsumes "mix < 1.0 is a boundary";
- skip — **amended during build** (the original "skip-capable card is a boundary" rule
  would have excluded nearly the whole shipped library: ColorGrade, Invert, HdrBoost,
  Dither all declare `OnZero`, nullifying the ColorGrade → Kaleidoscope → Bloom target).
  The amended rule rests on a runtime fact: skip is **static per build** — `is_skipped_for`
  is in `compute_topology_hash`, a flip rebuilds the chain, and a skipped card is never
  spliced at all. So no fused kernel ever contains skip logic, which is the constraint's
  intent. Concretely: a currently-skipped card is *transparent* (excluded from the segment
  without breaking the run — the exact adjacency the per-card path produces); a
  currently-active `OnZero` card is an ordinary member; each skip state is its own segment
  content key, and a flip rides the existing rebuild → fallback → background-compile path.
  Flip churn is absorbed by the cache after the first crossing in each direction;
- def contains no stateful primitive (checked on the flattened effective def). Stateful
  cards splice per-card exactly as today, byte-identical, and their state identity
  (`StateStore` keys, primitive-instance state) is untouched by fusion;
- Transform IO (single texture in, single texture out) — true by construction for every
  effect preset.

For each segment, build a **concatenated def**:

1. Flatten each card's effective def (`fx.graph` if edited, else canonical) via
   `flatten_groups` — same as `fuse_canonical_def_masked` does today.
2. Namespace every node id with the card's segment position: `c{i}.<node_id>`. Required
   because NodeIds are only unique per def; two Blooms in one segment would collide.
   Wires, per-node params, and `wgsl_source` configs carry over unchanged.
3. Stitch seams: drop card i's FinalOutput and card i+1's Source; wire the producer that
   fed FinalOutput.in to every consumer that read Source.out. Card 0's Source and the last
   card's FinalOutput survive as the segment's boundaries.
4. Per-card `system.generator_input` nodes survive (namespaced); each card's `EffectSlot`
   keeps pushing frame scalars to its own.

Then `fuse_segment(concat_def, per_card_bindings)` runs the **existing** pipeline:
`partition_regions` → region mask (see §4) → codegen → `FusedDef { def, retarget,
expected_spaces }` → `fused_def_builds` validation. A region that spans a seam is just a
region. The result is cached (see §3) as a `SegmentView`:

- the fused segment def (spliced **once** in place of the cards' individual splices);
- per-card retargeted static binding lists (namespace-prefixed lookup into the shared
  retarget map, then the existing `retarget_bindings` semantics including the
  EnumRound→IntRound rewrite from 62dad20c);
- the retarget map itself, for per-instance user bindings (the existing repoint at
  preset_runtime.rs:981 extends with the card's namespace prefix).

**Each card keeps its own `EffectSlot`.** `node_map` resolves against the segment splice
(NodeIds are namespace-unique); `bound.apply` walks that card's `fx.param_values` with its
own `source_index` mapping, unchanged. Every slider, MIDI map, Ableton binding, and envelope
keeps writing `param_values` → `ResolvedBinding` → fused uniform field, through the same
machinery, per frame. Fused uniform fields are named by segment-wide member index
(`n{idx}_<param>`), so two same-typed cards never collide.

Binding parity note: there is no generator analog of a card *chain* — generators are single
cards rendered through `JsonGraphGenerator::from_def`. The only shared machinery touched is
the effect-side `retarget_bindings`; `retarget_binding_defs` (generator) has no cross-card
case. The real future seam — generator output → first effect card — crosses two runtimes
and is explicitly out of v1 (noted in §7).

## 3. Cache key and lifecycle

**Key = `def_content_key(concat_def)`** — the existing structural content key. It subsumes
every element the campaign memory called for:

- card type ids + per-card content: the concat def *is* the cards' effective defs
  (canonical or edited), so an inner-graph edit changes the key;
- order: namespacing is positional (`c0.`, `c1.`, …), so [A,B] ≠ [B,A];
- bypass / skip / enabled / watched states: these determine segmentation *upstream* —
  a disabled card isn't in `active_effects`, a skipped or watched card is a boundary —
  so each distinct on/off combination produces different segment extents and therefore
  different keys. No second keying mechanism needed.

Storage: a `SEGMENT_CACHE: thread_local AHashMap<u64, Option<&'static SegmentView>>`,
sibling of `FUSED_EFFECT_CACHE`, negative-cached, capped (same `FUSED_CACHE_CAP` rationale).
A second map holds the segment **gate verdicts** (`u64 → bool` / winning region mask) —
unlike the per-type `OnceLock<AHashMap>` verdicts, segments are project-specific and arrive
over time, so this is an insert-only keyed map, not set-once.

## 4. The per-region gate across cards

`tune_all`'s greedy leave-one-out generalizes directly: the segment def's regions are
masked and measured through `measure_def` like any multi-region card. Two refinements:

- **Seed from per-card verdicts.** A segment region whose member nodes all carry one
  card's prefix is interior to that card; if the card's tuned winner dropped the matching
  region (compare member node-id sets, prefix-stripped), pre-drop it in the segment mask.
  Only seam-spanning regions are genuinely novel measurements.
- **When measurement happens** (never-stall constraint):
  - *Project load:* chains are known from the project file. Load enqueues every chain's
    segments for compile + measure on the worker (§5) immediately, before the show starts.
    A chain dispatched before its verdict lands renders per-card (current performance)
    until the swap — degraded-to-today, never stalled.
  - *Live edit:* the novel segment compiles + measures in the background. Live-edit
    measurement is capped to fused-all vs unfused (no greedy exploration mid-show; the
    greedy pass is a load-time luxury). Measurement GPU time contends with the render
    queue for a bounded one-off window (~tens of 4K frames) in the seconds after an edit
    the operator just made, while the chain is already at fallback performance. If fused
    loses, the veto is cached and the chain stays per-card — never-worse holds per region,
    per device, same as every prior tier.

## 5. Background compile and the fallback story

**Per-card rendering is the permanent fallback and is byte-identical code** — the existing
splice loop, untouched. It runs when: chain fusion is disabled (`MANIFOLD_CHAIN_FUSION=0`
kill-switch, plus it inherits `MANIFOLD_FREEZE=0`), the segment cache misses, the segment
gate vetoed, or `fuse_segment` returned None (no fusable region / stranded binding /
build failure — all existing fail-closed paths).

Live-edit sequence (reorder / insert / delete / bypass / skip flip / editor open-close):

1. Topology hash changes → `dispatch_chain` rebuilds → segmentation runs → new segment
   keys miss → cards splice **per-card immediately** (a few frames at current performance,
   never a stall, never a wrong frame).
2. The miss enqueues a compile job to the **fusion worker** (one background thread; jobs
   and results cross on crossbeam channels — no new shared state, per the hard rule).
   `compile_fused_view` was explicitly built device-free ("the unit that relocates to a
   background worker later", install.rs) — `fuse_segment` inherits that; the worker owns
   its own `GpuDevice` + queue for measurement only.
3. Worker posts the finished `SegmentView` + verdict. The content thread drains results at
   `dispatch_chain` entry, inserts into the thread-local caches, bumps a global atomic
   `chain_fusion_generation`.
4. A runtime built with pending segments records that fact + the generation it saw;
   `needs_rebuild` adds `cg.has_pending_segments() && generation_advanced`. The next
   dispatch rebuilds, hits the cache, splices fused.

**Status: BUILT (2026-06-11), and broader than first scoped — the harvest runs on EVERY
chain rebuild, not just the swap-in.** Reordering, adding, removing, or bypassing a card
no longer resets the other cards' sims and trails at all (the pre-existing wipe-on-rebuild
wart is gone, per Peter's direction while dialing in chains). Three pieces, because state
lives in three places: (1) node *impls* (`Box<dyn EffectNode>` — Watercolor RTs, DNN
workers) swap from the prior graph into the new one, matched by `(EffectId,
def_content_key)` per card then stable `NodeId` + type per node; (2) StateStore buckets
re-key old→new instance ids (`StateStore::migrate_node`); (3) the cross-frame *pixels* —
feedback's ping-pong pair — live in backend persistent slots, so each harvested node's
persistent textures install into the new backend's slots (one retain, no GPU copy) and
`Executor::mark_persistent_initialized` suppresses the first-frame clear-to-black that
would otherwise wipe them. Harvest skips on dimension change (resolution-dependent state
must rebuild); intentional resets (seek, load, idle clear, card deletion, editing the
card's own graph) are untouched. Proof: a chain rebuilt mid-trail with the prior as donor
continues bit-for-bit like a never-rebuilt chain; a donor-less rebuild visibly resets.

**Membership gate (added 2026-06-11 after on-stage testing):** the harvest runs only when
the rebuild keeps the SAME SET of active cards — reorders, value edits, editor
open/close, fused-segment swap-ins. A membership change (card added / removed / enabled /
disabled / skip-flipped) resets everything, as before the harvest existed. Reason: a
feedback trail accumulated through a card that was just toggled off holds that card's
look, and latching blends (Screen / Additive at full amount) hold it FOREVER — Peter hit
exactly this: blown-white quad-mirrored frames frozen in the loop after toggling Quad
Mirror / Infrared off, with no escape. Toggling is an intentional look change; the reset
is the escape hatch.

**Upstream-prefix gate (same day, second on-stage repro):** per card, the harvest also
requires the ordered sequence of cards *before* this one to be unchanged — a stateful
card's state is a picture of its upstream chain, so an upstream reorder resets exactly
that card while downstream reorders carry. Identity is EffectId (not content key), so
upstream value edits carry and the trail evolves with the tweak. Net semantics: carry =
same card set + same upstream order + own content unchanged + same dimensions.

**The swap-in rebuild is user-invisible, so it must be state-invisible.** Rebuilds today
recreate every primitive instance and a fresh `StateStore` — acceptable when the user
caused the rebuild, not when the compiler did. `try_build` gains a `prior` runtime:
for any card whose splice is content-identical across the two builds (always true for
stateful cards — they're boundaries, spliced per-card both times), the freshly-constructed
node instances are replaced by the prior graph's instances (moved, keyed by
`(EffectId, NodeId)` + type match) and `StateStore` entries re-key old→new
`NodeInstanceId`. This is exactly "state identity keyed by card, never by chain position":
moving a bloom rebuilds the chain, but the fluid sim's harvested state carries across.
Harvest is skipped when width/height changed (resize legitimately drops
resolution-dependent state, as today).

## 6. Editor == performance

The watched card is a segment boundary and renders unfused (existing watched-target
mechanism, already in the topology hash). Opening the editor on card B of [A,B,C]
dissolves the segment — A and C render per-card or in smaller segments. Closing it
rebuilds; the segment recompiles in the background if its key is cold. Closing the editor
must not shift the look: the checkpoint and the final proofs hold fused-segment output to
ulp-class agreement with the per-card sequence (freeze/proof.rs standard, same as every
prior tier).

Open verification item (parity with per-card fused behavior, checked during build):
`apply_inner_overrides` on a value-only edit (`graph_version` bump, no structure change)
against a card whose inner nodes are fused away — whatever the per-card fused path does
today, the segment path must do identically. If per-card today recompiles via content key
only on the next rebuild, segments inherit that; if it retargets in place, segments extend
the same retarget.

## 7. v1 scope and explicit non-goals

In scope: adjacent-card pointwise seams (the stencil/buffer tiers participate only where
regions already admitted them per card — interior regions keep their tuned masks);
single-in/single-out cards; per-region gate on every cross-card region; background compile
with per-card fallback; state harvest across user-invisible rebuilds.

Out of v1, noted for later legs:
- generator → first-card seam (crosses `JsonGraphGenerator` / `PresetRuntime` boundary);
- fusing through Mix (wet/dry) seams — Mix is pointwise and the obvious v2 candidate, but
  group semantics (multi-segment groups, the 1.0-crossing rebuild avoidance) make it a
  separate, carefully-proved leg;
- fusing through skip-capable cards (would need dynamic specialization or per-frame
  kernel selection);
- cross-card CSE / algebraic rewriting (deep-layer item in the campaign map).

## 8. Checkpoint and validation

Fail-fast checkpoint: the first concatenated two-card def that builds gets a parity proof —
fused two-card segment vs the same two cards spliced sequentially, ulp tolerance — before
any generalization (segmentation pass, worker, gate plumbing).

Battery (end of build): `cargo clippy --workspace -- -D warnings`; freeze suite +
`node_graph::execution::tests` + `bundled_presets` (release, flaky-under-parallel suspects
verified in isolation); `check-presets`; `freeze-profile attribute` on a chain-heavy case
before/after + `freeze-profile tune`. Known pre-existing failures (WireframeDepthGraph
blit panic, lut1d white_hot, user_binding reshape, liveschool ableton mapping) are not
regressions.
