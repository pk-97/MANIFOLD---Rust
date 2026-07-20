# Param Storage Redesign — Handoff Brief for Fable

**Status:** CLOSED 2026-07-05 — the design was written and approved:
`docs/PARAM_STORAGE_DESIGN.md` (id-keyed per-instance param manifest; goes
further than §7's sketch — registry demoted to a template, one-time migration
deletes the positional arms). Execute from the design doc, not from this brief;
this stays as the grounded census behind it.

Original status: brief for a SOTA redesign. Not a design doc — this is the
grounded input Fable turns into one, following `docs/DESIGN_DOC_STANDARD.md` +
`docs/DESIGN_BUILD_ORDER.md`.

**Author:** Opus (investigation session 2026-07-05), after fixing the immediate
misroute bug (`e226be46`).

**One-line problem:** a parameter's runtime identity is its *integer position*
in a flat array, when it already has a stable string id. That single choice
spawns a class of "two lists must stay in sync" bugs that has bitten repeatedly.

---

## 1. The triggering bug (already fixed — read as the canonical failure)

A driver / LFO on a card slider that *survived* a node deletion got applied to
the **neighbouring** slider. Fixed at `resolve_param_in`
(`crates/manifold-core/src/effects.rs`, the generator branch) in commit
`e226be46`, with a regression test
(`effects::tests::bundled_slider_delete_does_not_misroute_survivor_drivers`).

Root cause: **two authorities for "param_id → slot index" that disagreed after
an edit.**

- The card display and the delete/prune path use `param_id_to_value_index`
  (`effects.rs:2142`), which for a graph-backed generator reads the **live**
  `graph.preset_metadata.params`.
- The runtime modulation resolver `resolve_param_in` (`effects.rs:188`),
  consumed every frame by the driver/envelope/audio-mod evaluators in
  `manifold-playback`, resolved the static prefix through the **frozen
  registry** `def.id_to_index`. The registry def is captured when the preset is
  installed (e.g. a glb auto-import) and never tracks a per-instance node
  delete, so its indices go stale the instant a bundled slider is removed. glb
  imports make *every* card slider a bundled (`user_added: false`) param, so all
  of them live in that stale prefix.

The one-line fix aligned the two resolvers. It does **not** address the disease:
there are still *three* resolvers that must be kept in lockstep by hand
(`param_id_to_value_index`, `resolve_param_in`, `static_param_count`), plus a
renderer-side mirror. This brief is about deleting that whole class.

---

## 2. The disease: positional identity

`PresetInstance.param_values: Vec<ParamSlot>` (`effects.rs:664-675`) is a flat
array. In memory, **a param's identity is its index.** Everything that
references a param — drivers, envelopes, automation lanes, ableton mappings,
audio mods, OSC, the card UI, undo — must convert a stable `param_id` into that
index, and the index is *derived* from a list that changes on every edit. So:

- Every edit that adds/removes a param must renumber the array in lockstep with
  the binding list and the metadata list.
- Every reader must derive the index from the *same* authoritative list — and
  there are two candidate lists (registry def vs live `meta.params`), which is
  exactly how the triggering bug happened.

The fragile core is the addressing convention
**`param_values = [static registry prefix | user-added tail]`**, with tail slot
`= static_count + user_position`. That arithmetic appears in at least seven
places in `effects.rs` and is mirrored in the renderer and editing commands (see
§5).

---

## 3. The key finding that makes this tractable: **disk already speaks id**

Verified in source, not inferred:

- **Serialize** (`serialize_param_values`, `effects.rs:1099-1140`): emits a
  **map keyed by param_id** (`{ "amount": {value, exposed}, ... }`) whenever the
  registry def is available. Static prefix keyed by `def.param_ids[i]`, user
  tail keyed by binding id. The positional array is only a *fallback* for
  unregistered types (test contexts).
- **Deserialize** (`ParamValuesWire::into_positional`, `effects.rs:785-844`):
  accepts four historical shapes (V1.0/1.1 positional `[f32]`, V1.2 keyed
  `{id: f32}`, V1.3 positional `[{value,exposed}]`, V1.3 keyed
  `{id: {value,exposed}}`) and folds them into the positional Vec, using the
  registry only to *place* keyed values by index.

**Implication:** moving the *in-memory* model from `Vec<ParamSlot>` to an
id-keyed map requires **no disk-format change and breaks no backward compat.**
Old positional files still load through the `Positional` arm; the canonical
form is already a map. The deserialize `Keyed` arm becomes nearly the identity
(drop the `out[idx] = ...` index computation; keep alias resolution + unknown-id
drop + default backfill). This is a *representation* refactor, not a format
migration. That is the crux that makes a SOTA redesign realistic.

`ParamSlot` shape (`effects.rs:470-497`): `{ value: f32, base: f32, exposed:
bool, touched: bool }`. Only `value` + `exposed` are serialized per slot; `base`
rides a parallel `baseParamValues` wire (same keyed/positional duality),
`touched` is runtime-only.

---

## 4. The GPU is NOT a positional consumer (verified — this removes the main objection)

The intuitive objection to id-keyed storage is "the shader needs a contiguous
array at fixed offsets." **That is false here.** Traced end to end:

- `generator_renderer.rs:579-587` copies `gp.param_values` into a positional
  staging array `params: [f32; MAX_GEN_PARAMS]` on `PresetContext`.
- `PresetContext` (`preset_context.rs:22`) is `#[derive(Clone, Copy)]` **only —
  not `bytemuck::Pod`.** It is never uploaded to a buffer.
- The staging array is immediately turned *back* into `ParamSlot`s
  (`apply_param_values`, `preset_runtime.rs:2843`) and fed to `apply_bindings`
  (`param_binding.rs:569`), which writes each value into the target inner node's
  param **by inner-param name string** via `ResolvedBinding.source_index`.
- The actual GPU uniform structs (`BlendUniforms`, `Globals`, per-node
  `*_params: [f32; 4]` in `render_scene.rs` / `render_3d_mesh.rs`) are filled
  from **that node's own string-keyed `node.params` map**, never from the outer
  `param_values` array.

So the shader binding layout is per-node and name-keyed. The outer array's order
only matters because `source_index` currently happens to be computed
positionally — and `source_index` is *already* documented as a load-bearing
handle decoupled from array position (`param_binding.rs:224-249`). Under
id-keyed storage, `source_index` becomes an id (or an id→value lookup) and the
GPU contract is untouched.

**Net: nothing in the system fundamentally requires a contiguous positional
ordering of `param_values`.**

---

## 5. Blast-radius census (grounded, categorized by *why* each is positional)

Full list with file:line is in the investigation transcript; here is the shape
Fable needs. Three categories:

**(a) Positional-by-accident** — resolve an id to an index at a boundary, use the
index transiently. Trivially convertible to id-keyed lookup:
- All modulation writers: drivers (`modulation.rs:229-244`), envelopes
  (`modulation.rs:109-124`), audio-mods (`modulation.rs:430-452`).
- Automation sample/latch (`automation.rs:211-242`).
- App `Param` command (`app.rs:106`), inspector/card watched-value reads
  (`app_render.rs:343`, `inspector.rs:1315`).
- `apply_bindings` / `source_index` (`param_binding.rs:569`).

**(b) Positional-as-transport** — a self-consistent snapshot where order-out =
order-in. Needs the two ends to agree, not a global contract. These survive
id-keying as long as the encode/decode agree (ideally re-key them):
- UI↔content-thread modulation bridge: flat `values: Vec<f32>` + `block_lens`
  (`content_state.rs:283-401`), gated on `param_values.len() == len`.
- Clipboard copy/paste snapshot (`clipboard.rs:79-90`).
- Card `param_info` ↔ slot zip (`param_card.rs:2556, 2627`).
- The `generator_renderer` → `PresetContext.params` staging array (§4).

**(c) The one genuinely fragile derivation** — the static/user split. This is
what an id-keyed redesign *deletes*:
- `param_id_to_value_index` (`effects.rs:2142`), `resolve_param_in`
  (`effects.rs:188`), `static_param_count` (`effects.rs:2127`) — the three
  hand-synced resolvers.
- `align_to_definition` (`effects.rs:2422`) — rebuilds the array positionally on
  load; contains a **hardcoded WireframeDepth 14→12 slot reorder**
  (`effects.rs:2455-2473`), the most brittle instance in the codebase.
- Renderer mirror: `n_static` / `n_static_slots` in `preset_runtime.rs`
  (`:567-575` and every rebuild site).
- Editing: `static_slot_for` + `static_count + position` (`commands/graph.rs`),
  `SetParamExposedCommand { param_index }` (`commands/effects.rs:663`).

**Undo** captures raw slot indices in several inverse pairs
(`remove_user_binding_by_id`/`restore_user_binding_at`,
`remove_exposures_for_node`/`restore_exposures`, the static-slot exposure flip).
These must keep working; id-keyed storage arguably makes them *simpler* (restore
an entry by id, no re-insert-at-index).

---

## 6. Latent bugs in the same class (evidence the disease is active, not theoretical)

Found during the audit; each is a symptom, and each becomes a free win if the
redesign is done right:

- **Ableton mappings only reach the static registry prefix.**
  `ableton_bridge.rs:2100` resolves via `def.id_to_index` (registry static map)
  and never consults the user tail — so a user-added / bundled-beyond-registry
  param likely can't be Ableton-mapped or mis-resolves. Worth confirming with a
  repro before the redesign so it's captured as an acceptance test.
- **OSC dispatch is registry-positional and static-only.**
  `osc_param_router.rs:126-227` iterates `0..def.param_count` and stores
  `param_index: pi`. Same blind spot as Ableton.
- **Three-resolver lockstep** is an unenforced invariant. The triggering bug was
  one resolver drifting from the other two. Nothing in the type system prevents
  the next drift.

---

## 7. Design direction (a hypothesis for Fable to pressure-test, not a mandate)

Single source of truth: store param values **keyed by id** (e.g.
`IndexMap<ParamId, ParamSlot>` — insertion-ordered so any place that still wants
a stable enumeration order gets one for free). Then:

- Deleting a param is removing one entry. Nothing renumbers. Drivers keyed by id
  just resolve. The static/user split (§5c) is *deleted*, not fixed.
- The registry keeps its real job — the *catalog template* (what params a preset
  type has by default, their ranges, defaults, aliases) — but stops being
  consulted to *place* per-instance values by index.
- Positional layout is computed **only** at the two boundaries that want it
  (the transport snapshots in §5b and the render staging array in §4), as a pure
  function of the id-keyed store — the same discipline as the graph flattener
  (`docs/NODE_GROUPS_DESIGN.md`): derive the positional view, never store it as
  identity.
- `ResolvedBinding.source_index` becomes an id (or an id→value lookup at
  apply-time).

This matches Peter's standing principle: kill the bug class where the data
lives, not at each call site that reads it
(`feedback_eliminate_bug_class_at_storage_layer`).

---

## 8. Constraints & invariants the redesign MUST preserve

- **Disk backward compat** — already free (§3), but old positional files must
  still load and unknown-id drop policy must stay.
- **Per-frame hot-path cost** — modulation runs every frame over every driven
  param. Today it does an id→index resolve (a scan or map lookup) *then* indexes
  a Vec. An id-keyed store must not regress this; an `IndexMap` get is
  comparable, but Fable should measure. See `feedback` on hot-path discipline in
  `CLAUDE.md`.
- **Undo inverse-pairs** must round-trip exactly (there are tests).
- **`base` (pre-mod) + `value` (post-mod) + `exposed` + `touched`** per slot must
  all survive; `touched` is the automation-latch flag and is runtime-only.
- **Range overrides** live in `graph.preset_metadata.params[].min/max` and apply
  even when the index comes from the registry (`override_range`,
  `effects.rs:200`) — the redesign must keep per-instance reshape working.
- **Generators vs effects asymmetry** — a graph-backed generator's `meta.params`
  order is authoritative; an effect's static prefix comes from the registry and
  its user tail from bindings. The redesign should *unify* these, not preserve
  the asymmetry.

---

## 9. Open questions for Fable

1. `IndexMap<ParamId, ParamSlot>` vs a `Vec<(ParamId, ParamSlot)>` + lookup
   cache — which wins on the per-frame path at typical scale (`docs`:
   `project_typical_project_scale` = up to ~thousands of clips, but param counts
   per instance are small, <~40)?
2. Should `source_index` become a `ParamId`, or should bindings resolve
   id→value once per rebuild into a positional scratch (keeping the apply loop
   index-based but the *identity* id-based)?
3. Do the transport snapshots (§5b) get re-keyed to id, or kept positional with
   a stronger "order-out = order-in" guarantee? Re-keying is safer but touches
   the content-thread protocol.
4. Fold Ableton/OSC (§6) onto the unified id resolver as part of this, or
   sequence them after? They're the same disease.
5. Is `align_to_definition`'s load-time realign still needed at all once storage
   is id-keyed, or does it collapse into "backfill missing defaults, drop
   unknown ids" (which deserialize already does)?

---

## 10. Pointers

- Immediate fix + regression test: commit `e226be46`,
  `effects::tests::bundled_slider_delete_does_not_misroute_survivor_drivers`.
- Resolvers: `effects.rs:188` (`resolve_param_in`), `:2142`
  (`param_id_to_value_index`), `:2127` (`static_param_count`), `:2422`
  (`align_to_definition`).
- Serialization: `effects.rs:1099` (serialize), `:785` (deserialize/into_positional).
- GPU path: `generator_renderer.rs:579`, `preset_runtime.rs:2843`,
  `param_binding.rs:569` + `:224-249` (source_index doc).
- Standards to write against: `docs/DESIGN_DOC_STANDARD.md`,
  `docs/DESIGN_BUILD_ORDER.md`.
- Related memory: `feedback_eliminate_bug_class_at_storage_layer`,
  `project_preset_unification` (the step-3 fold-in that unified binding storage —
  this is the natural next step of that arc).
