# Preset Effect/Generator Fork Inventory — the honest remaining-work map

**Why this doc exists.** `PRESET_UNIFICATION_PLAN.md` and `PRESET_INSTANCE_COLLAPSE_PLAN.md`
both carry "COMPLETE"/"DONE" status text that contradicts their own "still forked"
lists, and that contradiction is how eight attempts kept claiming the unification
was finished when it wasn't (the right-click "reset to default" snap-back on
generator sliders — item #5 below — is a fork the plan literally lists as unfixed).
This file is grep-verified against the current tree (2026-06-07, branch
`preset-collapse-phase5`) and is the source of truth for what is actually left.
Definition of **done** here is behavioral, not "it compiles": *a generator and an
effect run the same code path for a given action, provable by grep showing no
`GraphParamTarget::Generator` / effect-vs-generator-registry fork remaining.*

## Progress (2026-06-08, branch `preset-collapse-phase5`)

Every commit below is on the branch, pushed, and gated: `cargo check
--workspace --all-targets` + `clippy -D warnings` clean, and the headless
suites green (core 222 / io 37 incl. the golden Liveschool round-trip /
editing 13 / playback 81), plus the renderer generator sweep for A3/A4. The
one renderer failure (`WireframeDepthGraph` first-frame execute) is the
documented pre-existing in-flight-decomp fail, proven orthogonal.

| Fork | Status | Commit(s) |
|---|---|---|
| #1 definition store | ✅ DONE | `26dc2b02` (A1) |
| #5 param-id→slot snap-back | ✅ DONE (fixed at source + re-validated at UI dispatch) | `26dc2b02` (A1), `aa119f72` (D #8 ParamRightClick) |
| #2 type/picker registry | ✅ DONE | `edec3102` (A2) |
| #3 bundled loader | ✅ DONE | `dbdc88ef` (A3) |
| #4 LoadedPresetView | ✅ DONE | `36d2242f` (A4) |
| #6 duplicate accessors | ✅ DONE | `aca6a87d` (B1) |
| #7 modulation walk | ✅ DONE | `d63c60ca` (C) |
| #8 UI dispatch arms | 🔧 IN PROGRESS | `aa119f72`+`0161a0b1` (resolver + the 4 param-value arms incl. snap-back + `ActiveInspectorDrag` unified). Remaining: the modulation/env/trim/target/env-range arms (pure DRY via `DriverTarget::from(GraphTarget)` + `with_preset_graph_mut`), the 2 behavioral asymmetries (EnvModeToggle `last_elapsed`, EnvRandomJumpToggle `enabled` filter), the 2 Ableton arms, and the `ui_root` DropdownContext/ParamLabelRightClick pair. |
| #9 state-sync config builders | ⬜ TODO | |
| #10 editor snapshot entries | ⬜ TODO | |
| #11 Ableton dispatch | ⬜ TODO | |
| #12 persistence migration | ⬜ TODO | |
| #13 version accessors | ✅ KEPT (not a fork): `Layer::generator_graph_version` reads the layer's singleton generator's `graph_version` field; there is no effect-Layer equivalent (effects are a `Vec`, not a singleton on the layer), so nothing to unify. | — |
| #14 skip-mode (generator gap) | ⬜ TODO | |
| #15 string-bindings (effect gap) | ⬜ TODO | |
| #16 base_param_values residue | ⬜ TODO (B2 — serialization-gated) | |

**Note on #8 remaining + the UI batches (#9/#10):** these are
behaviorally-already-consistent (the snap-back and param resolution are
fixed at the source), so what's left is largely DRY of the inspector
dispatch + state-sync + snapshots. They are UI-layer and cannot be verified
headlessly — `cargo check`/`clippy`/grep gate the collapse, but the editor
canvas behavior (live drag, modulation config, generator snapshot) needs a
running app to confirm. The two EnvModeToggle/EnvRandomJumpToggle
asymmetries are the only genuine behavioral fixes left in #8.

## Already unified (verified)

| Layer | Evidence |
|---|---|
| Instance type — one `PresetInstance` both sides | `gen_params: Option<PresetInstance>` (core/layer.rs:90) |
| Runtime — one `PresetRuntime` | `effect_chain_graph.rs` + `generators/json_graph_generator.rs` deleted (commit 8d33c9d2) |
| Frame context — one `PresetContext` | renderer/preset_context.rs:23; `EffectContext`/`GeneratorContext` gone |
| Graph-home storage | `PresetInstance.graph` + `gen_params.graph` |
| Envelope storage + apply core | `apply_instance_envelopes` (playback/modulation.rs:209) |
| Expose/unexpose mirror; card-build modulation builder | `mirror_effect_side`; `build_card_modulation` |

## Still forked — structural (must collapse)

| # | Area | Effect side | Generator side | File:line | Why it bites |
|---|---|---|---|---|---|
| 1 | **Definition registry** (KEYSTONE) | `preset_definition_registry::effect` | `preset_definition_registry::generator` | manifold-core | every downstream fork picks a registry by kind |
| 2 | Type/picker registry | `effect_type_registry` | `generator_type_registry` | core/lib.rs:8,14 | two pickers; display-name resolution forks |
| 3 | Bundled loader | `node_graph::bundled_presets` | `generators::bundled_generator_presets` | renderer | two tables/loaders |
| 4 | **LoadedPresetView** | exists, effect-keyed | **absent** | renderer/node_graph/loaded_preset_view.rs:56 | generators re-resolve per build; the doc's "fix first" |
| 5 | **Param-id→slot resolution** | `param_id_to_value_index` / `resolve_param` / `set_base_param_by_id` (effect registry only) | `resolve_gen_param_slot` (Layer) | core/effects.rs:1557/1907/1936; core/layer.rs:319 | **right-click reset snap-back**; shared-looking methods silently no-op for generators |
| 6 | Duplicate index accessors | `set_base_param`/`get_base_param` (effects.rs:1704/1715) | `set_param_base`/`get_param_base` (effects.rs:2374/2387) | core/effects.rs | two names for one op; callers split ~14/13 |
| 7 | Modulation entry walks | `evaluate_all_envelopes` (modulation.rs:507) | `evaluate_gen_param_envelopes` (548) | playback | two walks + forked def lookup (shared apply core only) |
| 8 | UI dispatch arms | `GraphParamTarget::Effect` | `GraphParamTarget::Generator` | app/ui_bridge/inspector.rs (23 paired actions) | every param/driver/env/trim/target action coded twice |
| 9 | State-sync card build | `effects_to_configs` (state_sync.rs:1382) | `gen_params_to_config` (1619) | app | two parallel config builders |
| 10 | Editor snapshot entry | `active_graph_snapshot` (content_thread.rs:1231) | `active_generator_graph_snapshot` (1275) | app | two paths to one `GraphSnapshot::from_def` |
| 11 | Ableton dispatch | `MasterEffect`/`LayerEffect` | `GenParam` | core/ableton_mapping.rs (~25 sites) | one enum, three dispatch paths |
| 12 | Persistence migration | per-kind | per-kind | io/migrate.rs | two migrations to maintain |
| 13 | Version accessors | `graph_version` | `generator_graph_version()` | core/layer.rs:276 | parallel counters/wrappers |

## Still forked — capability gaps (feature exists one side only)

| # | Feature | Where | Gap |
|---|---|---|---|
| 14 | Skip-mode | effect-only (`is_skipped_for`, chain build) | generators can't declare "skip when amount=0" |
| 15 | String-bindings | generator-only (`PresetRuntime` Generate path) | effects can't expose string params (schema supports it) |
| 16 | `base_param_values` | `Option<Vec<f32>>` residue (effects.rs:575) | not the unified `Vec<ParamSlot>` shape |

## Dependency order (read: my analysis, not yet proven by doing it)

Most of #5–#13 fork *only* because they ask "which registry?". Collapse the
keystone first — #1–#4, ideally into the single disk-load loader
`PRESET_UNIFICATION_PLAN.md` describes — and #5–#13 largely fall out; then #8 (the
23 UI arms) collapses; then #14–#15 become one-line enables. #5 (the snap-back) can
be fixed in isolation, but doing it without #4 is a mirror-patch (another fork),
not a fix.

## Not re-verified (flagged, do not assume)

- **OSC scheme** — `PRESET_UNIFICATION_PLAN` claims it's unified to one scheme; not
  re-checked in this audit.
- The "fall out" dependency claim above is reasoning, not a proven build result.

## Remaining Phase-4 UI (separate from the forks above; backend already built)

- Picker lists project `embedded_presets` (browser_popup.rs).
- Duplicate/Make-unique action (PanelAction → ContentCommand → `ForkPresetCommand`;
  `source_def` via `loaded_preset_view_by_id(...).canonical_def` for pristine
  effects). Integration map in `PRESET_INSTANCE_COLLAPSE_PLAN.md` §"DEFERRED Phase 4 UI".
- Export/import menu (`rfd` + `manifold_io::preset_file`).
- (Variant header label — DONE, commit 6cf72f32.)
