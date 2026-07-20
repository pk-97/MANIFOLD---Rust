# Param Manifest — id-keyed per-instance parameter storage

**Status:** SHIPPED (closed 2026-07-16) — all five phases landed 2026-07-05; no code work remains. Outstanding items are Peter-owned, not design work: the one-time library re-save to V1.4, and the running-app/hardware confirmations VD-007–VD-010 (`docs/VERIFICATION_DEBT.md`), all closable in one rig pass. Detail: **P5 CODE-COMPLETE (2026-07-05 @ `bdeebbd3`)**: inspector single-source landed. The angle-display flag `is_angle` now has a single persistent home on the manifest `ParamSpecDef` (serde default, skipped when false so presets stay byte-identical) — seeded at every user-expose path (`append_user_binding`, the position-aware insert) from the inner `ParamType::Angle`, and read by the inspector card through the existing single-source overlay. `synth_user_binding` reads `spec.is_angle` instead of the hardcoded `false` that had dead-fed every card since the P2 unification, so exposed angle params (incl. glTF camera orbit/tilt/FOV, threaded through `card_param`) show degrees again. **Two corrections to the earlier plan, found by reading the code:** (a) the *calibrated-range* worry was already solved — `EditParamMappingCommand` dual-writes calibration to both the manifest spec and the `meta.params` shadow, and the card overlay reads the shadow, so calibrated bundled params already displayed correctly; the only real defect was `is_angle`. (b) `convert` was NOT added to the spec — it already lives correctly on `BindingDef` (synth reads it; `whole_numbers` is derived onto the spec at expose), so putting it on the spec would have duplicated a field with a home. Back-compat verified: the V1.4 migration only writes value-state, never the spec, so no migration change was needed. Regression test `user_exposed_angle_param_carries_is_angle_through_manifest_and_synth`. Full workspace test + clippy clean (except pre-existing BUG-030). See `docs/landings/2026-07-05-param-storage-p5-inspector.md`. **REMAINING P5 — 1 item, Peter-owned:** the one-time real library re-save to V1.4 (open each show project, confirm integrity, save). The earlier field-shed (`PresetDef` dropped `param_count`/`id_to_index`/`param_ids`/`index_for_param` + positional registry fns) and the macro-label slice (`describe_macro_mapping`) landed before this. See `docs/landings/2026-07-05-param-storage-p5-fieldshed.md` + `…-p5-partial.md`. **P4 SHIPPED (2026-07-05)**: Ableton + OSC resolve param mappings by manifest id against the LIVE manifest, not the frozen registry's positional tables — user-added and glb-imported params are now mappable (Ableton) and addressable (OSC) where they were silently dropped; repros written first (red), bundled OSC addresses proven byte-identical by guard (VD-009 = the owed real-hardware round-trip). P5 (registry containment + library re-save) remains. **P3 SHIPPED (2026-07-05)**: the UI↔content modulation bridge now stamps each transport block with `ParamManifest::topology()` at capture and skips a block on apply when the live topology no longer matches — replacing the `len == len` guard that silently misrouted a same-length param reorder (VD-008 = the owed running-app smoke). **P2 SHIPPED (2026-07-05)**: positional Vec<ParamSlot> + three resolvers deleted; per-instance params now the id-keyed ParamManifest end to end (storage, funnels, renderer bind seam, modulation/automation, serde). bench_resolve 72.38 ns/op (≤271.5 ceiling). Three production migration gaps the test-pass surfaced — revert-prunes-user-params, edit-mapping-writes-live-manifest-spec (D6), restore-honors-snapshot-arity — fixed at the root. See `docs/landings/2026-07-05-param-storage-p4.md` + `…-p3.md` + `…-p2.md`; P1 SHIPPED @ `c7ae831f`. · approved 2026-07-05 · Fable 5
**Prerequisites:** none
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 before starting any phase.

A parameter's runtime identity today is its integer position in a flat array,
even though every parameter already has a stable string id. Every consumer —
drivers, envelopes, automation, Ableton, OSC, the card UI, undo, the renderer —
converts id → index through one of three hand-synchronized resolvers, and the
index is derived from a list that changes on every edit. One resolver drifting
from the other two produced the driver-misroute bug fixed at `e226be46`; the
fix's own comment ("mirrors `param_id_to_value_index`'s generator branch
*exactly* so the two resolvers can't disagree again") is a correctness invariant
maintained by prose. **This design deletes positional identity entirely: each
instance owns a single manifest of `Param` entries — descriptor and state in one
struct, id as identity, order as card order — and nothing between creation and
disk ever resolves a parameter through an index or a registry again.**

Peter's directives (2026-07-05, verbatim — these decided the shape):

- "I HATE the registry system it's legacy infra from before the node graphs and
  it's been an absolute curse on this code base."
- "I would like to do a one-time migration and get rid of the positional arms,
  they are extremely bug prone and difficult to maintain."
- "the mapping and chevron calibration is also a bit 'bolted on' it should be
  first class and integrated properly."
- "This is one of the most core and fundamental systems in Manifold so it needs
  to be as strong, safe, performant, and well designed and architected as
  possible."

Companion docs: `docs/archive/PARAM_STORAGE_REDESIGN_BRIEF.md` (Opus's grounded census —
the input this design was built from; its §5 blast-radius categories still
apply). `docs/NODE_GROUPS_DESIGN.md` (the derive-don't-store discipline this
design copies: positional views are computed at boundaries, never stored as
identity). `docs/AUTOMATION_LANES_DESIGN.md` §4 (the `touched` latch semantics,
which must survive unchanged). `docs/FREEZE_COMPILER_MAP.md` (freeze reads
params through the same runtime apply path; it has no positional dependency —
verified below).

---

## 1. Audit — what exists (verified 2026-07-05)

Instruction to executors: **extend, don't redesign.** Re-verify every anchor at
phase entry; a moved anchor is an escalation, not a guess.

| Piece | Where | State |
|---|---|---|
| `ParamSlot { value, base, exposed, touched }` | `crates/manifold-core/src/effects.rs:470-497` | `base` already folded in (fork #16); `touched` runtime-only via hand-written serde |
| `PresetInstance.param_values: Vec<ParamSlot>` | `effects.rs:664-675` | positional, layout `[static registry prefix \| user-added tail]` |
| `base_tracked` presence bit | `effects.rs:685` | gates `baseParamValues` wire emission |
| Resolver 1: `param_id_to_value_index` | `effects.rs:2142` | generator branch reads live `meta.params`; effect branch reads registry + user tail |
| Resolver 2: `static_param_count` | `effects.rs:2127` | kind-asymmetric authority |
| Resolver 3: `resolve_param_in` → `ResolvedParam { idx, min, max, whole_numbers }` | `effects.rs:188`, `:272` | per-frame modulation resolution; range-override closure reads `meta.params` |
| Renderer mirror: `n_static` / `n_static_slots` | `crates/manifold-renderer/src/preset_runtime.rs:567-575` | fourth copy of the split |
| `align_to_definition` + hardcoded WireframeDepth 14→12 reorder | `effects.rs:2446-2533`, reorder at `:2455-2474` | load-time positional realign |
| Serialize: id-keyed map when registry def exists | `effects.rs:1099-1140` (`serialize_param_values`) | positional Array is the fallback; **generators with user-added bindings emit positional** (`into_positional_for_generator` doc, `effects.rs:846-853`) |
| Deserialize: 4 historical shapes | `effects.rs:785-844` (`ParamValuesWire::into_positional`) | V1.0/1.1 positional f32, V1.2 keyed f32, V1.3 positional slot, V1.3 keyed slot |
| `baseParamValues` parallel wire | `effects.rs:1145-1186` | same keyed/positional duality, floats only |
| Descriptor already exists id-keyed: `PresetMetadata.params: Vec<ParamSpecDef>` | `crates/manifold-core/src/effect_graph_def.rs:373-479` | `ParamSpecDef` carries id/name/min/max/default/curve/invert/labels/osc_suffix; bindings are id-addressed with fan-out (`:404-413`) |
| Registry: `PresetDef { param_count, param_defs, id_to_index, param_ids, legacy_param_aliases, … }` | `crates/manifold-core/src/preset_def.rs:51-82` | global singleton `preset_definition_registry`, frozen at preset install |
| `ResolvedBinding.source_index` | `crates/manifold-renderer/src/node_graph/param_binding.rs:224-249` | documented as decoupled handle; computed positionally today |
| GPU staging: `[f32; MAX_GEN_PARAMS]` on `PresetContext` | `crates/manifold-renderer/src/generator_renderer.rs:579-588` | `Clone, Copy` only, **not Pod, never uploaded** (`preset_context.rs:22`); inner uniforms fill from string-keyed `node.params` |
| Driver eval | `crates/manifold-playback/src/modulation.rs:209-247` | resolves per frame via `resolve_param_in`, **allocates a `Vec<(usize, f32)>` per instance per frame** (`:225`) |
| Envelope eval | `modulation.rs:109-127` | same resolve + index write |
| Transport: flat `values: Vec<f32>` + `block_lens`, guarded by `param_values.len() == len` | `crates/manifold-app/src/content_state.rs:283-402` | **length guard misses same-length reorders** — silent misroute window |
| Ableton write targets | `crates/manifold-playback/src/ableton_bridge.rs:2098-2121` | resolves via registry `id_to_index` only — **user-tail / bundled-beyond-registry params unmappable**; `WriteTarget` already carries `param_id`, the index is redundant |
| OSC registration | `crates/manifold-playback/src/osc_param_router.rs:120-234` | iterates `0..def.param_count` — static-only, registry-positional |
| Undo: `RemovedExposure` captures raw indices | `effects.rs:532-550` | `meta_param_index` / `value_index` / `binding_index` |
| Index-carrying command | `crates/manifold-editing/src/commands/effects.rs:661-693` (`ToggleStaticParamExposeCommand { param_index }`) | index crosses the UI→command boundary |
| Editing slot arithmetic | `crates/manifold-editing/src/commands/graph.rs:1286` (`static_slot_for`), `:1538` (`static_count + position`) | |
| Freeze compiler | `crates/manifold-renderer/src/node_graph/freeze/install.rs:201,323,1021` | **doc-comment mentions only** — freeze consumes params via the runtime apply path, no positional dependency |

Sweep size (re-derive at execution): `rg -c "param_values" crates/ --type rust`
→ ~200 refs over ~25 files (top: effects.rs 195 internal, preset_runtime.rs 32,
modulation.rs 31, commands/graph.rs 27, automation.rs 21, generator.rs 19,
commands/preset.rs 17, content_state.rs 15).
`rg -c "id_to_index|param_id_to_value_index|static_param_count|align_to_definition" crates/ --type rust`
→ ~142 refs (68 in effects.rs, 41 in the registry itself, rest in consumers).

---

## 2. Decisions

**D1 — One struct, one list: `Param` = descriptor + state; `ParamManifest` =
`Vec<Param>` per instance.** Not a value map beside a metadata list — that keeps
two id-keyed structures whose *membership* must stay in lockstep, which is the
same disease one level up. The manifest's insertion order IS the card display
order; identity is the id; nothing is derived. Rejected:
`IndexMap<ParamId, ParamSlot>` beside `meta.params` (the brief's §7 sketch),
because it preserves the descriptor/state split.
Rejected: a global project-wide param table keyed by (instance, param) — it
centralizes nothing that isn't already O(1) per-instance, and it breaks the
locality that makes clip copy, undo, and serialization simple.

**D2 — The registry stops existing at runtime.** After instantiation, no live
`PresetInstance` ever consults `preset_definition_registry` for params. The
bundled preset JSONs (loaded as `PresetMetadata`) remain the *template*:
consulted at instantiation (seed the manifest), at load (backfill new template
params, drop unknown non-user ids), and at calibration reset (re-read template
range). Those are all boundary operations; none are per-frame. Rejected:
keeping the registry as the static-prefix authority — that is the frozen-index
staleness that caused the triggering bug, and Peter's directive kills it.

**D3 — The static/user split is deleted from the data model.** Every param is a
manifest entry. `Param.origin: Bundled | UserAdded` remains as a *behavioral*
tag (unexposing a bundled param hides it; removing a user param deletes the
entry; user specs serialize inline, bundled specs don't) — but origin has **zero
addressing consequence**. `static_param_count`, `param_id_to_value_index`,
`resolve_param_in`, `align_to_definition`, `ResolvedParam`, `n_static`,
`n_static_slots`, `static_slot_for` are all deleted, not reimplemented.

**D4 — One-time migration; the positional wire dies.** (Peter: "extremely bug
prone and difficult to maintain.") A quarantined JSON-value migration module in
`manifold-io` converts all four historical `paramValues` shapes +
`baseParamValues` to the V1.4 form before typed deserialization; the typed
loader understands only V1.4. Positional→id mapping uses a **baked legacy-order
table** (generated once from today's registry, committed as data), NOT the live
registry — the migration must stay correct as templates evolve. The
WireframeDepth 14→12 reorder moves into this table. Old files load forever
through the module; Peter's project library also gets a one-time re-save sweep.

**D5 — `base` folds into the wire entry.** V1.4 serializes one object per
param: `{value, exposed, base?}` (base iff `base_tracked`). The parallel
`baseParamValues` wire and `serialize_base_param_values` are deleted.

**D6 — Calibration is first-class.** (Peter: "it should be first class and
integrated properly.") The chevron recalibration edits `Param.spec.min/max`
(and curve/invert) in place and sets `Param.calibrated = true`. The flag is the
serialization gate: calibrated entries emit a `calibration: {min, max, curve?,
invert?}` block; uncalibrated entries emit nothing and track the template.
Reset-calibration re-reads the template (boundary op, D2). The
`override_range` closure in `resolve_param_in` (`effects.rs:200-206`) and the
generator graph-meta range authority both collapse into "read the Param".

**D7 — `ResolvedBinding.source_index` becomes `source_id: ParamId`; the
per-frame apply does a direct manifest lookup.** No cached index, no rebuild
scratch — a cached index is a smaller copy of the bug this design kills. At
< 40 params per instance a linear id scan is nanoseconds (§8). Fallback if
profiling ever disagrees: topology-keyed index caching, listed in Deferred —
do not build it preemptively.

**D8 — Transport blocks stay flat f32, guarded exactly.** The UI↔content
modulation bridge keeps its zero-alloc `values` + `block_lens` layout (order =
manifest order on both ends, derived from the same snapshot), but each
instance block additionally carries `ParamManifest.topology` (a u32 bumped on
any add/remove/reorder). Apply skips a block on topology mismatch instead of
trusting `len == len` — the current guard misses same-length reorders.
Rejected: fully id-keyed transport — per-frame string traffic for no benefit
when both ends already share the manifest order.

**D9 — Ableton and OSC fold onto the manifest in this redesign.** They are call
sites of the resolvers being deleted, and they are the two live-hardware input
paths. OSC registration iterates the manifest (user-added params become
addressable); Ableton write targets drop `param_index` and resolve by the
`param_id` they already carry. Both blind spots become unrepresentable.

**D10 — Undo addresses params by id.** `RemovedExposure` keeps a `position:
usize` captured at removal time purely to restore card order
(`insert_at(position, param)`); it is a display-order snapshot, never an
identity. `ToggleStaticParamExposeCommand { param_index }` becomes
`ToggleParamExposeCommand { param_id }`.

**D11 — Id lookup is a linear scan; the resolve path stays allocation-free.**
Ids are short strings, manifests are < ~40 entries; a scan with early exit
beats hash overhead at this size and preserves order for free. The driver
evaluator's per-frame `Vec` allocation (`modulation.rs:225`) is deleted as part
of the rewrite (worked example in P2's seam brief). Measure per §8; interning
is Deferred until a profile demands it.

**D12 — On disk, the manifest is the single per-instance param home.** The
V1.4 `params` map carries state for every param, calibration iff calibrated,
and the full `spec` inline iff user-added. The per-instance graph's
`meta.params` is **derived on save from the manifest** (keeps `EffectGraphDef`
uniform with bundled preset JSON, keeps save-as-preset trivial) and **ignored
on load** for V1.4+ files — one direction, one authority, same discipline as
the graph flattener. `meta.bindings` is untouched: routing is graph data, id-
addressed, fan-out capable, and orthogonal to storage.

---

## 3. Data model (committed — the executor transcribes)

New module `crates/manifold-core/src/params.rs`. Owned by `manifold-core`;
lives inside `Project`, so content-thread residency, UI sees it only via
`Arc<Project>` snapshots. No new shared state, no locks.

```rust
/// Why this param exists on the instance. Behavioral tag only — origin has
/// no addressing consequence (D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamOrigin {
    /// Seeded from the preset template at instantiation.
    Bundled,
    /// Added by the user (graph-editor expose). Spec serializes inline.
    UserAdded,
}

/// One parameter: descriptor + live state, one struct, id as identity.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Descriptor — reuses the existing wire struct (effect_graph_def.rs:450).
    /// Calibration edits spec.min/max/curve/invert in place (D6).
    pub spec: crate::effect_graph_def::ParamSpecDef,
    pub origin: ParamOrigin,
    /// True once calibration has diverged this param's spec from the template.
    /// Serialization gate for the calibration block; cleared by reset.
    pub calibrated: bool,
    /// Effective (post-modulation) value — what the renderer reads.
    pub value: f32,
    /// User-intended base (pre-modulation) value. Same semantics as today
    /// (effects.rs:474-482).
    pub base: f32,
    pub exposed: bool,
    /// Runtime-only automation-latch flag. Same semantics as today
    /// (effects.rs:485-495); never serialized.
    pub touched: bool,
}

impl Param {
    #[inline] pub fn id(&self) -> &str { &self.spec.id }
}

/// The per-instance parameter manifest. Insertion order = card display order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParamManifest {
    entries: Vec<Param>,
    /// Bumped on every add / remove / reorder — NOT on value writes.
    /// Transport blocks and any derived positional view guard on this (D8).
    topology: u32,
}

impl ParamManifest {
    pub fn get(&self, id: &str) -> Option<&Param>;
    /// Value/state writes only — does NOT bump topology.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Param>;
    pub fn iter(&self) -> impl Iterator<Item = &Param>;
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Param>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn topology(&self) -> u32;
    /// Bumps topology. debug_assert!s id uniqueness within the manifest.
    pub fn push(&mut self, p: Param);
    /// Bumps topology. Returns the removed entry for undo capture.
    pub fn remove(&mut self, id: &str) -> Option<Param>;
    /// Undo restore at a captured display position. Bumps topology.
    pub fn insert_at(&mut self, index: usize, p: Param);
}
```

`PresetInstance` changes (`effects.rs`):

```rust
// OLD                                          // NEW
pub param_values: Vec<ParamSlot>,               pub params: ParamManifest,
pub base_tracked: bool,                         pub base_tracked: bool,   // unchanged
```

Deleted outright: `ParamSlot`, `ResolvedParam`, `resolve_param_in`,
`resolve_param`, `param_id_to_value_index`, `static_param_count`,
`align_to_definition`, `ParamValuesWire`, `serialize_param_values`,
`serialize_base_param_values`, `into_positional`,
`into_positional_for_generator`. The id-uniqueness invariant
`generate_user_param_id` already provides (effects.rs:2203-2206) is now also
debug-asserted at the storage layer.

Renderer seam (`param_binding.rs`):

```rust
// OLD                                          // NEW
pub struct ResolvedBinding {                    pub struct ResolvedBinding {
    ...                                             ...
    pub source_index: usize,                        pub source_id: ParamId,
}                                               }

// OLD apply loop (param_binding.rs:569):       // NEW:
let v = values[binding.source_index].value;     let Some(p) = manifest.get(&binding.source_id) else { continue };
                                                let v = p.value;
```

The `PresetContext.params: [f32; MAX_GEN_PARAMS]` staging array and
`apply_param_values` (preset_runtime.rs:2843) are replaced by passing
`&ParamManifest` into the runtime apply. `PresetContext` keeps time/beat/dims
only. ⚠ VERIFY-AT-IMPL: confirm the apply call sites have snapshot access at
that point — `rg -n "apply_param_values|PresetContext" crates/manifold-renderer/src/preset_runtime.rs crates/manifold-renderer/src/generator_renderer.rs`.
If a call site genuinely cannot borrow the manifest, escalate; do not
resurrect the staging array silently.

**The plausible-wrong architecture, forbidden by name:** you will want to keep
a positional `Vec` view of the manifest ("just for the hot path", "just for
the transport", "just during migration") or to cache `id → index` on drivers,
bindings, or mappings. **No.** Positional views are computed transiently at the
two boundaries that need them (transport blocks, and nowhere else), guarded by
`topology`, and never stored. A stored index is the bug class this design
exists to delete.

---

## 4. Wire format V1.4 + the one-time migration

V1.4 `PresetInstance` param serialization — the key renames from `paramValues`
to `params`, which is also the migration trigger:

```jsonc
"params": {
  "amount":              { "value": 0.7, "exposed": true },
  "density":             { "value": 0.5, "exposed": true, "base": 0.4,
                           "calibration": { "min": 0.2, "max": 0.8 } },
  "user.blur.radius.1":  { "value": 3.0, "exposed": true,
                           "spec": { /* full ParamSpecDef, camelCase */ } }
}
```

- `base` emitted iff `base_tracked` (D5). `touched` never on the wire.
- `calibration` block present iff `calibrated` (D6); `curve`/`invert` inside it
  only when non-default, matching ParamSpecDef's existing skip rules.
- `spec` present iff `origin == UserAdded` (D12).
- Load reconcile, in order: (1) template seeds bundled descriptors; (2) file
  entries overlay state + calibration; (3) file entries with `spec` append as
  user-added; (4) template params missing from the file append with defaults
  (today's backfill); (5) file entries matching neither template nor `spec` are
  dropped with the existing observability warning — **but only when a template
  actually resolved** (informed deprecation). When NO template resolves at all
  (registry miss + no inline generator graph), the entry is kept on a
  placeholder spec instead of dropped: an unresolvable template is a load-order
  or environment condition, not evidence the id was deprecated, and dropping
  there was silent data loss (BUG-036 — project-local preset templates used to
  register after layer deserialize; the loader's `_with` pre-pass now orders
  them first, and this keep rule is the storage-layer backstop). Alias
  resolution (template `param_aliases`) runs between (2) and (5).
- Per-instance `graph.preset_metadata.params` is rewritten from the manifest on
  save and ignored on load (D12). `meta.bindings` round-trips unchanged.

Migration module: `crates/manifold-io/src/migrations/param_storage_v14.rs`.
Operates on the `serde_json::Value` project tree before typed deserialization,
for both V1 JSON and V2 ZIP containers. Per preset-instance node:

1. Identify the shape of `paramValues` (absent / positional f32 / keyed f32 /
   positional slot / keyed slot — the four arms of `effects.rs:785-844`).
2. Positional → id mapping: for a generator with a per-instance graph, key by
   that instance's own `graph.preset_metadata.params` order (self-contained in
   the same file — this covers the generator-with-user-bindings positional
   emission); otherwise key by the **baked legacy-order table**:
   `const LEGACY_PARAM_ORDER: &[(&str, &[&str])]` generated once from today's
   registry (P1 deliverable) and never regenerated. The WireframeDepth 14→12
   remap is an entry-level rule in the same table.
3. Keyed forms: resolve legacy aliases via a baked copy of
   `legacy_param_aliases`, then emit V1.4 entries directly.
4. Fold `baseParamValues` (either shape) into per-entry `base`.
5. Write `params`, delete `paramValues` + `baseParamValues`.

Both kinds ride the same keys — generators serialize `paramValues` /
`baseParamValues` too, discriminated by `generatorType` vs `effectType`
(`effects.rs:1356-1374`) — so the trigger and rename are uniform.

Failure story (the migration's policy for bad input, decided here):

- **Keyed shapes never need the baked table** — ids are already ids; they
  convert directly (aliases aside). The table is consulted only for positional
  arrays on instances without a usable per-instance `meta.params` order.
- **Positional values for a type absent from the table:** drop the values and
  emit the loud warning, exactly today's unregistered-type policy
  (`effects.rs:795-805`). The instance loads with template defaults. In
  practice this arm is test-contexts only; a warning firing on a real project
  is a bug report, not a silent loss.
- **Array longer than the table's order** (junk tail): truncate, warn — the
  same posture `align_to_definition` takes today (`effects.rs:2444-2445`).
- **Malformed entry** (non-numeric, wrong shape): drop that entry, warn, keep
  the rest. The migration never aborts a whole project load over one param.

The module is the ONLY place positional param knowledge survives. It is pure
`Value → Value`, unit-tested per shape, and its tests carry frozen JSON
fixtures for all four arms — the arms' logic is deleted from `effects.rs`, not
moved.

---

## 5. Hot-path budget (§8 of the brief, resolved)

Today's per-frame driver path: registry `try_get` (hash) + `resolve_param_in`
(meta.params linear scan for generators, AHashMap + user-binding scan for
effects) + a **per-instance `Vec` allocation** (modulation.rs:225) + indexed
write. The new path: one manifest linear scan + direct write, zero allocation.
At ≤ 40 short-string entries with early exit this is strictly less work than
the current resolve; the allocation removal is a net hot-path win.

Gate (P2): a micro-bench test (`params::bench_resolve`, `#[test]`, reports
ns/op over ≥ 1M resolves on a synthetic 40-param manifest, worst-case id last)
compared against the same harness run on the old `resolve_param_in` (measure
before deleting it — record the number in the phase notes). Regression beyond
2× the old number is an escalation. This is a measured number to report, not a
feel.

---

## 6. Phasing

Five phases, one session each. P2 is the heavy one — it is atomic by nature
(a compile-driven storage swap cannot be half-landed) and is sized for a
strong-model session. Test scope is stated per phase; the workspace sweep is
now cheap (GPU tests are feature-gated behind `gpu-proofs` since `7332e49d` —
default sweep ≈ 990 tests / ~24s), so infrastructure phases run it.

### P1 — V1.4 wire + quarantined migration; the positional arms die

- **Entry state:** clean tree off current `origin/main`;
  `rg -n "ParamValuesWire" crates/manifold-core/src/effects.rs` shows the four
  arms; `cargo test -p manifold-core --lib` green.
- **Read-back:** this doc §2 D4/D5/D12, §4 whole; `effects.rs:785-844`,
  `:1099-1186`, `:2446-2474`; `docs/DESIGN_DOC_STANDARD.md` §5–§6. Restate the
  four legacy shapes and where each dies.
- **Deliverables:**
  - `manifold-io/src/migrations/param_storage_v14.rs` + baked
    `LEGACY_PARAM_ORDER` table (generated from the live registry ONCE, by a
    dev-test that prints it; committed as source, never regenerated) + baked
    alias table. Wired into both V1 JSON and V2 ZIP load paths.
  - V1.4 serialize/deserialize in `effects.rs`: `params` map per §4;
    `paramValues`/`baseParamValues` producers and the four wire arms deleted.
    (In-memory storage is still `Vec<ParamSlot>` this phase; the loader places
    keyed values by id→index at the boundary — that placement code survives
    until P2. Do not start the storage swap here.)
  - WireframeDepth reorder moves from `align_to_definition` into the table.
  - Test fixtures re-saved to V1.4 where they exercise load (keep copies of
    the legacy-shape fixtures inside the migration module's tests).
  - Migration round-trip tests: one per legacy shape, plus a generator-with-
    user-bindings positional case, plus a WireframeDepth 14-slot case.
- **Gate:**
  - Positive: `cargo test -p manifold-io --lib migrations::` green;
    `cargo test -p manifold-core --lib` green; the canonical fixture loads —
    `Liveschool Live Show V6 LEDS.manifold` through the migration path
    (existing fixture test); `./scripts/check-presets` (or the repo's preset
    checker) clean.
  - Negative: `rg -n "ParamValuesWire|baseParamValues|serialize_base_param_values" crates/ --type rust -g '!*/migrations/*'`
    → **0 hits**. `rg -n "WIREFRAME_DEPTH" crates/manifold-core/src/effects.rs`
    → 0 hits in `align_to_definition`.
- **Forbidden moves:** keeping a positional arm "for tests" (tests use the
  migration module); regenerating the baked table from the live registry at
  runtime; migrating inside typed serde instead of the Value layer; touching
  in-memory storage.
- **Test scope:** focused (`-p manifold-core --lib`, `-p manifold-io --lib`)
  plus the fixture-load tests. No workspace sweep — storage semantics are
  unchanged this phase.

### P2 — the storage swap: `ParamManifest` replaces positional identity

- **Entry state:** P1 landed;
  `rg -c "param_values" crates/ --type rust` — re-derive the sweep list; if
  file counts differ materially from §1's table, list the new sites in the
  phase notes before editing anything. Record the old-resolver bench number
  (§5) BEFORE deleting `resolve_param_in`.
- **Read-back:** §2 D1/D2/D3/D6/D7/D10/D11, §3 whole (the types are
  transcribed, not designed), §5; `feedback_eliminate_bug_class_at_storage_layer`.
  Restate: what gets deleted, what replaces it, the forbidden positional view.
- **Seam brief (§6 of the standard):**
  - Old → new signatures: §3 blocks, verbatim.
  - Technique: compiler-driven. Delete `param_values`, `ParamSlot`, and the
    three resolvers FIRST; the build errors across
    core/playback/renderer/editing/app are the exhaustive checklist.
  - Mechanical-rewrite categories, one worked example each:
    1. *Resolve-then-index* (drivers/envelopes/audio-mods/automation/app
       `Param` command/inspector reads): `resolve_param_in(def, fx, id)` +
       `fx.param_values[idx].value = v` → `fx.params.get_mut(id)` and write
       `.value` / read `.spec.min`/`.spec.max`/`whole_numbers` off the entry.
       Range/whole-number data comes off the same `Param` — `ResolvedParam` has
       no replacement, it's just fields.
    2. *Driver evaluator de-allocation* (modulation.rs:209-247): take the
       drivers out (`let drivers = fx.drivers.take()`), iterate resolving and
       writing through `fx.params.get_mut(...)`, put them back (`fx.drivers =
       drivers`). The per-frame `Vec<(usize, f32)>` collect is deleted. Same
       pattern for envelopes and audio-mods if the borrow conflict recurs.
    3. *Whole-instance iteration* (transport capture, clipboard, card zips,
       state_sync, staging): `for p in fx.params.iter()` in manifest order —
       order-stable, no index arithmetic.
    4. *Slot arithmetic* (commands/graph.rs:1286/:1538, append/restore paths,
       `RemovedExposure`): delete the arithmetic; `push` for append,
       `remove(id)` + captured `position` + `insert_at` for undo restore.
  - Individually-specified sites (not mechanical):
    `ToggleStaticParamExposeCommand` → `ToggleParamExposeCommand { param_id }`
    including its UI dispatch site; `ResolvedBinding.source_id` + apply loop +
    user-tail rehydration in `preset_runtime.rs` (the `n_static`/
    `n_static_slots` fields die with it); `PresetContext` staging removal (§3
    VERIFY-AT-IMPL); calibration absorption — the chevron write path targets
    `Param.spec` + `calibrated` and the `override_range` closure dies;
    instantiation (`create_default` / preset install / glb import) seeds the
    manifest from the template; loader reconcile per §4.
  - Re-derivation commands: the two `rg -c` sweeps from §1, re-run at entry
    AND at gate (the second run must show only migration-module and
    template-boundary hits).
- **Deliverables:** `params.rs` types + unit tests (get/push/remove/insert_at/
  topology semantics, id-uniqueness debug_assert); the sweep; rewritten
  regression test `bundled_slider_delete_does_not_misroute_survivor_drivers`
  (now provable at the type level — keep it anyway as the canonical-failure
  memorial); undo inverse-pair tests updated and green; bench per §5.
- **Gate:**
  - Positive: **full workspace sweep** (`cargo test --workspace`, default
    features) green; `cargo clippy --workspace -- -D warnings`; bench number
    recorded vs the pre-recorded old number (≤ 2×); Liveschool fixture loads.
  - Negative: `rg -n "param_id_to_value_index|static_param_count|align_to_definition|resolve_param_in|ResolvedParam|ParamSlot|source_index|n_static" crates/ --type rust -g '!*/migrations/*'`
    → **0 hits**. `rg -n "param_values" crates/ --type rust` → 0 hits.
    `rg -n "Arc<Mutex|Arc<RwLock" crates/manifold-core/src/params.rs` → 0.
- **Forbidden moves:** a `to_vec()`/positional-view helper on `ParamManifest`;
  caching resolved indices anywhere; keeping `ParamSlot` as an internal type;
  adapters that accept an index "for the UI's convenience" (the UI passes
  ids); widening into Ableton/OSC (that's P4) or transport internals (P3).
- **Test scope:** full workspace sweep + clippy (this IS the infrastructure
  case). GPU behavior can't change (no shader/uniform edits) — no `gpu-proofs`
  run.

### P3 — exact transport guard + running-app smoke

- **Entry state:** P2 landed; `rg -n "block_lens" crates/manifold-app/src/content_state.rs`
  anchors intact.
- **Read-back:** §2 D8; `content_state.rs:283-402`; the two-thread model
  section of `CLAUDE.md`.
- **Deliverables:** per-instance-block topology stamp in the snapshot
  (`Vec<u32>` parallel to `block_lens`, or a widened block header — executor's
  choice, it's private layout); capture writes `manifest.topology()`, apply
  compares and skips the block on mismatch (structural guard, replacing the
  `len == len` gates at `:346`, `:371`, `:390`); a unit test that builds a
  same-length-different-order manifest pair and proves the block is skipped
  (the exact case the old guard missed).
- **Gate:**
  - Positive: `cargo test -p manifold-app --lib` green; **running-app smoke**
    (this is the one phase headless tests can't fully prove): launch, put an
    LFO on a card slider, delete a neighbouring slider mid-modulation, confirm
    the modulation display stays on the right slider. Run by Peter or via the
    UI-automation harness if available.
  - Negative: `rg -n "param_values.len\(\) == len|params.len\(\) == len" crates/manifold-app/src/content_state.rs`
    → 0 hits (the approximate guard is gone, not paralleled).
- **Forbidden moves:** sending ids over the bridge; allocating in
  capture/apply; "temporarily" keeping the len guard alongside the topology
  guard.
- **Test scope:** focused `-p manifold-app --lib` + the smoke. No sweep.

### P4 — Ableton + OSC onto the manifest (repros first)

- **Entry state:** P2 landed (P3 independent);
  `rg -n "param_index" crates/manifold-playback/src/ableton_bridge.rs crates/manifold-playback/src/osc_param_router.rs`
  shows today's index plumbing.
- **Read-back:** §2 D9; §1 rows for both files; `project_ableton_param_scaling`
  memory (50+ mapped params must keep their scaling behavior).
- **Deliverables:**
  - FIRST, the two acceptance repros as failing tests: (a) an Ableton mapping
    on a user-added / bundled-beyond-registry param (glb-import shape) resolves
    and writes; (b) an OSC address for a user-added param exists and dispatches.
    These are the §6-of-the-brief latent bugs, captured before the fix.
  - `WriteTarget` drops `param_index`; apply resolves via the `param_id` it
    already carries (`ableton_bridge.rs:2119`).
  - OSC registration iterates `fx.params.iter()` (all params, user included);
    `OscParamTarget::{MasterEffect, LayerEffect, GenParam}` carry `param_id:
    String` instead of `param_index`; dispatch writes through the manifest.
    Address shape is `/{scope}/{osc_prefix}/{param_id}`
    (`preset_definition_registry.rs:395-422`) — `param_id` comes off the
    manifest entry, `osc_prefix` is *type-level* metadata read from the
    template at registration time (a boundary op, allowed under D2). Existing
    params keep byte-identical addresses (assert in a test against a known
    preset); user-added params gain addresses for the first time — additive
    only.
- **Gate:**
  - Positive: both repro tests green; `cargo test -p manifold-playback --lib`
    green; the OSC address-stability test green.
  - Negative: `rg -n "param_index" crates/manifold-playback/` → 0 hits;
    `rg -n "id_to_index|param_count|param_defs|param_ids" crates/manifold-playback/`
    → 0 hits. (A template read for `osc_prefix` may remain until P5 decides
    the template library's final home — positional param lookups are what
    must be gone.)
- **Forbidden moves:** renaming OSC addresses "while we're here" (address
  stability is a live-rig contract); registering only exposed params (hidden
  params stay addressable — effects.rs:652-655 semantics).
- **Test scope:** focused `-p manifold-playback --lib`. No sweep.

### P5 — registry containment, library re-save, final proof

- **Entry state:** P1–P4 landed.
- **Read-back:** §2 D2; `preset_def.rs:51-82`;
  `rg -l "preset_definition_registry" crates/` re-derived.
- **Deliverables:**
  - Registry containment: `preset_definition_registry` consumers reduced to
    the three boundary roles (instantiation/template seeding, migration's
    one-time table-generation dev-test, calibration reset). Every other import
    dies or moves to template (`PresetMetadata`) reads. `PresetDef` sheds
    `param_count` / `param_defs` / `id_to_index` / `param_ids` /
    `legacy_param_aliases` if nothing but the boundary roles remain on them —
    if something else still needs one, escalate rather than keep the field
    quietly.
  - One-time re-save sweep of Peter's real project library (run with Peter:
    open each show project, confirm visual + mapping integrity, save → V1.4).
    The Liveschool fixture is committed re-saved; a legacy-shape copy stays in
    the migration tests.
  - `docs/` truth pass: EFFECT_RUNTIME_UNIFICATION §7, DEVELOPMENT_REFERENCE,
    and the brief get status addenda; this doc's status flips when shipped.
- **Gate:**
  - Positive: full workspace sweep + clippy; `gpu_proofs` smoke suite
    (`--features gpu-proofs`, smoke only) — this phase closes the wave, so the
    on-device check runs once here; Peter's library opens clean (his call, not
    the executor's claim).
  - Negative: `rg -n "preset_definition_registry" crates/ --type rust` hits
    ONLY in the three boundary modules (list them in phase notes);
    `rg -n "id_to_index" crates/ --type rust` → 0 hits outside migration.
- **Forbidden moves:** deleting the registry module outright if a boundary
  role still needs it (containment, not eradication — eradication is Deferred);
  re-saving Peter's library without him present.
- **Test scope:** full sweep + gpu_proofs smoke (wave-closing phase).

---

## 7. Decided — do not reopen

1. Storage is one `Vec<Param>` manifest per instance; descriptor + state in one
   struct; id = identity; order = card order. No value-map-beside-spec-list.
2. No registry consultation for a live instance's params, ever. Template =
   bundled preset JSON, consulted at instantiation/load/reset only.
3. Positional wire arms are deleted, not relocated into typed serde. Migration
   is a quarantined `Value → Value` module in manifold-io with baked tables.
4. `baseParamValues` is gone; `base` rides the V1.4 entry.
5. Calibration = in-place `spec` edit + `calibrated` flag; `calibration` block
   on the wire; reset re-reads template.
6. `source_index` → `source_id: ParamId`; per-frame apply is a direct manifest
   lookup; no cached indices anywhere.
7. Transport stays flat f32 + block lens, guarded by manifest `topology`, not
   length.
8. Ableton + OSC fold in now (P4), not "after".
9. Undo restores by id + captured display position via `insert_at`.
10. The static/user split has no addressing meaning; `origin` is behavioral
    only.
11. Per-instance graph `meta.params` is derived-on-save, ignored-on-load
    (V1.4+); `meta.bindings` untouched.
12. `main` merge-trunk discipline applies; each phase lands via the
    fetch/merge/gate/push loop in `.claude/GIT_TREE_DISCIPLINE.md`.

## 8. Deferred (with revival triggers)

- **Id interning / `SmallVec` for manifest entries** — revive iff the P2 bench
  or a real-project profile shows the resolve path above budget (§5).
- **Topology-keyed index caching for `ResolvedBinding`** — revive only on the
  same profile evidence, never preemptively (D7 fallback).
- **Registry module eradication** (beyond containment) — revive when the three
  boundary roles have a better home (e.g. a preset template library type that
  owns instantiation).
- **Id-keyed transport** — revive if the bridge ever needs to survive
  cross-version UI/content skew (it doesn't today; same process, same build).
- **`PresetDef` full removal** — rides registry eradication.
- **Card param drag-reorder** — the manifest makes it a `Vec` reorder + one
  topology bump; free feature, not this wave.
