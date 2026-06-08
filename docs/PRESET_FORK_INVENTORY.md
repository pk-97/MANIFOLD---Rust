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
| #8 UI dispatch arms | ✅ DONE | `aa119f72`+`0161a0b1` (resolver + param-value arms + `ActiveInspectorDrag`), `79905d63` (all modulation/env/trim/target/env-range arms via `graph_env_dual_edit`/`graph_driver_dual_edit` + `with_preset_graph_mut`; the 2 asymmetries fixed — EnvModeToggle re-seeds `last_elapsed` on both kinds, EnvRandomJumpToggle matches param_id only; the 2 Ableton arms; the `ui_root` DropdownContext/ParamLabelRightClick pair + the 3 downstream Map/Unmap/OpenAbletonPicker action pairs). Grep: 0 paired `PanelAction::*(GraphParamTarget::Generator)` arms remain. |
| #9 state-sync config builders | ✅ DONE | `42b1f55a` (one `preset_to_config`; `effects_to_configs`/`gen_params_to_config` are thin adapters) |
| #10 editor snapshot entries | ✅ DONE | `662db82b` (one `graph_snapshot(&GraphTarget)`; one `CachedGraphSnapshot`; one `watched_graph_target`) |
| #11 Ableton dispatch | ✅ DONE | `a160f3b6` (one `Project::find_preset_instance` locate behind every per-target Ableton accessor) |
| #12 persistence migration | ✅ DONE | `686f759e` (one `for_each_preset_instance` walk over effects + gen_params) |
| #13 version accessors | ✅ KEPT (not a fork): `Layer::generator_graph_version` reads the layer's singleton generator's `graph_version` field; there is no effect-Layer equivalent (effects are a `Vec`, not a singleton on the layer), so nothing to unify. | — |
| #14 skip-mode (generator gap) | ✅ DONE | `ad843985` (generator render loop honors `SkipMode` via the shared `is_skipped_for`; transparent frame when gated) |
| #15 string-bindings (effect gap) | ✅ DONE | `ad843985` (effect chain `try_build` resolves `PresetMetadata.string_bindings` against the splice node_map + seeds defaults) |
| #16 base_param_values residue | ✅ DONE | `2d04de6a` (folded into `ParamSlot.base` + `base_tracked` bit; wire byte-identical, golden io round-trip green) |

**All 16 forks collapsed (2026-06-08).** Headless verification: workspace
`cargo check --all-targets` + `clippy -D warnings` clean; core/editing/io/
playback full suite green (exit 0) incl. the golden Liveschool io round-trip
(base wire byte-identical). Adversarial grep: 0 paired
`PanelAction::*(GraphParamTarget::*)` dispatch arms; no `set_param_base`/
`get_param_base`/`resolve_gen_param_slot`/`evaluate_gen_param_envelopes`/
`effect_type_registry`/`generator_type_registry`/`active_generator_graph_snapshot`
symbols survive; the only `GraphParamTarget::Generator` uses left are the single
resolver + two kind-specific picker-context reads (dispatch unified). The
remaining UI-layer behaviors (editor live drag, modulation config, generator
snapshot) compile/clippy/grep-clean but want a running app for visual confirm;
the one renderer execute failure (`WireframeDepthGraph` first-frame) is the
documented pre-existing in-flight-decomp fail, orthogonal to this work.

## How to finish — continuation recipe (post-compaction handoff)

State at handoff: forks #1–#7 done (table above); #8 ~25% (resolver +
param-value arms + DriverToggle + `ActiveInspectorDrag` + `DriverTarget::From`
landed). Pick up from here.

**Hard process rules (these caused real friction — obey):**
- Bash: ONE pre-approved command per call. NO `echo`/`sed`/`comm` inside a
  `;`/`|` compound, NO `cmd ... | rg | head` (split it). Redirect to
  `/tmp/...` UNQUOTED (a quoted `/tmp` target reads as a repo write → prompt).
  `cargo build/check/test/clippy` and `cargo run -p manifold-renderer --bin
  check-presets` are allowlisted as single commands. Commit messages: plain
  text, NO backticks/`$()`.
- Do NOT launch agent-spawning Workflows — their sub-agent shell calls prompt
  the user. Do reviews inline with `rg`.
- Per-batch gate: `cargo check --workspace --all-targets`, then
  `cargo clippy --workspace --all-targets -- -D warnings`, then for
  core/editing/io/playback changes `cargo test -p manifold-core -p
  manifold-editing -p manifold-io -p manifold-playback --lib` (io 37 carries
  the golden Liveschool round-trip). Commit + push each batch (always-green).
  Known pre-existing fails to ignore: WireframeDepthGraph first-frame execute,
  lut1d/watercolor parity, Liveschool FluidSimulation Ableton mapping.

**#8 remaining (crates/manifold-app/src/ui_bridge/inspector.rs) — the pattern:**
each paired action collapses to ONE arm `PanelAction::X(gpt, ..)` (match the
`GraphParamTarget` BOUND, not destructured) →
`let Some(target) = resolve_graph_target(gpt, editor_target, effective_tab,
active_layer, selection, project) else {..}` → route the body through
`project.with_preset_graph_mut(&target, |inst| ..)` for reads/live-edits and
the captured `target.clone()` inside the `MutateProject` closure (dual-write
— keep BOTH or the next snapshot stomps the live tweak), and emit the
existing GraphTarget-keyed command (`ChangeGraphParam`, driver/env commands
via `DriverTarget::from(&target)`). Then DELETE the `GraphParamTarget::
Generator` twin. Reference the 5 already-collapsed arms (ParamRightClick/
Snapshot/Changed/Commit, DriverToggle) for the exact shape.
Remaining actions: EnvelopeToggle (with_preset_graph_mut + `envelopes_mut`),
DriverConfig (DriverTarget + 6-case match unchanged), EnvParamChanged,
TrimChanged, TargetChanged, EnvRangeChanged (live dual-write editing the
driver/env on the instance), TrimSnapshot/Commit, TargetSnapshot/Commit,
EnvRangeSnapshot/Commit, EnvParamSnapshot/Commit (use the `*_snapshot` vars +
command), EnvModeToggle, EnvRandomJumpToggle. **FIX the two asymmetries while
collapsing (don't freeze the divergence): EnvModeToggle must reset
`last_elapsed` on BOTH kinds; EnvRandomJumpToggle's `&& e.enabled` env filter
must match on both — pick the correct one and apply uniformly.** Ableton arms
(AbletonTrimChanged/AbletonInvertToggle) collapse onto
`project.ableton_param_mappings_mut(&AbletonMappingTarget)` (already exists),
deriving the mapping target from `target`+tab — NOT via with_preset_graph_mut.
Also collapse ui_root.rs ParamLabelRightClick pair + DropdownContext::
{EffectParamContext,GenParamContext} → one (they open menus, no mutation).
Keep the `GraphParamTarget` enum (param_card.rs emits it); only dispatch
collapses.

**#9 (state_sync.rs):** replace `effects_to_configs` + `gen_params_to_config`
with one `preset_to_config(inst, kind, graph, ..)` — walk the def's params
then any user tail; surface is_toggle/is_trigger from the spec; exposed via
`inst.is_param_exposed`; append generator string_params; call the shared
`build_card_modulation`. Update the ~7 call sites + `editor_card_config`.

**#10 (content_thread.rs):** replace `active_graph_snapshot(eid)` +
`active_generator_graph_snapshot(lid)` with one `graph_snapshot(&mut self,
target: &GraphTarget)`; unify `CachedGeneratorGraphSnapshot` → one cache keyed
`(GraphTarget, version)` (version = the instance's `graph_version`); collapse
the two `watched_graph_*` fields to one `Option<GraphTarget>`. The A4 work
already routed the generator pristine path through `snapshot_for_view`; mirror
that for effects or converge both on the compositor's id-keyed
`graph_snapshot_for`/`outer_routings_for` (now serve generator ids too).

**E #11 (core/ableton_mapping.rs ~25 sites):** dispatch on the
MasterEffect/LayerEffect/GenParam target via the existing
`Project::ableton_param_mappings_mut` (the locate-fork already lives there) —
collapse the parallel hand-written locate sites onto it. **#12 (io/migrate.rs):**
fold the per-kind load migrations into one (effects + gen_params are both
`PresetInstance` now).

**F #14 skip-mode:** generators can't declare skip-when-amount-0. The effect
path is `chain_spec.rs is_skipped_for` + `SkipMode` on `LoadedPresetView`;
generators now HAVE views (A4) — wire generator skip-mode through the same
`SkipModeDef`/`is_skipped_for`. **#15 string-bindings:** effects can't expose
string params; the generator Generate path in preset_runtime.rs handles
`string_bindings` — generalize so the effect Transform path also reads
`PresetMetadata.string_bindings`.

**B2 #16 (effects.rs):** fold `base_param_values: Option<Vec<f32>>` into
`ParamSlot` (add a `base: f32` field), eliminating the parallel Vec +
`ensure_base_values` length-sync. CRITICAL: keep the on-disk JSON wire
identical — `serialize_base_param_values` + the `FloatValuesWire` round-trip
stay byte-identical via a versioned load migration; gate on io round-trip +
check-presets. Keep `reset_effectives` (modulation.rs hot path) alloc-free.
Touch every base_param_values access (~25 in effects.rs + project.rs,
layer.rs, clipboard.rs, tests).

**FINAL:** full `cargo test --workspace` (minus known fails), clippy, mark
remaining forks DONE in the table above with commits, adversarial review of
the whole diff (inline `rg` for surviving `GraphParamTarget::Generator` /
`::generator::` / `gen_params_to_config` / `active_generator_graph_snapshot`).

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
