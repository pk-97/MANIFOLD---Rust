# Bindings Unification Plan

**Status:** Draft 1, 2026-05-17. Implements the cleanup discussed after the
`&[]` user-binding bug (`49c80bd2`) exposed the parallel-path smell.

**Goal:** Collapse the runtime split between static spec bindings
(`ParamBinding`, declared on `ChainSpec`) and per-instance user bindings
(`UserParamBindingRuntime`, hydrated from `EffectInstance.user_param_bindings`)
into a single resolved-binding list per effect, then sweep the cousins that
naturally follow from that change.

## 0. Motivation

Today the chain-graph runtime has two parallel structures doing the same job:

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

External addressing (OSC, MIDI, Ableton macros, drivers, envelopes) already
keys on `ParamId` and walks both tiers transparently via
`param_id_to_value_index`. **The runtime split is the only place the
duality still lives.**

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
  "user-exposed inner-graph params" (togglable via the graph editor's expose
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

Four phases, ~4-6 hours total. Each phase ends with `cargo test --workspace`
and `cargo clippy --workspace -- -D warnings` green. Phases can ship
separately; later phases depend on earlier ones.

---

### Phase 1 — Core unification (the load-bearing change)

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

### Phase 2 — Outer routing + handle-resolution cleanup

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

### Phase 3 — Convert simplification

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
- All Phase 1-2 tests pass.
- Workspace clippy clean (deleting enum variants may surface dead match arms
  elsewhere).
- Audit test: `curated == 0` (already enforced).

**Risk:** higher than Phases 1-2 because crossing crate boundaries
(`manifold-core` ↔ `manifold-renderer`). Mitigate by doing the enum move as
the very first sub-step, before touching call sites.

**Rollback:** revert single commit. If the enum move spans multiple commits
for review, each is independently revertable.

---

### Phase 4 — Documentation + closure

**Goal:** Capture the new invariants in code comments and reference docs.
No behaviour change.

**Estimated effort:** 30-60 minutes.

**Files touched:**
- `crates/manifold-core/src/effects.rs` (doc on `EffectInstance.param_values`)
- `crates/manifold-renderer/src/effect_chain_graph.rs` (doc on `EffectSlot.bindings`)
- `docs/EFFECT_RUNTIME_UNIFICATION.md` (update §7 to reflect the unified model)
- This file (mark phases completed)

**Steps:**

1. Add a doc comment on `EffectInstance.param_values` stating the layout
   invariant explicitly:
   > Positional. The first `effect_definition_registry::get(&effect_type).param_count`
   > slots correspond to the effect's static-spec bindings in declaration
   > order. The remainder correspond to `user_param_bindings` in declaration
   > order. After bindings unification this layout maps directly onto
   > `EffectSlot.bindings[i]` — no parallel structure to keep in sync.

2. Add a doc comment on `EffectSlot.bindings` referencing the same
   invariant.

3. Update `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 (the original spec for the
   binding system) to describe the unified runtime model. Note the source
   tier distinction (registry vs project file) is preserved but the runtime
   is one list.

4. Add an entry to `MEMORY.md` referencing the new architecture so future
   agents don't re-propose the unified change after compaction. Suggested
   slug: `project_bindings_unified_2026_05.md`.

5. Mark this plan's phases as `[completed]` in section 3 above.

**Test gates:** N/A (docs only).

---

## 4. Out-of-scope follow-ups

Items 11 and 12 from the discussion are observations, not work items:

- **Strategic alignment with the effect-to-preset migration** (memory:
  `project_effect_to_preset_migration.md`). The unification removes a major
  obstacle to making `ChainSpec.bindings` data-driven, but doesn't itself
  do the data-driven migration. Revisit when Phase 3 of the preset plan
  starts.

- **"Static prefix + user tail" pattern audit elsewhere.** After Phase 1
  lands, `rg 'static.*user|n_static' crates/` will surface other places
  that walk the two tiers as if they're separate concepts. Most of those
  are correct (they're touching the source-of-truth split, which IS tiered);
  a few may be incidental. Quick scan after Phase 4 lands.

## 5. Verification: did the unification actually work?

Post-migration acceptance criteria:

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

If any of those don't hold at the end of Phase 4, the unification missed
something and the work isn't done.

## 6. Order of operations

```
Phase 1  ──┬──► Phase 2  ──┬──► Phase 3  ──► Phase 4
           │               │
       all tests       all tests
       green            green
```

Each phase is mergeable independently. Phases 2 and 3 depend on Phase 1.
Phase 4 depends on all prior phases being complete (it documents the final
state).
