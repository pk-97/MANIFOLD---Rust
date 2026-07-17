# Bug Backlog — Closed

<!-- index: Closed bugs archived verbatim from docs/BUG_BACKLOG.md on 2026-07-12 to keep the live tracker readable. IDs are stable and permanent; grep here for the full investigation history of any FIXED/SUPERSEDED bug. -->

Closed entries archived verbatim from `docs/BUG_BACKLOG.md` on 2026-07-12 to keep the live
tracker readable. IDs are stable and permanent — never renumbered on either side of the split.
Grep here for the full investigation history of any `FIXED`/`SUPERSEDED` bug; the live file
keeps a one-line pointer per entry under its own `## Fixed` section.

---

### BUG-001 — Pasting an effect shares the source's `EffectId` — HIGH — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

Copy/paste of an effect card clones the `PresetInstance` verbatim and keeps the original's
`EffectId`. Nothing mints a fresh id. The two cards then share one identity, and the whole
system addresses effects by id with **first-match-wins** resolution, so they collide.

**Root cause**
- Clipboard clones verbatim: [clipboard.rs:32-34](../crates/manifold-editing/src/clipboard.rs#L32-L34) (`get_paste_clones` is a bare `.clone()`; `.clone()` copies the `id` field).
- Paste path 1: [input_host.rs:263-273](../crates/manifold-app/src/input_host.rs#L263-L273) (`handle_effect_paste`) — feeds the clone to `AddEffectCommand`, no `regenerate_id()`.
- Paste path 2: [app_render.rs:1907-1918](../crates/manifold-app/src/app_render.rs#L1907-L1918) (PanelAction paste) — same omission.

**Symptom (user-visible)**
- Move a slider on one card → the other card's value moves too.
- Undo/redo of an edit to one card hits the other (or the wrong one).
- The two cards share GPU/visual state (feedback trails, sim buffers) — see blast radius below.

**Why each symptom happens**
- Edits resolve via `Project::find_effect_by_id_mut` ([project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947)) and `set_base_param_by_id` — first match by id wins, so card B's edit lands on card A.
- Undo/redo commands store an `EffectId` and re-resolve the same way.
- The renderer's per-frame chain rebuild `harvest_state_from` ([preset_runtime.rs:1667-1743](../crates/manifold-renderer/src/preset_runtime.rs#L1667-L1743)) matches cards by first-match `EffectId` (lines 1684, 1697-1701). Two same-id slots in one chain both match the *same* prior slot → GPU node impls + `StateStore` buckets migrate to the wrong/shared card.

**Correct pattern to mirror**
`Layer::clone_with_new_ids` already does this right — it calls `effect.regenerate_id()` on
every cloned effect ([layer.rs:886-900](../crates/manifold-core/src/layer.rs#L886-L900)).
`PresetInstance::regenerate_id` is at [effects.rs:1768](../crates/manifold-core/src/effects.rs#L1768).

**Fix shape**
Call `fx.regenerate_id()` before building the `AddEffectCommand` in both paste paths. Decide
the `group_id` question (see BUG-003) and the carried-binding question (see BUG-004) in the
same pass. Add a paste test mirroring the graph-node one.

**Test:** none yet. Add `effect_paste_assigns_fresh_id` to `manifold-editing`.

---

### BUG-002 — `Clip::clone_with_new_id` doesn't regenerate nested effect ids — MED — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

Same class as BUG-001, one layer down. `Clip::clone_with_new_id` mints a fresh `ClipId` but
bare-`.clone()`s everything else, including `effects: Vec<PresetInstance>`
([clip.rs:105](../crates/manifold-core/src/clip.rs#L105)). So a duplicated clip's effects keep
the **source clip's** `EffectId`s. Clip effects share the same first-match namespace
([project.rs:938-944](../crates/manifold-core/src/project.rs#L938-L944)).

**Root cause**
[clip.rs:168-172](../crates/manifold-core/src/clip.rs#L168-L172) — shallow clone of nested effects.

**Every clip-duplication path inherits it** (all funnel through that one function):
- Paste clip — [service.rs:452](../crates/manifold-editing/src/service.rs#L452)
- Duplicate clip — [service.rs:740](../crates/manifold-editing/src/service.rs#L740)
- Split clip (overlap-driven + explicit) — [layer.rs:616](../crates/manifold-core/src/layer.rs#L616), [SplitClipCommand](../crates/manifold-editing/src/commands/clip.rs#L599)
- Trim / copy-in-region — [service.rs:628](../crates/manifold-editing/src/service.rs#L628)
- Duplicate layer — [layer.rs:871](../crates/manifold-core/src/layer.rs#L871) (clones clips, never touches their effect ids)

**Symptom**
Editing an effect on a duplicated/split clip crosstalks with the source clip's effect.
**Split is the surprising trigger** — a user doesn't think of splitting a clip as
"duplicating," but it produces two clips silently sharing effect ids.

**Scope note:** only bites clips that carry effects (effects usually sit on layers, so this is
the less-traveled path — hence MED, not HIGH). Renderer state does **not** collide across
clips: clip chains have distinct `OwnerKey` per clip ([state_store.rs:30-34](../crates/manifold-renderer/src/node_graph/state_store.rs#L30-L34)), so the model-layer collision is the whole bug here.

**Fix shape**
Make `Clip::clone_with_new_id` deep-regenerate `cloned.effects[*].id` (and clip-effect
`group_id` if any). One function fixes all six entry points, including the layer-dup gap.

**Test:** none yet. Add `clip_clone_assigns_fresh_effect_ids` to `manifold-core`.

---

### BUG-003 — Duplicating a grouped effect leaves `group_id` pointing at the source's group — LOW — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

A pasted/duplicated effect keeps its `group_id`, which still references a group on the
**source's** chain. `Layer::clone_with_new_ids` remaps this for layer effects
([layer.rs:889-893](../crates/manifold-core/src/layer.rs#L889-L893)), but the effect-paste
path (BUG-001) and the clip-effect path (BUG-002) don't. Fixing BUG-001/002 by regenerating
ids must also decide the `group_id` remap, or you trade an id collision for a dangling group
ref.

**Status:** rolled into the BUG-001/BUG-002 fix; tracked separately so it isn't forgotten.

---

### BUG-004 — Effect paste carries Ableton/automation bindings; generator paste drops them — LOW — ✅ FIXED (`2e3dc4f3`)
**Status:** FIXED

Effect paste clones the whole `PresetInstance`, so `ableton_mappings`, `drivers`, `envelopes`,
and `audio_mods` all ride along — a pasted effect ends up mapped to the **same Ableton
control** as the source, and one knob drives both. Generator paste does the opposite: its
`GeneratorSnapshot` carries `drivers` + `envelopes` but **not** `ableton_mappings` or
`audio_mods` ([clipboard.rs:54-95](../crates/manifold-editing/src/clipboard.rs#L54-L95)).

This is an inconsistency, not strictly a crash. Per the effect/generator binding-parity
principle the two paste paths should agree. Decide the intended behavior (most DAWs do **not**
carry hardware/MIDI mappings onto a paste) and make both paths match.

**Status:** design decision to settle alongside BUG-001.

---

### BUG-005 — Macro targets can't disambiguate two same-type effects on one layer — LOW — ✅ FIXED (`9f43f183`)
**Status:** FIXED

`MacroMappingTarget` addresses an effect param by `(layer_id | master, effect_type, param_id)`
([macro_bank.rs:64-82](../crates/manifold-core/src/macro_bank.rs#L64-L82)) — **not** by
`EffectId`. So duplicating an effect (trivially producing two `Blur`s on one layer) makes any
macro mapping to that `(layer, Blur, param)` ambiguous; resolution can't tell the copies
apart. Distinct from the id-collision class (macros are immune to that because they don't key
on `EffectId`), but the same root trigger — duplication — exposes it.

**Fix shape:** address macro targets by stable `EffectId` like single-card edits already do
(`docs/CARD_TARGET_UNIFICATION.md`). Larger than a one-liner; parked here so it's recorded.

---

## Checked and safe (coverage proof)

Audited during the 2026-06-23 duplication sweep; these duplicate correctly. Recorded so the
audit boundary is auditable.

- **Graph-node copy/paste** — `PasteNodesCommand` ([graph.rs:1985-2110](../crates/manifold-editing/src/commands/graph.rs#L1985-L2110)) mints fresh runtime ids + fresh `NodeId`s, remaps internal wires, starts pasted nodes un-exposed. Has regression tests (`paste_node_clones_with_fresh_identity_and_undo_removes`, `paste_remaps_internal_wires_to_the_new_node_ids`). **This is the reference implementation** for the BUG-001/002 fixes.
- **Generator paste** — `PasteGeneratorCommand` overwrites the target layer's single generator in place, addressed by `LayerId`. No id minted, no collision.
- **Markers** — created fresh via `TimelineMarker::new` (fresh `MarkerId`, [marker.rs:20-27](../crates/manifold-core/src/marker.rs#L20-L27)); no copy/paste/duplicate-marker path exists (markers are timeline-level, untouched by layer/clip dup).
- **New-clip-from-scratch paths** (MIDI/percussion/live-trigger/browser-drop) — construct fresh clips, not duplicates of existing ones.

## Blast radius — id-keyed resolvers that a duplicate `EffectId` breaks

All first-match-wins; all used by both editing and undo/redo:
- `Project::find_effect_by_id_mut` — [project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947) (master + layer + clip effects)
- `Project::find_effect_by_id` — [project.rs:711](../crates/manifold-core/src/project.rs#L711)
- `GraphTarget::Effect` / `set_base_param_by_id` paths that wrap them
- Renderer chain rebuild `harvest_state_from` — [preset_runtime.rs:1667](../crates/manifold-renderer/src/preset_runtime.rs#L1667) (per-card GPU state migration)

**Not** in the blast radius: macros (`(layer, type, param)`-addressed — see BUG-005),
markers, generators (`LayerId`-addressed).

## The pattern behind all of this

Duplicating an id-bearing entity must mint a fresh identity for itself **and** every nested
id-bearing child, or id-keyed first-match resolution collides. The graph-node path enforces
this with a test and never regressed; the paths without a test (effect paste, clip clone)
did. The durable fix for the class is a test per duplication path, not a doc note.

Related agent-memory notes: `feedback_hidden_field_dependencies` (the mirror — removing a
field silently breaks identity), and `project_invariant_audit` (its "Positional identity"
category is marked *already fixed*; BUG-001/002 are live counterexamples — correct that claim
when one is fixed).

---

### BUG-006 — Param edits/undo on fused-away nodes silently no-op until an unrelated rebuild — HIGH
**Status:** FIXED (bug/wave1-lane-a-freeze) — reproduced by a unit test on the current tip
(`inner_override_routes_fused_away_node_through_retarget`, bound_graph.rs) and fixed at the
shared home. `BoundGraph` now carries a `fused_retarget` map; the chain builder populates it
from `view.fused_retarget` right after `BoundGraph::new` (preset_runtime.rs ~1398).
`apply_inner_param_overrides` gained a `fused_retarget` param: on a `node_map` miss it routes
each of the fused-away node's params through the retarget onto the live kernel's uniform field
(`n{i}_field`), the same repoint the card + user bindings already take. Empty on an unfused
view (live editor path) → the fast per-node `node_map` hit is unchanged. Verified for float
fields (the pinned `gain.gain` case). **Residual, unfixed:** (a) the fused SEGMENT in-place
override path is still no-op — the segment `node_map` is `c{i}.`-prefixed while the per-card
def is unprefixed, so BOTH surviving and fused-away nodes miss (logged BUG-111); (b) an
enum-typed fused field is written as `ParamValue::Enum(idx)` rather than the binding path's
int-rounded float — harmless for the common float case, worth confirming if an enum param ever
sits on a fused-away node.

**Root cause** — [bound_graph.rs:114-133](../crates/manifold-renderer/src/node_graph/bound_graph.rs#L114-L133):
`apply_inner_param_overrides` looks each node's `node_id` up in `slot.node_map` and silently
`continue`s on a miss. For a fused card, `node_map` is built from the FUSED def
([preset_runtime.rs:1285-1288](../crates/manifold-renderer/src/preset_runtime.rs#L1285-L1288)),
so fused-away members (e.g. `gain`) aren't in it. The path never consults the fused view's
`fused_retarget` map (which knows `gain.gain` → `fused_region_0.n0_gain`). Value-only edits
bump only `graph_version`, which is deliberately not in `compute_topology_hash`, so no rebuild
fires.

**Symptom** — edit a param in the editor, close it (re-fuses, bakes the value), then Undo
while viewing another effect: the def reverts but the fused kernel keeps rendering the OLD
value indefinitely, until a resize/editor-open/unrelated edit forces a rebuild. Live control
stranded, zero errors. `CHAIN_FUSION_DESIGN.md` §6 already flags this as an open item.

**Fix shape** — thread the fused view's `fused_retarget` into `apply_inner_param_overrides`
(or into `node_map` construction): on a `node_map` miss, translate `(node_id, param)` through
the retarget map to `(fused node, n{i}_field)` and apply there. Test: fuse, value-edit,
assert the fused node's param moved without a rebuild.

---

### BUG-007 — Particle-loop fusion exclusion is blind to configured `node.wgsl_compute` shapes — HIGH
**Status:** FIXED (bug/wave1-lane-a-freeze) — reproduced (`cycle_through_configured_particle_wgsl_compute_is_particle_loop`,
region.rs, loads the real StrangeAttractor sim node) and fixed. `cycle_contains_array`
(region.rs:834) now uses `configured_construct(registry, node)`. Swept the file: also converted
the two sibling holdouts `node_is_buffer_atom` + `region_is_buffer` (~region.rs:1898/1914) and
`input_port_access` (via `wire_coincident_consumed`) — all constructed bare, all the same
blind-spot class (a configured full-kernel `node.wgsl_compute`'s Array output / gather access
only appears after `wgsl_source` is parsed). The test pins the root cause directly: a bare
construct of the sim node reports NO Array output; the configured construct does. Two remaining
bare `registry.construct` sites in region.rs are the `#[ignore]`d audit-diagnostic prints
(`domain_flags` ~2613, `explain_preset` ~2682) — not on the runtime classification path; left as
census-only (they'd under-report configured shapes but only in a manual audit dump).

**Root cause** — [region.rs:834](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L834):
`cycle_contains_array` uses a bare `registry.construct(type_id)` — the ONE hold-out in the
file; every other classification call site uses `configured_construct`, whose own doc comment
states why the bare form is wrong. A full-kernel `node.wgsl_compute` with a
`var<storage, read_write> array<Particle>` output (StrangeAttractor's "simulate" node is a
shipped instance) introspects as the DEFAULT kernel (no Array output) under the bare
construct, so the cycle scan can't see the particle stage.

**Symptom** — a texture atom on a feedback loop whose only Array producer is such a node
passes cut rule 12 and fuses tier-A f16 in-loop, where the bit-exact induction argument does
not hold across a particle/scatter stage (FluidSim precedent: max_abs ~0.73 over ~31% of
pixels). Fused render visibly diverges from the editor.

**Fix shape** — one line: use `configured_construct(registry, node)` in
`cycle_contains_array`. Sweep the file for any other bare-construct hold-outs
(`node_is_buffer_atom` / `region_is_buffer` at
[region.rs:1885-1905](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L1885-L1905)
have the same pattern — audit while there). Test: a loop through a configured wgsl_compute
particle node must classify its texture atoms Boundary.

---

### BUG-008 — Fused buffer region with mismatched array lengths reads out of bounds — HIGH
**Status:** FIXED (bug/wave1-lane-a-freeze) — guard, not refuse. Option (a) "refuse >1 array
external at build_region" was ruled out: the shipped DigitalPlants generator fuses a buffer
region containing `lerp_instance_fields` (two required `Array<InstanceTransform>` inputs), so
refusing would regress a real preset (`digitalplants_buffer_fusion_renders_like_unfused` would
panic on the `.expect("...fuses")`). Instead `generate_fused_buffer` (codegen.rs) now bounds
the 1D dispatch `count` by the SHORTEST array external — `min(arrayLength(&src_0),
arrayLength(&src_1), …)` — since every array external is pre-read at `[idx]`. This matches the
unfused atoms' own `min(a, b, …)` clamp and makes every coincident read in-bounds. Byte-identical
for single-external regions (still `arrayLength(&src_0)`) so the pipeline-cache key of every
existing buffer kernel is unchanged; multi-external regions with equal lengths (all shipped
cases) render identically (`min` of equal lengths). Reproduced by a codegen-text test
(`fused_buffer_region_two_array_externals_bounds_count_by_min`, LerpInstanceFields two-external
region); DigitalPlants proof re-run green. Paired with BUG-011 (output-side tail).

**Root cause** — [codegen.rs:1777-1813](../crates/manifold-renderer/src/node_graph/freeze/codegen.rs#L1777-L1813):
`generate_fused_buffer` anchors the dispatch guard to the FIRST array external's
`arrayLength`, then unconditionally pre-reads EVERY array external at that index. Nothing
anywhere (classify, union, `build_region`, `fused_def_builds`) checks that a buffer region's
array externals agree on length — the tier-6 uniformity gate is texture-only. The unfused
atom (e.g. `LerpInstanceFields`) explicitly clamps to `min(a_cap, b_cap, out_cap)`.

**Symptom** — two array inputs of different lengths fuse; for indices past the shorter
buffer the kernel does an out-of-bounds Metal storage read and writes garbage
instances/particles to the output — silent visual corruption. Shipped presets happen to share
lengths today; user graphs are unprotected.

**Fix shape** — either refuse at `build_region` when a buffer region has >1 array external
(conservative, fail-closed, cheapest), or emit a per-external in-bounds guard
(`idx < arrayLength(&src_e)` with a defined fallback element). Pair with BUG-011.

---

### BUG-009 — Segment "stateless" gate misses StateStore-held scalar state; harvest skip resets it — HIGH
**Status:** FIXED (bug/wave1-lane-a-freeze) — root fix, the truthful signal already existed. The
`NodeRequires`-style flag the fix shape asked for is `EffectNode::requires().state_store`, and it
is already declared true by every one of the six named primitives (each calls
`ctx.state.expect(...)` in `evaluate`, so the executor's provide/withhold contract runtime-enforces
the declaration — a node that read the StateStore without declaring it would panic, never ship).
`def_is_segment_stateless` (segment.rs) now checks `!node.requires().state_store` alongside the
existing `state_capture_input_ports` + `aliased_array_io` gates. No hard-coded exclusion list.
Reproduced by `state_store_scalar_card_is_not_segment_stateless` (a compressor_envelope card is now
ineligible; a pure-pointwise card stays eligible). Also refreshed the stale `NodeRequires` doc
comment ("today only Feedback"). Stricter gate = under-segmenting for stateful cards, which is the
safe direction.

**Root cause** — [segment.rs:153-171](../crates/manifold-renderer/src/node_graph/freeze/segment.rs#L153-L171):
`def_is_segment_stateless` checks only `state_capture_input_ports` + `aliased_array_io`.
Primitives that hold real cross-frame state in the StateStore without declaring either —
`sample_and_hold`, `envelope_decay`, `trigger_ease_to`, `compressor_envelope`,
`envelope_follower_ar`, `inject_burst` — pass as stateless. Segment member slots get
`def_content_key: 0` ([preset_runtime.rs:1105](../crates/manifold-renderer/src/preset_runtime.rs#L1105))
and `harvest_state_from` skips them
([preset_runtime.rs:1693](../crates/manifold-renderer/src/preset_runtime.rs#L1693)), so any
chain rebuild drops their state.

**Symptom** — AutoGain (shipped: `compressor_envelope` next to pointwise atoms) joins a
segment; any rebuild while it's a member — editor open/close elsewhere, an unrelated card
edit, or the fused-segment swap-in itself — resets the envelope: gain snaps to unity, a
visible/audible pop mid-show. Violates the chain-fusion design's own "never resets state"
invariant.

**Fix shape** — the root fix is a truthful statefulness signal: a `NodeRequires`-style
`uses_state_store` flag (or derive it from `ctx.state` usage) that `def_is_segment_stateless`
also checks. Stop-gap is a hard-coded exclusion list, which is exactly the pattern the freeze
module refuses everywhere else — prefer the flag.

---

### BUG-010 — `wgsl_compute` silently dispatches the first of multiple entry points — MED
**Status:** FIXED (bug/wave1-lane-a-freeze) — a new `select_compute_entry(&naga::Module)` helper is
the single authority all three sites now share (introspect for workgroup size, the dispatch-side
`create_compute_pipeline`, and the specialization-variant pipeline — each previously picked
`entry_points[0]` independently). Rule: a single `@compute` entry wins by any name (back-compat);
with more than one, only the entry named `cs_main` is unambiguous, else validation fails with the
warning the module doc always promised. Refreshed the stale module doc (lines 29-31). Two repro
tests: a stray leading `@compute fn debug_pass` no longer steals introspection (cs_main's workgroup
[16,16,1] lands, not debug_pass's [8,8,1]); two entries with no cs_main now set `compile_failed`.

**Root cause** — [wgsl_compute.rs:615-624](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L615-L624):
`introspect()` takes `module.entry_points[0]` with no `len() == 1` check (the module doc at
lines 29-31 claims multiple entry points fail validation — they don't). The pipeline compile
independently picks the same first entry. A fragment-form node embeds the author's raw text
BEFORE the synthesized `cs_main`, so any leftover `@compute fn` in the fragment becomes
entry 0 and is what actually runs. Verified empirically by a skeptic (scratch test:
`compile_failed=false`, `debug_pass` dispatched, real kernel never runs).

**Symptom** — a user kernel/fragment with a stray second `@compute` function (debug leftover,
copy-paste) renders stale/blank output with no warning; downstream wires read it as if it
worked. Authoring-time surface, so MED — but it's the exact silent-wrong-output class.

**Fix shape** — in `introspect()`: if the module has >1 compute entry point, prefer `cs_main`
by name; if absent, fail validation with the warning the doc already promises. Keep the
dispatch-side pick in lockstep.

---

### BUG-011 — Fused `@fused_output` buffer sized to max of ALL array inputs, not the member's own rule — MED
**Status:** FIXED (bug/wave1-lane-a-freeze) — paired with BUG-008's guard decision. The
fused-output branch of `array_output_capacity` (wgsl_compute.rs) now returns
`input_capacities.min()`, not `.max()`. This is the exact complement of BUG-008's count guard:
the kernel writes only `[0, min(arrayLength of every array external))`, so sizing `dst` to the
SMALLEST input means there is NO never-written tail — the ghost-instance source is removed at the
allocation, not patched with a zero-fill. (`min` also dominates the "follow input a" member rule
the entry named: with the min-count guard, a is written only up to `min(a, b)`, so `min` is the
tail-free size regardless of which input the member nominally follows.) Equal-length regions
(every shipped buffer preset — DigitalPlants proof re-run green) are unaffected: `min` of equal
lengths is that length. Reproduced by `fused_output_capacity_is_min_of_inputs_not_max`
(mismatched `[10, 4]` inputs → capacity 4, not 10).

**Root cause** — [wgsl_compute.rs:1828-1829](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L1828-L1829):
the fresh-output branch of `array_output_capacity` returns
`input_capacities.max()` generically, overriding the fused output member's own semantic
capacity rule (e.g. `LerpInstanceFields` follows only input `a`). Downstream consumers
(`render_instanced_3d_mesh` computes capacity from physical buffer size) can then draw ghost
instances from the never-written tail.

**Symptom** — with mismatched input lengths (same shape as BUG-008), the fused output buffer
is larger than the unfused chain's, and its tail is uninitialized pooled VRAM — potential
stale-data ghosting across preset/frame boundaries.

**Fix shape** — falls out of BUG-008's decision: if multi-external buffer regions are
refused, this is unreachable; if guarded instead, size `dst` from the anchor external and
zero-fill or guard the tail.

---

### BUG-013 — `commit_and_wait_completed` never checks command-buffer status (likely the GPU-proof flake mechanism) — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Root cause** — [encoder.rs:1655-1662](../crates/manifold-gpu/src/metal/encoder.rs#L1655-L1662):
`waitUntilCompleted()` returns on ANY terminal state including `Error`; no caller checks
`status()`/`error()`. Every heavy freeze proof and `TextureDiff::compare` submit through this
call and read the result back as if it succeeded. Under cross-binary GPU contention
(documented in `.config/nextest.toml` and the `GPU_TEST_LOCK` comment; three call sites build
unlocked devices), a transiently failed buffer reads back stale/partial → spurious large diff.

**Status** — split verdict, judged REAL-as-flake-mechanism: it precisely explains the
observed signature (several heavy tests, random divergence sizes, never reproducing
isolated). It is test-infra, not a compiler miscompile — but it gates trust in the entire
oracle suite, so it blocks using the suite as a hard gate for agent work.

**Fix shape** — check the buffer's terminal status in `commit_and_wait_completed`; on error,
panic in tests (fail loudly, retryable) and log in production. Then re-baseline the flake:
if red runs now report command-buffer errors instead of pixel diffs, the mechanism is
confirmed; if divergences persist with clean status, keep hunting.

**FIXED 2026-07-05** — [encoder.rs](../crates/manifold-gpu/src/metal/encoder.rs) now calls a
`verify_completed()` helper after `waitUntilCompleted()`: if the buffer's status isn't
`Completed`, it reads `status`/`error()` and, in `debug_assertions` builds (tests + dev),
panics with the code+message; in release (the live show) it logs and continues rather than
crash mid-set. The dev-vs-release split via `cfg!(debug_assertions)` gives "loud in tests,
survivable on stage" without a test-only cfg (the helper lives in `manifold-gpu`, whose tests
aren't where the flake showed up). The `GPU_TEST_LOCK` "three unlocked sites" note above was
partly stale: the lock is a `parking_lot` reentrant mutex inside `test_device()`, and every
lib GPU test acquires it; the only unlocked device is the `gpu_proofs` integration binary's
own `GpuDevice::new()`, which runs in a separate process. That cross-process contention is now
self-reporting (a contended failure panics instead of reading stale pixels) rather than
silent, so a dedicated cross-process lock is no longer needed. Landed alongside the GPU-test
`gpu-proofs` feature gate (default `cargo test` is now GPU-free; run `--features gpu-proofs`
to exercise the proofs).

---

### BUG-016 — Imported .glb layers are black boxes: no card params, no Model File picker, edit paths silently no-op — FIXED 2026-07-04 (`2d5e4dc6`)
**Status:** FIXED (2026-07-04)

**Resolution** — PRESET_LIBRARY P0 (D9) shipped: the drop now registers the assembled
graph as a project-embedded preset (`origin: Saved`) and the layer TRACKS it (`graph:
None`); the assembler emits a curated 13-slider card (camera/sun/envmap/per-object
material) with real bindings; the app installs the catalog overlay before the layer is
created, so the process-global preset registry seeds `init_defaults` consistently on both
threads. The `graph_def_mut` override install is deleted. verify-at-impl #4 resolved
(`bundled_preset_json` reads the overlay-merged catalog, no change needed). Assembler +
command tests + GPU render proofs green. **Still owed: the live drag-drop manual gate** in
a running app (card sliders move pixels, editor opens on the cog, save/reload intact) — the
one thing only Peter can eyeball. Original analysis below for reference.

**Root cause** — the glTF Stage-4 install mints a preset id that resolves in no catalog and
stashes the def only on the layer
([app_lifecycle.rs:506](../crates/manifold-app/src/app_lifecycle.rs#L506),
[layer.rs:100](../crates/manifold-editing/src/commands/layer.rs#L100)). Every type-keyed
surface then fails independently: the assembler emits empty `params`/`bindings`
([gltf_import.rs](../crates/manifold-renderer/src/node_graph/gltf_import.rs), metadata block)
so the card is empty; generator string params are sourced from the registry only
([inspector.rs:2251](../crates/manifold-app/src/ui_bridge/inspector.rs#L2251)) so the Model
File picker never shows; the editor's catalog default is `None`, which gates several edit
dispatch arms into silent no-ops (e.g. [app.rs:1356](../crates/manifold-app/src/app.rs#L1356)).
The reported empty editor canvas is NOT fully root-caused: `GraphSnapshot::from_def` on the
assembled def is proven good (12 nodes / 10 wires), so the entry path loses the watch target —
observe at repro.

**Fix shape** — `PRESET_LIBRARY_DESIGN.md` P0 (D9): the drop registers an `EmbeddedPreset`
and the layer tracks it; assembler emits curated performance bindings. Not per-consumer
fallbacks.

---

### BUG-017 — `docs_index_is_in_sync_with_docs_dir` red on main: two design docs never regenerated the index — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Symptom** — found 2026-07-04 running the full workspace sweep for the automation-P4
landing (unrelated to that work — pre-existing on origin/main before the landing branch
touched anything, confirmed via `git show 90ab8531:docs/README.md`).
`cargo test -p manifold-core --test docs_index_sync` fails:
`docs/README.md is out of sync with docs/. Missing from the index: ["AUDIO_SENDS_UX_DESIGN.md",
"TIMELINE_INGEST_DESIGN.md"]`.

**Root cause** — two sessions added design docs (`AUDIO_SENDS_UX_DESIGN.md`,
`TIMELINE_INGEST_DESIGN.md`) without re-running the generator afterward.

**Fix shape** — mechanical: `python3 scripts/gen_docs_index.py`, commit the regenerated
`docs/README.md`. Not fixed this session because other sessions were actively adding more
docs concurrently — regenerating now risked going stale again within the hour. Whichever
session next touches `docs/` and finds the tree quiet should run the generator and close
this out.

**Fixed 2026-07-05** — regenerated while adding `VERIFICATION_DEBT.md` (orchestration-quality
pass); `cargo test -p manifold-core --test docs_index_sync` green, 103 docs indexed.

---

### BUG-018 — `node_graph::catalog_gen::tests::regenerates_in_sync` red on main: `docs/node_catalog.json` stale against the node registry — LOW
**Status:** FIXED @ `38ec595f` (2026-07-12, Fable, `feat/cinematic-camera-designs`) — regenerated per the test's own instruction; the diff was the stale ApricotBloom `wireAmount` card entry (scene-3 morph revert leftover), inspected before overwrite per the note below. Verified: `catalog_gen` tests 4/4 green in the worktree post-fix (this was main's sole red in a 3089/3090 pre-fix sweep); the landing sweep is the full re-proof.

**Symptom** — found 2026-07-04, same full-workspace sweep as BUG-017, same shape: confirmed
pre-existing on origin/main (`90ab8531`) before the automation-P4 landing branch touched
anything — reproduced standalone in a disposable worktree at that exact commit.
`cargo test -p manifold-renderer --lib node_graph::catalog_gen::tests::regenerates_in_sync`
fails with `docs/node_catalog.json is stale`.

**Root cause** — not investigated; some session added/changed a node-graph primitive without
re-running `cargo run -p manifold-renderer --bin gen_node_catalog` afterward. Given `node_count`
sits at 214 in the checked-in file, worth diffing against the live-generated output to see
which node(s) are missing/changed before just overwriting.

**Fix shape** — mechanical: `cargo run -p manifold-renderer --bin gen_node_catalog`, commit
the regenerated `docs/node_catalog.json`. Same reasoning as BUG-017 for not fixing it this
session (unrelated to the work at hand, and worth doing once rather than mid-churn).

---

### BUG-022 — Main-window browser popup: Escape while the search field is focused cancels the text session but leaves the popup open — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Resolution** — applied the documented fix shape: in the main-window `text_input.active` Escape arm
(`window_input.rs`), when `field == SearchFilter`, also call
`self.ws.ui_root.browser_popup.handle_escape()` alongside `text_input.cancel()`, mirroring the
editor window's node-picker branch — one press now dismisses both the search field and the popup.
The closed-overlay pump reconciles the already-cancelled session next frame. Compiles + clippy clean.
Owed: the in-app one-press-closes confirmation (headless can't drive it), but the code mirrors the
proven editor branch exactly. Original analysis below.

**Symptom** — found 2026-07-04 auditing `window_input.rs`'s keyboard routing while
implementing `docs/OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`. For the MAIN window (effect/
generator browser), once the search field has focus (`self.text_input.active &&
field == SearchFilter`), every keystroke is intercepted by the `if self.text_input.active { ... }`
block in `window_input.rs` (`primary_keyboard_input`, ~line 1593) before it ever reaches
`UIRoot::process_events`/`route_overlay_event`. Its `Key::Named(NamedKey::Escape)` arm calls
only `self.text_input.cancel()` — it never touches `self.ws.ui_root.browser_popup`. So Escape
while typing clears the search text and ends the text session, but the popup itself stays
open; a second Escape (now routed normally, since `text_input.active` is false) is needed to
actually dismiss it. This is plausibly the exact mechanism behind Peter's original report
("the search and text seems to stay after you search and need to click elsewhere again to
close it properly") — P1's stash-and-drain fix (`TextSessionOwner`/`take_closed_overlays`)
closes the *orphaned-session-after-popup-closes-elsewhere* class, but this is the inverse:
popup not closing when the session ends.

Note the EDITOR window's analogous bespoke branch (`window_input.rs` ~1145, node picker) does
NOT have this gap — its Escape arm already calls `browser_popup.handle_escape()` directly
alongside cancelling the text input (now also wired through `note_overlay_closed_if` as part
of this session's P1 work).

**Root cause** — the main-window `text_input.active` Escape arm was written before the browser
popup existed as an `Overlay`-driven modal; it only ever needed to cancel a plain text field.
Nothing updated it when `BrowserPopupPanel` started hosting a `SearchFilter` session.

**Fix shape** — in the main-window Escape arm, when `self.text_input.field == SearchFilter`,
also call `self.ws.ui_root.browser_popup.handle_escape()` (mirroring the editor's branch) instead
of only `self.text_input.cancel()`. Small, localized to `window_input.rs`'s
`if self.text_input.active` block — no design-doc scope change, since this is a pre-existing
gap outside P1/P2's stated deliverables (which target orphaned-session-on-close, not
missing-close-on-cancel).

---

### BUG-023 — `no_new_raw_color_literals` red on main: real count (201) one above baseline (200) — FIXED 2026-07-05 (in the P6 landing)
**Status:** FIXED (2026-07-05)

**Resolution** — the extra raw literal was localized (not a "prior session" — it was THIS
orchestration's own P5 landing `0d6e857e`): `browser_popup.rs` carried
`const BADGE_TEXT: Color32 = Color32::new(130, 130, 134, 255)` for the origin-badge text,
added by P5 and missed because that phase ran clippy + focused tests but not the
`design_tokens` integration guard. Fixed by tokenizing it into `color::BROWSER_CELL_BADGE_TEXT`
(color.rs is the scan's exempt token home), dropping the counted set back to 200. Guard green.
Lesson for the orchestration: run `-p manifold-ui --test design_tokens` on any phase that
adds UI color, not just clippy. Original analysis below.

**Symptom** — found 2026-07-05 running the full gate for `PRESET_LIBRARY_DESIGN.md` P6
(thumbnails). `cargo test -p manifold-ui --test design_tokens no_new_raw_color_literals` fails:
`Raw Color32::new( count rose to 201 (baseline 200)`. Confirmed pre-existing and unrelated to
P6: re-ran the same scan logic against `git show HEAD:<path>` for every file under
`crates/manifold-ui/src` (a standalone Python re-implementation of `scan()`/`classify()`) and got
201 on HEAD alone, before any P6 edit — the P6 changes to `browser_popup.rs`/`color.rs` net to
**zero** new raw literals (three new cells' worth of `Color32::new(` were added to `color.rs`,
which the scan excludes as the token home, and the matching local consts in `browser_popup.rs`
were pointed at those new tokens instead of a raw literal — no net change to the counted set).

**Root cause** — not investigated; some prior session's commit added exactly one raw
`Color32::new(` line somewhere under `crates/manifold-ui/src` without bumping
`COLOR_BASELINE` in `crates/manifold-ui/tests/design_tokens.rs` (or without using a
`// design-token-exempt:` comment for a genuine one-off). `git bisect`/`git log -S"Color32::new("`
over the file list the scan touches would localize it quickly; not run this session since it's
orthogonal to P6 and risked burning session budget chasing an unrelated one-line drift.

**Fix shape** — mechanical, one of: (a) find the extra raw literal and tokenize it (count back to
200, no baseline change), or (b) if it's a genuine one-off, add `// design-token-exempt: <reason>`
on that line (count back to 200), or (c) bump `COLOR_BASELINE` to 201 if it's accepted debt. Not
fixed this session — the gate confirms the diff at hand is P6-clean; picking apart an unrelated
pre-existing count belongs to whoever next touches `manifold-ui/src`'s colour call sites.

---

### BUG-024 — Generator preset thumbnails render on a WHITE background (unrepresentative) — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Resolution** — root cause was (a) from the suspect list: generators leave their background
transparent (alpha 0), and `readback_tonemapped_rgba8` saved that alpha into the PNG, so viewers
showed the transparent background as white. Fixed by compositing over opaque black in the readback
(`rgb * a`, force alpha 255) — generators produce straight (non-premultiplied) alpha per
[[alpha-standardisation]], so `rgb * a` is the correct over-black composite, and opaque content
(effects, a=1) is byte-identical. Verified by regenerating + Reading the PNGs: StarField now reads
as stars on black, Lissajous as a clean curve on black, Bloom (effect) unchanged and correct.
**Residual (separate, minor):** a few full-frame generators still read low-saturation in their bare
state — Plasma is a grey blob on black (its background is now correct, but its bare/default output
without audio modulation or a colormap param is desaturated). Not the white-bg bug; a per-generator
"bare look" issue, low priority — leave for a thumbnail-polish pass if it matters on the picker.

---

### BUG-024-ORIG — original analysis (Generator thumbnails on WHITE background) — superseded by the FIXED note above
**Status:** SUPERSEDED

**Symptom** — found 2026-07-05 eyeballing the committed `assets/preset-thumbnails/generators/*.png`
after adding warm-up frames (PRESET_LIBRARY P6). Effect thumbnails (rendered over the gradient
fixture) look correct (Bloom reads right). But GENERATOR thumbnails render their content over a
WHITE background instead of the generator's own (usually dark) field: StarField is dark specks on
white (should be bright stars on black); Plasma is a grey blob on white. Warm-up frames (t advances,
state accumulates) did NOT fix it — so this is a render-path issue, not cold-start.

**Root cause** — unknown, not yet diagnosed. Suspects in
`crates/manifold-renderer/src/preset_thumbnail.rs::render_generator`: (a) the `Rgba16Float` render
target isn't cleared to the generator's expected background (black/transparent) before
`runtime.render`, so unwritten/low-alpha regions read as white after `readback_tonemapped_rgba8`;
(b) premultiplied-alpha / straight-alpha mismatch in the readback vs how generators composite
(cf. [[alpha-standardisation]] — compositor is premultiplied, producers aren't); (c) the tonemap
maps the clear/HDR default toward white. The live `GeneratorRenderer` path composites over the
correct background, so comparing its clear/blend setup against this one-shot path should localize it.

**Fix shape** — likely: clear the thumbnail target to the same background the live generator path
uses (black or transparent) before rendering, and match its alpha convention in the readback. Then
regenerate the 46 factory PNGs via `cargo run -p manifold-renderer --bin generate-preset-thumbnails`.
Effects are unaffected. Until fixed, generator thumbnails are present but not visually usable — the
P6 image-cell display infra is correct; the generator render output is not.

---

### BUG-027 — Graph-editor node previews composite on the wrong z-layer vs. node chrome — MED — FIXED 2026-07-05
**Status:** FIXED (2026-07-05)

**Fix** — node previews now draw INLINE via a new `Painter::draw_image_uv` primitive, emitted by
`GraphCanvas::draw_node` right after each node's body, with each node pushed to its OWN increasing
depth band (`CONTENT+1+i`); the renderer's per-depth loop draws that band's rects then its image,
and a node stacked above (higher band) occludes a lower node's preview. Both flat post-pass blits
(live `app_render.rs`, headless `ui_snapshot/render.rs`) are deleted; the live path registers the
rotating atlas front via `UIRenderer::register_external_texture` + a per-cell UV, the harness
registers each node's output texture. Verified: a deterministic depth-band unit test
(`node_previews_render_in_per_node_depth_bands`) proves the occlusion ordering, and a Kaleidoscope
effect-graph PNG confirms real previews render inline correctly. Full default suite green.

---
_Original analysis (kept for the record):_

**Symptom** — reported by Peter 2026-07-05 (screenshot in session transcript): node preview
thumbnails overlap neighbouring nodes inconsistently — a preview (e.g. Luma to Color) draws
OVER another node's body/ports while that node's own chrome draws over the preview, so
stacking order disagrees within a single node pair. Previews look like they live on a
separate layer that ignores node z-order.

**Root cause** — KNOWN (2026-07-05 Opus, deeper read; the earlier "unknown" was wrong). The
node preview thumbnails are NOT part of the depth-ordered chrome render at all — they're a
SEPARATE flat blit pass issued AFTER the whole chrome is composited, in `visible_node_thumbnails`
order (no depth). Both paths do it identically:
- Live app: [app_render.rs](../crates/manifold-app/src/app_render.rs) clears the offscreen to the
  canvas bg (a `clear`, not a drawn rect), renders chrome + black preview-screen placeholders via
  the depth-ordered tree/canvas pass, presents to the drawable, then blits each node's atlas cell
  over the drawable in a final flat loop (~L3668).
- Headless harness: [ui_snapshot/render.rs](../crates/manifold-app/src/ui_snapshot/render.rs)
  `render_graph_to_png` does the same — chrome first, then a `ui-snap-graph-thumbs` blit loop over
  each node's output texture (~L228).
Because every thumbnail is painted after every node body, no node body can occlude a preview, and
a lower node's preview lands over a higher node's body. The reason it's a bolt-on post-pass: the
immediate-mode `Painter` trait (`draw.rs`) has rect/line/text primitives but **no textured-quad
primitive**, so previews couldn't be drawn inline with the node bodies and were blitted separately.

**Repro** — IS headless-reachable (the earlier entry said it wasn't — wrong). `render_graph_to_png`
reproduces the exact flat-blit bug; render two overlapping preview-emitting nodes and the lower
node's thumbnail draws over the higher node's body. That gives a before/after PNG to verify a fix.

**Fix shape** — depth-interleave the previews instead of post-blitting them: add a thumbnail-draw
primitive to the `Painter` trait, have `canvas.render` emit each node's preview inline right after
its body (so occlusion follows node draw order), route it through the existing depth-interleaved
Image pipeline in `ui_renderer.rs` (which already draws per-depth: rects, then images, then text —
needs the rotating node atlas bound + a per-cell UV subrect for the live path; the harness feeds
per-node output textures with full UV), and delete BOTH flat blit passes. Real immediate-mode
renderer change (Painter trait + UIRenderer + canvas render + both blit-pass deletions), but
headless-verifiable. Not a "patch the overlap cases" job.

---

### BUG-028 — File-drop targeting can't read the live pointer during a Finder drag (both AppKit poll sources frozen) — MED — FIXED 2026-07-05 (`wave/timeline-drop`, landed on main 2026-07-05; Peter's live-drag verification still owed)
**Status:** FIXED (2026-07-05)

**Symptom** — dragging an audio file onto an existing audio lane lands it on a NEW lane
instead of the target lane. Verified 2026-07-05 (Peter, live drag test).

**Root cause** — the `DroppedFile` arms in `app.rs` resolve their target from `cursor_pos`,
which winit freezes for the whole drag (its macOS backend implements no `draggingUpdated:`
and emits no `CursorMoved` during a drag session). Both AppKit poll fallbacks were live-tested
and are ALSO frozen during an NSDragging session: `mouseLocationOutsideOfEventStream` and
`+[NSEvent mouseLocation]` both returned byte-identical values across dozens of frames while
the pointer was actively moving. The poll site (`about_to_wait`) runs during the drag, so the
loop isn't starved — the position APIs simply don't update while macOS owns the drag. Polling
is a dead end.

**Fix (as built)** — `crates/manifold-app/src/drag_interpose.rs`: winit's macOS drag
destination is its `NSWindow`'s window delegate (not a view), and that delegate implements
`draggingEntered:`/`performDragOperation:`/etc. but NOT `draggingUpdated:`. At startup we
`class_addMethod` a fresh `draggingUpdated:` onto the delegate's class (returns
`NSDragOperationCopy`) and swizzle the existing `performDragOperation:` (so the drop position
is captured even if the pointer never moves again after entry), both stashing
`[sender draggingLocation]` — converted window-point → view-point (`convertPoint:fromView:nil`)
→ flipped to `cursor_pos`'s logical top-left convention — into a UI-thread-only cell. New
`crates/manifold-app/src/drag_hover.rs` (`DragHoverTracker`) wraps it; all three `DroppedFile`
arms (audio/MIDI, image, glTF) in `app.rs` now read
`drag_tracker.drop_position().unwrap_or(cursor_pos)`. P2 (drop-target ghost): a full-length
translucent preview clip renders on the target audio lane during the drag
(`app_render.rs`, reusing the existing `ClipBody`/`emit_clips`/ghost-alpha pipeline that
in-app clip-move drags already use); the "New lane: ⟨filename⟩" label and a discrete beat-line
for the non-audio-lane case were **not** built — no existing floating-text-over-viewport
primitive to reuse, out of scope for this pass. Overrides TIMELINE_INGEST_DESIGN §2 D1 (see
its §3 for the full poll-failure writeup, now superseded).

**Verification** — clean compile + clippy (`-D warnings`) + full `manifold-app` test suite,
plus 4 new unit tests for the coordinate flip (`drag_interpose::macos::tests`). The one thing
that can't be verified headless: whether `NSWindow` actually forwards `draggingUpdated:` to a
delegate that only gained the method at runtime (documented AppKit behavior, `respondsToSelector:`
is checked per-message — but only a live drag proves it). Gate: drag a Finder audio file over an
existing audio lane → joins that lane at the pointer's beat, ghost clip shows lane+length before
drop; an image drop lands under the pointer.

---

### BUG-029 — `profiling` feature doesn't compile: rotted against the Beats/Bpm newtypes — FIXED 2026-07-06
**Status:** FIXED (2026-07-06)

**Fix** — the three newtype casts (`.as_f32()` / `.0`) applied; `cargo check -p manifold-app
--features profiling` and clippy are clean, default build untouched. Un-parked because the
profiler is the next oracle for BUG-035 (per-frame content-thread phase breakdown, LFO on vs
off). Toggling the perf HUD starts/stops a session when built with `--features profiling`
(input_host.rs `toggle_performance_hud`); sessions land in `profiling_sessions/`. Note: GPU
pass-level numbers are still zero on native Metal (pre-migration profiler) — the CPU phase
breakdown (engine tick / render_content / gpu_poll) is the usable signal.

**Root cause** — the `#[cfg(feature = "profiling")]` blocks in `manifold-app` predate the
`Beats`/`Bpm`/`Seconds` newtype migration and still treat those values as raw `f32`/`u32`.
Three sites: [content_thread.rs:854](../crates/manifold-app/src/content_thread.rs#L854)
(`Beats as u32` — non-primitive cast), [content_thread.rs:988](../crates/manifold-app/src/content_thread.rs#L988)
(`expected f32, found Beats`), and [content_commands.rs:933](../crates/manifold-app/src/content_commands.rs#L933)
(`expected f32, found Bpm`).

**Symptom** — `cargo build -p manifold-app --features profiling` fails with 3 `E0308`/`E0605`
type errors. The default build (profiling off) is unaffected, which is why the rot went
unnoticed — the feature evidently hasn't been compiled since the newtype migration landed.

**Found during** — PARAM_STORAGE P2 (2026-07-05), while compile-checking the profiling path
after migrating its param readout from the deleted positional `param_values` to `ParamManifest`
(that param-side migration is done and correct; these 3 errors are unrelated newtype-cast rot
in the same blocks).

**Fix shape** — wrap each site in the Beats/Bpm accessor instead of a raw cast (~3 one-line
fixes). Unrelated to param storage, so parked here rather than folded into P2.

---

### BUG-032 — glTF import: a model with >2 materials fails to load ("unknown parameter 'pos_x_2'") and renders black — HIGH — FIXED 2026-07-05 (`dc97bbe6`)
**Status:** FIXED (2026-07-05)

> Id note: originally logged as BUG-029 (commit `dc97bbe6`, commit-message and
> the `prove-render-path` memory still say 029). A concurrent PARAM_STORAGE P2
> session independently used BUG-029 for the profiling-compile bug (still Open,
> above) and added BUG-030. To resolve the collision without splitting that
> open sequential pair, this closed entry was renumbered to BUG-032. The
> `dc97bbe6` commit reference is immutable history — this entry is canonical.

**Symptom** — Peter, 2026-07-05: importing `cc0__japanese_apricot_prunus_mume.glb` (4 distinct
materials) produced a black viewport and a repeating log flood: `Generator … failed to load from
def: graph load error: node 4 (node.render_scene): unknown parameter 'pos_x_2'` +
`Generator type … not found in the preset catalog`. Escaped: glTF wave / PRESET_LIBRARY P0 ·
caught-by: **held-out input in the running app** (the VD-003 mesh-snapshot render harness looked
green because it exercises `gltf::import` directly, NOT the production `PresetRuntime::from_def`
load path where the failure lives — a wrong-path verification, see VERIFICATION_DEBT VD-003).

**Root cause** — `node.render_scene` is the first primitive whose PARAM set (not just its ports)
grows with a reconfigure param: per-object transforms `pos_x_N`/`pos_y_N`/… exist only after the
node reconfigures to `objects >= N+1`. The def loader (`graph_loader::instantiate_def`)
snapshotted the declared param surface ONCE at the node's default 2-object count, then validated
every def param against that stale snapshot — so `pos_x_2` (object index 2, present for the
apricot's 4 objects) was rejected as unknown before the node ever reconfigured. The runtime calls
`node.reconfigure(&params)` after every build (graph.rs, snapshot.rs, freeze/region.rs); the
loader was the one path that didn't. mux_texture/multi_blend hid the gap because their reconfigure
grows PORTS (validated at wire time), not params; the azalea dev fixture hid it because it has
exactly 2 objects.

**Fix** — call `boxed.reconfigure(&doc_params)` before the `param_defs` snapshot in the loader
(mirrors snapshot.rs: seed declared defaults, override with doc values, reconfigure). No-op for
static-shape nodes; general across every reconfigure-param node. Verified on the REAL path: the
apricot `.glb` (4 objects) now loads clean through `PresetRuntime::from_def`. Regression tests:
`render_scene_with_three_objects_loads_per_object_transform_params` (synthetic, portable) +
`held_out_gltf_generator_loads_through_from_def` (`#[ignore]`, env-gated on a >2-material `.glb`).

---

### BUG-033 — `ui-snapshot` feature build broken: `manifold_core::effects::resolve_param_in` no longer exists — FIXED (verified in-tree 2026-07-07)
**Status:** FIXED (2026-07-07)

**Fixed note (2026-07-07, timeline-ux pass)** — `lane_param_range` now reads
`param.spec.min/max` directly (interact.rs:497), the broken `resolve_param_in` call is gone,
and the harness builds AND runs on the 2026-07-07 tip (`cargo build -p manifold-app --features
ui-snapshot` clean; all scenes + `--script` flows rendered this session). Fixed by a landing
between 07-05 and 07-07 that didn't close this entry; closing on direct evidence.

**Root cause** — [interact.rs:500](../crates/manifold-app/src/ui_snapshot/interact.rs#L500) (`lane_param_range`, an
automation-lane interact verb) calls `manifold_core::effects::resolve_param_in(&def, fx, param_id)`
to read a param's `(min, max)`. That function/module path is gone after the PARAM_STORAGE
refactor (the range now lives on the `ParamManifest`/spec, not a `resolve_param_in` helper).

**Symptom** — `cargo build --bin manifold --features ui-snapshot` fails with `E0425` (unknown
function) + a knock-on `E0433`. The DEFAULT build is unaffected, so it went unnoticed — but it
means the entire `ui-snap` headless harness (graph/editor/timeline PNG + `--script` driver) can't
compile on trunk. Found 2026-07-05 (Opus) while rendering a BUG-027 verification PNG; worked
around with a temporary local stub (reverted) to get the render.

**Fix shape** — resolve the param spec through the current manifest API and read its min/max
(mirror whatever `lane_param_range`'s live-app equivalent now does). Owner: PARAM_STORAGE P2 (its
refactor moved the range); ~1 site. Unrelated to the LayerId / node-preview work in this session.

---

### BUG-035 (authoring-hitch) — 3D scenes hitch when a camera/light param is animated — FIXED (clip-atlas persist f16 convert moved off the content thread)
**Status:** FIXED @ `55faec0f` (bug-wave lane B, 2026-07-11) — headless before/after `MANIFOLD_RENDER_TRACE` confirms the spike is gone; rig confirmation still owed (not run this session).

**Resolution (`55faec0f`):** the CAUGHT section below pinned the root cause to
`content_pipeline.rs`'s clip-atlas disk-persist debounce calling
`ReadbackRequest::try_read()` on the completed readback — a scalar,
per-pixel, per-channel f16→u8 convert over the full 8192×1152 clip atlas
(9.4M pixels), inline on the content thread, once per
`CLIP_ATLAS_SAVE_DEBOUNCE` (~5s) cycle. Fix: switch to `try_read_packed()`
(plain memcpy, no conversion) and move the f16→u8 convert + per-cell slice
into the existing clip-thumb disk worker thread via a new
`ClipThumbCache::store_atlas()` message — no new threads, no on-disk format
change. The now-dead RGBA8-only persist path (`CacheMsg::Store`,
`ClipThumbCache::store`, `slice_atlas_for_store`) was deleted rather than
suppressed (no-bare-`#[allow(dead_code)]` rule). `gpu_readback::f16_to_f32`
made `pub` for reuse by the worker-side convert.

**Verified at the level that caught it:** a new headless harness
(`crates/manifold-app/src/bug035_verify.rs`, `journey-proofs` feature)
reuses `journey_proof.rs`'s headless `ContentThread` construction, wires a
real clip-atlas IOSurface bridge (`SharedTextureBridge` — a kernel GPU-memory
object, no display needed), and drives the real, unmodified
`ContentPipeline::render_content` (`export_mode = false`, the exact call
`ContentThread::tick_frame` makes every frame) for 900 frames — 3 debounce
cycles — with `MANIFOLD_RENDER_TRACE=1`. BEFORE (`try_read()`, via `git
stash` of the fix on the same harness): `frame=301 ... clip_atlas=690.7ms`,
`frame=631 ... clip_atlas=691.5ms` (dev-profile `cargo test` inflates the
~58ms live-app figure — no vectorization/inlining — but same debounce
cadence, same call site, same root cause). AFTER (`try_read_packed()` +
`store_atlas()`, production 20ms trace threshold): **no `clip_atlas` trace
line at all** across the same 900 frames — the spike drops entirely below
the threshold (a diagnostic run with the threshold lowered to 0.3ms measured
the residual content-thread cost of the now-plain-memcpy at ~11ms, still a
~63× reduction from the removed scalar-conversion cost, and expected to be
far smaller in a release build). Gates: `manifold-app` workspace tests (163
lib + 4 integration, all green), `manifold-renderer --lib` (1020 green, 3
unrelated ignored), `cargo clippy -p manifold-app -p manifold-renderer -- -D
warnings` clean, plus a clippy pass with `--features journey-proofs --tests`
for the new harness module.

**Measurement (2026-07-06, Fable)** — `freeze-profile scene <glb> [param] [frames]` (new bench
arm): drives the production import door (`assemble_import_graph`) + production
`PresetRuntime::render` on the azalea fixture, static params vs `cam_orbit` swept per frame
(the LFO shape), with a convergence gate (async texture decode means the first ~120 frames
render black — un-gated numbers are void) and a sweep-sanity readback (min→mid must change
pixels; min→max on an angle param is a full circle, a no-op).

Results (600 frames/arm, converged, sweep verified live):
- **CPU encode of the whole chain: ~70µs p50, 0.35ms max, zero >1ms frames in 2400** —
  static or animated, 1080p or 4K. The "full-chain re-encode grazes the 16ms deadline"
  hypothesis is off by three orders of magnitude. Incremental command encoding would
  recover ~0.07ms/frame — **do not build it for this bug.**
- **No static-vs-animated delta**: CPU 0.067 vs 0.065ms p50 (1080p); GPU 2.23 vs 2.18ms.
  The graph runtime prices an LFO'd scene identically to a static one.
- Also refuted along the way: there is NO held-when-static gate at the compositor/layer
  level (the occlusion skip is blend-only — content_pipeline.rs "Everything still
  RENDERS"); the static-scene smoothness the original diagnosis leaned on comes from the
  executor's pure-step memo, and render_scene/gltf_mesh_source re-run every frame anyway.
- The mesh re-blit + per-object rebind "smaller shaves" live inside that 70µs envelope —
  not worth building for this bug either.

**Surviving suspects (all app-side, only run when a param animates):** the modulation/LFO
evaluator on the content thread; UI redraw driven by visibly-changing values (inspector
sliders, graph-editor canvas + thumbnail dump_set when the editor is watching); content↔UI
GPU contention (see `ui-present-content-gpu-contention` memory); present/pacing path.

**In-app profiler sessions (2026-07-06, Peter, `meshImportTests.manifold`)** — the hitch is
now precisely characterized: baseline content frame ~0.09ms, with **isolated single frames
of ~59ms (58.6/58.7/59.2), entirely inside `render_content_ms`**, cadence roughly one per
5–6s, present in BOTH the static and the LFO run. LFO/animation is fully exonerated as a
cause (the original framing was wrong — a static scene hitches identically; you just see it
when something moves). The quantized ~59ms magnitude + slow cadence says periodic
maintenance work or a blocking wait inside `render_content_native`, not render cost.
Candidate: `pool.prune_stale(300)` every 300 frames (content_pipeline.rs:1584-1595) — frame
indices of the spikes (900, 1233, 3630) are ≡ 0/33/30 mod 300, consistent if the pool's
counter is offset from the profiler's frame index. Unproven.

**CAUGHT (2026-07-06, MANIFOLD_RENDER_TRACE run)** — five of five spikes land in the
`clip_atlas` section: `clip_atlas=57.9–61.6ms`, cadence ~360 frames, exactly the
CLIP_ATLAS_SAVE_DEBOUNCE=300 cycle. The culprit line is
[content_pipeline.rs:2225](../crates/manifold-app/src/content_pipeline.rs#L2225) —
`clip_atlas_readback.try_read()` on the completed persist readback. `try_read`
([gpu_readback.rs:99-115](../crates/manifold-renderer/src/gpu_readback.rs#L99)) converts
f16→u8 **per pixel, per channel, scalar, on the content thread**, and the clip atlas is
8192×1152 Rgba16Float (75MB, 9.4M pixels) — ~58ms of CPU once per debounce cycle. The
section's "all disk IO is off-thread" claim is true; the CPU conversion before the
hand-off is the stall. (The separate one-off `generators=37.1ms` spike on the first
frame after load is glTF texture/pipeline warm — not this bug.)

**Fix shape (root: no O(surface) CPU work on the content thread)** — switch the persist
path to `try_read_packed()` (plain memcpy, gpu_readback.rs:148) and move the f16→u8
conversion + `slice_atlas_for_store` into the existing clip-thumb disk worker: hand it
(raw bytes, layout snapshot, hashes) and let it slice/convert/store on its own thread.
No new threads, no format change on disk.

**Symptom** — animating a 3D scene's camera or sun/light via LFO produces a slight, visible
hitch — an uneven frame spike, not a clean framerate drop. Reported by Peter 2026-07-05 on
glTF ("glp") scenes; suspected across all `render_scene` / 3D-mesh output. A static 3D scene
is smooth, and the *same* LFO on a 2D effect param is smooth (Peter confirmed 2D is fine).

**Root cause (hypothesis, reasoned from code — NOT yet measured)** — when a layer is dirty
it re-executes its whole effect chain, re-encoding every node's GPU commands into a fresh
command buffer each frame. There is no incremental "encode once, patch the changed uniform"
path. A static scene is held/composited without re-running the chain (this held-when-static
behavior is *inferred* from observed smoothness — the exact gate was not located in code and
should be confirmed during design). An LFO makes the layer dirty every frame, so the full 3D
chain re-runs 60×/s. That re-encode is the suspected fixed per-frame cost that grazes the
16ms deadline on the heavier 3D path while staying invisible on cheap 2D chains.

Confirmed by reading:
- `render_scene` and `gltf_mesh_source` are both non-pure (`PURE` defaults false,
  [primitive.rs:104](../crates/manifold-renderer/src/node_graph/primitive.rs#L104);
  neither overrides it), so the executor's memo-skip
  ([execution.rs:189](../crates/manifold-renderer/src/node_graph/execution.rs#L189)) never
  spares them — they re-run every frame the chain runs. The still-scene savings are NOT at
  the node-memo level.
- Per animated frame `render_scene` recomposes each object's model matrix, rebuilds its
  uniform struct, looks up the pipeline, and re-binds all 8 texture/buffer slots
  ([render_scene.rs:605-680](../crates/manifold-renderer/src/node_graph/primitives/render_scene.rs#L605-L680)),
  and `gltf_mesh_source` re-blits the whole mesh buffer
  ([gltf_mesh_source.rs:213-222](../crates/manifold-renderer/src/node_graph/primitives/gltf_mesh_source.rs#L213-L222))
  even though geometry never changed.
- NOT the freeze compiler: render nodes are `Boundary` (non-fusable) and its recompile keys
  on structural content, "never per frame" ([freeze/install.rs:195-205](../crates/manifold-renderer/src/node_graph/freeze/install.rs#L195-L205)).
  Exposed-param modulation flows as runtime uniforms and never changes the content key.

**Fix shape** — incremental command encoding for the graph runtime: cache a layer's command
buffer and only re-record when the graph *structure* changes, patching camera/light (and
other exposed) uniforms in place between frames. System-wide upgrade (every animated layer
benefits; payoff concentrated on expensive chains — 3D scenes, long stacks, many bindings).
Orthogonal to, and layers on top of, the existing memo system (skips pure nodes) and freeze
compiler (fuses pointwise passes) — an *addition*, not a rewrite. It sits on the hot render
path where a stale-uniform bug becomes the show, so this is HIGH-risk-to-touch. Smaller
shaves that reduce (not eliminate) the re-encode cost: persistent mesh buffer to kill the
per-frame re-blit; trim `render_scene`'s per-object rebind.

**Before building** — confirm the CPU re-encode is actually where the ms go: add per-frame
timing around the 3D chain execution and watch it under a running LFO. Steady ~X ms → render
cost, optimize the render; sawtooth → scheduling/overhead, and incremental encoding is the
fix. (Not run this session — the app isn't headless and Peter didn't want the round-trip.)

**Design owner** — queued to Fable for a proper design doc (`docs/*_DESIGN.md`), per
[[fable-priority-queue]]. Reasoned diagnosis only; verify the measurement first.

---

### BUG-036 (dead-LFO-on-reload) — LFO on an imported-glb generator's card param is dead after project reload; re-importing the same .glb revives it — MED — FIXED 2026-07-06
**Status:** FIXED (2026-07-06)

**FIXED 2026-07-06** — both halves of the fix shape below, plus two siblings the audit
found in the same class:
- **Ordering (root):** `manifold_io::loader` gained `_with` variants that hand the file's
  `embeddedPresets` to an installer BEFORE the typed `Project` deserialize
  ([loader.rs](../crates/manifold-io/src/loader.rs) `EmbeddedPresetsPrePass`); the app
  passes `install_embedded_presets` so the overlay + core registry are populated when the
  V1.4 param loader resolves each instance ([project_io.rs](../crates/manifold-app/src/project_io.rs)).
- **Keep-don't-drop (class-kill):** `build_param_manifest` now only drops an unknown id
  when the template actually RESOLVED and says the id is gone (informed deprecation).
  With no template at all, the entry is kept on a placeholder spec — state is never lost
  to a missing template ([effects.rs](../crates/manifold-core/src/effects.rs)).
- **Sibling 1:** history-snapshot restore/open-copy never installed the snapshot's
  overlay at all (params dropped AND stale overlay left live) — now go through
  `load_project_snapshot_with` + an unconditional overlay install at the
  `apply_project_io_action` seam.
- **Sibling 2:** New Project never cleared the previous project's overlay (fork leak) —
  covered by the same apply-seam install.
Verified against the real repro: `meshImportTests.manifold` loads with all 17 imported
card params present and the saved `cam_orbit` driver resolving; regression test
`crates/manifold-app/tests/project_local_preset_reload.rs` proves both defenses
independently.

**Symptom** (Peter, 2026-07-06, `~/Downloads/meshImportTests.manifold`) — a project saved
with a glb auto-built graph (the `assemble_import_graph` door) reloads fine visually, but
an LFO bound to one of its card params (Camera Orbit) doesn't run. Deleting the layer and
re-creating it by dropping the SAME .glb makes the identical LFO run. So the modulation
path works against a freshly-imported instance and not against the deserialized one.

**Root cause — SMOKING GUN in the 2026-07-06 trace-run log.** On project load, EVERY card
param of the imported preset is dropped at deserialization:
`[manifold-core] dropping unknown param id "cam_orbit" on PresetTypeId(cc0_japanese_apricot_prunus_mume#2) load (no template descriptor, no inline spec)`
— same for cam_dist/cam_fov/cam_tilt, sun_int/x/y/z, metal_0..3, rough_0..3, env_bright.
The LFO is inert because its target param no longer exists in the loaded manifest. The
drop lines appear BEFORE `[presets] merging 4 project generator preset(s)` in the log:
the V1.4 param loader resolves specs against the template registry, and project-local
(imported) preset templates are merged into the registry only AFTER the project's layer
data deserializes — so every param keyed to a project-local preset type resolves to "no
template descriptor" and is dropped. Re-importing works because a fresh import registers
the template first. Almost certainly a param-storage-redesign (landed 2026-07-05)
load-ordering regression, cousin of the known-RED `expose_mirror` test.

**Fix shape** — order the loader so project-local preset templates register before layer
param deserialization; AND (class-kill, per `eliminate-bug-class-at-storage-layer`)
make the loader keep an unresolvable param as an inline spec instead of dropping it —
silent data loss on load is the storage-layer bug class this repo already decided to
eliminate. The drop log line should become a hard test assertion (load the repro project,
assert zero drops).

**Repro** — load `meshImportTests.manifold`, press play: Camera Orbit LFO inert. Delete
layer, drag the .glb back in, rebind: runs.

---

### BUG-039 (saw-rotation-wrap) — Angle params clamp at range ends, so a saw LFO / automation can't drive a smooth full rotation — FIXED 2026-07-11 (wave2 lane D)
**Status:** FIXED

**Symptom** (Peter, 2026-07-06) — binding a saw LFO or an automation ramp to a rotation
param and sweeping 0→360° hitches at the wrap point: the effective value clamps at the
range end instead of wrapping, so continuous rotation — the most common motion move in a
VJ set — can't be played with a saw. Affects default card slider bindings across effects
and generators.

**Root cause:** two independent clamp sites, both `.clamp(min, max)`, neither wrap-aware —
`ParameterDriver` (the LFO) evaluates its own periodic phase safely, but nothing prevented
a trim-overshoot (`trim_max > 1.0`) from producing an out-of-range value that a downstream
clamp would then plateau at the rail; and `AutomationLane::value_at` samples correctly
between authored points but a multi-turn ramp (points drawn past the param's `max`, e.g.
0→720° for two full rotations) got clamped to `max` and held there.

**Fix:**
- `crates/manifold-core/src/effect_graph_def.rs` — added `wraps: bool` (serde default
  `false`) to `ParamSpecDef`. Explicit tag, not inferred from `is_angle` (FOV is
  angle-typed but must stay clamped).
- `crates/manifold-core/src/params.rs` — added `Param::wraps()` accessor (mirrors
  `whole_numbers()`) and the shared `constrain_to_range(value, min, max, wraps)` helper:
  `wraps` params use `min + (value - min).rem_euclid(max - min)`; everyone else keeps
  `.clamp(min, max)`. `rem_euclid` (not `%`) so a downward-sweeping saw lands on the
  correct in-range value instead of a negative one.
- `crates/manifold-playback/src/modulation.rs` (`evaluate_instance_drivers`) and
  `crates/manifold-playback/src/automation.rs` (line ~229, the lane sample write) both now
  route their computed value through `constrain_to_range` instead of a bare `.clamp`.
  Base/undo semantics untouched — the wrap applies to the read-back effective/sampled value
  only, never to the stored automation points or the undo-relevant base.
- Mechanical sweep — ~28 Rust `ParamSpecDef` struct literals got `wraps: false` (compiler-
  forced, no `Default` impl on the struct); 10 preset JSON card params tagged
  `"wraps": true` after auditing every angle/degree-range param across all 49 preset files
  (see full audit + per-param reasoning in the session that landed this): `ChromaticAberration.angle`,
  `ColorGrade.hue`, `ColorGrade.tint_hue`, `DepthOfField.angle`, `Transform.rotation`,
  `BlackHole.rotate`, `BlackHole.roll`, `DigitalPlants.cam_orbit`, `MetallicGlass.cam_orbit`,
  `OilyFluid.hue`. Left unwrapped (clamped-for-a-reason, or a rate/speed dial not a
  position): `BlackHole.tilt`/`DigitalPlants.cam_tilt`/`MetallicGlass.cam_tilt` (±89°-style
  tilt), `DigitalPlants.cam_fov`/`MetallicGlass.cam_fov` (FOV), `StrangeAttractor.tilt`,
  `StylizedFeedback.rotate`, `BlackHole.spin`, `Duocylinder`/`Tesseract`/`Wireframe`'s
  `rotate_*_speed` params, `DigitalPlants.rot_speed`, `FluidSim3D.rotate_x/y/z`,
  `Lissajous.phase_rate`, `Breathe.phase` (a `node.mesh_ramp` spatial reveal threshold, not
  a periodic angle — verified by reading the primitive, not guessed).
- Gate: `constrain_to_range` unit tests (positive/negative saw, offset range, degenerate
  range, no-op in-range) in `manifold-core::params`; driver-wrap + full-cycle-no-plateau
  integration tests in `manifold-playback::modulation`; automation-ramp-wrap tests in
  `manifold-playback::automation`; a `#[cfg(feature = "gpu-proofs")]` render smoke test
  (`crates/manifold-renderer/tests/param_wrap_smoke.rs`) proving `DigitalPlants.cam_orbit`
  and `MetallicGlass.cam_orbit` render identically at their `min`/`max` rails (the
  precondition that makes the wrap seamless on screen).
**Oracle:** rendered a 6-frame saw sweep on `DigitalPlants.cam_orbit` across the wrap
boundary (160°→170°→179°→[wrap]→-175°→-170°→-160°) via `render-generator-preset` — no
visible hitch or snap at the wrap point, the orbit continues smoothly in the same direction.

**Sequencing** — the param-system post-refactor audit this was blocked on (BUG-040) shipped
2026-07-09; this landed unblocked.

---

### BUG-040 (v13-import-migration-drop) — V1.3→V1.4 migration drops positional params of a project-local (imported) generator — LOW (narrow window) — FIXED 2026-07-09 (`wave/param-boundaries-p3`, PARAM_STORAGE_BOUNDARIES_DESIGN.md P3)
**Status:** FIXED (2026-07-09)

**Found** during the 2026-07-06 param-system post-refactor audit (BUG-036 sibling hunt),
by reading `crates/manifold-io/src/migrations/param_storage_v14.rs` — not reproduced on a
real file.

**Mechanism** — the migration maps positional `paramValues` to ids via (a) the instance's
own `graph.presetMetadata.params` order, else (b) the baked `LEGACY_PARAM_ORDER` table.
A TRACKING instance of an imported/forked generator has `graph: None` and its type id is
project-local, so it's absent from the baked table → arm (b) drops the values with the
"not in the baked LEGACY_PARAM_ORDER" warning and the instance loads with template
defaults. The file itself carries the missing order: `embeddedPresets[type].def
.presetMetadata.params`.

**Exposure** — only projects saved between the glTF import door landing (2026-07-04) and
the V1.4 wire landing (2026-07-05) can hold positional params for a project-local type;
anything saved since writes the id-keyed map. The drop is loud (warning), one-time, and
values-only (defaults still load).

**Fix shape** — in `param_storage_v14`, between the per-instance-graph arm and the baked
table, consult the project tree's own `embeddedPresets` for the type's
`def.presetMetadata.params` order (pure `Value → Value`, self-contained in the same
file). Unit fixture: positional generator instance + matching embedded preset.

**Fixed** — `positional_ids` (`crates/manifold-io/src/migrations/param_storage_v14.rs`)
gained a new arm ("case 1.5") between the own-graph arm and the WireframeDepth/baked-table
arms: a generator with no own `graph.presetMetadata.params` now consults
`embedded_param_orders(root)` — a lookup keyed by each `embeddedPresets[i].def
.presetMetadata.id`, built once (read-only) before the mutable per-instance walk — before
falling through to `LEGACY_PARAM_ORDER`. Three tests cover it:
`bug040_positional_generator_with_matching_embedded_preset_resolves_by_its_order` (the fix),
`bug040_positional_generator_without_matching_embedded_preset_falls_to_baked_table` (unchanged
drop behavior when no embedded match exists either), and
`bug040_generator_own_graph_order_still_wins_over_embedded_preset` (priority order preserved).
Still pure `Value → Value`; no consult of the live registry added; the baked table is
untouched.

---

### BUG-041 (superflux-glide-fire) — Transients fire continuously through a pure pitch glide — FIXED 2026-07-06 (AUDIO_OBJECT_TRACKING P3)
**Status:** FIXED (2026-07-06)

**Symptom** (found 2026-07-06, mod_harness selftest) — the `dive` scenario (7-voice
supersaw gliding 1200→150 Hz, no attacks anywhere in the signal) lights the Transients
lane continuously in all bands: `docs/evidence/audio_modulation/selftest_dive.png`.
SuperFlux's frequency max-filter exists precisely to suppress pitch slides, and it
works for a single slide — the suspected mechanism is the supersaw's 7-voice detune
beating: per-harmonic amplitude modulation reads as genuine broadband dB flux that a
±1-bin max-filter (at bpo 24) cannot cover. Unconfirmed; needs the parameter sweep.

**Root cause:** unknown — suspects: `MAXFILTER_RADIUS` (1 bin) too narrow for detuned
stacks; `SUPERFLUX_DELTA`/threshold floor too low for dense sustained material
(`crates/manifold-audio/src/analysis.rs`, superflux consts ~line 540).

**Fix shape:** parameter sweep against the harness CSV gates (dive = 0 fires, kicks =
exactly 8, busymix ≥ 7 of 8) — owned by `docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P3,
which carries the full brief. If no sweep point passes, that phase escalates with the
table rather than redesigning the detector.

**Blast radius grew 2026-07-06 (P2):** the false fires also break the D5 ridge
tracker — onset re-acquire (D5 step 4) teleports the tracked pitch on every false
fire, so P2's dive/wobble gates (max Δ 24 st, wobble stddev 7.25 st) are BLOCKED on
this bug. P3's exit gate now includes re-running the P2 gate lines to PASS.

**Fixed 2026-07-06** — root cause confirmed by the P3 parameter sweep (~150 configs):
the adaptive threshold was simply far too permissive for dense sustained material, not
the max-filter width (radius 1/2/3 indistinguishable). `SUPERFLUX_THRESH_FACTOR`
2.0→7.0, `SUPERFLUX_DELTA` 3.0→48.0 (mid-plateau: real kicks survive delta 30–300).
Result: dive/riser/growl 0 false fires, kicks exactly 8, busymix 8, and the P2
tracker gates all PASS (dive max Δ 0.38 st, wobble stddev 7.25→0.32 st) with NO D5
softening needed. ⚠ Sensitivity caveat: tuned on synthetics only — the raised
threshold makes the live Transients feature stricter everywhere; validate soft-onset
material (ghost notes, quiet hats) when Peter’s reference clips arrive.


All five entries below were fixed 2026-06-23, with a test per path:
- BUG-001–004 — commit `2e3dc4f3` (`PresetInstance::duplicated()`, both paste paths, `Clip::clone_with_new_id`, `Layer::clone_with_new_ids`).
- BUG-005 — commit `9f43f183` (macros address effects by `EffectId`; versioned load migration).

The fresh-copy carry-rule (id always fresh; drop Ableton/MIDI + audio mods; drop cross-chain group; keep drivers/envelopes) is settled and lives in `PresetInstance::duplicated()`.

---

### BUG-042 (onset-settle-grab) — Tracker re-acquired garbage pitch during the post-attack settle window — FIXED 2026-07-06 (third design: position-anchored re-acquire window)
**Status:** FIXED (2026-07-06)

**Was:** D5's onset re-acquire teleported to `strongest_peak()` on the fire hop; the
VQT needs ~12 hops to settle post-attack, so the estimate was wrong ~70 ms on EVERY
note. Two prior fix shapes rejected with traces (instant teleport; zero-slack settle
window) — see the design doc P2c record.

**Fix (third design, honoring the measured 3-hop position / 12-hop strength split):**
an onset now OPENS a re-acquire window (CHALLENGE_HOPS long) instead of teleporting.
`pos` holds through the attack (correct for same-pitch re-attacks, the dominant real
case), continuation/takeover keep running (nothing freezes — rejected shape 2's flaw),
and the jump fires on position evidence: SETTLE_STREAK (3, plateau-swept 2/3/4 =
69.2/87.6/86.1) consecutive hops with the memoryless apex parked within MAX_SLEW of
the streak's ANCHOR (anchored, not hop-to-hop — the post-attack splash drifts 1–3
bins/hop and reads hop-to-hop-consistent), PLUS the apex must out-value the held
bin by CHALLENGE_RATIO — the window is an accelerated takeover clock (3 parked hops
instead of 12), never a lowered strength bar (without that clause a warm-up-artifact
fire teleported the dive 19 st onto a fade-in harmonic). Two sibling continuation
fixes shipped with it: super-slew+moving candidates are refused (hold, not
clamp-chase — kills the 7-st gap ring-down drag), and static peaks in the
MAX_SLEW..SLEW_RADIUS dead zone snap (tremolo-trough recovery; the hole the refusal
would otherwise open — wobble regressed 0.34→0.52 st before the snap, 0.39 after).
Also fixed: `gt_notes` claimed a phantom 19th note (synth writes 18) — 26
guaranteed-miss hops in the gate denominator.

**Verified:** notes accuracy 61.9→87.6 (gate 90 still red — the residual is a
DIFFERENT mechanism, filed as BUG-045), notes presence 43.6→100 PASS, octave-jump
gate PASS, all other selftest lines green. Real clips: tears bass (the oracle) 30→5
octave jumps; jumps drop across ~all 25 clips (bad_guy bass 26→13, vocals ~halved);
presence flat-to-up everywhere; apricots bass stays perfect (0 jumps, 0.83).

---

### BUG-043 (deep-bass-floor-anchor) — Tracker anchored at the spectrum bottom on deep sub-bass — FIXED 2026-07-06 (apex-masked salience comb)
**Status:** FIXED (2026-07-06)

**Was:** on real deep-sub stems (bad_guy, apricots bass) the Full/Low tracker sat at
10–18 Hz under the real ~40–80 Hz fundamental for whole clips; presence dark.

**Mechanism (pinned by the `sub` synthetic + column-level breakdown,
`sub_45hz_salience_argmax_on_fundamental_not_subharmonic_ghost`):** BOTH original
hypotheses, coupled. At the transform's bottom octaves the 4096-sample kernels are far
under-Q — a 45 Hz peak smears over ~40 bins at >50% magnitude — so a subharmonic
candidate's comb teeth (spaced only 8–14 bins) ALL land inside the one smeared mound:
h3 collects the true peak (ghost), h2/h4 collect its skirt (smear). Measured: S[15 Hz
ghost] 0.70 vs S[45 Hz true] 0.52. The memoryless salience argmax itself was wrong —
upstream of the tracker.

**Fix (at the mechanism):** the harmonic comb reads only spectral APEXES — `salience_into`
masks the column to local maxima ±`PEAK_MASK_RADIUS` (4 = half the minimum tooth
spacing) dilated ±1 bin, so a tooth landing on skirt collects 0. Restores the dominance
property that makes harmonic-sum salience correct: a sub-octave ghost collects each true
harmonic at strictly lower weight than the true fundamental does. Frequency-independent
(no fmin raise; a 22.5 Hz f0/2 ghost of a 45 Hz sub dies the same way).

**Follow-up the mask forced (riser presence-null regressed 100%→14.5%, fixed same
session):** sparse salience gave EVERYTHING neighbourhood contrast, so presence needed
two new multiplicative factors, both constant-free: **dominance** (`S[pos] / window max`
— presence requires being ON the window's dominant object; a tracker parked on residue
reads ~0) and **apex position-consistency** (window argmax within MAX_SLEW of last
hop's argmax — a real object's apex is self-consistent, band-noise's wanders; measured
10–20 bins/hop on the riser vs <0.3 on any real object, at every frequency). Dead ends
measured so they aren't retried: dominance² (pressure-tuned, still 88%), kernel-
normalized mound width (band-noise apex rides narrow chi-square structure — width does
NOT separate noise from tone). Riser's `distinct_full_acquisitions` gate became a
Schmitt counter (light display bar 0.25 / re-arm below 0.02) because presence now
legitimately hovers near the old 0.02 edge-count threshold on noise.

**Verified:** `sub` scenario gates 100%/100%; all selftest lines green except BUG-042's
known-failing notes-accuracy line; 25-clip scan — apricots bass median 66 Hz, 0 octave
jumps, presence 0.83 (was 3-bars-then-collapse); bad_guy/feel/tears/inhale bass at true
36–44 Hz fundamentals, presence 0.52–0.71; vocals/others unchanged. Side effect: notes
presence oracle (BUG-042's) went 43.6%→95.2% PASS; notes accuracy baseline moved
61.9%→56.4% (still the open BUG-042 target).

---

### BUG-044 (mix-trigger-deafness) — Transient detection near-silent on dense full mixes — FIXED 2026-07-06 (novelty-vs-recent-max dual criterion; Sonnet agent build, orchestrator-verified)
**Status:** FIXED (2026-07-06)

**Was:** the adaptive threshold `median(ODF)×7+48` self-raised on dense productions
(continuous broadband change keeps the median elevated) — feel mix 1 Full fire in
11 s (drums stem: 32), apricots 2 (drums: 51), tears halved.

**Fix:** a genuine attack masked by a dense bed is admitted by a second, OR'd
criterion: `candidate > 2.0 × max(ODF over hops t−15..t−7) + 125`. A dense-but-
steady bed cannot inflate its recent MAX to kick size; every BUG-041 false-firer
(dive/riser/growl) spikes continuously so its recent max ≈ its peaks and novelty
never admits it (growl's ODF is a ~5-hop spike train to ~1259 — see observations).
Window excludes the candidate's own VQT-smeared rise (t−6..t) and the previous
16th-note hit (t−16). Median criterion untouched. Constants sit on a measured
plateau (factor ≥ 2.0, δ 48–300 all hold the zero-false-fire guards; sweep table
in the agent report, session 2026-07-06).

**Reproduction first:** new `densemix` scenario (three LFO'd supersaw clusters +
bright noise + 8 kicks; a static detuned bed contributes ~0 ODF — why busymix
never caught this — and the low cluster must sit inside the kick's sweep range).
Entry constants: 4 of 7 catchable kicks = gate FAIL; after: 7/7.

**Verified (orchestrator re-ran):** all selftest lines green (BUG-045's notes
87.6 unchanged, guards 0/0/0, kicks 8, busymix low 8, densemix low 7); feel mix
1→10, apricots 2→31, tears 35→60, inhale 45→58, bad_guy 61→82 Full fires;
on-grid ≥ 96%. Three brief retention caps (bad_guy mix ±30%, feel/apricots drums
±20%) exceeded by 2–3 fires: accepted — the caps were blunt proxies, the added
fires match real-hit magnitude (300–1600) and grid-align equal-or-better than
entry fires, and a six-family feasibility scan showed the caps jointly
unsatisfiable with tears ≥60 under ANY criterion shape. busymix Full went 0→7:
the P3 threshold had been over-suppressing genuine Full-band fires on sparse
mixes too.

**Follow-ups recorded, not done:** (a) consider a busymix/densemix FULL-band gate
once the right bound is understood (kicks full=9 vs low=8 needs explaining
first); (b) vocals stems got notably more sensitive (inhale vocals 29→49) —
plausibly real syllable onsets, no ground truth in the fixture set; check
against Peter's labeled clips when they arrive; (c) growl's spike-train ODF
means any future shortening of the median window below ~2 spike periods
resurrects BUG-041 — greppable warning lives here.

---

### BUG-046 (low-band-kick-deafness-on-mixes) — The canonical Low=kick binding is near-deaf on full mixes with active basslines — FIXED by the dedicated Kick channel (KICK_SWEEP_EVENT)
**Status:** FIXED — resolved by `KICK_SWEEP_EVENT_DESIGN.md` P1/P2/P4/P5 (SHIPPED 2026-07-07): kick-triggering now binds the dedicated ridge-only **Kick** channel (split from Transients), which reads the mix kick's descending FM sweep — the one surviving evidence no flux-family detector could recover — and breaks the bad_guy deafness specifically at equal bass-false-fire cost. Peter confirmed implemented 2026-07-11. The Low band itself still fires on bass-note attacks (the `61c2b0fd` behavior change, kept knowingly) — the fix is the new canonical binding, not a Low-band cure. Remaining: the live feel-pass is the design's P3, owed to Peter and tracked there, not here.

**Found 2026-07-06 (post-BUG-044 measurement, prompted by Peter):** on full mixes,
the Low band catches almost no kicks while Full catches plenty — bad_guy mix Low 6
vs drums-stem Low 46 (mix Full: 82); feel 7 vs 36; apricots 6 vs 13. inhale (29 vs
23) and tears (32 vs 26) are healthy — arrangement-dependent. Peter's use model is
per-band by design (Low = kicks/bass, Mid = vocals/synths, High = hats), so this is
the primary binding for kick-triggering being broken on bass-heavy genres.

**Mechanism (high confidence):** the Low band of a mix is where the sustained,
note-active bassline lives; the kick's low-frequency energy competes with the bass
IN the very band bound for it, keeping that band's ODF baseline (median AND recent
max) elevated. Full recovers kicks via their broadband attack click in mid/high —
which is why mixes fire well on Full but not Low. BUG-044's novelty criterion can't
help: bass notes are themselves novel events in the Low band.

**Fix direction (REVISED 2026-07-06 evening — HPSS-at-the-ODF measured and
exhausted; do NOT re-try it):** the P6a offline campaign
(AUDIO_OBJECT_TRACKING_DESIGN.md D9/P6; instrument kept at
`crates/manifold-audio/examples/hpss_proto.rs`, replica validated
fire-count-exact on all 25 fixtures) swept four causal families — column masks
(flutter manufactures ±59 dB events; growl 16–73 false fires), Wiener (dB flux
is scale-invariant; no effect), dB-novelty-floor replacement (collapses the
adaptive median's context; growl 0→62-73), OR'd floored-novelty (guard-green,
drums retention 1.00, apricots 5→12/13, feel 4→16/35, tears 8→12/25 — but
bad_guy 0→8/45). None reached the ~50% bad_guy bar; not integrated. **Measured
mechanism limit:** in a bass-occupied Low band the mix kick's surviving
evidence is its descending FM sweep (~2 bins/hop, plainly visible in the
bad_guy mix PNG crossing the bassline), which SuperFlux's max-filter nulls BY
DESIGN — no flux-family detector or threshold can recover it. **Successor
direction:** a percussive-sweep EVENT read from ridge motion (D5-tracker-
adjacent; v0 argmax-run prototype confirmed the signal exists but needs real
ridge tracking — apex sticks to the louder bass, bass portamento must be
discriminated by rate/extent, and cross-criterion refractory is needed or
attack+body double-fires). Needs its own short design; re-run the tracker gate
lines (extra Low fires feed D5 step 4 re-acquire). **Partial SHIPPED 2026-07-06 late @ `61c2b0fd`**
(Peter approved; masked-novelty third criterion in `reduce_send`; exact-match
gate vs the prototype 100/100, selftest green minus BUG-045's line): recovery
now apricots 12/13, feel 16/35, tears 12/25, inhale 17/22 — **bad_guy 8/45
keeps this bug OPEN** for the ridge-motion successor. Behavior change shipped
knowingly: Low transients also fire on bass-note attacks now. **Oracle caveat
(Peter, same night):** the drums-stem ground truth is ITSELF unverified
detector output — the bad_guy stem shows ~31 kick sweeps by eye but fired 46
times, so every recovery denominator is suspect until human labels exist. Do
NOT re-litigate recovery percentages against stem fires; the next pass grades
BOTH stem and mix detection against Peter's hand-labeled kick positions
(corpus incoming, Ableton-labeled — doubles as ingest P1's blocker). Also
settled in discussion: on-stage routing (drum bus as its own send) caps this
bug's priority for Peter's own sets — mix-only detection matters most for
finished/other-people's tracks; and the ridge-motion successor should track
the second ridge under the bass (the D5 machinery), not the argmax. Full-band is still NOT a substitute (hats spam —
Peter). Crossover-defaults sweep: independent report-only task; does not
address this bug (kick and bass share bins — re-confirmed at the bin level).

---

### BUG-047 (setup-panel-overflow) — Audio Setup panel content clips past the bottom edge when chrome exceeds viewport − SCOPE_H_MIN — LOW (needs ~18 combined input/consumer rows on one source at full height; ~5 extra rows at a 720px window)
**Status:** FIXED 2026-07-10 (AUDIO_SETUP_DOCK P1, `36a96791`) — the docked panel body is now a `ScrollContainer` (GPU scissor), and `scope_h` is a fixed fraction of the panel rect rather than "absorb remaining space", so control rows overflow into the scroll region instead of clamping the spectrogram at `SCOPE_H_MIN` and running sections past the bottom. See `docs/landings/2026-07-10-audio-dock-p1.md`. (Follow-on BUG-101 tracks the spectrogram blit not yet following the scroll offset.)

**Found 2026-07-06 during AUDIO_SENDS_UX P3 review** (orchestrated wave, found by the
worker's own analysis after an orchestrator-caught clipping defect was root-caused —
the clamp behavior below is the designed residue, not the bug that was fixed).
The panel sizes its spectrogram as `viewport − chrome_height()` floored at
`SCOPE_H_MIN` (200px). When a selected send's Inputs + Consumers rows (28px each)
push `chrome_height()` past `viewport − SCOPE_H_MIN`, the scope clamps at the floor
and the sections below it run past the panel's bottom edge — same visual as the
fixed P3 bug, different cause. **Symptom:** bottom consumer rows invisible on a
heavily-bound source. **Fix shape:** cap the consumers list at N rows + a "+N more"
summary row, or wrap the sections in the existing ScrollContainer (see
`guide_scroll_and_clipping` memory) — a deliberate UX call, not a mechanical fix;
don't improvise it inside an unrelated wave. **Oracle:** `audio_setup_panel.rs`
test `consumers_fit_within_panel_on_first_build_after_configure` guards the fixed
ordering bug; no executable test for this clamp overflow yet.

---

### BUG-051 (trigger-clear-unwired) — `LiveTriggerState::clear()` never called; armed flags survive transport stop — FIXED 2026-07-07 @ 3089e0a3
**Status:** FIXED @ 3089e0a3

**Was:** `LiveTriggerState::clear()`'s own doc said "call on transport stop /
project reset so a stale 'fired, not yet re-armed' flag can't suppress the
first onset next time" (`live_trigger.rs:94-98`), but the engine's only use of
`live_trigger_state` was `.evaluate` in `tick_audio_triggers` — `clear()` had
zero call sites. Narrow in practice (flags re-arm on the next evaluate once
the impulse decays), but a real gap: transport stopping during an impulse
plateau and restarting while the same band was still hot would suppress the
first onset.

**Fix:** `PlaybackEngine::stop()` now calls `self.live_trigger_state.clear()`
directly, and the new `modulation::clear_all_trigger_edges(project)` (§8 P1,
`LIVE_AUDIO_TRIGGERS_DESIGN.md`) walks master effects, layer effects, and
generator instances clearing both the per-instance `audio_trigger.edge` (D2)
and every `is_trigger`-target `ParameterAudioMod.trigger_edge` (D5b) — the two
new §8 edge-state holders, folded into the same reset point per the fix shape
this entry originally specified. Regression proof:
`modulation::tests::clear_all_trigger_edges_rearms_generator_edge`.

---

### BUG-052 (sample-rate-dependent-detection) — onset + kick detection mis-tunes at non-48k sample rates — FIXED 2026-07-07 @ 6e0e8988
**Status:** FIXED @ 6e0e8988

**Fixed:** `SpectrogramConfig::with_time_grid_for(sample_rate)` (manifold-spectral) rescales
`hop`/`n_fft` from the 48k reference so a hop is always ~5.33 ms and the window ~85 ms; the
analyzer applies it at build (`analysis.rs` `StreamingSendAnalyzer::new`). Frequency bins were
already SR-invariant, so nothing there changed. No-op at 48k, exact 2× at 96k. Proven by
`time_grid_holds_hop_and_window_duration_across_rates` across 44.1/48/88.2/96/192k plus the full
manifold-audio analysis suite (46 tests green). **Still owed (VD, cheap):** the end-to-end proof
named in the original gate — resample a fixture to 96k, run the harness, confirm fire TIMES in
seconds match the 48k run. The grid-invariance test makes this belt-and-suspenders, not load-bearing.
Original diagnosis retained below.



**Found 2026-07-07 (Peter's question during the kick-detector discussion).** The audio
analysis runs at the DEVICE'S native rate — `audio_mod_runtime.rs:322` sets the analyzer
rate to `device_rate`, and the resampler only aligns layer audio to that rate, never to a
canonical one. Every timing constant is in HOPS (`ODF_MEDIAN_HOPS`, `ONSET_REFRACTORY_HOPS`,
`KICK_WIN`/`KICK_AGE_CAP`) and every rate constant is bins-per-hop (`KICK_STEP_MAX`, the D5
tracker slew), while a hop is `256/sample_rate` seconds. At 96k a hop is 2.7 ms (half of
48k's 5.3 ms), so the kick detector's "14 bins within a 10-hop window" spans only 27 ms and
the kick's ~90 ms chirp has descended only ~10 bins by then — under the 14-bin threshold, so
**the kick detector goes near-deaf at 96k** (the whole onset analysis mis-tunes, though the
adaptive-threshold/refractory parts degrade more gently). Bins are already SR-invariant
(log-freq CQT anchored at 10 Hz), so `drop_bins` needs no change. Confirmed by arithmetic +
the device-rate code path; NOT observed on a 96k run (Peter: no need to prove in code).

**Fix shape (root, Peter-directed):** normalize the analysis TIME GRID, not the sample rate
(no resampling) — derive `hop` and `n_fft` in samples from the device SR so a hop is always
~5.3 ms and the window ~85 ms (`SR/n_fft` stays 11.7 Hz, so frequency resolution is
unchanged). Then every hop-count and bins-per-hop constant is literally unchanged and
automatically invariant; no fixed ODF ring becomes a Vec. Cost: larger FFT at higher rates
(proportional to the extra data). Rejected alternative: keep hop=256 samples and scale all
hop-COUNTS with SR — more blast radius (dynamic ring sizes) for no gain. Gate: resample a
fixture to 96k, run the harness, confirm fire TIMES in seconds match the 48k run (the eval
already grades in seconds). Fold into the kick time-constant rework.

---

### BUG-055 (eval-harness-stale-time-grid) — both audio eval harnesses used the unscaled default hop on non-48k files — FIXED 2026-07-07 (kick P5 retune branch)
**Status:** FIXED (2026-07-07)

**Symptom:** kick exact-match gate drifted ±1–5 fires per 44.1 kHz drums fixture; mod_harness
CSV `time_s`, PNG bar grid, and printed hop line stretched 8.8% on 44.1k files. **Root cause:**
BUG-052 made the LIVE analyzer's time grid rate-invariant (`with_time_grid_for`), but neither
example harness followed: `hpss_proto::build_clip` hopped 256 native samples, and `mod_harness`
built its own unscaled `SpectrogramConfig` for its feed cadence and time base — so it pushed
256-sample chunks against a 235-sample analyzer hop and sampled `latest()` at the wrong rate,
silently missing/duplicating fires in every per-hop record on 44.1k input. The P2/P4 kick
"exact match" was measured through this sampler. **Fix:** both harnesses now scale their config
(`with_time_grid_for`); `StreamingSendAnalyzer::hop()` accessor added so consumers can't
re-derive it stale; `mod_harness` `debug_assert`s its grid equals the analyzer's. Residual
documented in KICK_SWEEP_EVENT_DESIGN §P5: offline replay vs live stream legitimately diverge
for ridges born during the fade-in (window-fill) region only.

---

### BUG-056 (audio-mixdown-clippy-debt) — `manifold-playback` fails `cargo clippy -D warnings` pre-existing on `audio_mixdown.rs` — LOW (blocks the crate's clippy gate, not correctness)
**Status:** FIXED — verified gone 2026-07-11 (discipline audit): full workspace clippy `-D warnings` green in 9.8s warm, and `audio_mixdown.rs` carries no `#[allow(clippy)]` attrs, so the lints were genuinely rewritten away, not silenced. Almost certainly the P1 offline-export mixdown refactor (`d207f94a`, 2026-07-07) — the last substantive change to the file; not bisected to the exact commit (LOW stakes, dev-tooling only).

**Found 2026-07-07** while gating U-P1 of `LIVE_AUDIO_TRIGGERS_DESIGN.md` §9 (the
`AudioTriggerMod` → `ParameterAudioMod` unification). Not this wave's fault: reproduces
identically on the wave's base tip (`8ccc4fc6`, verified via `git stash` + re-run before
touching anything) — a clippy-version-sensitive lint that was clean at whatever toolchain
last gated `manifold-playback`, now firing on unrelated code:

- `crates/manifold-playback/src/audio_mixdown.rs:589` and `:643` —
  `clippy::cloned_ref_to_slice_refs`: `&[normal_id.clone()]` / `&[analysis_id.clone()]` should
  be `std::slice::from_ref(&normal_id)` / `std::slice::from_ref(&analysis_id)`.
- `crates/manifold-playback/src/audio_mixdown.rs:623` — `clippy::needless_range_loop`: `for i
  in 0..tapped.len()` should iterate `tapped.iter().enumerate()`.

**Fix shape:** three mechanical one-line-ish clippy fixups in `audio_mixdown.rs`, no behavior
change. `cargo test -p manifold-playback --lib` is unaffected (tests still build and pass;
only `--tests -- -D warnings` fails). Not fixed here — out of scope for the audio-trigger
unification and touching `audio_mixdown.rs` wasn't part of this phase's brief.

---

### BUG-057 (ui-snapshot-dead-blit-pipeline) — `manifold-app --features ui-snapshot` fails `cargo clippy -D warnings` pre-existing on an unused fn — LOW (blocks that feature's clippy gate, not correctness)
**Status:** FIXED 2026-07-10 (UI_HARNESS_UNIFICATION P0 landing) — deleted the unused `make_blit_pipeline` from `ui_snapshot/render.rs`; feature clippy now clean. (BUG-067 is a duplicate, closed together; BUG-093 was the same gate's cast debt, also fixed.)

**Found 2026-07-07** while gating U-P2 of `LIVE_AUDIO_TRIGGERS_DESIGN.md` §9 (the trigger-gate
UI unification). Not this wave's fault — `crates/manifold-app/src/ui_snapshot/render.rs`
wasn't touched this session; `git log -S "fn make_blit_pipeline"` shows it landed in an
earlier, unrelated commit (`fea20ade`, "real per-node output thumbnails in the graph scene").
`fn make_blit_pipeline(device: &GpuDevice) -> manifold_gpu::GpuRenderPipeline` at
`ui_snapshot/render.rs:760` has zero call sites under any feature combination — plain
`dead_code`, not a lint-version regression like BUG-056.

**Fix shape:** either delete the function or wire it to its intended call site (unclear which
without reading the surrounding thumbnail-render code, out of scope for this phase). `cargo
build -p manifold-app --features ui-snapshot` and `cargo test -p manifold-app --lib` are both
unaffected (only `clippy --features ui-snapshot -- -D warnings` fails); plain `cargo clippy
--workspace -- -D warnings` (which doesn't enable the feature) stays clean. Not fixed here —
out of scope for the audio-trigger unification and touching `render.rs` wasn't part of this
phase's brief.

---

### BUG-058 (drag-end-consumable) — timeline stuck in move/trim mode: DragEnd is a routable/consumable event, but N independent drag-state owners depend on receiving it — HIGH (live editing gesture wedges; reported by Peter 2026-07-07) — FIXED 2026-07-08 (DRAG_CAPTURE P1–P3)
**Status:** FIXED (2026-07-08)

**Fixed 2026-07-08 — DRAG_CAPTURE P1–P3.** The root fix shipped: single drag-capture ownership (P1 `6e4bddcb`, D1–D4) makes drag-terminal events non-consumable broadcasts routed to the owner by identity, eliminating the eater class (open dropdown/modal can no longer swallow the terminal DragEnd). P2 `12683746` z-aware window seams; P3 `2fc4cfbd` per-widget immediate-drag threshold. The P1 landing also exposed and fixed BUG-075 (dropped terminal DragEnd). Original report retained below.

**Symptom** — sometimes a clip move/trim doesn't release: after the mouse leaves the
timeline (typically onto the inspector) and the button is released, the timeline stays in
move/trim mode (cursor stuck as Move/ResizeHorizontal, next interaction behaves as if the
drag were still live). Self-heals on the next clip press (`on_begin_drag` overwrites
`drag_mode`), which is why it reads as intermittent.

**Root cause (architecture)** — a drag's terminal event must reach every drag-state owner,
but the routing treats it as consumable, first-match-wins. `InteractionOverlay.drag_mode`
(EXCLUSIVE owner of clip move/trim/region state, `interaction_overlay.rs`) is only cleared
by `on_end_drag`, which only runs if the `DragEnd` survives `process_events`' routing
gauntlet (`ui_root.rs:1411` overlay-first) and reaches the tracks-area stash
(`ui_root.rs:1445`). Confirmed eaters:
- `dropdown.rs:695-702` — an open dropdown consumes ALL `DragBegin`/`Drag`/`DragEnd`
  unconditionally (e.g. an accidental two-finger right-click mid-drag opens the clip context
  menu; the release's DragEnd is then eaten by the menu).
- any `Modality::Modal` overlay open at release captures everything, even events it ignores
  (`ui_root.rs:1003-1006`).
The input layer already learned this lesson twice: `859bbceb` made `DragEnd`/`PointerUp`
unconditional at the `UIInputSystem` level, and `process_events` routes
`DragEnd|PointerUp` to the inspector + layer_headers in a dedicated UNCONDITIONAL second
loop (`ui_root.rs:1499-1515`) — but the timeline overlay's DragEnd still travels the
consumable path plus a positional gate (`is_event_in_tracks_area`) patched by a boolean
latch (`overlay_drag_active`). Also unverified at the OS seam: winit-macOS delivery of
`MouseInput(Released)` when the release lands outside the window (inspector is at the right
edge; BUG-028 precedent says winit macOS seams are real). Cheap decisive oracle if the
eater-list explanation doesn't hold: eprintln at four seams (Up received in
`primary_mouse_input` / DragEnd emitted in `input.rs` / stashed in `process_events` /
`on_end_drag` entered), reproduce once, read which link broke.

**Instrumentation SHIPPED 2026-07-07** (`feat/drag-capture-instrumentation`): launch with
`MANIFOLD_INPUT_TRACE=1` and every discrete pointer transition prints a `[input-trace]`
line at each seam — window interceptors, input-system press/release/drag-begin, overlay
routing (which overlay consumed/captured), tracks-area stash gate (with latch state), and
timeline-overlay begin/end (with drag mode). One repro of the stuck state names the broken
link. Root fix is `docs/DRAG_CAPTURE_DESIGN.md`.

**Fix shape (root)** — make drag-terminal events non-consumable broadcasts: every
drag-state owner (InteractionOverlay, inspector, layer_headers, every overlay panel with an
armed drag) receives `DragEnd`/`PointerUp` regardless of routing outcome; overlays may
still *act* on it but never block it. Cleaner still: single drag-capture ownership — at
`DragBegin` one owner is recorded, all subsequent `Drag`/`DragEnd` route to that owner by
identity (not position), killing the latch, the positional gate, and the eater class in one
move.

---

### BUG-059 (band-line-grab-falls-through) — Audio Setup band-divider grabs are sticky, and a MISSED grab silently drags clips/region under the modal — HIGH (silent project edits during calibration; reported by Peter 2026-07-07) — FIXED 2026-07-08 (DRAG_CAPTURE P1–P3)
**Status:** FIXED (2026-07-08)

**Fixed 2026-07-08 — DRAG_CAPTURE P1–P3.** The silent-edit fall-through (the HIGH) was first closed 2026-07-07 by the `swallow_drag` origin-claim stopgap, then superseded by P2 `12683746` z-aware window seams; P3 `2fc4cfbd` added the per-widget immediate-drag threshold for the divider lines (the 4px dead-zone). Root: DRAG_CAPTURE single-owner capture. Original report retained below.

**Symptom** — the horizontal crossover lines in the Audio Setup spectrogram are hard to
grab: fine adjustments stick, grabs sometimes do nothing, and (unreported but confirmed in
code) a missed grab over the timeline area starts an invisible clip move / region select
UNDERNEATH the modal, committing real project edits.

**Root cause (several, stacked)** —
- **Missed-grab fall-through (the HIGH):** the panel's `PointerDown` arm returns `Ignored`
  when the press misses a divider/label (`audio_setup_panel.rs:2269-2294`), and the panel is
  `Modality::Modeless`, so the whole DragBegin/Drag/DragEnd family falls through to the
  layers beneath. The tracks-area stash gate classifies by RAW POSITION with zero z-order
  awareness (`ui_root.rs:2455` `is_event_in_tracks_area`), so a drag starting on the modal
  background over the timeline is stashed and `InteractionOverlay` hit-tests clips by
  position (clips aren't tree nodes) — editing the project through the modal. The `Click`
  arm was already patched to swallow exactly this (`owns_node(id) || point_in_scope(*pos)`,
  the prior fix attempt Peter remembers); the drag family wasn't.
- **4px dead zone:** the global `DRAG_THRESHOLD_PX = 4.0` (`color.rs:837`) applies to a
  precision control — no `Drag` event fires for the first 4px, so sub-4px nudges are
  impossible and every grab starts sticky.
- **Window-seam interceptors punch through the modal:** `primary_mouse_input` checks
  `is_near_split_handle` (6px full-width band at the timeline's top edge) and
  `is_near_inspector_edge` (±4px at `insp.x`, full window height) BEFORE overlay routing and
  BEFORE hit-testing (`window_input.rs:274-310`) — when the centered modal overlaps those
  zones, a press on a band line there is stolen for a panel-resize drag.
- **Dropdown-dismiss swallow:** with any dropdown open (the panel has device/layer
  dropdowns), the next press outside it is consumed by the dismiss branch
  (`window_input.rs:269-273`) — first grab after touching a dropdown always dies.
- **Scope-dark deadness:** `scope_fmin <= 0` (no capture yet) makes dividers ungrabbable by
  design (`audio_setup_panel.rs:422`) — reads as "sometimes it just doesn't work" if lines
  are visible before audio flows.
- **First-click-dead — TRACE-CONFIRMED 2026-07-07 (Peter, `MANIFOLD_INPUT_TRACE=1`), the
  dominant "always the second click works" mechanism:** the press arms the band drag and is
  consumed by the panel, but by threshold-crossing the pressed node no longer resolves
  (`DRAG-BEGIN … resolves=false` in the trace; dead and working clicks carried different
  WidgetIds) — and `input.rs` `process_pointer` emits `DragBegin` and every `Drag` ONLY
  while the pressed widget resolves, so the entire motion stream is silently swallowed and
  the armed position-based drag never hears a move. Same disease `859bbceb` fixed for the
  terminal events, unfixed for motion. Probable node-death path (inferred): the panel's own
  consume sets `overlay_dirty` → overlay rebuild between Down and threshold. Root fix:
  `DRAG_CAPTURE_DESIGN.md` D9 (added same day) — unconditional `DragBegin`/`Drag` emission
  with `node_id: Option<NodeId>`, in P1.

**Fix shape** — same root as BUG-058's capture model plus locals: (1) the modeless panel's
`PointerDown`/drag family must swallow anything inside the panel rect (mirror the `Click`
arm) — that alone kills the silent-edit hole; (2) per-widget drag threshold (0 for the
divider lines — arm on press, track raw moves); (3) window-seam interceptors must respect
z-order (both handles already have tree nodes — route them through hit-testing instead of
raw-position pre-checks); (4) hover-glow the grab zone only when actually grabbable
(scope live).

**(1) SHIPPED 2026-07-07 as an explicit stopgap** (`feat/drag-capture-instrumentation`,
superseded by `docs/DRAG_CAPTURE_DESIGN.md`): the panel now claims the whole
DragBegin/Drag/DragEnd family for any drag whose ORIGIN is inside `panel_rect`
(`swallow_drag`), keyed on origin so a timeline drag crossing the panel still passes
through; unit tests `missed_grab_drag_inside_panel_is_swallowed` +
`timeline_drag_crossing_panel_passes_through`. The silent-edit hole is closed; the feel
items (2)–(4) remain for the design.

---

### BUG-060 — Inspector content paints over the footer bar — REOPENED 2026-07-08 (UI_CLIP_AND_Z P1 verified the wrong render path)
**Status:** FIXED @ `39836352` (landed main `cc4eeb37` 2026-07-10, rig-verified by Peter same day; bookkeeping closed 2026-07-12 on Peter's confirmation — the index row's own tail already recorded the fix + the enqueue-time clip class-kill, only this status line was stale)

**REOPENED 2026-07-08 (Peter, on latest main after P1 landed).** Still repros. New observations,
none yet explained:
- Inspector content bleeds below the panel's bottom edge into/over the footer (a stray param-slider
  fragment renders below the footer divider; see the report screenshots).
- **Swapping the inspector tab between Layer and Master clears it.** A full repaint makes it go away.
- It behaves *slightly* differently on generator inspectors (e.g. Plasma + stacked Color Compass)
  than on video-layer inspectors.

**Why the P1 "fix" didn't catch it:** P1's acceptance PNG (`bug060.after.png`) was rendered through
the headless snapshot harness, which draws via `UITree::traverse()` — the tier-sorted, region-clipped
path P1 actually changed. The **live app renders the main window via `panel_cache_info()` +
`UICacheManager`** (`crates/manifold-app/src/ui_root.rs`, `crates/manifold-renderer/src/ui_cache_manager.rs`),
which P1 did NOT change (it was flagged as VD-018). So the region clip was verified in a render path
the performer never sees, and the live path was never checked. The earlier "structural fix, dead by
construction" claim (below) was premature and applies only to `traverse()`.

**Cause: OPEN.** No confirmed root cause — do not assume one from the notes above; investigate the
live path directly. Next `BUG-NNN` for anything discovered en route.

**Investigation 2026-07-08 (Opus, 2nd pass) — tree-geometry cause ELIMINATED on the LIVE cache
path; cause localized below the UI tree, to the cache/dirty or offscreen layer.**

A new CPU test drives the *live* render path (`UIRoot::panel_cache_info` →
`UIRenderer::render_sub_region` → `UITree::traverse_flat_range`), not the headless `traverse()`
P1 checked. It builds the real `bug060` scene through `UIRoot::build()` (the D1 region wrap),
scrolls the inspector to the bottom with drawers open — 648 visible nodes, content raw-extending
to y=2870, **8 nodes straddling the footer edge (y=1180)** — and walks the inspector panel's node
range exactly as the cache manager does. Findings:
- **Zero** inspector nodes — rects OR text — paint below the footer top. The region clip is a
  fixed-bounds ancestor `traverse_flat_range` pre-pushes at *every* scroll offset and tween state
  ([tree.rs:962-977](../crates/manifold-ui/src/tree.rs#L962-L977)), so nothing can geometrically
  escape it. Text is CPU-clipped to `clip_bounds` ([native_text.rs:1121](../crates/manifold-renderer/src/native_text.rs#L1121)).
- The footer's OWN render on the cache path is correct: full-width background `(0,1180 1536×36)`,
  Q/FPS group intact, every node clipped to the full footer rect.
- Test: `crates/manifold-app/src/ui_snapshot/mod.rs` →
  `footer_leak_probe::cache_path_inspector_does_not_paint_below_footer_top` (feature `ui-snapshot`).
  This is the **footer-edge containment test P1 never had** — the P1 test
  (`layer_scroll_clip_prevents_scrolled_columns_painting_over_the_tab_strip`) checked the TOP edge
  (tab strip) via `traverse_range` + `panel.build`, i.e. neither the bottom edge nor the cache path.

So BOTH "inspector escapes its clip into the footer" (P1's claim) and "inspector content bleeds
below the panel" (this reopen's framing) are **disproven on the live path.** The bug is not a
mis-drawn or mis-clipped node. (The prior footer-leak session's own scissor trace agreed;
its "(B) untraced immediate-mode draws overwrite the footer" theory is **unsupported** — the
inspector/footer use no immediate-mode draws, that's the graph canvas only.)

Cache decision path traced (both layers), which explains the triggers:
- **Scroll:** `inspector.take_scrolled_in_place()` → `cm.invalidate_inspector()` *only*
  ([app_render.rs:960-963](../crates/manifold-app/src/app_render.rs#L960-L963)) — no atlas clear,
  footer's atlas region NOT repainted, frozen from the last clear.
- **Drawer expand:** `inspector.drawer_anim_active()` forces `needs_rebuild=true` every tween frame
  ([app_render.rs:2942-2944](../crates/manifold-app/src/app_render.rs#L2942)) → `build()` +
  `cm.invalidate_all()` → whole-atlas clear + all panels repaint (footer included); settles instantly.
- **Tab switch:** structural change → same `build()` + `invalidate_all()` → whole-atlas clear +
  footer repaint. **This is why swapping tabs clears the artifact** — it is a full recomposite.
- Second cache layer: the composited offscreen is re-blitted from cache unless `offscreen_dirty`
  ([app_render.rs:3951](../crates/manifold-app/src/app_render.rs#L3951)), set when any panel node in
  `[0, overlay_region_start)` is dirty ([:3122-3125](../crates/manifold-app/src/app_render.rs#L3122)).
  The atlas incremental sub-region path ([ui_cache_manager.rs:206-248](../crates/manifold-renderer/src/ui_cache_manager.rs#L206))
  repaints only dirty cards with `LoadOp::Load` and clears dirty for the whole inspector range — the
  same shape as **BUG-015** (stale pixels), fixed this week in this same file.

**Honest open gap:** by static reading, *no* write path puts wrong pixels into the footer band —
every UI draw clips at footer_top, and the footer repaints correctly on every clear. Yet the repro
persists and only a full clear fixes it, which is a *caching* signature (Peter's read, well-founded).

**CORRECTION (Peter, 2026-07-08) — the artifact is STALE UI CONTENT, not darkness.** The footer band
retains real UI pixels: UI colours, button / UI-chrome fragments left over from a prior render. It
is **not** a black or clear-colour gap. The prior footer-leak session's atlas dumps that read
"footer-right goes dark, RGB ~9-16" were a **harness failure** (bad atlas readback), not the live
symptom — do NOT cite them as evidence and do NOT chase a clear-colour / un-repainted-transparent
theory. Whatever lands in the footer band is old UI content that a full clear (tab-swap) wipes.

So the cause is a **stale-pixel / dirty-clear** failure the static read doesn't expose: UI content
that was validly drawn once is not cleared/repainted when it should be, so old chrome persists in
the footer band until the next whole-atlas clear. This is squarely the **BUG-015** class (the
incremental cache path leaving stale chrome), in the same file. Leading unverified suspects: (1) the
incremental sub-region path ([ui_cache_manager.rs:206-248](../crates/manifold-renderer/src/ui_cache_manager.rs#L206))
clears dirty for the whole inspector range while a node-index/extent shift leaves some band of
pixels un-repainted; (2) the footer's own prior-state pixels (old button positions/labels) surviving
a footer render that doesn't fully overpaint its rect; (3) another panel whose atlas rect or node
range overlaps the footer band leaving content there. Do not treat any as settled.

**Next step (needs LIVE pixels, not the geometry harness):** dump BOTH the atlas and the composited
offscreen footer band across the exact scroll + drawer + tab-swap repro, and identify *which* UI
content lands in the footer band and on *which* frame it is written-then-not-cleared — plus per-frame
`panel_valid` / `needs_clear` / `has_dirty_in_range(footer)`. That is the only oracle that shows the
stale content's source and the flag values at the instant it breaks. Prior instrumentation exists on
branch `fix/bug-060-footer-leak-trace` (worktree `.claude/worktrees/footer-leak`, env
`MANIFOLD_TRACE_FOOTER_LEAK=1`) but predates P1 AND its atlas readback is suspect (it produced the
bogus "dark" reading) — re-point it at current main and re-validate the readback first. The
`footer_leak_probe` cache-path containment test landed with this pass is the durable regression guard
for the geometry half (it proves the inspector does not geometrically leak, which stands).

**Symptom** (Peter, 2026-07-07; also the prior `f4b895d7` session's subject): with the
audio-mod drawer open on a Clip Trigger row (`is_trigger_gate`), scrolling the inspector to
the bottom paints card content over the footer bar.

**Root cause (two layers, both verified in code):**
1. **The inspector has no pixel clip of its own.** `build_in_rect` creates no
   `CLIPS_CHILDREN` container — the only `CLIPS_CHILDREN` reference in
   [inspector.rs](../crates/manifold-ui/src/panels/inspector.rs) is inside a test helper
   (:2711). Card visibility is managed by layout math, so any node that extends past the
   inspector's bottom edge paints straight into the footer's atlas region. The inspector
   renders AFTER the footer in the atlas panel loop
   ([ui_root.rs:480 `panel_cache_info`](../crates/manifold-app/src/ui_root.rs#L480) order),
   so its spill wins; the footer only repaints when IT dirties, so the spill persists.
2. **The concrete escape:** `build_toggle_trigger_row`'s drawer
   ([param_slider_shared.rs:1532](../crates/manifold-ui/src/panels/param_slider_shared.rs#L1532))
   lacks the `drawer_reveal` clip `build_param_row` has (:2005-2017) — measured ~119.5px of
   unclipped paint below the card frame. A bottom-straddling card with that drawer open is
   exactly the repro.

**Fix (both landed):**
1. `build_in_rect` ([inspector.rs](../crates/manifold-ui/src/panels/inspector.rs)) now mints
   a `CLIPS_CHILDREN` node bounded to `(rect.x, columns_y, rect.width, columns_h)` before
   building either scroll column, and sweeps both columns' scroll clips (and everything
   built under them, `reparent_root_nodes`) under it once both are built — mirrors exactly
   how `ScrollContainer::reparent_content` already sweeps its own column. The pinned macros
   strip and tab strip chrome are built outside this range by construction, per the scope
   call in the original analysis.
2. `build_toggle_trigger_row` ([param_slider_shared.rs](../crates/manifold-ui/src/panels/param_slider_shared.rs))
   gained the same `drawer_reveal: Option<f32>` parameter and mid-tween `ClipRegion` wrap
   `build_param_row` already had — both call sites (effect and generator cards,
   [param_card.rs](../crates/manifold-ui/src/panels/param_card.rs)) now thread
   `self.drawer_height_anim` through it exactly like the slider-row call sites already did.
   `row_drawer_height`/`active_mod_tabs` already computed the correct tween target for
   `is_trigger_gate` rows (§9 folded them into the shared `ModTab::Audio` path) — this was
   pure wiring, not new height math.

**What's independently verified vs. what's inherited from the stated fix shape:** fix #2
(the mid-tween clip) is concretely proven by a new regression test,
`trigger_gate_drawer_tween_clips_midflight` (param_card.rs) — confirmed to fail on the
"builds under a clip region" assertion with the clip logic neutered and pass with it restored.
Fix #1 (the inspector-level container) was implemented exactly per the pre-decided scope and
verified to (a) not regress the `inspector` ui-snap scene (byte-identical render before/after)
and (b) pass the full `manifold-ui` lib suite (646/646) + `clippy -D warnings`. Digging into
*why* the footer-overpaint repro didn't reproduce via a plain headless render even pre-fix
turned up that `ScrollContainer::reparent_content` already sweeps card content under its own
column clip (bounded to the same inspector bottom edge) via `reparent_root_nodes` — so for
content built inside the master/layer scroll brackets specifically, the settled-state escape
this layer targets was already closed by existing infra; the new container is real
defense-in-depth for the "no pixel clip of its own" class as a whole (any future content built
directly in `build_in_rect` outside a scroll bracket), matching the pre-decided scope, but I
could not independently reproduce a settled-state footer overpaint this fix alone closes.
**BUG-071** (new, filed this session) documents a tooling gap that made this harder to verify
than it should've been.

**Verified:** `cargo test -p manifold-ui --lib` (646 passed) +
`cargo clippy -p manifold-ui -p manifold-app --all-targets -- -D warnings` (clean, aside from
pre-existing unrelated warnings). Shipped @ `27557d18` on `fix/bug-060-inspector-clip`.

**Order (Fable, 2026-07-07): fix BUG-060 FIRST, then BUG-015's stale-chrome hole.** The
clip container bounds what the atlas cache can ever be wrong about (no inspector pixel can
land outside its region afterwards), which shrinks the reasoning surface for the BUG-015
fix. BUG-060 is Sonnet-ready and verifiable with the existing headless snapshot tool (the
spill shows in plain full renders — before/after PNG of a bottom-straddling open trigger
drawer). BUG-015's hole is Opus-grade: it sits at the seam between the cache's incremental
path and the frame loop's blanket `clear_dirty`
([app_render.rs:4807](../crates/manifold-app/src/app_render.rs#L4807)), which exists
precisely because leftover dirty flags once defeated the idle fast path — a careless fix
reintroduces that regression; verify by reasoning + a unit test at the
`render_dirty_panels` helper layer (no snapshot can show it).

---

### BUG-061 (slider-reset-per-panel-lottery) — FIXED 2026-07-08 @ 480acf63 — right-click reset works on some sliders and not others; reset is per-panel hand-wiring instead of a slider behavior — MED (live recovery gesture a performer can't trust; reported by Peter 2026-07-07)
**Status:** FIXED @ 480acf63

**Fixed (root):** reset is now the slider's own gesture. Every slider carries a real default
(`BitmapSlider::build` stores it in `SliderNodeIds.default_normalized`; `SliderSpec.default`
threads through `ChromeHost`), and a single generic `PanelAction::SliderReset { snapshot,
changed, commit }` re-dispatches the slider's OWN value-change trio with the default baked
into `changed` — the exact Snapshot→Changed→Commit path a drag uses, so undo behaves like a
drag to that value (each `*Commit` already guards `old != new`, so resetting an at-default
slider is a no-op). One app-side handler recurses the trio (`ui_bridge/mod.rs`); the 7 bespoke
`*RightClick` actions and their duplicated handlers are gone. Covered: effect/generator params,
macros, master opacity, LED brightness, layer opacity, layer audio gain (new), modulation-drawer
sliders (new). Clip slip/loop sliders were already removed in `52920ab6` — deleted their dead
actions/handlers/stubs. Behavior change: a param reset now jumps (the eased snapback lost its
only caller), consistent with a drag. Guarded by per-surface tests that a right-click on the
track resolves to `SliderReset` with the declared default, so no panel can silently opt out
again. Excluded surfaces → BUG-070. Original diagnosis retained below.

**Symptom:** right-click-to-reset-to-default works on effect/generator param sliders, macros,
master opacity, LED brightness, and layer opacity — and silently does nothing on clip
slip/loop sliders, the modulation drawer sliders, and the Audio Setup gain sliders. On stage
this is the recovery gesture (crank a param too far, snap it home in one click); a gesture
that only works on some faders is one you can't use without thinking.

**Root cause (structural, investigated 2026-07-07):** reset is not a slider behavior — it's a
per-panel contract wired twice: each panel registers a bespoke `PanelAction` on the track node
in `register_intents` (e.g. `ParamRightClick` at `param_card.rs:3707`, `MacroRightClick` at
`macros_panel.rs:466`, `MasterOpacityRightClick`/`LedBrightnessRightClick` at
`master_chrome.rs:365-368`, `LayerOpacityRightClick` at `layer_chrome.rs:267`), and
`ui_bridge/inspector.rs` handles each one separately, re-deriving the default. Any panel that
skips or loses either half silently has no reset. Two proofs it's the wiring model:

- **Clip slip/loop is a regression.** Reset shipped in `b78dc9ba` (March), then `52920ab6`
  deleted clip_chrome's legacy event handler during the intent-registry migration and the
  right-click was never re-registered. The app-side handlers still sit dead at
  `ui_bridge/inspector.rs:1017` (`ClipSlipRightClick`) and `:1032` (`ClipLoopRightClick`) —
  no UI code emits those actions anymore.
- **The infra has a vestigial field.** `SliderNodeIds.default_normalized` (`slider.rs:47`),
  commented "for right-click reset", is written once with the *initial* value (not the
  default) and read by nothing — reset was meant to live in the widget and never did.

Drawer sliders (`drawer.rs:331`) and Audio Setup gain sliders never had reset at all. The
graph editor's inline param sliders use right-click for the mapping popover instead —
intentional, but decide explicitly whether that surface keeps the divergent gesture.

**Fix shape (root):** make reset the slider's own gesture. Every slider already has a
value-change path (Snapshot → Changed → Commit, same one a drag uses); give
`BitmapSlider`/`SliderSpec`/`SliderController` a real `default` value and have right-click on
any track synthesize "set to default" through that existing path. Every slider gets reset by
construction, the bespoke `*RightClick` actions and their duplicated app-side handlers
collapse into the generic path, and the clip slip/loop regression fixes itself. New sliders
can't opt out by forgetting. Watch the two non-uniform cases: param-card sliders whose
right-click currently carries `(target, param_id, default)` context, and label/row right-click
menus (`ParamLabelRightClick` etc.) which are context menus, not reset — they stay.

---

### BUG-062 (no-forward-version-guard) — an older build opening a newer .manifold silently strips unknown fields/effects and saves the loss back — HIGH (latent; becomes live the day two builds coexist)
**Status:** FIXED @ 1e349bf5

**Fixed 2026-07-09 — PROJECT_FILE_INTEGRITY P2.** A forward-version guard now runs at the top
of `load_project_from_json_with`, before migrate: the file's `projectVersion` is compared to
the single-source `CURRENT_PROJECT_VERSION` const, and a newer file is refused with
`LoadError::TooNew` — the Ableton-style message "This project was saved by a newer version of
MANIFOLD (project format X) than this build can open (Y). Update MANIFOLD to open it.",
surfaced through the existing load-error modal (no app change). A coarse secondary guard
refuses a newer archive `format_version`. No unknown-field round-trip (deferred by design D1 —
refusal is the honest fix while an old build can't render missing effects). See
docs/PROJECT_FILE_INTEGRITY_DESIGN.md.

**Found 2026-07-07 by the PROJECT_IO_MAP read (docs/PROJECT_IO_MAP.md §9 E1).**
`migrate_if_needed` (migrate.rs:5) only gates on `is_version_less_than` — there is no check
that the file's `projectVersion` is ≤ the build's ceiling (`Project::default` stamps
`"1.11.0"`, project.rs:1467). A newer file runs zero migrations, serde's
ignore-unknown-fields default drops every field the older binary doesn't know,
`strip_unknown_effects` (loader.rs:188) deletes newer effect types, and the next manual save
or 60s autosave writes the stripped project back — still carrying the newer version string,
so nothing ever notices. Scenario: laptop on last release opens the studio machine's current
show file once. **Fix shape:** before the typed deserialize, compare the file's
`projectVersion` against a build-version ceiling constant; refuse with a dialog (or open
read-only with autosave disabled). One constant + one comparison + one alert.

---

### BUG-064 (save-rename-before-fsync) — V2 save renames before fsyncing the temp file — MED (power-loss window replaces a good save with a torn one)
**Status:** FIXED @ 050e3fd7

**Fixed 2026-07-09 — PROJECT_FILE_INTEGRITY P1.** `save_v2_archive` now captures the `File`
returned by `zip.finish()` and calls `file.sync_all()` before the atomic rename (the existing
parent-directory fsync stays). Contents are durable *then* the durable rename points at them.
Verified at L1 (code inspection + a negative gate asserting two `sync_all` calls); power-loss
durability itself isn't unit-testable without fault injection — carried as a VERIFICATION_DEBT
line.

**Found 2026-07-07 by the PROJECT_IO_MAP read (§9 E3).** `save_v2_archive` (archive.rs:196)
writes the zip to a temp file, atomically renames it over the archive, then fsyncs the
parent directory — but never calls `sync_all()` on the temp file itself; `zip.finish()` only
flushes userspace buffers. On power loss the rename metadata can be durable while the file's
data blocks aren't: a correctly-named `.manifold` full of garbage that has already replaced
the previous good save (history blobs included — they live in the same zip). Venue power is
exactly the environment GIG_RESILIENCE_DESIGN plans for. **Fix shape:** one line —
`file.sync_all()` between `zip.finish()` and the rename (keep the File handle or reopen the
temp path).

---

### BUG-065 (24-bit-snapshot-hash) — save dedup and history identity key on 6 hex chars of SHA-256 — LOW probability / HIGH cost
**Status:** FIXED @ 050e3fd7

**Fixed 2026-07-09 — PROJECT_FILE_INTEGRITY P1.** `compute_hash` now returns 64 bits (16 hex
chars) instead of 24. Backward-compatible: old 6-char history entries keep their names and
copy forward untouched; the manifest's `current_hash` transitions on the next save; a
mixed-width archive's worst case is one *skipped* dedup (a redundant save), never a *wrong*
one.

**Found 2026-07-07 by the PROJECT_IO_MAP read (§9 E4).** `compute_hash` (archive.rs:289)
truncates SHA-256 to 24 bits for both the "no changes detected → skip save" dedup
(archive.rs:89) and `history/<hash>.json.gz` snapshot identity. A dedup collision silently
skips a real save; a history collision makes restore return the wrong snapshot. ~7×10⁻⁵ per
50-entry project lifetime — small, but the failure is silent data loss on the one
unrecoverable asset. **Fix shape:** widen to 16 hex chars (64 bits); entry names stay short,
old 6-char history entries stay readable (identity is string equality against the manifest,
so mixed-width archives keep working as saves roll over).

---

### BUG-066 (fluid3d-corner-drift) — FluidSim3D density herded into the top-right octant — FIXED (partial-volume dispatch; class killed via shared `VOLUME_WORKGROUP_3D`)
**Status:** FIXED — root cause found + fixed 2026-07-10 16:51 @ `eebac94d` (on main); Peter confirmed the artifact gone on the rig 2026-07-11.

**Resolution (`eebac94d`):** `node.edge_slope_3d` and `node.swirl_force_3d` sized their dispatch grids `div_ceil(8)` (the legacy hand shaders' 8×8×8 workgroup), but the freeze codegen emits every 3D-volume kernel at `@workgroup_size(4,4,4)` — so only the (0..64)³ octant of the 128³ volume ever received forces, and under the default camera that octant projects exactly to the top-right screen quadrant. Every observed symptom follows: sharp axis-aligned box edges (the dispatch cutoff), same position every boot, independent of seed/container/camera orbit. Kernel parity tests couldn't catch it — they dispatch the generated kernel with the test's own grid. The fix removes the class, not the instances: codegen exports `VOLUME_WORKGROUP_3D`, all four volume-node `run()`s size their grids from it, and a unit test pins the emitted `"4, 4, 4"` workgroup string to the constant so emission and host dispatch can't drift silently again. Verified headless on Peter's exact repro settings (uniform centred cloud at frame 900 vs. the pre-fix parked box); `d401e202` the same day restored the toroidal blur + legacy curl axis. **Timeline note:** the "root cause NOT found" status correction was authored 15:03 that same day and merged to main 2026-07-11 morning — it predates the 16:51 fix. Read the addenda below as the falsification record that narrowed the search to this, not as the current state.

**Found 2026-07-07 by Peter on the live output (subtle top-right dominance, no container and
cube container), bisected headless the same session.** Harness:
`crates/manifold-renderer/tests/fluid3d_bias.rs` (gpu-proofs, `--ignored`) — renders the
bundled preset 900 frames per scenario, prints per-quadrant luminance shares, dumps PNGs to
`/tmp/fluid3d_bias/`. Scenario matrix is edit-and-rerun (~12s/scenario at 512²); it injects
card params past their UI ranges (e.g. `curl 0`/`90`, `flow` sign flip), which the UI can't.

**Established (all at 512², cube container, deterministic):**
1. Baseline is clean: with turbulence=0, curl=0, flow=0 the steady state is symmetric
   (≈25% per quadrant) — spawn, NGP scatter, resolve, integrator, container repel/bounds,
   camera splat are not the bias.
2. **Turbulence is a wandering tide** (largest contributor at defaults): the 3-plane 2D
   simplex in `simplex_noise_force_3d_at_particles_body.wgsl` spans only ~2 lattice cells
   across the volume (`noise_pos = pos * 2.0`), so its instantaneous volume-mean is a real
   net force (measured ±0.05/axis in a CPU replica; drifts over ~30–60s as
   `noise_time = time*0.1` scrolls). Whole fluid leans on one wall, wall changes slowly.
   Fix shape: make the per-axis noise zero-mean over the volume (subtract the analytic/
   sampled mean, or raise frequency + add octaves — changes the look, needs Peter's eye).
3. **Slope force has a systematic diagonal drift — root cause OPEN.** slope-only (turb 0,
   curl 0, flow −0.01 default) pools 33–37% of luminance in the top-right with voxel-aligned
   "shelves"; at feather=40 it's a violent bulk translation (TR 50% by f300, striping against
   the +x face); at feather=4 the bias is gone. Flipping `flow` sign mirrors it to
   bottom-left; rotating the camera 180° mirrors it on screen (so sim-space, +x/+y-ward).
   The mean slope force over all particles is nonzero — for a pure density-gradient force
   that should be impossible (momentum conservation), so something in
   density→blur×3→gradient→curl_slope→blur×3→trilinear-sample is spatially shifted, and the
   shift grows with blur radius.
4. **Refuted with evidence (don't re-chase):** (a) NGP-deposit/trilinear-read PIC mismatch —
   a matched trilinear (CIC) 8-corner deposit in the scatter body changed the trajectory
   ~0.1% and the bias not at all; (b) Metal 8-bit subtexel rounding of the blur's bilinear
   tap-pair offsets — replacing pairs with exact integer-offset taps changed nothing;
   (c) codegen uv convention — the 3D wrapper uses texel-center `(id+0.5)/dims`
   (codegen.rs:168); (d) resolve index mapping — `id.z*dims.x*dims.y + id.y*dims.x + id.x`
   matches the scatter packing exactly. Also checked clean by read: gradient central-diff
   wrap, container SDFs, euler step, samplers (linear, clamp-to-edge).

**New evidence (Peter, 2026-07-07, after the session):** the pre-decomposition fused
`node.fluid_simulate_3d` (the original Rust generator, before the node-graph migration)
did NOT visibly have this bug. Unverified memory — verify it first (checkout a
pre-decomposition commit, run the harness's slope_only/feather40 scenarios against the old
generator, or eyeball a long run). If it holds, the root is something the decomposition
changed about the COMPOSITION, not the kernels — the per-kernel gpu_tests prove each atom
matches its legacy shader, so the delta must be in: pass ordering / which texture each pass
reads (stale-intermediate suspect, already ranked first below), double-buffering the legacy
frame did that the graph doesn't, blur radius units or pass count, gradient boundary
convention, or preset wiring/param scaling. Corroborating smell: the per-voxel curl wobble
(2211b20f) was added AFTER decomposition to fix "swirl pools in one octant" — pooling the
legacy sim reportedly never had, i.e. the wobble may have papered over one symptom of the
same introduced asymmetry. **This makes the first step a history diff:** `git log -S
fluid_simulate_3d` to recover the fused generator's frame recipe (kernel order, texture
ping-pong, per-pass params), then line it up against the FluidSim3D preset graph's execution
plan and diff the compositions.

**Legacy-composition diff (Fable, same night — step 0 partially done):** recovered the
pre-node-graph Rust generator (`git show 044e7c8a~1:.../fluid_simulation_3d.rs`) and the
fused-primitive era (`git show 50909419~1`). Force math is IDENTICAL through both eras
(same central-diff gradient, same cross(gradient, ref_axis) + slope, no wobble; fused
per-particle kernel = today's chain step for step), so the decomposition did NOT change the
physics. The composition deltas, complete list: (a) blur radius — legacy
`feather × vol_res/640` = 4 at defaults vs preset `feather × 0.2 + 1` = 5
(`blur_radius_scaled`); the harness cliff sits exactly there (radius ~1 clean, 5 visibly
biased, 9 violent); (b) legacy amortized the whole volume pipeline to alternate frames —
half-rate field updates ≈ half-speed drift; (c) legacy blur ping-ponged vol→temp→vol
explicitly (graph allocates per-pass outputs — check the executor actually does the same);
(d) the curl wobble exists only post-decomposition. Net: legacy likely drifted too, ~2–3×
slower (radius 4 vs 5 at the steep part of the response + half-rate updates) — consistent
with \"never noticed\" rather than \"absent\". So Peter's report probably does NOT convict the
decomposition; the drift mechanism itself (why any radius ≥ ~4 drifts diagonally) is still
the open question and the antisymmetry probe remains the way in. Verify (b)/(c) against the
execution plan before trusting this paragraph's \"identical\" claims for the blur pass wiring.

**Next steps (Opus):** the drift needs blur *range*, not blur sampling mode. Candidates, in
order: (0) finish the legacy diff — confirm the graph's blur ping-pong/order matches (c),
and if cheap, run the harness at radius 4 + half-rate to see legacy-equivalent drift speed;
(1) synthetic-volume antisymmetry probe at the kernel level — upload a symmetric
Gaussian density, run blur→grad→(slope)→blur, sample at mirrored probe positions with the
codegen standalone kernels (pattern: `gpu_tests` in scatter_particles_3d.rs), assert
F(p) = −F(mirror p) stage by stage; the first stage that breaks antisymmetry is the bug.
(2) The executor/fusion schedule for the 6-blur chain — if any blur pass reads a stale
(last-frame or not-yet-written) intermediate, symmetric math still yields a lagged, shifted
field; check the plan order + barriers for Render Volume/Force Field groups (this is the one
hypothesis that needs blur *range* to matter and survived every kernel-level check).
(1b) measure the violation directly instead of the drift: the per-particle force array is a
plain buffer — read it back in the harness and sum it. Nonzero mean force = the conservation
break itself, visible in ONE frame (vs 900-frame integration), and nulling force terms
attributes it to a stage instantly. Build this meter first; it makes every other probe 100×
faster. (1c) elimination note: vol_res 128 is a power of two, so blur/gradient coordinates
are EXACT in f32, and the integer-tap experiment made the blur reads exact too — drift
unchanged. The only position-dependent inexact ops left in the loop are the deposit
(refuted via CIC) and the per-particle trilinear read (2b below) — among precision theories,
2b is nearly the last one standing;
(2b) Peter's rounding hunch, sharpened (untested, fits ALL evidence): any precision theory
must round DIRECTIONALLY — symmetric noise can't pick a corner. Prime candidate: the per-particle force read in
`sample_volume_at_particles` — the one filtered read the integer-tap experiment did NOT
touch. Metal's trilinear filter fraction is ~8-bit; IF its rounding truncates (UNVERIFIED —
Apple documents the precision, not the direction; if it's round-to-nearest this candidate
dies), every read carries a fixed ~1/512-voxel shift toward −xyz and the feedback loop
amplifies it, and feather sets the force field's spatial COHERENCE
(radius 1 → decorrelated gradients, biases cancel; radius 5+ → aligned pushes compound) —
which explains the radius cliff without the per-read error changing size. Cheap test:
replace that one trilinear sample with a manual 8-tap textureLoad trilerp in f32 and rerun
slope_only — if the drift dies, root found (and the fix is exactly that manual trilerp);
(3) clamp-to-edge interaction at large sigma: blur clamps while the gradient wraps
toroidally — mixed boundary conventions couple opposite faces asymmetrically once the kernel
reaches the volume edge. Fix at whichever level the probe convicts; then rerun the harness
matrix (slope_only + slope_feather40 must go ≈25% flat) and give Peter a look-pass, since
zero-mean turbulence (item 2) changes the fluid's feel.

**2026-07-10 session (Fable + Peter) — dominant defect FIXED, projection fix REFUTED, meter now in-repo:**

*The force meter (next-step 1b) is built* into `fluid3d_bias.rs`: `set_dump_all` +
buffer readback prints per-axis mean/max force for every per-particle array at
checkpoint frames (note: `Array<[f32;3]>` decodes as three scalar `f32` fields, not
`vec3f`; and the in-place force chain aliases one buffer, so every force-node row
shows the same post-accumulation content — attribute terms via scenario nulls, not
rows). Measured: the slope tide is real — slope_only holds a persistent
+5e-5/axis mean (~0.5% of peak force) for 900 frames; the null control reads ~1e-7.

*Refuted this session, with evidence:* (e) hardware trilinear rounding (2b) — a
manual exact-f32 8-tap trilerp in the sample body left the tide and pooling
unchanged; ALL precision theories are now dead. (f) uniform zero-mean projection as
the fix — built (`node.remove_drift_3d`, three-pass reduce+subtract, GPU-oracle
proven, registered but UNCONSUMED — shelf atom) and wired into the preset: it
*inverted and amplified* the pooling (slope_only TR 37% → BL 46%; no-container BL
63%). The imbalance is spatially concentrated (wall bands), so cancelling its net
uniformly injects a volume-coherent counter-force — coherence is the amplifier.
(g) volume-edge convention mixing (blur clamps / gradient wraps) — refuted by
logic over existing data: no-container puts particles AT the volume edges and
measures clean. Fused vs `MANIFOLD_FREEZE=0` renders bit-identical (weak evidence
against the executor-schedule suspect; the toggle's effect on this path unverified).

*The DOMINANT visible defect was a different bug at a higher level* (Peter's call —
he saw quadrant-structured turbulence on the live output, one cube-shaped region
behaving differently, stable across resolutions): the turbulence noise lattice.
`node.turbulence_3d` sampled its 3-plane simplex at `pos * 2.0` (baked constant) —
~2 lattice cells across the whole volume, so one noise cell reads as a quadrant of
the sim, and a 2-cell field can't average to zero (= the item-2 wandering tide).
**Fixed:** `turb_scale` param (port-shadowed, default 2.0 = legacy so old saves
render unchanged) + "Turb Detail" card param on FluidSim3D (default 8.0). Sweep
(detail 2/4/6/8/12, full defaults, 900f): quadrant anatomy gone from ~6 up,
quadrant shares stable within ~2 points (legacy sloshes 10+), wandering tide 3–10×
smaller. Peter look-pass at the rig owed (default 8 = my eye, not his yet).

*Still open (the original slope tide, now LOW-MED):* root cause of the +diagonal
~0.5%-of-peak slope-force mean. Next instruments, in order: (1) the synthetic
antisymmetry probe (upload a mirror-symmetric density, run blur→gradient→slope→blur
via the standalone kernels, find the first stage where F(p) ≠ −F(mirror p));
(1b) octant-conditioned meter means (10-line harness change) to confirm the
wall-band concentration; (2) unverified hypothesis from this session: an off-center
tap range in `blur_3d_separable` (a `[-r, r-1]`-style kernel = half-voxel shift per
pass, same sign every axis — fits all-axes-equal tide, feather scaling, legacy
drifting slower, and survives parity because the oracle would share the defect).
Read the blur kernel before building anything.

**2026-07-10 part 2 (same session, after Peter's live falsification at flow −0.10 /
feather 43 / turbulence 0 / ctr_scale 1.0 / 2M particles — the TR cube survives the
Turb Detail fix because turbulence isn't even running):**

*Shipped:* **hash-lane decorrelation.** `hash_float3` in the FluidSim3D seed pattern
and `dfp_hash_float3` in `diffuse_force_3d_at_particles` (body + hand oracle) chained
their lanes (h1 = hash(h0)), correlating x/y/z at 0.75/0.75/0.50 (CPU-verified) — the
default seed cluster was a corner-to-corner diagonal CIGAR, and every anti-clump kick
leaned diagonal. Fixed to independent lanes (seed XOR distinct constants; corr ≈ 0.01
after). Real defects, worth having fixed — but NOT the cube's root: the artifact
survived unchanged. **`BlackHole.json` carries the same chained-hash pattern — unfixed,
needs its own look pass.**

*Falsified this part, with evidence:* curl-wobble anatomy (cube survives curl=0 —
though the wobble trig IS 2-periods-across-volume and its axis_raw CAN pass near
zero; cosmetic hazard, still worth a later look); volume-edge convention mixing (cube
unchanged at ctr_scale 1.0/0.9/0.8); blur kernel asymmetry (read: taps and weights
exactly mirrored); container bounds + Euler integrator (read: symmetric);
`flatten_to_camera_plane` at flatten=0 (read: clean early-out); Texture3D
allocation-vs-dispatch mismatch (all vol_res/vol_depth params 128, plan sizes volumes
from those params); two-population/index-identity split (half-split meter: all 2M
live particles uniform in the low half of a 4M buffer, forces present for all).

*Open clues for the next session:* (a) center of mass at f900 sits at
**[0.58, 0.41, 0.27] — the displacement is z-DOMINANT**, invisible in the 2D view and
unexplained by any surface theory; (b) peak |force| at flow −0.10 is **0.137/frame ≈
14% of the volume per Euler step** — wildly over-CFL; the churn cube may be a
numerical-instability zone whose location is set by whatever seeds the z asymmetry;
(c) the artifact needs strong flow (−0.10); at −0.01 only mild TR pooling.

*Next instrument (build BEFORE more hypothesis testing):* a **Texture3D slice
viewer** — render z-slices of each stage's volume (density, blurred density,
gradient, force, blurred force) to PNGs from the harness, and LOOK at which stage
the cube/asymmetry first enters. Bisects the pipeline in one run instead of testing
mechanisms one at a time; also a graph-editor gap (no Texture3D preview exists), so
the work serves the product. The half-split meter machinery in `fluid3d_bias.rs`
stays.

---

### BUG-067 (ui-snapshot-dead-blit-pipeline) — dead `make_blit_pipeline` fails clippy under the ui-snapshot feature — LOW
**Status:** FIXED 2026-07-10 (UI_HARNESS_UNIFICATION P0 landing) — `make_blit_pipeline` had zero callers (abandoned thumbnail-blit path); deleted from `render.rs`. Duplicate of BUG-057, closed together.

**Found 2026-07-08 during DRAG_CAPTURE P1 gating; confirmed pre-existing** (present at base
`b9304330`, reproduced in a throwaway worktree; `git diff --stat b9304330 -- .../render.rs`
empty). Symptom: `fn make_blit_pipeline` at `crates/manifold-app/src/ui_snapshot/render.rs:760`
is never called; under `-D warnings` with the `manifold-app/ui-snapshot` feature the dead-code
lint denies the build. The no-feature clippy gate is clean, so it only bites a combined
`clippy --features ui-snapshot` invocation. Root cause: leftover helper. Fix shape: delete it,
or wire it if the blit path is meant to be used. Not blocking; outside DRAG_CAPTURE's file list.

---

### BUG-070 (stepper-and-nonstandard-slider-reset) — right-click reset was absent on the non-slider-track gain controls — FIXED 2026-07-10 (AUDIO_SETUP_DOCK P4, `feat/audio-dock-p4`), decay drawer PARTIALLY FIXED 2026-07-08 @ 3a88f728 — LOW (found 2026-07-08 during BUG-061)
**Status:** FIXED

**Update 2026-07-10 (AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md P4, D8):** closed the
remainder. `audio_setup_panel.rs`'s `handle_event` gained a `UIEvent::RightClick` arm that hit-tests
against `gain_minus`/`gain_value`/`gain_plus` for the row it belongs to and, on a hit, emits
`PanelAction::slider_reset(AudioSendGainDragBegin(send), AudioSendGainDragChanged(send, 0.0),
AudioSendGainDragCommit(send))` — replaying the SAME drag-commit trio a real drag already uses, at
0.0 dB (unity), through the SAME generic `SliderReset` dispatcher every other reset in the app goes
through (`ui_bridge/mod.rs:189`). Verified: this is the "every card/panel slider in the app"
convention BUG-105 names, not a new gesture — the panel's OWN other reset (double-click on the
dock's resize *handle*, `window_input.rs:343`) resets a layout WIDTH, a different affordance from
resetting a control's VALUE, so it wasn't the precedent to match. Covers all three of the gain
control's interaction surfaces (the two step buttons + the D7 drag-value zone/"overlay-drag
send-fader") in one hit-test, since they're the same logical control. L3-gated:
`scripts/ui-flows/audio-setup-hygiene.json` steps the gain to +3 dB via 3 clicks, right-clicks it,
and asserts the value reads 0 dB again (count-based, not just existence) — exit 0. Added
`Gesture::RightClick` to the flow-script vocabulary (`automation.rs`/`script.rs`) to make the
gesture scriptable at all; it didn't exist before this phase.

**Update 2026-07-08 @ 3a88f728 (intrinsic-reset follow-through):** the envelope-decay drawer
slider is now wired (its `EnvDecay*` trio had a real handler, just no registration). More
importantly that commit made reset a *required build input* — `BitmapSlider::build` takes a
`reset` and returns `Slider { ids, reset }`, registered by one shared replay instead of per-panel
loops — which also closed the real motivating gap: the Clip Trigger drawer's Amount/Attack/Release
sliders (a trigger-gate row with no main slider, so the old per-panel loop bailed before reaching
them).

**Symptom:** BUG-061 made right-click reset the shared gesture for every intent-registered
slider *track*, but three gain-ish controls don't render as a slider track and so were left
out: the Audio Setup gain `[−] value [＋]` steppers and the overlay-drag audio send-fader (both
in `audio_setup_panel.rs`), and the envelope-decay drawer slider (`param_slider_shared.rs`
`build_envelope_config`, which rides the same `drawer.rs` path but emits a different
value-change than the `AudioModShape*` trio BUG-061 wired).

**Root cause:** the steppers are a `[−]value[＋]` control (no track), the send-fader is
overlay-drag (`AudioSendGainDrag*`, not an intent-registered track), and the decay slider was
simply not in BUG-061's surface list. None expose the `SliderNodeIds.track` node that
`SliderReset` registration hangs off.

**Fix shape (as built):** rather than force the stepper/drag-zone controls onto the
intent-registered `SliderNodeIds.track` path (they have no track), `audio_setup_panel.rs`
handles `RightClick` directly in its own bespoke `handle_event` (the panel isn't migrated to the
node-intent system) and reuses the drag-commit action trio it already had for D7's drag gesture —
no new command, no new `PanelAction` variant needed.

---

### BUG-071 (ui-snap-dump-stale-parent) — `ui_snapshot::dump.rs` serializes the mint-time parent, not the live reparented one — LOW (dev-tooling only)
**Status:** FIXED 2026-07-10 (`UI_HARNESS_UNIFICATION_DESIGN.md` P0, D9c) — `dump_tree_ex`
(`dump.rs:38`) and `terse` (`dump.rs:92`, now ~95/~99 after the comment) both serialize
`tree.parent_of(n.id)` (the live, already-`pub`, `parent_index`-backed accessor) instead of
`n.parent_id`. The rejected alternative (mutating `nodes[i].parent_id` inside
`reparent_root_nodes`) was NOT taken — it would touch live UI code, which P0's zero-live-code
rule forbids; the dump-only fix was already the design's stated preference. Committed on
`feat/ui-harness-p0` (not yet landed to main at time of writing — see that branch's history for
the exact SHA once merged).

`ui_snapshot::dump.rs` serializes `UINode.parent_id` (the mint-time struct field) instead of `UITree.parent_index` (the live array `reparent_root_nodes` actually mutates) — any node reparented via `ScrollContainer::reparent_content` (or the like) shows `parent: null`/its original parent in `--dump` JSON even though it's correctly clipped/nested for real rendering. Found 2026-07-08 verifying BUG-060: the dump made a correctly-fixed tree look unclipped, costing real debugging time before the PNG (the actual render) proved it was fine. **Fix shape:** either serialize `tree.parent_index[i]` in `dump.rs:38`/`:92`, or have `reparent_root_nodes` also update `self.nodes[i].parent_id` so the two stay in sync.

---

### BUG-072 (audio-mixdown-all-targets-clippy-debt) — pre-existing lint failures in audio_mixdown.rs only visible under `--all-targets` — FIXED @ `78e97d4a`
**Status:** FIXED @ `78e97d4a` — same fix as BUG-088. `bug-wave1-lane-d-test-hygiene`, 2026-07-11.

**Symptom:** `cargo clippy --workspace --all-targets -- -D warnings` fails on
`crates/manifold-playback/src/audio_mixdown.rs:623` (`needless_range_loop`) and `:643`
(`cloned_ref_to_slice_refs`, `std::slice::from_ref` suggested). Confirmed pre-existing —
reproduces identically on main at `2682f9f4`, before any PARAM_STEP_ACTIONS change touched
this file.

**Root cause:** this codebase's standard gate command is `cargo clippy --workspace --
-D warnings` (no `--all-targets`), which never compiles integration-test binaries or exercises
lints inside them; these two lints only fire under the stricter `--all-targets` invocation,
which nothing had run against this file before. Found via the same stricter check adopted
this session after P1's `load_project.rs` compile break slipped through the non-`--all-targets`
gate.

**Fix (applied):** re-verified still firing before the rewrite (at shifted lines 644/664 after
this session's Part 1 `TestDir` fix landed above them). Replaced with `for (i, (&tapped_sample,
&solo_sample)) in tapped.iter().zip(solo_audio.master_mono.iter()).enumerate()` and
`std::slice::from_ref(&analysis_id)` / `std::slice::from_ref(&normal_id)` — mechanical, no
behavior change. **Verified:** `cargo clippy -p manifold-playback --tests --all-targets --
-D warnings` clean for these lints (one unrelated pre-existing `osc_receiver.rs` lint remains
out of scope, logged as BUG-110).

---

### BUG-074 (audio-mixdown-flaky-under-parallel-tests) — a manifold-playback test fails intermittently only under the default parallel runner — FIXED @ `78e97d4a`
**Status:** FIXED @ `78e97d4a` — same shared-state bug as BUG-090/BUG-106, see BUG-090 for the root cause writeup. `bug-wave1-lane-d-test-hygiene`, 2026-07-11.

**Symptom:** `cargo test -p manifold-playback` (default, parallel) fails
`audio_mixdown::tests::render_export_audio_tapped_layer_matches_rendering_alone`
roughly 1 run in 3; `cargo test -p manifold-playback -- --test-threads=1` is
green every time (5/5 tried). Not caused by this phase's changes — the new
`param_step_clip_edge.rs` round-trip test (a different file, its own temp
path keyed by pid+nanosecond timestamp) isn't in the failure's module path.

**Root cause:** confirmed — the leading suspect, `TestDir`/temp-path collision, was correct (the
GPU-contention suspect was ruled out: `render_export_audio` is pure CPU decode/resample, no GPU
device involved). `tempfile_dir::TestDir::new`'s `{prefix}_{pid}_{nanos}` key collided across
near-simultaneous calls from different test threads because `SystemTime::now()`'s nanosecond value
is not actually nanosecond-resolution on this machine (~96% collision rate measured over 200k tight-
loop calls). This test and 4 others share the `build_fixture_project()` fixture (same
`"manifold_audio_mixdown_test"` prefix), so a colliding directory means two tests race on the same
`tone.wav` file — full mechanism in BUG-090.

**Fix (applied):** per-process atomic sequence number added to the `TestDir` path, eliminating the
possibility of collision regardless of clock resolution. **Verified:** `cargo test -p
manifold-playback --lib` green across 10 consecutive parallel runs (228 passed each run).

---

### BUG-075 (timeline-drag-end-never-finalizes) — the terminal DragEnd for trim/marquee/move was dropped, so on_end_drag never ran — HIGH — FIXED 2026-07-08 (found + fixed same session)
**Status:** FIXED (2026-07-08)

**Symptom:** after the DRAG_CAPTURE landing (P1 `6e4bddcb`), timeline
trim-drag and click-drag marquee/region-select never finalized on release —
the trim didn't commit, the selection box stayed live, `drag_mode` stuck.
Clip *move* looked fine but silently lost its undo snapshot and left the
same stuck `drag_mode` (its live per-frame preview masked the miss).

**Root cause:** ordering bug in `ui_root::process_events`. The terminal
`DragEnd`/`PointerUp` arm called `broadcast_gesture_end()`, which set
`drag_owner = None`, BEFORE the post-match `should_stash_for_tracks(event)`
read `drag_owner` to decide whether to stash the event for
`InteractionOverlay`. With the owner already nulled, the terminal `DragEnd`
was never pushed to `viewport_events`, and `on_end_drag` (the sole finalizer,
`app_render.rs`) never ran. Confirmed empirically by an adversarial repro
driving the real `process_events` path: stashed kinds = PointerDown,
DragBegin, Drag — no DragEnd. The existing ownership unit tests set
`drag_owner` by hand and called `should_stash_for_tracks` directly, bypassing
the broadcast-before-stash seam, which is why it shipped.

**Fix:** split `broadcast_gesture_end` into `fire_gesture_end_hooks()`
(overlay hooks only) + the fused clear. The terminal arm fires the hooks but
defers the `drag_owner = None` clear to the end of the iteration, after the
stash read; the PointerDown self-heal keeps the fused `broadcast_gesture_end`
so a lost-OS-release still clears a stale owner. Guarded by a new
`process_events`-driven regression test
(`timeline_drag_end_reaches_viewport_events_through_process_events`) that
drives Down→Move→Up and asserts the terminal DragEnd reaches
`drain_viewport_events()`.

---

### BUG-077 (test-fixtures-not-region-wrapped) — 17 tests across `manifold-renderer` + `manifold-ui` mint root-parented nodes outside a region and panic on the D4 ownership assertion — LOW (pre-existing, found 2026-07-09 during PARAM_STORAGE_BOUNDARIES P3's full-workspace sweep) — FIXED 2026-07-09 (`fix/bug-077-uicache-regions`); workspace fully D4-clean
**Status:** FIXED (2026-07-09)

**Symptom:** `cargo test --workspace` fails 17 tests, all with the same panic:

```
thread '...' panicked at crates/manifold-ui/src/tree.rs:290:9:
root-parented node minted outside an open UITree::begin_region — UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md D1/D4.
Wrap this subtree's build in begin_region(...)/end_region(...) instead of rooting it at the tree.
```

The full failing set (the whole D4-conformance class — enumerated iteratively via
`--no-fail-fast` runs, each fix surfacing the next binary the previous fail-fast had
hidden):

- **6 in `manifold-renderer/src/ui_cache_manager.rs` unit tests:**
  `extent_change_forces_fallback`, `extents_unchanged_when_bounds_stable`,
  `incremental_used_when_only_card_dirt`, `no_subregions_signature_is_empty`,
  `out_of_subregion_dirt_forces_full_render`, `partition_change_forces_fallback`
  (surfaced by the `ui_cache_manager` test filter — found first).
- **6 in `manifold-renderer/tests/ui_color_swatches.rs` snapshot tests:**
  `header_demo`, `footer_demo`, `transport_demo`, `modulation_drawer_sheet`,
  `browser_popup_demo`, `browser_popup_thumbnails_paint` (surface only on a full
  crate run, not the narrow `ui_cache_manager` filter).
- **5 in `manifold-ui/tests/chrome_param_card_proof.rs`:**
  `badge_toggle_is_in_place`, `build_matches_card_structure`,
  `intents_resolve_and_fold_up`, `opening_drawer_needs_rebuild_then_grows`,
  `value_change_reconciles_in_place` (surface only on the full-workspace run — a
  different crate; the 6th test in the file, `validate_catches_unwired_control`,
  builds no tree and always passed).

**Root cause:** `0bb51dad` ("region mechanism — ZTier, RegionToken,
begin_region/end_region, D4 enforcement") landed the D4 root-parented-node panic
guard (`mint`'s `debug_assert!` at `tree.rs:290`, `#[cfg(not(test))]` — so it is
active for any *non-`manifold-ui`* dependent, which every one of these test
binaries is: `manifold-renderer`'s own tests, and `manifold-ui`'s *integration*
tests, which compile `manifold-ui` as an external non-test dependency). These test
fixtures still build their tree directly against the root, outside any
`begin_region`/`end_region` pair — they were never migrated to the region contract.
One bug class, three files, two crates.

**Confirmed unrelated to PARAM_STORAGE_BOUNDARIES P3:** `git diff --stat` for the
P3 session touches only `crates/manifold-io/src/migrations/param_storage_v14.rs`;
`manifold-ui`/`manifold-renderer` are untouched. The `ui_color_swatches` half was
additionally confirmed pre-existing by `git stash`ing the `ui_cache_manager` fix
and rerunning `--test ui_color_swatches` against the base commit `b15e5c20` — the
same 6 fail identically, so no half is caused or masked by another.

**Escaped:** `wave/param-boundaries-p1` or an earlier UI-region wave (whichever
landed `0bb51dad` without touching these fixtures) · caught-by: the next phase's
full-workspace sweep (P3) plus this fix session's own crate-wide and workspace gate
runs, not that wave's own gate — the region-enforcement landing's test scope did not
include a full `cargo test --workspace` run.

**Fixed** — test files only; no production code touched, `tree.rs:290`'s D4
assertion unchanged. Every failing fixture now wraps its tree build in a single
`tree.begin_region(rect, tier, label, UIFlags::empty())` / `tree.end_region(region,
start)` bracket, matching the idiom real callers use (`ui_root.rs`'s per-panel
pairs; the closer precedent for these flat, non-tiered fixtures is
`ui_snapshot/render.rs`'s single-region wrap and the `split_handles` region — both
use a no-op-clip rect precisely so the region's `CLIPS_CHILDREN` is a guaranteed
no-op and the rendered pixels are unchanged). Proof of completeness:
`cargo test --workspace --no-fail-fast 2>&1 | rg 'tree.rs:290'` returns **zero**
hits — the whole D4 class is gone on this branch.

- `crates/manifold-renderer/src/ui_cache_manager.rs`: the two fixture builders
  (`tree_with_subregions`, `tree_with_chrome_and_card`) now open one region around
  their whole build. Wrapping shifts every node index by the region container node
  `begin_region` mints first, so the fixtures return the panel's own `node_start`
  (`tree.count()` captured right after `begin_region`) and compute sub-region ranges
  relative to it; the six tests reference edges via that `start` (or `subs[i].0`,
  `end - 1`) instead of the old absolute literals.
- `crates/manifold-renderer/tests/ui_color_swatches.rs`: the six snapshot tests wrap
  their `panel.build(...)` / `popup.build(...)` / slider+drawer build loop in a
  full-canvas region (Chrome tier for the transport/header/footer panels, Overlay
  for the browser popups, Base for the mod-drawer sheet — semantically faithful,
  but with one region per test the tier only labels intent). Note that since
  `0bb51dad` `render_tree` walks *registered regions* (`traverse` → `traverse_regions`),
  not root-parented nodes, so the wrap is also what makes each test's panel/popup
  tree content render at all — an unwrapped build registers no region for the
  traversal to visit. Verified by rerunning the suite: all six now produce their
  PNGs and pass.
- `crates/manifold-ui/tests/chrome_param_card_proof.rs`: the shared `ProofCard::build`
  helper (every failing test routes through it) opens one region around the
  `ChromeHost::build` call, region rect == the card rect. Safe because the region
  container is minted directly on the tree, NOT through the host — so `ChromeHost`'s
  own `ids`/`node_count`/DFS indices (which the tests assert on exactly:
  `host.node_id(N)`, `host.node_count()`, `t.count()`) are untouched; the host bases
  off `tree.count()` at build start, exactly as its own
  `build_assigns_contiguous_ids_from_tail` unit test already proves for a mid-tree
  build. The intent fold-up tests still resolve correctly: `IntentRegistry::resolve`
  stops at the first ancestor carrying the gesture/area-claim (always a host node
  below the region), and the region node carries neither, so the extra transparent
  ancestor changes nothing.

---

### BUG-078 (generator-runtime-reshapes-from-stale-meta-params) — a structural-rebuild's reshape read the graph's stale `preset_metadata.params` shadow because the constructor took no `ParamManifest` — LOW — FIXED 2026-07-09 on `fix/bug-078-reshape-manifest` (confirmed by regression test, then fixed same session)
**Status:** FIXED (2026-07-09)

**Symptom:** calibrate a generator param (widen its range / add a curve) → make
a structural graph edit (add/remove a node) before saving → the *rendered* param
mapping could momentarily revert to the pre-calibration reshape. Bounded and
non-data-loss: the authoritative `PresetInstance.params[id].spec` (the manifest)
was never touched, and the correct reshape reasserted itself the moment the
project was saved and reloaded (D12 derives `meta.params` from the manifest at
serialize time).

**Root cause:** `PresetRuntime::from_def` (`crates/manifold-renderer/src/preset_runtime.rs`)
built its `param_reshape: AHashMap<String, (min, max, curve, invert)>` — the
map every generator binding's [`Reshape`](crates/manifold-renderer/src/node_graph/param_binding.rs:273)
is resolved from at construction — entirely from `doc.preset_metadata.params`,
the shadow. `from_def` / `from_def_with_device` / `from_json_str_with_device`
took no `ParamManifest` parameter, so no code path could hand a live,
post-calibration manifest spec to the reshape. This was the generator analog of
the effect path's `synth_user_binding` (`manifold-core/src/effects.rs:1752-1783`),
which already reads the manifest (`self.params.get(&b.id)`) post-P2; generators
never got the equivalent wiring because their whole binding list (stock +
user-added) resolves through one shared `doc.preset_metadata` path.

**Fix (shipped):** threaded `Option<&ParamManifest>` through the constructor
chain — `from_def` / `from_def_with_device` / `from_json_str_with_device` →
`GeneratorRegistry::create` / `create_with_override` →
`GeneratorRenderer::install_layer_generator` / `acquire_clip`. When the manifest
is present, `from_def` overlays each param's reshape (min/max/curve/invert) from
the manifest `spec` over the shadow, manifest-wins-per-id; when `None` it keeps
reading the shadow (correct for a fresh-from-disk standalone build). The one live
caller — `generator_renderer.rs`'s `start_clip` and the per-frame `render_all`
structural-rebuild sweep — passes `layer.gen_params().params`. Every other caller
(thumbnails, `check_presets`, `freeze_profile`, gltf import, freeze proofs, the
cold-start thumbnail path, type-swap rebuild) passes `None` and is byte-identical.
`from_json_str` (mock/test) keeps its 2-arg signature, passing `None` internally.
The empty-fast-path and the `preset_metadata.bindings` scale/offset +
`string_bindings` reads were left untouched.

**Regression test** (now green, in the default suite):
`crates/manifold-renderer/src/preset_runtime.rs`,
`generator_runtime_tests::generator_rebuild_reshape_honors_live_manifest_over_stale_shadow`.
A def whose `preset_metadata.params` `amt` spec is fixed at
`min=0,max=1,curve=Exponential` (the stale shadow) is rebuilt via
`PresetRuntime::from_def` with `Some(&values)` where `values` carries the
recalibrated `min=0,max=2` — the reshape now resolves value 1.0 to `0.5` (the
fresh 0..2 range), where the pre-fix output was `1.0` (the stale 0..1 range).
The bug is only observable when the reshape has a non-identity curve/invert
(`apply_card_reshape` only consults min/max when `invert || curve != Linear`).

**Escaped:** `wave/param-boundaries-p2` (`254792c0`) — dual-write deletion
(D4) landed correctly for the manifest side but left this generator-only
construction-time read pointed at the now-unmaintained shadow · caught-by:
review (audit during PARAM_STORAGE_BOUNDARIES P2, not the wave's own gate —
no existing test exercised a structural rebuild after a calibration).

---

### BUG-082 (trigger-fire-mode-level-features-near-dead) — fire-mode audio mods silently near-dead on non-impulse features — MED
**Status:** FIXED @ `12fbc37d`, closure corrected 2026-07-11 (the fix never functioned — see BUG-109), **re-fixed 2026-07-11 by AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md §7 P5.** The meter now pushes a level in every transport state with UI-side peak-hold (BUG-109's fix, detailed there). This entry's remaining gap is the same one BUG-109 carries: Peter's rig look (VD-025) hasn't confirmed the live crossing yet — don't re-close this a second time on a PNG alone.

Found 2026-07-09 (Peter noticed while discussing the Audio Setup redesign; mechanism confirmed in code same session). A fire-mode audio mod (`is_trigger`/`is_trigger_gate` target, §9-unified shape) evaluates *whatever* feature the user picks — [`modulation.rs:519`](../crates/manifold-playback/src/modulation.rs#L519) extracts the configured `AudioFeature`, shapes it, and edge-detects it rising through 0.5 via `trigger_edge.advance` — and the drawer's Feature row ([`param_slider_shared.rs:1574`](../crates/manifold-ui/src/panels/param_slider_shared.rs#L1574)) offers all of `AudioFeatureKind::ALL` on trigger cards with no restriction or warning. The edge chassis (`TransientEdge`: fire at 0.5, re-arm hysteresis) is tuned for spike-and-decay signals: **Transients** and **Kick** fire per hit as intended, but level features (**Amplitude/Centroid/Flux/Pitch/Presence**) cross mid once when the track gets loud/bright and then sit disarmed until it drops — from the performer's view the trigger is silently dead or fires arbitrarily. (Non-obvious workaround that already works: the `rate_of_change` toggle differentiates a level into impulses.) **Fix shape (revised same day, on reconciling with LIVE_AUDIO_TRIGGERS §9 U2 "any feature, standard drawer" — a decided D that a feature restriction would walk back):** the engine *does* honor level features — they behave as a Schmitt trigger against the fixed 0.5 edge with Amount as the tune knob; what's missing is visibility, not capability. The fix is `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` D6 (lands its P3): a live level meter with the fire threshold drawn as a line, beside Amount on every fire-mode drawer, so the crossing is tunable by eye. The separable widening — a first-class level-crossing detector (explicit threshold + hysteresis knobs) — is that design's Deferred #1 with a named revival trigger; Peter's call 2026-07-09: "'fire when amplitude crosses a level' is a real future widening but it's a detector question, separable from the config unification."

**Fixed (P3c, `12fbc37d`):** the meter itself, not a feature restriction — U2 stands. Both content-thread evaluators (`modulation.rs`'s `is_trigger_gate` arm, `live_trigger.rs`'s clip-trigger walk) now capture the exact `shape.condition()` signal they edge-detect against 0.5 into a fixed-size `FireMeterCapture` (zero heap allocation), which rides `ContentState` to the UI thread. `build_audio_mod_drawer`'s Amount row grows a track+fill+0.5-tick meter strip whenever the row is a fire-mode config (a param `is_trigger_gate` card or any clip trigger); its fill is pushed in place every UI tick, never rebuilt (mirrors the deleted `update_trigger_levels` precedent, 470228ec). Verified with a headless PNG + tree dump showing the meter rendering correctly on both drawer kinds and correctly absent on a plain continuous mod — the live crossing itself is Peter's L4 feel-pass (no audio device in the build/test sandbox).

---

### BUG-083 (video-export-has-no-progress-display) — exporting video gives zero on-screen feedback until the finish toast — MED (export is a release pillar; long exports look like a hang)
**Status:** FIXED @ `<PENDING-LANDING-SHA>` (wave2 lane C, 2026-07-11)

Found 2026-07-09 (A1 orphan purge: the un-suppressed dead-code lint flagged `is_exporting` /
`export_progress` / `export_status` as never read; `git log -S` confirmed **no consumer ever
existed** — only the intro commit `d754eb08` and a lint sweep ever touched them). The content
thread's export loop faithfully sent progress snapshots every 10 frames into a void; the only
user-visible export feedback is the D17 finish toast (`export_finished`, which IS wired). From
the performer's view a multi-minute export is indistinguishable from a hang.

**Fixed:** restored `is_exporting` / `export_progress` / `export_status` on `ContentState`
(`content_state.rs`) with `send_export_progress` (`content_export.rs`) populating them again from
the real `ExportSession`, and built the missing consumer this time — a header progress strip
(`HeaderPanel::set_export_status`, `manifold-ui/src/panels/header.rs`, wired from
`app_render.rs`'s per-frame content-state drain), mirroring the existing percussion-import
status/progress bar's always-emit/toggle-visibility pattern (no tree rebuild on progress ticks).
Verified against the REAL production export path (not a mock): the `journey-proof` harness
(`crates/manifold-app/src/journey_proof.rs`, `--features journey-proofs`) drives the unmodified
`ContentThread::run_export`, and a run of `audio_reactive_export_moves -- --nocapture` printed
`is_exporting`/`export_progress`/`export_status` climbing 1.0% → 94.8% across ten real snapshots
before the finish event — confirming the exact fields the header consumer reads actually progress
during a real export. The header widget itself is unit-tested
(`export_progress_toggle_is_in_place_and_text_updates`) for the same value/visibility contract.
**Not verified this session:** watching the progress strip render live in the running app window
on the rig — owed to Peter's eye on a real multi-minute export.

---

### BUG-084 (recording-drop-counter-never-surfaced) — live-recording dropped frames counted but never shown — LOW (gig-resilience visibility gap)
**Status:** FIXED @ `<PENDING-LANDING-SHA>` (wave2 lane C, 2026-07-11)

Same discovery path as BUG-083: `recording_dropped_frames` (fed by
`recording_session.frames_dropped()`, pool-exhaustion drops) was emitted every tick and read
nowhere. A recording silently dropping frames during a set is exactly the failure the performer
needs to see.

**Fixed:** restored `recording_dropped_frames` on `ContentState`, added a sibling
`recording_dropped_audio_frames` fed by a new native counter (see the audio-drop-counter
observation appended to BUG-086 below — that native work was built as part of this fix's
"surface the recording indicator" scope), and surfaced both on the layer-header Record button's
label (`LayerHeaderPanel::set_recording_drops`, `manifold-ui/src/panels/layer_header.rs`): "Stop
Recording" while clean, "Stop Recording ⚠ N dropped" once anything has dropped, cleared on stop
so the next recording starts clean. Verified with a focused unit test exercising the real
build/update path (`recording_drops_surface_on_record_button_label`) confirming the button's tree
text actually changes; the layout/pulse-color golden tests were re-run and are unaffected. **Not
verified this session:** a real drop actually firing on the video-pool-exhaustion path and being
seen on the running app's Record button — the soak run in this session's audio-drop observation
(see BUG-086) had 0 drops on both counters, so the wiring is proven correct at zero but not
watched moving on the rig.

---

### BUG-087 (osc-timecode-receiving-flag-false-positive-at-startup) — `is_receiving_timecode` can read true before any real OSC message ever arrives — LOW-MED (narrow boot-time window)
**Status:** FIXED 2026-07-10 — `last_timecode_received_time` now defaults to `Seconds(f64::NEG_INFINITY)` (osc_sync.rs), so the timeout check can never pass before a real message sets it. Regression test `osc_update_no_false_receive_at_startup_before_any_timecode`.

Found 2026-07-10 while wiring F1 (OSC/SMPTE timecode receive path, CORE_ENGINE_MAP §13.1) —
[`osc_sync.rs`](../crates/manifold-playback/src/osc_sync.rs)'s `update()` computes `receiving =
(now - last_timecode_received_time) < transport_timeout`, and `OscSyncController::new()`
defaults `last_timecode_received_time` to `Seconds::ZERO` rather than a sentinel far in the
past. `now` is the content thread's `time_since_start`, which also starts at zero at app boot.
So if OSC M4L sync becomes enabled while `time_since_start` is still within `transport_timeout`
(default 0.5s) of boot, the very first `update()` tick computes `receiving = true` with zero
real timecode ever having arrived — a false positive. Combined with `follow_transport` (default
on), this can fire a spurious PLAY at the very start of a session with OSC M4L mode already
selected. The F1 receive-path test (`crates/manifold-playback/tests/osc_timecode.rs`) hit this
directly: its `wait_for_receiving` loop's first iteration reported "receiving" before the test's
synthetic UDP packet had actually been processed, until the test was changed to offset its own
`now` baseline well past `transport_timeout` — a test-side workaround, not a production fix.
**Fix shape:** give `last_timecode_received_time` a sentinel default (e.g. a large negative
`Seconds`, matching the existing `pending_timecode_seconds: Seconds(-1.0)` sentinel pattern one
field up) so the timeout check can never pass before a real message sets it. One-line change;
left open because it's outside F1's scoped wiring fix and the exact boot-time semantics
("ported from Unity") deserved a dedicated look rather than a same-session drive-by.

---

### BUG-088 (pre-existing-clippy-tests-gate-dirty-since-f1-landing) — `cargo clippy -p manifold-playback --tests -- -D warnings` was already failing at the commit F2 started from — FIXED @ `78e97d4a` (partial — see note)
**Status:** FIXED @ `78e97d4a` — `bug-wave1-lane-d-test-hygiene`, 2026-07-11.

Found 2026-07-10 running F2's gate (`cargo clippy -p manifold-playback --manifest-path
.../Cargo.toml --tests -- -D warnings`). Two files fail, neither touched by F2: `doc_lazy_continuation`
in [`tests/osc_timecode.rs:172`](../crates/manifold-playback/tests/osc_timecode.rs#L172) (an F1
doc-comment paragraph needs a blank line or indent), and three `cloned_ref_to_slice_refs` /
`needless_range_loop` hits in [`src/audio_mixdown.rs`](../crates/manifold-playback/src/audio_mixdown.rs)
at lines 589, 623, 643. `git diff cf1f3dc6 -- <both files>` is empty — both are byte-identical to
the base commit F2 branched from, so this predates F2 and isn't a toolchain drift F2 introduced.
Confirmed F2's own diff is clean two ways: the plain `cargo clippy -p manifold-playback --
-D warnings` (no `--tests`, compiles the lib including its own unit tests via `#[cfg(test)]`)
passes, and `cargo clippy -p manifold-playback --test live_clip -- -D warnings` (the target F2
actually edited) passes standalone. **Fix shape:** trivial — indent/blank-line the doc comment in
`osc_timecode.rs:172`; replace the two `.clone()`-into-single-element-slice calls with
`std::slice::from_ref` and the `for i in 0..len` loop with `.iter().enumerate()` in
`audio_mixdown.rs`. Left open rather than fixed as a drive-by: both files are unrelated to F2's
scope (timecode receive path and audio mixdown respectively) and the fix, however small, belongs
to whoever owns those files' next change.

**Re-verified 2026-07-11 before fixing:** the three `audio_mixdown.rs` lints still fired (at
lines 610/644/664 by then, shifted +21 from this session's Part 1 `TestDir` fix landing above
them) — fixed as prescribed. `osc_timecode.rs:172`'s `doc_lazy_continuation` no longer reproduces
at all — the file is unchanged since this bug was filed, but the current toolchain (`clippy 0.1.94
(4a4ef493e3 2026-03-02)`) doesn't flag it; left untouched since there's nothing to fix. **New,
unrelated pre-existing issue surfaced while isolating this gate:** `osc_receiver.rs:366,368` fails
`clippy::type_complexity` under `--tests` (byte-identical to the base commit, last touched by an
unrelated "F3" session) — out of scope for BUG-088/072, logged as BUG-110.

---

### BUG-090 (audio-mixdown-analysis-only-test-flakes-under-parallel-run) — an exact-float-equality mixdown test failed once under parallel `cargo test`, passed on rerun — FIXED @ `78e97d4a`
**Status:** FIXED @ `78e97d4a` — `bug-wave1-lane-d-test-hygiene`, 2026-07-11.

Found 2026-07-10 running F2's gate: `cargo test -p manifold-playback` (full crate, default
parallel test threads) reported `audio_mixdown::tests::render_export_audio_analysis_only_layer_taps_but_never_hits_master`
FAILED — `assertion left == right failed: analysis-only layer altered master left` at
`audio_mixdown.rs:678`. Two follow-ups both passed: running the same test alone
(`cargo test -p manifold-playback --lib audio_mixdown::tests::render_export_audio_analysis_only_layer_taps_but_never_hits_master`)
and rerunning the full suite immediately after (186 passed, 0 failed — same test count, this one
included). `git diff` against F2's base commit (`cf1f3dc6`) is empty for `audio_mixdown.rs`, so
F2 didn't touch it. The test does an exact `assert_eq!` on two independently-rendered `f32` audio
buffers (`audio.left`/`audio.right` vs. a second `render_export_audio` call on a trimmed-down
project) — that comparison pattern is inherently sensitive to any nondeterminism between the two
render calls (shared mutable state, thread-scheduling-dependent float summation order, or a
`TestDir`/temp-path collision with a concurrently-running test in another thread).

**Root cause (confirmed 2026-07-11):** it's the named `TestDir`/temp-path collision suspect, not a
float-summation-order race. `tempfile_dir::TestDir::new` in `audio_mixdown.rs`'s test module keyed
its temp directory as `{prefix}_{pid}_{nanos}` — but `SystemTime::now()`'s nanosecond value is NOT
actually nanosecond-resolution on this machine: a controlled loop of 200k consecutive calls to the
same clock read produced ~96% collisions (192,266 / 200,000 duplicate timestamps). Five tests in
this module call `build_fixture_project()`, which shares the SAME prefix
(`"manifold_audio_mixdown_test"`); under `cargo test`'s default parallelism several of those calls
fire from different threads (same `pid`) at near-identical wall time, landing in the same coarse
clock bucket. A collision means two tests' `TestDir`s resolve to the SAME directory: they race
writing/reading the same `tone.wav` fixture, and whichever `TestDir` drops first
(`remove_dir_all`) deletes the directory out from under the other — producing a corrupted or
missing fixture file for one of the two concurrently-running tests, which decodes to genuinely
different sample data and fails the exact `assert_eq!`. Not order-dependent global state, not
float non-associativity — a real filesystem race from an under-specified uniqueness key.

**Fix (applied):** added a per-process `AtomicU64` sequence number to the `TestDir` path
(`{prefix}_{pid}_{nanos}_{seq}`), guaranteeing every call produces a distinct path regardless of
clock resolution — the same pattern already in use by
`percussion_backend.rs::build_temp_config_path`. **Verified:** `cargo test -p manifold-playback
--lib` green across 10 consecutive parallel runs (228 passed each run, 0 failed).

---

### BUG-091 (osc-drop-frame-timecode-uses-approximate-divisor) — SMPTE drop-frame seconds conversion divides by literal `29.97` instead of the true `30000/1001` rate — LOW (self-correcting, sub-frame magnitude)
**Status:** FIXED 2026-07-10 — drop-frame branch now computes `(total_frames as f64 * 1001.0 / 30000.0) as f32` (osc_sync.rs); 00:01:00:02 DF is now exactly 60.06s. Exact-value test `drop_frame_absolute_seconds_are_standards_exact`.

Found 2026-07-10 building F3's drop-frame timecode test vectors
(`crates/manifold-playback/src/osc_sync.rs::tests`). `timecode_to_seconds`'s drop-frame branch:

```rust
let total_frames = 108000 * hours + 1800 * minutes + 30 * seconds + frames - dropped_frames;
total_frames as f32 / 29.97
```

The frame-*drop pattern* (skip display numbers `:00`/`:01` at the start of every minute except
every tenth) is exactly SMPTE 12M and was safe to pin with a test (`drop_frame_skips_two_frame_numbers_at_non_tenth_minute_boundary`,
`drop_frame_does_not_skip_at_ten_minute_boundary` — both phrased as self-consistent one-frame
deltas so the divisor question doesn't leak into them). The *divisor* is where it diverges from
the standard: real 29.97 drop-frame timecode runs at exactly `30000/1001 ≈ 29.970029970...`
fps, not the flat decimal `29.97`. At TC `00:01:00:02` (`total_frames = 1800`), the code's
`1800/29.97 = 60.060060...s` vs the standard's `1800/(30000/1001) = 60.06s` exactly — a ~60µs
gap that scales linearly with elapsed frame count (~3.6ms/hour). `timecode_frame_rate` (the
serialized, user-facing field) is also silently ignored in this branch — only the non-drop-frame
`else` arm reads it, so setting it to anything while `drop_frame == true` has no effect.

**Why this is LOW despite being a real divergence from the standard:** `sync_timecode_to_playback`
nudges (or seeks) to the freshly-received OSC timecode on every incoming message while playing
(§7/§11: "no threshold — apply every OSC frame so drift never accumulates"), so any error this
small is overwritten before it could ever be perceived, let alone reach the 0.05s stopped-seek
threshold. Root cause is a one-line arithmetic constant, not a design issue.

**Fix shape:** replace the literal `29.97` with `30000.0 / 1001.0`, and read `self.timecode_frame_rate`
in the drop-frame branch too (or document why it's deliberately fixed at the NTSC rate there).
Left open rather than fixed in F3 because F3's scope is test coverage, not sync-code changes —
this is exactly the class of finding F4/F6 (the correctness-fix phases this net exists for)
should pick up.

---

### BUG-092 (gltf-import-caps-render-scene-objects-at-8-stale-mirror) — glTF import truncates to 8 objects mirroring render_scene's REMOVED object cap — LOW (import-time truncation with a user warning, multi-material models only) — ✅ FIXED (scene-build-p2 session)
**Status:** FIXED @ scene-build-p2

**Fixed:** landed as a drive-by in SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md P2 (the importer
session already touching this exact truncation line for D9's cap-fix requirement). Deleted the
local `MAX_RENDER_SCENE_OBJECTS` constant/duplication entirely; `gltf_import.rs` now imports and
truncates against `crate::node_graph::primitives::render_scene::OBJECT_SLIDER_MAX` directly (made
`pub(crate)`), so the two can never drift again. `rg -n "MAX_RENDER_SCENE_OBJECTS" crates/` → 0
hits.

Found 2026-07-10 while landing RENDER_SCENE_UNBOUNDED_LIGHTS (an unrelated axis — that change
uncapped *lights*; this is *objects*). `crates/manifold-renderer/src/node_graph/gltf_import.rs`:

```rust
const MAX_RENDER_SCENE_OBJECTS: usize = 8;
// ...
let dropped_over_cap = materials.len().saturating_sub(MAX_RENDER_SCENE_OBJECTS);
materials.truncate(MAX_RENDER_SCENE_OBJECTS);
```

The comment at gltf_import.rs:60 says the cap is "mirrored from `node.render_scene`'s own
`MAX_OBJECTS`" — but that constant no longer exists. render_scene generalized object count to a
soft `OBJECT_SLIDER_MAX = 64` on 2026-07-05 (per-object `mesh_n/material_n` ports are generated
by `format!`, one draw call each, no structural cap). So the import path drops objects that
render_scene is now perfectly able to draw: a glTF with, say, 12 distinct materials imports as 8
objects and silently loses 4, with a warning.

**Why LOW:** it's import-time truncation with a user-visible warning, not a crash or a wrong
render; and it only bites models with more than 8 distinct materials. But it's a real capability
regression against the generalized renderer, and the stale comment actively misleads.

**Fix shape:** raise `MAX_RENDER_SCENE_OBJECTS` to track `OBJECT_SLIDER_MAX` (64), or drop the
import-side cap entirely and let render_scene's `objects` slider clamp — importing unbounded and
clamping at the editor is the cleaner match to the "soft editor bound, no structural cap" model.
Refresh the gltf_import.rs:60 comment either way. Left open rather than fixed while landing the
lights change because it's a different axis (objects, not lights) and out of that phase's scope.

---

### BUG-093 (ui-snapshot-fixtures-unnecessary-cast-clippy-debt) — two redundant `as i32` casts fail feature-clippy — LOW
**Status:** FIXED @ a56f641a

**Fixed 2026-07-10 (UI_HARNESS_UNIFICATION P0 landing).** Dropped the two `i as i32` / `20 + i as i32` casts in `ui_snapshot/fixtures.rs` (the loop var `i` from `0..20` is already `i32`). Pre-existing debt on the same `cargo clippy -p manifold-app --features ui-snapshot -- -D warnings` gate as BUG-057/067; all three cleared together.

---

### BUG-094 (fluidsim3d-clip-trigger-turbulence-mux-double-wire) — turbulence burst fires on EVERY clip-trigger mode, not just mode 0 — FIXED 2026-07-10
**Symptom:** any clip trigger (Rot Flip / Flow Inv / Pattern / Inject) also detonates the 10x turbulence boost; the legacy generator boosted only in mode 0 ("Turbulence").
**Root cause:** in FluidSim3D.json's Clip Triggers group, `noise_factor.b` has TWO wires (`mux_noise.out` and `env_x9.out`); the loader keeps the last wire (graph.rs connect() replaces), so `env_x9` wins unconditionally and `mux_noise` (with its `const_one` feed) is dead. Found during the 2026-07-10 full old-vs-new diff.
**Fix shape:** rewire — `env_x9.out -> mux_noise.in_0`, delete `const_one -> mux_noise.in_0` and the direct `env_x9 -> noise_factor.b` wire, keep `mux_noise.out -> noise_factor.b`. Consider a check_presets lint for duplicate wires into one input (silently last-wins today).

---

### BUG-095 (fluidsim3d-boot-seed-center-cluster-not-random) — sim boots from a center-cluster seed instead of the legacy random fill — FIXED 2026-07-10 (boot mux: random until first trigger)
**Symptom:** on load, all particles boot in a tight Gaussian at the volume centre (pattern 0); the legacy generator booted with a uniform random fill (pattern 255). First seconds of the sim look clumped instead of smoky.
**Root cause:** the seed kernel's `pattern` port is wired from `clip_trigger_cycle` (modulus 8), whose first emission at trigger_count 0 is `0 % 8 = 0` (center cluster). The node's static `pattern=7` param (random fill) is shadowed by the wire.
**Fix shape:** boot-time pattern should be 7 (random) — e.g. initialise ClipTriggerCycle's first emission, or gate the cycle wire behind the first real trigger. Related divergence, deliberate to keep: cycle modulus 8 (random joins the rotation; legacy cycled 7).

---

### BUG-097 (ui-snap-render-overlay-pass-uses-wrong-traversal) — `render_ui_to_png`'s overlay pass calls `render_tree_range` where the live app uses `render_sub_region`, and the live app's own comment says the former can render nothing for some overlay ranges — FIXED 2026-07-10 (found during UI_HARNESS_UNIFICATION P2's VERIFY-AT-IMPL diff)

**Status:** FIXED 2026-07-10, by construction — not point-fixed. HARNESS_FIDELITY_INVARIANT §4 step 2 deleted the harness's parallel immediate-pass assembly (`draw_immediate_passes` + its overlay loop) and made `ui_frame::render_main_ui_passes` the single owner of the overlay pass; that owner uses `render_sub_region` @ `Depth::OVERLAY` (with the shadow-peek wrapping), so there is no longer a second copy that could pick `render_tree_range`. The "may be latent" hedge below turned out to be wrong: `UIRoot::build_overlays` takes `start = tree.count()` AFTER `begin_region`, so EVERY open overlay records a range that excludes its own region root — the trigger is universal, not scene-dependent. Permanent regression proof: `crate::ui_snapshot::overlay_fidelity_proof::bug097_render_sub_region_draws_root_excluding_overlay_that_render_tree_range_blanks` (GPU test, passing) opens a real overlay and proves on the SAME range that `render_tree_range` leaves the offscreen byte-identical (blank) while `render_sub_region` and the production seam both draw it. Reverting the seam to `render_tree_range` fails that test.

**Original report (kept for the record):** renumbered from BUG-094 on 2026-07-10 to resolve a concurrent-session ID collision (fluidsim3d's session independently claimed BUG-094 for `fluidsim3d-clip-trigger-turbulence-mux-double-wire`, now FIXED, further down this file). This overlay bug landed on main first (P2 @ `b8fa192f`) but has no code/test references, so renumbering it — rather than the fluidsim entry — orphans nothing but the immutable P2 commit message `5965d44d`.

**Symptom (potential, unconfirmed against a real repro):** `render.rs`'s Pass 5
(`ui_snap`'s `render_ui_to_png`, and `script.rs`'s `Runner::write_png` via the
shared `draw_immediate_passes`) draws each `overlay_draw` range with
`renderer.render_tree_range(&ui.tree, start, end)`
([`render.rs`](../crates/manifold-app/src/ui_snapshot/render.rs), Pass 5). The
live app's equivalent pass
([`app_render.rs:4617`](../crates/manifold-app/src/app_render.rs#L4617)) uses
`render_sub_region` instead, with an explicit comment: an overlay's
`(start, end)` deliberately EXCLUDES its own
`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` region root, and `render_tree_range` is a
root-scan (`UITree::traverse_range`) that finds no roots in such a range and
renders NOTHING — whereas `render_sub_region`'s flat, ancestor-aware scan
(`traverse_flat_range`) picks the children up regardless. The live app also
wraps each range in `push_depth`/`draw_shadow`/`pop_depth`, none of which the
headless pass does. **Not confirmed against an actual failing overlay** — no
scene exercised by the current test suite or the two shipped `ui-flows`
happens to open an overlay whose range excludes its own root, so this may be
latent rather than currently biting; it was found by reading the two code
paths side by side (the P2 phase brief's mandatory VERIFY-AT-IMPL diff), not
by a failing render. **Fix shape:** swap `render_tree_range` for
`render_sub_region` in `draw_immediate_passes`' overlay loop, matching the
live app; add the shadow/depth wrapping too if a future scene needs a modal
proven visually. Left unfixed this session (escalation, not a silent keep —
the P2 phase brief's Forbidden-moves list names this exact move) because
confirming which overlay ranges, if any, actually exclude their root wasn't
scoped to P2 and a same-session fix risked changing rendered output nobody
asked this phase to verify.

---

### BUG-098 (film-grain-drifts-and-reads-as-blocky-pixels) — FilmGrain's time jitter pans the hash field instead of re-rolling it, and the grain cells read as blocky pixels
**Status:** FIXED (bug/wave2-lane-b-filmgrain) — JSON-only, no new primitives. **The final "does this read as real film grain" call is Peter's, on the rig — this session only verified the two measurable/visual claims below, not the subjective one.**
**Symptom:** grain pattern visibly drifts toward the top-left corner; individual grains are hard square blocks, "not real film grain" (worst at 4K).
**Root cause (confirmed, reproduced before fixing):** two authoring mistakes in `FilmGrain.json`, unchanged since `8ac2e211`/`4c9b146e`. (1) The animation wired `time x 39.7 -> noise.offset_x` / `time x 61.3 -> offset_y` — a CONTINUOUS offset translates the hash lattice, so the pattern pans; measured: holding `frame_count` fixed and stepping wall-clock time by one frame (1/60s) between renders gave grain-layer Pearson correlation 1.0000 at both 1080p and 4K (effectively frozen/panning, not re-rolling). (2) `node.noise` type Random emits unfiltered square hash cells; at scale 1000 (fixed, resolution-independent) on a 4K canvas each cell spans ~3.8px — crisp squares, not emulsion, and the SAME fixed scale means the bug gets categorically worse at higher canvas resolutions (cell-to-pixel ratio grows with width).
**Fix (both root-caused, not patched):**
  1. **Re-roll instead of pan:** `grain_jitter_x/y` (`node.math`, still `op=Multiply`) now take `a` from two new `node.math Modulo` nodes (`grain_frame_mod_x/y`) wired to `system.generator_input.frame_count` instead of `.time` — `offset = (frame_count % CYCLE) * PRIME`. `CYCLE_X=127`/`PRIME_X=7919`, `CYCLE_Y=113`/`PRIME_Y=6571` (all prime): the per-frame jump (`PRIME`) is comfortably larger than any realistic noise scale so every frame decorrelates instead of sliding, the modulo keeps the offset magnitude bounded (~1e6 max) so it never grows into f32-imprecision territory over a long show (naive unbounded `frame_count * prime`, taken literally from the original fix-shape note below, would have re-introduced blocky/frozen grain after ~35s once the offset exceeded f32's 2^24 exact-integer range — caught by design, not by re-testing), and the two axes' coprime cycle lengths push the full 2D repeat period out to `127*113=14351` frames (~4 min @ 60fps).
  2. **Finer, resolution-relative cells:** `grain_size_to_scale` (`node.math`) now computes `px_per_cell = 1.2 * size` (was `1600/size`, a FIXED constant divorced from canvas resolution — the actual root cause of "worst at 4K"); a new `node.texture_size` node (`grain_canvas_size`, wired from `system.source`) reads the real canvas width, and a new `node.math Divide` (`grain_scale_from_width`) computes `scale = width / px_per_cell`. At the reference 1080p width (1920) this reproduces the exact old scale (1000, zero regression); at 4K (3840) it's now 2000 — 2x finer, matching the fix-shape's "~2x canvas density" — and it stays correctly-fine at ANY resolution instead of specifically-patched-for-4K.
  3. **Soften the hard-edge read:** two new `node.gaussian_blur` nodes (`grain_soften_h/v`, 9-tap, `step=0.25` ⇒ effective σ≈0.5px) sit between `grain_noise` and `grain_mono`, softening the Random type's unfiltered nearest-neighbor cell edges into a continuous texture without smearing away the fine structure (a full-strength blur would; the "Value-noise blend" alternative from the fix-shape note was passed over as a different noise character, not obviously more emulsion-like).
**Verification (this session, worktree only — see gate results in the landing commit):**
  - `check-presets`: 49/49 ok — confirms the JSON is well-formed and loads, **not** that it looks right.
  - New permanent regression test `crates/manifold-renderer/tests/gpu_proofs::film_grain_decorrelation::consecutive_frames_are_decorrelated_at_1080p_and_4k` (gated behind `gpu-proofs`): renders the `grain_mono` layer in isolation (not the full composite, which is dominated by the static source and would mask the grain's own correlation) at frame_count 500 vs 501, same wall-clock time held fixed so only frame identity differs. **Before fix:** correlation 1.0000 at both 1080p and 4K (reproduced by temporarily reverting the JSON in-worktree). **After fix:** 0.0016 (1080p) / -0.0004 (4K) — near-zero, decorrelated.
  - 4K render looked at directly (not just computed): pre-fix grain reads as a hard, high-contrast, ~4px checkerboard-like block pattern; post-fix reads as fine, soft, mottled texture with no visible cell edges, closer to film emulsion. PNGs are not committed (regenerate via the test: `cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs film_grain -- --nocapture`, writes to `target/gpu_proofs_out/`).
**Where:** `crates/manifold-renderer/assets/effect-presets/FilmGrain.json` (nodes `grain_jitter_x/y`, `grain_frame_mod_x/y` [new], `grain_size_to_scale`, `grain_canvas_size` [new], `grain_scale_from_width` [new], `grain_noise`, `grain_soften_h/v` [new]).
**Original fix-shape note (superseded by the above):** re-roll instead of pan — quantize the offset per frame (e.g. `floor(time * fps) * large_prime`, or hash the frame index) so consecutive frames jump whole cells; then make the grain read soft: finer cells (~2x canvas density) plus a half-pixel blur, or blend a Value-noise octave, and consider a luma-weighted response so grain sits in midtones (the Overlay blend already helps). All JSON-level; no new primitives expected.

---

### BUG-099 (design-tokens-raw-color-literal-count-drifted-past-baseline) — `manifold-ui`'s `no_new_raw_color_literals` ratchet test failed (baseline 200, live 201) — FIXED
**Status:** FIXED @ 54a80448 (SCENE_BUILD P2 landing, orchestrator). Found running P2's full workspace sweep gate; the P2 worker confirmed it pre-existed on the P2 base `ab215ab8` and logged it as unknown-root-cause drift (correctly, from the worker's render/migration scope). The orchestrator, owning the whole SCENE_BUILD wave, identified the actual cause: it was P1's own addition, masked because P1's landing sweep short-circuited on an inherited docs-index failure and wasn't re-run after that fix.
**Symptom:** `cargo test -p manifold-ui --test design_tokens` failed: `Raw Color32::new( count rose to 201 (baseline 200)`.
**Root cause:** SCENE_BUILD P1 added `PORT_TRANSFORM_COLOR: Color32 = Color32::new(255, 128, 199, 255)` at `graph_canvas/mod.rs:298` — an 8th port-pin-colour const, in exactly the same defined-once-const style as the seven grandfathered pin-colour consts beside it (Texture2D/3D, Scalar, Array, Camera, Light, Material). One new raw literal → count 201.
**Fix:** bumped `COLOR_BASELINE` 200→201 in `crates/manifold-ui/tests/design_tokens.rs` with a comment folding the new pin colour into the same pin-colour debt the §15 colour ramp will tokenise together (consistent with how the 2026-07-03 graph-editor pass re-baselined its own additions). Tokenizing one pin colour while its seven siblings stay raw would be inconsistent; all eight are §15-ramp debt.

---

### BUG-100 (gltf-fresh-import-renders-near-black-for-non-azalea-geometry) — a fresh glTF import of a non-azalea model renders near-black — FIXED 2026-07-11 (wave2 lane D)
**Status:** FIXED — root cause was NOT the sun/material tuning this entry originally named; that diagnosis was wrong. Real cause + fix below.

**Original symptom (2026-07-10):** `cc0__japanese_apricot_prunus_mume.glb` and `lowe.glb`, freshly imported via `assemble_import_graph`, both rendered as a legible silhouette with almost no lit surface detail when captured via `imported_azalea_renders_faithfully_to_png` (`MESH_SNAP_GLB` pointed at each). The originally filed root cause — "the synthesized sun's fixed `pos_x/y/z = 5,2,3`/`intensity = 3.5` were tuned against azalea's own scale, so a differently-scaled model gets the sun aimed edge-on or too dim" — read as plausible but was **never actually verified by rendering**; this session did that verification and it falsified the theory.

**Actual root cause (confirmed by rendering, not assumed):** the TEST HARNESS's convergence check, not the lighting rig. `imported_azalea_renders_faithfully_to_png`'s polling loop broke out as soon as `non_black_fraction > 0.02` — but `node.pbr_material`'s `ambient = 0.18` floor lights the ENTIRE silhouette from frame 1 regardless of whether `node.gltf_texture_source`'s background-thread base-color decode has landed yet, so `fraction > 0.02` is satisfied almost immediately by an ambient-only, still-textureless frame. Azalea's small texture happened to decode fast enough that nobody noticed; apricot's/lowe's larger textures took longer, so the harness captured (and asserted on) a genuinely under-textured frame and called it "the render."
**Proof this was the whole story:** with `assemble_import_graph`'s sun/material constants completely UNTOUCHED (verified via `git diff` = zero lines changed there), forcing the harness to poll longer turned both "near-black" captures into fully lit, richly textured renders — apricot's white blossom branches and lowe's stone lion statue, both correctly shaded. Scaling the sun's position (tried first, mirroring camera's `distance = 2.2 * radius`) changed NOTHING — `node.light` Sun mode is a pure directional light (`dir = normalize(aim - pos)`); `pos`'s magnitude only anchors the (here irrelevant) shadow ortho frustum. Disabling `cast_shadows` also changed nothing. Only forcing extra poll time — with zero lighting-code changes — fixed it.

**Fix:** `crates/manifold-renderer/src/node_graph/gltf_import.rs`, both `imported_azalea_renders_faithfully_to_png` and `imported_azalea_renders_through_create_with_override_to_png` — replaced the `fraction > 0.02` one-shot check with a stability gate: poll until the RGBA readback is byte-identical across 3 consecutive frames (`STABLE_STREAK`) AND non-black, since once every async parse has landed the render is a pure function of a static camera/geometry/light and stops changing frame to frame. `assemble_import_graph`'s sun/camera/material code is byte-for-byte unchanged from before this session.
**Commit:** (this branch, lane D wave 2) — see `crates/manifold-renderer/src/node_graph/gltf_import.rs` diff.
**Oracle:** rendered azalea/apricot/lowe through the fixed harness — azalea byte-identical to its pre-fix reference render (max channel diff 0 across the full 768×768 canvas); apricot and lowe both converge in <1s (attempts 8–15) to fully lit, richly textured PNGs, reproducibly across repeated runs.
**Open question, out of this session's scope:** whether the SAME async-decode-vs-first-paint race exists in the live app (a freshly dropped-in glTF generator layer showing a black/ambient-only frame for the first ~15 render calls before textures land) — this fix only touches the TEST harness's verification logic. If Peter sees a brief black flash on a real fresh glTF import before it pops in lit, that's the production side of this same mechanism and would need a genuinely new fix (e.g. blocking the first compositor frame on texture decode, or the BUG-037 pre-warm pattern) — worth a quick live-rig check, not filed as a new bug since it's unconfirmed.

---

### BUG-103 (outer-routings-drop-bindings-that-target-a-node-inside-a-group) — card bindings whose target node lives inside a group never surface in the graph canvas's `outer_routings`, so glTF-imported object groups showed no D6 mirror rows — MED (D6 group-face rows inert for exactly the imported-scene case the SCENE_BUILD wave exists for)
**Status:** FIXED @ 9384d080
**Escaped:** feat/scene-build-p4 (D6 group-face-rows phase) · caught-by: demo (building the `gltfeditor` ui-snap scene to prove D6 on the real glTF import surfaced it — the two per-object groups showed their interface ports correctly but zero mirrored param rows)
**Symptom:** open the graph editor on a freshly glTF-imported generator layer: the perform card correctly shows every exposed slider incl. per-object Metallic/Roughness, but the canvas never shows the "↳ <outer label>" hint on the driving inner-node row, and the object's group box shows no D6 mirror row — because the canvas's `outer_routings` for that binding is missing entirely.
**Root cause (corrected — the first diagnosis was wrong):** NOT a per-instance-user-binding problem. A pristine glTF import is `graph: None` and has ZERO per-instance user bindings; the Metallic/Roughness bindings were always in the canonical embedded def's `preset_metadata.bindings` (the importer emits all 13 there — `gltf_import.rs:503-509`). The real cause: `outer_routings_from_view` (`loaded_preset_view.rs`) — the resolver the pristine snapshot arm runs via `ContentThread::graph_snapshot` → `snapshot_for_view` — built its `node_id → handle` map from **top-level `canonical_def.nodes` only**, never recursing into group bodies. The importer puts each object's `mat_k` material node (the binding target) INSIDE that object's group box (`gltf_import.rs:442,488` — `plain_node(mat_id, &mat_node_id, …)` pushed into `group_nodes`), while camera/sun/envmap sit at the top-level spine. So the 9 spine bindings resolved and the 4 in-group `mat_0`/`mat_1` metallic/roughness bindings were silently dropped. Verified empirically before the fix: the real azalea import resolved 9 of 13 routings; after, 13 of 13. The diverged generator arm (`content_thread.rs`) had the identical top-level-only bug (a diverged imported scene would lose the same rows).
**Fix:** `outer_routings_from_view` now collects handles recursively into group bodies via a new shared `collect_node_handles` helper (`loaded_preset_view.rs`); the diverged arm calls the same helper instead of its inline top-level-only map. Inner handles stay unprefixed in the grouped display def (the flattener's group-name prefixing is a runtime-build step, not applied to this display def), so a routing's `node_handle` matches the D6 group-face join (`model.rs`'s `find_node_by_handle`) exactly. Regression test `loaded_preset_view::tests::gltf_import_group_material_bindings_resolve_through_groups` drives the REAL azalea importer + the REAL resolver, asserting 13/13 with `mat_0`/`mat_1` metallic/roughness present. Proven on-screen: `ui-snap gltfeditor` — both object group boxes now carry their Metallic/Roughness slider rows on the group face (`/tmp/scene-build-p4/gltfeditor-fixed.png`).

---

### BUG-104 (audio-trigger-takes-over-shared-param-mod-goes-dead) — with a Trigger enabled, audio modulation on a card param that the graph's trigger option also drives goes DEAD, and stays dead after disable — MED
**Status:** FIXED 2026-07-11 (Sonnet, `bug/104-trigger-takeover`, all 5 parts landed) — see the landing summary near the end of this entry for commits, tests, and the two items still owed to Peter (the `trigger_modulate` idiom name, the Lissajous snap feel on the rig). Originally reported by Peter 2026-07-10.

**Symptom:** this is a takeover, not a leak. Peter had audio modulation on a card param that is also one of the params the graph's trigger-parameter option can drive; with a Trigger enabled, that param **stopped responding to its card modulation** — and **stayed broken after the Trigger was turned off**. The persistence is the strongest datum: whatever breaks the param is state that outlives the trigger (graph-side per-port state, the mod's own config, or a corrupted base), not a read that only diverts while the trigger is live. Still unpinned: which generator and which param pair, and whether the CARD value still moves while the visual doesn't (separates a graph-side takeover from an engine/UI-side one).

**Root cause:** unknown — suspects reweighted for the dead-not-leaking symptom:
1. **Preset wiring — trigger path replaces instead of composes** (primary): in graphs where the trigger option muxes onto a user-exposed param (BUG-094 family, e.g. FluidSim3D `noise_factor` = turbulence slider × `mux_noise` envelope), the trigger-selected input can shadow or zero the exposed binding while the trigger is enabled — between fires the envelope's resting value dominates the product/mux, so the modulated slider value never reaches the kernel. Predicts: card value moves, visual doesn't.
2. **Engine — `is_trigger` hijack of the mod arm:** any audio mod targeting a param whose spec is `is_trigger` (graph binding convert = Trigger) skips Continuous entirely and writes `base + fire_count` (`modulation.rs:550`). If the shared param carries that flag, its card mod can never modulate continuously by construction. Predicts: card value frozen/stepping, cheap to check once the param is named.
3. **UI state:** the inspector AUDIO TRIGGERS section fabricates synthetic `clip_trigger_{i}` ParamIds and shares `ParamModState`/drawer machinery with `ParamCardPanel` (`audio_trigger_section.rs:189`) — an id/index collision could land the trigger's enable/config on the param mod's row, silently disabling or mispointing it. Predicts: the mod's drawer state looks wrong after enabling the trigger.

**Persistence lens (applies across all three suspects):** the break surviving trigger-off means the disable path fails to restore something. Check what trigger disable actually resets — `clear_all_trigger_edges` re-arms edges, but does anything reset the graph-side envelope/mux per-port state (state store holds the last value once the trigger stops advancing — see `effect-chain-state-caches`: reset must walk both caches), restore `p.base` if takeover writes were ever committed into it, or undo a misrouted config write from suspect 3? Also worth one check: does anything recover the param if Peter re-saves/reloads or resets the effect — that scopes which cache the stale state lives in.

**Fix shape:** reproduce with Peter's setup, watch the card value vs. the visual to pick the level, then log `evaluate_instance_audio_mods` writes (engine) or probe the graph per-stage (wiring) before changing anything. If (1), the fix is the compose-not-replace rewire in the preset (and the same audit across the other trigger-option presets) plus whatever state reset the disable path is missing; if (2), the flag/arm needs to distinguish "fire-button param" from "continuous param a trigger also drives".

**Instrument impact:** a fader Peter is riding stops being his mid-set and does not come back when he backs the trigger out — unrecoverable-live breakage on the perform surface, which is why this stays MED despite the pending repro.

**Investigation 2026-07-11 (Lane C, wave1 — INVESTIGATE-ONLY, no fix landed).** Could not reproduce on the rig (no live rig here); repro built by static graph analysis of every trigger-option preset + a focused engine harness test (`modulation::tests::bug104_continuous_card_mod_is_decoupled_from_trigger_reset`, passing). Verdicts:

- **Suspect 2 (`is_trigger` mod-arm hijack) — FALSIFIED (comprehensive, computable).** `param.spec.is_trigger` is derived ONLY from `binding.convert == ParamConvert::Trigger` at instance-build time (`effects.rs:792/1875/2064`) and is static — nothing flips it when a trigger is enabled at runtime. A scan of all shipped generator+effect presets found **zero** bindings with `convert: Trigger` and **zero** params declaring `isTrigger: true`. So there is no card-modulatable param whose spec is `is_trigger`; the `is_trigger` arm (now `modulation.rs:571`) can never intercept a card mod on any shipped preset. The sibling `is_trigger_gate` arm (`:544`) never writes `p.value` (it only pushes pulses), and no arm couples to trigger enable/disable state. The engine harness confirms a Continuous card mod on a Float param tracks its signal every tick and survives `clear_all_trigger_edges` untouched (`m.enabled` and `p.value` are never cleared by it).

- **Suspect 1 (trigger mux replaces instead of composes) — SURVIVES as a mechanism, but the NAMED candidate is FALSIFIED.** FluidSim3D's noise path is `noise_final = turbulence(a) × noise_factor`, `noise_factor = 1.0 + mux_noise_out` (math ops verified: 0=Add, 2=Multiply). At envelope rest the mux output is 0, so `noise_final = turbulence × (1 + 0) = turbulence` — a genuine multiply-compose with a +1 identity offset; the turbulence card mod ALWAYS reaches the kernel (×1 minimum), the trigger boosts on top. It does NOT shadow or zero the fader. The real replace-shapes are elsewhere: `switch_value` muxes whose selector is trigger-driven and one input is a user binding — **ConcentricTunnel `mux_final_n_sides`** (selector `shapeActive`; in_0 = user 'Shape', in_1 = trigger shape-cycle), **MriVolume `axis_mux`** (in_0 = user 'Folder' axis, in_1 = trigger axis-cycle), **Wireframe `shape_mux`** (selector = 'Clip Trigger'). Every one of these shadows a **discrete SELECTOR** (shape/axis/pattern index), NOT a continuous fader. No preset was found where a trigger mux replaces a continuous fader — which is in tension with Peter's "a fader I'm riding goes dead" wording.

- **Suspect 3 (synthetic `clip_trigger_{i}` ParamId collision) — structurally WEAK.** `audio_trigger_section.rs:190` fabricates `clip_trigger_{i}` ids but owns a **separate** `ParamModState` instance (`:232`, `ParamModState::allocate(n)`) from `ParamCardPanel`; no shipped preset param is named `clip_trigger_<number>` (scanned, zero hits), so the ids can't alias a real generator param. It is also aimed at LAYER-level clip triggers, not the generator's own graph trigger the report describes. Its prediction ("drawer state looks wrong") is a running-UI observation not reachable from a headless harness — left UNTESTED at the visual level, but the id/index-collision mechanism is falsified.

- **Persistence lens — real cross-crate reset gap identified.** `clear_all_trigger_edges` (`modulation.rs:731`, called on transport stop, `engine.rs:593`) walks ONLY the playback-side `ParameterAudioMod.trigger_edge`/step shadows. It cannot touch the RENDERER's graph-side StateStore, where `clip_trigger_cycle`/`clip_trigger_index`/`sample_and_hold` latch their last value. For the selector-mux replace-shapes, that latch is exactly what would keep the mux pinned to the trigger-cycle input after the trigger stops firing — a persistence path that no playback-side reset can reach. Not proven at runtime (needs a renderer graph run across enable→disable), but it is the structurally correct home for "stays dead after disable" IF the takeover is graph-side.

**Sharpest open question for a rig repro (the one datum that forks the diagnosis):** with the trigger enabled and the visual dead, **does the card/fader value still animate in the UI, or does it freeze too?** — AND which generator + which param (a continuous fader, or a shape/axis/pattern SELECTOR)? If the card value still moves and the param is a selector → confirmed graph-side replace + StateStore-latch persistence (fix = compose-not-replace rewire on the specific mux + a graph-state reset on trigger disable). If the card value ALSO freezes on a continuous fader → it is NOT the engine Continuous path (proven decoupled) and NOT any replace-shape found (they only hit selectors), so the hunt moves to a UI-side disable/mispoint of the mod itself. Scratch harness + this analysis on branch `bug/wave1-lane-c-trigger-hunt`.

**Review 2026-07-11 (Fable).** Every citation, mechanism, and falsification above re-verified against the code (harness test re-run in the lane worktree: passes) — EXCEPT the negative claim, which is **FALSE**: "no preset where a trigger mux replaces a continuous fader" came from a scan that only checked direct param→mux bindings and missed transitive wires across node-group boundaries. **Lissajous is the counterexample:** in its "Frequency Selection" group, `mux_x`/`mux_y` take the user's 'Clip Trigger' as selector, the trigger-cycled `frequency_ratio` on `in_1`, and the Oscillator Bank's LFO outputs on `in_0` — the LFOs whose rates are the continuous **'Freq X Rate' / 'Freq Y Rate'** faders. Trigger on → both continuous faders dead, exactly the reported symptom, so the "tension with Peter's wording" above doesn't exist and the open-question fork is miscalibrated on one branch: a dead continuous fader does NOT imply UI-side. The enumeration was also incomplete: Plasma `pattern_mux` (shadows user 'Pattern'), StrangeAttractor `type_mux` ('Attractor Type'), and BasicShapes' gated muxes ('Fill') have the identical trigger-selected replace shape (all discrete).

**Root fix (Fable 2026-07-11 — three parts, sized for a Sonnet fix session):**
1. **Lissajous: compose, don't replace.** Rewire `mux_x`/`mux_y` so the trigger ratio composes with the LFO frequency path instead of switching it out. **Composition point matters — CORRECTION 2026-07-11 (Sonnet, landing session): this was Fable's design recommendation, not something Peter confirmed; the "(Peter 2026-07-11)" attribution below was wrong.** Preserve the snap: today's replace gives crisp on-beat jumps between figure families; a naive additive boost (`1 + trigger_term`) would smear that. Compose by MULTIPLYING the user-driven frequency by the cycled ratio (`freq = user_path × ratio`, ratio held at identity 1 when the trigger is idle/disabled) — the trigger still snaps figure families on the beat, the faders still sweep underneath. FluidSim3D's noise path remains the template for the *identity* idea, not the exact formula. Acceptance: with Clip Trigger enabled, riding Freq X/Y Rate still visibly changes the curve AND triggered figure changes stay crisp. Then walk the discrete replace-muxes (Wireframe `shape_mux`, MriVolume `axis_mux`, ConcentricTunnel `mux_final_n_sides`, Plasma `pattern_mux`, StrangeAttractor `type_mux`, BasicShapes gated muxes) and decide per-preset whether replace is the intended semantics for shape/pattern/axis cycling — either is fine, but record the decision in the preset description; no silent takeover.
2. **Graph-side trigger-state reset.** Close the cross-crate gap the persistence lens identified: disabling the trigger option (and transport stop) must return the graph to the user path. The renderer StateStore latches (`sample_and_hold` `last_trigger`/`held`, `clip_trigger_cycle`) survive both, and playback's `clear_all_trigger_edges` structurally cannot reach them. Fix at the mechanism level — either wiring that gates every hold by the trigger-enable so disable decays to the user input, or a scoped StateStore reset for trigger-derived nodes on disable. Read `docs/EFFECT_CHAIN_LIFECYCLE.md` (state-cache eviction) and `docs/FREEZE_COMPILER_MAP.md` first; whatever ships must hold on the freeze/fusion path too.
3. **Class-guard test.** Encode the review's scan as a workspace test so the class can't come back: no `switch_value` whose selector derives from a trigger source may — transitively, tracing wires across `group_input`/`group_output` boundaries — shadow a user binding on a continuous param (discrete/toggle/whole-number targets allowlisted per the part-1 decisions). The direct-binding version of this scan is exactly what produced the false negative above; the transitive version found Lissajous in seconds.

**Amendment (Peter + Fable, 2026-07-11 — makes the fix hold for user- and MCP-authored graphs, not just shipped presets):**
- **Safe vocabulary, audit-first.** The compose pattern must be a named, discoverable thing authors reach for, not a three-node idiom (mux + const-one + multiply) they have to know. Run the §2.5 audit BEFORE deciding its shape: if the semantics are genuinely new, add the primitive (contract: user value passes at identity while the trigger is idle, trigger composes on top while firing, fully releases on disable — no surviving StateStore latch); if the audit finds it one wire away from existing atoms, name and document the idiom instead of adding a redundant atom. **Naming (Peter 2026-07-11): NOT "blend"** — vetoed, reads as crossfade and collides with compositing blend modes. Working candidate `trigger_modulate` (matches the card-mod vocabulary: a mod composes, never replaces); final name is Peter's call at fix time. Whatever ships MUST be on the freeze/codegen path (Peter 2026-07-11: every node fusable, no plain hand-WGSL).
- **Validation at the mutation gateway, not the editor.** The part-3 transitive scan becomes a shared check with two runtime consumers plus CI: (a) the workspace test over shipped presets (hard fail); (b) graph-mutation validation in the graph-command layer (`EditingService` path), so EVERY authoring surface — editor UI, MCP clients, agents — gets the same warning when a wiring makes a trigger-driven selector shadow a continuous user binding, with the safe node suggested. Editor-only linting would miss exactly the MCP/agent case.
- **Part 2 is the stage-safety floor and is authoring-independent:** once trigger disable reliably releases graph-side trigger state, even a takeover wiring no scan ever saw recovers when the performer backs the trigger out — worst case becomes "overridden while enabled" (a performance choice), never "unrecoverable live."
- The deepest version — takeover composing at the param-binding layer where card mods already compose, so graphs never mux user bindings at all — is deliberately NOT in this fix; it's logged to the binding-unification design track.

**Fix landed 2026-07-11 (Sonnet, branch `bug/104-trigger-takeover`, all 5 parts) — Status: FIXED.**

- **Part 1 (graph-side trigger-state release) — LANDED, commit `dcc075bc`.** `EffectNode`/`Primitive` gained `is_trigger_latch()` (default false), flagged `true` on the seven trigger-edge-latch primitives (`sample_and_hold`, `clip_trigger_cycle`, `clip_trigger_index`, `frequency_ratio`, `cycle_table_row`, `trigger_gate`, `trigger_ease_to`); `StateStore::cleanup_nodes` purges exactly those nodes' buckets; `PresetRuntime::clear_trigger_state()` walks the graph and releases them without touching feedback/particle/mip state a full reset would nuke; `GeneratorRenderer::clear_all_trigger_state()` runs it across every live generator, wired into `ContentCommand::Stop`/`LoadProject` (the same "kill the trigger" moments `clear_all_trigger_edges` already covers on the playback side). Closes the "stays dead after disable" half of the symptom for every trigger-latch primitive in the library, not just Lissajous.
- **Part 2 (compose vocabulary) — LANDED, commit `9462498b`.** §2.5 audit: genuinely NOT a new primitive — `node.switch_value` (identity default on the idle branch, e.g. `in_0: 1.0`) + `node.math` (Multiply) already compose exactly this, matching FluidSim3D's `vol_factor_mux`/`noise_factor` precedent. Documented as the `trigger_modulate` idiom in `docs/DECOMPOSING_GENERATORS.md` §4.1 — **name unconfirmed by Peter, flagged below.**
- **Part 3 (Lissajous rewire) — LANDED, commit `26856e59`.** `mux_x`/`mux_y` now hold identity (`1.0`) while idle instead of being wired to the continuous LFO output; new `freq_x_scale`/`freq_y_scale` (`node.math`, Multiply) multiply the mux's output onto the continuous `freqX`/`freqY` path. Verified: `check-presets` (52/52), the pre-existing `fan_out_binding_writes_every_target_with_the_same_outer_value` regression test (unchanged, still passes — mux_x/mux_y keep their selector wiring), the full `gpu-proofs` suite (fusion coverage unaffected), and a headless render (`render-generator-preset`) at two very different Freq X/Y Rate values with Clip Trigger firing every 60 frames — the curve visibly differs between them (fader alive) and the retrigger points still read as crisp lobed transitions. **Snap feel on the real rig unconfirmed by Peter, flagged below.**
- **Part 4 (discrete replace-mux audit) — LANDED, commit `ad3522d0`.** Transitive audit (wires AND `presetMetadata.bindings` — the direct-binding path a prior scan missed on Lissajous) found 9 more presets with trigger-driven `switch_value` nodes: BasicShapes, ConcentricTunnel, Wireframe, MriVolume, Plasma, StrangeAttractor, FluidSim2D, FluidSim3D, ParticleText. Every one selects a discrete option (shape/axis/pattern/attractor-type/simulation-mode enum), never a continuous fader — "replace" is correct there. Each preset's `description` now records the decision by name (no silent takeover). Garden/Lathe/Vine (the newer presets called out for the check) have zero `switch_value` nodes.
- **Part 5a (class-guard test) — LANDED, commit `100fcb61`.** `crates/manifold-renderer/tests/trigger_shadow_class_guard.rs` — GPU-free workspace sweep over every generator+effect preset, `flatten_groups`-based (closes the group-boundary false negative), allowlist mirrors Part 4. A synthetic regression test reproduces the exact pre-fix Lissajous shape and proves the guard flags it.
- **Part 5b (mutation-gateway warning) — LANDED, commit `5a2da3a1`.** Infra inventory first: `Command::execute` is void-return with no per-command result channel (changing it would touch every command impl — disproportionate). Used the smallest existing channel instead: `ChainError::TriggerShadowsContinuousBinding`, pushed from `PresetRuntime::from_def` — the single construction path every generator (re)build goes through regardless of caller (editor, MCP via `EditingService`/`MutateProject`, agent-authored graphs, `check-presets`, thumbnails, freeze proofs). Shared logic lives in `node_graph::trigger_shadow_lint` (one implementation, two consumers — the sweep test and this). **Scope boundary, documented not silent:** effect-chain per-instance graph overrides aren't wired to this yet (the chain-splice path builds from multiple spliced effects, not one preset's own graph, and threading the check through that ~5000-line state machine was out of scope this pass); generators — BUG-104's own domain — are fully covered.
- **Gates:** both landings (parts 1–3, parts 4–5) went through the full fetch/merge-origin/gate/`merge --no-ff`/push loop — worktree gate (scoped clippy, focused nextest, `check-presets`, `gpu-proofs` feature build) then the full-workspace gate in the main checkout (`cargo clippy --workspace`, `cargo nextest run --workspace`, `cargo deny check bans`) — all clean both times.

**Two items owed to Peter — not yet confirmed, need his call:**
1. **The `trigger_modulate` idiom name.** Working name only (Part 2 / `docs/DECOMPOSING_GENERATORS.md` §4.1) — needs Peter's confirmation or a rename pass.
2. **Lissajous's multiplicative-compose "snap" feel on the real rig.** The headless render shows the retrigger points still reading as crisp lobed transitions rather than a smear, and Freq X/Y Rate audibly changes the curve while triggered — but "does it still SNAP the way it used to" is an ear/eye call on the actual instrument, not something a render script can certify. Owed to Peter's rig pass.

---

### BUG-105 (graph-node-slider-no-right-click-reset) — node-face param sliders don't reset to default on right-click — LOW
**Status:** FIXED 2026-07-11 (`bug/wave2-lane-a-cardui` c41132dc, tests in the same commit) — found by Peter on the rig 2026-07-10. Root cause confirmed exactly as pinned below; fix matches the pinned shape.

**Symptom:** right-clicking a slider on a node face in the graph editor does not reset the param to its default, unlike every card/panel slider in the app. On a param row exposed as a card binding, right-click opens the mapping popover; on an unexposed row it does nothing.

**Root cause:** the graph canvas draws node param sliders through `BitmapSlider::draw` — `graph_canvas/render.rs:879` calls it "the immediate-mode twin" of the retained builder — so they look identical to card sliders, but their interaction is canvas-owned: left-press starts a bespoke `DragMode::ParamScrub` (`graph_canvas/interaction.rs:766`) and right-press routes the whole row through `on_right_button_down` → mapping popover (`interaction.rs:307`). The app-wide reset contract — `chrome/diff.rs:294` registers `Gesture::RightClick → SliderReset` on every materialised slider track — lives in the retained intent registry, which this immediate-mode surface never touches. Peter's diagnosis ("not the same fundamental slider objects") is exactly right at the interaction layer; the visual twin is what makes the missing gesture read as a broken slider rather than a different widget.

**Fix shape:** mirror the card's hit-zone contract on the node face. The card already splits right-click by zone — label → mapping (`slider.rs:142`), track → reset (`chrome/diff.rs:294`). In `on_right_button_down`, when the hit row is a numeric ranged param and the x lands in the track zone (right of the label cell), emit the same command the scrub already commits — `SetGraphNodeParam` with the snapshot's `default_value` (already carried, `graph_view.rs:124`), or `SetOuterParam` for a group-face mirror row — and keep the label zone on the mapping-popover path. Skip wire-driven rows (read-only, same guard as the scrub). This restores the canvas's own stated parity invariant (`interaction.rs:748`: "Every branch emits the same command the sidebar did (parity); only where you click moves"). Related: BUG-070 — same missing-intrinsic-reset class on other non-retained surfaces (Audio Setup steppers, overlay send-fader).

**Instrument impact:** authoring-surface only (the graph editor is authoring, not perform), and the value is recoverable by scrubbing or typing — but right-click-reset is muscle memory from every card slider, and the node slider is visually indistinguishable from them, so the gesture silently failing reads as breakage mid-authoring. LOW.

**Fix:** new `param_slider_track_x` hit-test helper (`graph_canvas/hit.rs`) mirrors `render.rs`'s exact slider geometry (`slider_x = node_x + PARAM_LABEL_X * zoom`, `+ PARAM_SLIDER_LABEL_W * zoom` = track start) so the zone boundary can't drift from the drawn label. `on_right_button_down` now checks: numeric ranged param (`p.scrub.is_some()`), click x in the track zone, not wire-driven → emits `SetGraphNodeParam`/`SetOuterParam` with `p.default_value` and returns `None` (skips the mapping-popover path for that click); otherwise falls through unchanged to the existing row-hit return.
**Verified:** two focused tests in `graph_canvas/tests.rs` — `right_click_track_zone_resets_numeric_param_to_default`, `right_click_label_zone_still_reports_row_hit_for_mapping_popover` — both confirmed red-before/green-after by temporarily reverting the fix and re-running. `cargo test -p manifold-ui --lib` (670 passed), clippy clean.
**Still owed to Peter's own eyes:** try right-click-reset on a live node-face slider in the actual graph editor window (interactive click, not the harness) — the tests drive `on_right_button_down` directly and a `cargo xtask ui-snap graph` render confirms the sliders draw correctly, but neither exercises real mouse-event dispatch end-to-end.

---

### BUG-106 (audio-mixdown-analysis-only-test-order-flaky) — a playback mixdown test fails intermittently inside the full workspace sweep but passes deterministically alone — FIXED @ `78e97d4a`
**Status:** FIXED @ `78e97d4a` — same shared-state bug as BUG-090/BUG-074, see BUG-090 for the root cause writeup and verification. `bug-wave1-lane-d-test-hygiene`, 2026-07-11.
**Symptom:** `audio_mixdown::tests::render_export_audio_analysis_only_layer_taps_but_never_hits_master` panicked at `crates/manifold-playback/src/audio_mixdown.rs:678` once inside `--workspace`; re-running `-p manifold-playback` (228 ok) and the test in isolation both pass. Non-deterministic across sweep runs.
**Root cause:** confirmed — `tempfile_dir::TestDir::new`'s uniqueness key (`prefix_pid_nanos`) collided across near-simultaneous calls from different test threads sharing the same `pid`; not order-dependent global state as originally suspected. Full mechanism in BUG-090.
**Fix shape:** applied — per-process atomic sequence number added to the TestDir path, guaranteeing uniqueness regardless of clock resolution.

---

### BUG-108 (effect-card-add-effect-button-floats-over-sectioned-rows) — "+ Add Effect" renders mid-card over the Sun rows instead of at the bottom, on a sectioned glTF-scene card — MED
**Status:** FIXED 2026-07-11 (`bug/wave2-lane-a-cardui` 33fc99b8, class-kill test in the same commit) — reported by Peter on the rig 2026-07-10 (screenshot).
**Symptom:** on a glTF-imported scene's effect card — SCENE_BUILD P3 section headers (QS1694/Material.001/Camera/Sun/Environment) with the P3b `AUDIO TRIGGERS` section stacked above — the full-width "+ Add Effect" bar draws MID-CARD, overlapping the Sun Y / Sun Z rows, rather than at the bottom below the last row. Card reads as broken.
**Root cause (confirmed against current main):** suspect (a) — `ParamCardPanel::effect_body_natural_height`/`compute_height_generator` (`crates/manifold-ui/src/panels/param_card.rs`) summed `param_info` linearly, blind to the D5 section-header bar every section run draws (`build_section_header`, `ROW_HEIGHT + ROW_SPACING`) and to a folded section's rows painting nothing. This undercounted a sectioned card's true drawn height, so `layer_column_height()` (which sums each card's `compute_height()` to place the button, `inspector.rs`) landed the button mid-card. Suspects (b)/(c) ruled out: `layer_column_height()` already summed `audio_trigger_section.height()` unconditionally, and there is no sticky/overlay button in this path.
**Also:** the mangled "ừ" glyph prefixing each section header is a SEPARATE bug — BUG-107 (font-coverage/fallback rasterization), triggered here by the section header's "▾"/"▸" disclosure-triangle glyph (outside Inter's coverage). Both fixed this session.
**Honesty note:** this floating "+ Add Effect" was VISIBLE in a prior session's own P3/P4 verification PNGs; that orchestrator saw it and wrote it off as fixture noise instead of a real layout defect. The harness rendered it faithfully; the miss was in the looking, and no bounds-overlap assertion existed to catch it programmatically.
**Fix:** `effect_body_natural_height`/`compute_height_generator` now walk `section_runs()` the same way `build_effect`/`build_generator`'s own draw loops do — header row height added per section run, folded runs skip their rows' height, matching the draw loop's fold-skip exactly.
**Class-kill:** `docs/UI_LAYOUT_INVARIANT_LINTS_PROPOSAL.md`'s full P1 (the generic intra-stratum overlap lint, `painted_rect_of`, 15-scene registry) has NOT shipped yet — it's a separate session-sized deliverable per that doc's own phasing, not something to build inline with a 3-bug wave fix. Delivered instead the narrower thing I3 actually names: `add_effect_button_does_not_overlap_sectioned_card_last_row` (`inspector.rs` tests) — an anchored assertion reading REAL painted bounds from the tree (two small `pub` accessors added to `ParamCardPanel`: `param_row_rect(tree, param_id)`, `section_header_ids()`), not just re-checking the height formula against itself. Verified red-before/green-after by temporarily reverting the fix. `docs/UI_LAYOUT_INVARIANT_LINTS_PROPOSAL.md`'s P1/P2 remain open follow-up work for whoever picks up that design.
**Verified:** unit test above (red before fix / green after, confirmed by temporary revert); visual, via the real `gltfeditor` ui-snap fixture (`cargo xtask ui-snap gltfeditor`) — before: button overlaps Sun Y/Sun Z; after: clean, button sits below Environment/Reflections. `cargo test -p manifold-ui --lib` (670 passed) and `-p manifold-app --bin manifold --features ui-snapshot` (171 passed) both green; clippy clean on both crates.
**Still owed to Peter's own eyes:** look at the sectioned glTF card live on the rig, not just the fixture PNG — the fixture uses the same real import path (`assemble_import_graph` + `ImportModelLayerCommand`) but a synthetic card config with a "Sun" section could still miss some rig-specific detail (real audio-trigger state, a different section arrangement).

---

### BUG-109 (fire-meter-dead-in-all-transport-states) — the D6 fire meter has never displayed a clip-trigger level: wiped every playing tick, never evaluated while stopped — MED-HIGH
**Status:** FIXED 2026-07-11 — `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7 P5, wave 2. **Plumbing proven by unit tests + a synthetic-level PNG; the live crossing on a real device is Peter's L4 feel-pass, not yet run — VD-025 stays open until he confirms it** (the exact overclaim this bug is about, not repeated here on purpose).

Reopened the substance of BUG-082 (whose P3c closure was false — see that entry's 2026-07-11 annotation). Three breaks: (1) **playing** — P3c (`12fbc37d`) put the per-tick `FireMeterCapture` reset in `tick_playing`'s modulation step ([`engine.rs:901`](../crates/manifold-playback/src/engine.rs#L901)), whose comment claims clip triggers evaluate "below"; they evaluate at step 3b ([`engine.rs:844`](../crates/manifold-playback/src/engine.rs#L844), placed by `d285663f` so fired clips start same-frame before step 4's `sync_clips_to_time`) — so trigger levels are pushed then wiped, every tick. The stale `tick_audio_triggers` doc comment ("called after modulation") and CORE_ENGINE_MAP's correct frame diagram disagree; the worker trusted the comment. (2) **stopped** — `tick_non_playing` never calls `tick_audio_triggers`, so the meter is zero in exactly the tune-a-trigger-at-soundcheck state the D6 meter was built for. (3) **latent** — even with (1)+(2) fixed, a conditioned transient decays in ms; without UI-side peak-hold the fill is invisible at snapshot cadence.

**Fixed (P5):** (1) the `FireMeterCapture` reset moved to ONE call at the top of `PlaybackEngine::tick`, before either branch's evaluators run, replacing the two mid-branch resets. (2) `LiveTriggerState::evaluate` was split into a shared `walk` helper behind `evaluate` (fires) and the new `evaluate_meter_only` (pushes the identical `condition()` signal, never advances `TransientEdge`, never fires); `tick_non_playing` calls it whenever the project has an active clip trigger. (3) `MeterIds` (`crates/manifold-ui/src/panels/drawer.rs`) grew `Cell<f32>` peak-hold state — instant attack, `PEAK_HOLD_SECONDS = 0.25` hold, `PEAK_DECAY_PER_SEC = 5.0` fall — threaded a `dt: f32` UI-frame-delta parameter down through `update_fire_meters` on `ParamCardPanel`/`AudioTriggerSection`/`InspectorCompositePanel`/`UIRoot` from `app_render.rs`'s existing `frame_timer` value. Also fixed the stale `tick_audio_triggers` doc comment and updated `CORE_ENGINE_MAP.md` §3/§6. **Gate:** `playing_tick_leaves_the_clip_trigger_level_in_fire_meters` and `stopped_tick_pushes_the_level_and_fires_no_clip` (`crates/manifold-playback/tests/engine_tick.rs`) are the regression the bug made impossible before; `evaluate_meter_only_pushes_the_level_but_never_advances_the_edge` (`live_trigger.rs`) proves the edge never moves while stopped. `cargo xtask ui-snap inspector` with a synthetic `Some(0.9)` fed through the real `update_fire_meters` API (not a live `FireMeterCapture` — that path is the engine tests' job) shows the Strobe card's Amount meter filled ~90% and bright — evidence in the landing report.

---

### BUG-119 (timeline-layer-flickers-intermittently) — clip-thumbnail atlas triple-buffer clear-during-read race — HIGH, FIXED at root this session
**Status:** FIXED (root-caused + fixed this session; pending Peter's visual confirm on a heavy scene). Companion fix `3c108f24` (paused-recapture save loop, same hunt) already landed separately.

**Symptom** — the timeline layer clip rendering sometimes flickers rapidly ("flicks like
crazy"), reading as a blue/navy strobe over part or all of a layer's filmstrip. Correlated
with GPU saturation (heavy instanced 3D scenes, high density); render FPS observed
oscillating 60->30->60 in cycles while it happens.

**Root cause (found this session)** — the clip-thumbnail atlas was a private content-owned
"persistent" texture PLUS three rotating IOSurface copies (`clip_atlas_textures`, a
`SharedTextureBridge`, `SURFACE_COUNT = 3`). Every atlas change re-painted the FULL
8192x1152 texture onto the next rotating write surface via a `MTLLoadAction.Clear` full-copy
blit (`fill_clip_atlas`'s "Clip Thumbnail Atlas Publish" draw) while the UI thread
concurrently sampled whichever surface `front_index` pointed at. Under GPU saturation the
content thread falls behind the fence that gates surface reuse; the ring wraps and the
writer's `clear=true` publish blit clears the EXACT surface the UI is mid-read on — a
whole-layer flicker to blue. The 2026-07-11 flicker-probe hunt below (preserved for
history) never caught it because every probe counter instrumented writer-side bookkeeping
(captures, layout rebuilds, propagation window) — none of them watched the read/clear edge
itself, which is a cross-thread race, not something a counter on one side can see.

**Fix (root, this session)** — replaced the triple-buffer ring with ONE IOSurface-backed
texture (`shared_texture::SharedAtlasSurface`) imported by BOTH threads, so the content
thread's persistent atlas and the UI's sampled texture are the same surface. Four
invariants, all shipped: (1) exactly one clip-atlas texture — no rotation, no publish copy,
no front-buffer flip (`clip_atlas_textures`/`clip_atlas_bridge`/`clip_atlas_propagate` all
deleted); (2) no clear after the one-time init clear at surface creation — every cell blit
is `LoadAction::Load`, so the deleted publish blit was the only `clear=true` write the atlas
ever had; (3) layout never leads pixels — a (re)allocated cell's layout is stashed in
`clip_atlas_pending_layout` UNSTAMPED, stamped with that frame's real GPU signal value right
after `signal_event`/commit, and only promoted to the UI-visible `last_clip_atlas_layout`
once `native_event.is_done(signal)` — so the UI can never learn about a cell before its
pixels are confirmed landed; (4) a pressure gate (`last_fence_wait_ms > 0.1`, meaning the
content thread blocked on the GPU fence last frame) skips ALL thumbnail GPU work that
frame — cold-start renders, filmstrip decode driving, capture/restore blits, new
save-readback submits — while still allowing an already-in-flight readback to drain (a CPU
memcpy, no new GPU work). Accepted trade, by design: the UI may sample a cell mid-blit for
one frame (valid-old or valid-new pixels, never blank) — replacing the ring's stale-frame
guarantee. See `crates/manifold-app/src/shared_texture.rs` (`SharedAtlasSurface`) and
`crates/manifold-app/src/content_pipeline.rs` (`fill_clip_atlas`/`restore_clip_atlas`/
`render_content`'s clip-atlas snapshot + promotion + pressure gate).

**Hunt log (2026-07-11, preserved for history — narrowed the search but didn't find the
race; superseded by the root cause above):**

**Symptom (full):** timeline layer flickers blue when GPU saturates (heavy instanced 3D
scenes, high density); render FPS oscillates 60->30->60 in cycles; churn continues while
transport is PAUSED. Mid-flicker artifact screenshots show the clip's filmstrip mostly
blank/navy with a few ghost cells.

**Exonerated by probe (MANIFOLD_FLICKER_PROBE=1, three runs):** thumb-pass skips 0, strip misses 0, empty layouts 0, alloc fails 0, RT-only capture refusals 0, surface fence timeouts 0 — all while flicker was CONFIRMED on-screen. The UI thumbnail-draw guard, capacity/eviction failure, and the fence-timeout force-clear are not the mechanism.

**Confirmed real (probe):** a perpetual disk-save loop — ATLAS SAVE READBACK (75MB Rgba16Float 8192x1152, GPU->CPU) fired every ~5-6s in EVERY run (frames 301/661/967; 301/635/995/1355/1685/2045), even fully idle. Capture every ~1.5s -> re-arms the 5s save debounce -> readback, forever. A beat-movement gate on the throttled refresh shipped (commit on feat/scene1-wind) but the FINAL run still shows captures ~1/s AND periodic layout_rebuilds=1 (a burst of captures=4 layout_rebuilds=4 as fence_wait rose to ~20ms) — layout rebuilds on a stable timeline mean cells are being EVICTED and re-allocated, and the existing.is_none() recapture path bypasses the beat gate.

**Prime suspect (partially right):** clip_atlas_visible set flapping was suspected as the
eviction/re-allocation driver; the actual mechanism found this session is upstream of
eviction entirely — the clear-during-read race on the rotating ring, independent of
whether a cell is newly allocated or just being republished.

**Next instrumentation (superseded):** the planned eviction/visible-set logging pass was not
needed — the root cause was found by re-reading the publish path's `clear=true` argument
against the ring's completion-lag behavior, not by further counter instrumentation.

**Fix shape (superseded by "Fix (root, this session)" above)** — the originally-planned
next step (capture a live repro) was overtaken by finding the mechanism from the code path
directly.

---

### BUG-186 (sheenwoodleathersofa-webp-error-message-misattribution) — `SheenWoodLeatherSofa.glb` is correctly rejected (MANIFOLD has no webp decoder) but the surfaced error is the crate's raw `textures[].source: Missing` validation dump, not our own clean `extensionsRequired` veto message — MED-LOW, found 2026-07-16 during GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6's deferred-3 reclassification sweep

**Status:** FIXED — IMPORT_ANYTHING_WAVE_DESIGN.md W1 (2026-07-17) shipped webp texture decoding (`EXT_texture_webp` added to `MANIFOLD_SUPPORTED_EXTENSIONS`; `import_images_with_webp`/`decode_webp_image` decode the extension's bufferView bytes via the `image` crate directly, since `gltf::import_images` hard-fails the whole document on any unrecognized mime type — its own bundled `image` dependency has no webp feature). `SheenWoodLeatherSofa.glb` is no longer rejected at all: `sheenwoodleathersofa_webp_import_status` (gltf_load.rs) confirms it imports cleanly (6 materials, real bbox). Both halves of this entry are moot — the asset isn't rejected, so there's no misattributed error message to fix. Manifest promoted `xfail:BUG-186` → `expect_pass` (`non_black_fraction_min` 0.02); `docs/GLB_CONFORMANCE_STATUS.md` regenerated (134→135 expect_pass, 14→13 xfail).

**Symptom:** `render-import` on `SheenWoodLeatherSofa.glb` fails with `glTF validation failed: [(Path("textures[0].source"), Missing), (Path("textures[1].source"), Missing), ... 15 entries]` instead of the clean `"EXT_texture_webp": extensionsRequired[..] = ...: unsupported extension (MANIFOLD does not import this extension)` message `gltf_load.rs`'s own veto path is designed to produce.

**Root cause:** the asset's `textures[]` entries carry their image source exclusively via `extensions.EXT_texture_webp.source` (spec-legal — the top-level `source` field is omitted when a texture-level extension supplies it instead). `gltf_load.rs`'s filter only strips validation errors whose path starts with `extensionsRequired` (the top-level unsupported-extension list); it does not know that a `textures[N].source: Missing` error is an *expected* consequence of the same unsupported extension at the texture level, so that error survives the filter and reaches the caller first, several validation-error-list-entries ahead of anything naming `EXT_texture_webp` by extensionsRequired path. The asset genuinely can't render without webp decoding either way — the veto is correct, only the message is misleading (looks like a random malformed-texture bug, not "unsupported extension").

**Original fix shape (superseded — the asset is decoded now, not vetoed):** when an `extensionsRequired` entry is in `MANIFOLD_SUPPORTED_EXTENSIONS`'s complement (i.e. the asset is going to be vetoed anyway), suppress/reorder validation errors so the extensionsRequired veto message surfaces first — or, more directly, run the extensionsRequired veto check BEFORE invoking `json::Root::Validate` at all, so an asset requiring an unsupported extension never reaches the crate's own validator to produce a confusing secondary error.

---
