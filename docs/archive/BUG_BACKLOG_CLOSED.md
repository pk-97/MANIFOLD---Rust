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

### BUG-261 (card-slider-drags-record-no-undo-entry) — `ParamCommit`'s scene guard consumed `drag_snapshot` before the scene check, so every exposed card slider drag and param type-in recorded NOTHING; plus EnvelopeToggle and 10 unguarded drag families — Peter's 2026-07-19 "undo/redo broken across the UI" report

**Status:** FIXED 2026-07-19 (`573b50ea`, lane/undo-redo-baseline). Four root-cause clusters, all fixed at the root with a 44-test baseline matrix (`undo_baseline` in `ui_bridge/inspector.rs`) enforcing every undoable gesture family + mid-gesture snapshot stomp: **A** the scene guard reads without `take()` (only the scene branch consumes) — this was the 100%-reproducible "sliders don't undo"; **B** EnvelopeToggle routes through `AddEnvelopeCommand` / new `ToggleEnvelopeEnabledCommand` instead of `MutateProject`; **C** ten new `ActiveInspectorDrag` guard variants (AudioGain, EnvelopeTarget/Decay, AudioModShape, AudioModStepAmount, AudioTriggerShape, AudioSendGain, AudioCrossover, RelightParam, AbletonMacroTrim) so a mid-gesture full-snapshot acceptance can't revert the in-flight value; **D** the Param guard restores via `set_base_param` (was effective-only `set_param`; commit reads base). Last unfixed family of cluster C: BUG-262.

**Symptom:** undo/redo "broken, out of order, or just don't respond" across sliders, buttons, toggles, clips, trims — worst during playback (every frame is a data_version bump, so every unguarded drag lost its undo entry mid-gesture).

**Root cause:** gesture lifecycle, four shapes (above). Clips were never broken — the host batching + composite path was sound; baseline proved it and Peter's clip report was the same stomp class seen from the timeline side.

**Class guard:** the baseline matrix runs in the default suite; any NEW snapshot/changed/commit trio added without an `ActiveInspectorDrag` variant fails its stomp test as soon as someone writes its row — the matrix doc-comment says to add one.

### BUG-253 (shininess-and-tone-map-uniform-order-drift-weird-tints) — hand-packed uniform structs disagree with the codegen kernel's PARAMS-order layout — found 2026-07-18 by the P7 fused-vs-unfused relight parity test; matches Peter's live "weird tints with 3D Shading" report

**Status:** FIXED 2026-07-18 (P7 lane, `lane/depth-relight-p7-impl`). `BlinnUniforms` reordered to PARAMS order AND its color moved to the generated layout's offset (four consecutive f32 after `power`, no vec4 alignment pad); `ToneMapUniforms` reordered (curve/mode were after the nit fields). Hand-oracle WGSL kernels unchanged — the gpu_tests now pack hand-layout bytes explicitly instead of reusing the Rust struct. All 155 codegen-path atoms audited by script for the same drift: these two were the only real hits (gradient_ramp flagged but false-positive — Table params lay out specially). Proven by the P7 parity test going green at strict tolerances and both atoms' generated-vs-hand gpu_tests.

**Symptom:** `node.shininess` (used by the 3D Shading relight template's specular, tinted by source) rendered with scrambled uniforms — the kernel read view=(48,0,0) and power=1.0 — producing broad wrong-colored specular: the "sometimes things get weird tints" report. `node.tone_map` similarly read curve/mode from the wrong words, so non-default curve/mode combinations selected the wrong transfer.

**Root cause:** these atoms' `run()` dispatches the codegen-generated kernel (`standalone_for_spec`) but packs uniforms via a hand-written `#[repr(C)]` struct whose field order matched the *old hand kernel*, not the generated PARAMS-order layout. The per-primitive parity tests never caught it because they pack per-kernel bytes independently of the Rust struct — the struct itself was untested.

**Class guard:** the audit script is one-shot; a durable meta-test (assert each `*Uniforms` struct field order == PARAMS order for standalone_for_spec dispatchers) needs source-level reflection we don't have — the real class kill is packing uniforms from ParamDefs generically (param-storage territory). Logged here so the next uniform-struct author greps this entry.

### BUG-249 (scene-panel-modulation-is-decorative-synth-pids-never-resolve-at-runtime) — every modulation affordance on scene panel rows arms state that the runtime silently drops — found 2026-07-18, scene-panel audit after Peter's "modulation not working" report

**Status:** FIXED 2026-07-18 (`lane/bug-249-expose-then-arm`, pending landing) — Peter's design call: option (b), expose-then-arm. `resolve_mod_target` (`inspector.rs`) funnels ALL 19 modulation-family actions: a scene synth id translates to the real exposed binding id (`PresetInstance::binding_id_for_node_param`, bundled + user bindings), materializing the exposure via the same `ToggleNodeParamExposeCommand` on first arm (metadata from the primitive's `ParamDef` table); read-back translates the same way (`scene_row_modulation`, state_sync.rs — including the density row's panel-key/graph-key mismatch). Gate: `scene_row_driver_toggle_arms_a_real_exposed_param` proves driver lands in `inst.params` namespace, read-back reports armed, re-toggle reuses the binding; expose+arm = two undo entries (documented trade-off, mirrors the two clicks). Was: OPEN — HIGH, the "modulation not working" half of the 2026-07-18 scene-panel mess report; shipped green because BUG-234/BUG-239 made modulation unobservable in every flow-script gate.

**Symptom:** clicking a scene row's D/E/A button opens the drawer, shows the armed color, and persists a driver/envelope/audio-mod — but nothing on screen ever modulates, for any family (World/Object/Light/Camera/Modifier).

**Root cause (traced end-to-end):** scene rows are keyed by synthesized ids (`synth_world_param_id` → `scene.{node_doc_id}.{param_key}`, `scene_setup_panel.rs:816`). Only THREE actions get the id_map interception that translates a synth id into a real `SetGraphNodeParamCommand` write (`ui_bridge/inspector.rs:1185/1248/1314` — ParamChanged/ParamSnapshot/ParamCommit). `DriverToggle` (inspector.rs:1372), `EnvelopeToggle`, `AudioModToggle`, `DriverConfig`, and every `AudioModSet*` fall through to the shared card path, which stores the modulation on the generator `PresetInstance` keyed by the synth id — and captures a garbage `base_value` via `inst.get_param(synth_id)` on a param that doesn't exist. At runtime, `modulation.rs` applies drivers/envelopes/audio-mods only via `inst.params.get_mut(param_id)` (lines 109/213/533+); synth ids are never in `inst.params`, so evaluation silently no-ops. Meanwhile `state_sync.rs`'s `row_modulation_for_id` reads armed-state back by the same synth id (state_sync.rs:1654-1660), so the UI confirms the arm it just stored — a closed loop that never touches the render.

**Fix shape:** root fix, not per-action patching: scene-row modulation must target a param the runtime can resolve. Either (a) route ALL card-shaped actions through the scene id_map and store scene-row modulation against the real inner-node address (needs a driver/envelope/audio-mod runtime that can write `RowAddr`-shaped targets — the same write path `SetGraphNodeParamCommand` uses), or (b) make arming a scene row's modulation first materialize a REAL exposed instance param bound to the inner node (the existing `SceneSetupExposeParam` mechanism) and hang the driver off that. (b) reuses the entire existing modulation runtime unchanged and is the shape the design's "same widgets, same systems" directive implies. Blocked-on-nothing; needs a design call on (a) vs (b).

### BUG-250 (scene-panel-enum-value-cells-dead-after-convergence-removed-enum-click-path) — enum rows lost their click interaction in C-P1c/d — found 2026-07-18, same audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`311bfb2a` + `d28bfff4`) — enum click dispatch restored; follow-up root fixes: value_text INTERACTIVE flag, scene writes target panel's own layer, enum/int/bool reads. 15/16 scene flows green; `scene-setup-empty-states.json` stale (PLASMA/NOISE FIELD layers not in gltfscene fixture) — retarget or retire.

**Symptom:** clicking the value cell of any enum row (Light's Cast Shadows On/Off, Shadow Softness Hard/Soft/VerySoft/Contact, Light Mode, Modifier Axis X/Y/Z) dispatches no action and changes nothing — verified headless: a name-targeted click on `scene_setup.light.cast_shadows_value` emits zero PanelActions. The only way to change an enum is to drag the row's slider track.

**Root cause:** the P4/D9 mechanism for enum interaction was `PanelAction::SceneSetupEnumClicked` (click value → dropdown/cycle). C-P1c/d moved enum rows onto the card core's `value_labels` path and deleted every `SceneSetupEnumClicked` producer — the landing report flags the variant as "dead weight" cleanup, not realizing the producers WERE the interaction. `value_labels` in the shared card core is display-only (`format_param_value`, `param_slider_shared.rs:838`); `match_param_row_click` has no variant for a value-cell click, and the card's type-in path explicitly excludes `value_labels` params (`param_card.rs:1085`). So the regression is inherent to the conversion, not a wiring miss.

**Fix shape:** give the shared card core a real enum-cell interaction (click cycles a 2-state, 3+ opens the dropdown — the behavior SCENE_OBJECT_AND_PANEL_V2 D9 committed to), emitting through the existing ParamSnapshot/Changed/Commit trio so the scene id_map interception and undo granularity come free — and so the inspector card's own `value_labels` params gain the same affordance. Do NOT resurrect `SceneSetupEnumClicked` (bespoke, panel-only — the convergence was right to kill it, wrong to replace it with nothing).

### BUG-251 (scene-and-audio-dock-scroll-inverted-vs-every-other-surface) — both docks negate the shared wheel delta — found 2026-07-18, same audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`ea812c24`).

**Symptom:** mouse-wheel/trackpad scroll in the Scene Setup and Audio Setup docks moves content in the opposite direction from the inspector, browser, and every other scrolling surface.

**Root cause:** `window_input.rs` hands the same normalized `dy` to every consumer. Inspector: `apply_scroll_delta(delta)` (inspector.rs:953). Scene dock: `handle_scroll` negates — `apply_scroll_delta(-delta)` (`scene_setup_panel.rs:3863`); `audio_setup_panel.rs:567` has the identical negation (BUG-199's fix apparently copied the wrong sign convention and the scene panel copied the audio panel).

**Fix shape:** drop both negations; add a shared-direction assertion or a flow that scrolls two surfaces and compares offsets so a third panel can't re-introduce it.

### BUG-252 (eight-scene-flow-scripts-dead-at-step-2-on-stale-outliner-assert) — most scene-panel flow coverage silently never ran at the convergence landing — found 2026-07-18, same audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`f101a585` + flow retargets in `d28bfff4`). `scene-setup-empty-states.json` follow-up RESOLVED 2026-07-18 (BUG-249 lane): the flow was never rot — it targets the default `timeline` fixture (PLASMA/NOISE FIELD live there, not in `gltfscene`); verified 7/7 green via `cargo xtask ui-snap timeline --script scripts/ui-flows/scene-setup-empty-states.json`, both empty-state asserts matching for real. Run it against `timeline`, not `gltfscene`.

**Symptom:** 8 of the 21 scene flow scripts (`add-fog-drag`, `eye-toggle`, `fog-undo-removes-fog`, `heldout-merge-snapshot`, `light-cast-shadows-toggle`, `light-intensity-drag`, `numeric-typein-box`, `shadow-softness-dropdown`) fail at step 2 on `Assert Query(text="Outliner")` — the header was renamed "Objects" — so every later step (the actual button under test) never executes. The landing fixed the three scripts in its own gate list and left the rest dead. `scene-setup-empty-states.json` is additionally broken at step 0 (expects a "PLASMA" layer the `gltfscene` fixture doesn't have). With the assert patched, `eye-toggle` (9/9), `fog-undo` (17/17), `numeric-typein-box` (8/8), `heldout-merge` (3/3) pass; `cast-shadows-toggle` and `shadow-softness-dropdown` fail for the real reason (BUG-250); `add-fog-drag`/`light-intensity-drag` fail only their final displayed-value assert (BUG-239 class, dispatch verified). BUG-240 (scrub-fine) is one instance of this same rot, already logged.

**Fix shape:** sweep all scene flows to the "Objects" header + a fixture that satisfies empty-states; then a meta-gate: a landing that claims flow re-verification must run EVERY `scene-*` flow, or a nightly/pre-landing runner that executes the whole `scripts/ui-flows/` directory and fails on any script that can no longer reach its last step.

### BUG-214 (ext-mesh-gpu-instancing-missing-from-supported-extensions-allowlist) — `EXT_mesh_gpu_instancing` is fully implemented but absent from `MANIFOLD_SUPPORTED_EXTENSIONS` — found 2026-07-17, IMPORT_ANYTHING_WAVE Lane W6 extension roadmap audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`a1963ffa`).

**Symptom (would-be):** an asset that lists `EXT_mesh_gpu_instancing` under `extensionsRequired` (spec-legal — an exporter may mark it required when the asset's geometry depends on the instance transforms to exist at all) is rejected at import with "unsupported extension (MANIFOLD does not import this extension)" — even though MANIFOLD fully parses and renders this extension (`gltf_load.rs:278-394`, instance transform composition via `mat4_mul`, non-`F32` accessor and sparse-accessor guards, buffer-bounds checks).

**Root cause:** `MANIFOLD_SUPPORTED_EXTENSIONS` (`gltf_load.rs:34-53`) is the allowlist the `extensionsRequired` veto (`gltf_load.rs:110-116`) checks membership against. It was populated from the crate's typed-feature list plus the raw-JSON-sniffed material extensions (`GLTF_MATERIAL_EXTENSIONS_DESIGN.md` E1-E6); `EXT_mesh_gpu_instancing` support landed separately (`GLB_XFAIL_BURNDOWN_DESIGN.md` D6, BUG-168) and was never added to this array, since none of that work's own fixtures marked it `extensionsRequired`.

**Fix shape:** add `"EXT_mesh_gpu_instancing"` to `MANIFOLD_SUPPORTED_EXTENSIONS`. One-line, no behavior change for any asset that doesn't mark it required (the extension is already read via `node.extension_value(...)` regardless of the required-list check). Verify with a fixture that lists it under `extensionsRequired` rather than only `extensionsUsed` (the local `SimpleInstancing.glb` and hostile-shelf instancing fixtures use the looser `extensionsUsed` form and don't currently exercise this path).

### BUG-213 (no-report-line-for-unimplemented-optional-material-extensions) — MANIFOLD never reads `document.extensions_used()`, so any unimplemented *optional* extension silently degrades with no report line — found 2026-07-17, IMPORT_ANYTHING_WAVE Lane W6 extension roadmap audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`a1963ffa`).

**Symptom:** an asset carrying `KHR_materials_diffuse_transmission` (4 Khronos assets: `DiffuseTransmissionPlant/Teacup/Test.glb`, `ScatteringSkull.glb`, currently `xfail:diffuse-transmission-deferred`) imports and renders as a plain opaque material — correct in that nothing crashes or looks broken, but the user gets no indication the translucency effect they authored is missing. The same silent-degrade will recur for any future ratified extension MANIFOLD doesn't implement yet, since nothing currently detects it.

**Root cause:** `document.extensions_used()` — the glTF field listing every *optional* extension an asset carries (as opposed to `extensionsRequired`, which lists only the ones the asset cannot render correctly without) — has zero call sites anywhere in `crates/manifold-renderer/src/node_graph/`. The only unsupported-extension detection MANIFOLD has is the `extensionsRequired` veto (`gltf_load.rs:110-116`), which by spec definition never fires for optional extensions. `ImportReport::report_lines` (`gltf_import.rs:93`, fed today only by the animation-drop paths in `gltf_load.rs`) is the existing channel this should feed into.

**Fix shape:** after import, diff `document.extensions_used()` against a `MANIFOLD_RECOGNIZED_EXTENSIONS` set (the union of `MANIFOLD_SUPPORTED_EXTENSIONS` plus every raw-JSON-sniffed extension read without a required-list entry, e.g. `EXT_mesh_gpu_instancing` — see BUG-214) and push one `report_lines` entry per unrecognized name. This is a one-time generic fix that retroactively covers `KHR_materials_diffuse_transmission` and every future gap, rather than a one-off patch per extension — see `docs/GLTF_EXTENSION_ROADMAP.md` for the full writeup (this is that doc's top-ranked finding). The full `KHR_materials_diffuse_transmission` BTDF lobe itself (as opposed to just naming its absence) is separate, lower-priority follow-on work — not required to close this entry.

### BUG-242 (live-trigger-edge-rearm-hostage-to-shape-release) — dense-material trigger recall collapses because edge re-arm depends on the visual envelope release — found 2026-07-18, causal-detection diagnosis session

**Status:** FIXED 2026-07-18 (same evening; Peter approved). `TransientEdge` now advances on the sensitivity-scaled RAW impulse (no attack/release smoothing) in both consumers — `live_trigger.rs` clip triggers and `modulation.rs` trigger-gates; the conditioned envelope is unchanged for meters/modulation. Measured at DEFAULT shape: edm_kit generic-hit recall 0.204 → 0.714 @ P 1.000; kick_hat byte-identical (0.785/1.000/0.646); 617 unit tests green. Known cost, accepted deliberately: sustained_pad trigger fires 46 → 71 — the envelope was MASKING the analyzer's pad false fires; the trigger layer is now faithful, so BUG-243 (the analyzer-level pad fix) is the sole remaining owner of that symptom.

**Symptom:** on dense material (`self_render/edm_kit_128bpm`, 196 truth hits) the causal trigger path fires only ~21% of hits at the default trigger shape, despite the SuperFlux analyzer firing on 194/196 (99%) at the detection level (proven via `MANIFOLD_ODF_DEBUG` attribution, landed `726d81b4`).

**Root cause:** `TransientEdge::advance(conditioned, 0.5)` re-arms only when the shape-conditioned envelope falls below 0.5, and the default `AudioModShape` release (120 ms) exceeds dense hit spacing (~80 ms), so the edge never re-arms between hits. Measured: `--release-ms 20` on the dumper takes generic-hit recall 0.204 → 0.673 at precision 1.000, timing 4.6 ms; sparse fixture (`kick_hat`) unchanged.

**Fix shape:** decouple trigger re-arm from the modulation shape's release — give `TransientEdge` its own re-arm criterion (fixed short re-arm window ≈ the analyzer's 32 ms refractory, or hysteresis on the RAW impulse rather than the conditioned envelope) so the visual envelope can stay long while the trigger stays fast. Parameter-level stopgap: short release on trigger routes (works today, but couples visual feel to detection).

### BUG-238 (scene-setup-camera-world-light-eye-toggle-reads-as-dead) — the Scene Setup outliner's dimmed eye glyph on Camera/World/Light rows reads as a broken button, not a disabled one — found 2026-07-17, Peter live-testing the dock ("The params and visibility buttons for all of these cameras, world, lights, etc don't work either. They do nothing currently.")

**Status:** FIXED 2026-07-18 — SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1c (`wave/scene-card-convergence`). `EyeSlot::Dimmed` deleted from `scene_setup_panel.rs`; the Camera/World/Light outliner rows now use `EyeSlot::Empty` (draws nothing, keeps the reserved slot width) — Custom Light rows use the same empty slot, Custom Object rows keep the dimmed glyph (Object DOES carry a real `visible` param, this instance just isn't addressable).

**Symptom:** Camera/World/Light rows in the outliner show a dimmed eye glyph in the trailing affordance slot. Clicking it does nothing.

**Root cause:** this is BY DESIGN, not a bug in the click/dispatch path — `SCENE_PANEL_UX_DESIGN.md` D5 deliberately renders a non-interactive dimmed eye glyph on any row whose scene item has no `visible`/enable param (only Object rows carry `scene_object.visible`; Camera/World/Light have no equivalent per their `ParamDef`s traced in `scene_vm.rs` — `SceneLightVm`/`CameraVm`/`EnvironmentVm`/`AtmosphereVm` carry no visibility-style address at all). The row template reserves the SAME slot width for every row and fills it with either a live eye (Object) or a dimmed placeholder (everything else) for visual uniformity (`feedback_no_conditionally_visible_ui`). That uniformity choice reads, to a user clicking it, as a broken button rather than an absent one.

**Fix shape (orchestrator amendment to SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1, overriding D5's dimmed-placeholder choice):** rows with no real visible/enable param get NO eye at all — an empty trailing slot, not a dimmed glyph. Rows with a real param keep the live eye, verified to actually flip the render.

### BUG-237 (scene-setup-camera-world-light-param-scrub-does-nothing-live) — Peter reports Camera/World/Light parameter rows in the Scene Setup panel don't do anything when scrubbed, in the running app — found 2026-07-17, Peter live-testing the dock

**Status:** FIXED 2026-07-18 — SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1c (`wave/scene-card-convergence`), with a render-level mechanism proof (not just dispatch coverage). Root cause was (a) from the "what remains unproven" list below: the bespoke per-family Light/Camera click/drag routing (`build_light_numeric_row`/`build_light_triplet_row`/`build_light_enum_row`/`build_camera_numeric_row`/`build_camera_triplet_row`, and their `light_value_cells`/`camera_value_cells`/`light_steppers`/`camera_steppers`/`light_enum_cells` hit-testing) — deleted and replaced by the shared card row core (`build_light_card_row`/`build_camera_card_row` → `build_param_row`/`match_param_row_click`, the same infrastructure Object rows already proved correct in C-P1b). Hypotheses (b) (stale node ids on a diverged layer) and (c) (content-thread re-render) were checked and ruled out by the render-level proof below — the fix was purely the dispatch-routing layer, as C-P1c's own brief predicted.
**Closing proof (two halves, both required):** (1) dispatch-level — `inspector.rs::scene_card_convergence_tests::light_intensity_commit_writes_the_layer_instance_def_at_root_scope` / `camera_orbit_commit_writes_the_layer_instance_def_at_root_scope` drive a REAL card-row `ParamSnapshot`→`ParamChanged`→`ParamCommit` sequence through `dispatch_inspector` and read the committed value back out of `layer.generator_graph()`'s `EffectGraphDef`, proving the write lands (mirrors C-P1b's Object-family proof; Light/Camera are root-scoped by construction, so there is no group-scope case like Object's D12 bug). (2) render-level — `manifold-renderer/tests/gpu_proofs/bug237_light_camera_commit_render_proof.rs` (`sun_intensity_commit_visibly_changes_the_render` / `camera_orbit_commit_visibly_changes_the_framing`, gpu-proofs feature) renders SceneStarter before/after a def mutation shaped exactly like `SetGraphNodeParamCommand::execute`'s own write, through the real `PresetRuntime`, and asserts + the session LOOKED AT the PNG pair: sun intensity 1.0→8.0 visibly brightens the cube (mean_abs_diff 0.09), camera orbit +π/2 visibly reframes it a quarter-turn (mean_abs_diff 0.06). Together: card row → command → def (proven in `manifold-app`) → def → pixels (proven in `manifold-renderer`) — the full BUG-237 chain, closed with mechanism, not just "the dispatch log looks right."

**Original diagnosis (superseded by the closing proof above, kept for history):** Diagnosed session 1; a session 2 resume (2026-07-17) narrowed the C-P1 integration architecture further but still did not implement the fix — see the design doc's status note (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md) for the full write-up. Short version: `resolve_graph_target(GraphParamTarget::Generator, …)` already resolves the exact `GraphTarget` `SetGraphNodeParamCommand` needs, and that command already self-captures `previous_value` (a real commit-with-prior-value shape) — so neither of D2/D4's `⚠ VERIFY-AT-IMPL` escalation triggers fired; what's missing is ordinary (if nontrivial) wiring: three new branches in `dispatch_inspector`'s `ParamSnapshot`/`ParamChanged`/`ParamCommit` arms that resolve through the scene panel's id map instead of `with_preset_graph_mut` when a synthesized scene param id is seen, plus `ScenePanel` growing `build_param_row`-shaped per-frame state and a real Begin/Changed/Commit drag sequence in place of its current per-move-event dispatch.

**Symptom:** scrubbing a Light/Camera/World (Environment/Fog) numeric row in the Scene Setup panel produces no visible change in the running app, per Peter's live report. No prior test exercised this path past "an action gets built" — `scene_setup_panel.rs`'s own tests only assert a drag/click produces the right `PanelAction`, never that dispatching it changes anything.

**Diagnosis this session (value-level, not dispatch-log-level, per the escalation brief):** added `crates/manifold-app/src/ui_bridge/project.rs`'s `scene_setup_param_changed_writes_light_intensity_to_def` / `..._writes_camera_orbit_to_def` / `..._writes_fog_density_to_def` — each drives the REAL `dispatch_project(PanelAction::SceneSetupParamChanged(...))` entry point against a freshly-materialized `SceneStarter` layer and reads the resulting `EffectGraphDef`'s param value back out (not just that dispatch returned `handled`). **All three pass** — for a fresh, undiverged layer, `RowAddr` (root scope + `node_doc_id` traced off the same `SceneVm::from_def` `state_sync.rs` walks) is correct and `SetGraphNodeParamCommand` genuinely writes the new value into the def, for all three families. This rules out the addressing/dispatch layer (state_sync's `RowAddr` tracing + `project.rs`'s `SceneSetupParamChanged` arm) as the root cause — it is proven correct where BUG-218's suspicion class (stale/wrong scope) would have shown up.

**What remains unproven / next steps:** the def-level write is confirmed sound; the live "nothing happens" symptom therefore lives ABOVE this layer — most likely candidates, none confirmed this session: (a) the bespoke per-family click/drag routing in `scene_setup_panel.rs` (`build_light_numeric_row`/`build_light_triplet_row`/`build_camera_numeric_row`/the World rows' equivalents, and their `light_value_cells`/`camera_value_cells`/stepper hit-testing) has a live-only geometry/hit-test bug no unit test catches (unit tests assert the action shape, not that a simulated pointer at real screen coordinates reaches the right widget); (b) something specific to a layer whose `generator_graph()` has ALREADY diverged from the bundled default (a saved/edited project, unlike this session's fresh-layer test) traces stale node ids; (c) the content thread doesn't re-render after a non-structural `ContentCommand::Execute` for these param families specifically (untested this session — the diagnosis tests only check the UI-thread's local `Project` copy, not a real running content thread). Not narrowed further — no interactive GUI access this session to reproduce the live click.

**Fix shape:** SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md's C-P1 (card-row convergence) deletes the entire bespoke per-family click/drag routing layer named in (a) above and replaces it with the card's own proven `build_param_row`/`match_param_row_click` path — the same infrastructure Object rows use, which Peter has NOT reported as broken. If (a) is the root cause, C-P1 fixes it as a structural side effect of the row swap. If (b) or (c) turn out to be the cause, C-P1 does not fix them and they need separate root-causing. C-P1 was NOT implemented this session (see design doc status) — this diagnosis is the input to whoever executes it next, and that session's acceptance gate must include a render-level (pixels-changed) or def-level (post-swap) proof for these three families specifically, not just dispatch-log coverage.

**Escaped:** SCENE_PANEL_UX P3a/P3b-i (landed `76251784`) — those flows proved dispatch and exposure for these rows but never the render effect, exactly the gap this entry closes the visibility on.

### BUG-182 (hdri-exr-files-fail-or-fail-silently) — Peter's real .exr HDRI files don't work through `node.hdri_source`, despite the atom's claimed .exr support — MED (glb-import lighting / HDRI env_mode)
**Status:** FIXED 2026-07-18 — root cause (`8294ac0a`, lane/hdri-card-binding-clobber — investigated AND implemented by a Kimi K3 agent via cc-fleet, verified + landed by the lead session). The live-app "doesn't do anything" mechanism was neither of the prior suspects: `apply_string_params` fell back to each string binding's declared default for keys absent from `clip.string_params` and re-ran every frame, so the card's empty `hdri_file` binding clobbered `node.hdri_source`'s node-level `path` (set via the graph editor's picker) every frame; `apply_string_defaults` did the same at construction. Fixed at the class level in `preset_runtime.rs`: present-keys-only writes + construction seeding with precedence host value → def node param → binding default; 3 CPU regression tests. **Still open:** (Fix B) the graph-editor 3D viewport renders with `ParamManifest::default()` (`viewport_session.rs:169/:304`) so env_mode can NEVER reach that surface, and its dirty-gated rendering starves async HDRI decodes — HDRI must be judged in the compositor output, not the viewport, until fixed. (Fix C, prior (b)/(c)) card-level error surface + card file picker. Plus a flagged UI follow-up: clearing a card text field (key removal) no longer reverts to the binding default — the clear gesture should commit `Some("")` if explicit clearing is wanted. Peter's live visual confirmation in the compositor still owed.

**Symptom:** Loading one of Peter's own .exr environment maps into `node.hdri_source` (the `hdri_file` string binding, env_mode = HDRI) doesn't produce the expected lit result. No specific failing file is on record yet.

**Investigation this session:** the two committed fixtures (`tests/fixtures/hdri/hdri_float32.exr`, `hdri_half16.exr`) decode cleanly through `load_hdri` with HDR range intact (max R > 3.0, max G > 6.0 — new CPU test `committed_hdri_fixtures_decode_with_hdr_range_intact`) — they don't carry whatever trait breaks Peter's files. Reproduced two of the original suspects directly against real Blender 4.5.2 renders: **DWAA compression decodes fine** (ruled out — the `image` crate 0.25.10's OpenEXR decoder has added DWAA support since the doc-comment era it was flagged in). **Multi-layer/multi-part EXR reliably fails** — a Blender "OpenEXR MultiLayer" render (Combined + Z + Normal passes) reproduces the exact `image::ImageError` the crate's OpenEXR decoder gives when no header has a flat top-level R/G/B triple (`"image does not contain non-deep rgb channels"` — color is nested under a named render-pass layer like `"ViewLayer.Combined.R"` instead). This is the most plausible real-world cause: HDRI creation/compositing workflows commonly default to multilayer export. Committed as `tests/fixtures/hdri/multilayer_unsupported.exr` with a new regression test (`multilayer_exr_reports_an_actionable_cause_not_a_bare_decode_error`).

**Fixed this session:** `load_hdri`'s error path (`hdri_source.rs`) now detects this specific crate error and rewrites it into an actionable message naming the cause in plain terms and suggesting the fix ("re-export as plain OpenEXR, not MultiLayer") instead of forwarding only the crate's internal wording. This closes suspect (a) for the multi-layer case specifically — the next real failing file that hits this path will name its own cause in the log, per the no-silent-fallbacks doctrine. Suspect (a) is NOT fully closed: only the multi-layer/multi-part shape was reproduced and named; other exotic shapes (deep images, luminance-chroma encodings) would currently fall through to the generic `image::open(...): {e}` message, which is at least always path-qualified and forwards the crate's own text, but hasn't been individually verified as user-actionable.

**Still open — not attempted this session (out of Lane W5's scope, which targeted decode-path instrumentation only):** (b) the log line still isn't surfaced anywhere in the running app's UI — a decode error is invisible without a terminal/log file open, so an unsupported file is still indistinguishable from "HDRI does nothing" to a user at the rig. This needs a card-level error state (a small UI feature, not a log-line change) — no such per-node error-surface mechanism exists yet anywhere in the graph editor to hook into (checked: `node.gltf_texture_source` has the identical `log::error!`-only gap). (c) the outer CARD's `hdri_file` field is still a bare text field with no file picker (`GenStringParamClicked` → text input only) — separate from the GRAPH NODE's own `path` row, which now has a working native file picker (2026-07-18, see below).

**Investigation 2026-07-18 (Peter reported live: "Loading a HDRi file into the node and using the HDRi environment mode on the card doesn't do anything"):** two things landed/confirmed this session.

1. **Fixed:** the graph node's own `path` row (`node.hdri_source`, clicked directly in the graph editor — distinct from the card's `hdri_file` field in (c) above) opened a native FOLDER picker (`rfd::FileDialog::pick_folder()`), which cannot select a file at all — the literal cause of "I can't select an .exr file from the file browser." Root cause: `BrowseGraphNodePath`'s handler (`app_render.rs`) always called `pick_folder()` regardless of the param's shape; only `node.image_folder`'s `folder` param actually wants a directory. Now routes on param name (`folder`/`dir` → `pick_folder()`, `path`/`file` → `pick_file()`). Also fixes the equivalent row on `node.gltf_texture_source`/`gltf_mesh_source`/`gltf_morph_deltas_source`/`gltf_skinned_mesh_source`, which had the identical bug. Landed `2ce8bcf2`.

2. **Ruled out as the cause of "doesn't do anything":** ran Peter's actual `kloppenheim_07_puresky_4k.exr` (74MB) through the real production path end to end — `load_hdri` decodes it cleanly (no multi-layer/DWAA failure, no error logged), `render-import` converges (took until frame 158 — the async decode is not instant on a 74MB file), and the rendered result visibly changes: on `tests/fixtures/gltf/hostile/mixamo_like.glb`, mean object-pixel brightness drops 147→104 (max per-channel diff 92/255) between `env_mode=0` (Softbox) and `env_mode=1` + this HDRI, matching the documented "real HDRI reads ~4× dimmer than the softbox default" behavior. Also traced the live-switch path specifically (not just a fresh graph built with `env_mode=1` from frame 0): `execution.rs`'s `compute_live_steps` (which prunes `node.hdri_source`'s branch off `node.switch_texture` while `env_mode=0`) re-runs every frame from live params, and `render_scene`'s IBL cache key hashes the envmap's write-generation + texture identity, so a live `env_mode` toggle on an already-running executor is not structurally different from a fresh build — this was a designed live-perform-mode-switch path, not new code. **So the backend/graph mechanism is confirmed working for this exact file.** Not yet verified: whether the live GUI app itself (as opposed to the headless `render-import` harness) actually shows the change promptly — remaining suspects are (i) the ~74MB decode's real wall-clock latency being mistaken for "nothing happened," or (ii) a live-app-specific UI/content-thread propagation gap in the same class as BUG-136. Needs a hands-on-app repro to settle; out of this session's reach (no interactive display access).

### BUG-241 (stage1-dsp-onset-frontend-misses-loud-real-kicks-track-dependent) — the bake-off Stage-1 DSP drum detector's onset front-end fails to fire on loud, clearly-present kicks on some tracks while nailing them on others — found 2026-07-18, Fable audio-accuracy follow-up (ADTOF bake-off B3 post-mortem)

**Status:** FIXED 2026-07-18 (Fable, same-day follow-up on the lane) — root cause was the B1-era backtrack refinement itself: `librosa.onset.onset_backtrack` against the broadband RMS envelope walks each flux peak to the preceding loudness minimum, which on dense material is the PREVIOUS hit's tail, not this hit's attack — peaks landed 60–140ms early, track-dependently (sparse mixes have a clean pre-attack RMS dip, dense ones don't; hence apricots fine / feel_the_vibration broken). Neither of the triage's killed suspects; found by stage-wise accounting (curve-peak → threshold → backtrack → merge) on the apricots-vs-feel minimal pair: backtrack was the stage that took feel from 8/16 to 2/16. Fix: backtracking removed entirely from `detect_onsets` (band-own-energy backtrack was also measured — strictly worse than none); the flux peak's frame-center time already carries B1's timing correction. Post-fix any-onset kick recall: feel 8/16, inhale_exhale 11/14, tears 6/10, bad_guy 10/17 (up from 2/16, 2/14, 1/10, 8/17), apricots unchanged 15/16; kick-LABELED recall tracks closely (8/16, 9/14, 6/10, 9/17, 11/16). All 121 eval tests green incl. the exact-truth timing tests (no reintroduced bias). **Owed:** ~~bake-off scoreboard re-run~~ DONE same day: 75-track re-run (with the 0.075 threshold tune) read electronic kick 0.311 → **0.493** vs ADTOF's unchanged 0.702 — the bug had overstated the gap by ~0.18; verdict unchanged (0.493 < the 0.5 bar, all classes still trail). Full re-read incl. the hat-labeling regression it surfaced: AUDIO_ANALYSIS_ACCURACY_DESIGN.md §"B3 kick-line re-read".

**Symptom:** on the 5 `tests/fixtures/audio/<track>_<bpm>bpm/drums.wav` electronic stems, kick recall against the fixtures' own `drums_time_s` labels (±50ms), measured directly through `manifold_audio.stage1_dsp_detection.detect_drums_stage1` (Event attr is `.type`, NOT `.label` — a wrong-attr spot-check produced spurious 0s during triage, don't repeat): apricots **15/16** (front-end works), feel_the_vibration **2/16**, inhale_exhale **2/14**, tears **1/10**, bad_guy 8/17 (bad_guy separately misaligned — 15.0s stem vs 13.24s mix, its own README caveat, exclude it). Recall measured against ANY detected onset regardless of class label, so this is the ONSET stage failing, upstream of the cluster-labeling weakness the B3 verdict already documented. Kicks confirmed real: all 16 feel_the_vibration labeled times sit at ~2x track-median RMS. The 58 onsets it does find land 60-140ms from the kicks — densely detects other content, steps around the kicks.

**Root cause:** UNKNOWN. Two plausible causes TESTED AND KILLED this session (record so nobody re-runs them): (1) whole-track normalization crushed by a single giant peak — feel_the_vibration has a max-amplitude 1.0 transient at 12.83s, but clipping it changed recall 2->2, and analyzing only the clean first 12s still gave 3/15; (2) sub-kick-without-click (flux blind to low-freq-only onsets) — killed by spectrum check: caught vs missed kicks have near-identical profiles (all 64-77% energy <150Hz, centroids 1300-2677Hz; the MISSED feel is BRIGHTER than the CAUGHT apricots). Neither normalization-vs-peak nor kick timbre. Suspects for the real hunt: per-band flux ODF weighting/threshold vs these tracks' overall spectral balance; the multi-band picker's adaptive threshold; frame-center vs frame-start conversion (a ~25ms bias was fixed in B1 — verify it's applied on this path).

**Fix shape:** dedicated debug session on the Stage-1 onset front-end (`manifold_audio/stage1_dsp_detection.py` + the `spectral.py` per-band flux it calls) — instrument the per-band ODF and adaptive threshold on a minimal pair (apricots works vs feel_the_vibration fails), dump why the kick frames don't cross threshold. Whole-track normalization is a suspected wrong design choice for transient onset detection (Peter's instinct, shared) — a causal/local adaptive baseline is the likely correction, but PROVE the mechanism before changing it. Re-run the bake-off after: Stage-1's real kick numbers are unknown until this is fixed, and the B3 verdict's kick line should be re-read then (not the whole verdict — labeling was independently weak). Full triage narrative: the 2026-07-18 Fable follow-up chat.

### BUG-224 (scene-setup-close-button-bypasses-shared-toggle-action) — the Scene Setup dock's × button doesn't visibly close the panel — found 2026-07-17, Peter live-testing the dock ("The close button on the scene panel also doesn't work")

**Status:** FIXED 2026-07-17 (`lane/panel-interaction-bugs`) — `ScenePanel::handle_event`'s `Click` arm for `close_id` now returns `(true, vec![PanelAction::OpenSceneSetup])` instead of calling `self.close()` directly, mirroring `AudioSetupPanel::handle_event`'s already-correct close arm (which has its own passing unit test, `close_toggles_audio_dock`, documenting the "one toggle path" pattern). Regression test added: `scene_setup_panel::tests::close_button_click_routes_through_the_shared_toggle_action`. L3 proof: new `scripts/ui-flows/scene-setup-close-button.json` (opens the dock, asserts "Outliner"/"×" exist, clicks "×", asserts both "Outliner" and "Scene Setup" are gone — green against the `gltfscene` fixture; the dispatch log shows `dispatched OpenSceneSetup (structural=true)` firing on the × click, confirming the correct action now flows).

**Symptom:** clicking the × in the Scene Setup dock's title bar had no visible effect — the dock stayed open with all its content.

**Root cause:** `ScenePanel::handle_event`'s `Click` arm for `close_id` called `self.close()` directly and returned `(true, Vec::new())` — no `PanelAction`. That only flips the panel-local `open` bool. It never told the app to (a) reset `ui_root.layout.scene_setup_width` back to 0 — the dock's actual screen-space allocation, owned separately by `UIRoot`/`toggle_scene_dock()`, not the panel — so the region kept its footprint; (b) run any rebuild — `app_render.rs`'s dispatch loop (`self.needs_rebuild = true` etc.) only fires off a returned `PanelAction`, and an empty action Vec means nothing downstream ever reacts; (c) sync the header's Scene toggle-button highlight (`header.set_dock_toggle_state`), which only `toggle_scene_dock()` calls. The sibling `AudioSetupPanel` never had this bug — its `handle_event`'s close arm already emits `PanelAction::OpenAudioSetup`, the same action the header button and Escape key use, routing through the single owning toggle path (`ui.toggle_audio_dock()` + `DispatchResult::structural()` in `ui_bridge/mod.rs`). `ScenePanel` was supposed to mirror `AudioSetupPanel` throughout (SCENE_SETUP_PANEL_DESIGN D2 says so explicitly at several other call sites) but this one spot diverged — parity was never actually implemented for the close button, just assumed.

**Escaped:** SCENE_OBJECT_AND_PANEL_V2's P4/P5 (outliner + properties panel, landed 2026-07-17, `e78d97d2`) rebuilt this panel's body extensively but never added a close-button test or flow — the existing `scene-setup-add-fog-drag.json`/other flows all open the dock and never close it via the × (Escape/header-toggle paths, which route correctly through `toggle_scene_dock()`, were exercised instead). No flow or unit test ever drove `handle_event(Click { close_id })` before this session.

### BUG-223 (scene-setup-dock-scroll-state-updates-but-never-repaints) — mouse-wheel scroll over the Scene Setup (and Audio Setup) dock updates internal scroll state but the screen never visibly moves — found 2026-07-17, Peter live-testing the dock ("Scrolling is still not working on the scene panel"), reopens BUG-199 (closed 2026-07-17 same day, `2f7c6331`) for the real-mouse path BUG-199's own headless proof never reached

**Status:** FIXED 2026-07-17 (`lane/panel-interaction-bugs`) — `window_input.rs`'s `primary_mouse_wheel` dock-scroll branch (added by BUG-199) now sets `self.needs_rebuild = true` after `process_scroll`, matching the inspector branch's own precedent three lines below it. Stale "the dock rebuilds every frame" comments in `scene_setup_panel.rs`/`audio_setup_panel.rs` corrected in the same diff.

**Symptom:** scrolling the mouse wheel over an overflowing Scene Setup (or Audio Setup) dock body does nothing visible — content stays put.

**Root cause:** BUG-199's fix (landed earlier the same day, `2f7c6331`) routed dock wheel-scroll through `ui.input.process_scroll()` → `UIEvent::Scroll` → the panel's `handle_scroll` → `ScrollContainer::apply_scroll_delta` — which correctly updates the panel's internal `scroll_offset`. But nothing in that branch sets any of the flags `app_render.rs`'s `apply_ui_frame_invalidations` (`ui_frame.rs`) reads to decide whether to actually re-run `ui_root.build()` (`needs_rebuild`/`needs_structural_sync`) or `rebuild_scroll_panels` (`scroll_dirty.any()`) — and `rebuild_scroll_panels` wouldn't touch the dock's tree region even if triggered (it's scoped to the timeline viewport/layer-headers, not the Base-tier dock region `ScenePanel::build_docked` writes into). Without a triggered rebuild, `build_docked` — the only place that bakes `scroll_offset` into the tree's actual node Y-positions (`self.scroll.offset_content(tree, -offset)`) — never runs again, so the state changes but the screen doesn't. BUG-199's own code comment stated the (false) assumption directly: "the docks rebuild every frame, which is enough to re-apply the new scroll offset" — untrue under the app's dirty-flag-gated rebuild.

**Escaped:** BUG-199's landing verification (`scripts/ui-flows/audio-dock-scroll.json`/`scene-setup-add-fog-drag.json`, both green then and still green now) never caught this because the headless `--script` harness has its own masking bug: `ui_snapshot/script.rs`'s `Runner::advance_frame` builds its `UiFrameSignals` from `self.needs_structural_sync || ui.inspector.skip_to_settled(&mut ui.tree)` — and `skip_to_settled` returns `true` on the very first `advance_frame` call of ANY script (it force-settles the inspector's intro pop/spawn tweens), forcing one free full rebuild regardless of what the dispatched gesture actually flagged. Both BUG-199 flows dispatch their `Scroll` gesture as their first (or, for the fog-drag flow, an early) mutating action, so they always got this free rebuild and could never distinguish "the fix sets needs_rebuild" from "the fix sets nothing." Confirmed empirically this session: a two-scroll probe script showed the first scroll's rebuild (riding the free settle) but the second scroll (after `needs_structural_sync` — itself a separate harness-only bug: never reset to `false` after firing once, so it stays permanently "stuck true" and masks every later step too) produced no further content movement query mismatch, and reading `ui_frame.rs` directly confirmed the gating logic. This harness gap (the free first-frame rebuild, and separately the stuck-true `needs_structural_sync`) is real and not fixed by this session — logged here as a known blind spot for future scroll/rebuild-signal bugs in ANY dock/panel tested via `--script`, not just this one.

### BUG-216 (feedback-loop-into-final-output-freezes-at-depth-one) — a `node.feedback` loop whose blend output feeds `system.final_output` directly silently degrades to a one-frame loop with per-frame stderr spam — found 2026-07-17 during the depth-relight look probe (headless `PresetRuntime` path)

**Status:** FIXED @ f2684402 (DEPTH_RELIGHT_DESIGN.md P4, D6(b)).

**Symptom:** author a standard feedback graph (`mix → feedback → transform → … → mix`, mix out → `final_output`). Trails never accumulate — every frame shows only the current source — and stderr prints `texture swap out<->in failed (unbound or shadowed slot) — feedback state did NOT advance this frame` per frame.

**Root cause (observed headless, in-app chain path unverified):** the boundary output's resource carries a borrowed shadow (`MetalBackend::replace_texture_2d` — the mechanism `PresetRuntime::install_target` uses to install the host's canvas texture over `final_output.in` each frame); when the loop's capture source shares that resource, `MetalBackend::swap_texture_2d` (`crates/manifold-renderer/src/node_graph/metal_backend.rs:624`) refuses the ping-pong because a borrowed shadow is present. Its comment says "caller falls back to copies", but the executor's `late_capture` (`crates/manifold-renderer/src/node_graph/execution.rs:1689`) had no copy fallback — it printed the error and dropped the frame's capture, freezing the loop at depth 1.

**Fixed:** `late_capture` now falls back to a format-bridge copy (blit via `copy_texture_to_texture` when producer/state formats+dims match, `resize_sample` when only dims differ — same contract as `node.feedback`'s own `copy_with_format_bridge` in `temporal.rs`) landing `in`'s fresh content into `out`'s persistent texture, instead of dropping the frame. The eprintln now fires only when no copy is possible at all (missing texture, or a genuine format mismatch neither blit nor resize can bridge — a narrower, documented residual gap, not the common case). Regression test: `node_graph::execution::bug_216_gpu_tests::feedback_direct_to_final_output_accumulates_trails` (gpu-proofs feature) builds the exact repro shape (`mix` Add-blending a constant source against its delayed output, wired straight to `mix→feedback.in` AND `mix→final_output`, `final_output`'s resource installed via `replace_texture_2d` to reproduce the real borrowed-shadow condition) and asserts the readback value compounds monotonically frame over frame; verified failing (frozen at the alloc-frame value forever) with the fix reverted.

### BUG-218 (modifier-commands-splice-at-dead-group-output-vertices-port) — the D6 modifier-stack commands still target the pre-D12 splice point, so "Add modifier" silently no-ops on every real grouped object — found 2026-07-17, SCENE_OBJECT_AND_PANEL_V2 P5 flow-script verification

**Status:** FIXED 2026-07-17 (lane/scene-bugfixes) — `walk_mesh_modifier_chain`/`splice_modifier_into_chain` (`crates/manifold-editing/src/commands/graph.rs`) now resolve the group's `node.scene_object` via the group output's `object` producer (`find_scene_object_at_group_output`, mirrors `scene_vm.rs::find_scene_object_in_group`) and walk/splice against ITS `vertices` port instead of the dead `system.group_output` `vertices` port. Test fixture `object_group_scene` rebuilt to the real D12 shape (mesh → modifiers → `node.scene_object.vertices`, `scene_object.object` → group_output); all existing insert/remove/move inverse-pair tests now exercise the real shape and pass. Verified end-to-end: `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-modifier-stack.json` — modifier rows now appear after Add/Bend clicks and undo restores correctly (previously a silent no-op).
**Escaped: lane/scene-bugfixes · caught-by: landing workspace sweep** (`manifold-app::ui_bridge::project::tests::insert_modifier_on_scene_starter_lands_in_the_object_group_body`) — the initial fix above only handled the import shape (scene_object INSIDE the group); it broke the OTHER legitimate D12-era shape, `migrate_scene_object_wires`'s output (e.g. the bundled `SceneStarter.json`), where the minted `node.scene_object` stays a ROOT-level sibling of the mesh group and the group still exports `vertices` directly (`scene_vm.rs:617-618`). Root cause of the escape: the focused gate covered `manifold-editing` + `manifold-renderer` only; this consumer test lives in `manifold-app`. Fixed same day: `walk_mesh_modifier_chain`/`splice_modifier_into_chain` now branch on shape per-call — if the group output's `object` port resolves to a `node.scene_object` (import shape), walk/splice against its `vertices` input; otherwise (migrated/starter shape) walk/splice against the group output's own `vertices` port directly (the original pre-fix behavior, restored for this shape only). New inverse-pair tests `insert_modifier_on_migrated_shape_splices_at_group_output_and_undo_restores` + `remove_and_move_modifier_on_migrated_shape_splice_at_group_output_and_undo_restores` (`manifold-editing`) cover the migrated shape directly; `insert_modifier_on_scene_starter_lands_in_the_object_group_body` (`manifold-app`) now passes. Gate widened to include `manifold-app` for this fix.

**Symptom:** clicking a modifier "Add" chip (e.g. "Twist") in the Scene Setup dock's Properties body dispatches `SceneSetupAddModifier` → `InsertMeshModifierCommand::execute`, which returns without mutating anything — no modifier appears in the stack, no undo entry is pushed, no error, no log. Reproduced via `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-modifier-stack.json` against the real `cc0__oomurasaki_azalea_r._x_pulchrum.glb` fixture: the dispatch log shows `SceneSetupAddModifier(LayerId, 15, "node.twist_mesh")` firing, but the rebuilt tree still shows zero modifier rows (just the "Add modifier:" chip row, unchanged).

**Root cause:** `InsertMeshModifierCommand::execute` (`crates/manifold-editing/src/commands/graph.rs:3796`) does `let out_id = nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?.id;` then `walk_mesh_modifier_chain(nodes, wires, out_id)` — walking backward from `system.group_output`'s **`vertices`** port. That port existed pre-D12 (the group interface re-exported `vertices`/`material`/`transform` directly). D12's `build_import_graph`/`AddSceneObjectCommand` shape wires the mesh chain into `node.scene_object`'s own `vertices` INPUT instead, and the group boundary now only exports `object` (`gltf_import.rs`: `group_wires.push(wire(scene_object_id, "object", out_id, "object"))` — no `vertices` wire to the group output at all). `walk_mesh_modifier_chain` finds no producer for `out_id`'s `vertices` port, returns `None` on its very first lookup, and the whole command silently declines — matching the D6 "unparseable chain refuses the insert" tolerance doctrine by construction, but for the WRONG reason (a stale splice point, not a genuinely malformed chain). `RemoveMeshModifierCommand`/`MoveMeshModifierCommand` (same file, share `walk_mesh_modifier_chain`/`splice_modifier_into_chain`) have the identical bug. This is exactly the class of gap `feedback_no-silent-fallbacks` warns about, except the "fallback" here is an editing command's own stale assumption, not a rendering path.

**Fix shape (not applied this session — out of P5's blast radius, which was scoped to `manifold-ui`/`manifold-app`/`scene_vm.rs` only):** re-target `walk_mesh_modifier_chain`'s entry point at the group's `node.scene_object`'s own `vertices` INPUT port instead of `system.group_output`'s `vertices` OUTPUT port (find the scene_object the same way `scene_vm.rs::find_scene_object_in_group` now does — via the group_output's `object` producer — then walk backward from ITS `vertices` input, not the group boundary's). `splice_modifier_into_chain`'s re-wire target needs the same swap (the newly-spliced modifier's output must feed `scene_object.vertices`, not `group_output.vertices`). Needs its own inverse-pair unit tests using the D12 group shape (scene_vm.rs's own test fixtures in this session's diff are a ready template — `grouped_scene_object_def`). Also update this doc's Status line and the P5 landing report's escalations list once fixed.

### BUG-212 (duplicate-scene-object-string-bindings-dangle-on-imported-mesh) — `DuplicateSceneObjectCommand`'s fresh NodeIds break the "Model File" string binding on a cloned glTF-imported object's mesh source, so the clone has no path and loads no geometry — found 2026-07-17, SCENE_OBJECT_AND_PANEL_V2_DESIGN P3, via the render-path proof (`duplicate_demo_pair_renders_original_then_original_plus_offset_copy`, manifold-renderer gpu-proofs)

**Status:** FIXED 2026-07-17 (lane/scene-bugfixes) — `deep_clone_with_fresh_ids` now also collects an (old NodeId, new NodeId) map across the whole cloned subtree; `DuplicateSceneObjectCommand::execute` uses it to clone every `string_bindings` entry whose target falls inside the duplicated subtree (same `id`/`label`/`default_value`, re-targeted at the clone's fresh NodeId), via `resolve_target_instance` at the same undo-unit boundary `RenameSceneObjectCommand`'s D5 sweep uses; undo restores the whole `string_bindings` vec (whole-snapshot convention). `bindings`/`exposed_params` remain excluded per D11. The gpu-proofs demo test `duplicate_demo_pair_renders_original_then_original_plus_offset_copy` (`gltf_import.rs`) had its manual post-clone path-stamping workaround replaced with the real fix's shape (clone matching `string_bindings` entries via a node-id map), proving the actual mechanism rather than a demo-only stopgap.

**Symptom:** click Duplicate on an imported object (e.g. a glTF mesh). The command succeeds (undo/redo, wiring, `objects` count, transform offset all correct — proven by
`duplicate_scene_object_command_clones_grouped_object_with_fresh_ids_and_undo_restores`), but the clone renders nothing: its mesh source node has no geometry.

**Root cause:** the importer's "Model File" card control is one `StringBindingDef` per file-dependent node (`BindingTarget::Node { node_id, param: "path" }`, `default_value` = the resolved file path) — addressed by stable `NodeId`, not doc id. `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source`/`node.gltf_texture_source` never carry a literal `path` param in the def; it only ever arrives via this binding's `default_value` resolving against the node's `NodeId`. `DuplicateSceneObjectCommand`'s `deep_clone_with_fresh_ids` (`crates/manifold-editing/src/commands/graph.rs`) correctly mints a FRESH `NodeId` on every cloned node per D11 ("fresh NodeIds make cloned bindings dangle by construction") — but D11's own text was written about CARD exposes (`exposed_params`/`UserParamBinding`, performer-controlled sliders), which are legitimately meant to dangle. `string_bindings` is a different, non-performer-facing infra mechanism (keeps every file-dependent node in an import pointed at the same physical file) that hits the exact same "addressed by NodeId" mechanism unintentionally — dropping it isn't a deliberate tradeoff, it silently breaks mesh loading for the shipped importer's own object shape.

**Fix shape:** `DuplicateSceneObjectCommand` should also clone (with the SAME `default_value`, re-targeted at the clone's fresh NodeId) every `string_bindings` entry whose target NodeId falls inside the duplicated subtree — mirroring what it already does NOT do for `bindings`/`exposed_params` (those stay excluded, per D11). Needs `PresetInstance`/`preset_metadata` access at the SAME undo-unit boundary the D5 rename sweep (`RenameSceneObjectCommand`) already reaches via `resolve_target_instance` — likely the same shape, appending fresh `StringBindingDef`s instead of rewriting `section`. A synthetic-mesh object (no `string_bindings`, e.g. `AddSceneObjectCommand`'s cube) is unaffected and needs no fix. Demo-only workaround used to produce a real render proof without this fix: `render_once`'s caller in `gltf_import.rs`'s `duplicate_demo_pair_renders_original_then_original_plus_offset_copy` manually copies the source's resolved `path` onto the clone's mesh nodes post-clone — NOT present in the shipped command, do not mistake it for the fix.

### BUG-185 (e6-texture-completion-invalidates-two-stale-goldens) — `CompareSpecular.glb` and `CompareVolume.glb` genuinely regress in `glb_conformance_sweep` after E6's texture-completion sweep wires `specularTexture`/`specularColorTexture`/`thicknessTexture` for the first time — expected consequence of fixing the gap, not a shading bug
**Status:** FIXED with BUG-211's landing (2026-07-17) — the visual-confirmation call this entry was waiting on was made by eyeballing both renders: CompareSpecular's golden re-baselined (glossy spheres correct), CompareVolume's `region_green_minus_red_above` region moved from the bowl's upper interior (legitimately thin/clear once E6 honored `thicknessTexture`) to the thick lower interior where the Beer-Lambert tint lives (measured G-R 10.13; floor 8 → 6 for margin, ~0 with volume off). Manifest note carries the same rationale.

**Symptom:** `cargo test -p manifold-renderer --test glb_conformance --features gpu-proofs` fails two previously-passing `expect_pass` cases: `CompareSpecular.glb: golden mismatch: mean_abs_diff 3.34 > tol 2` and `CompareVolume.glb: region mean G-R 3.05 <= floor 8`. Both goldens/region-checks were captured/calibrated in G-P7 (`37c81fba`, 2026-07-15) — before E6 existed — against the OLD factor-only rendering (no `specularTexture`/`specularColorTexture`/`thicknessTexture` support).

**Root cause (confirmed by rendering + inspecting each asset's actual texture, not guessed):** both assets carry the exact texture E6 now newly honors, and turning it on legitimately changes the render:
- `CompareSpecular.glb`'s "glTF Logo Specular" material sets `specularColorFactor=[10,10,10]` (deliberately >1, meant to be scaled down by a texture) with BOTH `specularTexture` and `specularColorTexture` pointing at the SAME image (a red damask decorative texture whose ALPHA channel is a separate, spatially-varying strength mask averaging ~0.49, and whose RGB is reddish, not white). The stale golden was rendered with the factor `[10,10,10]` applied UNIFORMLY (texture ignored) — a strong, broad, near-white highlight. The new (correct) render modulates per-texel by that alpha mask and tints by the reddish RGB, producing a dimmer, spatially-varying, reddish highlight — verified visually (`/tmp/compare_specular_e6.png` vs the checked-in golden) and confirmed against the raw extracted PNG's actual RGBA content.
- `CompareVolume.glb`'s "glTF Volume" material's `thicknessTexture` (G channel) has a near-zero band that lands almost exactly on the bowl's THIN GLASS RIM (physically correct — rims are thinner than the base) — exactly where the test's calibrated region `[0.502,0.458,0.555,0.49]` samples. The stale calibration assumed the flat `thicknessFactor=0.75` applied everywhere (no texture), giving strong uniform Beer-Lambert tinting in that region; the new (correct) render has near-zero attenuation at the thin rim, dropping the measured region's G-R from >8 to 3.05.
- Also found and FIXED in the same session (not the cause of either failure above, but adjacent and real): `wire_map_texture`'s `map_tex_cache` was keyed by `tex_index` ALONE (`gltf_import.rs`) — safe for the base five maps (any shared index always wants the same decode, e.g. ORM) but wrong once one extension family (KHR_materials_specular here) legally reuses the SAME image index under TWO DIFFERENT decodes (linear-alpha vs sRGB-rgb). Fixed by keying on `(tex_index, color_space, channel_mode)`.

**Fix shape:** this is a re-baselining call, not a code fix — visually confirm the NEW renders are correct (they read as spec-compliant per the diagnosis above), then regenerate `tests/fixtures/gltf/goldens/compare_specular.png` and move `CompareVolume.glb`'s region (to a thicker part of the bowl) or its G-R floor to match the texture-aware rendering, same discipline as every prior family's Compare-asset re-certification in this design doc. Whoever lands E6 next should do this as the final "certification" step E6's own brief describes (manifest re-classification + status-doc arithmetic) — flagging per CLAUDE.md's "bug found but not fixed this session" rule.

### BUG-215 (conformance-sweep-panics-on-duplicate-mat-0-handle) — a glTF material named like its own inner handle (`"mat_0"`) panics `Graph::add_node_named` on duplicate handle — found 2026-07-17 during IMPORT_ANYTHING_WAVE Lane W6's landing gate, independently rediscovered and fixed by Lane W5

**Status:** FIXED same session. Not caused by Lane W5's own work (the .exr HDRI decode fix) — found only because the wave's gate requires the conformance sweep green before landing, and it wasn't. Lane W6 diagnosed this first (Fable-advisor-confirmed) but is analysis-only by design and left it OPEN with a fix-shape recommendation; Lane W5 independently hit the same red gate, consulted its own Fable advisor per the wave's escalation clause, and re-verified the mechanism directly against the code and the asset's own JSON before landing the fix below. The two diagnoses agree on the root cause; see W6's original write-up (superseded by this entry) for the fuller three-handle account.

**Symptom:** `cargo test -p manifold-renderer --features gpu-proofs --test glb_conformance` panics on `MetalRoughSpheresNoTextures.glb`:
```
thread 'glb_conformance_sweep' panicked at crates/manifold-renderer/src/node_graph/graph.rs:137:13:
Graph::add_node_named: duplicate handle 'mat_0/mat_0' (already mapped to NodeInstanceId(9), just tried to remap to NodeInstanceId(11)). Handles must be unique within a graph.
```

**Root cause (confirmed by reading the code and the asset's own JSON, not guessed):** `MetalRoughSpheresNoTextures.glb` has 98 materials literally named `"mat_0"` through `"mat_97"` (verified: parsed the GLB's JSON chunk directly). `build_object_group` (`gltf_import.rs`) names its inner `node.pbr_material` handle `format!("mat_{k}")` — for object 0, `"mat_0"`. `unique_group_name(m.name.as_deref(), k, ...)` — used for BOTH the object's group-box handle AND, since SCENE_OBJECT_AND_PANEL_V2 P3 (commit `1a5786cb`, landed on `origin/main` before this lane branched), the inner `node.scene_object` node's handle (D6: "the object IS its scene_object node; the name is its handle") — took the material's raw name verbatim, so for this asset it also produced `"mat_0"`. Both nodes live inside the SAME group body; `flatten_groups` prefixes each inner handle with the group's own handle (`{group_handle}/{inner_handle}`), so the group handle `"mat_0"` containing an inner `"mat_0"` (the pbr_material) AND the scene_object node ALSO handled `"mat_0"` (identical to the group's own handle) both flatten to the literal string `"mat_0/mat_0"` — two distinct nodes, same flattened name. `Graph::add_node_named` (`graph.rs:137`), invoked at graph-load time (`graph_loader.rs`), rejects the second one. Pre-P3, `build_object_group` created no `scene_object` node at all (verified: `git show 1a5786cb^ -- crates/manifold-renderer/src/node_graph/gltf_import.rs`), so this collision is new in P3 and was never exercised by the conformance sweep until this session's run (P3 landed same-day, just ahead of this lane).

**Fix:** `unique_group_name` (`gltf_import.rs`) now also dedupes against a new `collides_with_object_group_inner_handle(name, k)` helper naming the object's own deterministic inner-handle vocabulary (`mesh_{k}`, `mat_{k}`, `pose_{k}`, `skinmesh_{k}`, `morphweights_{k}`, `morphdeltas_{k}`, `morphblend_{k}`, `transform_{k}`, `anim_{k}`, `tex_{k}`, and the literal `"output"` group-output boundary handle) — a colliding material name now gets the same numeric-suffix treatment `unique_group_name` already gives sibling name collisions. This differs from W6's originally-recommended fix shape (rename the colliding *inner* node instead of the group/scene_object name) — both approaches close the collision; this one keeps `unique_group_name` as the single place that owns group-name uniqueness. Regression test `material_named_like_its_own_inner_handle_does_not_collide` (synthetic `GltfImportSummary` with a material named `"mat_0"`, asserts the flattened graph has no duplicate handles) — verified it fails without the fix and passes with it; this also closes W6's first follow-on (a GPU-free regression test in the default sweep). Conformance sweep reran green after this fix.

**Still open (W6's second follow-on, not attempted here):** `RenameSceneObjectCommand` (`manifold-editing/src/commands/graph.rs:3390`) sets a scene object's handle with no visible collision guard against sibling inner handles — a performer renaming an object to e.g. `"mesh_0"` at runtime could still hit the same class of panic on next graph rebuild, since that path doesn't go through `unique_group_name`.

### BUG-210 (add-scene-object-command-emits-pre-migration-legacy-wires) — `AddSceneObjectCommand`'s `catalog_default` still emits `mesh_k`/`material_k`/`transform_k`-shaped wires into `render_scene`, which no longer reads them post-SCENE_OBJECT_AND_PANEL_V2 P2 — found 2026-07-17 landing P1+P2

**Status:** FIXED @ 6e8b00ba — P3. `catalog_default`'s spliced group now binds mesh/material/transform through an inner `node.scene_object`, exposing a single `Object` interface port wired to `render_scene`'s `object_k` port; `add_scene_object_command_bumps_count_builds_group_and_undo_restores` updated to assert the new shape (5 body nodes incl. scene_object bind, single `object` interface output, `object_2` top-level wire).

**Symptom:** clicking "+ Object" in the scene panel bumps `render_scene`'s `objects` count and creates a group node, but the group wires its mesh/material/transform outputs to the legacy `mesh_k`/`material_k`/`transform_k` ports — `render_scene` v2 (P2, `object_{i}`-only surface) has no such ports anymore, so the added object is invisible and casts no shadow.

**Root cause:** `AddSceneObjectCommand` (`crates/manifold-editing/src/commands/graph.rs`) was not touched by P1/P2 — its `catalog_default` (the JSON template spliced in on Add) predates the Object-wire model. `RemoveSceneObjectCommand`'s mirror-image break (found by the same landing's full workspace sweep, when a concurrent lane's same-day BUG-193 fix collided with P2's port deletion) was fixed in the same landing; Add was flagged by a Fable advisor consult but deliberately left alone — it's SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P3's own committed deliverable ("`AddSceneObjectCommand`'s `catalog_default` emit scene_object-shaped objects"), not a same-session patch.

**Fix shape:** exactly P3's brief — retarget `catalog_default` to splice a `node.scene_object` (or its enclosing group, matching D5's placement rule) wired to the next free `object_{i}` port, instead of the legacy triplet. No new mechanism; P3 was always going to build this.

### BUG-211 (conformance-harness-advancing-clock-cant-converge-animated-imports) — every animated Khronos asset "never converged after 300 frames (last non-black fraction 0.0000)" in glb_conformance while rendering perfectly — found 2026-07-17 (surfaced by BUG-207's lane running the full gpu-proofs sweep; diagnosed same session)

**Status:** FIXED (this entry's landing commit, `lane/bugfix-210-conformance-frozen-time` — branch named before BUG-210 was claimed by the scene-object session's finding).

**Symptom:** 6 assets (CesiumMan, Fox, BrainStem, RiggedFigure, RiggedSimple, AnimatedMorphCube) failed `glb_conformance_sweep` with "never converged … last non-black fraction 0.0000", plus 3 golden mismatches (AlphaBlendModeTest, CompareSpecular, RecursiveSkeletons). Deterministic across runs — not device-contention flake.

**Root cause:** two independent staleness effects, neither a render bug. (1) `render_asset`'s convergence loop advances `time`/`beat` every frame and requires 3 byte-identical frames before it ever measures blackness; since GLTF_ANIMATION A1–A4 imports auto-play, an animated asset re-poses every frame, never goes byte-stable, and `last_fraction` keeps its 0.0 initializer — the reported "black" is a phantom (the same loop in `src/bin/render_import.rs` had the identical gap). (2) The golden mismatches were legitimate reframes: BUG-205 moved skinned assets' bbox into bind-skinned space and BUG-206's per-axis fit reframed wide/tall assets — the goldens were captured pre-fix (RecursiveSkeletons' old golden was visibly cropped top and bottom; the new one frames the full object).

**Fix:** freeze the clock across both convergence loops (`time`/`beat`/`anim_progress` constant, `frame_count` still advancing for the decode/io paths — byte-stability still catches late texture swaps, which was the loop's real job); `render-import` gained `--time SECONDS` to pick the animation moment. Six goldens regenerated and individually eyeballed (walking CesiumMan, posed BrainStem/RiggedFigure, fully-framed AlphaBlendModeTest wall, CompareSpecular spheres, uncropped RecursiveSkeletons pillar); the other 64 within-tolerance golden rewrites the update pass produced were deliberately reverted, not committed. Sweep runtime dropped 701s → ~180s (no more 300-frame timeout per animated asset).

### BUG-207 (materialless-skinned-mesh-silently-imports-static-at-node-scale) — a rigged mesh with no material renders invisible (~100× wrong-sized) with nothing in the report — found 2026-07-17 (Fable seam-hunt session; hostile-shelf probe, Blender-exported materialless rig)

**Status:** FIXED @ f3358d00 (`lane/bugfix-207-default-material-skin`).

**Symptom:** import succeeds, report shows `default_material_vertex_count > 0`, frame renders black (lit fraction exactly 0.0000). The geometry IS imported (BUG-171's `DEFAULT_MATERIAL_MESH_PARAM = -2` path loads all vertices — verified: static DefaultOnly loader returns the full 372) but as a STATIC mesh at the wrong size.

**Root cause:** two stacked seams. (1) `gltf_load.rs` skin/morph/animation resolution keys `nodes_by_material.get(&Some(material_index as usize))` — materialless primitives live in the `None` bucket and the synthetic default-material entry carries the `u32::MAX` sentinel, so `skin`/`morph` NEVER resolve for it (the synthetic entry is also hardcoded `skin: None`); `find_skinned_node_for_material` has the same `Some(...)` comparison. (2) With the skin lost, the object falls to `node.gltf_mesh_source`, which world-transforms by the mesh node's transform — a transform glTF skinning IGNORES (§3.7.3.3), and which Blender exports expect to be ignored (skinned POSITIONs are written in bind-meters). Measured on the probe fixture: mesh-node-world bbox 0.01 tall vs bind-skinned 1.0 tall — the render is 1% of the framed size. No report line fires (the only signal is a console warn, whose text — "v1 does not import these" — is itself stale since BUG-171 DID make them import).

**Fix:** the default-material bucket (`nodes_by_material`'s `None` key) is now a first-class key, not a special case. `gltf_load.rs`'s three duplicated skin/morph/animation resolution blocks (previously inline in `gltf_import_summary`'s per-material loop, each keyed `Some(material_index as usize)`) are factored into shared `resolve_skin_for_key`/`resolve_morph_for_key`/`resolve_animations_for_key` functions parameterized by `key: Option<usize>`; the per-material loop calls them with `Some(idx)` exactly as before, and the synthetic default-material entry now calls the SAME functions with `key: None` instead of hardcoding `skin: None, morph: None, animations: Vec::new()`. A new `primitive_material_matches` helper (sentinel-aware `Option<usize>` comparison) replaces every bare `primitive.material().index() != Some(material_index as usize)` in `flatten_skinned_node`, `find_skinned_node_for_material`, `find_mesh_node_for_material`, and `load_gltf_morph_deltas`'s primitive loop. `gltf_import.rs`'s `build_object_group` now translates `m.material_index == DEFAULT_MATERIAL_SENTINEL` to `DEFAULT_MATERIAL_MESH_PARAM` (-2) for BOTH `node.gltf_skinned_mesh_source`'s and `node.gltf_morph_deltas_source`'s `material_index` params (mirroring the pre-existing static-mesh branch) — without this, the sentinel `u32::MAX as i32` would collide with -1, the params' own "unset" convention. Both primitives' `material_index` `ParamDef` ranges widened to `(-2.0, 1024.0)` and their `run()` gates changed from `material_index >= 0` to `material_index >= 0 || material_index == DEFAULT_MATERIAL_MESH_PARAM`, translating -2 back to `DEFAULT_MATERIAL_SENTINEL` before calling `load_gltf_skinned_mesh`/`load_gltf_morph_deltas` — the same two-sentinel-space split `gltf_mesh_source.rs` already used. Merged after BUG-208 landed on main; the sentinel translation for `node.gltf_morph_deltas_source` was reconciled against BUG-208's new `skinned: bool` parameter on `load_gltf_morph_deltas`/`node.gltf_morph_deltas_source` (both changes compose — the sentinel selects WHICH primitives contribute, `skinned` selects WHICH coordinate space, orthogonal concerns). The stale "v1 does not import these" warn (`assemble_import_graph`, BUG-171 made it false) reworded to describe what actually happens.

**Regression:** `tests/fixtures/gltf/hostile/mixamo_like_nomat.glb` — a materialless variant of the existing `mixamo_like.glb` skinned rig, added to the hostile shelf (picked up automatically by `hostile_fixtures_*`). `hostile_fixtures_render_within_framing_invariants` (GPU) is the direct regression proof: measured lit fraction 0.1820, byte-for-byte identical to `mixamo_like.glb`'s own 0.1820, not cropped — confirmed via a temporary debug print, reverted before commit. Confirmed the bug reproduces pre-fix even more severely than "renders black": with the fix reverted, the harness panics (`assembled graph has no node.gltf_skeleton_pose with a duration_s param`) because the materialless object never got a skeleton pose node at all — it fell all the way through to the static-mesh branch, as the root-cause analysis predicted. Full gates (post-merge with BUG-206 and BUG-208): `cargo test -p manifold-renderer --lib` 1369/1369; `cargo test -p manifold-renderer --lib --features gpu-proofs hostile` 3/3; `cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings` clean; `cargo nextest run --workspace` 3564/3564. `docs/node_catalog.json` regenerated for the two widened param ranges.

### BUG-208 (skin-plus-morph-drops-morph-silently) — an object with both a skin and morph targets imports with its morph animation gone and no report line — found 2026-07-17 (Fable seam-hunt session; code-confirmed, no fixture yet)

**Status:** FIXED @ 50db9369 (`lane/bugfix-208-skin-morph`).

**Symptom:** body skinning animates, face/blendshape animation is absent; the import report says nothing (D9 "every unmapped feature is a report line, never a silent drop" violation).

**Root cause:** `gltf_import.rs` `build_object_group`: `morphed_vertices_source` was only built when `skinned_vertices_source.is_none()` (A3's documented out-of-scope call, "re-derive if a future asset needs both") — the skip branch pushed no report line and dropped the morph wiring entirely. A second, deeper seam sat behind the documented one: `node.gltf_morph_deltas_source`'s loader (`load_gltf_morph_deltas`) always world-transforms its deltas by the mesh-owning node's world matrix — correct for the rigid (non-skinned) path, where `node.gltf_mesh_source` world-transforms its base vertices the same way, but WRONG for a skinned object, whose base vertices (`flatten_skinned_node`) are emitted in UNTRANSFORMED bind-pose/local space per the A2 doctrine (a skinned object's positioning comes entirely from its joint palette). Naively chaining the two without addressing this would have put the deltas and the base vertices in different coordinate spaces.

**Fix:** real fix shipped, not just the minimum report line. glTF applies morph THEN skin (§3.7.2): `node.morph_targets_blend` now chains between `node.gltf_skinned_mesh_source`'s vertices and `node.skin_mesh`'s `in` when an object carries both. `load_gltf_morph_deltas` gained a `skinned: bool` parameter (threaded through a new `node.gltf_morph_deltas_source` `skinned` Bool param, set by `gltf_import.rs` from `skinned_vertices_source.is_some()`) that skips the world-transform entirely (identity matrices) for the skinned case, keeping the deltas in the same untransformed space as the skinned base vertices — `flatten_skinned_node` and `flatten_primitive_morph_deltas` share the identical triangle-index expansion, so per-vertex order lines up 1:1 without further changes. New report line names the composed chain and cites BUG-208. Regression: `tests/fixtures/gltf/hostile/skin_morph.glb` (Blender armature-skinned cylinder + keyframed "Bulge" shape key, both a skin and morph targets with animation channels for each) — new CPU test `skin_and_morph_combination_composes_instead_of_dropping` asserts the report line, the `node.morph_targets_blend` → `node.skin_mesh` wire, and the deltas source's `skinned` param; the existing hostile-shelf sweep tests (`hostile_fixtures_assemble_validate_and_build`, `hostile_fixtures_merge_into_existing_scene`, `hostile_fixtures_render_within_framing_invariants`) pick the new fixture up automatically and pass (framing/lit-fraction invariants hold). `cargo test -p manifold-renderer --lib --features gpu-proofs morph`/`skinned` green.

### BUG-206 (import-framing-crops-elongated-objects) — tall/thin imports overflow the synthesized camera's frame — found 2026-07-17 (first synthetic hostile-fixture run: Blender-generated skinned cylinder, 10:1 aspect, cropped top AND bottom at default framing)

**Status:** FIXED @ e182a391.

**Symptom:** an elongated import (bind-skinned bbox ~1.0 tall × 0.2 wide) renders correctly sized but overflows the default frame vertically — lit pixels touch both top and bottom edges. Compact objects (azalea, helmet, skeleton_animated post-BUG-205) frame fine.

**Root cause:** `assemble_import_graph`'s `distance = 2.2 * radius` (gltf_import.rs, near the BUG-165/169 near-clip comment) uses the bbox half-DIAGONAL as `radius`. For an object dominated by one axis the diagonal barely exceeds that axis, so the frame's vertical span (`2 * distance * tan(fov_y/2)` ≈ 1.08 × height at fov_y 0.9) contains the object with almost no margin — and camera tilt (0.3) plus perspective (near surface projects larger) push it past the edges.

**Fix:** `distance` is now `(2.2 * radius).max(per_axis_fit)`, where `per_axis_fit` is the max over the three bbox axes of `(extent/2) / tan(fov_y/2) * 1.15` (`gltf_import.rs::assemble_import_graph`, ~line 2001) — `fov_y` hoisted into one `let` shared by both the framing-distance computation and the camera node's `fov_y` param, so the two can't drift. The render aspect is unknown at import time, so the horizontal half-angle is conservatively treated as equal to the vertical one (square-aspect assumption, never under-frames a wider-than-tall render). The `2.2 * radius` floor keeps every compact asset's framing identical to before (golden-stability guarantee — confirmed by the 56-asset conformance sweep staying 56/56). Regression: `hostile_fixtures_render_within_framing_invariants`'s `EDGE_XFAIL` list (previously `["mixamo_like.glb"]`) is now empty and the sweep additionally checks phase 0.0 (rest pose, the worst case for an elongated rig) alongside the original 0.25 — both phases pass un-xfailed.

### BUG-205 (skinned-import-double-transform-and-wrong-bbox-space) — imported rig renders as a tiny speck, feet cropped below frame — found 2026-07-17 (Peter, skeleton_animated.glb, "the skeleton is TINY")

**Status:** FIXED @ 3aeafe4a (`lane/bugfix-skinned-import-scale`).

**Symptom:** an animated+rigged glb imports (post-BUG-204) but renders ~12px tall at the synthesized framing distance — correctly shaped and animating, just ~40× too small; after fixing that, the skeleton still sat low with its feet cropped below frame.

**Root cause:** two sites treated a skinned object as static, both violating the importer's own A2 doctrine (skinned positioning comes ENTIRELY from the joint palette, glTF 2.0 §3.7.3.3). (1) `resolve_object_animation` walks the mesh node's ancestor chain; skeleton_animated.glb animates `Bip01` — an ancestor ABOVE the joint tree whose static scale (0.0254, the FBX inch→meter conversion) is already inside the joint worlds via `joint_root_world` — so the resulting rigid `node.gltf_animation_source` wired into the object's transform_3d applied the whole chain a second time: 0.0254² ≈ 1/1550 of authored area. (2) `summarize_node` bboxed skinned primitives at mesh-node-world positions (y 0.36..2.22), a space glTF skinning ignores; the render lives at bind-pose-skinned positions (y -0.57..1.20), so the camera framed/recentered a box the mesh never occupies. CesiumMan/Fox never caught either: no scaled/animated ancestor above their rigs, near-coincident spaces.

**Fix:** `build_object_group` skips the rigid animation source when `skinned_vertices_source` is `Some`; `summarize_node` bboxes skinned primitives through new `bind_pose_skin_matrices` (static joint world × inverseBind — the same product `node.skin_mesh` applies). Known remaining approximation, unchanged by this fix: an ancestor that genuinely ANIMATES above the joint tree is still sampled statically (`joint_root_world`); composing an animated prefix into the pose path is a tracked gap, not regressed here. Regressions: `skinned_import_gets_no_rigid_animation_source` + `skinned_import_summary_bbox_is_in_bind_skinned_space` (both CPU, real fixture); headless snapshot goes speck → full-frame (non-black fraction 0.0001 → 0.0716); snapshot test now writes its PNG before asserting so failures leave the frame on disk.

### BUG-204 (animated-glb-import-rejected-by-retrigger-card-lint) — every animated or rigged glb fails at import: "card param '…_retrigger' is marked is_trigger but its binding targets '….trigger_count' … which is not a trigger-typed param" — found 2026-07-17 (Peter, skeleton_animated.glb)

**Status:** FIXED @ 6d7cac31 (`lane/bugfix-gltf-retrigger-trigger-type`).

**Symptom:** `[Import] glTF import failed … assembled graph failed validation: card param 'pose_0_retrigger' is marked is_trigger but its binding targets 'pose_0.trigger_count' (node.gltf_skeleton_pose), which is not a trigger-typed param` (and the same for `anim_0_retrigger` / `node.gltf_animation_source`). Static meshes import fine; anything with an animation clip or a rig is rejected — the exact assets A4 shipped to make performable.

**Root cause:** a four-day collision between two landings. Card lint (d) (`validate.rs`, `c6d809f0` 2026-07-13, graph-tooling P4) requires an `is_trigger` card param to bind to a `ParamType::Trigger` inner param. GLTF_ANIMATION_DESIGN.md A4 (`2a9e8808` 2026-07-17) then added the Retrigger card param bound to `trigger_count` — declared `ParamType::Int` on all three animation nodes (the pre-lint way of writing the monotonic-counter convention). A4's gate never ran the assembled import def through `check_card_lints`, so the conflict only surfaced at app import time.

**Fix:** `trigger_count` flipped from `Int` to `Trigger` on `gltf_animation_source`, `gltf_skeleton_pose`, `gltf_morph_weights` — the declaration the convention already meant (`image_folder`'s `next`/`prev` precedent); runtime semantics unchanged (Trigger params carry the same Float counter, coercion identical). Regression: `animated_and_rigged_import_passes_card_lints` runs `tests/fixtures/gltf/skeleton_animated.glb` through `assemble_import_graph` + `check_card_lints` (now `pub(crate)`) — verified red on the Int declaration, green on Trigger. Node catalog regenerated.

### BUG-198 (ui-automation-key-event-has-no-global-undo-seam) — headless `AutomationAction::Key { key: Z, modifiers: { command: true } }` never triggers Undo — found 2026-07-17 during SCENE_SETUP_PANEL_DESIGN.md P5 (modifier stack)

**Status:** FIXED @ 6318c9fb — `BUGFIX_WAVE_2026_07_17_DESIGN.md` Lane 4 (`lane/bugfix-ui-automation-harness`). The headless `Runner` now owns a real `manifold_editing::undo::UndoRedoManager` (same 200 cap); every `ContentCommand::Execute`/`ExecuteBatch` sent over the harness's `content_tx` is drained and `record()`-ed after each step (never re-executed — `AppEditingHost`/`ui_bridge` already mutate `data.project` synchronously before sending). The `Key` arm intercepts Cmd+Z/Cmd+Shift+Z and dispatches real `undo()`/`redo()`; any OTHER modifier-bearing key with no seam now FAILS LOUDLY instead of returning "ok" (mirrors the `Text` arm's fail), killing the silent-no-op class. New flow `scripts/ui-flows/scene-setup-fog-undo-removes-fog.json` proves it end to end (add fog → Cmd+Z → Density row gone → Cmd+Shift+Z → Density back); `scene-setup-modifier-stack.json`'s 4× Cmd+Z now genuinely walks `undo_count` down (9→8→7→6) instead of no-op'ing.

**Symptom:** Cmd+Z in the real app is a global menu-bar/window-level shortcut (`app_render.rs`'s `M::Undo` menu-action handling); the headless script driver's `Key` step only calls `UIRoot::key_event` → `InputSystem::process_key`, which does nothing unless a widget currently holds text-input focus (`process_key` only pushes a `KeyDown` event when `self.focused_node(tree)` is `Some`). No panel/dock in this codebase focuses a text field by default, so every headless `Key Z` step is a silent no-op: it "succeeds" (status `ok`, no error) while doing nothing. Confirmed directly this session: `scripts/ui-flows/scene-setup-modifier-stack.json` sends 4× Cmd+Z after inserting 2 modifiers + 1 param drag + 1 reorder — the modifier count is unchanged afterward (verified via a temporary post-undo assertion that failed with "found 2", then removed once the cause was confirmed, per the "never claim a green gate you didn't get" rule).

**Root cause:** `AutomationAction::Key`'s headless synthesis path only reaches `UIRoot`, which owns per-widget key handling (text fields, keyboard-drag-nudge, etc.) but NOT the app-level global shortcut table — that lives in `Application`/`app_render.rs`, outside anything `UIRoot::key_event` can reach, mirroring the exact gap `AutomationAction::Text` already documents in its own doc comment ("No headless injection seam exists yet: text editing lives entirely in `Application::text_input`... `UIRoot` can't reach").

**Fix shape:** extend the P3 live-door (`UI_AUTOMATION_DESIGN.md`, the TCP JSON-lines thread already wired for live injection) or the headless script driver itself to recognize `Cmd+Z`/`Cmd+Shift+Z` and dispatch the SAME `M::Undo`/`M::Redo` path `app_render.rs` uses, OR give `AutomationAction::Key` a documented escape hatch that calls `EditingService`'s undo directly against the driver's own `SceneData.project` (mirroring how `Pointer`/`Drag` already bypass the content thread and mutate `SceneData.project` in-process). Until then: every phase gate that wants "undo restores state" proven headlessly must prove it at the COMMAND level (`execute()` + `undo()` unit tests, byte-equal graph assert) — which P5 already does for all three new commands — never by trusting a `Key Z` step's "ok" status.

### BUG-192 (ui-automation-under-text-flat-card-rows) — `SelectorQuery.under_text` never resolves against `param_card.rs` slider rows — found 2026-07-16/17 building GLTF_ANIMATION_DESIGN.md A4's L3 flow

**Status:** FIXED @ 6318c9fb — `BUGFIX_WAVE_2026_07_17_DESIGN.md` Lane 4 (`lane/bugfix-ui-automation-harness`). Confirmed root cause via test-first repro (two new synthetic-tree tests in `automation.rs`, both failing before the fix): (1) the zero-match case this entry names — `param_card.rs`'s generator rows parent flat to the literal tree root (`parent: None`), so a `None`-parented node's ancestor chain is empty and neither the literal-ancestor nor common-ancestor check could ever fire; (2) a second, previously-undetected cross-match risk — `layer_header.rs`'s real shape nests every row's container under ONE shared outer scroll clip, and the old "any shared ancestor, however far up" walk could tunnel through that shared container and match a DIFFERENT row's label (`under_text_walks_ancestors`'s existing test never had a shared outer ancestor to expose this). Fixed `under_text_matches` to climb outward one enclosing level at a time, stopping at the nearest same-parent sibling carrying ANY text (build order puts a row's own label before the rest of that row) and only climbing further when a level has no textful sibling at all — resolves flat-sibling rows without ever crossing into an unrelated row. Both new tests green (`under_text_resolves_flat_sibling_rows`, `under_text_layer_header_shared_scroll_clip_does_not_cross_match`), plus the existing `under_text_walks_ancestors` (tight-container case) unaffected. The `gltf-clip-scrub-retrigger.json` widget-id/`nth` workaround is left in place per the directive's "keep the workaround if flaky" clause — not re-pointed this session.

**Symptom:** A ui-flow script using `{"type": "Button", "under_text": "<row label>"}` (or `under_text` alone) against any `gltf_scene`/`gltfanimscene`-style generator card returns zero matches, even when the labeled row and its value widget both visibly exist (`{"text": "<row label>"}` alone resolves fine). Confirmed empirically: `under_text: "Rate"` and `under_text: "Rate"` + `type: "Button"` both return 0 matches against `gltfanimscene`'s Rate/Clip/Loop Mode/Retrigger rows, while `type: "Button"` alone (no `under_text`) returns 126 matches in the same tree.

**Root cause:** `under_text_matches` (`crates/manifold-ui/src/automation.rs:367`) resolves "under" via a shared-ancestor walk, documented against `layer_header.rs`'s row shape (`layer_header.mute` and its "PLASMA" label are direct children of one tight per-row container `111`). `param_card.rs`'s slider rows don't have that per-row container — every row's label, value badge, and slider body are flat siblings directly under ONE shared card-content parent (verified via `ui-snap --script`'s own dump: node `71` is every row's `parent`, from `Rate`'s label at row y=229 through `Camera Orbit` at y=379). Under the doc comment's own stated "common-ancestor" semantics this should make EVERY row's widgets satisfy `under_text` for EVERY other row's label simultaneously (since they all share ancestor `71`) — instead it resolves to zero for all of them, meaning either the implementation doesn't match its own documented semantics for this topology, or the automation resolver's `nodes[]`/`parent_id` view diverges from what `--dump` prints as `p=`. Not diagnosed further — the fix needs stepping through `under_text_matches` against a live `param_card` dump, which wasn't done this session (schedule pressure on the A4 landing).

**Fix shape:** either (a) fix `under_text_matches` so it genuinely implements "nearest labeled row" (e.g. stop the ancestor walk at the first node with ANY text, not just the queried one, so two same-parent rows don't both "share" an unrelated ancestor) — the general, root-level fix; or (b) give `param_card.rs` genuine per-row containers (a structural change with its own blast radius across every existing card-targeting flow). (a) is the smaller, correctly-scoped fix. No `#[ignore]`-able Rust test exists yet since this is a `manifold-ui`/`ui-automation` behavior, not covered by the crate's own `#[cfg(test)]` suite — a repro belongs in `manifold-ui/src/automation.rs`'s own test module, built against a synthetic flat-sibling tree matching `param_card`'s actual shape (the existing `under_text_walks_ancestors` test at `automation.rs:503` only proves the tight-container case).

**Workaround (shipped):** `scripts/ui-flows/gltf-clip-scrub-retrigger.json` (A4's L3 flow) uses `AutomationTarget::Widget(<id>)` — a raw widget id read from a same-session dump — for the Rate slider drag (no literal text on a slider body), and `{"text": "▶", "nth": 1}` for the Retrigger click (disambiguating three same-text buttons by position, confirmed stable across repeated runs of the same deterministic fixture). Both work but are fragile to future card-layout changes in a way a working `under_text` wouldn't be.

### BUG-183 (fusion-coverage-baseline-slipped) — `fusion_coverage_baseline` fails on main: 32 bundled presets fuse, floor asserts ≥33
**Status:** FIXED 2026-07-17 (Sonnet, BUGFIX_WAVE_2026_07_17_DESIGN Lane 5) — floors lowered/raised to the new bundled reality (presets ≥32, regions ≥56, atoms ≥240) in `crates/manifold-renderer/src/node_graph/freeze/proof.rs`'s `fusion_coverage_baseline`, comment rewritten citing `a065dec4`. Measured at tip `1a161d91`: 32 presets / 56 regions / 243 atoms — matches the root-cause investigation below exactly. Test green.

**Symptom:** `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::freeze::proof::fusion_coverage_baseline` fails with "expected ≥33 bundled presets to fuse, got 32". Isolated as pre-existing: the agent restored `mix.wgsl`/`mix_body.wgsl` to their HEAD content and the test failed identically, so the BUG-181 alpha fix is not the cause — some landing on or before `02c5fbd5` dropped one preset out of fusion coverage without lowering (or noticing) the baseline.

**Root cause (identified 2026-07-17, Fable, verified by running the test + `git show`):** NOT a partition regression — commit `a065dec4` (2026-07-16) unbundled eight 3D-infra presets to `assets/reference-presets/`, and CinematicScene (which fused — its fused-WGSL golden was deleted in that same commit) left the bundled set, dropping the bundled fused-preset count 33 → 32. Fusion itself RATCHETED UP across the same window: measured at tip `9a7a7fa2`, 32 presets / 56 regions / 243 atoms vs the P6 floors 33/55/225. The earlier "do NOT just lower the floor" instruction assumed a regression and is superseded by this evidence.

**Fix shape:** update the floors to the new bundled reality (presets ≥32, regions ≥56, atoms ≥240 with the test's usual churn headroom) and rewrite the floor comment citing `a065dec4`. Directive: BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 5.

### BUG-196 (is-multiple-of-clippy-debt-gltf-import-render-scene) — `cargo clippy --features gpu-proofs --tests -- -D warnings` fails on 6 pre-existing `manual_is_multiple_of` lints outside this phase's touched files — found 2026-07-17 during RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1

**Status:** FIXED (bugfix-wave-2026-07-17 Lane 3) — all 8 sites found by `rg "% 4 != 0|% 2 == 0"` across `gltf_import.rs` (7, count drifted up from the 5 originally named here) and `render_scene.rs` (1) rewritten to `.is_multiple_of()` / `!….is_multiple_of()`. No behavior change. `cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings` and `cargo clippy -p manifold-renderer -p manifold-gpu --features gpu-proofs --tests -- -D warnings` both clean.

**Symptom:** `cargo clippy -p manifold-renderer -p manifold-gpu --features gpu-proofs --tests -- -D warnings` fails with 6 `clippy::manual_is_multiple_of` errors: `gltf_import.rs:2368`, `:2510`, `:5191`, `:5196`, `:5235` (all `while <len>.len() % 4 != 0 { … }` padding loops) and `render_scene.rs:4493` (`if band % 2 == 0 { … }`). The plain scoped clippy P1 actually gates on — `cargo clippy -p manifold-renderer -p manifold-gpu -- -D warnings` (no `--features gpu-proofs --tests`) — is clean; these lints only surface once the gpu-proofs feature's test binaries pull that code into a lint pass, which P1's phase brief didn't require but running the full gpu-proofs test suite for the four touched glTF source primitives incidentally exercised.

**Root cause:** a clippy version bump added the `manual_is_multiple_of` lint (stabilized `u32::is_multiple_of` in a recent Rust release); the 6 sites predate the lint and were never touched by any session since.

**Fix shape:** mechanical — replace `x % n != 0` with `!x.is_multiple_of(n)` (and `x % n == 0` with `x.is_multiple_of(n)`) at the 6 named sites. No behavior change. Whoever next touches `gltf_import.rs` or `render_scene.rs` for an unrelated reason should fold this in rather than opening a dedicated session for 6 one-line rewrites.

### BUG-195 (scene-setup-merge-no-stored-object-radius-for-scale-sanity) — SCENE_SETUP_PANEL_DESIGN.md D5's merge scale-sanity rule has no literal "scene reference radius" to read

**Status:** FIXED (bugfix-wave-2026-07-17 Lane 3) — BUG-194 shipped first, so the real fix named below (a stored per-object size signal) was available: `max_known_source_bbox_radius` reads the largest KNOWN `source_bbox_radius` (BUG-194's provenance param) recursively across the target def's existing mesh-source nodes and prefers it over the orbit-camera proxy, which remains only as the fallback for a def with no known-radius mesh-source node at all. Unit test `merge_scale_sanity_prefers_stored_radius_over_camera_proxy` (`gltf_import.rs`) proves the stored value — not the proxy — drives the decision when the two disagree.

**Symptom:** D5 commits to: "iff the incoming bbox radius differs from the scene's reference radius (the largest existing object's) by more than 10× in either direction, the seeded `scale` is `ref_radius / incoming_radius`". Reading `merge_import_into_graph`'s inputs (`&EffectGraphDef`, a freshly-parsed `GltfImportSummary`, the target `.glb` path), there is no per-object bbox/radius stored anywhere in the def to read "the largest existing object's" size from — the identical gap BUG-193/194 found for object counts and vertex counts, just for size this time.

**Root cause:** design-time assumption — D5 was written assuming a per-object size signal existed or could be trivially derived; on inspection, `manifold_core::effect_graph_def::EffectGraphNode`/`GroupDef` carry no geometry metadata at all (procedural mesh generators are Rust-side formulas, imported meshes' bbox is consumed once at import time to seed the recenter transform and then discarded).

**Fix shape (shipped as a defaulted proxy, not blocked on):** `merge_import_into_graph` derives a "scene reference radius" from the target's own top-level `node.orbit_camera`'s `distance` param, inverted through the EXACT formula `build_import_graph` used to seed it (`distance = 2.2 * radius`) — the camera's framing distance already encodes "how big is everything in this scene" as of the last import/creation that mattered. When no such camera exists at the top level, normalization is skipped entirely (native units) rather than guessed. Known blind spots, both honest and low-severity: (1) a user who hand-retunes Camera Distance on the card shifts this proxy without changing the actual scene scale; (2) a hand-built or non-importer-shaped scene (no top-level `node.orbit_camera`, or one with a hand-set `distance`) gets no normalization at all, ever. Real fix (generalizes BUG-193/194's option (a)): stash a real per-object size signal (e.g. bbox radius as a node param) at import/generation time and read THAT instead — Peter's call on scope, since it's bigger than a P4-only change.

### BUG-194 (scene-setup-vertex-count-not-computable-from-def) — Scene Setup panel's header "vertex count" (D4) has no honest source — mesh geometry isn't stored in the graph def

**Status:** FIXED (bugfix-wave-2026-07-17 Lane 3) — fix shape (a): `source_vertex_count` (Int, default -1 = unknown) and `source_bbox_radius` (Float, default -1.0 = unknown) declared as DECLARED node params on `node.gltf_mesh_source` and `node.gltf_skinned_mesh_source` (never read by `run()` — import-time provenance only), seeded by `gltf_import.rs`'s `build_object_group` (shared by both `build_import_graph` and `merge_import_into_graph`) from `GltfMaterialInfo::vertex_count` and the import's whole-scene bbox radius. `SceneVm::from_def` sums every resolved object's terminal mesh-source node's count (plus a closed-form `procedural_vertex_count` table covering `node.cube_mesh`/`node.grid_mesh`) into a new `SceneHeaderVm::vertex_count` / `vertex_count_exact` pair — an unresolved contribution (unmapped procedural generator, malformed def, unparseable modifier chain) sets `vertex_count_exact = false` so the panel can render "≥ N" rather than a fabricated exact number. Piping counts through `ContentState` (option (b)) was rejected per the design's D3 purity contract. UI wiring (`manifold-ui`/`state_sync.rs` consuming the new header fields) is a follow-up, not done this session — out of this lane's enumerated fix list.

**Symptom:** D4's header row commits to "object/light/vertex counts + shadow-caster count (static, from the Vm — the honest cheap cost line)". Object/light/shadow-caster counts are real (`SceneHeaderVm`, already shipped P1). A vertex count is not: mesh-source nodes are either procedural generators (`node.cube_mesh`, `node.generate_grid_mesh`, …, whose vertex counts are Rust-side constants/formulas, never stored as a graph-def param) or `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source` (which read a `.glb` from disk at RUN time — the importer's own `GltfImportSummary.vertex_count` is import-time metadata, never stashed back onto the mesh-source node's `params`). `SceneVm::from_def` is a pure function of `EffectGraphDef` alone (D3's own architectural constraint, enforced by §4's negative gate) with no GPU/mesh access — so no code path can produce a real vertex count without either violating that purity or loading assets.

**Root cause:** design assumed a "cheap proxy" count was available; on inspection, no per-node vertex metadata exists anywhere in the def, and fabricating one (e.g. counting resolved mesh-source nodes, which would just re-state `object_count`) would be a dishonestly-labeled number, not a proxy.

**Fix shape:** two real options, both bigger than a panel-only change: (a) stash `vertex_count` as a node param on mesh-source nodes at import/generation time (touches `gltf_import.rs` + every procedural mesh-generator primitive — a real, scoped feature); (b) compute it from loaded mesh data at content-thread render time and pipe it back as part of `ContentState` (crosses the UI/content boundary `SceneVm` was built to avoid). Until one ships, the header row omits "vertices" — P2 shipped objects/lights/shadow-casters only (already present from P1), which is what's genuinely computable today.

### BUG-236 (scene-setup-flows-assert-stale-outliner-text) — two flow scripts assert a literal "Outliner" text label that no longer exists anywhere in `scene_setup_panel.rs` — found 2026-07-17 during SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1b

**Status:** FIXED 2026-07-18 (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1d closing session). Both flows' step-2 selector changed from `Query(text="Outliner")` to `Query(text="Objects")` (the outliner's own top-level section header is `"Scene"`, per `build_outliner`'s first `add_label`; `"Objects"` is the nearer, unambiguous sub-section label the same section always renders, and the one every other converted family's flow already keys its own "outliner rendered" assertion off). `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-add-object.json` now runs green 8/8. `scene-setup-scrub-fine.json` also gets the text fix and clears that same step 2, but fails LATER (step 14) for an unrelated, newly-discovered reason — logged separately as BUG-240; that flow is not one of C-P1d's own required re-verify targets, so it's not driven to full green this session.

**Symptom:** `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-add-object.json` and `scene-setup-scrub-fine.json` both fail at step 2, `Assert { selector: Query(text="Outliner"), check: Exists }` — "expected a match for query{text=\"Outliner\"}, found none". `rg '"Outliner"' crates/manifold-ui/src/panels/scene_setup_panel.rs` → 0 hits: no section label or heading in the current panel renders that literal text.

**Root cause:** the outliner section's header text was renamed to `"Scene"` (`build_outliner`'s own top label) at some point pre-dating C-P1a without sweeping these two flow scripts' selectors — never precisely dated; not investigated further, the fix (swap to a text that DOES exist) doesn't require knowing exactly which historical commit did the rename.

**Fix shape:** `rg '"Objects"' crates/manifold-ui/src/panels/scene_setup_panel.rs` confirms the section renders that literal text unconditionally whenever the outliner builds — swapped both flows' step-2 selector to it.

### BUG-193 (scene-setup-no-remove-object-command) — Scene Setup panel's Objects section has no "Remove" affordance — no composite command exists to dispatch

**Status:** FIXED 2026-07-17 (bugfix wave, Lane 2, `lane/bugfix-removal-commands`) against the CURRENT parallel-wire object model. **V2 P3 NOTE:** `docs/SCENE_OBJECT_AND_PANEL_V2_DESIGN.md`'s P3 phase was independently scoped the same day to build this same removal affordance against its new `node.scene_object`/`Object`-wire model — that design was written without visibility into this concurrent lane. Advised resolution (Fable, 2026-07-17, consulted at landing): land Lane 2's fix as-is; it's real, tested, and closes a gap Peter has hit directly. P3's deliverable becomes *porting* `RemoveSceneObjectCommand`/`RemoveSceneLightCommand` to the `scene_object`/Object-wire shape (reusing their inverse-pair execute+undo tests), not authoring removal from scratch. New `RemoveSceneObjectCommand`/`RemoveSceneLightCommand` (`crates/manifold-editing/src/commands/graph.rs`), shaped exactly as this entry's fix shape: whole-level snapshot/restore undo (mirrors `AddSceneObjectCommand`/`AddSceneLightCommand`), deletes the object's group node + its 3 root wires (or the light's bare node + its single wire), decrements `objects`/`lights`, and renumbers every wire whose object/light-port index exceeded the removed one down by one (`shift_indexed_ports_down` helper). Wired to a per-row "✕" in both the Objects and Lights sections of `scene_setup_panel.rs` (new `PanelAction::SceneSetupRemoveObject`/`SceneSetupRemoveLight`, dispatched through `ui_bridge::project::dispatch_project` exactly like the existing Add actions). Gates: `remove_scene_object_middle_deletes_group_and_renumbers_survivors` (proves the renumbering claim on a 3-object fixture, removing the middle one) + `remove_scene_light_only_light_removes_node_and_zeroes_count`, both execute+undo byte-equal; panel-level click tests proving each "✕" carries its row's own index; `dispatch_project`-level tests proving the panel action reaches the real command against a `SceneStarter`-based project. `cargo nextest run -p manifold-editing -p manifold-ui -p manifold-app` 1262/1262 passed; `cargo clippy -p manifold-editing -p manifold-ui -p manifold-app -- -D warnings` clean.

**Symptom:** SCENE_SETUP_PANEL_DESIGN.md's D4 table and P2 phase brief both call for a per-object "remove (delete-group + decrement composite, the existing path)" — the brief's own VERIFY marker (`rg -n "delete.*group|RemoveScene" crates/manifold-editing/src/commands/graph.rs`) turns up nothing: no composite command decrements `render_scene`'s `objects` count, renumbers the remaining `mesh_k`/`transform_k`/`material_k` wires above the removed index, and deletes the group subtree as one undo unit. `RemoveGraphNodeCommand` (generic node+wire removal) exists but does NOT touch the `objects` param or renumber subsequent object ports — using it alone would leave a gap (e.g. removing object 1 of 3 leaves `objects=3` with `mesh_1` unwired, showing as a phantom Custom row, while `mesh_2` stays wired at the wrong index). `RemoveSceneCommand` (`manifold-editing/src/commands/session_commands.rs`) is a different concept entirely (SESSION_MODE clip-launch scenes, not 3D scenes).

**Root cause:** design-time assumption gap — SCENE_SETUP_PANEL_DESIGN.md assumed a removal composite already shipped (mirroring `AddSceneObjectCommand`/`AddSceneLightCommand`), but no prior phase (SCENE_BUILD P5 built add-only) ever built the remove side.

**Fix shape:** a new `RemoveSceneObjectCommand` (and `RemoveSceneLightCommand`), each one undo unit, shaped like `AddSceneObjectCommand`: delete the object's group node + its 3 root-level wires (`mesh_k`/`transform_k`/`material_k` → `render_scene`), decrement `objects`, and renumber every wire whose port index was above the removed one (`mesh_{k+1}` → `mesh_k`, etc. — lights are simpler, no renumbering needed since `node.light` wires directly with no per-object port shift beyond the single `light_k` slot). This is a genuine new composite, not one of SCENE_SETUP_PANEL_DESIGN's five named additions (env/fog add, D5 import, D6 ×3) — Peter's call needed on whether it's in-scope for this design or its own small follow-up. P2 shipped without a Remove control rather than invent this unreviewed.

### BUG-184 (automation-clear-lane-not-wired-to-ui) — no UI affordance clears a lane's automation once it's set

**Status:** FIXED 2026-07-17 (bugfix wave, Lane 2, `lane/bugfix-removal-commands`) — right-click on an automation lane's strip/segment/dot now opens a two-item context menu ("Clear Automation" → `ClearLaneCommand`, "Remove Lane" → `RemoveLaneCommand`), using the same `DropdownItem`/`open_context` infrastructure `ClipRightClicked`/`TrackRightClicked` already use. New `TimelineEditingHost::on_automation_lane_right_click` method, called from `interaction_overlay.rs`'s `on_pointer_click` right-click branch (resolved BEFORE the left-click automation handler so a right-click never mutates a point); `AppEditingHost` pushes `PanelAction::AutomationLaneRightClicked`, `ui_root.rs` builds the two-item menu, and `ui_bridge::project::dispatch_project` executes the chosen command (removed-lane index resolved by `param_id` lookup against `automation_lanes`, same pattern `remove_automation_point` already uses). Gates: existing `ClearLaneCommand`/`RemoveLaneCommand` unit tests in `manifold-editing` (unchanged, already covered execute+undo); new `interaction_overlay` test `right_click_on_automation_dot_opens_lane_context_menu_not_point_logic` proves the right-click routing decision itself (a right-click on a lane dot calls `on_automation_lane_right_click` exactly once and never touches `ui_state.selected_automation_point`, i.e. never falls into the left-click point-select/delete path); `cargo nextest run -p manifold-editing -p manifold-ui -p manifold-app` 1263/1263 passed; `cargo clippy -p manifold-editing -p manifold-ui -p manifold-app -- -D warnings` clean.

**Symptom:** `ClearLaneCommand` and `RemoveLaneCommand` exist in `manifold-editing` (`crates/manifold-editing/src/commands/automation.rs:306`, `:197`) but neither is referenced anywhere in `manifold-ui` or `manifold-app` — confirmed via `rg -n "ClearLaneCommand|RemoveLaneCommand" crates/manifold-ui crates/manifold-app` returning zero hits. The only shipped point-level edits are the AUTOMATION_LANES_DESIGN.md §7 vocabulary: double-click a dot deletes it, marquee-select + Delete removes a selection. There's no one-click "clear this lane" or "remove this lane" button/menu item/keybinding.

**Fix shape:** wire `ClearLaneCommand` (clear all points, keep the lane) and `RemoveLaneCommand` (delete the lane entirely) to a UI trigger — most likely a right-click/context-menu item on the lane header, per the lane-header re-enable click precedent already in the design doc (§7 "State affordances"). Design doc `docs/AUTOMATION_LANES_DESIGN.md` is otherwise implemented per its status board entry; this is a gap in that landing, not a new design.

### BUG-199 (audio-and-scene-setup-docks-have-no-working-scroll-input) — neither utility dock's `ScrollContainer` ever receives a real scroll gesture; content past the window height is unreachable — found 2026-07-17 landing SCENE_SETUP_PANEL_DESIGN.md P5 (wave close), confirmed by direct testing

**Status:** FIXED 2026-07-17, BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 1 (`lane/bugfix-dock-scroll`). `primary_mouse_wheel` (`window_input.rs`) now routes wheel events over either dock — gated on `layout.scene_setup().contains(pos)` / `layout.audio_setup().contains(pos)`, which is already the open-check since a closed dock's rect is `Rect::ZERO` — through the SAME generic `UIEvent::Scroll` → `pending_events` pipeline the open-dropdown branch already used (no per-panel in-place-offset special case). Both `ScenePanel::handle_event` and `AudioSetupPanel::handle_event` gained a `UIEvent::Scroll` match arm calling their existing `handle_scroll`, mirroring `dropdown.rs`'s "always consume while open" pattern; no position re-check needed inside the panel since `window_input` already gated on the dock rect. The real app and the headless `Gesture::Scroll` harness now share one path. Verified: `scripts/ui-flows/scene-setup-add-fog-drag.json` green again (15/15, one `Scroll` step added before the "+ Add Fog" click); new `scripts/ui-flows/audio-dock-scroll.json` (5/5) proves the audio dock scrolls against a purpose-built overflow fixture (`audio_sends_scene()` gained 24 extra Send-A consumer rows, `fixtures.rs`); `audio-setup-hygiene.json` re-run green (11/11, unaffected). Clears VD-029.

**Symptom:** `scripts/ui-flows/scene-setup-add-fog-drag.json` (P1's own L3 flow, previously green at P1's landing) now fails: clicking "+ Add Fog" by name resolves to `acted at (736.0, 1244.0)` but no `SceneSetupAddFog` action is dispatched, and the resulting "Density" row never appears. The window's rendered surface is only 1216px tall; by P5's landing the azalea fixture's Objects section (2 objects × transform/material/modifier rows, each object now carrying its own "Modifiers"/"Add modifier:" section per P5) pushes Lights/Environment/Fog/Camera to y≈960–1250+, i.e. genuinely past the physical window height, not just past a notional sub-viewport. The `ScrollContainer` exists (`scroll_container.rs`, wired into `scene_setup_panel.rs` at build time) but nothing ever drove it in the real input path.

**Root cause:** `crates/manifold-app/src/window_input.rs`'s `primary_mouse_wheel` explicitly branched on `inspector_rect.contains(pos)` and `tracks_rect.contains(pos)` (plus the dropdown-open case) — there was no branch for the scene-setup or audio-setup dock rects at all, so a real mouse wheel over either dock did nothing. Confirmed empirically, not just by reading the dispatch table: a `Gesture::Scroll` at the dock's body point, at deltas from -400 to -5000, left the "+ Add Fog" button's resolved screen position completely unchanged (1244 before and after); a `Drag` gesture on the dock's own scrollbar-thumb widget (`scroll_container.rs`'s track/thumb pair, identified via a forced-failure `--dump`) had the same null effect. The generic `UIEvent::Scroll` consumer list (`rg -n "UIEvent::Scroll" crates/manifold-ui/src/panels/`) covered only `dropdown.rs` and `browser_popup.rs` — neither utility dock was in it.

**Fix shape:** see Status above.

### BUG-166 (gltf-crate-vetoes-extensionsrequired-we-already-support) — `gltf::import` hard-fails any asset that lists a required extension the crate's own validator doesn't recognize, even when MANIFOLD's importer downstream already handles that extension — blocks otherwise-supported assets before our code ever runs
**Status:** FIXED 2026-07-16 (parse layer, GLB_XFAIL_BURNDOWN_DESIGN P2; the residual unlit-material-fidelity gap is BUG-174's, not this bug's) — `import_glb()` (`gltf_load.rs`) replaces `gltf::import()` at all 3 production call sites + the azalea test harness; parses via `Gltf::from_slice_without_validation` + re-runs the crate's own structural validation directly (`json::Root: Validate`) with only the `extensionsRequired`-`Unsupported` errors filtered out, then checks `extensionsRequired` against MANIFOLD's own `MANIFOLD_SUPPORTED_EXTENSIONS` list. `UnlitTest.glb` now imports and renders (converges non-black, frame 4, fraction 0.1788); `ClearCoatCarPaint.glb` now imports (parse-layer only — its render correctness was already shipped by GLB_CONFORMANCE_DESIGN, unaffected). Caveat found during this fix, logged separately as BUG-174: `UnlitTest.glb`'s render is geometrically correct but NOT shaded unlit — `gltf_import.rs` never reads `KHR_materials_unlit` and always builds a lit (Phong-ish) material; the crate-level import veto (BUG-166's actual defect) is fixed, but full unlit material fidelity was never in this doc's D1 scope and remains open.

**Symptom:** Khronos `ClearCoatCarPaint.glb` (`extensionsRequired: ["KHR_texture_transform", "KHR_materials_clearcoat"]`) and `UnlitTest.glb` (`extensionsRequired: ["KHR_materials_unlit"]`) both fail at `gltf::import()` itself with `invalid glTF: extensionsRequired[N] = "...": Unsupported extension` — never reaching `assemble_import_graph`'s own logic. Both extensions are ones MANIFOLD already has real support for elsewhere: clearcoat factors render correctly on `ClearCoatTest.glb` (G-P5, `extensionsUsed` not `extensionsRequired` there — the crate only vetoes *required* extensions it doesn't know), and `MATERIAL_SYSTEM_DESIGN.md` names `unlit` as a supported Material shading mode. The gate is upstream of MANIFOLD's code — the `gltf` crate (v1.4.1, pinned) validates `extensionsRequired` against its own internal known-extension list at `gltf::import()` time and refuses to proceed if an entry isn't on it, independent of what MANIFOLD does with the parsed data afterward.

**Root cause:** not investigated past confirming the crate-level veto (empirically: identical extension listed under `extensionsUsed` imports fine; the same extension listed under `extensionsRequired` hard-fails). Whether the pinned `gltf` crate version exposes a validation-strategy knob (e.g. a lower-level `Import`/`Root::from_slice` entry point that skips extension-requirement validation) is unverified.

**Fix shape:** likely swap `gltf::import(path)`'s convenience call for the crate's lower-level slice-based import (parse JSON + buffers/images ourselves, as `gltf_load.rs` already partially does) with extension validation disabled or pre-filtered — strip the specific extensions MANIFOLD supports out of `extensionsRequired` before validation, or vendor a permissive validator. Affects any spec-legal glb that correctly marks a load-bearing extension `extensionsRequired` (the compliant authoring choice) rather than `extensionsUsed`.

### BUG-200 (khr-animation-pointer-channels-fail-to-deserialize) — duplicate of BUG-170, id burned
**Status:** SUPERSEDED — same crate-level gap as BUG-170 (`gltf-json` 1.4.1's `Target::node` has no `#[serde(default)]`; `KHR_animation_pointer` channels legally omit it). Filed independently as BUG-187 during GLTF_ANIMATION A1, renumbered to 200 at the 2026-07-17 dedup, then recognized as BUG-170's five-asset class. This entry's root cause and fix options were folded into BUG-170, which is canonical (the conformance manifest's five `xfail:BUG-170` assets point there). Id stays burned — do not reuse 200.

### BUG-189 (import-graph-10ms-resolution-independent-gpu-floor) — glb import graph burns ~10 ms of GPU time per frame independent of resolution; 4K lands at 13.5 ms median / 22.7 ms p95, over the 60 fps budget at p95 — found 2026-07-16 measuring 4K60 feasibility for the AMG GT3 on M4 Max 36GB
**Status:** FIXED (residual documented) 2026-07-17 — RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0–P5 SHIPPED. The shadow+IBL re-render waste this bug named is closed (P2 shadow caching + P3/P3b IBL gating); the resolution-independent "floor" is gone as a dirty-scene phenomenon — what remains is `render_scene`'s main pass (draw calls + shading), which is real per-frame work, not waste, and cannot be dirty-gated on stage (the camera moves every frame). See the P5 final re-measure below for the closing numbers.

**Symptom:** `render-import` on `mercedes-amg_gt3__www.vecarz.com.glb` (302k tris, 166 primitives, 78 materials), true GPU execution time per frame via `commit_and_wait_completed_timed` (`GPUEndTime − GPUStartTime`), steady-state medians after decode convergence, back-to-back frames (no inter-frame sleep): 9.8 ms @1920×1080, ~9.8 ms @2560×1440, 13.5 ms @3840×2160 (p95 22.7 ms). Only ~3.7 ms scales with pixels across a 4× pixel-count jump; ~10 ms is a fixed per-frame floor. CPU encode is ~4.5 ms/frame on top (overlappable in the pipelined engine, serial in the harness).

**Root cause:** unknown. Suspects, in order: (a) shadow-map pass re-rendering the full 166-draw-call scene every frame at fixed shadow resolution; (b) per-pass/encoder overhead across the import graph's many sequential passes (dome fill, IBL, mips, tonemap) with GPU dead time between them; (c) something regenerating per-frame that should be cached (mips, environment). The 4K p95 spikes (22–37 ms) are a second unknown riding on top.

**Attribution (PERF_BUDGET_GATE_DESIGN.md P2b `--profile`, `cargo xtask perf-soak tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb --size 3840x2160 --profile`, 2026-07-16):** unprofiled confirms the floor — GPU min=13.293 p50=13.541 p95=13.815 max=14.220 ms over 300 frames (matches the earlier measurement). Profiled mode (forced-serial, per D6's honesty contract — its totals run higher than the gate numbers above and are not comparable to them) surfaced the worst frame's node breakdown:

| Node (tag) | type_id | GPU ms | Share of frame |
|---|---|---|---|
| `import:s294` | `node.render_scene` | 11.225 | 48.8% |
| `import:s295` | `node.ssao_gtao` | 1.927 | 8.4% |
| `import:s296` | `node.bilateral_blur` | 0.631 | 2.7% |
| `import:s297` | `node.bilateral_blur` | 0.568 | 2.5% |
| `import:s113` | `node.gltf_texture_source` | 0.522 | 2.3% |

`render_scene` alone is ~half the frame's GPU time and the only candidate anywhere near the ~10ms floor's magnitude — this points at suspect (a) (the 166-draw-call scene, most plausibly its shadow-map re-render) over (b)/(c), which would show up as many small passes each taking a slice, not one dominant node. `ssao_gtao` (8.4%) and the two `bilateral_blur` passes (2.7%/2.5%, its blur pair) are the next tier — real but an order of magnitude smaller. No untagged/uninstrumented GPU time in this frame (0.000ms). Capacity check: 198/2048 sampler spans used on the busiest frame, no overflow. Separately, `--profile`'s worst-frame selection surfaced an anomalous early frame (frame 6, just past the 6-frame warmup convergence point) whose GPU total and per-node shares don't reconcile cleanly (per-node shares summing past 100% of a reported frame time lower than their sum) — looks like a first-use pipeline-compile artifact near the warmup boundary, not a steady-state signal; noted here rather than folded into this bug's numbers, worth a look if it recurs on other assets.

**Fix shape (superseded below):** the attribution above narrowed this to `render_scene`'s shadow pass (suspect (a)) — that guess is now OVERTURNED, see the P0 refinement immediately below.

**Attribution refinement (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0/D4b, 2026-07-17):** the `--profile` tool previously merged `render_scene`'s internal shadow/IBL/main-pass dispatches into one row (`node.render_scene`'s tag matched a live executor step, so distinctly-*labeled* sub-passes silently summed together — a latent join bug, not the design's originally-suspected `None`/unmatched-arm collapse). P0 fixed this: each node row now carries a nested `passes` array (`{label, gpu_ms, share_of_frame}` per distinct GPU pass label under that node's tag), verified against real profiled runs on this fixture (`crates/manifold-app/src/perf_soak_import.rs`). Unprofiled anchors (300-frame steady-state, back-to-back, no readback/sleep): @3840×2160 GPU p50=13.554ms p95=13.869ms; @1920×1080 GPU p50=9.830ms p95=11.768ms — both consistent with the original floor. Corrected per-pass composition of `render_scene`'s own GPU time (its own total as denominator — frame-level shares are inflated past 100% under D6's stage-boundary profiling overhead and are not the right denominator for internal composition), two consecutive profiled runs per resolution, rank order stable both times:

| Pass label | @3840×2160 (run1 / run2) | @1920×1080 (run1 / run2) |
|---|---|---|
| main pass (`node.render_scene`) | 12.184ms (54.6%) / 12.150ms (54.7%) | 9.589ms (51.7%) / 9.679ms (53.0%) |
| `… ibl prefilter mip` + `… ibl irradiance` | 8.892ms (40.5%) / 9.272ms (41.7%) | 8.214ms (44.3%) / 7.771ms (42.6%) |
| `… shadow` | 0.905ms (4.1%) / 0.805ms (3.6%) | 0.756ms (4.1%) / 0.799ms (4.4%) |

**This overturns the earlier attribution's conclusion.** Shadow is a small share (~4%) at both resolutions, not the dominant cost as previously guessed from the collapsed single-row number — **IBL convolution (prefilter + irradiance) is the dominant internal cost, ~40–44% of `render_scene`'s time, roughly 10× shadow's share.** Main pass (draw calls + shading) is the largest single component (~52–57%). RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md's own P2 (shadow caching)/P3 (IBL gating) phase order should be read against this: P3's IBL gating has the larger ceiling of the two dirty-signal fixes, not P2's shadow caching, contrary to this doc's D1 framing ("shadow... is the headline win") which was written before this corrected breakdown existed.

**Fix shape:** unchanged in mechanism (dirty-signal caching, per RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P2/P3) but re-prioritize: IBL gating (P3) closes more of the floor than shadow caching (P2) alone. Measurement harness: `cargo xtask perf-soak <glb> --size WxH [--profile]` (PERF_BUDGET_GATE_DESIGN.md P1+P2b, landed 2026-07-16, `7afcb059`; D8 amendment for the per-label `passes` breakdown, 2026-07-17) — no longer a throwaway patch, it's the standing tool.

**P3 landed 2026-07-17 but did not close this bug — see BUG-197.** RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3 implemented exactly its brief (bake_equirect_envmap + hdri_source producer gating, render_scene consumption gating on the envmap slot's write generation) and every one of its own correctness gates (I2 animated-envmap parity, I4 static bit-identity, per-producer parity tests) passes on real GPU hardware. But the AMG's actual import graph wires `node.bake_environment → node.switch_texture (env_mode select) → node.render_scene`, and `switch_texture` copies its selected branch into its own output every frame without ever declaring `mark_outputs_unchanged` (BUG-197) — so `render_scene`'s envmap generation never stabilizes on a real import and the gate never hits. Re-measured after P3: AMG @3840×2160 unprofiled p50 13.554ms → 13.333ms, a ~0.22ms/1.6% drop — not the multi-ms/~41% drop this bug's floor implies is available. The floor is still ~13.3ms; only shadow's ~4% (P2) plus a fraction of a percent (P3, real-import path) has actually closed. Unblocked once BUG-197 lands.

**P3b landed 2026-07-17 — this bug's headline floor is now closed for the AMG fixture (Softbox AND HDRI env modes).** BUG-197's fix (`mux_texture.rs` evaluate-path gate + the `execution.rs` alias-path generation propagation) let `render_scene`'s IBL cache key actually stabilize on a real glTF import. Re-measured on the AMG @3840×2160, two consecutive unprofiled runs: p50 9.403ms / 9.456ms (down from the pre-P3b 13.554ms floor — a ~4.1ms/~30% drop, within tolerance of P0's ~41%-IBL-share prediction). HDRI mode measures the same (p50 9.400ms) — the `node.exposure` hop some feared would keep a residual floor does not. Residual floor (~9.4ms) is now dominated by `render_scene`'s main pass (~54% of ITS OWN GPU time per P0's breakdown), which cannot be dirty-gated on stage (the camera animates every frame) — this is R4's (indexed-mesh-rendering) target, per this doc's Deferred section; P5 will re-measure and record the exact residual main-pass share R4's revival trigger needs. `mercedes-amg_gt3` @1080p not re-measured this session (P5's job); the @4K number above is the one this bug was filed against.

**P5 final re-measure 2026-07-17 (full landed tree: P0–P4 + P3b, all phases in), full before/after for the whole workstream, AMG GT3:**

| Stage | @3840×2160 GPU p50 | @1920×1080 GPU p50 |
|---|---|---|
| P0 baseline (pre-fix) | 13.554ms (p95 13.869ms) | 9.830ms (p95 11.768ms) |
| Post P2 (shadow caching) | ~13.3–13.5ms (shadow ~4% share, inside run-to-run noise per D1b — not separately re-measured, P2's own gate note) | — |
| Post P3 (IBL gate, undischarged — BUG-197) | 13.333ms (~1.6% drop only — mux passthrough broke the cache key) | not measured |
| Post P3b (BUG-197 fix, IBL gate discharged) | 9.403ms / 9.456ms (two runs) | not measured |
| **Post P4 / P5 final (this re-measure, fresh two-run pairs)** | **9.454ms / 9.449ms** | **5.744ms / 5.716ms** |

Total drop from the P0 baseline: @4K 13.554ms → ~9.45ms (**~4.1ms / ~30%**); @1080p 9.830ms → ~5.73ms (**~4.1ms / ~42%** — a bigger proportional win at 1080p than 4K, since the fixed IBL convolution cost the fixes removed is resolution-independent while the residual main pass does scale with pixel count somewhat). A profiled sanity run at both resolutions shows the `node.render_scene` tag now carrying a **single pass row** in steady state (no separately-labeled `shadow`/`ibl prefilter`/`ibl irradiance` rows at all — both fully gated away): 9.10ms @1080p / 13.6ms-worst-frame @4K, i.e. the entire remaining cost is main pass, confirming D1b's ~54%-of-render_scene forecast in the strongest possible way — it's now ~100% of render_scene's GPU time, because everything else is gated to zero on a static scene. This residual is real per-frame work (draw calls + shading for 302k tris / 78 materials), not staleness — it is R4's (indexed-mesh-rendering, deferred) target; see the Deferred-section note below for the exact trigger number.

BrainStem (`khronos/BrainStem.glb`, 1920×1080, `--warmup-frames 30` per P4's tool addition, since this fixture never converges): GPU p50=4.003ms/p95=8.174ms (healthy, unrelated to this bug — BUG-190's territory). Not part of BUG-189's own claim; recorded here because it ran on the same fully-landed tree as this bug's closing measurement.

### BUG-197 (switch-texture-blocks-ibl-generation-gate) — `node.switch_texture` breaks the envmap write-generation chain between the bake/hdri producers and `node.render_scene`, so P3's IBL re-convolution gate never hits on a real glTF import — found 2026-07-17 measuring RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3's before/after on the AMG fixture
**Status:** FIXED — RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b, 2026-07-17. `mux_texture.rs`'s `evaluate()` now gates its copy dispatch (and the clear-fallback) on a hash of (effective selector index, selected source slot's write generation, selected source texture identity, output texture identity, executor rebuild epoch) — full match declares `mark_outputs_unchanged()`; the `execution.rs` alias-skip path additionally propagates generation-unchanged through the param-driven (`skip_passthrough`) alias fast path (fenced to `!data_skip`), which is the path the AMG's inline-selector `env_select` actually takes. Re-measured on the AMG @3840×2160: unprofiled p50 13.554ms (pre-P3b) → 9.403–9.456ms (post-P3b, two consecutive runs) — a ~4.1ms/~30% drop, within the ±30% tolerance band around P0's measured ~41% IBL share prediction (5.55ms theoretical). Profiled sanity: neither an `ibl prefilter`/`ibl irradiance` labeled row nor a `node.switch_texture` row appears anywhere in the captured frames (both fully gated/aliased away). HDRI-mode observation (temporary local probe, reverted before commit — not a shipped ablation flag): AMG in HDRI mode measures p50 9.400ms, matching Softbox — the `node.exposure` (hdri_gain) hop does NOT reintroduce the floor, so no residual note is needed on BUG-189 for that path.

**Symptom:** After RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3 landed (bake_equirect_envmap/hdri_source producer gating + render_scene consumption gating, all correct and gate-passing in isolation — see BUG-189's note above), the AMG GT3 import's measured unprofiled perf-soak delta is ~0.22ms/1.6% (@3840×2160, p50 13.554ms → 13.333ms), not the multi-ms/~41%-of-render_scene-GPU-time drop P0's profiled bisection measured as available and P3's own phase gate requires (±30% tolerance on a multi-ms delta).

**Root cause (confirmed by reading `mux_texture.rs`, the file `node.switch_texture` aliases to):** every glTF import's D6 env-mode wiring (`gltf_import.rs` ~671–715, ~2029–2032) is `node.bake_environment → node.switch_texture (env_select) → node.render_scene`'s `envmap` port — never a direct wire. `MuxTexture::run()` (`mux_texture.rs` ~355–370) unconditionally dispatches a copy-compute pass every frame it runs (selecting/copying whichever branch the selector picks into its own output texture) and never calls `ctx.mark_outputs_unchanged()` anywhere in the file. Per D5's conservative-default rule, a node that never declares unchanged always bumps its output slot's write generation — so `render_scene`'s `ctx.inputs.slot_generation("envmap")` (which resolves to `switch_texture`'s output slot, not `bake_equirect_envmap`'s) changes every single frame regardless of whether the upstream bake actually re-ran. P3's `ibl_cache_key` therefore misses every frame on any glTF import, even though `bake_equirect_envmap` itself is correctly skipping its own dispatch (confirmed: its gpu-proofs parity tests pass, and the real per-node saving is real — just tiny, since baking one 512×256 texture is cheap compared to convolving it).

**Fix shape:** give `node.switch_texture` the same `last_key`-style gate P1/P3 gave the other sources: skip the copy dispatch and call `mark_outputs_unchanged()` when (selected branch's slot generation, selector value, output texture identity) are all unchanged since the last frame it actually copied. This is the same pattern, a different file — no new mechanism, just the 4th producer in the generation-signal chain. Scope note: `switch_texture`/`mux_texture` is a shared primitive (also used by Plasma's 8-pattern cycling, ConcentricTunnel's 6-shape select, Infrared's 10-ramp palette per its own module doc) — any gate must preserve its existing "prune unselected branches" cost-model doc comment and pass its own existing tests (`selected_input_branch` unit tests) unchanged; this needs its own §2.5-style audit before touching it (CLAUDE.md decomposition-audit rule), not a copy-paste of P1's pattern without reading the file end to end first.

### BUG-158 (mapped-param-edits-snap-back-no-two-way-binding) — a param mapped to a card slider or driven by another port can't be adjusted in the graph editor; the edit snaps back — MED-HIGH (authoring surface), reported by Peter 2026-07-14
**Status:** FIXED — P2 SHIPPED 2026-07-15 (Fable orchestrating two Sonnet agents, `bug/158-two-way-p2`): the live-value tap (`live_node_params`) now reports wire-resolved values via a new executor `live_scalar_inputs` capture instead of the stale param map, and wire-driven rows render as honest non-interactive readouts (whole-slider dim, tinted jack halo, hover names the driving source, click highlights the feeding wire, wired+bound rows keep both attributions per D6). Scrub prevention itself predated P2 (already in-tree at `interaction.rs:873` with its unit test). L2 PNG gate passed (graph scene, Tesseract — wired row visibly dead, bound row visibly live on the same node). One unverified edge, logged in the design doc: per-node live values inside a FUSED region rest on FREEZE_COMPILER_MAP cut rule 10 (control producers survive fusion) by doc-level reasoning, not an empirical fused-graph run; soft-fails to the param map if wrong. P1 (inverse machinery + dispatch-layer reroute) SHIPPED 2026-07-14 (Sonnet, `bc2f2c0b`). `docs/PARAM_TWO_WAY_BINDING_DESIGN.md` is the authority for what's built vs. remaining. Deviation logged there and in the P1 commit: D10 said P2 must not trail a shipped P1 across a session boundary, but only one phase fit this session's budget and P1's implementation does not touch wired-param behavior at all (a wired param still runs the pre-existing, unchanged `SetGraphNodeParamCommand` path), so the harm D10 guards against — wire-driven snap-back reading as newly/more broken — does not occur. P2 still owed for the full fix (driven readout, scrub prevention, fan-out badge tooltip). Prior investigation record, kept for context: root cause REFINED (both paths located) by bug-wave lane A, escalated for design per the lane's contract. `SetGraphNodeParamCommand::execute` (`crates/manifold-editing/src/commands/graph.rs:797`) does successfully write the direct node-face edit into `def.nodes[node_id].params` — confirmed by reading it, no gating for card-bound params. The stomp happens one layer downstream: `apply_bindings` (`crates/manifold-renderer/src/node_graph/param_binding.rs:566`) runs on every chain rebuild and unconditionally re-writes `binding.apply(graph, handle, <current outer/card value>)` into the freshly-built `Graph`'s param slot — the card binding is the sole authority the render ever sees, independent of `scalar_or_param`. So the original mechanism note (`scalar_or_param`, `effect_node.rs:358`) correctly explains the WIRE-DRIVEN half of the symptom but not the CARD-SLIDER-MAPPED half — those are two distinct code paths that produce the same visible snap-back. Both point the same direction: the fix has to live at the binding/mapping layer (write-back through the inverse for card-bound params, a legible "driven" visual for wire-driven ones with no inverse) exactly as the backlog's own fix-shape already said — that's real product-UX design (how "driven" reads on the node face), not a patchable bug, so it's parked for a design pass rather than improvised here.

**Fable design consult (2026-07-14, lane's one consult, spent here)** — verified the code citations above independently, then proposed a concrete direction. **Card-slider case**: reroute, don't dual-write — a node-face drag on a bound param should NOT issue `SetGraphNodeParamCommand` at all; intercept at the editor input layer, invert through the reshape (new `invert_card_reshape` beside `apply_card_reshape` so preview/forward/inverse can't drift), and issue the existing "set outer card param" command instead. The existing forward path (`apply_bindings`) then propagates it back into the `Graph` for free — and the `LastAppliedCache` question this session flagged dissolves entirely, since nothing writes the `Graph` param behind the cache's back anymore. Node face must display the *effective* value (manifest value pushed through the forward reshape), not the (currently shadowed, possibly stale) `def.nodes[...].params`. **Wire-driven case**: prevent at the input layer, not allow-then-revert — replace the draggable slider with a live-value readout (dimmed track, fill animated by the actual driven value each frame) plus a tinted input-jack glyph; non-interactive for drag, hover shows the source, click highlights the wire. **Sequencing**: land the wire-driven "driven" treatment first or simultaneously — shipping the card-slider fix alone would make wire-driven params' remaining snap-back read as MORE broken (Peter's mental model becomes "two-way works" right up until it silently doesn't). **Risks flagged that this session's investigation missed**: a param can be BOTH wire-connected and card-bound at once — `scalar_or_param`'s wire-shadows-everything means the driven treatment must win regardless of binding, and a reverse-write there would move the card slider with zero visible render effect; fan-out bindings (one `source_id`, multiple targets, already handled elsewhere per `param_binding.rs`'s own doc) mean a reverse-write from one target moves every sibling target too — expected but should be legible in the UI (name the card param in the badge tooltip); `wraps_angle` params aren't invertible across periods (take the principal value, don't try to round-trip exactly); non-monotonic `MacroCurve` variants must route to a read-only fallback rather than a wrong inverse; existing projects carry stale shadowed `def` param edits from before this fix that should be cleared/normalized if a binding is ever removed, or they'll surface as years-old surprise values. Full consult transcript not preserved verbatim — this is the distilled direction for whoever picks this up next; re-run the consult or re-derive from the code citations above if finer implementation detail is needed.

**Symptom** — in the graph editor, dragging a param that is already mapped to an effect-card
slider (or wired from another port) appears dead: the control snaps back to the mapped/driven
value, so the only editable end of a mapping is the source. Peter's expectation: two-way
behaviour between the node param, the card slider, and other ports — turning either end moves
both, like a DAW control surface.

**Mechanism (located)** — the port-shadows-param convention:
`EffectNodeContext::scalar_or_param` (`crates/manifold-renderer/src/node_graph/effect_node.rs:358`)
resolves a wired scalar input unconditionally before falling back to the param, so while a
wire/mapping is connected the param write lands in the model but never reaches the render, and
the UI re-reads the driven value — visual snap-back. This resolution order is the deliberate
control-wire design (`control-wires-port-shadows-param` memory), so the fix is a binding-layer
behaviour change, not a bug in `scalar_or_param`.

**Fix shape** — two-way binding where an inverse exists: editing the node param on a
card-slider mapping writes back through the inverse mapping and moves the slider, keeping both
ends consistent. For signal-driven ports (LFO, audio, envelope, another node's live output)
there is no inverse — those should show a legible "driven" state on the param control instead
of silently snapping back. Implement once at the binding/mapping layer, not per widget. The
driven-state presentation (how a driven param looks on the node face) is worth one screenshot
to Peter before landing.

### BUG-202 (freeze-codegen-region-fusion-gpu-tests-fail-with-badinput-standalone) — six `freeze::codegen` GPU tests fail deterministically, even isolated — not a parallel-contention flake
**Status:** FIXED 2026-07-14 (Fable root-cause session, branch `bug/163-freeze-extkind-probe`; renumbered from BUG-163 2026-07-17 — id collision with the AMG-livery bug, which keeps 163 since every external reference means that one) — one-line fix in `generate_fused`: the D3/P4a (BUG-114, `ae9ab74c`, landed 2026-07-14 12:10 — the same day these failures were first seen) ExtKind-resolution loop classified any member input at `idx >= tex_count` as an array port, with `tex_count` counted from `node.node_inputs`; the six hand-built test regions use `node_inputs: &[]`, so every `InputSource::External` texture input resolved as an array port with no spec → `BadInput` (`codegen.rs:2326`). The loop now keys on the explicit `InputAccess::BufferIndex` tag — the same discriminator the body-rewrite loop directly below already used, and exactly what `build_region` produces (it only packs BufferIndex-tagged array entries and pushes the tag itself, `region.rs:1615-1635`), so production regions are unaffected by construction. Verified: the failing test reproduced on the unmodified tip, then all 161 `node_graph::freeze` tests green under `--features gpu-proofs` with the fix (was 6 red); `clippy -p manifold-renderer` clean; 1232 default lib tests green. The six existing tests are the regression coverage.

**Symptom** — `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::freeze::codegen::gpu_tests -- --test-threads=1` (module-scoped, serial — rules out device contention) fails 6 of 38 tests, all with the same error shape, a `.fuses(...)` call returning `BadInput`:
- `cross_resolution_external_sampled_at_uv` — `codegen.rs:3591`, `"cross-res region fuses: BadInput"`
- `fused_colorgrade_generated_matches_hand_kernel` — `codegen.rs:4229`, `"fuse ColorGrade region: BadInput"`
- `fused_fanout_emits_two_dst_bindings` — `codegen.rs:4023`, `"fan-out region fuses: BadInput"`
- `fused_gather_binds_sampler_and_passes_texture` — `codegen.rs:3944`, `"gather region fuses: BadInput"`
- `fused_prelude_carries_and_dedups_top_level_consts` — `codegen.rs:3524`, `"a region whose body declares a const fuses: BadInput"`
- `fused_texture_region_carries_and_dedups_wgsl_includes` — `codegen.rs:3672`, `"coc_from_depth + Gain region fuses: BadInput"`

**Confirmed pre-existing** — `git stash`ed this session's BUG-120 diff (only touches `triangulate_grid.wgsl`/`triangulate_grid_body.wgsl`/`triangulate_grid.rs`, nowhere near `freeze::codegen`) and re-ran `cross_resolution_external_sampled_at_uv` isolated against the unmodified tree: identical failure, same line, same message. The other 5 weren't individually re-verified against the stashed tree, but share the exact same error class (`.fuses(...)` → `BadInput`) from the same run — very likely the same root cause, not independently confirmed.

**Not BUG-144's class** — BUG-144's two prewarm-cache tests are a genuine parallel-only race (pass isolated). Re-checked this session: `render_scene::gpu_tests::prewarm_pipelines_populates_the_shared_render_cache` failed in the full unfiltered parallel run but passed clean when isolated — consistent with BUG-144, not a new finding. These 6 `freeze::codegen` failures are different: deterministic, serial, module-scoped, and don't touch a shared cache at all.

**Root cause** — not investigated. `BadInput` from a `.fuses(...)` call at 6 different call sites, all in `codegen.rs`, suggests something upstream of region-fusion is now producing a shape the fuser rejects across the board (a shared helper's output changed, or a validation the fuser performs got stricter) rather than 6 independent bugs — but that's a hypothesis, not a finding.

**Fix shape** — needs its own investigation session: start at `codegen.rs:3591` (`cross_resolution_external_sampled_at_uv`, the smallest/most specific-sounding test) and work out what input shape `.fuses()` is rejecting; check whether the same root cause explains all 6 before assuming it does. Given every hit is `--features gpu-proofs`-gated and none of the 6 tests were in this session's narrow `-p manifold-renderer node_graph::primitives::triangulate_grid` gate, this is invisible to the routine focused-test workflow CLAUDE.md recommends — worth a full `--features gpu-proofs --lib` sweep as part of any FUSION_SOTA / freeze-compiler landing to catch it early next time.

### BUG-154 (removing-group-with-slider-bound-nodes-leaves-stale-effect-card) — deleting a node group that has nodes assigned to card sliders doesn't update the effect card: no warning shown, and the stale slider isn't removed — MED, reported by Peter 2026-07-14
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Root cause confirmed and closed at the class level, per the lane's contract (BUG-154 named explicitly as "stale bindings on ANY node-removal path, not just group delete"): a group deletion IS a single-node removal (the group container, via `RemoveGraphNodeCommand`) — but `remove_exposures_for_node` only ever matched the ONE node id it was called with, and it was called with the group container's own id. A card slider bound to a node NESTED inside the removed group's subgraph (the common case — a group wraps existing nodes, one of which is exposed) was never matched, so its binding/param-spec/value-slot/automation survived the deletion as dangling state. Fixed by walking the removed subtree: new `subtree_node_ids()` helper (`graph.rs`, recurses into nested `GroupDef.nodes`, handling groups-of-groups) collects every node id in the removed node's tree (itself, for a plain node removal — the existing single-node-delete behavior is now just the one-element case of the same call), and `RemoveGraphNodeCommand::execute` prunes exposures for each. `undo` needed no change — it already restores however many `RemovedExposure`s were captured. Regression test `remove_group_node_prunes_card_slider_bound_to_a_nested_node` builds a group wrapping a slider-bound node and asserts the binding/param/value are pruned on delete and restored on undo; verified to fail without the fix (reverted the loop back to the single-id call, confirmed red, restored). Gated: `cargo clippy -p manifold-editing -- -D warnings` clean; `cargo nextest run -p manifold-editing` 217/217 passed.

**Symptom** — when a group is deleted from a node graph and that group contained a node that had been assigned to an effect card slider, the effect card doesn't react correctly: it should either warn that the binding is now dangling or remove the slider, but currently does neither — the stale slider is left on the card with no indication its underlying node is gone.

**Root cause** — unknown; not investigated. Likely the group-deletion path doesn't walk card/slider bindings the way node-level deletion does — suggests group deletion isn't routed through the same binding-cleanup logic as deleting the same nodes individually (compare against however single-node deletion updates card sliders on removal).

**Fix shape** — trace group deletion (`EditingService`/command path for group removal) and compare it against the single-node deletion path's card-binding cleanup; route group deletion through the same cleanup so it either surfaces the warning or removes the slider, matching single-node-delete behavior.

### BUG-120 (grid-terrain-winding-disagrees-with-vertex-normals) — terrain triangle winding contradicts vertex normals — LOW, consumer-side fixed
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — confirmed at the emitter and fixed at the class level, per the fix shape's first option. `node.triangulate_grid` (both `triangulate_grid.wgsl` and its fusable-body twin `triangulate_grid_body.wgsl`) wound every triangle CW-from-above while its own finite-difference normal (`compute_normal`/`tg_compute_normal`) declared +Y for a flat grid — swapping corners 1↔2 and 4↔5 in the per-vertex corner→(col,row) mapping flips the winding to agree, with the triangle coverage and normal computation both unchanged. New GPU test `flat_grid_triangle_winding_agrees_with_vertex_normal` builds a flat XZ grid, dispatches the hand kernel, and asserts every emitted triangle's winding-derived face normal (`cross(v1-v0, v2-v0)`) agrees with its declared vertex normal — verified to fail (`face normal [0,-1,0] disagrees with [0,1,0]`) on the pre-fix layout, confirmed to pass after. The existing `generated_triangulate_matches_hand_kernel` parity test still passes (both shader files changed identically, so generated-vs-hand agreement is untouched). Gated: `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::triangulate_grid` 5/5 passed; `cargo clippy -p manifold-renderer -- -D warnings` clean; full default `cargo nextest run -p manifold-renderer` 1265/1265 passed.

**Symptom** — scatter_on_mesh align_to_normal placed ~98% of instances upside-down (up mapped to -Y), rendering them under the terrain: BlossomField showed ~25 of 420 flowers; Garden showed 44 of 140. GPU test `align_on_flat_ground_keeps_instances_upright_and_finite` reproduced it deterministically on a hand-built flat quad.

**Root cause (consumer, FIXED)** — scatter's align path trusted the winding-derived face normal; the terrain's triangles wind -Y-facing while vertex normals declare +Y. Fixed in scatter_on_mesh.wgsl by flipping the face normal into the hemisphere of the triangle's vertex normals (mesh-declared outward), with flat + sloped GPU tests.

**Root cause (emitter, UNVERIFIED)** — whether grid_mesh/make_triangles genuinely emit -Y winding (vs the test data coincidence) has not been checked at the emitter. If real, every future winding consumer hits it.

**Fix shape** — read make_triangles' emission order against grid_mesh row-major layout; if winding is inverted, either fix the emission order (check draw paths that might depend on current order) or write the engine-wide rule "vertex normals are authoritative, winding is not" into DEVELOPMENT_REFERENCE.md.

### BUG-117 (render-generator-preset-silently-under-renders-async-loaded-presets) — the look-dev CLI has no wait-for-convergence signal, so a slow-decoding preset can write an incomplete PNG with no warning — LOW (tooling gap, not a runtime bug)
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Ported BUG-100's fix pattern exactly as the fix shape specified: `render_generator_preset.rs` now keeps rendering past the requested `--frames` warm-up, comparing raw (pre-tonemap) readbacks each additional frame, until 3 consecutive frames are byte-identical or a new `--max-frames` cap (default 300) is hit — printing a warning to stderr if the cap is hit before convergence, same as BUG-100's harness did. New `readback_raw_halves` helper does the comparison readback cheaply (no tonemap/composite math) separate from the final PNG-writing readback. Verified by actually running the tool (`cargo run -p manifold-renderer --bin render-generator-preset -- TrivialPassthrough --size 320x180 --frames 5 --out ...`): converged and printed `converged on frame 7 (stable for 3 frames)`, wrote a valid 320×180 RGBA PNG — not just a compile check. Gated: `cargo clippy -p manifold-renderer --bin render-generator-preset -- -D warnings` clean.

**Symptom** — `cargo run -p manifold-renderer --bin render-generator-preset -- ApricotWeather --frames 30` (and even `--frames 90`) sometimes wrote a PNG showing only the ground plane, or only some of the tree's 3 material-filtered objects, with zero indication anything was still loading. Re-running the identical command with `--frames 3000` (real wall-clock ~27s) reliably showed the complete scene. The result depended on wall-clock timing of a background thread, not on any `--frames`/`--param` value the caller controls in a predictable way.

**Root cause** — `node.gltf_mesh_source` (and `node.gltf_texture_source`, `node.image_folder`, `node.depth_estimate_midas`, etc.) parse/decode on a background thread and, per their own documented contract, leave the pre-bound output buffer untouched ("nothing parsed yet ... leave the pre-bound buffer's existing contents") until the background job completes — correct runtime behavior, not a bug. But `render-generator-preset` (`crates/manifold-renderer/src/bin/render_generator_preset.rs`) just runs `--frames N` simulated frames and dumps whatever is in the target texture at the end; each *fresh process* re-triggers the parse from scratch (no cross-process cache) and the tool has no signal for "still loading" vs "fully converged," so the wall-clock race is invisible to the caller. This is the same underlying class BUG-100 hit (FIXED) for `imported_azalea_renders_faithfully_to_png` — that fix added a 3-consecutive-identical-frames convergence check to ONE test harness; `render-generator-preset` (a general dev tool explicitly documented as "the iteration loop for shader work inside a preset: edit JSON → render → Read the PNG") never got the equivalent fix and has the identical race, worse here because a multi-material glTF preset re-parses the same large file once per `node.gltf_mesh_source` instance (4× the wall-clock cost for a 4-material scan).

**Fix shape** — port BUG-100's convergence pattern into `render_generator_preset.rs`: after the requested `--frames`, keep rendering and comparing consecutive readbacks until N (e.g. 3) are byte-identical (or a `--max-frames` safety cap is hit), and print a warning if the cap is hit before convergence. Cheaper alternative: expose a "pending background loads" count off `PresetRuntime`/`EffectNodeContext` that the bin can poll and block on before the final readback. Either removes the silent-partial-render trap for every current and future async-loading primitive, not just gltf.

**Instrument impact:** dev-tooling only — no runtime/show-time path is affected (the primitives' own async-load behavior is correct and by design); this only bit an offline authoring/look-dev session. Worth fixing before Scene 2/3 (or any future large-asset preset work) burns the same wall-clock-guessing cycle again.

### BUG-147 (bokeh-gather-cpu-reference-helpers-dead-without-gpu-proofs) — a `#[cfg(test)]` CPU-reference-parity helper module emits `dead_code` warnings under a plain (no `gpu-proofs`) test compile — LOW (cosmetic, no correctness impact), now confirmed SYSTEMIC across at least 2 primitives
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — gated both `bokeh_gather.rs::cpu_reference` and `bilateral_blur.rs::cpu_reference` behind `#[cfg(all(test, feature = "gpu-proofs"))]` (was plain `#[cfg(test)]`), per the entry's own fix shape. Audited the whole primitives directory for the same shape (`#[cfg(test)] mod cpu_reference`/`cpu_ref`) — found two more hits, `motion_blur.rs` and `ssao_gtao.rs`, but both have a SEPARATE plain-`#[cfg(test)]` module (`analytic_sanity`/an unnamed hand-check module) that consumes the same `cpu_reference` helpers without `gpu-proofs`, so gating those two would break their default-sweep tests — correctly left alone; not the same bug. Verify: `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` clean (0 errors, was 17); `cargo test -p manifold-renderer --lib` still 1224 passed (default sweep unaffected).
**Symptom:** rustc/rust-analyzer flags `BOKEH_N`, `BOKEH_GOLDEN_ANGLE`, `bokeh_hash_angle`, `Plane4` (+ its `texel`/`sample` methods), and `bokeh_gather_texel` in `bokeh_gather.rs` as unused. **Confirmed 2026-07-13, same landing session:** the identical shape reappeared in the just-landed `bilateral_blur.rs` (`K9`, `Fixture`, `depth_at`/`color_at`, `bilateral_texel`) the moment it compiled in the main checkout post-merge — this is a reflex of the CPU-reference-parity authoring pattern itself (`docs/ADDING_PRIMITIVES.md`'s I1-style precedent), not a one-off in one file. Any future primitive following the same pattern will reproduce it.
**Root cause:** these items live in the outer `#[cfg(test)]` module but are only consumed by the nested `#[cfg(all(test, feature = "gpu-proofs"))]` submodule; a compile of test code without `gpu-proofs` builds the outer module and never calls them. The standard scoped gate (`cargo clippy -p manifold-renderer -- -D warnings`, no `--tests`) doesn't compile test code at all, so it stays clean — this is only visible via `--tests` or IDE diagnostics, same blind spot as BUG-126.
**Not caused by this session's diff (for `bokeh_gather.rs`):** untouched by the `bilateral_blur` commit; `git log` shows its last change is P4's original `d85c6dc0`. The `bilateral_blur.rs` instance IS this session's own diff, logged here rather than fixed because it's the same one-line-fix-shape issue as the `bokeh_gather.rs` case and worth batching.
**Fix shape:** move each file's outer CPU-reference-parity helpers under the same `#[cfg(feature = "gpu-proofs")]` gate as the submodule that uses them (or nest them directly inside it). Mechanical, no behavior change. Given it's now confirmed systemic, worth a `docs/ADDING_PRIMITIVES.md` note in the I1-parity-test pattern itself so new primitives don't reintroduce it a third time.

### BUG-144 (prewarm-cache-tests-flake-under-full-lib-parallel-run) — two shared-pipeline-cache prewarm tests race each other under the full parallel `--lib` run — LOW (flaky-gate, not functional)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — per the entry's second fix option: both tests ("entry exists after" instead of "count increased by exactly one" — the assertion changed from `after > before` to `after > 0`, so a prior test having already populated the shared entry no longer fails the delta check. Order-independent by construction now, no cache-key or mutex changes needed. Verify: `cargo test -p manifold-renderer --features gpu-proofs --lib prewarm` → 6 passed, including both previously-racing tests. (Also ran the full unfiltered `--lib` gate: 6 unrelated pre-existing `BadInput` fusion-codegen failures in `freeze/codegen.rs`, untouched by this lane — out of scope, not fixed.)
**Symptom** — `cargo test -p manifold-renderer --features gpu-proofs --lib` (the full GPU suite) fails two tests: `node_graph::primitives::render_scene::gpu_tests::prewarm_pipelines_populates_the_shared_render_cache` and `node_graph::primitives::gltf_texture_source::gpu_tests::prewarm_pipeline_populates_the_shared_compute_cache`, both with a `before=N, after=N` panic (the count didn't grow). Both pass cleanly filtered to just that one test (`--test-threads=1` or an exact-name filter).

**Root cause** — the two tests race to prewarm the SAME process-global pipeline cache (the same cache `GeneratorRegistry::prewarm_all` populates at real startup). Whichever runs second in the parallel test binary finds its target entry already populated by the other's prewarm call moments earlier — an order-dependency the tests don't guard against (no reset of the shared cache between tests, no isolation of the specific pipeline key they check) — so its own before/after delta reads zero. Reproduced deterministically on the unmodified tree via `git stash`/`test`/`stash pop` (VOLUMETRIC_LIGHT_DESIGN P1), ruling out any relationship to that phase's Atmosphere/render_scene edits (the `RenderSceneUniforms` size change, 448→464 bytes, doesn't touch the pipeline-cache path at all).

**Fix shape** — either give each test a cache key/scene shape unique to itself (so the other test's prewarm can't satisfy its assertion), check for "entry exists after" instead of "count increased by exactly one" (weaker but order-independent), or add a named mutex/serial-test guard between the two so they never interleave. Cosmetic scope: the two named `--test gpu_proofs` runs and any single-module `--features gpu-proofs <module>::gpu_tests` filter (the CLAUDE.md-recommended narrow-run pattern) never hit this, and it's fully outside the default `nextest` sweep (gpu-proofs-gated) — only a full unfiltered `cargo test -p manifold-renderer --features gpu-proofs --lib` run is affected.

### BUG-142 (fire-meter-capture-bench-flakes-under-parallel-load) — a hard µs/tick ceiling on fire-meter capture cost fails under contention, same class as BUG-113 — LOW (flaky-gate)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D), same fix as BUG-113 — gated `worst_case_capture_cost_is_negligible_against_the_20ms_frame_budget` behind `manifold-core`'s new `bench-timing` feature (off by default). Verify: `cargo test -p manifold-core --lib` (default, test absent) and `cargo test -p manifold-core --lib --features bench-timing worst_case_capture` (runs, passes) both green.

**Symptom** — `manifold-core::audio_trigger::fire_meter_tests::worst_case_capture_cost_is_negligible_against_the_20ms_frame_budget` asserts a hard microseconds-per-tick ceiling for 512 fire-meter configs and panics when it's exceeded: `fire-meter capture: 512 configs/tick, 254.54 us/tick (budget: 20000 us/frame)` — well under the stated 20ms/frame budget in absolute terms, but over whatever internal ceiling the test itself asserts. An isolated rerun (`cargo nextest run -p manifold-core --lib <test>`) passed immediately after, and a clean full-workspace rerun moments later passed 3192/3192 including this test.

**Root cause (known, by analogy)** — same shape as BUG-113: a wall-clock micro-benchmark with a hard ceiling, run inside the normal correctness sweep, sensitive to CPU contention from nextest's parallel thread pool and whatever else the machine was doing (this session had just finished a `cargo clippy --workspace` compile moments before). `crates/manifold-core/src/audio_trigger.rs` was not touched by this session's changes.

**Fix shape** — same remedy BUG-113 names: give the ceiling real margin under parallel/loaded conditions, retry once before asserting, or move wall-clock ceiling assertions out of the default nextest sweep entirely (a dedicated feature/bin, same convention as `gpu-proofs`). Worth fixing both BUG-113 and this one together since they're the same underlying gate-design gap, not two unrelated bugs.

### BUG-124 (mesh-primitive-tests-clippy-debt-under-tests-features) — clippy fails on `-p manifold-renderer --tests --features gpu-proofs` in files unrelated to any recent change — LOW, gate-scope gap
**Status:** FIXED 2026-07-14 (bug-wave3 lane D), same fix as BUG-126 (this and BUG-126 named the identical 12 findings) — rewrote each flagged index loop to `iter().enumerate()`/`.skip()`/`.take()` in `push_along_normals.rs`, `scatter_on_mesh.rs`, `taper_mesh.rs`, `twist_mesh.rs`, `revolve_curve.rs`, plus `bend_mesh.rs`, `facet_normals.rs`, `gltf_mesh_source.rs`, `morph_mesh.rs`. Re-running the exact gate surfaced 5 MORE pre-existing findings beyond the original 12 (an inner/outer doc-attribute conflict in `coc_from_depth.rs`, an inconsistent-digit-grouping + excessive-precision pair on the same literal in `ssao_gtao.rs`, and a manual `.is_multiple_of()` in `blinn_specular.rs`) — fixed all 5 too rather than leave the gate red; folded into this fix since they're the same mechanical-debt class. Verify: `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` clean (0 errors, was 17); `cargo test -p manifold-renderer --lib` 1224 passed (unaffected); `cargo test -p manifold-renderer --features gpu-proofs --lib prewarm` unaffected. **Merge note (bug-wave3 lane A, folded in from a stale duplicate OPEN copy this same day):** a FUSION_SOTA P4a session had already hit this bug once and logged a precursor compile-error fix (`wgsl_compute.rs`'s `mod gpu_tests` referenced `Marker::StaticParam` with no `use` in scope, a plain `E0433` blocking the gpu-proofs test binary from compiling at all — fixed with one `use` line) before this bug's original 12 lint errors reappeared unchanged underneath it. Preserved here since the duplicate entry carrying it was removed as part of a multi-lane merge dedupe.

**Symptom** — `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` fails with 12 errors (`needless_range_loop`, `manual_range_contains`, `identity_op`) in `push_along_normals.rs`, `scatter_on_mesh.rs`, `taper_mesh.rs`, `twist_mesh.rs`, `revolve_curve.rs` test modules — none touched by the P9 session. The plain `cargo clippy -p manifold-renderer -- -D warnings` (lib+bins, no `--tests`) stays clean, which is why this debt went unnoticed: the CLAUDE.md-specified per-phase gate omits `--tests`, so nobody runs the stricter form routinely.

**Root cause (known)** — ordinary clippy debt in `#[cfg(test)]` code (index-loop patterns, manual range checks, a `1 * COLS` identity-op) that accumulated because the test-scope clippy variant isn't part of the routine gate.

**Fix shape** — mechanical: apply the suggested rewrites (`iter().enumerate()`, `RangeInclusive::contains`, drop the `1 *`) in the five listed files. Small, isolated, no behavior change. Optionally fold `--tests --features gpu-proofs` into the landing-time full-workspace clippy sweep so it doesn't silently drift again.

**Addendum (2026-07-14, FUSION_SOTA P4a session)** — hit again running this phase's `cargo test --features gpu-proofs` gate. One PRECURSOR bug surfaced first and was fixed in this session (out of P4a's scope, but blocking even compiling the gpu-proofs test binary at all): `wgsl_compute.rs`'s `mod gpu_tests` referenced `Marker::StaticParam` at 3 call sites with no `use` import in scope — a plain compile error (`E0433`), not a lint, present at `feat/fusion-sota`'s tip (`8bb94ea6`) before this session touched anything. Fixed with one `use crate::node_graph::freeze::markers::Marker;` line inside that module. Once that compiled, this bug's original 12 lint errors reappeared unchanged (still the same five files, still none touched by this session) — confirming BUG-124 needs no update beyond this note; the compile gap was simply masking it from anyone running `--features gpu-proofs` before this session.

### BUG-110 (osc-receiver-test-type-complexity-clippy-debt) — `manifold-playback`'s `--tests` clippy gate fails on `osc_receiver.rs`, unrelated to any of this session's changes — LOW (lint-only)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — factored `Arc<Mutex<Vec<(String, Vec<f32>)>>>` into a local `type RecordedOsc` alias at both sites, per the entry's fix shape. Verify: `cargo clippy -p manifold-playback --tests -- -D warnings` clean.

**Symptom:** `cargo clippy -p manifold-playback --tests -- -D warnings` (and `--all-targets`) fails
`clippy::type_complexity` twice in [`src/osc_receiver.rs`](../crates/manifold-playback/src/osc_receiver.rs)
at lines 366 and 368: `fn recording_receiver(address: &str) -> (OscReceiver, Arc<Mutex<Vec<(String, Vec<f32>)>>>)`
and its matching `let log: Arc<Mutex<Vec<(String, Vec<f32>)>>> = ...` binding.

**Root cause:** unknown/not investigated — out of scope for this session (BUG-088/072 name only
`osc_timecode.rs` and `audio_mixdown.rs`). Confirmed pre-existing and unrelated to this session's
edits: `git diff dd31cde4 -- crates/manifold-playback/src/osc_receiver.rs` is empty; the file's
last touching commit is `e4f51459` ("F3: external-sync test net"), an unrelated session.

**Fix shape:** trivial — factor the repeated `Arc<Mutex<Vec<(String, Vec<f32>)>>>` into a local
`type` alias (e.g. `type RecordedOsc = Arc<Mutex<Vec<(String, Vec<f32>)>>>;`) at both sites.
Mechanical, no behavior change. Left open per the same file-ownership convention BUG-088 used —
belongs to whoever owns `osc_receiver.rs`'s next change.

### BUG-113 (param-manifest-get-bench-flakes-under-parallel-load) — `bench_resolve`'s hard ns/op ceiling fails under nextest's parallel thread pool — LOW (flaky-gate)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — gated `bench_resolve` behind a new `bench-timing` cargo feature on `manifold-core` (off by default, same convention as `gpu-proofs`), so the wall-clock ceiling no longer runs in the default nextest sweep. Same class fix applied to BUG-142. Verify: `cargo test -p manifold-core --lib` (default, test absent from output) and `cargo test -p manifold-core --lib --features bench-timing bench_resolve` (runs, passes) both green.

**Symptom:** `crates/manifold-core/src/params.rs`'s `params::tests::bench_resolve` times
`ParamManifest::get`'s worst case (40 params, id last) and asserts `best_ns_per_op <= 271.5`
(a 2x ceiling over an old baseline). Under `cargo nextest run --workspace`'s default sweep,
run right after a heavy build and (in this session's case) a real 2-minute recording-soak
process that had just finished, it measured 333.25 ns/op then 398.98 ns/op on consecutive runs
and failed both times; an isolated `cargo test -p manifold-core --lib
params::tests::bench_resolve` immediately after measured 215.02 ns/op and passed, and a
subsequent clean full-workspace nextest run passed 3052/3052 including this test.

**Root cause:** the test is a wall-clock micro-benchmark with a hard-coded nanosecond ceiling,
run inside the normal correctness-test sweep — inherently sensitive to CPU contention from
nextest's shared thread pool and any other load on the machine (the failing runs in this
session followed heavy sequential cargo builds and a real recording capture). Confirmed
unrelated to the change being landed: `crates/manifold-core/src/params.rs` was not touched.

**Fix shape:** give the ceiling real margin for a loaded/parallel run, retry once before
asserting (take the best of N *sequential process* runs, not just N in-process rounds — the
in-process `ROUNDS` loop already exists but can't out-run sustained *external* contention), or
move this out of the default nextest sweep entirely (behind a feature or a dedicated bin, same
convention as `gpu-proofs`) since a wall-clock ceiling assertion doesn't belong in a "safe to
run freely, always green" default suite per CLAUDE.md's own testing-scope description.

### BUG-112 (manifold-ui-all-targets-clippy-debt-audio-setup-panel-graph-canvas-tests) — `manifold-ui`'s `--all-targets` clippy gate fails on two pre-existing, unrelated lints — LOW (lint-only)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — dropped the two unneeded `&` before `format!(...)` in `audio_setup_panel.rs` and replaced the `vec![WireView {...}, ...]` fixture in `graph_canvas/tests.rs` with a plain array literal, per the entry's fix shape. Re-running the exact gate (`cargo clippy -p manifold-ui --all-targets -- -D warnings`) surfaced a THIRD, unrelated `type_complexity` debt in `interaction_overlay.rs` — logged separately as BUG-161 rather than folded in here (different lint, different file, out of this entry's stated scope).

**Symptom:** `cargo clippy -p manifold-ui --all-targets -- -D warnings` fails on two lints that
have nothing to do with this session's changes:
1. `clippy::needless_borrows_for_generic_args` twice in
   [`src/panels/audio_setup_panel.rs`](../crates/manifold-ui/src/panels/audio_setup_panel.rs) —
   lines 2494 and 2498, both `LayerId::new(&format!(...))` where the borrow is unneeded
   (`LayerId::new` already accepts an owned `String` generically).
2. `clippy::useless_vec` once in
   [`src/graph_canvas/tests.rs`](../crates/manifold-ui/src/graph_canvas/tests.rs) — line 2391, a
   `vec![WireView { .. }, ...]` fixture that clippy wants as a plain array.

**Root cause:** unknown/not investigated (test-target lint debt, out of scope for this session).
Confirmed pre-existing and unrelated: `git diff HEAD -- crates/manifold-ui/src/panels/audio_setup_panel.rs
crates/manifold-ui/src/graph_canvas/tests.rs` is empty; both files' last-touching commit is
`f1a35270` ("feat(audio-dock-p4): D7 readability + D8 hygiene (P4)"), an unrelated session. The
scoped, non-`--all-targets` gate this session actually ran (`cargo clippy -p manifold-app -p
manifold-ui -p manifold-recording -- -D warnings`, per CLAUDE.md's worktree convention) is clean
— this only surfaces when test/bench targets are included, same pattern as BUG-110.

**Fix shape:** trivial and mechanical, no behavior change — drop the `&` before each
`format!(...)` argument at the two `audio_setup_panel.rs` sites; replace the `vec![...]` literal
in `graph_canvas/tests.rs` with a plain array literal (`[WireView { .. }, ...]`). Left open per
the same file-ownership convention BUG-110 used — belongs to whoever owns these files' next
change.

### BUG-089 (live-clip-pending-tick-queue-dead-on-all-live-paths) — `LiveClipManager`'s tick-based pending-launch queue can never be populated in production — LOW (dead code, correctness-neutral)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — deleted the whole subsystem per the entry's fix shape: `pending_by_tick`/`pending_by_layer`/`pending_by_clip_id` fields, `PendingLiveLaunch`, `queue_pending`, `remove_pending_by_clip_id`, `activate_due_pending_launches`, `activate_due_pending_launches_at_tick`, `has_pending_activations`, `pending_launch_count`, the `engine.rs` tick-3 call site, and the dead cancellation arm in `commit_live_clip` (now just `if !self.live_slots.contains_key(&layer_index) { return; }`). `trigger_live_clip`/`trigger_live_generator_clip` now call `activate_live_slot_now` unconditionally (the `event_absolute_tick >= 0` branch that used to queue was always false in production, confirmed by this entry's own grep). Deleted `tests/live_clip.rs::pending_launch_queue_activates_at_tick` (the test that only exercised this) and the now-meaningless `pending_launch_count() == 0` assertion in `midi_launch_with_release_before_snap_still_gates_correctly`-family test, plus the now-dead `MockHost::at_tick` helper. Deletion gate: `rg` for every listed symbol across `crates/**/*.rs` returns zero hits. Verify: `cargo test -p manifold-playback --lib --tests` 236+9+10+23+2+8+5+4 passed, 0 failed; `cargo clippy -p manifold-playback --tests -- -D warnings` clean.

Found 2026-07-10 while implementing F2 (MIDI launch quantize, CORE_ENGINE_MAP-adjacent). F2's
brief specifically flagged `activate_due_pending_launches_at_tick` as a deletion candidate and
asked for a caller grep before removing it. That grep turned up more than the one function:
`queue_pending` (`live_clip_manager.rs`) — the only writer of `pending_by_tick` /
`pending_by_layer` / `pending_by_clip_id` and the only place `PendingLiveLaunch.target_tick` is
set — only runs when its caller's `event_absolute_tick >= 0`. Every live producer of that value
traces back to `MidiNoteEvent.absolute_tick`, and `midi_input.rs`'s midir callback (the *only*
constructor of `MidiNoteEvent` in the whole workspace — confirmed by grep, not inference) always
sets it to `-1`. `fire_layer_oneshot` (the audio-trigger path) also always passes `tick = -1`
explicitly. So `pending_by_tick` can never be non-empty on any live path today. Its one live
reader, `activate_due_pending_launches_at_tick` (`engine.rs:803`, called every tick with
`self.last_frame_count as i32` — a frame counter, not a real MIDI clock tick), is therefore an
unconditional no-op in production (`if self.pending_by_tick.is_empty() { return false; }` fires
every call). The sibling beat-based `activate_due_pending_launches` and `has_pending_activations`
have no live caller at all — only `tests/live_clip.rs` exercises them. `commit_live_clip`'s
"pending launch cancellation" branch (the `!self.live_slots.contains_key(&layer_index)` arm) is
similarly unreachable live, since nothing ever queues a launch that skips straight to `live_slots`.

**Fix shape:** delete the whole subsystem — `pending_by_tick`, `pending_by_layer`,
`pending_by_clip_id`, `PendingLiveLaunch` (and its `target_tick` field), `queue_pending`,
`activate_due_pending_launches`, `activate_due_pending_launches_at_tick`,
`has_pending_activations`, the `engine.rs:803` call site, and the dead cancellation arm in
`commit_live_clip` — plus the `tests/live_clip.rs` coverage that only exercises it
(`pending_launch_queue_activates_at_tick`). Left open rather than done as part of F2: the
footprint is a full subsystem across two files and a test, wider than the single function F2 was
scoped to evaluate for deletion, and removing it correctly (without leaving `queue_pending`'s
write side orphaned, or silently changing `commit_live_clip`'s NoteOff behavior for some future
native-clock caller) deserves a dedicated pass with its own review, not a rider on a launch-
quantize fix. F2 left this code untouched and unexercised by its own changes.

### BUG-073 (ui-snap-script-drawer-tween-never-ticks) — the headless `--script` driver has no per-frame animation tick, so a mod armed mid-script renders an unclickable, zero-height drawer — LOW (found 2026-07-08 during PARAM_STEP_ACTIONS P3)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — built fix shape (b): `ParamCardPanel::skip_to_settled(&mut self, tree) -> bool` (settles drawer height, tab-ink, collapse, spawn-pop, delete-fade, value-flash, and value-snap-back tweens in one call, reusing `tick_drawers`/`tick_value_flash`'s own tick logic with a huge `dt_ms` rather than duplicating the settle math; returns whether anything was actually mid-flight) and `InspectorCompositePanel::skip_to_settled` (walks all cards, bubbles the bool up). Wired into `script.rs`'s `Runner::advance_frame` — the ONE seam every dispatch (`Key`/`Pointer`) runs through before `Snapshot`/`Dump` read the tree, since those two don't rebuild themselves — called unconditionally, forcing a rebuild only when something was actually settled (so a script with nothing armed keeps its prior cache-hit behavior; verified this doesn't regress `apply_ui_frame_invalidations`'s `needs_structural_sync` semantics). This is stronger than "audit existing flows for a missing `Step`" (the entry's other fix option): every flow is now correct by construction, no per-script opt-in needed. Verify: re-ran `scripts/ui-flows/param-step-action.json` (uses the pre-arm-in-fixture workaround this bug's report named) and `inspector-drawer-filmstrip.json`/`audio-clip-trigger-add.json`/`select-and-inspect.json` — all still pass; `cargo test -p manifold-ui --lib` 754 passed, `cargo test -p manifold-app --bin manifold` 174 passed; `cargo clippy -p manifold-ui -p manifold-app -- -D warnings` clean.

**2026-07-10 (UI_HARNESS_UNIFICATION P2):** the root symptom — "nothing calls
`tick_drawers`/`Panel::update` with real elapsed time" — is no longer true.
`script.rs`'s `Runner` was repointed at the shared render seam
(`crate::ui_frame::apply_ui_frame_invalidations` +
`composite_main_ui_frame`), and its `AutomationAction::Step` handler now
does a REAL `std::thread::sleep(DT)` + `ui.update()` per stepped frame
(mirroring `cache_path_full_render`'s P0 drawer-tween loop), so a script
that inserts `{"Step": {"frames": N}}` after arming a drawer now genuinely
ticks it toward settlement — confirmed working: `scripts/ui-flows/
inspector-drawer-filmstrip.json` (a fresh 12-frame `Step` after a compact-
toggle click) settles and its filmstrip shows the drawer visibly changing
across tiles. This is fix-shape (a)'s mechanism, just opt-in per script
rather than automatic on every dispatch — **not fully closed**: an EXISTING
flow (e.g. `param-step-action.json`) that doesn't add a `Step` after arming
still hits the original symptom, and this session didn't retrofit it or
build the unconditional auto-settle option (b) (`skip_to_settled()`/
`finish_all()` on `ParamCardPanel`). Revive to CLOSED by either auditing
existing flows for the missing `Step` or building (b).

**Symptom:** in a `cargo xtask ui-snap <scene> --script <flow>.json` run, a
click that newly arms a param's audio mod (or otherwise grows an EXISTING
card's drawer row count) dispatches correctly (confirmed via
`ui_bridge::dispatch` debug instrumentation — the right `PanelAction` fires
and mutates the project), but the drawer's own P1 reveal tween
(`ParamCardPanel::drawer_height_anim`, ticked by
`InspectorCompositePanel::update`'s `tick_drawers`) never advances: the
driver's `AutomationAction::Step` only increments a local `self.clock` field
used for input-event timestamps, nothing calls `tick_drawers`/`Panel::update`
with real elapsed time. The clip region sizing the reveal stays pinned at its
t=0 height (0, if the card is easing from unarmed) forever, so subsequent
rows in that clip region are invisible in the PNG AND unreachable by
`ui.pointer_event`'s hit-test (confirmed: `dump_tree_ex` still reports the
clipped nodes' raw, pre-clip rects with `VISIBLE | INTERACTIVE` flags, so the
dump looks fine while both the render and the click silently no-op — a
"dump says it's there, nothing else agrees" trap worth remembering before
trusting a dump alone against a freshly-armed drawer).

**Consequence for evidence-gathering:** any headless script that arms a
config-drawer-bearing param FOR THE FIRST TIME on an EXISTING card (one that
already went through one build with a smaller drawer) mid-script will show a
believable-looking `PNG`/dump pair with a truncated drawer. The workaround
used in `scripts/ui-flows/param-step-action.json`: pre-arm the mod directly
in the fixture (`ui_snapshot::fixtures::param_steps_scene`) so the card's
*very first* `configure()` call snaps `drawer_height_anim` straight to its
settled target (`param_card.rs`'s own comment: "a *new* param... snaps so it
never stalls half-open") — no tween in flight, no clipping. A REAL in-script
click that only changes content WITHIN an already-open, unarmed-row-count-
stable drawer (e.g. selecting a different Action/Mode segment on a param
that's already armed) is unaffected — confirmed working in the same flow.

**Fix shape:** either (a) give the `--script` driver a `self.rebuild`-adjacent
call that also ticks `ui.inspector`'s drawer/value-flash animations by a
large synthetic `dt` (e.g. `color::MOTION_MED_MS * 2.0`) after every
dispatch that sets `structural_change`, fully settling in one call instead of
requiring many small real-time-gated ticks; or (b) expose a
`skip_to_settled()`/`finish_all()` on `ParamCardPanel` the driver calls
unconditionally before every `Snapshot`/`Dump`/`Pointer`. Either closes the
gap for every future script that arms something mid-flow, not just this one.

### BUG-159 (timeline-scroll-past-playhead-violent-snapback) — scrolling past the playhead during playback violently snaps the view back; should be a smooth edge limit like Ableton — MED (performance-surface feel), reported by Peter 2026-07-14
**Status:** FIXED 2026-07-14 (bug-wave lane B). Root cause: `check_auto_scroll`
(`crates/manifold-app/src/ui_bridge/state_sync.rs`) unconditionally overwrote the
viewport's scroll offset every playback frame with zero suppression mechanism —
not a broken existing flag, there was none. Fix: `TimelineViewportPanel` tracks
the last user-driven horizontal scroll gesture (`note_user_scroll_x`/
`user_scroll_x_recent`, `crates/manifold-ui/src/panels/viewport.rs`), noted from
both wheel-scroll write sites in `window_input.rs` (Shift+scroll pan, native
trackpad horizontal swipe); `check_auto_scroll` yields (returns without moving
the viewport) while a scrollbar drag is active OR a gesture happened within an
800ms grace window — re-engage is automatic and implicit (the very next call
after the grace window elapses resumes following, no separate event). Tests:
`viewport::tests::user_scroll_x_recent_reflects_a_note_then_expires`,
`state_sync::bug159_auto_scroll_yield_tests::{auto_scroll_moves_when_no_user_gesture_is_active,
auto_scroll_yields_to_a_recent_user_scroll_gesture}`. Feel (the 800ms grace
value, whether it matches Ableton closely enough) is Peter's call on the rig —
not test-provable, flagged for his pass.

**Symptom** — during playback, manually scrolling the arrangement past the playhead fights the
playhead-follow auto-scroll: the view violently yanks back to the playhead instead of yielding.
Reference behaviour (Ableton and other professional DAWs): a user scroll takes over — or eases
against a soft limit — and follow re-engages predictably, never mid-gesture.

**Root cause** — unknown, not investigated. Suspect surface: the playhead-follow auto-scroll
writing the viewport offset unconditionally every frame during playback, racing the user's
in-progress scroll gesture instead of being suppressed or eased while one is active.

**Fix shape** — TBD after reading the follow logic; likely a follow-yields-to-gesture rule
(suppress auto-follow while a user scroll gesture is active, plus an explicit re-engage rule)
or an eased soft clamp at the playhead edge. Pin the exact feel against Ableton's behaviour;
acceptance is Peter's hands on it, not a test.

### BUG-161 (ui-snapshot-feature-fails-to-compile-canonical-def-arc-mismatch) — the headless `ui-snap` tool's own compile is broken — LOW-effort mechanical fix, but blocks the prescribed oracle for UI-regression bugs
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — found independently by two concurrent lanes the
same day (lane A while trying to bisect BUG-160, lane C while capturing before/after PNGs for
BUG-048/049/068); this is one bug, lane C's fix landed first. At each of the 8 `E0308` sites in
`ui_snapshot/mod.rs`: `view.canonical_def` → `&view.canonical_def` (6 by-reference call sites) or
`(*view.canonical_def).clone()` (2 by-value sites) — pure borrow/deref through the `Arc`, no
semantic change. `cargo build`/`cargo clippy --features ui-snapshot -- -D warnings` clean.
**Unblocks BUG-160**: its prescribed oracle (`ui-snap inspector` bisection between `a0eba10c` and
its parent) can now actually run.

**Symptom** — `cargo check -p manifold-app --features ui-snapshot --bin manifold` fails with 8 `E0308` mismatched-type errors in `ui_snapshot/mod.rs` (lines 454, 504, 581, 638, 660, 670, 896, 917): `view.canonical_def` is passed where callees (`render::render_graph_to_png`, `render::render_graph_editor_to_png`, others) expect `&EffectGraphDef`, but `canonical_def` is now `Arc<EffectGraphDef>` — a plain `&` no longer coerces. The whole `ui-snap` binary target fails to compile, so no headless UI scene can be rendered at all right now.

**Root cause** — unknown which change flipped `canonical_def` from an owned `EffectGraphDef` to `Arc<EffectGraphDef>` (likely a preset-loading or graph-caching change elsewhere that made the canonical def shared); `ui_snapshot/mod.rs`'s 8 call sites were never updated to match. Confirmed pre-existing on `origin/main` (identical content on the commit this lane branched from) — this session's diff never touches `ui_snapshot/mod.rs`. Not caught by the default `nextest` sweep because `ui-snapshot` is a separate cargo feature, not compiled by a plain build.

### BUG-049 (child-row-right-indent) — Group-child header rows double-pay the indent on right-anchored controls — LOW (visual misalignment, ~20px)
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — `layer_header.rs` split `pad` (left-anchor,
still `PAD + CHILD_INDENT`) from a new `right_pad = PAD` used by every right-anchored x/width in
`compute_layer_row`/`compute_audio_row`: `handle_x`, `dd_w` (Blend dropdown), and both
`right_edge` computations (routing form + audio Gain/Send row). Class-swept: `rg '\bpad\b'` in
the file found no other right-anchored use outside these five. `layout_matches_frozen_oracle`'s
hand-copied oracle updated identically in the same commit (mirrors the live fix rect-for-rect);
`manifold-ui --lib` green (26/26 in the file).

**Found 2026-07-07 by the label-collision fix worker (timeline-ux pass), verified in the
Liveschool after-PNG.** `layer_header.rs:489`: `handle_x = w - pad - HANDLE_W - 8.0` uses
`pad = PAD + CHILD_INDENT`, but the indent only moves the card's LEFT edge — so child
cards get a ~28px interior right margin vs 8px on top-level rows. Drag handles and Blend
chips sit ~20px left of their top-level siblings, and the collapsed name budget is 20px
tighter than necessary (it contributed to how early BUG-fixed label truncation kicks in).

### BUG-048 (arm-two-reds) — Automation ARM idle vs armed are both red, distinguished only by shade — LOW (stage-legibility; behavior-changing mode)
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — ARM no longer shares REC's red pair.
`transport.rs::automation_group`: idle = `BUTTON_INACTIVE_C32` (matches sibling LANES/BACK idle
chrome), armed = `STATUS_WARNING` (amber) — never red, so it can't be mistaken for REC's
recording/not-recording pair at stage distance. Class-swept (`rg RECORD_RED RECORD_ACTIVE`):
the only other survivors are REC itself (a genuine on/off pair, correctly red) and BACK's
override-latch (red vs neutral gray, not two reds against each other — no ambiguity). UX call
made without Peter (he's away): amber/`STATUS_WARNING` chosen over reusing
`AUTOMATION_LINE_COLOR` because that token is itself `RED_ACTIVE` (would have reintroduced the
exact bug). `automation_state_updates_in_place` test updated to pin the new colors; before/after
PNGs saved for Peter's look-pass (see session report).

**Found 2026-07-07 (timeline-ux headless audit).** `transport.rs::automation_group`:
idle ARM = `RECORD_RED`, armed = `RECORD_ACTIVE` — a deliberate mirror of the REC
active/idle pair. But REC's two states are "recording or not", while ARM's decide what
touching a param DOES (override the lane vs punch automation INTO the arrangement) —
a wrong read on stage silently writes automation into the show.

### BUG-101 (setup-spectrogram-scroll-offset) — Docked Audio Setup spectrogram blit doesn't follow the body scroll offset — LOW
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — `audio_setup_panel.rs::build_nodes` now shifts
`self.scope_rect` by the same `-offset` applied to the scrolled tree content, right after
`self.scroll.offset_content(tree, -offset)`. `update_band_meters` derives its geometry from
`scope_rect()` too, so the 2026-07-11 follow-up note (band meters sharing this root cause) is
fixed by the same one-line change. New regression test
`scope_rect_follows_body_scroll_offset` scrolls the docked body via `handle_scroll` and asserts
`scope_rect().y == unscrolled_y - offset`; still dormant in the shipped app per the existing
note (`AudioSetupPanel::handle_scroll` has zero call sites), so this closes the mechanism ahead
of it being wired up.

**Found 2026-07-10 during AUDIO_SETUP_DOCK P1** (worker shortcut #3, orchestrator-logged
at landing). The spectrogram waterfall is a GPU blit positioned by a CPU-side `scope_rect`
computed at build time in `audio_setup_panel.rs`, and that rect does not add the
`ScrollContainer` scroll offset. At `scroll_offset == 0` (the default, and everything the
P1 gate exercises) it's correct; once the docked body is scrolled, the waterfall draws at
its pre-scroll position while the rows around it move. **Symptom:** spectrogram visually
detaches from its section header when the panel body is scrolled. **Fix shape:** offset
`scope_rect` by the scroll delta (or parent the blit rect to the scroll content like the
rows), and clip it to the scroll viewport. **Oracle:** not reproducible headless (the blit
doesn't run in the snapshot harness) — needs the live app or a harness that runs the scope
blit; a scrolled-body render test would guard it.

**Update 2026-07-11 (fire-meter-unification pass):** confirmed the same root cause also
explains `SendRowIds`'s per-send level meter (fixed this session — `meter_track: NodeId`
now read live instead of cached `meter_x/y/w/h`) and `AudioSetupPanel::update_band_meters`
(`audio_setup_panel.rs`, still open) — both derive from geometry captured in `build_nodes`
before its own `self.scroll.offset_content()` call. The send-meter case had a track node to
anchor to, so it's fixed. The scope/band-meter case roots in `self.scope_rect` (`pub fn
scope_rect(&self) -> Option<Rect>`, no `&UITree` param) — making it scroll-live needs a
signature change threading `&UITree` through every caller (the present-pass blit, both
hit-tests, `update_scope_lane_labels`), which is this bug's real fix shape and is out of
scope for a geometry-sourcing-only pass. Currently dormant either way:
`AudioSetupPanel::handle_scroll` has zero call sites anywhere in the app (grepped), so
`scroll_offset()` is always 0 in the shipped build and neither symptom is user-reachable yet.

### BUG-081 — Audible blip when an audio clip's voice is built (play-then-pause leaks ~10ms of the file's start) — LOW
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — `make_voice` now applies `.volume(0.0)` to the
`StaticSoundData` before `manager.play`, so the voice is built silent instead of played-then-
paused; the per-tick sync path already restores the real volume via `set_volume(volume,
declick())`, so activation is unaffected. Kills the whole class, including the race window a
pause-based fix wouldn't close. `manifold-playback` builds clean.

**Symptom** — a very subtle pop/click from the speakers at the moment an audio file is
loaded onto the timeline (e.g. Finder drag-drop). Reported by Peter 2026-07-05.

**Root cause** — [audio_layer_playback.rs:171-179](../crates/manifold-playback/src/audio_layer_playback.rs#L171-L179):
`make_voice` calls `manager.play(data)` at full volume and only then
`handle.pause(Tween::default())`. kira's `pause` is a fade-out — and `Tween::default()`
is a **10ms** linear fade (kira-0.9.6 `tween.rs:110`), not instantaneous — so the first
~10ms of the file renders audibly before the voice reaches its "start paused at 0" state.
Any file whose first samples carry signal produces the blip. (The 5ms `declick()` tween
used everywhere else in this module doesn't apply here; this is the one edge built on
kira's default tween.)

**Fix shape** — build the voice silent instead of pausing it after the fact: apply
`.volume(0.0)` to the `StaticSoundData` before `manager.play`, keep the pause+seek. The
per-tick sync path already restores the real volume via `set_volume(volume, declick())`,
so activation is unaffected. This kills the whole class including the race where an audio
callback fires between play and pause. One-line-ish, `manifold-playback` only.

### BUG-031 — Layer context-menu + rename still address layers positionally — LOW (follow-up to the LayerId migration `877852a9`)
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — both clusters now carry `LayerId` end to end.
`ContextAddVideoLayer/GeneratorLayer/AudioLayer/DeleteLayer/DuplicateLayer/PasteAtLayer/
ImportMidi/Ungroup/SetLayerColor` and `DropdownContext::LayerContext` all switched from `usize`
to `LayerId`; consumers re-resolve the live index via `project.timeline.find_layer_by_id`/
`find_layer_by_id_mut` at dispatch (execution) time instead of baking in the index at menu-open
time — mirrors the pattern `DeleteLayerClicked(LayerId)` already used. `TextInputField::
LayerName(usize)` became a bare `LayerName` variant with the id stored on `TextInputState::
layer_id` (mirrors the existing `MarkerName`/`marker_id` and `AudioSendLabel`/`audio_send_id`
idiom already on this `Copy` enum) — no `Copy`-dropping cascade needed. Class-swept the
`TrackRightClicked` menu-build site too (a second, separate constructor for the same shared
`Context*` actions) — it now resolves a `LayerId` from the row-under-cursor before building menu
items, closing the same window there. `cargo check --workspace --tests` and `cargo clippy -p
manifold-ui -p manifold-app -- -D warnings` clean; `manifold-app` test suite 189/189.

**Root cause** — the primary layer-header actions were migrated to carry a stable `LayerId`
(commit `877852a9`, kills the panel-index-vs-live-model collision). Two related clusters were
deliberately left positional to keep that diff bounded:
- The **`Context*Layer` right-click-menu family** (`ContextPasteAtLayer`, `ContextImportMidi`,
  `ContextAddVideoLayer/GeneratorLayer/AudioLayer`, `ContextDuplicateLayer`, `ContextUngroup`,
  `ContextDeleteLayer`, `DropdownContext::LayerContext`) still carry a `usize`. `LayerHeaderRightClicked`
  now carries the id and `ui_root` resolves it to the current row synchronously when the menu opens,
  so there's no regression — but the menu ITEMS bake in that index, leaving a (rare) stale window
  between menu-open and item-click.
- **`TextInputField::LayerName(usize)`** (layer rename): the enum derives `Copy`, and `LayerId`
  isn't `Copy`, so migrating it forces dropping `Copy` and cascades through the whole text-input
  subsystem (`app.rs` field handling). The double-click intercept resolves id→index locally, so the
  rename has the same (unchanged) stale window it always had.

**Symptom** — none observed; latent. A context-menu action or a rename committed after the layer
list changed under it (another command, undo/redo, MIDI phantom layer) could hit the wrong layer.
Same bug class as the migration killed for the primary controls.

**Fix shape** — carry `LayerId` in the `Context*Layer` family (thread it from
`LayerHeaderRightClicked` through the menu items) and switch `TextInputField::LayerName` to
`LayerId` (drop `Copy` from `TextInputField`, fix the fallout in `app.rs`). Mechanical, compiler-driven.

### BUG-068 (inspector-scene-cliphit-overlap) — `inspector` ui-snap fixture clip/panel hit overlap — LOW
**Status:** FIXED 2026-07-14 (bug-wave3 lane C)

**Found 2026-07-08 during DRAG_CAPTURE P1 L3 authoring; pre-existing at `b9304330`.** The
`inspector` snapshot scene forces a generous `inspector_width` (600px of the 1536px canvas, set
in `ui_snapshot::mod` for the inspector-subject scenes) so the selected layer's param cards have
room — at the fixed 24px/beat zoom that leaves only ~29 beats of clip area before the inspector
column starts. GLOW's clip (48 beats), PLASMA's clip (48 beats), and TEXT BOT L's RETURN clip (24
beats starting at beat 24) all extended past that boundary. `TimelineViewportPanel::
visible_clip_rects` only culls clips fully outside the tracks rect — it returns each surviving
clip's FULL, unclamped pixel width (the comment notes "the GPU scissor clamps partials at the
edges" for *rendering*, but the returned hit-test rect is unclamped) — so those three clips' hit
rects genuinely overlapped the inspector column's screen region, meaning no clip in the scene was
simultaneously uniquely-labeled and safely draggable near the inspector edge. This is why P1's
`drag-clip-release-over-inspector.json` flow proves position-independence on the `timeline` scene
(drag past the tracks' right edge) instead. Fixture-only, no app runtime impact.

**Fix (FIXED @ this session):** shortened GLOW's and PLASMA's clips from 48 to 20 beats and
TEXT BOT L's two clips from 24+24 to 10+10 beats (`fixtures.rs::inspector_scene`) — every clip
now ends by x≈710, well clear of the inspector's left edge at x=936 (226px margin). Regression
test `bug068_clip_panel_overlap::inspector_scene_clips_clear_the_inspector_column`
(`ui_snapshot/mod.rs`) builds the real scene, reads `ui.viewport.visible_clip_rects`, and asserts
every clip's right edge stays left of `ui.layout.inspector().x` — fails red on the pre-fix
48-beat clips, green post-fix. **Class swept:** `bug060`/`gltfscene`/`bug047`/`paramsteps` share
the same `inspector_width = 600.0` override and could in principle grow a similarly long clip,
but none of them are used for clip-drag/hit-test flows today (their subject is inspector
scroll/param display) — not touched; revival trigger is a future script that drags a clip in one
of those scenes.

### BUG-125 (preset-runtime-generator-picks-first-final-output-nondeterministically) — a generator preset JSON with more than one `system.final_output` node has its tracked output picked via `AHashMap` iteration order, not graph position — LOW today (no shipped preset has two), but a real correctness trap
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`), option (a) from the fix shape — reject at load. `PresetRuntime::from_def`'s generator path (`preset_runtime.rs:2617`) now counts `FINAL_OUTPUT_TYPE_ID` matches before resolving the tracked output; a count > 1 returns a new `JsonGeneratorLoadError::MultipleFinalOutputs { count }` instead of silently picking one via `.find()`. Threaded through `graph_tool validate`'s error → `ValidationIssue` conversion too (`validate.rs:216`), so a bad preset JSON is caught by the pre-flight tool as well as the runtime loader — the invariant is enforced at both entry points, not just one. Regression test `dual_final_output_is_rejected_at_load` builds a real two-`final_output` generator graph and asserts the new error. Gated: `cargo clippy -p manifold-renderer -- -D warnings` clean; `cargo nextest run -p manifold-renderer` full crate sweep 1265/1265 passed.

**Symptom** — `PresetRuntime::from_def`'s generator path resolves its ONE tracked output via `graph.nodes().find(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)` (`preset_runtime.rs` ~line 2566). `Graph::nodes()` iterates the graph's `AHashMap<NodeInstanceId, NodeInstance>`, whose iteration order is not insertion order and not guaranteed stable across runs (random-seeded hash). A JSON graph with TWO `system.final_output` nodes (e.g. one authored to inspect a second Texture2D output alongside the primary) gets one of the two picked *nondeterministically per process*, and `render()`'s per-frame `replace_texture_2d` installs the host's canvas render target onto whichever one won — silently overwriting that node's real producer-allocated texture (format included) with the canvas's format. Reproduced empirically: a scene wiring both `color` (Rgba16Float) and a second `R32Float` output each to their own `system.final_output` sometimes rendered correctly and sometimes corrupted the second output's texture with the canvas's Rgba16Float data, occasionally triggering a genuine Metal command-buffer fault (`kIOGPUCommandBufferCallbackErrorInnocentVictim`) when the mismatched byte layout confused a subsequent GPU pass — this looked exactly like a device-contention flake until isolated to specifically the dual-final-output configuration (single-final-output graphs, rebuilt in the same tight loop, never flaked).
**Root cause** — `.find()` over an unordered map with more than one matching element is inherently ambiguous; the generator-path data model (`PresetIo::Generate { final_output_input_resource: ResourceId, .. }`, singular) was never designed to support more than one `system.final_output` and doesn't validate that assumption at load time.
**Fix shape** — either (a) `from_def`'s generator path errors loudly (`JsonGeneratorLoadError`) when it finds more than one `system.final_output` node instead of silently picking one, or (b) if multi-output generators are a real desired feature, design a named/keyed output surface (a stable `handle`, not `.find()`'s first hash-map hit) and thread it through `PresetIo::Generate`. Until then: document (here, and in a `from_def` doc comment) that generator JSON must carry exactly one `system.final_output`; a second output's texture is only safely inspectable via `dump_textures_all` fed by a WIRE to any non-`FinalOutput` sink (dead-end consumer — the resource still gets a step-output binding per `execution_plan.rs`'s `consumed_outputs`, without touching the ambiguous single-output tracking at all). `crates/manifold-renderer/tests/gpu_proofs/gbuffer_depth.rs`'s module doc documents and works around this exact trap.

### BUG-122 (graph-editor-node-face-loses-type-name-when-custom-named) — node cards show only the custom name, type name shows nowhere — LOW/MED authoring legibility
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Generalized the `(WGSL)` marker's existing append-not-replace precedent, per the backlog's own suggested fix shape: `display_label` (`snapshot.rs:838`) now renders `"<author title> — <friendly type>"` when they differ, and just the plain label when they'd be identical (no "Blur — Blur" on a no-op rename). Single-function fix — `render.rs`'s header draw already elides long titles to the node's width, so the compound form degrades gracefully with no render-side change needed. 4 new unit tests on `display_label` (compound form, identical-skip, no-title passthrough, WGSL marker still appends after the compound). Gated: `cargo clippy -p manifold-renderer -- -D warnings` clean; `cargo nextest run -p manifold-renderer` full crate sweep 1264/1264 passed.

**Symptom** — In the graph editor, once a node has a custom (author-assigned) title, its face shows only that name. The node's actual type (e.g. `node.blur`, `node.mix`, `node.scatter_on_mesh`) is nowhere on the card — no subtitle, no badge, no tooltip fallback — so a graph with several renamed nodes becomes unreadable as to what each node actually is.

**Root cause (found)** — `display_label()` (`crates/manifold-renderer/src/node_graph/snapshot.rs:838-848`) computes the header title as: the author title if set, else the friendly palette label, else a prettified type id. When an author title exists, it's returned as-is — the friendly/type label is dropped entirely, with the sole exception of a `(WGSL)` suffix appended for `wgsl_compute` nodes. `render.rs:625` then draws only this single `title` string on the node face; there's no second field carrying the type id anywhere for display. This has been the shipped behavior since `ebd48cde` ("Node titles: honor an author title on any node, keep (WGSL) marker", 2026-06-03) — not a recent regression, just newly bothering Peter now that graphs carry more custom-named nodes (Scene 2/3 authoring).

**Fix shape** — `display_label` (or its caller) should combine both when an author title is set, not choose one: e.g. a small secondary type line under the header, a tooltip that always surfaces the type id, or a "Custom Name — Friendly Type" compound header. The `(WGSL)` marker's append-not-replace pattern for `wgsl_compute` is the precedent to generalize.

### BUG-121 (graph-editor-effect-card-missing-mapping-drawer-chevron) — sideways slider-mapping drawer chevron missing from the effect/generator card — HIGH authoring surface (users can't edit mappings at all)
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Root-caused live and closed the whole class: (1) `InspectorCompositePanel` (the shared inspector/card-lane host, used by BOTH the main window and the graph-editor window's own `UIRoot` instance) had no `CardContext` plumbing at all — every card it built via `ParamCardPanel::default()` always defaulted to `CardContext::Perform`, confirming the backlog's suspected lead. Fixed at the host: added `InspectorCompositePanel::set_card_context`/`card_context` field, applied in `reconcile_cards`/`configure_gen_params` to every card built (new AND already-held, including mid-collapse `dying` ones); `Workspace::new` calls `ui_root.inspector.set_card_context(CardContext::Author)` once for `WorkspaceKind::GraphEditor`, never for `Main` — a single, structural fix, not a per-card patch. (2) A second, independent gap: even with the chevron rendering, its click emits `PanelAction::OpenCardMapping`, which `ui_bridge::mod.rs` routed to "handled in app_render.rs" — but app_render.rs never actually handled it (`self.editor_mapping_popover.open()` had zero call sites app-wide). Wired it: `app_render.rs`'s editor-card-action loop now resolves the watched target's reshape (`watched_full_reshape`) and the clicked card's own chevron anchor (`InspectorCompositePanel::mapping_chevron_rect`, pre-existing but never called from production) and opens `editor_mapping_popover` there. Host-level regression test added (`inspector.rs::author_context_host_draws_resolvable_mapping_chevron`) proving an Author-context host draws a resolvable chevron and a Perform-context host never does — guards the exact gap that shipped (the widget's own unit tests already passed while unreachable app-wide). Gated: `cargo clippy -p manifold-ui -p manifold-app -- -D warnings` clean; `cargo nextest run -p manifold-ui -p manifold-app` 980+189 passed. Not independently confirmed live in the running app (no GUI session this pass) — Peter's eyes still owed; the click path is otherwise fully exercised by the new test down to the resolvable anchor rect.

**Symptom** — The graph editor's effect card (and, by the same code path, the generator card) has lost the right-edge chevron that opens the sideways drawer for a param's slider mapping / range (drag trim range, Ableton range, etc.).

**Root cause (suspected, strong lead)** — The chevron and its click action are gated by `author && info.mappable` (`param_card.rs:2578`, `:2792`), where `author = self.context == CardContext::Author` (`param_card.rs:1863` etc.). `context` defaults to `CardContext::Perform` (`param_card.rs:654`) and only ever changes via `set_context()` (`param_card.rs:1205`), whose doc comment says "the host sets it once on its dedicated panel." A repo-wide search finds **zero** production call sites for `set_context(CardContext::Author)` — every call is inside `param_card.rs`'s own `#[cfg(test)]` module. `info.mappable` itself isn't the problem: `manifold-app/src/ui_bridge/state_sync.rs:2098` sets it unconditionally `true` for every row built from the live param manifest. If no live host actually calls `set_context(Author)`, the mapping drawer (`PanelAction::OpenCardMapping`, only ever emitted from these two gated chevron-click handlers) is unreachable everywhere in the shipped app, not just missing on the effect card specifically. Not yet confirmed which struct is "the host" from the doc comment, nor whether its `set_context` call was removed or never wired up — needs a live repro plus git blame on the actual host construction site before calling this fully root-caused.

**Fix shape** — find the panel host that owns the graph-editor's Author-context `ParamCardPanel` instance and confirm/restore its `set_context(CardContext::Author)` call; add a regression test at the host level (not just param_card's internal unit tests, which already correctly cover Author-context behavior in isolation) so a missing wire-up like this fails loudly again.

### BUG-012 — Fragment `tex_` port-rename corrupts scalar params named `tex_*` — LOW
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Root fix at the class level: `wgsl_compute.rs`'s fragment-form input rename (`:581`) now filters to texture-typed ports (`matches!(inp.ty, PortType::Texture2D | PortType::Texture2DTyped(_))`) before stripping `tex_`, mirroring the sibling binding-key rename's existing filter (`:586`, `BindingKind::SampledTexture`) — the two renames can no longer disagree. Regression test `fragment_scalar_param_named_tex_prefixed_is_not_stripped` proves a `@param: tex_speed` scalar keeps its full port name and its param-manifest entry. Gated: `cargo clippy -p manifold-renderer -- -D warnings` clean; `cargo nextest run -p manifold-renderer node_graph::primitives::wgsl_compute` 32/32 passed.

**Root cause** — [wgsl_compute.rs:544-548](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L544-L548):
the fragment-form rename loop strips a literal `tex_` prefix from EVERY input port name with
no type filter (the sibling texture-binding rename at 549-561 IS filtered to
`SampledTexture`). A scalar `@param: tex_speed` exposes port `speed` while the uniform layout
and params stay keyed `tex_speed`; the dispatch-time wire lookup misses and the live wire is
silently ignored.

**Symptom** — a wired LFO/Ableton control on such a param renders as connected but never
moves the value. Latent — no shipped preset uses a `tex_`-prefixed param name.

**Fix shape** — filter the rename to texture-typed ports, mirroring lines 549-561. One-line.

### BUG-076 (inspector-scroll-underestimates-content-height) — `try_inspector_scroll` clamps to a tiny max_scroll on genuinely tall content — LOW (found 2026-07-08 during UI_CLIP_AND_Z_OWNERSHIP_DESIGN P1)
**Status:** FIXED (closed as not-reproducible) — Peter's call 2026-07-14: "I also think this is not a real bug, I don't think I can even reproduce it." For the record: the original 2026-07-08 log carried a concrete measurement (~13–20px max_scroll on a stack ~1200px too tall), so something was observed once — but two investigations found no mechanism, every fixture-level test passes, and it doesn't reproduce on the rig. Reopen if a tall inspector stack ever won't scroll live. History: 2026-07-13 (`8d37d5e0`): the drawer-tween undercounting theory below was built and tested (a `ParamCardPanel`-level and an `InspectorCompositePanel`-level test with a 9-card stack, several audio-mod drawers armed at `configure()` time, zero `tick_drawers` calls) and RULED OUT — a card's `drawer_height_anim` is already snapped (not eased) to its settled target on first `configure()` for the single-configure/no-tick path, so `compute_height()` does not undercount there. Root cause remains open; next place to look is the real `state_sync`/`build` call ordering in `manifold-app`, or a card-reuse scenario the single-configure fixture doesn't cover. Regression tests for the ruled-out theory are kept as coverage. 2026-07-14: not to be confused with BUG-060 (inspector scroll ARTIFACTS, rig-verified FIXED + class-killed) — Peter asked whether this was that; it isn't. This entry is the scroll-RANGE clamp on tall content (can't scroll far enough), still unexplained.

**Symptom:** built a headless gate scene (`ui_snapshot/fixtures.rs`'s `bug060_scene`, added this
session) with 9 stacked effect cards, several with audio-mod drawers open — visibly, per the
unscrolled render (`target/ui-snapshots/bug060/bug060.png`), several cards extend well past the
1216px-tall canvas. Calling `UIRoot::try_inspector_scroll` (the same method
`window_input.rs`'s real mouse-wheel handler calls) with a delta of 300, 1000, or 1_000_000 all
converge to the SAME ~13-20px of movement and then stop — as if `max_scroll()` were computed as
roughly 20px, not the ~1200px the visible overflow implies.

**Root cause:** unknown — suspected but not confirmed. `ScrollContainer::apply_scroll_delta`
clamps against `self.content_height`, set via `InspectorCompositePanel::update_scroll_bounds`'s
`right_column_height()` -> `layer_column_height()`, which sums `card.compute_height()` per
effect card. Suspect: `compute_height()` reads a drawer-open-tween-animated height
(`drawer_height_anim`, see `param_card.rs`'s `drawer_open_tween_reserves_interpolated_height_
clips_then_settles` test) that starts at/near 0 and needs `tick_drawers(dt)` calls to reach its
settled value — a card configured with its audio mod ALREADY armed (as `bug060_scene` does, no
"click to open" step) renders its FULL drawer immediately (the build path uses the target
height directly) but `compute_height()` may still be reading the un-ticked animation state,
undercounting every card's height by its drawer's contribution. Not verified: whether
`configure()` seeds the animation at its target when armed from a cold build, or always starts
from 0.

**Fix shape:** instrument `right_column_height()`/`card.compute_height()` directly (a
`manifold-ui` unit test asserting `layer_column_height() ≈ sum of settled per-card heights` for
a 9-card, all-drawers-open fixture) to confirm or rule out the animation-state theory; if
confirmed, seed `drawer_height_anim` at its target value on first configure when the mod is
already armed (mirroring how the card already renders it), not just on a later toggle.

**Impact on this session:** blocked producing a scrolled-to-bottom PNG for
`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1's BUG-060 acceptance demo — worked around by deciding the
stopgap-removal question via a direct unit test (`InspectorCompositePanel::try_scroll_in_place`
called with a 1,000,000 delta, `manifold-ui`'s own suite, no PNG round-trip needed) instead of
the headless CLI harness. Also found and partially fixed en route, independent of this bug: the
L3 script runner's `Gesture::Scroll` never reached the inspector at all before this session
(routed only through the generic `UIEvent::Scroll` pipeline, which is real for the
dropdown/timeline but a no-op for the inspector's direct-call scroll path) — `script.rs` now
branches on `ui.layout.inspector().contains(center)` and calls `try_inspector_scroll` directly,
matching `window_input.rs`'s real dispatch. That fix is real and committed; this bug is what's
left after it.

### BUG-015 — Inspector sections render overlapping / at stale offsets after scroll — MED (repro needed)
**Status:** FIXED — closed by Peter's call 2026-07-14 (staleness audit). The root-cause fix for the stale-chrome-state class shipped 2026-07-08 (`738f4e94`/`4319eb8d`, `fix/bug-015-out-of-region-dirt`: the incremental cache path falls back to a full render on out-of-sub-region dirt), with tests and gates. The original 2026-07-04 "sections interleaved" sighting was never reproduced (headless attempts 2026-07-05 and 2026-07-07 ×2). Reopen if the sighting recurs.

**Symptom** — observed once by Peter, 2026-07-04, right after the timeline-P0 / multi-select
UX changes landed: the layer inspector drew its sections interleaved — the MIDI block
(MIDI / CHANNEL / DEVICE) and the audio-send block (send dropdown, +0.0 dB) overlapping
each other with a dead band between them, and the "No audio input" header clipped mid-panel.
Described as "a scrolling bug with the UI timeline updates". Screenshot lives in the
2026-07-04 session transcript.

**Root cause** — unknown. Suspect surface: inspector section Y-layout vs. scroll offset
(the `single-source-y-layout` invariant) or a stale subregion scissor
(`subregion-scissor-invariant`) going stale when timeline updates force a rebuild while the
inspector is scrolled.

**Repro** — not yet pinned. First step is reproducing: select a generator layer, scroll the
inspector, then trigger timeline churn (clip drag / multi-select updates) and watch for
section overlap.

**Fix shape** — TBD after repro. If it's the known invariant class, the fix is at the layout
single-source, not per-section patches.

**Repro attempt 2026-07-07 (timeline-ux headless audit)** — scroll-seeded `states` render
(101px) + driven inspector scroll on the `inspector` scene: sections stay correctly laid
out in both. Not reproduced. The missing ingredient per the symptom is timeline churn
DURING a scrolled state (rebuild-while-scrolled); the `--script` driver can now interleave
scroll + clip-drag + snapshot in one flow (post real-dispatch fix, this branch), so a
dedicated repro flow is now writable when this bug is next picked up.

**Sighting + concrete progress 2026-07-07 (Opus session)** — Peter hit inspector
artifacts again: on a Fluid Simulation generator (Master tab), stale fragments at the
panel's left edge (one is a patch of viewport/video showing through) plus a clipped sliver
above the Layer/Master tab strip. Screenshot in this session's transcript. May be this bug
or a close sibling — same suspect surface (stale inspector content), same repro difficulty.

_Ruled out this session:_
- NOT the just-merged trigger-gate drawer (§9): the drawer is CLOSED in the repro
  screenshot. Two proposed mechanisms for it — a "Mode row" escaping its clip parent, and an
  unbalanced Overlay paint-layer push — are both refuted by the code (every node in
  `build_toggle_trigger_row` parents to one `parent`; there is zero paint-layer manipulation
  in the card/drawer path).
- NOT a settled-state containment error: built the armed trigger-gate card into a real
  `UITree` and measured — one root node, max node bottom == the card's reserved height
  exactly, zero overflow. Height accounting (incl. the Mode row via `audio_config_height(true)`)
  is exact.

_New concrete suspect (stronger than the two above):_ the inspector's incremental atlas
cache. `UICacheManager::render_dirty_panels`
([ui_cache_manager.rs:175](../crates/manifold-renderer/src/ui_cache_manager.rs#L175))
repaints only dirty CARD sub-regions and trusts `LoadOp::Load` for everything else. The
sub-regions are the cards only
([inspector.rs:506 `sub_region_ranges`](../crates/manifold-ui/src/panels/inspector.rs#L506)) —
section backgrounds, tab strip, padding, inter-card gaps and margins sit in NO sub-region, so
an incremental frame never repaints them and a stale pixel there survives until the next full
render. The guard `extents_unchanged`
([:282](../crates/manifold-renderer/src/ui_cache_manager.rs#L282)) approximates each card's
painted extent by its FIRST node's bounds, so anything a card paints OUTSIDE its frame is
untracked.

_Measured seed candidate:_ `build_toggle_trigger_row`
([param_slider_shared.rs:1532](../crates/manifold-ui/src/panels/param_slider_shared.rs#L1532))
lacks the `drawer_reveal` reveal-clip that `build_param_row` has
([:2005-2017](../crates/manifold-ui/src/panels/param_slider_shared.rs#L2005)), so its drawer
paints ~120px below the card frame, unclipped, for the whole open/close tween (measured:
0 clip regions, 119.5px overflow, vs the slider path's 1 clip region that contains its
overflow).

_The crack in the hypothesis (why this is a reasoning problem, not a quick fix):_
`extents_unchanged` keys on the frame's bounds, and the trigger-drawer overflow exists only
while the frame is ALSO resizing (mid-tween), which changes the frame bounds → guard trips →
full self-clearing render → no ghost. So the guard MAY already prevent this exact ghost class.

_The one open reasoning question:_ is there any realistic in-place edit that keeps a card's
first-node (frame) bounds stable while changing what it paints outside that frame — OR that
paints into the never-repainted margins outside all sub-regions? If yes, the ghost is real;
fix = always repaint the inspector's opaque full-rect background before the dirty sub-regions
AND clip every card render to its own frame. If no, the guard covers the card case and the
culprit is the margins / a panel-boundary atlas-staleness issue (which fits the left-edge +
video-bleed fragments better).

_Repro difficulty:_ `render_ui_to_png` renders the tree directly and bypasses
`UICacheManager` entirely
([render.rs:44-51](../crates/manifold-app/src/ui_snapshot/render.rs#L44)), so NO existing
headless snapshot can show this class — every snapshot is a clean full render. A repro needs
a new harness driving `render_dirty_panels` across full→edit→incremental and reading back the
atlas. Blast radius is contained: only the inspector passes `sub_regions`; every other panel
full-renders when dirty and can't ghost this way. Handed to Fable as a reasoning task
(2026-07-07). Same family as BUG-025 (timeline-scissor-bleed).

**Verdict (Fable reasoning pass, 2026-07-07) — hypothesis REFUTED for the card-ghost class;
a different, real hole found; the video fragment exonerates the atlas entirely.**

_1. The card-ghost class cannot occur — three independent seals, verified in code:_
- **Every card-geometry tween runs under full invalidation, never the incremental path.**
  `tick_drawers` ([param_card.rs:1401](../crates/manifold-ui/src/panels/param_card.rs#L1401))
  bubbles collapse, spawn-pop, delete-fade, drawer-height AND tab-ink tweens into
  `drawer_anim_active`, which the app polls every frame
  ([app_render.rs:2940](../crates/manifold-app/src/app_render.rs#L2940)) → `needs_rebuild` →
  `invalidate_all()` ([:2842](../crates/manifold-app/src/app_render.rs#L2842)) → whole-atlas
  clear + full self-clearing renders. The trigger-drawer's unclipped ~120px overflow therefore
  never meets `LoadOp::Load` at all — the guard doesn't even need to catch it.
- **Bounds-stable-but-paints-outside edits don't exist in the card path.** Searched: the
  chevron's `Affine2::rotate` pivots about its own small rect (contained); slider fill/thumb/
  value-flash writes are contained under the card's opaque frame, which the incremental path
  always redraws first (`dirty_only=false`,
  [ui_cache_manager.rs:228](../crates/manifold-renderer/src/ui_cache_manager.rs#L228)).
- **The scroll-clip hole is already patched.** `traverse_flat_range` pre-pushes ancestor
  `CLIPS_CHILDREN` bounds for mid-tree ranges
  ([tree.rs:737-756](../crates/manifold-ui/src/tree.rs#L737)) — an incremental card repaint
  IS clipped by the scroll viewport. And the inspector's first node is a genuine full-rect
  opaque background ([inspector.rs:1892](../crates/manifold-ui/src/panels/inspector.rs#L1892)),
  so every full render self-clears the margins. The proposed fix direction ("repaint the
  background before dirty sub-regions") is actively WRONG: the background would overpaint the
  tab strip/chrome, which no sub-region would then redraw.

_2. The real hole (different from the hypothesis): out-of-sub-region dirt is silently
dropped._ The incremental path
([ui_cache_manager.rs:212-238](../crates/manifold-renderer/src/ui_cache_manager.rs#L212))
fires when ANY sub-region is dirty and repaints ONLY dirty sub-regions — it never checks for
dirt in the panel range that belongs to NO sub-region (tab strip, cog/Collapse controls,
scrollbar, all built directly in `build_in_rect`). `rendered_ranges` clears only the card
ranges, and the end-of-frame blanket `tree.clear_dirty()`
([app_render.rs:4807](../crates/manifold-app/src/app_render.rs#L4807)) then wipes the
remaining flags — erasing the evidence, so the fallback-to-full-render ("dirty list empty
next frame") never fires. The comment at
[app_render.rs:3870](../crates/manifold-app/src/app_render.rs#L3870) ("Deferred panels keep
their dirty flags") is falsified by :4807. **Trigger:** an in-place chrome mutation
co-occurring with card dirt — guaranteed whenever any param is audio-modulated (per-frame
card dirt), e.g. hover/unhover a tab or the Collapse button while a modulated generator
plays → the un-hover repaint is dropped and the stale hover state persists until the next
rebuild. This produces stale chrome STATES in place (ghost highlights, stale scrollbar) —
real, but probably NOT the screenshot's fragments. **Fix shape (root):** the incremental
path must detect dirt in the complement of the sub-regions and fall back to the full panel
render, and dirty-flag clearing for panel ranges should be owned by the cache manager (the
blanket `clear_dirty` may only touch the overlay region). **Sequencing: land BUG-060's clip
container first** — it bounds what this cache can be wrong about; order rationale in the
BUG-060 entry. This half is Opus-grade (fast-path regression risk), not Sonnet-mechanical.

_3. The video-bleed fragment cannot be atlas staleness at all._ The atlas never contains
compositor pixels: composite order is clear-to-black → atlas blit (pass 2,
[app_render.rs:3972](../crates/manifold-app/src/app_render.rs#L3972)) → compositor video
into `layout.video_area()` (pass 3, [:4001](../crates/manifold-app/src/app_render.rs#L4001),
opaque, aspect-fit INSIDE the rect) → timeline passes (4) → overlays (5, drawn straight into
the offscreen, [:4587](../crates/manifold-app/src/app_render.rs#L4587)). An atlas failure
shows black/transparent, never video. **Resolved by Peter 2026-07-07: the "video patch" in
the screenshot is just the preview window — legitimately there, not a bug.** (The composite
reasoning above stands for any future genuine video-over-UI sighting: it would implicate a
post-atlas pass, the BUG-025 class, never this cache.) The 2026-07-04 "sections interleaved"
sighting should be re-examined against hole #2 + rebuild-while-scrolled rather than the card
cache. The footer-overlap symptom this investigation started from is now its own entry with
a grounded root cause: **BUG-060** (no inspector-level pixel clip + the trigger-row drawer's
missing reveal clip).

**Outcome (Opus, 2026-07-08) — hole #2 (out-of-sub-region dirt dropped) FIXED at the root.**
BUG-060's clip container confirmed on `main` (@ `27557d18`) before starting, per the sequencing
note. Two-part structural fix:

1. _Incremental path now falls back on out-of-sub-region dirt._ New
`UITree::has_dirty_outside_ranges(start, end, covered)`
([tree.rs](../crates/manifold-ui/src/tree.rs)) reports DIRTY nodes in the panel range that lie
in no sub-region. The cache manager's incremental branch gates on a new `incremental_path_safe`
helper (`extents_unchanged` AND `!has_dirty_outside_ranges`)
([ui_cache_manager.rs](../crates/manifold-renderer/src/ui_cache_manager.rs)) — chrome dirt
(tab strip, cog/Collapse, scrollbar) now forces the full, self-clearing panel render the same
frame, so the stale-chrome ghost never paints. No one-frame lag: the fallback is same-frame,
not deferred.

2. _Panel-range dirty-flag clearing moved to the cache manager; blanket clear narrowed to the
overlay region._ The incremental path now returns the FULL panel range (safe: it only fires
when there's no out-of-sub-region dirt), so `clear_dirty_range` over `rendered_ranges`
([app_render.rs:3871](../crates/manifold-app/src/app_render.rs#L3871)) owns all panel-range
clearing. The end-of-frame blanket `clear_dirty()` at :4807 became
`clear_dirty_range(overlay_region_start, count)` — it no longer erases out-of-sub-region panel
dirt before the fallback can fire. The now-false comment at :3870 ("Deferred panels keep their
dirty flags") was corrected.

_Fast-path safety (the CRITICAL constraint):_ traced the tree layout — the 7 panels contiguously
tile `[0, overlay_region_start)` with zero gaps (build order: transport→header→footer→inspector
are back-to-back; the two split/resize handles are the SplitHandles catch-all
`[inspector_end, scroll_panels_start)`; layer_headers then viewport run to `overlay_region_start`;
`node_range() == (first, first+count)`). So clearing all rendered panel ranges + the overlay
range clears every node, exactly as the old blanket clear did — `has_dirty_in_range(0, panel_end)`
still settles to false and the `offscreen_dirty` idle fast path
([app_render.rs:3915](../crates/manifold-app/src/app_render.rs#L3915)) stays reachable. Both the
old and new end-of-frame clears run only on slow-path (dirty) frames — the fast path returns at
:3961 before either — so there is no clearing-parity change on idle frames. Since every dirty
panel is always rendered (no deferral in `render_dirty_panels`), no panel-range dirt can survive
a slow frame. Fast-path preservation is by reasoning + the tiling verification above, NOT a live
trace (the app is an interactive GPU rig I can't idle-observe headlessly here; `render_ui_to_png`
bypasses the cache).

_Verification:_ new device-free unit tests at the cache-manager helper layer —
`out_of_subregion_dirt_forces_full_render` (chrome dirt rejects the incremental path while
`extents_unchanged` still passes, isolating out-of-region dirt as the sole cause) and
`incremental_used_when_only_card_dirt` (in-card dirt stays on the fast path). Gate:
`cargo test -p manifold-renderer -p manifold-app -p manifold-ui` (993 + 158/10/1/3 + 646 passed)
+ `cargo clippy -p manifold-renderer -p manifold-app -p manifold-ui --all-targets -D warnings`
(clean; only pre-existing manifold-media Obj-C deprecation warnings). Shipped on
`fix/bug-015-out-of-region-dirt`. **Note:** this closes the stale-chrome-STATE class (ghost hover,
stale scrollbar). The 2026-07-04 "sections interleaved" sighting (hole #2 + rebuild-while-scrolled)
is a separate open thread if it recurs.

### BUG-025 — Timeline layer/header scissoring: clip content bleeds across row bounds — MED (repro needed)
**Status:** FIXED (believed) — closed by Peter's call 2026-07-14: he attributes the sighting to the GPU-pressure/contention issue behind the timeline blue-flicker, since fixed (the UI-present vs content GPU contention work). Never reproduced across three headless attempts (2026-07-05, 2026-07-07 ×2 — the second with a genuinely-applied scroll). Reopen if seen on the rig.

**Symptom** — reported by Peter 2026-07-05 (screenshot in session transcript) as "layer and
header scissoring": in the arrangement view, the bottom layer's purple clip body renders far
beyond its row — a solid block filling the timeline from its row down to the window edge —
while the layer-header column at bottom-left shows the Plasma MIDI drawer (MIDI / CHANNEL /
DEVICE) overlapping into that region. Clip content and header-column content are not being
mutually clipped to their rows/panes.

**Root cause** — unknown. Suspect surface: the per-row scissor rect for clip bodies (last or
expanded row), the `track-header-invariant` / `single-source-y-layout` class, or a stale
subregion scissor (`subregion-scissor-invariant`). Likely same family as BUG-015 (inspector
sections at stale offsets) — both smell like Y-layout/scissor divergence after the recent
timeline waves.

**Repro** — not pinned; NOT reproduced headless (2026-07-05 Opus). Snapshotted the `states`
and `timeline` scenes (both carry a selected generator layer with an open MIDI/CHANNEL/DEVICE
drawer, the closest fixtures to Peter's screenshot) — both render correctly: every clip body is
scissored to its row, every header drawer stays in the left column, group nesting clips fine.
A scroll-down + re-snapshot on `timeline` also did not reproduce (and scroll may not be fully
wired in the headless tracks path). So the general scissoring path is sound; the bug is
state-specific. Triage narrows it to a config the fixtures don't hit — most likely the
*last* row being a selected generator whose clip fills the remaining viewport height, and/or a
live scroll offset. Pin it with either a targeted fixture (selected generator as the final
layer) or a running-app repro from Peter's project.

**Repro attempt 2026-07-07 (timeline-ux audit)** — the 07-05 note's "scroll may not be fully
wired in the headless tracks path" is now explained: `--scroll` was seeded AFTER the base
render (fixed this branch), so every prior "scrolled" base PNG was actually unscrolled. With
scroll genuinely applied (via the interact after-render), headers + lanes offset together and
clip bodies stay scissored to their rows — still not reproduced. The state-specific triage
above stands.

**Fix shape** — TBD after repro. If it's the invariant class (likely, given BUG-015 is the same
family), fix at the single Y-layout source, not per-widget patches.

### BUG-151 (graph-editor-node-browser-container-fill-not-drawn) — the graph editor's node-spawn browser renders its cell rows but not the popup container fill/scrim, so the graph and inspector show through between the cells — MED (authoring surface looks broken; main-window instance of the same component is fine)
**Status:** FIXED — `docs/EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1, completed 2026-07-14 in two passes: the first pass (`9e3d710e`) landed D1 (the shared `tree_passes.rs::render_tree_overlay_passes`) but hit a genuine escalation on D2, logged below and superseded by this fix. Second pass (this commit) closed the gap D1 exposed: `UIRoot::overlay_draw`/`overlay_region_start` are populated only inside `UIRoot::build_overlays()`, which was reachable only from `UIRoot::build()` — a MAIN-window-only method the editor's `Workspace::ui_root` never called (it clears + lays out the entire main-window panel set, which would stomp the editor's per-frame tree). Fix: a new `pub(crate) fn build_overlays_for_screen(&mut self, w, h)` wrapper on `UIRoot` (`ui_root.rs`) sets `screen_width`/`screen_height` (the editor's `UIRoot` never gets `resize()` either, so this was also load-bearing) then calls the existing `build_overlays()` unchanged — safe standalone since it only reads screen size and the live open-overlay set. The editor's per-frame render (`app_render.rs`'s `present_graph_editor_window`) now calls this wrapper in place of the old hand-rolled `begin_region`/`browser_popup.build`/`end_region` block that bypassed the overlay system entirely; `editor_frame.rs`'s `composite_editor_frame` now narrows its base tree render to `[0, ui_root.overlay_region_start)` (D2, now meaningful) so the node browser renders ONLY through the shared tree-overlay pass, region-aware at OVERLAY depth — identically to the main window. Verified with a headless PNG (`ui-snap editor --open-picker`, saved at `docs/landings/BUG-151_editor_after_open_picker.png`): opaque popup container with search bar + full node grid over the graph, not bare cells.

**Root cause, corrected** — the original hunt (below) suspected a missing modal-dim-background scrim as a candidate explanation. **That hypothesis is wrong, confirmed by reading `BrowserPopupPanel::modality()`**: it returns `Modality::Modal { dim_background: false }` specifically because the popup builds its OWN full-screen backdrop node inside `popup_shell::build` — the driver deliberately does not add a second scrim for this overlay. There was never a missing scrim. The actual root cause is exactly what P1's first pass found and escalated: the editor's popup nodes were never registered as an overlay at all (`overlay_draw` was permanently empty for the editor), so the shared tree-overlay pass (already landed by D1) had nothing to draw — cells painted because the flat root-scan swept them up at CONTENT depth, but the popup's own backdrop/container lived inside the SAME un-recorded region and rendered exactly the same way, i.e. it should also have appeared under the old flat scan; the visually "missing" fill was actually cells-over-graph with no z-order enforcement between the popup's internal draw order and the rest of the flat-scanned tree, not a missing node. Fixed at the root by making the editor register overlays through the same driver the main window uses, not by adding a scrim.

**Symptom** — opening the node browser inside the graph editor shows floating search bar + cell rows with the graph canvas and the inspector panel bleeding through between and behind them. The SAME component opened from the main window (+ Add Effect) draws correctly: opaque `MODAL_BG` well, scrim, border.

**Design-doc correction** — `EDITOR_WINDOW_UNIFICATION_DESIGN.md` §1's audit (row 6 / line ~9) asserted "the editor's `build()` populates `overlay_draw` exactly like the main window's" — this was false (only the main window ever called `UIRoot::build()`; the editor built its tree by hand each frame and never recorded overlays). Corrected in the doc itself as part of this fix.

### BUG-152 (ui-snapshot-render-graph-node-textures-arc-migration-miss) — `ui_snapshot/render.rs`'s `MetalBackend::new` call was missed by the BUG-054 `Arc<GpuDevice>` migration, breaking the `ui-snapshot` feature build — MED (build-breaking for the whole feature; zero default-sweep blast radius)
**Status:** FIXED (in the `feat/editor-window-unification` P1 diff, uncommitted at session end pending the BUG-151 escalation) — found 2026-07-14 during `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1, trying to run the `ui-snapshot`-feature test gate (`bug097_…`, `editor_window_harness`) that phase's brief requires. Unrelated to that design but small/mechanical/isolated to the harness, so fixed inline rather than just logged, to unblock verifying the actually-in-scope P1 work. **Fixed:** the two `let device = GpuDevice::new();` sites in `render_graph_editor_to_png`/`run_graph_preset`'s render fn that flow into `render_graph_node_textures` now wrap in `std::sync::Arc::new(...)`; `render_graph_node_textures` takes `&std::sync::Arc<GpuDevice>` and calls `std::sync::Arc::clone(device)` at the `MetalBackend::new` call site. Verified: `cargo check -p manifold-app --features ui-snapshot --tests` compiles clean; `cargo test -p manifold-app --features ui-snapshot --bin manifold` — 177 passed, 0 failed, 2 ignored.

**Symptom** — `cargo check -p manifold-app --features ui-snapshot` (with or without `--tests`) fails: `crates/manifold-app/src/ui_snapshot/render.rs:736`, `MetalBackend::new(device, GW, GH, GFMT)` — expected `Arc<GpuDevice>`, found `&GpuDevice`. Confirmed present on unmodified `origin/main` via `git stash` (not caused by the P1 diff). Blocks the entire `ui-snap` CLI (`cargo run --features ui-snapshot -- ui-snap ...`) and every `#[cfg(test)]` module gated on that feature, including `ui_snapshot::mod::overlay_fidelity_proof::bug097_render_sub_region_draws_root_excluding_overlay_that_render_tree_range_blanks` and `ui_snapshot::mod::editor_window_harness::node_the_fixture_places_renders_at_its_declared_screen_rect` — both load-bearing regression proofs that currently cannot run at all under `cargo nextest`/`cargo test` (they aren't `#[ignore]`d; the whole binary just fails to compile with the feature on).

**Root cause** — `d447ec8d` (BUG-054, "renderer-device-ptr-dangles — Arc<GpuDevice> replaces cached raw pointers") changed `MetalBackend::new`'s signature from `&GpuDevice` to `Arc<GpuDevice>` and updated every call site in the workspace (`rg 'MetalBackend::new\('` shows ~60 hits, all using `device.arc()` in tests or `Arc::clone(device)` in production code) except this one — the sole caller reachable only behind the `ui-snapshot` cargo feature, which the default `nextest`/`clippy` sweeps never build, so the migration's own gate never saw it.

**Fix shape** — `render_graph_node_textures(device: &GpuDevice, ...)` (`render.rs:673`) needs an owned/cloneable `Arc<GpuDevice>` to hand `MetalBackend::new`. Its sole caller, `render_graph_editor_to_png`, constructs `let device = GpuDevice::new();` (`render.rs:336`) and passes `&device` to several other calls (`UIRenderer::new`, `RenderTarget::new`, `composite_editor_frame`, this function) that only need `&GpuDevice` and would keep compiling via `&Arc<GpuDevice>`'s `Deref` coercion — so the minimal fix is likely: change that one `let device = GpuDevice::new();` to `let device = std::sync::Arc::new(GpuDevice::new());`, thread `Arc<GpuDevice>`/`&Arc<GpuDevice>` through `render_graph_node_textures`'s signature, and call `Arc::clone(device)` at line 736. Verify no OTHER caller of `render_graph_node_textures` exists with a differently-shaped `device` before committing to that shape (`rg` it — only one call site was seen at P1 time, `render.rs:420`, but re-check at fix time). Small, mechanical, but touches `manifold-app`'s ui-snapshot harness — do it as its own dedicated pass with the `ui-snapshot`-feature test gate actually green afterward as proof, not folded into an unrelated landing.

### BUG-150 (mute-chip-press-motion-teleports-hit-bounds-after-scroll) — the mute chip's press animation re-applies a stale build-time Y after the scroll fast-path moves rows, teleporting its draw + hit bounds off the row — HIGH (perform-surface control fails first click)
**Status:** FIXED @ `804ea043` — `tick_mute_motion`'s bounds write deleted, colour tween kept; `mute_base_y` and the now-orphaned `ChipMotion::press_offset_y` removed. Root-level rule applied class-wide (Peter, 2026-07-14): animations never move hit geometry, colour-only. Class audit found no other violation — `inspector.rs` card-drag ghost/indicator are non-interactive (`add_panel`, no `INTERACTIVE` flag); `param_card.rs`'s badge reposition and target-bar drag both re-derive from live layout each call, not a cached absolute; `interaction_overlay` lift/ghost offsets apply only to a per-frame draw scratch (`clip_rect_scratch`) that `hit_test_clip` never reads (hit-testing goes through `beat_to_pixel`, an independent path); `param_card`'s drawer-height tween is a genuine layout reveal — the app forces a full rebuild every animating frame, so downstream row positions are always freshly recomputed, never cached. Solo has no motion tick at all (no `solo_motion`, `solo_base_y`, or `tick_solo_motion` anywhere in `layer_header.rs`) — confirmed by reading `Self::new()` and `update()` in full: it never had this defect.

**Symptom** — clicking Mute (Peter reports Solo too) on the layer headers sometimes toggles in one click, sometimes requires clicking twice — the first click selecting the layer instead. Separately observed: the "M" chip visibly drifts off its row during a scroll. Show-stopper class: a mute that needs two clicks on stage.

**Root cause** — `tick_mute_motion` (`crates/manifold-ui/src/panels/layer_header.rs:1062`) writes the chip's bounds every animated frame as `mute_base_y[i] + press_offset_y`, where `mute_base_y` is captured ONLY at build time (`layer_header.rs:2373`). The scroll fast-path `try_update_vertical_scroll` (`layer_header.rs:2193`) is a set-only frame: it offsets every row node's bounds but never updates `mute_base_y`. First hover after a scroll starts the press/hover tween, which re-applies the stale Y — snapping the chip's bounds (shared by renderer and hit test) back by the full scroll delta. The click aimed at the visible M hits the row background → `LayerClicked` → selection → structural rebuild recaptures `mute_base_y` → next click works. Trace-confirmed (MANIFOLD_INPUT_TRACE probes, branch `probe/first-click-instrumentation`): consecutive clicks at (22,689) resolved to an unnamed row node firing `LayerClicked`, then to `layer_header.mute` firing `ToggleMute`. Solo has no motion tick — its reported involvement is unconfirmed; plausibly the displaced mute chip overlapping the solo position, or observation blur. Verify during the fix.

**Fix shape** — Peter's decision (2026-07-14), root-level for the whole class: **animations never move hit geometry; press/hover feedback is colour-only.** Delete the 1px `press_offset_y` bounds write from `tick_mute_motion` (keep the colour tween), drop `mute_base_y` entirely, and audit every other animation-driven `set_bounds` against the same rule — known sites: inspector drawer (`inspector.rs:1603/1637`), param_card (`param_card.rs:2907-2915/3906`), interaction_overlay lift/ghost (non-interactive drag visuals, likely exempt), browser/settings popup enter anims (being deleted anyway by the popup pass). Container reveals that genuinely relayout must re-derive positions from current layout per frame, never from a cached absolute.

### BUG-114 (draw-family-blocked-on-array-into-texture-codegen-read-path) — `draw_*` atoms pass the codegen-mandate scope test but the compiler can't express them — LOW (tracked codegen gap)
**Status:** FIXED — `docs/FUSION_SOTA_DESIGN.md` P4a+P4b+P5. P4a (`ae9ab74c`) built the
`InputAccess::BufferIndex` mechanism (classify variant, region-grow rule, standalone+fused codegen
struct synthesis from a port's `Channels[…]` layout, `buf_<port>` binding) and proved it on
`node.draw_dots`. P5 (`1b013b0e`) lifted the Vec3/Vec4/Color param gate the six atoms' `color`
param independently tripped. P4b converts the remaining five `draw_*` atoms
(`draw_markers`/`draw_ticks`/`draw_gauge`/`draw_scanlines`/`draw_connections`) + `blob_overlay` per
the ADDING_PRIMITIVES recipe (`wgsl_body` fragment + `fusion_kind`/`input_access` + generated-vs-hand
parity oracle), removing every `boundary_reason: Blocked`. `draw_connections` additionally proves
the BufferIndex mechanism generalizes to TWO tagged array inputs on one atom (`detections` +
`edges`) — P4a only exercised one. `draw_scanlines` needed no BufferIndex tag at all (no array
input) — it was purely gated by the Color param P5 lifted. Measured on `BlobTracking.json` (the
real HUD preset all six/seven atoms and their overlay chain live in): `graph-tool fusion` — before
18 nodes / 0 regions / 18 estimated dispatches; after 18 nodes / **1 region (6 members: both
draw_markers instances + draw_dots + draw_gauge + draw_ticks + draw_connections) / 13 estimated
dispatches**. `draw_scanlines` stays isolated in this preset (topologically separated from the HUD
chain by two `value_overlay` draw-call boundaries, not a param/array gap) — a genuine, expected
non-fusion, not a regression. `docs/node_catalog.json`/`NODE_CATALOG.md` and
`docs/fusion_census.md` regenerated (buffer-index-shaped family: 22→16 refusals, 12→10
dispatches-saved-if-lifted — the six/seven converted atoms leaving the bucket). Logged 2026-07-11
while sharpening the codegen-mandate scope test.

**Symptom** — the six `draw_*` atoms (draw_dots/markers/ticks/gauge/scanlines/connections) remain
plain-WGSL fusion boundaries despite being per-element in shape: each dispatches one thread per
OUTPUT PIXEL (`[w/16, h/16]`, e.g. draw_dots.rs ~161) and indexes a marks `Array` (blob
detections) inside the body — a gather, not a scatter. An overlay chain costs one dispatch per
atom where a fused run would cost ~1.

**Root cause** — a codegen capability gap, not an atom defect: texture-domain codegen has no
read-path for an input storage `Array`. Classify cut rule 9 (FREEZE_COMPILER_MAP §4) makes any
wired Array input on a texture atom a Boundary, and the buffer-region path requires no texture
output, so the shape fits neither. `freeze/classify.rs` names the needed kind — `BufferIndex`,
"read element i from a storage buffer" — as planned-but-not-built (additive: one codegen
read-path + one region-grow rule, per its own comments).

**Fix shape** — build the `BufferIndex` read-path for texture-domain bodies, then convert the six
atoms per the ADDING_PRIMITIVES recipe (wgsl_body + markers + `standalone_for_spec` + parity
oracle). Per the mandate's scope test #5 these are BLOCKED, not exempt — the debt lives in the
compiler. Severity LOW: each atom sits in exactly 1 shipped preset (overlay/HUD vocabulary), so
the unfused cost only bites in stacked per-pixel overlay chains.

### BUG-146 (render-scene-atom-pipelines-never-prewarmed) — a scene layer's first frame pays every atom's lazy codegen-pipeline compile (node.cube_mesh confirmed; likely every `primitive!` atom no bundled preset happens to exercise structurally) — LOW-MED (first-frame stall, not steady-state)
**Status:** FIXED (fusion-sweep worktree, this session) — option (b) from the original fix shape: a registry-wide "every atom prewarms its own codegen pipeline once, unconditionally" sweep, structural rather than atom-by-atom. Found 2026-07-13 as the residual left over after BUG-145's shaft/shadow prewarm fix, during VOLUMETRIC_LIGHT_DESIGN P3's `MANIFOLD_RENDER_TRACE` content-thread perf gate.

**Symptom** — with BUG-145's fix applied AND a control scene where every light has `cast_shadows: false` and `node.atmosphere`'s `shaft_intensity: 0` (so neither the shadow pass nor any shaft pipeline ever runs), the SAME two-pillar scene's frame 0 still measured ~41.5ms on the content thread — over the 20ms budget, with no shafts or shadow-casting lights anywhere in the graph.

**Root cause** — `GeneratorRegistry::prewarm_all` (`crates/manifold-renderer/src/generators/registry.rs`) only (a) builds every BUNDLED preset's graph *structure* via `PresetRuntime::from_json_str_with_device`, which never calls any node's `run()` (the comment there says so explicitly), and (b) as of BUG-037, explicitly prewarms `RenderScene::prewarm_pipelines` (now also covering BUG-145's shadow/shaft pipelines) + `GltfTextureSource::prewarm_pipeline`. Any atom whose GPU pipeline is compiled lazily inside its OWN `run()` (the `primitive!` codegen path's `self.pipeline.get_or_insert_with(...)`, e.g. `node.cube_mesh`'s `GenerateCubeMesh::run` at `generate_cube_mesh.rs:97`) is never touched by either mechanism unless some bundled preset's *rendered* first frame happens to hit it — and prewarm never renders a frame. `node.cube_mesh` is the confirmed example named in the original diagnosis; investigating it during the fix found neither DigitalPlants.json nor NestedCubes.json actually wires `node.cube_mesh` in yet (it's a decomposition building block, not currently reachable via any shipped preset's structure) — the ~41.5ms residual the symptom above measured is a SEPARATE scene hitting the same class of gap via other codegen-path atoms in that scene's graph, which is exactly why the fix generalizes rather than special-casing cube_mesh.

**Fix (landed this session)** — option (b): `prewarm_all_atom_codegen_pipelines` (`crates/manifold-renderer/src/generators/registry.rs`, called from `prewarm_all`) walks every `type_id` in `PrimitiveRegistry::with_builtin().known_type_ids()` (the same enumeration `freeze/classify.rs`'s meta-tests use), constructs a fresh instance, and compiles its standalone kernel via a new `codegen::standalone_for_node` (`node_graph/freeze/codegen.rs`) — a dynamic (type-erased) mirror of `standalone_for_spec::<Self>()`. This was possible with NO new trait method: every const `standalone_for_spec` needs (`WGSL_BODY`, `INPUTS`, `OUTPUTS`, `PARAMS`, `INPUT_ACCESS`, `DERIVED_UNIFORMS`, `WGSL_INCLUDES`, `ATOMIC_OUTPUTS`, `FUSION_KIND`, `STENCIL_FETCH`) was already exposed as a same-shaped `&dyn EffectNode` method by the existing blanket `impl<P: Primitive> EffectNode for P` (`primitive.rs`) — the trait already carried everything codegen needs, just behind dynamic dispatch instead of a compile-time type parameter. O(atom count) — 144 atoms — not O(bundled presets × render cost), and needs no GPU inputs/fixtures (`standalone_for_spec` only needs WGSL text + a `PrimitiveSpec`'s consts, never bound resources), so the "render one throwaway frame per preset" alternative was correctly avoided. Atoms with no `wgsl_body` (hand-written pipelines, `wgsl_compute`, `draw_*`/BUG-114) return `CodegenError::NoBody` and are skipped — nothing to prewarm there generically.

**Measured** (fresh, uncached `GpuDevice`, this session — direct pipeline-compile timing, not an end-to-end scene, since no shipped preset currently reaches `node.cube_mesh` to drive a MANIFOLD_RENDER_TRACE repro): `node.cube_mesh` alone compiles cold in ~12-15ms vs ~0.02-0.04ms once `prewarm_all_atom_codegen_pipelines` has run; the worst case of touching every one of the 144 codegen-path atoms cold in one frame (the shape of the original BUG-037/145/146 diagnosis) sums to ~1.0-1.1s cold vs ~1-2ms prewarmed — both comfortably under the 20ms/frame budget after the fix, both far over it before.

**Residual, not silently dropped** — `node.variable_blur` (`gaussian_blur_variable_width.rs`) is the sole atom declaring `wgsl_specialization` (`QUALITY_LEVEL`/`WEIGHTING_MODE` free identifiers in its generated text, resolved only by its own `run()` via `device.create_specialized_compute_pipeline` with live param values). Plain `standalone_for_node` + `create_compute_pipeline` fails a real naga parse on it (confirmed: `WGSL parse error: no definition in scope for identifier: WEIGHTING_MODE`) — the substitution values and their string encoding are genuinely bespoke per atom, not derivable from `PrimitiveSpec`'s const data the way everything else in this sweep is. Detected dynamically via `EffectNode::wgsl_specialization()` (non-empty) and skipped; stays a lazy first-use compile (up to 6 variants, `quality` × `weighting_mode`) same as before this fix. If a future atom adopts `wgsl_specialization`, it lands in this same skip bucket automatically.

**Test** — `generators::registry::gpu_tests::prewarm_populates_the_shared_cache_for_representative_converted_atoms` (`crates/manifold-renderer/src/generators/registry.rs`), scoped to one atom from each of this session's three conversion waves (`node.grid_mesh`, `node.shininess`, `node.rotate_coordinates`), same before/after + idempotent + cache-hit shape as the sibling BUG-037 tests. Written order-independent per BUG-144's documented cross-test-ordering hazard on the same shared, process-global test device.

### BUG-141 (import-graph-fused-region-linearize-depth-parse-fail) — glb import's fused region fails WGSL parse (`no definition in scope for identifier: linearize_depth`), card silently renders unfused — LOW-MED (perf, not visual)
**Status:** FIXED this session (fusion-sweep mechanical-sweep phase 1 worktree; lands with its commit) — same mechanism and fix as BUG-135 below: `generate_fused` (`freeze/codegen.rs`, the texture-domain multi-atom fusion path) now collects and prepends each region member's `node_includes` (deduped), mirroring the block `generate_fused_buffer` already had. Proven end-to-end by a new proof test, `coc_from_depth_fuses_with_pointwise_neighbor_and_matches_unfused` (`freeze/proof.rs`) — builds the real glb-import-shaped region (`node.coc_from_depth`, a `linearize_depth`-calling camera-derived atom, fused with a pointwise `node.invert`), asserts `fuse_canonical_def` no longer falls back to `None`, the fused kernel contains `fn linearize_depth`, and fused vs unfused render match within the out-of-loop tolerance. Verified the test reproduces the pre-fix symptom exactly (`fuse_canonical_def` panics with the fix reverted) before restoring the fix.

**Symptom** — Loading a project with an imported glb layer logs `[freeze] fused region 0 failed to parse: ParseError { message: "no definition in scope for identifier: linearize_depth" ... }`. The freeze install falls back to unfused execution for that card, so the import graph's per-element tail (SSAO chain / mix) pays N dispatches instead of a fused one. Output is correct — the fallback works as designed — but the fusion win is silently lost on exactly the heavy-scene cards that need it.

**Root cause** — confirmed: a fused region includes a node whose `wgsl_body` calls the shared `linearize_depth` helper (`depth_common.wgsl` — `node.coc_from_depth` and/or `node.ssao_gtao`, both declare `wgsl_includes: [DEPTH_COMMON]`) but `generate_fused`'s texture path never emitted `node.node_includes` into the generated kernel — see BUG-135's root-cause writeup below, same code.

**Fix shape (applied)** — see BUG-135.

### BUG-149 (glb-import-fog-slider-per-world-unit-cliff) — the importer's Fog Density slider maps 1:1 onto a per-world-unit density, so on real imports it's a cliff: light fog flattens the mesh grey and god rays blow out the frame — MED-HIGH
**Status:** FIXED @ `ee16c3b5` — found 2026-07-14, Peter live on `SceneLadders.manifold` (apricot-blossom glb, one session after `59010e84` added the sliders); fixed same session. **Fixed:** the card binding now scales the slider by `3.0 / framing_distance` (optical depth at the subject: 1.0 ≈ 95% fogged, 0.1 ≈ 26% haze), pinned by a test assertion against the `cam_dist` card default. Applies to NEW imports only — an already-imported card (e.g. the SceneLadders apricot) keeps its baked `scale: 1.0` binding until re-imported. Visual confirm on the rig owed by Peter.

**Symptom** — With the glb card's Fog Density at just 0.13, the whole model renders as a flat fog-grey silhouette (screenshot confirmed); adding God Rays 0.21 on top blows out the entire frame, empty sky included. Fog and shafts each work as designed — the control range is what's broken.

**Root cause** — Unit mismatch, in the importer, not the atmosphere node. `node.atmosphere`'s `fog_density` is per-world-unit (`1 − exp(−density · distance)` in `render_scene.wgsl`; the node's own composition notes call ~0.05 "a light haze over tens of units"). `gltf_import.rs`'s card wiring (`card_param("fog_density", …, 0.0, 1.0, …)` → atmosphere `fog_density`, scale 1.0) passes the 0–1 slider straight through, ignoring scene scale — even though the importer auto-frames the camera from mesh bounds and therefore *knows* the scale. On the apricot fixture (framing distance 27.87 units), slider 0.13 ⇒ optical depth ≈ 3.6 ⇒ 1 − e^−3.6 ≈ 97% fog: every fragment is ~pure fog color regardless of its depth, hence "flat grey mesh" (background stays black since fog only applies to geometry). The shaft march then accumulates in-scattering ∝ `fog_density × sun color × intensity` per step along every camera ray (`shaft_march.wgsl`), so with an effectively opaque air column and Sun Intensity 3.5, shaft slider 0.21 makes the whole volume glow — full-frame blowout. Usable Fog Density on this model is roughly 0.00–0.04; on a differently-scaled GLB it'd be some other unknowable sliver.

**Fix shape** — Normalize the fog slider by the importer's own framing scale: map slider → optical depth at the subject, i.e. `fog_density = slider_value / framing_distance` (or bounds radius), via the card mapping's scale factor at import time. Slider 0.5 then means "about half fogged at the subject" on any model — a perceptual fader that behaves the same on a bonsai and a cathedral, which is what a live card control has to be. God Rays needs no separate fix; it inherits sane density once fog is scaled. Consider the same normalization for the bundled CinematicScene preset if it exposes raw `fog_density` at fixed camera framing. Related: BUG-118 (fog saturation on bounded subjects — same `1/density` decay-length physics, different surface).

### BUG-145 (shaft-pipelines-not-in-prewarm-first-frame-cold-start-spike) — FIXED (shaft/shadow half); residual first-frame cost from an UNRELATED, broader gap now tracked as BUG-146 — was LOW-MED
**Status:** FIXED (this session, VOLUMETRIC_LIGHT_DESIGN P3) for the shaft/shadow-pipeline half of the original finding; the residual first-frame cost this entry also measured is a SEPARATE, pre-existing bug — see BUG-146, do not re-attribute it here.

**Symptom (as first found)** — `MANIFOLD_RENDER_TRACE=1` on a headless `ContentThread` running a scene with 4 shadow-casting Point lights and `shaft_quality` High (32 steps) printed exactly one trace line: `frame=0 total=79.9ms | generators=79.6 ...`. No other frame across a 120-frame run printed (the trace only fires above 20ms), so frames 1–119 were already under budget — a one-time first-frame cost, not a sustained regression.

**Root cause (fixed half)** — `RenderScene::ensure_shadow_pass` and `RenderScene::ensure_shaft_pipelines` (`crates/manifold-renderer/src/node_graph/primitives/render_scene.rs`) lazily compile the shadow depth-only pipeline and the shaft march/downsample/composite compute pipelines via `Option`-cache-on-first-use. `RenderScene::prewarm_pipelines` — the fix BUG-037 added specifically to close this class of gap for the MATERIAL render pipelines — did not call either, so the first frame with a shadow-casting light or `shaft_intensity > 0` paid the full compile cost for 4 shader pipelines synchronously on that frame.

**Fix (landed this session)** — `RenderScene::prewarm_pipelines` now also compiles the shadow depth-only pipeline and all 3 shaft pipelines (asset-independent, same fixed-source shape as the BUG-037 fix it extends). Measured before/after on the SAME scratch scene: **79.9ms → 42.0ms** on frame 0. `GeneratorRegistry::prewarm_all` already calls `RenderScene::prewarm_pipelines`, so no registry-level wiring was needed — the fix is entirely inside the one function BUG-037 already established as the extension point.

**Residual — do NOT re-fix here** — a CONTROL run of the identical scene with every light's `cast_shadows` forced to `false` and `shaft_intensity` forced to `0` (so neither pipeline this fix touches ever runs) STILL measured ~41.5ms on frame 0. That remaining cost predates this design and is unrelated to shafts/shadows specifically — tracked as **BUG-146**, its own root cause and fix shape.

**Verification trail** — measured via `crates/manifold-app/src/vol_light_p3_perf_verify.rs` (a scratch `journey-proofs` harness, same pattern as `bug035_verify.rs`/`bug037_verify.rs`, deleted before this session's landing — not a committed regression guard) against a scratch bundled preset (`VolLightP3PerfScratch.json`, also deleted before landing). Re-run recipe if this needs re-measuring: rebuild an equivalent scene (2 occluders, 4 Point lights all `cast_shadows: true`, `node.atmosphere` with `shaft_quality` High) as a headless `ContentThread` generator layer, call `RenderScene::prewarm_pipelines` (mirroring real startup) before ticking, and drive `MANIFOLD_RENDER_TRACE=1 cargo test -p manifold-app --features journey-proofs <test_name> -- --nocapture`.

### BUG-127 (decode-worker-silent-drop-wedges-export-flush) — missing-handle decode jobs get no reply, `decode_pending` never clears, export flush blocks forever — MED-HIGH
**Status:** FIXED @ `450f01c4` — missing-handle arms now reply `DecodeResultStatus::Error`, plus a bounded-wait backstop in `flush_pending_decodes`. Found 2026-07-12 during the MEDIA_EXPORT_MAP.md mapping pass (full read of `manifold-media`). See that map §12 for the pipeline context of BUG-127..133.

**Symptom** — `decode_scheduler.rs` worker arms for `Prepare`/`Seek`/`DecodeNext` are `if let Some(handle) = active.get_mut(&clip_id) { ... }` with no else — a job for a clip the worker doesn't hold (its `Open` failed on a missing/corrupt file, since `start_clip` inserts the `ActiveVideoClip` eagerly and submits `Open`+`Prepare` back-to-back) is dropped with no result sent. App-side `decode_pending` (set true at submit) then never clears. `VideoRenderer::flush_pending_decodes` loops on `recv_results_blocking()` until no clip is pending — and `ContentThread::export_one_frame` calls it every export frame (`content_export.rs:458`). Failed-open clip + one `Seek` (a scrub, a loop restart, export warmup) = the export loop wedges on the content thread with no way out (there is also no cancel-export UI). In live playback the same state just leaves the clip permanently black (no flush, so no hang).

**Root cause (known)** — the worker protocol has no "job refused" reply; the `decode_pending` invariant assumes every submitted job produces exactly one result.

**Fix shape** — make the missing-handle arms send `DecodeResultStatus::Error("no handle for <job>")` so the pending flag always resolves; consider a bounded wait in `flush_pending_decodes` as a second fence. Small, contained in `decode_scheduler.rs`.

### BUG-128 (sdr-video-export-gamma-diverges-from-display-and-stills) — export bakes `pow(1/2.2)`, display/stills use true sRGB — MED (release deliverable fidelity)
**Status:** FIXED @ `63937590` (encode side `b692bb9a`) — shared `manifold_srgb_encode`/`manifold_srgb_decode` in `ColorTransferFunctions.h`, ported literally from `still_exporter.rs`'s tested constants, now used by both the SDR export shader and the decoder's linearization. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass. Full transfer-function table: MEDIA_EXPORT_MAP.md §7.

**Symptom** — the SDR encoder copy shader (`MetalEncoderPlugin.m`, `kCopyShaderSDR`) applies a plain 2.2 power curve to the linear compositor output; the live display applies the true piecewise sRGB function at scanout, and `still_exporter::linear_f16_rgba_to_srgb8` applies the true function too. The same frame therefore has three subtly different tones — video export darkest in the shadows (2.2 vs sRGB diverge most below ~0.04 linear). The decoder's inverse (`pow(2.2)` in `MetalVideoDecoderPlugin.m`) makes video→export→import self-consistent, but everything video diverges from what Peter sees on stage and in stills.

**Root cause (known)** — approximate gamma chosen in both native shaders; stills got the correct function later (still_exporter is documented and tested against sRGB) and the video shaders were never aligned.

**Fix shape** — one shared sRGB OETF/EOTF definition used by both native shaders (piecewise, matching `still_exporter.rs`); EDR handling stays hard-clip unless the still exporter's rolloff is wanted for parity. Behavior change is subtle-but-visible: worth one before/after export Peter can eyeball.

### BUG-129 (export-fractional-fps-silently-rounds) — integer CMTime timebase mistimes 23.976/29.97 exports — LOW-MED
**Status:** FIXED @ `8a814c23` — Option A (Peter's call): exact rational timebase. `fps_to_rational()` maps f32 fps to (num, den), passed across FFI instead of a rounded int; `AVAssetWriter`'s track `mediaTimeScale` also had to be set explicitly (it silently re-rounded to 600 otherwise) — verified end-to-end with a real export + `ffprobe` showing `r_frame_rate=30000/1001`. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — `MetalEncoder_CreateInternal` stores `fpsNum = (int)(fps + 0.5)` and stamps frames `CMTimeMake(frameIndex, fpsNum)`. `ProjectSettings::frame_rate` accepts any f32 ≥ 1.0, so a 29.97 project exports at a 30 fps timebase: frame count is computed from the true fps (`ExportSession`), but presentation times use the rounded one — duration shrinks ~0.1% and the ffmpeg-muxed audio (correct wall-clock) drifts ~60 ms/min against picture.

**Fix shape** — rational timebase (`CMTimeMake(frameIndex * 1001, 30000)`-style, derived from the f32), or clamp/validate `frame_rate` to integers at the settings layer and say so in the UI. Decide which; don't leave the silent mismatch.

### BUG-130 (export-audio-mux-fails-late-and-leaks-temp) — ffmpeg resolved only at finalize; temp video left behind on mux failure — MED
**Status:** FIXED @ `2c829eaf` — `ffmpeg_preflight` runs before frame 0 when audio is present and aborts immediately if ffmpeg is missing; on mux failure the temp video is renamed to `<output>.video-only-audio-mux-failed.mp4` (preserved, not deleted) with the failure reason surfaced. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — `run_export` calls `AudioMuxer::resolve_ffmpeg` only in the finalize block, after every frame has been encoded: a machine without ffmpeg renders a full multi-minute export and then fails at the last step. Separately, `ExportSession::finalize` deletes the `<output>.video_only.mp4` intermediate only after a *successful* mux — on `MuxError` the temp stays on disk next to the (absent) final file.

**Root cause (known)** — fail-fast check missing at export start; cleanup only on the happy path.

**Fix shape** — resolve ffmpeg before frame 0 when `config.has_audio()` and abort with a clear error; on mux failure either delete the temp or (better for a failed long render) rename it to the output path with a "video-only, mux failed: <reason>" report so the render isn't lost. The second half is a product call — flag to Peter.

### BUG-131 (video-decode-hardcodes-bt709-video-range) — one YCbCr matrix for every source — LOW-MED
**Status:** FIXED @ `87427ec0` — reads `kCVImageBufferYCbCrMatrixKey` (via `CVBufferCopyAttachment`) to select 601/709/2020 coefficients per-frame, falling back to the SD/HD height convention when untagged; verified against ffmpeg-generated 601/709/2020-tagged fixtures in-session (instrumented smoke test, then removed as a one-off manual check, not a permanent gate). Full-range-vs-video-range sources remain an unverified secondary, unchanged from the original bug note. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — the NV12→RGBA shader in `MetalVideoDecoderPlugin.m` applies BT.709 video-range constants unconditionally. BT.601-tagged SD sources (old footage, some phone/web encodes) and BT.2020 sources get a visible hue/saturation shift (601-vs-709 green/magenta skew). The CVPixelBuffer's colorimetry attachments (`kCVImageBufferYCbCrMatrixKey` etc.) are never read. Unverified secondary: full-range sources — the reader requests video-range NV12, so VideoToolbox probably normalizes; confirm with a full-range fixture before trusting it.

**Fix shape** — read the attachments on the decoded buffer and pick 601/709/2020 constants (function-constant variants or a matrix uniform). A 601-tagged fixture clip is the proof.

### BUG-132 (video-decode-nearest-neighbor-scaling) — unfiltered scale in the convert shader — LOW-MED visual
**Status:** FIXED @ `2b3e15e1` — manual 4-tap bilinear blend (`bilinear_read_r`/`bilinear_read_rg`) replaces truncated-coordinate nearest-neighbor sampling on Y and CbCr planes independently. Code-verified (shader compiles, exercised by decoder tests); pixel-level before/after on a resolution-mismatched clip is **Peter-owed** (no GPU readback harness for this path in-crate). Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — the convert shader does the FitInside scale with `texture.read()` at a truncated source coordinate: nearest-neighbor. Any resolution mismatch between source and canvas (1080p file on a 4K canvas, 4K file on a 1080p canvas) gets blocky upscaling or shimmering downscaling — on the live rig's portrait towers most video content is resolution-mismatched, so this is the common case, not the edge.

**Fix shape** — bilinear: sample Y and CbCr planes with a linear sampler (or manual 4-tap around the fractional coordinate). One shader change; eyeball a mismatched-resolution clip before/after.

### BUG-133 (video-extension-list-overpromises-webm-avi) — import gate accepts what the decoder can't open — LOW
**Status:** FIXED @ `5711f65c` — Peter's call: extension list stays broad; the existing probe-failure path (previously `log::warn!` + silent skip) now routes through the same `alerts::error` dialog used for other import/save/load failures, naming the file and the codec problem. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — `metadata::SUPPORTED_EXTENSIONS = [".mp4", ".mov", ".webm", ".avi"]`, but decode is AVFoundation, which has no VP8/VP9 and patchy AVI support: the import gate accepts the file, then `VideoDecoder_Open`/probe fails per-clip later, surfacing as a mystery-black clip instead of an import-time rejection.

**Fix shape** — either trim the list to `.mp4`/`.mov` (honest), or keep the broader list and make `import_video_clip`'s existing probe failure reject the import with a "codec not supported" message (better). One-file change either way.

### BUG-143 (macros-panel-ableton-trim-drag-outside-p7-inventory) — `MacrosPanel`'s Ableton-range trim-bar drag is a hand-rolled sentinel machine, outside every P7 fold — LOW
**Status:** FIXED @ `d5ab1ae7` (UI_WIDGET_UNIFICATION P8, 2026-07-13) — `dragging_ableton_trim: i32` + `dragging_ableton_trim_is_min: bool` folded onto `DragController<AbletonTrimDrag>` (struct payload); pinning test green before and after; `manifold-ui --lib` 759/759; negative gate (`rg 'dragging_ableton_trim' crates/manifold-ui/src`) zero hits.

**Symptom** — none. The gesture (dragging a macro's Ableton min/max range trim bars, `handle_press`/`handle_drag`/`handle_release` in `panels/macros_panel.rs`) worked correctly before and after. This was a drag-lifecycle-unification gap, not a behavior bug.

**Root cause** — `dragging_ableton_trim: i32` (−1 = idle sentinel) + `dragging_ableton_trim_is_min: bool` (`macros_panel.rs:70-71`) was exactly the pre-P7.1 `ParamDragState` shape — a discriminant-by-sentinel plus a parallel bool, the disease D8 exists to kill. It was distinct from `MacrosPanel`'s own per-macro VALUE sliders (`self.sliders`, already `SliderDragState`/`DragController`-backed), so P7's original design-pass audit never surfaced it. Found only by P7.6's own closing `rg -n 'dragging'` inventory.

**Fix shape (as landed)** — `struct AbletonTrimDrag { index: usize, is_min: bool }` (chosen over an enum: the call sites already carry index and a min/max bool as separate values, so the struct converts one-for-one with no match-arm rewrites), `DragController<AbletonTrimDrag>` replacing the two fields, `handle_press`/`handle_drag`/`handle_release` updated to `start`/`payload`/`release`.

### BUG-111 — In-place inner-param edits on a fused SEGMENT card never reach the live kernel — MED
**Status:** FIXED @ `d73b3e36` — `EffectSlot::card_prefix` (`c{i}.` for a segment member, `""` otherwise) threads through a new `BoundGraph::apply_inner_overrides_prefixed`, translating both the `node_map` lookup (surviving nodes) and the `fused_retarget` lookup (fused-away nodes) into the segment's prefixed namespace; the segment slot's `bound.fused_retarget` is now also populated from `SegmentView::retarget` at build time. New gpu-proofs test `fused_segment_inner_override_reaches_live_kernel` (`preset_runtime.rs`) proves it on a real 2-card fused ColorGrade segment — independently reconfirmed red (`over=0/65536`) on the pre-fix code path and green with the fix restored.

**Root cause** — the fused-segment build path (preset_runtime.rs ~977–1064) builds one `node_map`
from the concatenated segment def, whose node ids carry the `c{i}.` per-card prefix
(`freeze::segment::card_prefix`). The per-frame in-place override (`run` → `apply_inner_overrides`,
preset_runtime.rs ~1863) passes each card's OWN def (`fx.graph`), whose node ids are UNPREFIXED.
So `apply_inner_param_overrides` misses every node: a surviving node `foo` is `c{i}.foo` in the
map, and a fused-away node isn't there at all. The BUG-006 retarget fix doesn't help — the
segment's `EffectSlot.bound.fused_retarget` is left empty (the segment retarget map is prefix-keyed
too), and even a prefixed retarget wouldn't cover the surviving-node miss.

**Symptom** — a value/position edit to a card that is part of a fused segment (multi-card fusion)
never lands in place; the old value keeps rendering until an unrelated rebuild. Same silent-stale
class as BUG-006 but scoped to segments. Narrower than BUG-006 (needs a multi-card segment that
fused), hence MED. Stateless-only cards today (segment eligibility), so no state-loss compounding.

**Fix shape** — populate the segment slot's `bound.fused_retarget` from the segment view's
prefix-keyed retarget with the `c{i}.` prefix normalized to the per-card def's key space, AND
translate surviving-node overrides by prefixing the def node id before the `node_map` lookup (or
apply overrides against a per-card-prefixed view of the def). Pair the two so both surviving and
fused-away nodes resolve. A focused test mirroring `inner_override_routes_fused_away_node_through_retarget`
but over a 2-card segment would pin it.

### BUG-054 (renderer-device-ptr-dangles) — renderers cache a raw `*const GpuDevice` that only `ContentThread::run()` repoints — MED (latent; every new headless/embedded consumer of ContentThread hits it)
**Status:** FIXED @ `d447ec8d` — `Arc<GpuDevice>` (approved sharing, device is internally synchronized, no `Arc<Mutex<_>>` introduced) replaces the cached raw pointer end-to-end: `ContentPipeline` and the UI-thread `GpuContext` own it from construction; every renderer clones it. Beyond the three renderers named above, `MetalBackend` also cached the same raw pointer and needed the same migration — threaded through `PresetRuntime`'s constructors, `GeneratorRegistry`, preset-thumbnail/graph-tool/freeze-profile/render-generator-preset bins, and the freeze GPU-parity test harness. `ContentThread::run()`'s repoint block and `journey_proof.rs`'s `rebind_gpu_device_pointers` workaround deleted — the invariant they existed to paper over no longer exists. Negative gate: `rg '\*const GpuDevice' crates/` — zero code hits (one doc-comment mention narrating the fix). Full workspace nextest (3250/3250), full workspace clippy, and the full `gpu-proofs` suite (1488/1490 — the 2 failures are BUG-144, a pre-existing order-dependent flake, confirmed unrelated) all independently reverified by the orchestrator, not just the worker.

**Found 2026-07-07 by the OFFLINE_AUDIO_REACTIVE_EXPORT P3 harness (first code path ever to
drive `run_export` outside the app's thread spawn).** `GeneratorRenderer` / `VideoRenderer` /
`ImageRenderer` cache a raw device pointer set at construction
(`generator_renderer.rs:126,180`); it dangles as soon as the owning `ContentPipeline` moves.
The running app is safe only because `ContentThread::run()` repoints every renderer once,
after all moves are complete (`content_thread.rs:300-328`) — a load-bearing, undocumented
ordering invariant. Any new consumer (headless export/journey harness, future preview
contexts, tests) that constructs a `ContentThread` and calls methods without replicating that
exact repoint gets an ObjC nil-receiver panic or a straight segfault, as the P3 build did
twice before finding the correct point. **Workaround shipped:** `journey_proof.rs`
`rebind_gpu_device_pointers` runs after the struct reaches its final binding — correct but a
second copy of the invariant. **Fix shape (root):** remove the self-referential raw pointer —
either pass `&GpuDevice` per render call (renderers already receive per-call context), or
hold the device behind a stable heap indirection owned above the pipeline so moves can't
invalidate it. Blast radius: renderer call signatures; no behavior change. Until then, any
brief that constructs `ContentThread` outside `Application::resumed()` must name the repoint
step.

### BUG-123 (mesh-edges-capacity-vs-active-count) — node.mesh_edges emits edges for the full buffer capacity, not the loaded vertex count — LOW visual artifact, tracked v1 limitation
**Status:** FIXED @ `1b854d45` — added an optional `active_count` scalar input (port-shadow, mirrors `node.range`'s convention) that overrides the buffer-capacity-derived vertex count when wired; unwired graphs are unaffected. 5 new tests in `edges_from_mesh.rs`.

**Symptom** — When a `gltf_mesh_source` feeding `node.mesh_edges` has `max_capacity` larger than the asset's actual flat-vertex count, the zero-filled buffer tail produces degenerate edges that draw as a bright dot artifact at vertex 0's projected position in `draw_lines`.

**Root cause (known)** — Array wires carry buffer capacity but `mesh_edges` has no runtime active count; it derives edge count from `buffer.size / size_of::<MeshVertex>()`. Presets work around it by sizing `max_capacity` exactly to the asset (BlossomWire: 9210 for the confetti cut).

**Fix shape** — An optional `active_count` scalar input on `mesh_edges` mirroring `node.range`, or a mesh-wire active-count convention (curve/particle families already have one). Small, isolated.

### BUG-079 (missing-preset-fails-silently-no-onscreen-signal) — an unresolvable preset def degrades safely but with no on-screen signal — LOW
**Status:** FIXED @ `834fdaa6` — reuses the BUG-063 P3 `LoadReport`/"opened with repairs" toast mechanism: an unresolved preset template now adds to the same load-time report instead of only an `eprintln`, so it surfaces in the existing non-blocking toast. No new notification mechanism. Covered by `crates/manifold-io/tests/load_report.rs`.

Loading a project that references an unresolvable preset def (deleted, unregistered, or missing on this machine) degrades *safely but silently*: saved params are kept on a placeholder (keep-don't-drop, `effects.rs:940`) and the effect falls back to **source passthrough** (`preset_runtime.rs:808`) — but the ONLY signal is a console `eprintln`; nothing shows on screen. A performer sees the layer render without its effect (a missing *generator* layer likely renders empty — inferred, unconfirmed) with no visible reason. **Fix shape:** surface unresolvable presets in-app (a card/badge or a load-time notice).

### BUG-038 (ableton-log-spam) — AbletonBridge retries + WARN-spams every ~1.5s forever when Live isn't running — LOW (log hygiene)
**Status:** FIXED @ `06bfd879` — warns once on first OSC-send failure, downgrades repeats to DEBUG, logs a single INFO "reconnected" on the next success. Throttle decision is a pure `note_send_outcome` state machine, unit-tested.

**Symptom** — any session without Ableton running logs
`[AbletonBridge] OSC send failed for /live/song/get/num_tracks: Connection refused` at
WARN level every ~1.5s indefinitely (see any 2026-07-06 trace-run log).

**Fix shape** — warn once on first failure, then downgrade repeats to debug until a send
succeeds (state flip logs "reconnected" at info). Optionally back off the poll while
refused. `manifold-playback/src/ableton_bridge.rs`, small.

### BUG-086 (recording-audio-track-under-covers-duration-on-longer-takes) — the recorded audio track can silently fall short of the intended duration on longer takes, no counter, root cause unknown — MED
**Status:** FIXED

**FIXED 2026-07-13 (recording-sync lane).** Diagnosis protocol (per the brief): added
counters first, ran the paced 2-minute 1080p soak 3x. All 3 measured `audio_frames_dropped
= 0` while duration was full-length — falsifying the native backpressure gate as this bug's
cause (consistent with the 2026-07-11 observation below, now confirmed with a clean signal).
Checked the named suspects in order with ffprobe: before reaching AAC priming/fragment-flush
discriminators, instrumented `recording_soak.rs`'s OWN synthetic-audio pusher
(`push_realtime_audio_chunk`) by making `push_audio_chunk` return `ringbuf::Producer::push_slice`'s
actual accepted count instead of discarding it — and found the real defect immediately: the
bounded `HeapRb` (~5s capacity) the soak bin pushes into can transiently fill under unpaced/
encoder-stress timing bursts, and the OLD code advanced its `pushed_frames` bookkeeping by the
INTENDED push amount regardless of what the ring actually accepted, so an overflow was silently
discarded rather than retried next call — a harness-side loss, not a native-encoder one.
Root-fixed by tracking the real accepted count (self-heals: the next call's `to_push` naturally
includes whatever didn't fit). Verified directly: 3x paced 2-min 1080p soaks measured
`audio_duration_s` at 120.0087s / 120.0102s / 120.0115s (<0.01% off, intended 120.0s), two
paced 1-min soaks (720p/1080p) measured 60.0038s / 60.0102s, and the previously-reliable
unpaced/encoder-stress 2-min repro now measures 120.0007s — full coverage, no shortfall,
across three separate reruns. `LiveRecordingPlugin.m`'s `WriteAudioSamples` backpressure gate
was ALSO hardened while investigating (bounded spin-wait matching the video path, `LR_OK` never
returned on a drop — a real defect under the session's class rule, landed together with
BUG-085, though it turned out not to be this bug's cause). `docs/LIVE_RECORDING_PROOFS_DESIGN.md`
doesn't need a status change (P1/P2 already SHIPPED; this is a post-ship bug-fix pass, not a
phase).

Found 2026-07-10 building `LIVE_RECORDING_PROOFS` P2's `recording-soak` self-check gate
(`crates/manifold-recording/src/bin/recording_soak.rs`). Sequence: the soak bin's synthetic
audio was originally paced one chunk per video frame-loop iteration (media-time-locked); an
unpaced 4K/1080p run compresses many minutes of "media time" into a few seconds of wall time,
which triggered the native audio input's real-time backpressure gate
([`LiveRecordingPlugin.m:546-547`](../crates/manifold-recording/native/LiveRecordingPlugin.m#L546):
`if (!state->audioInput.isReadyForMoreMediaData) return LR_OK; // drop samples rather than
block`) and lost ~91% of the audio (10.8s decoded out of an intended 120.0s) — worse than
BUG-085's video path in one respect: this returns `LR_OK` (success) on drop and never logs
anything, so there isn't even a warning, let alone a counter. Root-fixed the soak's OWN pacing
(decoupled audio production from the video loop, paced to real wall-clock time instead, plus a
post-loop real-time catch-up phase — matches how production audio actually arrives, from a
real-time CoreAudio callback, never frame-coupled) which recovered the overwhelming majority of
the loss (10.8s → 118.4s of 120.0s). A **residual, VARIABLE shortfall remains and is
unexplained**: three repeated 2-minute 1920x1080 unpaced runs measured `audio_duration_s` at
116.0s, 118.4s, and 118.5s against an intended 120.0s (1.3%-3.3% short, run to run — not a
stable fixed percentage), while two independent 1-minute runs (1280x720 and 1920x1080) both
measured exactly 60.0s — so whatever's causing it is **duration-dependent, not
resolution-dependent**, onset is somewhere between 60s and 120s of continuous writing, and the
magnitude varies (possibly with system load/contention — not isolated). Ruled out: a "still
queued in the ring buffer at `stop()`" race — inserting a 500ms settle delay before calling
`session.stop()` changed the measured shortfall by <0.1s, so the loss is happening *during* the
run, not at shutdown. **Fix shape:** unknown without native-side instrumentation (out of P2's
scope — proof-harness/soak-authoring work, not native FFI investigation). Suspects: sustained
real-time backpressure that only manifests past some duration/data-volume threshold (disk I/O
contention as the fragmented MOV grows, fragment-flush cadence interacting with the audio
append queue, or AAC's own internal encoder buffering not being fully flushed by the periodic
drain before a threshold is crossed). Wire an `appendedFrameCount` counter for audio analogous
to BUG-085's video-side fix, or add NSLog-level visibility on the `LR_OK`-drop path at minimum,
so this stops being silent. Given the observed variance, the soak's own audio-coverage check
(`recording_soak.rs`, "Audio coverage sanity gate" comment) does NOT gate PASS/FAIL on a tight
tolerance — a made-up number would misrepresent confidence that doesn't exist yet — it gates
only a coarse 50% floor (catches a genuine collapse like the original 91%-loss defect) and
prints a non-gating stderr warning past 2% short, naming this bug. **Unknown whether this loss
scales worse over a full 20-minute take** — Peter's first full-scale soak run (P2's Deferred
item, per design §6 P2) will be the first real data point at show scale; if the shortfall grows
materially at that scale, this bug's severity should be revisited upward.

**Orchestrator disambiguation 2026-07-10 (Opus, at P2 landing):** ran the soak in `--realtime`
mode (submissions paced to wall clock — the true show proxy) for 2 minutes at 1920×1080 on an
idle machine: `audio 120.0s` exactly, full coverage, versus `audio 118.6s` on the same-size
unpaced run moments earlier. This is strong evidence the shortfall is an **unpaced-stress-mode
artifact**, not a show-path defect: unpaced video encodes at 100% duty and the synthetic-audio
catch-up floods the native audio input's real-time gate, which cannot happen in a real 60fps
show where audio arrives wall-clock-paced from CoreAudio (exactly what `--realtime` replicates).
Severity for the SHOW path is therefore LOW; the bug is real but lives in the soak's unpaced
audio-feed pacing under encoder saturation. **Still worth the silent-drop fix** (the `LR_OK`-on-drop
path with no counter/log is the actual defect worth removing, per BUG-085's sibling shape).
Peter's first full-scale 20-minute run remains the confirming data point, but the show-relevance
concern is now much reduced.

**Observation 2026-07-11 (Lane C, wave2 export/recording sweep):** the silent-drop fix named
above landed as an instrument — `LiveRecordingPlugin.m`'s `WriteAudioSamples` now counts and
NSLogs every sample-frame it drops on the `isReadyForMoreMediaData` backpressure gate (an atomic
`audioFramesDropped`, read live via `LiveRecorder_GetAudioFramesDropped` /
`LiveRecordingSession::audio_frames_dropped()`, surfaced end-to-end through `ContentState`
(`recording_dropped_audio_frames`) onto the layer-header Record button, and printed by
`recording_soak` next to its existing audio-coverage check). Ran a real unpaced 2-minute
1920×1080 soak (`recording-soak --width 1920 --height 1080 --fps 60 --minutes 2`, the same shape
as the original repro): `audio_frames_dropped = 0` while `audio_duration_s` still measured
118.8s against the intended 120.0s (1.2s / ~1.0% short — inside this run's own non-gating 2%
warning threshold, so no WARNING printed, but still the same class of shortfall this bug tracks).
**This is a real data point, not a fix**: the native backpressure gate reported zero drops on a
run that still fell short, so for THIS run the gate is ruled out as the cause — the shortfall is
happening somewhere the counter can't see (consistent with the standing suspects: AAC encoder
internal buffering, fragment-flush cadence, or disk I/O contention, none of which the backpressure
gate would catch). Only one run was captured this session (time-boxed); the counter is now in
place for whoever runs the confirming full-scale 20-minute soak to check whether it ever fires
at show scale.

### BUG-085 (recording-frames-recorded-overstates-async-append-drops) — `frames_recorded` can overstate the file's real packet count under sustained backpressure — MED accounting / LOW practical likelihood
**Status:** FIXED

**FIXED 2026-07-13 (recording-sync lane).** `frames_recorded` no longer accumulates from
`LiveRecorder_EncodeVideoFrame`'s synchronous `LR_OK` return — that return only proves the
frame was queued for the async `appendPixelBuffer:` call, not that it landed. Native: a new
`videoFramesAppendDropped` atomic counter (+ `LiveRecorder_GetVideoFramesAppendDropped`,
mirroring the existing audio counter) now counts every way the async append can fail
(backpressure, writer not Writing at append time, `appendPixelBuffer:` returning NO, or an
Objective-C exception) — all previously silent. Rust: `recording_thread::run` polls that
counter (before `Finalize`, which frees native state) and uses `LiveRecorder_Finalize`'s
return value — the native `videoFramesAppended` ground truth, read only after the append
queue is fully drained — for `frames_recorded`, instead of the untrustworthy synchronous
tally. `run()` now returns a `RecordingStats { frames_recorded, frames_sync_failed,
video_append_dropped }`; `LiveRecordingSession::stop()` sums every drop source into
`RecordingResult.frames_dropped` (session-level pool/channel drops + native sync failures +
native async-append drops), so `frames_recorded + frames_dropped` always equals frames
submitted, and no path reports success on a drop (the class rule this bug and BUG-086 share).
`pool_accounting_consistent`'s forced-backpressure test tightened from `pts.len() <=
frames_recorded` to exact equality plus `assert!(frames_dropped > 0)`; green across 3
consecutive runs.

Found 2026-07-10 building `LIVE_RECORDING_PROOFS` P1's `pool_accounting_consistent` test
(`crates/manifold-recording/tests/recording_proofs.rs`), during a bounded-retry-recovery variant
that deliberately holds pool slots un-released to simulate a slow encoder. `session.stop()`
reported `frames_recorded: 107`; the file the harness's independent ffprobe oracle actually
opened had 106 video packets. Root cause, in
[`LiveRecordingPlugin.m`](../crates/manifold-recording/native/LiveRecordingPlugin.m) around
line 490: `LiveRecorder_EncodeVideoFrame` returns `LR_OK` (line 519) as soon as the *synchronous*
GPU blit into the CVPixelBuffer finishes — but the actual `[adaptor appendPixelBuffer:...]` call
happens **later, asynchronously**, on `state->appendQueue` (`dispatch_async`, lines 490-516).
Inside that async block, if `videoIn.isReadyForMoreMediaData` is false at the moment it runs
(real VideoToolbox backpressure), the frame is silently dropped —
`NSLog(@"[LiveRecorder] VideoToolbox backpressure — dropped frame at %.3fs", ...)` — with **no
counter incremented anywhere Rust can see**. Rust's `frames_encoded` (→
`RecordingResult::frames_recorded`) only reflects the synchronous return value, so it can never
observe this drop. The container file itself stays completely valid (PTS strictly monotonic,
no corruption) — this is purely an accounting gap: a post-set "N frames recorded" readout could
overstate the truth by however many frames VideoToolbox silently dropped under backpressure.
**Fix shape:** wire `atomic_int* appendedCounter` (already tracked at line 489, incremented at
line 500 on real success) back out through the FFI — e.g. a `LiveRecorder_AppendedCount(handle)`
query at `stop()`/finalize time, or have `LiveRecorder_Finalize`'s return value report the true
appended count instead of (or alongside) the synchronous-call count — and have
`LiveRecordingSession::stop()` prefer it. **Practical severity is LOW**: this needs genuinely
sustained `isReadyForMoreMediaData == false` backpressure, which the harness's artificial
fence-holding produces on purpose but a real 60fps show submission rate is very unlikely to
sustain (VideoToolbox's ProRes proxy encode is comfortably faster than realtime at these
resolutions). No `#[ignore]`-able regression test yet — `pool_accounting_consistent`'s current
gate (`frames_recorded + frames_dropped == frames_submitted_total`, tracked entirely
Rust-side) is internally consistent and doesn't touch this gap; a future test would need to
assert `probe(file).pts.len() <= frames_recorded` under intentional backpressure instead.

### BUG-138 — `node.variable_blur` fixed tap count looks blocky at large CoC radius — FIXED 2026-07-13 (CINEMATIC_POST DoF)
**Status:** FIXED @ 8659c11a (2026-07-13, Sonnet 5, `dof-polish` worktree, branch `feat/dof-polish`)

**Root cause** — the Gaussian kernel is a fixed 9/17/25 taps (`QUALITY_LEVEL` specialization,
`gaussian_blur_variable_width_body.wgsl`), but tap *spacing* scales directly with the per-pixel
CoC radius (same `step_size` line as BUG-137). At a large blur radius — e.g. `CinematicScene`'s
`DoF Blur Radius` card at 64px — the fixed tap count spreads across a wide span with visible gaps
between the actual samples, so heavily out-of-focus areas render as discrete rings rather than a
smooth blur, instead of the graceful falloff a real lens produces.

**Fix shape** — P4 (`node.bokeh_gather`, already designed in `CINEMATIC_POST_DESIGN.md` D5, a
true 32-tap 2D disc gather rather than a sparse separable 9/17/25-tap kernel) will likely reduce
this substantially just by construction, though it hasn't been built or verified against this
specific symptom. It will NOT fix BUG-137's dilation/bleeding gap on its own — that's a separate
mechanism. If P4 alone doesn't resolve the blockiness, the fallback is scaling tap count with
radius rather than holding it fixed. **Demoted to secondary 2026-07-13:** P4 is escalated to the
DoF root fix (CINEMATIC_POST status amendment) and `CinematicScene` stops using the gaussian pair
once it lands — the tap-scaling fix then only matters for the still-user-wireable
`variable_blur` atom itself, at the dof-polish lane's tail.

**2026-07-13 update (P4 landed):** `CinematicScene` now runs `node.bokeh_gather` (true 32-tap 2D
disc gather, `crates/manifold-renderer/src/node_graph/primitives/bokeh_gather.rs`) in place of the
two `variable_blur` H/V nodes this bug names — the `variable_blur` atom itself is untouched and
still ships/wireable elsewhere, only the preset swap happened. Whether the blockiness this bug
describes is actually resolved is a look-pass question, not a gate question (the numeric gate
proves the atom matches its own committed D5 spec, not that it looks better than the old kernel)
— look-pass waived by Peter 2026-07-16 (verification-debt burn-down, same pass as BUG-137's); if the blockiness shows on a real scene it comes back as a fresh sighting.

**2026-07-13 fix (dof-polish lane tail, `node.variable_blur` atom itself):** Built the literal
fallback named above — tap count now scales with the per-pixel CoC radius instead of holding
fixed. `gaussian_blur_variable_width_body.wgsl` and its hand parity oracle
`gaussian_blur_variable_width.wgsl` both changed identically: each of the fixed 9/17/25 logical
taps now densifies into up to 4 evenly-spaced sub-samples filling the gap back toward the previous
tap (`vbw_subtap_count(step_size)`, weight split evenly across the sub-samples) once `step_size`
exceeds an 8px threshold; below that threshold — including the documented `max_radius = 6.0` DoF
parity setting (`step_size` ≤ 7px there) — the kernel is byte-identical to the original
single-sample-per-tap arithmetic, so the "matches legacy DoF blur byte-for-byte" claim in
`composition_notes` still holds. At the bug's own 64px repro (High quality), effective tap count
goes from the old fixed 25 to 97 (25 → 4× density, capped). **§2.5-equivalent audit finding worth
recording:** `node.gaussian_blur` (`separable_gaussian_body.wgsl`'s `sg_blur_linear`) already ships
an adaptive-radius, runtime-loop, analytically-weighted Gaussian on the same codegen path — a
different (fancier) shape than the literal "scale tap count with radius" fix asked for here; it
was surveyed and deliberately NOT reused wholesale because `variable_blur` carries per-tap CoC
weighting (`WEIGHTING_MODE`/`coc_weight`) that `sg_blur_linear` doesn't have, and re-deriving
per-tap Gaussian weights analytically would have been the "fancier invented algorithm" the task
explicitly said to avoid — the fixed-table + sub-sample-densification shape stays closest to the
bug's own literal fix note. Cost tradeoff stated explicitly (not hidden): worst-case per-pixel tap
count is capped at 4× the original (SUBTAP_CAP=4) — this reduces but does not fully eliminate
visible banding at extreme radii (65px spacing → ~16px worst-case gap); a full elimination would
need a disc-gather redesign at `variable_blur`'s own granularity, which is exactly what
`bokeh_gather` already is for `CinematicScene` and is out of scope for this atom-level fix. Low
tiers (Low/Medium quality) are unaffected at their own default radii — the fix only escalates cost
when `step_size` is genuinely large, never degrades a tier to cheapen another. Gate green: 6 new
+existing unit tests (`gaussian_blur_variable_width.rs` `tests` module, including 3 new BUG-138
numeric proofs), `generated_gaussian_blur_variable_width_matches_original` (I4 generated-vs-hand
parity) green at the new algorithm, `fused_variable_width_blur_matches_unfused` green, full
`manifold-renderer --features gpu-proofs` sweep green, `cargo nextest run -p manifold-renderer
--lib` 1153 passed, scoped clippy clean, `check-presets` 57/57 (unchanged param shape —
`DepthOfField.json`, the remaining user-facing preset using `node.variable_blur`, still loads with
its existing defaults). No PNG look-pass required this phase (numeric-gated atom, not wired into
any gated demo chain per the CINEMATIC_POST precedent) — `variable_blur` is no longer in
`CinematicScene`'s chain (moved to `bokeh_gather`), so there is no look-pass gate for this atom.

### BUG-137 — `node.variable_blur` has no CoC dilation; hard cutoff at depth discontinuities — MED (CINEMATIC_POST DoF)
**Status:** FIXED 2026-07-13; confirmation waived by Peter 2026-07-16 (verification-debt burn-down, VD-020-CINEMATIC — reopen as a fresh sighting if the seam shows live) — `node.coc_dilate` (fixed 3x3 neighborhood-max, `crates/manifold-renderer/src/node_graph/primitives/coc_dilate.rs`) built and wired into `CinematicScene` (`coc_from_depth.out -> coc_dilate.in -> bokeh_gather.width`, replacing the direct `coc_from_depth -> variable_blur` wires, then re-pointed at `bokeh_gather` when P4 landed the same session) 2026-07-13 (Sonnet 5, `dof-polish` worktree, branch `feat/dof-polish`). Gate green (I1 generated-vs-hand parity + flat-field no-op gpu_tests, full `manifold-renderer --features gpu-proofs` sweep, focused nextest, scoped clippy clean, `check_presets` 57/57, I5 load-smoke). Orchestrator PNG look-pass confirmed the silhouette-bleed halo is visibly gone post-fix (see the note below) — but `CinematicScene`'s test geometry (one flat mesh) has no real foreground/background depth split, so Peter's own look at a richer scene (`SceneLadders.manifold` or similar) is still the real exit per the amended demo rule.

**Root cause** — `node.variable_blur` picks its per-pixel gather radius from *only the center
pixel's own* CoC (`step_size = center_coc * max_radius + 1.0`,
`gaussian_blur_variable_width_body.wgsl:77`). There is no dilation / max-CoC pre-pass, so a
heavily-blurred pixel never borrows a wider radius from a neighboring high-CoC pixel, and a sharp
pixel is never bled into by an adjacent blurred one. At any depth discontinuity — the silhouette
of an in-focus subject against a blurred background, or vice versa — this produces a hard seam
right at the edge instead of a soft transition. Peter's description: "like the blur is applied to
a plane." Confirmed `CinematicScene` runs `weighting_mode: 0` (plain averaging, every neighbor
weighted equally) — the CoC-comparison step function (`coc_weight()`, same file) isn't even
active in the shipped preset, so the hard edge is purely the missing dilation, not the weighting
mode.

**Fix shape** — add a CoC-dilation atom: spread the maximum CoC found in a small neighborhood
(e.g. one tile) outward before the two `variable_blur` passes consume it — the standard technique
used by most real-time DoF implementations to get soft depth-edge blending from an otherwise
naive per-pixel-radius gather. New primitive, `Gather`-family input access, CPU-reference
testable like every other atom this wave. **Scoping decision COMMITTED 2026-07-13 (Fable, design
session): a standalone atom (`node.coc_dilate`, neighborhood max of the CoC texture) — folding a
neighborhood read into `coc_from_depth` would change that atom's Pointwise fusion classification
and cost its fusability, so the fold option is dead.** The dilated CoC feeds whichever gather
consumes it: the shipped `variable_blur` pair today, `node.bokeh_gather` after CINEMATIC_POST P4
(which needs the dilation equally — P4 does not make this bug obsolete).

**2026-07-13 update (P4 landed):** `node.bokeh_gather` (`crates/manifold-renderer/src/node_graph/
primitives/bokeh_gather.rs`) built and swapped into `CinematicScene` in place of the two
`variable_blur` H/V nodes, still reading `coc_dilate`'s dilated output (`coc_from_depth.out ->
coc_dilate.in -> bokeh_gather.width`, `coc_from_depth`/`coc_dilate` wires unchanged from the
BUG-137 fix above) — per this same note's own prediction, P4 does not obsolete the dilation, and
the wiring confirms it still feeds the gather. Gate green (I1 generated-vs-CPU-reference +
generated-vs-hand parity, I2 zero-CoC pass-through, full `manifold-renderer --features gpu-proofs`
sweep 1463 passed, focused nextest 1150 passed, scoped clippy clean, `check_presets` 57/57, I5
load-smoke `bundled_cinematic_scene_loads_and_compiles`).

**Orchestrator PNG look-pass (2026-07-13, Sonnet 5):** rendered `CinematicScene` before/after via
`render-generator-preset` (1280x720, 90 frames) at the wire state immediately before vs. after the
`bokeh_gather` swap. Visible difference: the pre-swap render shows a soft glow/halo bleeding
outward from the plane's silhouette into the black background; the post-swap render's silhouette
is crisp with no bleed — consistent with D5's occlusion-aware `step()` weighting suppressing
cross-edge contribution. This is a real, looked-at improvement, but the test scene (`CinematicScene`'s
single flat mesh) has no foreground/background depth split, so it does not exercise BUG-137's
literal "in-focus subject against blurred background" seam scenario end-to-end. **Status downgraded
to FIXED, pending Peter's own confirmation on a richer scene** (same posture as BUG-119 in the
`scene-ladder-state` memory) rather than closed outright.

### BUG-139 (bug-status-rebuild-drops-fixed-pointer-lines) — bug_status.py's parse() mis-bucketed the ## Fixed archive-pointer lines (rebuild drop + false check noise) — FIXED 2026-07-13
**Status:** FIXED (2026-07-13) — pointers are now a first-class parse() bucket (never entry body, never strays); rebuild() re-emits them after the resolved entries under ## Fixed; write() grew a pointer-fidelity guard that refuses to write if any pointer line would change. check()'s archive cross-check reads the pointer bucket, killing the ~78 false "no ## Fixed pointer" warnings per landing. Regression tests: .claude/hooks/test_bug_status.py (both shapes: pointers with and without a leading full entry). log_bug.py's splice-not-rebuild rationale updated; its own insert path was already safe.

**Symptom:** Running 'bug_status.py --write' in a worktree reconstructs docs/BUG_BACKLOG.md via rebuild(head, entries, tail), which only re-emits parsed ### BUG-NNN Entry objects. The one-line closed-bug pointers under ## Fixed (e.g. '- BUG-078 (slug) — FIXED ... — full history in docs/archive/BUG_BACKLOG_CLOSED.md') are not Entry objects — parse() buckets them as 'strays' — and rebuild() never re-inserts strays. A --write run would silently delete all ~74 archive-pointer lines from the live file, breaking the archive cross-check bug_status.py --check itself performs. write() does print a stderr note listing dropped stray lines, so it is not fully silent, but it reads as a low-priority note rather than a content-loss warning, and nothing stops the write from completing.
**Root cause:** parse() classifies every non-blank, non-heading, non-'## ' line between ## Open and the next appendix heading as either an Entry body line (if under a ### heading) or a stray. The ## Fixed pointer lines match neither pattern (POINTER_RE recognizes them for the archive cross-check in check(), but rebuild() never consults POINTER_RE or the strays list at all).
**Second symptom (2026-07-13, widget-unification landing):** the same mis-bucketing makes check() print ~78 false "`archived ... but has no ## Fixed pointer here`" lines at every landing. With a full `### BUG-NNN` entry at the top of ## Fixed (BUG-140 currently), parse()'s block loop swallows the entire pointer list that follows it into that entry's *body* (it collects until the next ### heading), so `pointer_ids(strays)` sees zero pointers despite all of them existing in the file. Same root cause, opposite bucket. Side effect: while swallowed into a body, the pointers survive `--write` (verified no-op 2026-07-13) — the content-loss half above only fires when ## Fixed has no leading full entry. Fixing parse() to classify POINTER_RE lines as pointers wherever they appear kills both symptoms.
**Fix shape:** Have rebuild() re-emit the pointer strays verbatim in their original relative position within ## Fixed (sort resolved Entries and pointer strays together, or just append all Fixed-section strays after the resolved Entries, matching current file order). Add a regression test: parse a fixture with a resolved Entry + a pointer stray under ## Fixed, run write(), assert the pointer line survives.

### BUG-140 (glb-import-non-square-aspect-distortion) — imported glb scenes rendered at the envmap's 1024×1024 and stretched to canvas — FIXED 2026-07-12
**Status:** FIXED (2026-07-12) — root cause was the plan compiler, not the import path. Probe-confirmed (runtime eprintln): render_scene's color intermediate was allocated at the envmap's fixed **1024×1024** while the final target was project res — the scene rendered square then got stretch-sampled to canvas by the SSAO mix (distortion + ~8× resolution loss at 4K). Cause: `ExecutionPlan`'s default output-dims policy is "max of texture input dims", which is wrong for rasterizers whose texture inputs are scene resources (envmap, base-color maps), not screen buffers; the import graph is the only graph wiring concrete-dims textures into a render node, which is why only imports broke and Tesseract/basic shapes were clean. Fix: producer-declared `output_canvas_scale` now takes priority over the max-of-inputs heuristic in plan build (execution_plan.rs), and render_scene / render_3d_mesh / render_instanced_3d_mesh declare `(1, 1)` (canvas-sized outputs). Verified by Peter in-app at 1440v and 3840×2160. Residual class risk (recorded decision, not fixed): the max-of-inputs default is still a trap for any FUTURE rasterizer-style node that forgets the declaration — the deeper fix is a port-level screen-image vs resource distinction so the planner only sizes from screen inputs; belongs with the codegen/primitive-contract work. ADDING_PRIMITIVES carries the authoring rule.

**Symptom:** Peter screenshot of an imported glb scene (flower photogrammetry scan) shows the reference color-checker chart rendered visibly stretched/non-square. Follow-up observation (same day): landscape project squashes vertically, portrait squishes horizontally — i.e. the render ignores output aspect.
**Root cause:** see Status — plan dims policy sized the rasterizer's output from its resource-texture inputs.
**Fix shape:** shipped as above.

### BUG-126 (manifold-renderer-tests-clippy-debt-under-gpu-proofs) — 12 pre-existing clippy findings in `manifold-renderer`'s test code, only visible under `--tests --features gpu-proofs` — LOW, found not fixed 2026-07-12 (CINEMATIC_POST P0 fusion-layer session)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — same fix as BUG-124 (identical 12 findings, one fix covers both entries); see BUG-124's Status line for the fix shape, the 5 extra findings caught by the same gate re-run, and verification commands.
**Symptom:** `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` fails with 12 errors; the STANDARD gate (`cargo clippy -p manifold-renderer -- -D warnings`, no `--tests`, no feature) is clean and is what every prior session's gate ran, so this debt accumulated unnoticed.
**Root cause:** ordinary `needless_range_loop` / `manual_range_contains` / `identity_op` clippy lints in `#[cfg(test)]`/`gpu_tests` modules that only compile under `--tests --features gpu-proofs` — never exercised by the standard scoped clippy invocation.
**Sites:** `primitives/bend_mesh.rs:526`, `primitives/facet_normals.rs:324`, `primitives/gltf_mesh_source.rs:434`, `primitives/morph_mesh.rs:489`, `primitives/push_along_normals.rs:527,559`, `primitives/scatter_on_mesh.rs:551-553`, `primitives/taper_mesh.rs:512`, `primitives/twist_mesh.rs:505`, `primitives/revolve_curve.rs:334` — none touched by this session's D7/P0 diff (verified via `git status`/`git diff --stat`); found only because this session additionally ran `--tests --features gpu-proofs` clippy beyond its specified gate.
**Fix shape:** mechanical — rewrite each flagged loop to an iterator (`enumerate()`/`skip()`/`take()`) or `RangeInclusive::contains`; no behavior change. Low priority (lint-only, no correctness impact); worth a dedicated pass rather than folding into an unrelated phase's diff.

### BUG-135 (fused-texture-codegen-drops-wgsl-includes) — the FUSED multi-atom texture region codegen (`generate_fused` in `freeze/codegen.rs`) never emits a member's `wgsl_includes` — LOW, found not fixed 2026-07-12 (CINEMATIC_POST P1, `coc_from_depth` session)
**Status:** FIXED this session (fusion-sweep mechanical-sweep phase 1 worktree; lands with its commit).
**Symptom:** a texture-domain Pointwise atom that declares `wgsl_includes` (e.g. `node.coc_from_depth`, whose body calls `depth_common.wgsl`'s `linearize_depth`) and lands in a FUSED region with a pointwise neighbour would generate a kernel missing the shared helper's definition, failing naga parse — the install-time "fused kernel fails naga" refusal (`FREEZE_COMPILER_MAP.md` §4) makes this fail CLOSED (falls back to unfused, still renders correctly), so it is a missed-fusion perf gap, not a correctness bug. Confirmed to be BUG-141's exact root cause.
**Root cause:** `RegionNode.node_includes` (`freeze/codegen.rs:1270`, populated at `freeze/install.rs:1256` via `node.wgsl_includes()`) is read by `generate_fused_buffer` (`freeze/codegen.rs:1591`, `for inc in node.node_includes`) but NEVER read by `generate_fused` (the texture-region path, `freeze/codegen.rs:2084`–2628) — asymmetric with the STANDALONE texture path, which this same P1 session fixed (`generate_standalone_ext` gained an `includes: &[&str]` parameter, threaded from `standalone_for_spec` via `P::WGSL_INCLUDES`; see the fix's own commit). The FUSED texture path's `prelude`/`helpers`/`bodies` (`split_fns`-based) emission never merges in `node.node_includes` at all.
**Why not fixed at the time:** `coc_from_depth`'s neighbors in `CinematicScene` (P1's only consumer at the time) never formed a fusable region with it — its upstream `depth` input came from `node.render_scene` (always `Boundary`, a draw call) and its downstream consumer `node.variable_blur` read it via a Gather wire (gather-consumed wires never union per the union gates), so `coc_from_depth` was always an isolated single-node region there. BUG-141's glb-import graph hit a different topology where the fused region actually forms, surfacing the gap.
**Fix (applied):** mirrored `generate_fused_buffer`'s dedup-and-prepend `includes: Vec<&'static str>` loop inside `generate_fused`'s texture path (`crates/manifold-renderer/src/node_graph/freeze/codegen.rs`) — collects `node.node_includes` across `region.nodes` in the existing per-node loop (dedup by value), prepends the joined text right before the shared-prelude emission (same relative position `generate_fused_buffer` uses, before body emission). Two new tests: `codegen.rs::gpu_tests::fused_texture_region_carries_and_dedups_wgsl_includes` (unit-level: fuses `node.coc_from_depth` + `Gain`, asserts `fn linearize_depth` appears exactly once and the kernel parses through naga) and `freeze/proof.rs::coc_from_depth_fuses_with_pointwise_neighbor_and_matches_unfused` (end-to-end: the real glb-import-shaped region — `coc_from_depth` fused with `node.invert` — asserts `fuse_canonical_def` no longer falls back to `None`, both D7/P0 markers are present, and fused vs unfused render match within the out-of-loop tolerance). Both tests were verified to fail with the fix reverted (reproducing the exact pre-fix symptom: naga's "no definition in scope for identifier: linearize_depth" / `fuse_canonical_def` panicking) before the fix was restored.

### BUG-162 (ui-snapshot-feature-canonical-def-arc-regression) — `--features ui-snapshot` doesn't compile: `GraphView::canonical_def` changed to `Arc<EffectGraphDef>`, 8 call sites in `ui_snapshot/mod.rs` never updated — LOW (build-only, no runtime impact)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — found as a blocker while investigating BUG-153 (needed a working `ui-snap` binary to reproduce). `cargo build --features ui-snapshot --bin manifold` failed with 8 `E0308` mismatches: `render::render_graph_to_png`/`render_graph_editor_to_png` expect `&EffectGraphDef` but received `view.canonical_def` (now an `Arc<EffectGraphDef>` per an unrelated session's change — `crates/manifold-app/src/ui_snapshot/mod.rs` was untouched by that session, confirmed via `git log`), and `AddSceneObjectCommand::new`/`AddSceneLightCommand::new` expect an owned `EffectGraphDef` but received `view.canonical_def.clone()` (an `Arc` clone, not a value clone). Fixed all 8 sites: 6 call sites now pass `&view.canonical_def` (auto-deref through the `Arc`), 2 now pass `(*view.canonical_def).clone()` (deref then clone to get an owned value). Mechanical, no behavior change once it compiles. Verify: `cargo build --features ui-snapshot --bin manifold` clean; `cargo run --features ui-snapshot --bin manifold -- ui-snap inspector` runs and writes a PNG.

### BUG-163 (amg-livery-black-body-carpaint-extension-and-texture-cap) — AMG GT3 body renders black: livery/base-color lives in an unmapped carpaint extension material and 14 textures drop over the per-object cap — MEDIUM (hero-asset fidelity)
**Status:** FIXED 2026-07-15 (GLB_CONFORMANCE_DESIGN G-P2, `909976d2`) — confirmed by the orchestrating session rendering the real AMG fixture through the landed code: `render-import` on `mercedes-amg_gt3__www.vecarz.com.glb` now reports `material_count: 78, object_count: 78, textures_wired: 39` (was `29` with `dropped_over_cap: 14`), and the rendered PNG shows the correct silver/NASA livery on the body panels — no black panels. `render_scene`'s clamp rose from `OBJECT_SLIDER_MAX = 64` to a real 1024 safety bound; `ImportReport.dropped_over_cap` is gone; the importer wires every material 1:1.

**Symptom:** `mercedes-amg_gt3__www.vecarz.com.glb` imports with `material_count: 78, textures_wired: 29, dropped_over_cap: 14` and report lines including `EXT_Carpaint_Inst: KHR…` — the body panels render pure black while rims/glass/interior read correctly. The store-page silver NASA livery never appears.

**Root cause (diagnosed, not fixed; corrected same day after reading the glb's JSON directly):** the "carpaint extension" framing was wrong — "EXT_Carpaint_Inst" is just the material's NAME. Its actual extension is standard `KHR_materials_clearcoat` (clearcoatFactor 1.0), which only adds gloss and is already IMPORT_FIDELITY Deferred #1 (this asset was predicted to fire that trigger). The livery/base color is an ORDINARY `baseColorTexture` (image index 3) in the core material — so the black body is caused by `dropped_over_cap: 14`: the per-object texture-wire cap drops 14 maps on this 78-material asset, and the body's base-color map is evidently among them.

**Fix shape:** primary — revisit the texture cap for many-material assets: raise it, or prioritize base-color maps when rationing wires (importer-side only; gets the silver car). Secondary, separate trigger — Deferred #1's clearcoat lobe for the paint's lacquer gloss (shader work, priced in the design doc).

### BUG-164 (material-maps-force-one-repeat-sampler-ignores-per-texture-wrap) — every material map samples through ONE hardcoded REPEAT sampler; a glTF texture's own wrap/filter settings (CLAMP_TO_EDGE, MIRRORED_REPEAT, NEAREST) are parsed but never reach the GPU sampler — LOW (found via the glb conformance harness, not yet judged against a hero asset)
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P4, D3) — per-map-family samplers (5 bindings replacing 1) each built from the glTF texture's own wrapS/wrapT + min/mag filter; `TextureSettingsTest.glb` flipped to expect_pass.

### BUG-165 (boombox-multi-texture-never-converges) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P1)
**Status:** FIXED — root cause was NOT the texture-decode/wiring hypothesis this entry originally carried. Diagnosed via a new `--trace` flag on `render-import` (prints non-black fraction + `io_pending` every frame, not just after a stable streak): `io_pending` goes `false` by frame 1 and the frame stays byte-stable-black from then on — ruling out both prior hypotheses (a decode race, and an ORM-texture wiring bug; `textures_wired: 1` counting only base-color is a report-line quirk, not a bug). The real cause: `node.orbit_camera`'s `near` clip plane defaulted to a fixed `0.05` (`camera_orbit.rs`), never scaled to the framed object's size. The importer already scales `distance = 2.2 * radius` to the object's own bounding-sphere radius, so the object's front face sits at `distance - radius == 1.2 * radius` from the camera — BoomBox's radius (0.0172, a real-world-meters-scale asset) put its front face at 0.0206, inside the fixed 0.05 near plane, so every frame clipped the entire object to black.

**Fix:** `gltf_import.rs` now computes `near_clip = DEFAULT_NEAR.min((distance - radius).max(1e-4) * 0.5)` and wires it onto the synthesized camera's `near` param (`DEFAULT_NEAR` is now a `pub const` on `camera_orbit.rs`, single source of truth shared with its own `ParamDef` default). The `.min(DEFAULT_NEAR)` cap means every asset whose front face already clears the old fixed default gets the IDENTICAL near value as before — verified empirically across all 58 then-`expect_pass` golden-checked assets: only Avocado (radius 0.0404), Corset (0.0399), and PotOfCoals (0.0617) besides the two bugs' own assets had their near value change at all, and their re-renders differ from the committed goldens by mean_abs ~1.5e-5 (rounding noise, unmeasurable) — no regression. `BoomBox.glb` now renders correctly (converges frame 4, non-black fraction 0.1299) and is `expect_pass` with a golden (`goldens/boombox.png`).

**Distinct finding:** `VirtualCity.glb` carried the same `xfail:BUG-165` note ("same never-converges class") but does NOT share this root cause — after the fix it still never converges after 60 frames with `io_pending=false` (167 materials/147 textures, a genuine separate throughput issue). Its manifest note now says so explicitly; it needs its own diagnosis, not a re-run of this fix.

**Symptom:** `docs/GLB_CONFORMANCE_DESIGN.md`'s own audit (§1) already names the mechanism: material maps sample via the dedicated REPEAT `material_sampler` (binding 22, landed `85b5bb9d` same day as the wrap-smear fix) — deliberately, to fix the striped-helmet out-of-range-UV bug. But that fix is a single global sampler, not a per-texture one: any glTF asset whose texture explicitly declares `CLAMP_TO_EDGE` or `MIRRORED_REPEAT` (or a non-default min/mag filter) has that declaration silently ignored — every map is forced to REPEAT regardless. Khronos `TextureSettingsTest.glb` renders non-degenerate (`render-import` produces a populated grid, no crash, non-black fraction 0.18) but its whole design exercises exactly this axis (clamp-s/clamp-t/repeat-s/repeat-t/mirror-s/mirror-t cells), so its correctness could not be verified without a fragment-level ground truth this session didn't build. Classified `xfail:BUG-164` in the conformance manifest rather than `expect_pass`, pending a real fix or a verified-safe reading of the render.

**Root cause:** unknown/not fully investigated this session — the `material_sampler` binding is provably one GPU sampler object shared by every map (`render_scene.rs`, landed `85b5bb9d`); confirming the *consequence* (which specific TextureSettingsTest cells actually render wrong) needs either a per-cell pixel comparison or reading `render_scene.wgsl`'s resolve path end to end, neither done here.

**Fix shape:** likely a per-texture (or per-material, since a group owns one texture set) sampler keyed by the glTF's own wrap/filter fields, rather than the single shared `material_sampler` — mirrors the anisotropy field's shape (D7 in GLB_CONFORMANCE_DESIGN.md): read wrap/filter off `gltf_load`'s parsed sampler, thread it through `node.gltf_texture_source`/`node.pbr_material`, and build (or select from a small pool of) samplers keyed by the resolved `(wrap_u, wrap_v, min, mag)` tuple at draw time.

### BUG-167 (spec-gloss-pbrspecularglossiness-entirely-unhandled) — `KHR_materials_pbrSpecularGlossiness` (the legacy spec-gloss workflow) is not parsed at all — falls back to a default material with no diffuse/specular/glossiness mapped
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P3, D2) — `KHR_materials_pbrSpecularGlossiness` converts to metal-rough at import time; `SpecGlossVsMetalRough.glb` flipped to expect_pass. Known named gap: the specularGlossinessTexture's RGB specular tint stays Deferred (needs a shader change), so the asset's two halves render close but not pixel-alike — documented in the conformance manifest note, not silently claimed as full parity.

**Root cause:** not investigated — `rg "pbrSpecularGlossiness|SpecularGlossiness"` across `gltf_import.rs`/`gltf_load.rs` returns zero hits; the extension is simply never read. Not in `GLB_CONFORMANCE_DESIGN.md` D5's scoped extension list and not covered by any §7 deferred item (D5 names sheen/iridescence/anisotropy-the-extension/volume/Draco/KTX2/meshopt as the deferred set; spec-gloss isn't in that list — a genuinely new gap this session's audit surfaced).

**Fix shape:** parse `KHR_materials_pbrSpecularGlossiness`'s `diffuseFactor`/`diffuseTexture`/`specularFactor`/`glossinessFactor`/`specularGlossinessTexture` and either convert to the existing metallic-roughness port set at import time (the common approach — diffuse≈baseColor, invert glossiness→roughness) or add a dedicated spec-gloss shading path. Low priority: legacy extension, one asset in the whole Khronos suite.

### BUG-168 (ext-mesh-gpu-instancing-unhandled) — `EXT_mesh_gpu_instancing` nodes import as "no materials with geometry — nothing to import", not as N instanced copies
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P4, D6) — `EXT_mesh_gpu_instancing` expands to N wired copies at summary time (raw-JSON sniff, no typed crate support); `SimpleInstancing.glb` (125 instances) flipped to expect_pass. True GPU-instanced rendering stays Deferred.

**Symptom:** `render-import` on `SimpleInstancing.glb` fails with the same "no materials with geometry" error `gltf_import.rs` emits when a glb genuinely has zero material-bearing primitives — but this asset does have a mesh with a material; the geometry is expressed as per-instance attributes (`EXT_mesh_gpu_instancing`'s `attributes.TRANSLATION/ROTATION/SCALE`) on a node that owns no vertex data of its own, which the importer's material/geometry summary walk apparently doesn't recognize as geometry-bearing.

**Root cause:** not investigated. `rg "mesh_gpu_instancing|gpu_instancing"` returns zero hits in the importer — the extension is unparsed.

**Fix shape:** read `node.extensions.EXT_mesh_gpu_instancing.attributes`, and for each instance either emit N copies of the node's ports (if the graph node budget allows) or add per-object instancing support to the relevant primitive. Not scoped to any G-P phase; one asset in the suite.

### BUG-169 (metalroughspheresnotextures-renders-fully-black) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P1)
**Status:** FIXED — root cause was NOT a texture-less-material/lighting bug, despite the prior "ruled out camera framing" note (that ruling used `--orbit 3.0`, which changes orbit angle, not distance/near — it never actually tested the near-clip axis). This is the exact same mechanism as BUG-165: `node.orbit_camera`'s fixed `near = 0.05` clip plane exceeds this asset's front-face distance. `MetalRoughSpheresNoTextures.glb`'s bounding radius is 0.0056 (this texture-less variant is authored at a dramatically smaller scale than its textured sibling `MetalRoughSpheres.glb`, radius 6.99 — the two "same spheres" assets are not actually the same scale), giving a front face at `distance - radius = 0.0067`, deep inside the 0.05 near plane. Confirmed via `--trace`: `io_pending=false` from frame 0 (there's nothing to decode, as this entry originally noted) and the frame is black from frame 0 — a pure clip-plane bug, no decode or lighting involved.

**Fix:** shared with BUG-165 — see that entry for the `near_clip` formula, the no-regression proof across the other 58 `expect_pass` assets, and `gltf_import.rs`. `MetalRoughSpheresNoTextures.glb` now renders correctly (converges frame 4, non-black fraction 0.1488, visibly matches its textured sibling's metal/roughness gradient grid) and is `expect_pass` with a golden (`goldens/metal_rough_spheres_no_textures.png`).

**Lesson for future bug-hunts:** "ruled out camera framing" needs to specify WHICH camera parameter was varied — orbit angle and clip-plane distance are both "camera framing" in casual language but only one of them was actually tested here, and it was the wrong one.

### BUG-171 (boxvertexcolors-no-material-primitive-skipped-entirely) — a mesh primitive with vertex colors but no material index (spec-legal — implies the glTF default material) is skipped entirely, not imported with a default material
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P3, D4) — synthetic default-material entry (sentinel `material_index = u32::MAX`) added for materialless geometry; `BoxVertexColors.glb` now imports (previously errored) and flipped to expect_pass. Known named gap: COLOR_0 (per-vertex color) itself is still never read anywhere in the mesh pipeline — the box renders flat gray, not with its vertex colors — escalated to its own entry, BUG-177.

**Root cause:** not investigated. `gltf_import.rs`'s geometry-summary walk (the "no materials with geometry" error path, `gltf_import.rs:385`) appears to require every counted primitive to carry an explicit `material` index; a primitive that omits it (relying on glTF's implicit default material, spec-legal) contributes no geometry to the summary, so an asset built entirely of such primitives imports as empty.

**Fix shape:** treat a materialless primitive as using glTF's default material (base fallback PBR values) rather than excluding it from the geometry summary — likely a small addition next to the existing `default_material_vertex_count` report-line path (`gltf_import.rs:416`), which already tracks materialless *sub*-geometry within an otherwise-material asset but apparently doesn't cover an asset that is *entirely* materialless primitives.

### BUG-172 (recursiveskeletons-no-default-scene-rejected) — a glb with no `scene` index (spec-legal — importer should fall back to all root nodes) is rejected outright
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN P2, D5) — `resolve_import_nodes()` (`gltf_load.rs`) replaces all 3 `document.default_scene().ok_or_else(...)` sites: default scene if present, else the union of every `scenes[]` entry's nodes (de-duplicated by node index), else every parentless node in the document. `RecursiveSkeletons.glb` now imports and renders non-black (converges frame 4, fraction 0.0764). Originally found 2026-07-15 during GLB_CONFORMANCE_DESIGN G-P7 full-suite classification (`RecursiveSkeletons.glb`: `render-import` fails "glb has no default scene").

**Root cause:** not investigated — the importer requires `document.default_scene()` (or equivalent) to resolve, and errors rather than falling back to importing every root-level node when the glTF omits the top-level `scene` field (legal per spec: absence just means "no default scene is suggested," not "there is no content").

**Fix shape:** when no default scene is present, fall back to unioning all nodes referenced by any `scenes[]` entry (or, if `scenes` is also empty, all nodes with no parent) rather than erroring. Low priority: RecursiveSkeletons is a skinning stress-test asset (also out of scope per deferred item 7), and no-default-scene is a rare authoring choice.

### BUG-181 (import-ao-mix-flattens-alpha) — imported GLB layers composite fully opaque: the AO group's `node.mix` replaces the scene's alpha with the AO map's alpha=1, so the black void hides every layer below
**Status:** FIXED 2026-07-16 (same day) — option (a) shipped: `node.mix`'s non-Lerp blend modes now pass input `a`'s alpha through untouched (`mix.wgsl` + `mix_body.wgsl`, mode-conditional; Lerp keeps its full crossfade). Preset sweep confirmed nothing relied on the old alpha-lerp in a non-Lerp mode (43 instances checked; FilmGrain's Overlay had the same latent defect and is fixed by the same change; Lightning terminates in `set_alpha` so unaffected). Gate: new value-level alpha gpu_test + generated-vs-hand parity + full alpha-contract sweep + generator smoke, all green. Peter's visual confirm (plasma visible under the skull GLB void, contact AO intact) still owed.

**Symptom:** an imported GLB generator layer blacks out everything beneath it in the compositor, even though `render_scene` correctly clears its void to transparent `(0,0,0,0)` (`manifold-gpu` `encoder.rs` MSAA pass clear) and its lit shaders leave alpha untouched (alpha contract honored). The compositor's Normal blend is a correct premultiplied over (`compositor_blend_compute.wgsl` case 0) — with the scene's real alpha the layers below would show — but by the time the graph output reaches it, alpha is 1.0 across the whole frame.

**Root cause (confirmed by reading the full chain):** the import rig's spine is `render_scene → ao group → final` (`gltf_import.rs` ~1239–1349; the same AO group CinematicScene ships per CINEMATIC_POST D9, so that preset has the identical defect). Inside the group: `ssao_gtao` writes its AO map as `vec4(ao, ao, ao, 1.0)` (`ssao_gtao_body.wgsl:219` — legitimate for a data texture), the bilateral blurs preserve that alpha, then `node.mix` (Multiply, `amount = 1.0`) computes `out_a = mix(a.a, b.a, amount) = b.a = 1.0` (`mix.wgsl:96`) — the scene's alpha is replaced wholesale by the AO data texture's filler alpha. This is the alpha-contract violation class `alpha_contract.rs` guards against, but it arises from *wiring* (display texture × data texture through mix's lerp-alpha semantics), not from any single primitive, so the sweep can't see it.

**Fix shape:** AO modulation must preserve the display input's alpha. Root options: (a) make `node.mix`'s non-Lerp blend modes pass `a`'s alpha through (the shader already documents "blend modes are RGB-only" — lerping in `b`'s alpha during a Multiply was never meaningful), or (b) route AO through an RGB-only modulate path. Prefer (a) — it fixes the class (import rigs, CinematicScene, any user graph multiplying a data map onto a display chain); before changing semantics, sweep existing presets for anything relying on mix's alpha-lerp at amount=1, and re-run the fusion parity proofs for `mix_body.wgsl`. Instrument-level consequence while open: an imported GLB scene can't be layered over anything — it only works as the bottom layer of a stack.

### BUG-266 (inspector-tab-pin-dies-on-incidental-selection-change) — tab scope pin was versioned one-shot; command side effects silently cleared it → snap back to Layer default
**Status:** FIXED 2026-07-20 (W1-C lane, `fcd4c084`, merged `43c9d3d1`).
**Severity:** MEDIUM — user-visible: "tabs change randomly, especially when adding effects."
**Root cause:** `pin_scope` recorded `(tab, selection_version)`; any version bump (incl. add-effect side effects) killed the pin; state_sync then fell back to Layer default. Second path: pin filtered against per-sync recomputed tab set.
**Fix:** pin keyed to selection IDENTITY `(primary_selected_layer_id, primary_selected_clip_id, selected_layer_ids)`; `pinned_scope()` is a pure read — holds on equal identity or transient-empty, yields None on genuine change. state_sync's existing tab-set filter already gave keep-pin-while-absent semantics. 3 regression tests (`bug_266_tab_pin`) on the real sync path; red-before-fix shown for 2 (the third was already-correct behavior, kept as guard).
**Residue:** because the pin is a pure read and storage isn't cleared on a genuine change, re-selecting the ORIGINAL identity later resurrects the pinned tab. Flagged for Peter's in-app feel-pass; trivial to tighten if it feels wrong on stage.

### BUG-267 (inspector-duplicated-card-lists) — `master_effects` / `layer_effects` were two parallel `Vec<ParamCardPanel>` with duplicated match arms at every touchpoint
**Status:** FIXED 2026-07-20 (W1-D lane, `717f8910`, merged `726de5a0`).
**Severity:** MEDIUM (structural debt, standing bug class) — every card behavior had to be written twice; "fixed for Master, forgot Layer."
**Root cause:** two storage fields with hand-mirrored match arms at ~127 sites in `inspector.rs` (cards_for_tab, find_drag_handle, selection sets, skip_to_settled, press routing, …); Layer/Group/Clip all aliased `layer_effects`.
**Fix:** one `effects: [Vec<ParamCardPanel>; 2]` indexed by `scope_idx(tab)` (Layer/Group/Clip canonicalize to SCOPE_LAYER); `cards_for_tab(_mut)` is the single accessor path; `configure_master_effects`/`configure_layer_effects` public signatures unchanged (state_sync untouched). Pure refactor, existing card test suite as parity oracle (assertions unchanged, 3828 green on merged tree). Note: `ProjectSettings.master_effects` hits elsewhere in the workspace are an unrelated data-model field, not debt from this fix. Cleared the ground for BUG-265 (tree-bounds drag hit-testing, Wave 2).
