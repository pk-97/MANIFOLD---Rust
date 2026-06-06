# Preset Instance Collapse Plan — finish the effect/generator unification

**Status:** IN PROGRESS (attempt #8). Phases 0–2 landed (each on its own branch,
all gates green); Phase 3 is next. The previous seven attempts stalled for one
reason: each finished the *definition* side and deferred the *instance* and
*runtime* side. This plan does that deferred half, on a cleaner model than the
prior attempts assumed (see "The model" below). When it is done,
`EffectInstance` and `GeneratorParamState` are one thin type, `ChainGraph` and
`JsonGraphGenerator` are one runtime, calibration lives in the preset file, and
the bridge code that exists only to keep two types in sync is **deleted** — not
abstracted, deleted.

**Progress log.**
- **Phase 0–1 (branch `preset-collapse-phase1`):** `EffectTypeId` +
  `GeneratorTypeId` collapsed into one `PresetTypeId`; the kind-specific legacy
  integer discriminant is decoded via two explicit `deserialize_with` helpers at
  the (kind-known) instance deserializers. Old structs deleted, no on-disk format
  change.
- **Phase 2 (branch `preset-collapse-phase2`):** added `curve` + `invert` to the
  preset param authoring surface (`ParamSpecDef` + `ParamDef`), defaulted +
  skip-serialized so all 46 presets stay byte-identical. **Audit correction:** the
  preset file *already* carried range/label/default/whole_numbers
  (`presetMetadata.params`) and routing/scale/offset/convert
  (`presetMetadata.bindings`) — so no 46-file data migration was needed; curve +
  invert were the only gap. Runtime consumption of preset-authored curve/invert
  lands with the resolver-unification phase, per the reorder below.
- **Ordering correction (found during Phase 2):** deleting `ParamMapping` (the
  per-instance reshape note) requires a *home* for per-instance recalibration —
  and in this model that home is "fork the preset." So the **fork-infrastructure
  phase must precede the ParamMapping-deletion / resolver-unification phase.** The
  back half is reordered accordingly (old Phase 4 ↔ Phase 5).

**The contract.** No deferred items. No stop-gaps. No "fold-in later." No
additive-then-switch. No per-call fallbacks. No middle bridges left standing at
the end. A versioned, unit-tested load migration is the ONE permitted
transitional mechanism, because it converts old data once at load and then the
old shape never exists in memory. Every phase ends with the old code path
**deleted**, not gated. If a phase can't delete what it replaces, the phase isn't
done.

---

## The model (what changed from attempt #1–7)

The earlier attempts modeled a preset as a *shared catalog template* plus a
*hidden per-instance override* (copy-on-write graph diff + per-instance reshape
notes). That model is what made the fold-in scary and is the source of the bug
class. We are not finishing that model — we are replacing it.

**A preset is a self-contained file.** One JSON carries the whole thing: the node
graph, which params are exposed as sliders, their ranges, labels, curves,
defaults. This is the unit of sharing — drag-and-drop, someone else uses it, no
dependency on anything it was derived from. The Resolume/TouchDesigner model, not
the Ableton per-device-override model.

**An instance on a layer is thin.** It references a preset and holds only live
performance state: the current value of each exposed param and the modulation
routings that write those values every frame (drivers, envelopes, Ableton, OSC).
Nothing else. No graph, no bindings, no reshape notes, no ranges.

**There is no per-instance override layer.** Ranges and exposure are preset data,
not instance data. The per-instance `ParamMapping` reshape note is **deleted** —
which deletes the original Speed bug by construction, because there is exactly one
range (the preset's), so nothing can clamp a widened range back to a stale one.

**Preset namespace.** Three sources, one id space: stock presets (ship in the app
bundle, read-only), user presets (`~/Library/Application Support/MANIFOLD/presets/`),
and project-embedded presets (inside the project ZIP, scoped to that project).
An instance references a preset by id; the id resolves across all three.

**Editing + fork ergonomics.**
- Values and modulation are instance-local and instant. They never fork.
- A *preset-level* edit (a range, which knobs are exposed, the graph topology)
  edits the preset in place **if this instance is the only user of it**, and
  **auto-forks a named project-embedded variant for this instance if the preset
  is shared by 2+ instances.** So an edit can change other layers only when
  shared — which is exactly when it forks instead. You can never silently change
  another instance.
- Stock presets are read-only and shared by definition, so editing one always
  forks into the project. Falls out of the rule, no special case.
- The fork is **visible, not modal** — the editor header flips to
  "Oily Fluid — Layer 2 variant". Silent-local is fine; silent-global is banned.
- "Change it for everyone using this preset" stays a deliberate, separate action.
- A fresh fork is used by exactly one instance, so subsequent edits to it don't
  fork again. Forking happens only at the moment of divergence — no sprawl.

**Persistence vs sharing.** Edits autosave into the project (into whichever preset
is being edited), undoable. "Save as" means **export to a standalone file** for
sharing — forgetting it loses nothing, it just means you haven't shared yet.
Manual-save-to-not-lose-work is not a thing here.

---

## Why it's critical

Effects and generators are one concept stored as two types, so every feature
ships twice and every fix lands twice, months apart, with drift between. The
drift is the live-show bug surface. Known instances, all the same root cause
(range/binding resolution centralized for effects, re-derived informally for
generators):

- **Generator card "hidden maximum"** — a widened range clamps back to the static
  JSON range on write (`GeneratorParamState::set_param_base` → `clamp_param`). The
  reported `graphTests2.manifold` Speed-capped-at-4 bug.
- **Generator modulation hidden-max** — the generator driver/envelope walk in
  `manifold-playback/src/modulation.rs` scales into the static def range and
  ignores reshape notes, so automation hits the same cap. Effects route through
  `resolve_param_in`, which honours the range.
- **Freeze/fusion binding blindness** (`82a75332`) and the **Speed snap-back**
  (`903deaa8`) — both written up in `docs/PRESET_UNIFICATION_PLAN.md` as symptoms
  that die only at the instance + runtime collapse this plan performs.

Under the new model the first two die by deletion (one range, in the preset; one
resolver). The last two die at the runtime collapse (one runtime reading one
binding source).

---

## What is already done (do not redo)

Per `docs/PRESET_UNIFICATION_PLAN.md` (its 10 steps genuinely landed):

- **One `PresetDef` / `PresetKind`** — the *definition* side is collapsed
  (`crates/manifold-core/src/preset_def.rs`); the behavioral fork (skip-mode,
  wet/dry, OSC scheme, line-based) rides `PresetKind`.
- **One disk-loaded catalog** with hot-reload; no compiled shadow.
- **The runtime apply loop** (`pre_allocate_resources`, the graph executor) is
  shared by the effect chain and the generator path.
- **UI card panels** merged into one `ParamCardConfig` / `param_card.rs`.
- **OSC scheme** unified; **v1.4.0** binding-storage save migration shipped.

The definition side and the shared plumbing are done. This plan is the **instance
type**, the **renderer runtime type**, and the **preset-file model** for
calibration — the deferred half.

---

## What is still forked (the scope of this plan)

Audited 2026-06-06.

| Concern | Effect | Generator | Resolution |
|---|---|---|---|
| Instance struct | `EffectInstance` (`effects.rs:599`) | `GeneratorParamState` (`generator.rs:21`) | One thin `PresetInstance` |
| Type id | `EffectTypeId` | `GeneratorTypeId` | One `PresetTypeId` |
| Graph home | inline `EffectInstance.graph` | `Layer.generator_graph` | in the **preset file**, not the instance |
| Exposed params + ranges | static def + per-instance `ParamMapping` notes + `user_param_bindings` | def + `ParamMapping` + `preset_metadata.bindings` | in the **preset file** |
| Envelope home | `Layer.envelopes` keyed `(effect_type, param_id)` | inline on `GeneratorParamState` | inline on the instance, keyed `param_id` |
| base/effective values | `Vec<ParamSlot>` + `Option<Vec<f32>>` | `Vec<ParamSlot>` + `Option<Vec<f32>>` | one value bus on the instance |
| Clamp policy | UI clamps to note range | core clamps to def range | one resolver; range comes from the preset |
| Modulation | `resolve_param_in` | hand-rolled def-only walk | one walk via one resolver |
| Runtime type | `ChainGraph` | `JsonGraphGenerator` | one `PresetRuntime` |
| Addressing | `GraphTarget::Effect(EffectId)` | `GraphTarget::Generator(LayerId)` | **stays** a two-variant enum |

**The one genuine difference that does NOT collapse:** addressing. An effect is
one item in a layer's ordered, groupable chain (`EffectId`); a generator is the
layer's single source (`LayerId`). `GraphTarget` stays. Everything else collapses.

---

## Architectural target

### The preset file (manifold-core / on disk)

The authored, shareable unit. `PresetDef` already exists for the definition; this
extends the *file* to carry the full authoring surface so a preset is
self-contained:

- node graph (topology),
- exposed params: id, target inner node (stable `NodeId`), inner param, label,
  range (min/max), curve, default, convert,
- preset metadata (kind, osc prefix, etc.).

This is the home for everything that used to be split across the static def, the
per-instance `ParamMapping` notes, and `user_param_bindings`. One place.

### The thin instance: `PresetInstance` (manifold-core)

Replaces both `EffectInstance` and `GeneratorParamState`:

```rust
pub struct PresetInstance {
    pub kind: PresetKind,            // Effect | Generator
    pub preset_id: PresetTypeId,     // which preset (catalog/user/project)
    pub enabled: bool,
    pub collapsed: bool,

    pub param_values: Vec<ParamSlot>,        // live value per exposed param
    pub drivers: Option<Vec<ParameterDriver>>,
    pub envelopes: Option<Vec<ParamEnvelope>>,   // inline, both kinds
    pub ableton_mappings: Option<Vec<AbletonParamMapping>>,

    // Chain membership — real differences GraphTarget already encodes; here only
    // so the chain can carry one element type. Generator: id is layer-derived,
    // group_id is None.
    pub id: PresetInstanceId,
    pub group_id: Option<EffectGroupId>,
}
```

No `graph`, no `bindings`, no `param_mappings`, no `base_param_values` residue, no
legacy flat fields. Graph + exposure + ranges are in the preset; live state is
here. When you "modify" an instance's calibration or topology, you are editing
its preset (forking first if shared) — never mutating the instance with a hidden
override.

> Note: with calibration in the preset, "base vs effective" value is just the
> pre/post-modulation value of an exposed param — one `Vec<ParamSlot>` bus plus
> the modulation evaluator, same as effects already do.

### One runtime: `PresetRuntime` (manifold-renderer)

`ChainGraph` and `JsonGraphGenerator` both own a compiled `Graph` + `ExecutionPlan`
+ executor and both build through the shared `pre_allocate_resources`/`graph_loader`
pipeline. They differ only in input handling (effect transforms an input texture;
generator produces from nothing) and the binding-apply source. `PresetRuntime`
carries an optional input slot (None ⇒ generator) and reads exposure from the one
preset. The `Generator` trait and the effect-chain runtime surface collapse in.

### `GraphHost` is deleted

`GraphHost` exists only to operate over two structs without knowing which. With
one thin instance, call sites take `&mut PresetInstance`. The trait, both impls,
`GeneratorHost`, and the `with_graph_host_mut` closure dance delete. If
`GraphHost` survives, the collapse didn't happen.

---

## Phased plan — dependency ordered, each phase atomic

Every phase: branches off the current head, compiles clean, passes all gates, and
**deletes the code it replaces** before the next begins. No fork left standing
"for the next phase."

### Phase 0 — Field reconciliation + safety net `[read-only]` ✅ DONE

In the main context (no agents — read-only audit rule). Produce the exact
field-by-field map: every reader/writer of every `EffectInstance` and
`GeneratorParamState` field; which `Layer` fields move (the generator
`generator_graph*` and the effect slice of `Layer.envelopes`); every
`EffectId`/`GeneratorTypeId`/`GraphHost` call site; and — critically — the full
list of what must move from instance/def/notes **into the preset file**
(exposure, ranges, curves, the `ParamMapping` semantics). Record the Phase-0
`cargo test --workspace` baseline (only expected failure:
`liveschool_ableton_mappings_resolve_to_stable_param_ids`). Capture the canonical
`Liveschool Live Show V6 LEDS.manifold` as a golden load→save→diff fixture.

Gate: field map complete (no "TBD"); baseline recorded; golden fixture captured.

### Phase 1 — One `PresetTypeId` (manifold-core) ✅ DONE `[branch preset-collapse-phase1]`

Introduce `PresetTypeId`, collapse the two registry keyings onto it, delete
`EffectTypeId`/`GeneratorTypeId` (no permanent alias). Update every signature.
The legacy integer discriminant is kind-specific (effect 10 = Feedback vs
generator 10 = WireframeZoo) — decoded by two explicit functions used via
`deserialize_with` at the instance deserializers where the kind is statically
known; the bare `Deserialize` handles the modern string form. No on-disk format
change (the id serializes as its string value).

Outcome: core 218 + 9 new tests green; io + `load_project` green; app == baseline.

### Phase 2 — Preset file carries the full authoring surface ✅ DONE `[branch preset-collapse-phase2]`

**Audit correction (the plan's original premise was wrong here).** The preset file
already carried the bulk of the authoring surface: `presetMetadata.params`
(`ParamSpecDef`) holds range/label/default/whole_numbers, and
`presetMetadata.bindings` (`BindingDef`) holds the inner-node routing +
scale/offset + convert. The catalog is already disk-loaded with no compiled
shadow. So there was **no 26+20-file data migration to do** — the ranges already
live in the files. The *only* authoring fields missing were the slider response
`curve` and `invert`, which existed solely as per-instance `ParamMapping`
overrides (so no preset could ship a non-default knob feel).

What landed: `curve: MacroCurve` + `invert: bool` added to `ParamSpecDef` (wire)
and `ParamDef` (registry), defaulted to Linear/false and skip-serialized when
default → all 46 presets byte-identical, no migration. The user-binding →
spec/def synthesis paths preserve `binding.curve`/`binding.invert`.

Runtime *consumption* of preset-authored curve/invert is **not** in this phase —
it lands with the resolver unification (Phase 5 below), where `ParamMapping` is
deleted and the one resolver reads range/curve/invert from the preset. Wiring it
twice (here, then again at the resolver) would be throwaway, so the field is the
destination foundation and its consumer follows.

Outcome: core 221 (+3 serde tests) green; io green; app == baseline;
`check-presets` + clippy clean. `bundled_presets`: 7/8 execute a frame — the lone
failure is WireframeDepthGraph's **pre-existing** DNN `copy_texture` blit
(documented; unrelated to this change).

> **Migration discipline (applies to every phase below).** A serialized field is
> only removed in the same phase that ships its load migration, so old projects
> always load. There is **one** version bump (`projectVersion → "1.5.0"`),
> introduced by the first phase that changes the on-disk shape; later phases
> extend the same `migrate_v140_to_v150` branch. There is no standalone
> "migration phase" — a phase where the format is half-migrated would be the very
> half-state this plan forbids. Every format-touching phase is gated on the golden
> `Liveschool` round-trip plus the pre-1.5 fixture corpus.

### Phase 3 — Unify into one `PresetInstance`; delete `GraphHost` (manifold-core)  ⟵ NEXT

The type merge, and *only* the type merge. `EffectInstance` and
`GeneratorParamState` collapse into one `PresetInstance { kind: PresetKind, .. }`
carrying the **union** of their current fields (value bus, base values, drivers,
envelopes, ableton, `param_mappings`, the inline graph override + versions,
enabled/collapsed, id, group_id). Both kinds construct, serialize, and modulate
through it; the two modulation walks collapse to one over `&PresetInstance`. With
one struct there is nothing for `GraphHost` to abstract — call sites take
`&mut PresetInstance`; delete `GraphHost`, `GeneratorHost`, and the
`with_graph_host_mut` closure plumbing. Reconcile the field-home differences the
audit found: the generator's graph lives on `Layer` (`generator_graph*`) and
effect envelopes live on `Layer.envelopes` keyed by `(effect_type, param_id)` —
both move onto the instance (envelopes keyed by `param_id`). If that moves a
serialized field, this phase ships the migration slice + the v1.5.0 bump.

**Not yet thin.** `param_mappings` and the inline graph override ride along until
their replacements land (fork infra in Phase 4, resolver + fork-routed edits in
Phase 5). They are existing, working fields carried forward, each deleted in the
phase whose replacement makes it removable — not stubbed or gated in the interim.

Gate: full workspace test == baseline; both old structs + `GraphHost` gone;
behavior unchanged (golden `Liveschool` round-trip; parity suite, since the
modulation/binding path is touched).

### Phase 4 — Preset namespace + fork ergonomics (core + io + editing + ui)

A self-contained capability, independent of `ParamMapping`. Three-source preset
namespace (stock read-only / user folder / project-embedded in the project ZIP)
over one id space. Explicit duplicate/fork → project-embedded preset; export a
project preset to a standalone `.json`; import a `.json`; the picker lists
project presets. This builds the **home** for per-instance recalibration that
Phase 5 routes edits into when it deletes `ParamMapping`.

Gate: full `cargo test --workspace`; duplicate-into-project + export + re-import
round-trips and renders; project-embedded presets survive project save/load
(carries a migration slice if it changes the ZIP layout).

### Phase 5 — One resolver; delete `ParamMapping`; thin the instance; fork-route edits (core + playback + renderer + ui)

The cutover that makes the preset the single source. One `resolve_param` over
`&PresetInstance` reads range/label/curve/invert from the preset (Phase 2's
fields are consumed **here**) and scale/offset from the binding — no reshape-note
branch. The per-instance reshape **drawer** now edits the preset, forking per the
Phase 4 share-count rule (edit-in-place if sole user, auto-fork if shared, stock
always forks, visible-not-modal). Delete `ParamMapping` and drop the now-replaced
`param_mappings` + inline-graph-override fields from `PresetInstance` (a diverged
graph becomes a forked project preset). Ships its migration slice: existing
`param_mappings` + diverged `graph` in old projects fold into project-embedded
forked presets. **The clamp hidden-max and the modulation hidden-max both die
here** — there is one range, in the preset.

Gate: full `cargo test --workspace`; OilyFluid Speed (load `graphTests2.manifold`)
reaches its preset range from the slider *and* from a driver/envelope; editing a
shared preset forks and leaves siblings byte-identical; `ParamMapping` gone;
golden `Liveschool` + fixture corpus round-trip.

### Phase 6 — One runtime `PresetRuntime` (manifold-renderer)

Collapse `ChainGraph` and `JsonGraphGenerator` into `PresetRuntime` (optional
input slot ⇒ generator). The `Generator` trait collapses in. The freeze/fusion
compiler reads exposure from the one preset (the `82a75332` binding-blindness
class is now impossible). Delete `JsonGraphGenerator`, the `Generator` trait, and
the `GeneratorRegistry` fork.

Gate: full `cargo test --workspace`; all parity tests; `bundled_presets`; every
shipped effect and generator renders a frame (the WireframeDepthGraph DNN-blit
fail is the documented pre-existing exception until its decomp lands).

### Phase 7 — Command/UI collapse + final sweep (editing + app + ui)

Mirror dances (`mirror_effect_side`, `prepare/apply/unmirror_generator_mirror`)
collapse into one `mirror` over `&mut PresetInstance`. The two card-build blocks
in `state_sync.rs` collapse into one. Final `rg` deletion sweep: zero
`EffectInstance`, `GeneratorParamState`, `EffectTypeId`, `GeneratorTypeId`,
`GraphHost`, `GeneratorHost`, `JsonGraphGenerator`, `Generator` (trait),
`ParamMapping` outside historical doc mentions.

Gate: full `cargo test --workspace`; `clippy --workspace -D warnings`; deletion
sweep clean; real-app smoke test on Peter's machine (this env can't launch the
GUI) — load `graphTests2.manifold` + the `Liveschool` show, confirm the Speed
range, the fork-on-shared-edit behavior, and visual parity.

---

## Verification gates (every phase)

1. `cargo test --workspace` vs the Phase-0 baseline. Known pre-existing failures
   that are NOT regressions: `liveschool_ableton_mappings_resolve_to_stable_param_ids`
   (FluidSimulation gen Ableton mapping) and the renderer-lib in-flight fails
   (WireframeDepthGraph one-frame DNN blit, DepthOfField prewarm, chain_pool
   tests) carried on the `pointwise-fusion` lineage. `cargo test --workspace`
   aborts at the first failing binary, so for a clean oracle also run the focused
   crates: `cargo test -p manifold-core -p manifold-io -p manifold-app`
   separately and compare each to baseline.
2. `cargo run -p manifold-renderer --bin check-presets`.
3. `cargo test -p manifold-renderer --lib bundled_presets` (executes a frame).
   Acceptable failure: WireframeDepthGraph only (pre-existing DNN blit).
4. The golden `Liveschool` round-trip.
5. Renderer-touching phases: the parity suite.

## Rollback contract

Each phase is one branch off the prior; a phase that fails a gate is reverted
whole, not patched forward. Format-changing phases (3 / 5, and 4 if it touches
the ZIP layout) carry their own migration slice under one `projectVersion` bump
and do not land until the golden `Liveschool` round-trip + the pre-1.5 fixture
corpus are green — so the format is never left half-migrated between phases.

## Definition of done

- One thin instance (`PresetInstance`), one type id (`PresetTypeId`), one runtime
  (`PresetRuntime`), one resolver, one modulation walk, one card-build path, one
  mirror.
- A preset is a self-contained file carrying graph + exposure + ranges;
  instances hold only preset-ref + values + modulation.
- The fork model works: sole-user edits in place, shared edits fork a visible
  named project variant, stock always forks, export shares a standalone file.
- `EffectInstance`, `GeneratorParamState`, `EffectTypeId`, `GeneratorTypeId`,
  `GraphHost`, `GeneratorHost`, `JsonGraphGenerator`, the `Generator` trait, and
  `ParamMapping` are deleted from the tree.
- `GraphTarget` remains a two-variant enum (the one real difference).
- The four named bugs have regression tests that would have failed before.
- `docs/PRESET_UNIFICATION_PLAN.md` points here for the instance/runtime/preset-file
  half, so its "COMPLETE" status stops being misleading.
