# Bindings Unification Plan

**Status:** All 5 phases shipped 2026-05-17 (`1decd1a4` → `9073daa9`).
This document is now the historical record of the work. See
`docs/EFFECT_RUNTIME_UNIFICATION.md` §7.11 for the closure summary
and `MEMORY.md → project_bindings_unified_2026_05.md` for the
agent-readable invariants future contributors should preserve.

**Goal:** Collapse every parallel-tier path between "static spec bindings"
and "per-instance user bindings" so that one source serves both at every
layer. Phase 1 collapsed the chain-graph runtime. Phase 2 collapses the UI
↔ bridge wire format. Phases 3–4 sweep the cousins (outer routing display,
convert enum) that naturally follow.

## 0. Motivation

Two parallel structures were doing the same job at two different layers. The
runtime layer was collapsed in Phase 1 (`1decd1a4`). The UI wire-format
layer is still split as of 2026-05-17 — that's the live bug Phase 2 fixes.

### 0.1 Runtime layer (Phase 1 — closed)

Before Phase 1 the chain-graph runtime had two parallel structures:

| Layer | Static path | User path |
|---|---|---|
| Type | `ParamBinding` | `UserParamBindingRuntime` |
| Storage on `EffectSlot` | `resolved_bindings: Vec<ParamBinding>` | `user_bindings: Vec<UserParamBindingRuntime>` |
| Cache | `LastAppliedCache.static_outer` | `LastAppliedCache.user_outer` |
| Apply loop | Walk static slice | Walk user slice |
| Default seed | `apply_binding_defaults` (static only) | (nothing — latent bug) |
| Audit | `audit_outer_inner_param_drift_report` (static only) | (nothing) |
| Editor routing display | `outer_routings_from_bindings` (static only) | (separate / partial) |
| Hydrate helper | `ParamBinding::resolve_handles` | `user_binding_to_runtime{_with_handles}` |
| Convert enum | `ParamConvert` (6 variants) | `UserParamConvert` (4 variants) |

The `&[]` bug — passing an empty slice for user bindings in
`ChainGraph::run` — compiled, ran, and passed every existing test for weeks
before a user reproduced it visually. One source had two doors; one door was
nailed shut.

After Phase 1 the runtime walks one `Vec<ResolvedBinding>` per slot; the
`&[]` bug class is structurally unrepresentable.

### 0.2 UI ↔ bridge layer (Phase 2 — open)

The bug Phase 1 patched at the runtime had a sibling at the UI wire format.
`PanelAction` per-param variants (driver toggle, envelope toggle, beat-div
change, waveform change, ADSR edit, range trim, Ableton mapping, expose
on/off — ~30 sites total) carry a positional `pi: usize`. The bridge resolves
`pi → ParamId` through:

```rust
fn effect_param_id(et: &EffectTypeId, pi: usize) -> Option<&'static str> {
    manifold_core::effect_definition_registry::param_index_to_id(et, pi)
}
```

`param_index_to_id` is the **static-tier registry only**, built once at
startup from `inventory::submit!`. User-exposed bindings live on
`EffectInstance.user_param_bindings` at `pi ≥ n_static` and aren't in the
registry. For any user-tail `pi` the lookup returns `None`, every modulation
handler's `if let Some(pid) = …` guard silently skips, every modulation
button on every exposed slider is dead. On stage: any slider you bothered to
expose can't be driven, enveloped, or mapped.

Same parallel-tier smell, one layer up. The fix family has three tiers:

1. **Bandaid** — per-call-site `if pi < n_static { ... } else { ... }`.
   30 sites, 30 forks.
2. **Moderate** — unify the helper. One lookup that knows both tiers.
   Bug class reduced to "anyone tomorrow who reaches for
   `param_index_to_id` directly re-introduces the hole."
3. **Structural** — eliminate the `pi → ParamId` translation. The UI emits
   `ParamId` on the wire. No positional-index step. Bug class gone.

Phase 2 is option 3. The positional `pi` was a leaky abstraction in the
first place — what the message actually means is "the user clicked this
specific parameter," and the parameter's identity is its `ParamId`, not its
position in a rendered list.

### 0.3 Why this still matters after Phase 1

External addressing already keys on `ParamId` at the data-model layer
(drivers, envelopes, Ableton, OSC all string-compare on `param_id`). But
the *UI* still uses positional indices on the wire — that's the gap
Phase 2 closes. After Phase 1+2, every layer from "user clicks slider" to
"inner-node param receives value" runs on one id space; no tier-aware
lookup anywhere.

## 1. Invariants (do NOT change)

These keep the migration safe — anything in this list is out of scope and the
plan must preserve all of them.

- **External addressing keys on `ParamId`.** Drivers, envelopes, OSC, MIDI,
  Ableton macros, macro mappings all look up params by stable string id.
  After unification, the id namespace and lookup helper are unchanged.
- **`EffectInstance.param_values` stays positional.** A `Vec<ParamSlot>`,
  hot path indexes directly. Static prefix `[0..n_static)`, user tail
  `[n_static..n_static+n_user)`. Save format is unchanged.
- **Source-of-truth split stays tiered.** Static bindings live on the
  compile-time `ChainSpec` (in `inventory::submit!` blocks). User bindings
  live on `EffectInstance.user_param_bindings` (serialised under
  `userParamBindings` in project files). These have genuinely different
  lifetimes and persistence stories.
- **UI tier distinction stays.** The effect card distinguishes
  "registry-exposed params" (always visible per the spec) from
  "user-exposed inner-graph params" (toggleable via the graph editor's expose
  checkboxes). That affects right-click menus and the
  `ToggleEffectParamExposeCommand` logic. UI surface ≠ runtime storage.
- **`ParamSlot { value, exposed }` stays uniform.** Both tiers use the same
  slot type. The `exposed` flag is per-slot, applied uniformly.

## 2. Target architecture

After the migration, at chain build time:

```
spec.bindings (&'static [ParamBinding])        ─┐
                                                ├─► slot.bindings: Vec<ResolvedBinding>
fx.user_param_bindings (Vec<UserParamBinding>) ─┘
```

Each `ResolvedBinding` is fully resolved (target is `Node { node_id, param }`
or `Composite { handle, outer_name }`; no `HandleNode` left at runtime).
Both source variants flow through one hydration function that takes the
splice's `handles` slice and emits a `ResolvedBinding`.

Per frame:

```rust
apply_bindings(
    &slot.bindings,               // ONE slice
    &fx.param_values,
    &mut slot.binding_cache,      // ONE cache vec
);
```

One walk. One skip-on-unchanged check. One write path. Cannot forget a tier
because there is no second tier at runtime.

## 3. Phase plan

Five phases, ~6-8 hours total. Each phase ends with `cargo test --workspace`
and `cargo clippy --workspace -- -D warnings` green. Phases can ship
separately; later phases depend on earlier ones, but Phase 2 is independent
of Phases 1, 3, and 4 — it can ship in any order relative to the runtime
collapse.

---

### Phase 1 — Core runtime unification — `[completed 2026-05-17, 1decd1a4]`

**Goal:** One binding type, one cache, one apply loop, one default-seed call,
one audit walk. After this phase, the `&[]` bug class is unrepresentable.

**Estimated effort:** 2-3 hours, one commit.

**Files touched:**
- `crates/manifold-renderer/src/node_graph/param_binding.rs` (largest diff)
- `crates/manifold-renderer/src/node_graph/mod.rs` (exports)
- `crates/manifold-renderer/src/effect_chain_graph.rs` (apply call site, `EffectSlot`)
- `crates/manifold-renderer/src/node_graph/chain_spec.rs` (audit walk)

**Steps:**

1. Introduce `ResolvedBinding`:
   ```rust
   pub struct ResolvedBinding {
       pub id: ParamId,
       pub label: Cow<'static, str>,
       pub default_value: f32,
       pub target: ResolvedTarget,   // Node | Composite | Custom — NO HandleNode
       pub convert: ParamConvert,
       pub source: BindingSource,    // Static | User (for audit / editor only)
   }
   ```
   `BindingSource` is metadata, not a behavioural switch — apply doesn't
   branch on it.

2. Add `ResolvedBinding::from_static(&ParamBinding, &handles)` and
   `ResolvedBinding::from_user(&UserParamBinding, &handles, &graph)`
   constructors. Both call the same internal resolver. Both return
   `Option<ResolvedBinding>` so unresolvable bindings can log + drop.

3. Replace `LastAppliedCache.{static_outer, user_outer}` with one
   `entries: Vec<BindingCacheEntry>`. Update `seed_from_bindings` to take a
   single `&[ResolvedBinding]` slice. Reset semantics: clear the tail
   starting at index `n_static` on user-bindings version bump.

4. Refactor `apply_param_bindings` → `apply_bindings(bindings, values,
   cache)`. One loop, one skip check. The `&[]` second slice goes away.

5. Update `apply_binding_defaults` to walk the unified slice — this silently
   closes the latent "user binding default not seeded" bug at the same time.

6. Refactor `EffectSlot`:
   ```rust
   bindings: Vec<ResolvedBinding>,     // unified, length n_static + n_user
   n_static: usize,                    // boundary index for rehydrate
   user_bindings_version: u32,
   binding_cache: LastAppliedCache,
   ```
   Drop the separate `resolved_bindings` and `user_bindings` fields.

7. Update `ChainGraph::run`'s per-frame rehydrate to rebuild only the tail
   `[n_static..)` when `fx.user_param_bindings_version` advances.

8. Update `audit_outer_inner_param_drift` to walk the unified list. Add the
   `BindingSource` to the audit output so the report stays human-readable.

**Test gates:**
- All existing tests pass.
- Existing regressions still pass:
  - `binding_seed_tests::soft_focus_inner_blur_starts_at_binding_default_not_primitive_default`
  - `topology_hash_tests::hash_changes_when_skip_predicate_flips`
  - `topology_hash_tests::disabled_effects_are_excluded_from_active_set_and_change_hash`
  - `topology_hash_tests::stateful_effects_never_skip`
  - `user_binding_tests::build_time_hydrate_resolves_user_binding_to_inner_node`
  - `user_binding_tests::exposed_rotation_slider_value_reaches_inner_transform`
- New test: build a chain with both static spec bindings AND a user binding
  on the same effect, assert `slot.bindings.len() == n_static + n_user` and
  both halves apply to the inner graph on a single `apply_bindings` call.
- New test: user-binding `default_value` mismatch case — expose an inner
  param whose binding default differs from the primitive's `ParamDef::default`,
  verify the inner node starts at the binding default after chain build
  (catches the latent symmetric bug to `518436a7`).

**Rollback:** revert single commit. The change is internal — no save-format
or external API change, so a revert is safe at any point.

---

### Phase 2 — UI ↔ bridge wire-format collapse — `[completed 2026-05-17, dfbeb1f1]`

**Goal:** Eliminate the positional `pi → ParamId` translation step at the
UI ↔ bridge boundary. `PanelAction` variants that address a single
parameter carry `ParamId` directly. After this phase, no UI handler does
a positional-index lookup; the bug class is structurally gone — no
"third tier" can be added tomorrow that forgets to walk both sides,
because there's no positional index left to mis-resolve.

**Motivation:** Same parallel-tier smell as Phase 1, one layer up.
`effect_definition_registry::param_index_to_id` is static-tier only;
for `pi ≥ n_static` it returns `None`, every per-param modulation
handler silently no-ops. User-visible symptom (reproduced 2026-05-17):
on any effect card with an exposed inner-graph param, clicking the
driver / envelope / Ableton-map button does nothing.

**Estimated effort:** 1.5-2 hours, one commit (optional split: UI/panel
side first, then bridge-handler sweep).

**Files touched:**
- `crates/manifold-ui/src/panels/mod.rs` (`PanelAction` enum variants)
- `crates/manifold-ui/src/panels/effect_card.rs` (emit `ParamId` on click)
- `crates/manifold-ui/src/panels/generator_panel.rs` (sibling fix for
  generator params — same shape via `generator_param_id`)
- `crates/manifold-app/src/ui_bridge/inspector.rs` (drop the lookup
  helpers, simplify ~30 handler sites)
- `crates/manifold-app/src/ui_bridge/mod.rs` (any `PanelAction` matchers
  that destructure the per-param variants)

**Steps:**

1. **Survey the offending `PanelAction` variants.** Every variant
   currently spelled `Effect*(effect_index: usize, pi: usize, …)` or
   `Layer*(pi: usize, …)` for a *single param address*. Scan:

   ```bash
   rg 'PanelAction::(Effect|Layer)\w+\([^)]*\busize\b' crates/manifold-ui/
   ```

   Expected variants (non-exhaustive — confirm at start of work):
   driver toggle, driver waveform, driver beat-div, driver trim,
   driver reverse, envelope toggle, envelope ADSR fields, envelope
   target, envelope range, envelope mode, Ableton mapping, expose
   param, static-expose param, modulation-clear menu items.
   ~30 sites total per the inspector.rs grep.

2. **Change each variant's per-param `usize` to `ParamId`.** ParamId
   is already `Cow<'static, str>` — Borrowed for compile-time
   registry ids, Owned for user-bound ids. The enum variant carries
   the id by value; the bridge handler receives it by reference at
   match time. No allocation in the hot path: dispatch is one-shot
   per click.

3. **Effect card emits `ParamId` at click time.** The panel already
   threads per-param metadata (`EffectCardConfig.params[i]`) through
   render. Add `param_id: ParamId` to each per-param row of the
   config (it's already present as `id: &'static str` for static
   bindings; extend to carry `ParamId` covering both tiers). At click
   detection, the panel converts `pi → param_id` once using its own
   config (no registry lookup, no tier branching). The emitted
   `PanelAction` carries `param_id.clone()`.

4. **Generator panel parallel fix.** Same change for `generator_panel.rs`
   — generator params have the same static-tier-only
   `generator_param_id` helper and the same exposed-binding gap.

5. **Bridge handlers simplify.** Each handler today opens with:
   ```rust
   PanelAction::EffectDriverToggle(ei, pi) => {
       let pid = effect_param_id(fx.effect_type(), *pi);
       if let Some(driver_idx) = pid.and_then(...) { ... }
       else if let Some(pid) = pid { ... }
       // ← silent no-op on user tail
   }
   ```
   Becomes:
   ```rust
   PanelAction::EffectDriverToggle(ei, param_id) => {
       let driver_idx = fx.drivers.as_ref()
           .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id));
       if let Some(di) = driver_idx { ... }
       else { /* create new driver with this param_id */ }
   }
   ```
   The `Option<&str>` lookup is gone. The `None` arm is gone. Every
   call to `effect_param_id` / `generator_param_id` in inspector.rs
   gets deleted.

6. **Delete `effect_param_id` and `generator_param_id`.** Final sweep:
   ```bash
   rg 'effect_param_id|generator_param_id' crates/
   ```
   Expected: zero hits after this phase. (Registry-side
   `param_index_to_id` stays — it's a legitimate primitive used
   elsewhere; it's only the UI-bridge over-reliance on it that's
   wrong.)

7. **`AddDriverCommand::base_value` access.** Currently reads
   `fx.param_values[*pi]` to seed `base_value`. After the change,
   resolve the value index from `ParamId` via the existing helper
   `EffectInstance::param_id_to_value_index(param_id)` — already
   tier-aware. No new helper needed.

**Test gates:**
- All Phase 1 tests pass.
- New regression: build a Mirror card with `Transform.rotation`
  exposed; dispatch a `PanelAction::EffectDriverToggle(0,
  ParamId::Owned("user.uv_transform.rotation.1"))`; assert
  `fx.drivers` contains one entry with the matching `param_id` and
  `enabled = true`.
- New regression: same shape for `EffectEnvelopeToggle` —
  `fx.envelopes` (or the layer envelopes vec, depending on tab)
  gains an entry keyed by the owned `ParamId`.
- New regression: a generator card with an exposed user-tail param
  (or, if generators don't yet support user-exposed params, the
  smallest equivalent) — driver toggle reaches the right vec.
- Manual verification: open a Mirror card, expose
  `Transform.rotation`, click the driver button on the exposed
  slider → driver UI appears, modulation activates. Click envelope →
  envelope expands, ADSR drawer fills.

**Rollback:** revert single commit. The wire format is in-process —
no save format change, no FFI / IPC consumers of `PanelAction`. Safe
at any point.

**Acceptance criteria for Phase 2:**
1. `effect_param_id` and `generator_param_id` are deleted; grep
   confirms zero call sites in `crates/`.
2. No `PanelAction` variant addresses a single param by `usize`. The
   `usize` for "effect index in the chain" stays — that's structural
   position, not param identity.
3. The bridge handlers no longer have an `if let Some(pid) = …`
   guard around per-param work. `ParamId` arrives non-`Option`.
4. A 31st handler added tomorrow cannot reintroduce the bug because
   the wire format gives it `ParamId` directly. The `pi → ParamId`
   translation step doesn't exist.

---

### Phase 3 — Outer routing + handle-resolution cleanup — `[completed 2026-05-17, 6070031e]`

**Goal:** The graph editor's "Effect Parameters" panel and the outer-card
routing display walk one list. `ParamTarget::HandleNode` becomes a parse-time
construct only.

**Estimated effort:** 1-2 hours, one commit.

**Files touched:**
- `crates/manifold-renderer/src/node_graph/param_binding.rs`
- `crates/manifold-renderer/src/node_graph/snapshot.rs` (`OuterParamRouting`)
- `crates/manifold-ui/src/panels/graph_editor.rs` (panel rendering)
- `crates/manifold-app/src/app_render.rs` (configure call site)

**Steps:**

1. Extend `outer_routings_from_bindings` to take `&[ResolvedBinding]` and
   walk all of them. Each entry's `BindingSource` becomes a field on
   `OuterParamRouting` so the UI can still style "static" vs "user-exposed"
   differently (different chip colours, different right-click menus) — but
   the iteration is one walk.

2. Update the graph editor's "Effect Parameters" panel to render one
   read-only list of `OuterParamRouting`s. The current dual-source
   construction in `app_render.rs` (`build_card_entries` /
   `build_static_block_targets` / `build_card_exposures`) collapses to a
   single helper.

3. Audit `user_binding_to_runtime` (the global-graph-handles variant) call
   sites. After Phase 1 the only caller is the canonical-graph snapshot
   builder (`ChainSpec::build_canonical_graph`), which registers handles
   globally on the standalone graph it builds. That call site can either
   keep using the global-handles variant, OR migrate to
   `_with_handles` for symmetry. Pick one, delete the other.

4. Remove `ParamTarget::HandleNode` from the runtime `ResolvedTarget` enum.
   `HandleNode` only ever appears in source data on
   `ChainSpec.bindings[i].target`; resolution at chain build time always
   produces `Node` or `Composite`. The defensive
   "unresolved HandleNode at apply time" `Err` arm in the old
   `ParamBinding::apply` (which currently logs a developer error) becomes
   unrepresentable.

**Test gates:**
- All Phase 1 tests pass.
- New test: `outer_routings_from_bindings` on a slot with both static and
  user bindings returns entries for both, tagged with the correct
  `BindingSource`.
- Manual verification: open graph editor on Mirror, expose `Transform.rotation`
  on the card, verify the "Effect Parameters" panel shows BOTH "Amount" (static)
  AND "Rotation" (user) in one list with appropriate styling.

**Rollback:** revert single commit. No external API change.

---

### Phase 4 — Convert simplification — `[completed 2026-05-17, 9073daa9]`

**Goal:** Eliminate dead `ParamConvert` variants and unify the
core-side `UserParamConvert` with the renderer-side `ParamConvert`.

**Estimated effort:** 1-2 hours, one commit (possibly split into 2 if
`EnumRemap` / `FloatTransform` still have a remaining caller).

**Files touched:**
- `crates/manifold-renderer/src/node_graph/param_binding.rs` (drop variants)
- `crates/manifold-core/src/effects.rs` (remove `UserParamConvert`, import from renderer or move enum to core)
- `crates/manifold-editing/src/commands/effects.rs` (`UserParamConvert` references)
- Any remaining effect file with `ParamConvert::EnumRemap` or `FloatTransform`

**Steps:**

1. **Audit `EnumRemap`:** `rg 'ParamConvert::EnumRemap' crates/`. Expected:
   zero hits after the recent curation-elimination work. If any remain,
   migrate them (the conversion logic lives in the primitive, the binding
   becomes plain `EnumRound`). Delete the variant.

2. **Audit `FloatTransform`:** `rg 'ParamConvert::FloatTransform' crates/`.
   Expected: zero or near-zero. The recent Transform.rot and Strobe.rate
   migrations moved unit conversion into the primitive. Migrate any
   remaining users. Delete the variant.

3. **Merge `UserParamConvert` into `ParamConvert`:** today they're two
   parallel enums (`Float | IntRound | BoolThreshold | EnumRound` on the
   core side, plus `EnumRemap | FloatTransform` on the renderer side). After
   step 2, both sides have the same variants. Move `ParamConvert` to
   `manifold-core` (where it can be referenced by both `core::effects` and
   `renderer::node_graph`) and delete `UserParamConvert`.

4. Update the audit categories: with `EnumRemap` and `FloatTransform`
   removed, the `[CURATED]` row in `audit_outer_inner_param_drift_report`
   can no longer appear. Update the test to assert `curated == 0`
   structurally (it already does dynamically — make it a static guarantee).

**Test gates:**
- All Phase 1-3 tests pass.
- Workspace clippy clean (deleting enum variants may surface dead match arms
  elsewhere).
- Audit test: `curated == 0` (already enforced).

**Risk:** higher than Phases 1-3 because crossing crate boundaries
(`manifold-core` ↔ `manifold-renderer`). Mitigate by doing the enum move as
the very first sub-step, before touching call sites.

**Rollback:** revert single commit. If the enum move spans multiple commits
for review, each is independently revertable.

---

### Phase 5 — Documentation + closure — `[completed 2026-05-17]`

**Goal:** Capture the new invariants in code comments and reference docs.
No behaviour change.

**Estimated effort:** 30-60 minutes.

**Files touched:**
- `crates/manifold-core/src/effects.rs` (doc on `EffectInstance.param_values`
  and on `EffectInstance::param_id_to_value_index`)
- `crates/manifold-renderer/src/effect_chain_graph.rs` (doc on
  `EffectSlot.bindings`)
- `crates/manifold-ui/src/panels/mod.rs` (doc on `PanelAction` per-param
  variants: `ParamId` is the wire format, not `usize`)
- `docs/EFFECT_RUNTIME_UNIFICATION.md` (update §7 to reflect the unified
  model — runtime AND UI wire format)
- This file (mark phases completed)

**Steps:**

1. Add a doc comment on `EffectInstance.param_values` stating the layout
   invariant explicitly:
   > Positional. The first `effect_definition_registry::get(&effect_type).param_count`
   > slots correspond to the effect's static-spec bindings in declaration
   > order. The remainder correspond to `user_param_bindings` in declaration
   > order. After bindings unification this layout maps directly onto
   > `EffectSlot.bindings[i]` — no parallel structure to keep in sync.
   > Resolve `ParamId → index` via [`Self::param_id_to_value_index`]; that
   > helper is the single tier-aware lookup the codebase relies on.

2. Add a doc comment on `EffectSlot.bindings` referencing the same
   invariant.

3. Add a doc comment on the per-param `PanelAction` variants making the
   wire-format contract explicit:
   > Per-param `PanelAction` variants carry `ParamId`, never a positional
   > index. The id round-trips static and user tiers; the bridge resolves
   > it to a value-slot index via `EffectInstance::param_id_to_value_index`
   > only when it actually needs to read `param_values`. Positional
   > indices on the wire would re-introduce the Phase 2 bug class (a
   > registry-only lookup returning `None` for user-tail params).

4. Update `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 (the original spec for the
   binding system) to describe the unified runtime model AND the
   `ParamId`-keyed wire format. Note the source tier distinction (registry
   vs project file) is preserved but the runtime is one list and the wire
   carries one id space.

5. Add an entry to `MEMORY.md` referencing the new architecture so future
   agents don't re-propose the unified change after compaction. Suggested
   slug: `project_bindings_unified_2026_05.md`. Include both the runtime
   collapse (Phase 1) and the wire-format collapse (Phase 2).

6. Mark this plan's phases as `[completed]` in section 3 above.

**Test gates:** N/A (docs only).

---

## 4. Out-of-scope follow-ups

Observations surfaced during planning, not work items:

- **Strategic alignment with the effect-to-preset migration** (memory:
  `project_effect_to_preset_migration.md`). The unification removes a major
  obstacle to making `ChainSpec.bindings` data-driven, but doesn't itself
  do the data-driven migration. Revisit when Phase 3 of the preset plan
  starts.

- **"Static prefix + user tail" pattern audit elsewhere.** After Phases 1+2
  land, `rg 'static.*user|n_static|param_count' crates/` will surface other
  places that walk the two tiers as if they're separate concepts. Most of
  those are correct (they're touching the source-of-truth split, which IS
  tiered); a few may be incidental. Quick scan after Phase 5 lands.

- **Driver / envelope storage tier.** Drivers and envelopes for static
  effect params live on `EffectInstance.drivers / envelopes`. For
  user-exposed inner-graph params they... still live there, addressed by
  the user-binding's `ParamId`. After Phase 2 this Just Works (string
  compare on `param_id`), but the lifetime story for user-binding-targeted
  drivers when a user *un*-exposes the param is worth a doc note: today
  the driver becomes orphaned (stays in `fx.drivers`, never matched, never
  applied). Decide whether to gc orphans on `ToggleEffectParamExposeCommand`
  or leave them for re-bind. Not blocking — flag it.

## 5. Verification: did the unification actually work?

Post-migration acceptance criteria, grouped by phase:

### Runtime (Phases 1, 3, 4)

1. **`&[]` bug is unrepresentable.** Grep the codebase for
   `apply_param_bindings.*\[\]` and similar — there is no second slice to
   forget.
2. **One audit catches both tiers.** A test that exposes an unresolvable
   user binding (typo'd handle name) fails
   `audit_outer_inner_param_drift_report` with a clear `[UNRESOLVED]` row.
3. **Default seeding covers both tiers.** A test that exposes an inner
   param with a non-matching default fails without the unified seed call,
   passes with it (mirrors the `518436a7` regression test, extended to user
   bindings).
4. **No `UserParamBindingRuntime` type remains.** Search the codebase: zero
   hits.
5. **No `UserParamConvert` type remains.** Search the codebase: zero hits.
6. **No `ParamTarget::HandleNode` outside `ChainSpec` source data.** The
   runtime resolved-target enum has only `Node | Composite | Custom`.

### UI ↔ bridge (Phase 2)

7. **No `pi → ParamId` translation in the bridge.** `rg
   'effect_param_id|generator_param_id' crates/` returns zero hits.
8. **No `PanelAction` variant addresses a per-effect param by `usize`.**
   Visual inspection of `crates/manifold-ui/src/panels/mod.rs` — every
   per-param variant carries `ParamId` (or owns one indirectly via a
   wrapping struct).
9. **Modulation works on exposed params end-to-end.** Manual test: open a
   Mirror card, expose `Transform.rotation`, click the driver button →
   driver appears in `fx.drivers` with `param_id` matching the user
   binding's owned id; the driver writes through the chain runtime; the
   image rotates per the driver waveform.
10. **Same for envelopes and Ableton mapping** on the same exposed param.

If any of those don't hold at the end of Phase 5, the unification missed
something and the work isn't done.

## 6. Order of operations

```
                           Phase 1  ──┬──► Phase 3  ──► Phase 4 ──┐
(completed 2026-05-17)               │                            │
                                  all tests                       ▼
                                   green                       Phase 5
                                                                  ▲
                           Phase 2  ──────────────────────────────┘
                                  │
                              all tests
                               green
```

- Phase 1 closed the runtime split (shipped, `1decd1a4`).
- Phase 2 closes the UI wire-format split. **Independent of Phase 1** and
  of Phases 3/4 — can ship next, or in parallel with the cleanup phases.
- Phase 3 (outer routing) depends on Phase 1's `ResolvedBinding` type.
- Phase 4 (convert simplification) depends on Phase 1; independent of 2 & 3.
- Phase 5 (documentation) depends on all prior phases being complete (it
  documents the final state).

The user-facing impact ordering — what unblocks the instrument on stage
soonest:

1. **Phase 2** unblocks modulation on every exposed slider — the loudest
   live bug right now.
2. Phase 3 polishes the graph editor (currently fine, mostly aesthetic).
3. Phase 4 is invisible to the performer; it's a developer-side cleanup.
4. Phase 5 is the paper trail.

So the recommended ship order is **Phase 2 next** even though it's
numerically "out of order" relative to the runtime trio.
