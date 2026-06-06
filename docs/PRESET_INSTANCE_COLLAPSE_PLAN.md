# Preset Instance Collapse Plan — finish the effect/generator unification

**Status:** NOT STARTED. This is attempt #8. The previous seven stalled for one
reason: each finished the *definition* side and then deferred the *instance* and
*runtime* side. This plan does only the deferred half, and it carries a hard
no-deferral contract (see below). When this plan is done, `EffectInstance` and
`GeneratorParamState` are one type, `ChainGraph` and `JsonGraphGenerator` are one
runtime, and the bridge code that exists only to keep two types in sync is
deleted — not abstracted, deleted.

**The contract.** No deferred items. No stop-gaps. No "fold-in later." No
additive-then-switch. No per-call fallbacks. No middle bridges left standing at
the end. A versioned, unit-tested load migration is the ONE permitted
transitional mechanism, because it converts old data once at load and then the
old shape never exists in memory. Every phase ends with the old code path
**deleted**, not gated. If a phase can't delete what it replaces, the phase isn't
done.

**Why it's critical.** Effects and generators are one concept — a parameterized
node graph that renders, modulates, and round-trips to disk. They are stored as
two types. Every feature is therefore implemented twice and every bug fixed
twice, months apart, with drift in between. The drift is not hypothetical; it is
the live-show bug surface. The currently-known instances:

- **Generator card "hidden maximum."** A widened reshape range (e.g. OilyFluid
  Speed 0.1–10) is silently clamped back to the static JSON range (0.1–4) on
  write, because generators clamp in core against the def while effects clamp in
  the UI against the reshape note. `GeneratorParamState::set_param_base` →
  `clamp_param`.
- **Generator modulation hidden-max.** The generator driver/envelope walk in
  `manifold-playback/src/modulation.rs` scales into the static def range and
  ignores reshape notes entirely, so the same cap bites any automated generator
  param. Effects route through `resolve_param_in`, which honours the note.
- **Freeze/fusion binding blindness** (commit `82a75332`) and the **Speed
  snap-back** (commit `903deaa8`) — both written up in
  `docs/PRESET_UNIFICATION_PLAN.md` as symptoms that "die" only at the instance +
  runtime collapse this plan performs.

All four are the same root cause wearing different clothes: range/binding
resolution centralized for effects (`resolve_param_in`, `LoadedPresetView`) and
re-derived informally for generators in three or four places, each a chance to
drift. They cannot recur once there is one type, one resolver, one runtime.

---

## What is already done (do not redo)

Per `docs/PRESET_UNIFICATION_PLAN.md` (its 10 steps genuinely landed):

- **One `PresetDef` / `PresetKind`** — the definition side is collapsed. The
  registry value type is unified; the behavioral fork (skip-mode, wet/dry, OSC
  scheme, line-based) rides `PresetKind`. `crates/manifold-core/src/preset_def.rs`.
- **One disk-loaded catalog** with hot-reload; no compiled shadow.
- **The runtime *apply loop*** (`pre_allocate_resources`, the graph executor) is
  already shared by both the effect chain and the generator path.
- **UI card panels** merged into one `ParamCardConfig` / `param_card.rs`.
- **OSC scheme** unified; **v1.4.0** binding-storage save migration shipped.
- A `GraphHost` trait already abstracts the *write* surface over both structs.

The definition side and the shared plumbing are done. This plan is strictly the
**instance type** and the **renderer runtime type** — the part that was deferred.

---

## What is still forked (the entire scope of this plan)

Audited 2026-06-06. The two instance structs are ~80% identical fields already.

| Concern | Effect | Generator | Resolution |
|---|---|---|---|
| Instance struct | `EffectInstance` (`effects.rs:599`) | `GeneratorParamState` (`generator.rs:21`) | One `PresetInstance` |
| Type id | `EffectTypeId` | `GeneratorTypeId` | One `PresetTypeId` |
| Graph home | inline `EffectInstance.graph` + 2 versions | `Layer.generator_graph` + 2 versions | inline on instance |
| Binding storage | sibling `EffectInstance.user_param_bindings` Vec | inside `graph.preset_metadata.bindings` | one inline list on instance, decoupled from graph materialization |
| Envelope home | `Layer.envelopes` keyed `(effect_type, param_id)` | inline `GeneratorParamState.envelopes` keyed `param_id` | inline on instance, keyed `param_id` |
| base values | `Option<Vec<f32>>` | `Option<Vec<f32>>` (residue: not `ParamSlot`) | `Vec<ParamSlot>` on instance |
| Clamp policy | UI clamps to note range | core clamps to def range | one resolver, reshape-aware |
| Modulation | `resolve_param_in` (note-aware) | hand-rolled walk (def-only) | one walk via `resolve_param_in` |
| Runtime type | `ChainGraph` | `JsonGraphGenerator` | one `PresetRuntime` |
| Addressing | `GraphTarget::Effect(EffectId)` | `GraphTarget::Generator(LayerId)` | **stays** — a real difference |

**The one genuine difference that must NOT collapse:** addressing. An effect is
one item in a layer's ordered, groupable chain, addressed by `EffectId`. A
generator is the layer's single source, addressed by `LayerId`. `GraphTarget`
stays a two-variant enum. Everything else above is duplication and collapses.

---

## Architectural target

### One instance type: `PresetInstance` (manifold-core)

A single struct replaces both. The union of the two structs' fields, with the
real chain-membership fields made honest (a generator is not in a chain, so it
has no group and no chain id):

```rust
pub struct PresetInstance {
    pub kind: PresetKind,            // Effect | Generator — the only fork carrier
    pub type_id: PresetTypeId,       // replaces EffectTypeId / GeneratorTypeId
    pub enabled: bool,
    pub collapsed: bool,

    pub param_values: Vec<ParamSlot>,
    pub base_param_values: Vec<ParamSlot>,   // residue fixed: ParamSlot both buses
    pub drivers: Option<Vec<ParameterDriver>>,
    pub envelopes: Option<Vec<ParamEnvelope>>,   // inline for BOTH kinds now
    pub ableton_mappings: Option<Vec<AbletonParamMapping>>,

    pub bindings: Vec<UserParamBinding>,     // ALWAYS inline, ALWAYS present
    pub param_mappings: Vec<ParamMapping>,
    pub param_mappings_version: u32,

    pub graph: Option<EffectGraphDef>,       // None = use catalog default
    pub graph_version: u32,
    pub graph_structure_version: u32,

    // Effect-chain membership. None/synthetic for a generator. These are the
    // real differences GraphTarget already encodes; they live here only so the
    // chain can carry one element type.
    pub id: PresetInstanceId,                // EffectId-shaped; generator = layer-derived
    pub group_id: Option<EffectGroupId>,
}
```

Legacy flat fields (`legacy_param0..3`, `legacy_param_version`) do **not** move
to the new struct. They exist only to read pre-1.1 JSON; that reading moves into
the load migration (Phase 5) and the fields are deleted.

### The decoupling decision (this is the crux the last attempts dodged)

Binding storage is **decoupled from graph materialization.** `bindings` is an
inline list that is always present and tiny; `graph: None` still means "pristine,
use the catalog, perform-path unchanged." A binding does **not** force
`graph = Some(...)`.

This is the call that unblocks everything. The previous plan assumed the effect
fold-in required lifting the canonical graph into every effect (lighting the
MODIFIED badge on every effect, switching every effect to the divergent-rebuild
path on the live rig). That assumption is why Phase 1b was deferred seven times.
It is wrong. Bindings are data about *which inner params are exposed and how they
reshape*; they do not require a topology copy. Store them on the instance, resolve
their inner-node targets against the catalog graph when `graph` is `None` and
against the override when it is `Some` — exactly how static params already resolve
against `LoadedPresetView` today. Generators currently store bindings in
`preset_metadata` only because they always have a materialized graph; the new
home removes that accident.

### One runtime type: `PresetRuntime` (manifold-renderer)

`ChainGraph` and `JsonGraphGenerator` both already own a compiled `Graph` +
`ExecutionPlan` + executor and both already build through the shared
`pre_allocate_resources` / `graph_loader` pipeline. They differ in: input
handling (effect transforms an input texture; generator produces from nothing),
and the binding-apply source. `PresetRuntime` carries an optional input slot
(None ⇒ generator) and reads bindings from the one inline list. The `Generator`
trait and the `EffectChain` runtime surface collapse into it. `PresetKind` /
"has input" carries the two real behavioral differences.

### The `GraphHost` trait is deleted

`GraphHost` exists only to let call sites operate over two different structs
without knowing which. With one struct, there is nothing to abstract — call sites
take `&mut PresetInstance`. The trait, its two impls, `GeneratorHost`, and
`with_graph_host_mut`'s closure dance all delete. This deletion is the proof the
collapse is real; if `GraphHost` survives, the collapse didn't happen.

---

## Phased plan — dependency ordered, each phase atomic

Every phase: lands on its own branch off the current head, compiles clean, passes
all gates (below), and **deletes the code it replaces** before the next phase
begins. No phase leaves a fork standing "for the next phase to clean up."

### Phase 0 — Exact field reconciliation + safety net `[read-only]`

In the main context (no agents — read-only audit rule). Produce the precise
field-by-field map: every reader and writer of every `EffectInstance` and
`GeneratorParamState` field, the exact set of `Layer` fields that move onto the
instance (`generator_graph*`, the generator slice of `Layer.envelopes`), and the
list of every `EffectId`/`GeneratorTypeId`/`GraphHost` call site. Record the
Phase-0 `cargo test --workspace` baseline (the only expected failure is the known
`liveschool_ableton_mappings_resolve_to_stable_param_ids`). Capture the
canonical `Liveschool Live Show V6 LEDS.manifold` load as a golden round-trip
fixture: load → save → diff must be stable before any code changes.

Gate: the field map is complete (no "TBD"), baseline recorded, golden fixture
captured.

### Phase 1 — One `PresetTypeId` (manifold-core)

Introduce `PresetTypeId` and collapse the two registry keyings onto it. The
registry value type is already `PresetDef`; this unifies the key. Delete
`EffectTypeId` / `GeneratorTypeId` (or make one a transparent alias that is then
removed within the phase — no permanent alias). Update every signature.

Gate: workspace test == baseline; `check-presets`; the type-id newtypes are gone
from the tree (`rg` shows zero non-alias references).

### Phase 2 — `PresetInstance` struct, generator adopts it (manifold-core)

Define `PresetInstance` as above. Make a generator layer hold a
`PresetInstance { kind: Generator, .. }` — moving `Layer.generator_graph*` and
the generator's envelopes onto the instance, and `base_param_values` to
`Vec<ParamSlot>`. `GeneratorParamState` is deleted in this phase. The
generator-side modulation walk is repointed at `resolve_param_in` (which becomes
kind-generic in Phase 4; until then, a generator-kind branch inside the one
resolver — not a second function). The clamp bug dies here: generators stop
calling `clamp_param`; the reshape-aware resolver governs.

Gate: workspace test == baseline; the OilyFluid Speed range (load
`graphTests2.manifold`, widen to 10, confirm value reaches 10 and round-trips);
`GeneratorParamState` is gone.

### Phase 3 — Effects adopt `PresetInstance`, bindings inline (manifold-core)

Make the effect chain a `Vec<PresetInstance>` (`kind: Effect`). Move
`EffectInstance.user_param_bindings` into the inline `bindings` list and resolve
inner-node targets against the catalog graph when `graph` is `None` (the
decoupling decision). Effect envelopes move from `Layer.envelopes` onto the
instance, keyed by `param_id`. `EffectInstance` is deleted in this phase.

Gate: workspace test == baseline; an effect user-exposed binding survives a
re-fuse (the `82a75332` regression test); expose/unexpose round-trips; the MOD
badge stays off for a pristine effect with an exposed binding (proves decoupling).

### Phase 4 — One resolver, one modulation walk, delete `GraphHost` (core + playback)

`resolve_param_in` becomes `resolve_param` over `&PresetInstance` (kind-generic,
reshape-note-aware for both). The two modulation walks in
`manifold-playback/src/modulation.rs` collapse into one. Delete the `GraphHost`
trait, both impls, `GeneratorHost`, and the closure plumbing — call sites now
take `&mut PresetInstance` directly. The generator modulation hidden-max dies
here.

Gate: workspace test == baseline; a driver and an envelope on a generator param
both reach a widened reshape max; `GraphHost` is gone from the tree.

### Phase 5 — Load migration v1.4.0 → v1.5.0 (manifold-io)

The one permitted transitional mechanism. Add `migrate_v140_to_v150` to the
existing chain in `migrate.rs`. It rewrites old project JSON into the
`PresetInstance` shape: generator layers' `genParams` + `generatorGraph` +
generator-keyed envelopes fold into one instance object; effects' separate
`userParamBindings` fold into the inline `bindings`; legacy flat param fields
(`legacy_param0..3`, pre-1.1) are read and converted here, then the in-memory
fields that backed them are gone. Stamp `projectVersion = "1.5.0"`. This is a
one-way load conversion — after it, the old shape exists nowhere in memory.

Gate: the golden `Liveschool` fixture loads, renders, and round-trips; a corpus
of pre-1.5 fixtures (≥ the v1.0/1.2/1.3/1.4 cases already in
`migrate.rs` tests) each load and round-trip; unit tests per migration branch.

### Phase 6 — One runtime `PresetRuntime`, delete the second (manifold-renderer)

Collapse `ChainGraph` and `JsonGraphGenerator` into `PresetRuntime` with an
optional input slot. The `Generator` trait collapses into it. Both already share
`pre_allocate_resources`/`graph_loader`, so this is a boundary merge, not an
executor rewrite. The freeze/fusion compiler reads bindings from the one inline
list (the `82a75332` class is now structurally impossible). Delete
`JsonGraphGenerator`, the `Generator` trait, and the `GeneratorRegistry` fork.

Gate: full `cargo test --workspace`; all parity tests; `bundled_presets`
lib test (catches WGSL/binding errors check-presets misses); every shipped
effect and generator renders a frame.

### Phase 7 — Editing/UI command collapse + final sweep (editing + app + ui)

The mirror dances (`mirror_effect_side`, `prepare_generator_mirror`,
`apply_generator_mirror`, `unmirror_generator_side`) collapse into one `mirror`
over `&mut PresetInstance`. The two card-build blocks in
`state_sync.rs` collapse into one. Final `rg` sweep proving deletion: zero
`EffectInstance`, `GeneratorParamState`, `EffectTypeId`, `GeneratorTypeId`,
`GraphHost`, `GeneratorHost`, `JsonGraphGenerator`, `Generator` (trait) outside
their own historical mentions in docs.

Gate: full `cargo test --workspace`; `clippy --workspace -D warnings`; the
deletion sweep returns clean; real-app smoke test on Peter's machine (this
environment can't launch the GUI) — load `graphTests2.manifold` and the
`Liveschool` show, confirm the Speed range and visual parity.

---

## Verification gates (every phase)

1. `cargo test --workspace` — compared against the Phase-0 baseline. The ONLY
   acceptable failure is the pre-existing `liveschool_ableton_mappings_resolve_to_stable_param_ids`.
   Any other failure is a regression and blocks the phase.
2. `cargo run -p manifold-renderer --bin check-presets` — structural preset load.
3. `cargo test -p manifold-renderer --lib bundled_presets` — executes a frame;
   catches WGSL/binding errors check-presets cannot.
4. The golden `Liveschool` round-trip (load → save → stable diff).
5. For phases touching the renderer: the parity suite.

## Rollback contract

Each phase is one branch off the prior. A phase that fails a gate is reverted
whole, not patched forward. The load migration (Phase 5) is the only
irreversible-on-disk step; it does not run until Phases 1–4 are green, and it is
gated on the fixture corpus. Saving a v1.5 project and needing v1.4 again is a
git checkout of the binary, not a data problem (the migration is additive in the
version chain; old binaries refuse the newer `projectVersion`, they do not
corrupt it).

## Definition of done

- One instance type (`PresetInstance`), one type id (`PresetTypeId`), one runtime
  (`PresetRuntime`), one param resolver, one modulation walk, one card-build path,
  one mirror operation.
- `EffectInstance`, `GeneratorParamState`, `EffectTypeId`, `GeneratorTypeId`,
  `GraphHost`, `GeneratorHost`, `JsonGraphGenerator`, and the `Generator` trait
  are deleted from the tree.
- `GraphTarget` remains a two-variant enum (the one real difference).
- The four named bugs are covered by regression tests that would have failed
  before this plan.
- `docs/PRESET_UNIFICATION_PLAN.md` is updated to point here for the instance/
  runtime half, so its "COMPLETE" status stops being misleading.
