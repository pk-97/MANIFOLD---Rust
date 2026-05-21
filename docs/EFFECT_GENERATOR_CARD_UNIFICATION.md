# Effect Card + Generator Card unification — audit

Status: **Phase 1 only.** Phase 2 (the fix) and Phase 3 (panel unification) have not run yet; this doc is the prerequisite for both.

Context: commit `a7fd4698` unified the **graph-editor side** of exposure (one `exposed_params` set per graph node, one `PanelAction::ToggleNodeParamExpose` for both targets). The **outer-card side** is still forked: clicking the Animate checkbox on a Wireframe generator's `node.render_lines` flips the graph bit but produces no slider. Step 8 of the unification plan (per a7fd4698's commit body) is the work that closes this gap; this doc is the design for that step.

## Forked surfaces today

| Layer | Effect path | Generator path | Symmetric? |
|---|---|---|---|
| Per-instance dynamic param storage | `EffectInstance.user_param_bindings: Vec<UserParamBinding>` + `user_param_bindings_version: u32` | *(none)* — `GeneratorParamState` has `param_values: Vec<f32>` sized strictly to `def.param_count` | **No** |
| `ToggleNodeParamExposeCommand` outer-card mirror | `mirror_effect_side`: flips `ParamSlot.exposed` for static slots, or appends/removes a `UserParamBinding`; prunes drivers + envelopes + Ableton mappings on remove; full undo capture | Hard-coded `None` at [crates/manifold-editing/src/commands/graph.rs:1015](../crates/manifold-editing/src/commands/graph.rs#L1015) — the graph bit flips but nothing materialises on the card | **No** — this is the bug |
| state_sync → card config builder | `effects_to_configs` walks `reg_def.param_defs` (static prefix) **AND** `fx.user_param_bindings` (user tail), resolving drivers/envelopes/Ableton via `param_id_to_value_index` (tier-aware) | `gen_params_to_config` walks `reg_def.param_defs` only; drivers/envelopes use static-only `id_to_index` lookup. Comment at [state_sync.rs:1471](../crates/manifold-app/src/ui_bridge/state_sync.rs#L1471): "Generator params are static-tier only today" | **No** |
| Outer-slider → inner-node value path (renderer) | `EffectInstance.param_values` (static + user tail) lands in `EffectSlot.bindings[i]`; `param_id_to_value_index` resolves both tiers | `JsonGraphGenerator.bindings: Vec<BindingResolution>` resolved **once** at construction from `preset_metadata.bindings`; `apply_param_values(&ctx.params)` only pushes through those resolutions. Construction is the only entry point for new bindings | **No** — `JsonGraphGenerator` has no concept of a user-binding tail and is not currently rebuilt on `user_param_bindings_version` bump |
| PanelAction variants | `EffectParamSnapshot/Changed/Commit/RightClick`, `EffectDriverToggle`, `EffectEnvelopeToggle`, etc. — carry `(fx_idx: usize, ParamId)` | `GenParamSnapshot/Changed/Commit/RightClick`, `GenDriverToggle`, `GenEnvelopeToggle`, etc. — carry `(ParamId)` (one gen per layer) | Structurally parallel, ~30 variants each |
| Card panel implementation | `manifold_ui::panels::effect_card` (~2100 lines): header w/ enable toggle + DRV/ENV/ABL/MOD badges + drag handle; per-row slider + D/E + Ableton trim | `manifold_ui::panels::gen_param` (~1700 lines): header w/ change-type button + cog + chevron; per-row slider + D/E + Ableton trim; plus toggle-row variant for `is_toggle` params and string-param rows | Mostly parallel; `param_slider_shared.rs` (~1200 lines) already factors `ParamModState`, `ParamDragState`, `format_param_value`, button styles, `AbletonMappingDisplay`, `EnvelopeParam`, `DriverConfigAction` |
| Graph editor click handler | One `PanelAction::ToggleNodeParamExpose` for both targets, dispatched in [app_render.rs:689](../crates/manifold-app/src/app_render.rs#L689) → `ToggleNodeParamExposeCommand::new` with the appropriate `GraphTarget` | Same | **Yes** ✓ (this is the part `a7fd4698` already fixed) |

## What to do about each fork

### 1. Dynamic param storage — one list in the graph (approach A)

**Recommendation: user-added bindings live in the graph's `preset_metadata.bindings`.** A new boolean `userAdded` on each `BindingDef` distinguishes "shipped with the preset" from "added at runtime." `param_values` length still grows by the user-added count — drivers/Ableton/MIDI need that allocation-free per-frame write slot — but the **metadata** (label/min/max/convert/target) lives in one place: the graph.

Why this and not a separate `Vec<UserParamBinding>` on the host (like effects do today):

- **One source of truth.** The graph already holds the binding list (`preset_metadata.bindings`) and the exposure bits (`node.exposed_params`). Adding user bindings to that same list — flagged `userAdded: true` — keeps everything addressable from one place. No "metadata may be in the graph OR in this parallel Vec; check both" footgun.
- **Symmetric with what a7fd4698 already shipped.** That commit moved exposure into the graph; this extends the same move to the binding metadata. The end-state is the architecture the commit's Step 8 was pointing at — minus the muddled wording about killing `param_values`, which never made sense (drivers/Ableton/MIDI need that buffer, allocation-free, every frame).
- **Generators have no installed base of user-added bindings.** The feature has never worked on generators, so there are no save files to migrate. Doing approach A on generators first is genuinely cheap; effects can migrate to the same shape later when there's appetite for the save-file pass.

**Long-term: the Rust-side static registry collapses entirely.** Today every shipping effect/generator declares its outer-card params twice — once via `inventory::submit!` (the `EffectMetadata`/`GeneratorMetadata` registry) and once in the JSON preset's `preset_metadata.params`. The two are kept in sync at startup by the registry walking the bundled JSONs. Once every effect and generator is graph-backed (the direction the primitive-library + decomposition work is heading), the Rust static registry has nothing left to declare. The graph's `preset_metadata` becomes the only binding-metadata source on the system. Then there is truly one lookup, one list, full stop.

`param_values` stays as the per-instance value bus regardless — that part doesn't migrate (per `feedback_param_values_is_performance_surface`).

### 2. `ToggleNodeParamExposeCommand` generator mirror

**Add `mirror_generator_side`** that replicates `mirror_effect_side` against `GeneratorParamState`:

- Static-slot path: flips the graph's `exposed_params` only (no `param_values[i].exposed` on the generator side — generators have plain `Vec<f32>` because `FloatValuesWire` is what they serialize). The graph's `exposed_params` is sufficient; the static-prefix is always rendered, and the graph bit controls preset visibility.
- User-tail path: appends or removes a `UserParamBinding` on `gp.user_param_bindings`, prunes generator drivers + envelopes + Ableton mappings, captures reverse state. Envelope cleanup needs the layer borrow but doesn't need a separate layer walk — generator envelopes live directly on `GeneratorParamState.envelopes`, not on the layer's effect chain. Simpler than the effect side.

Static-slot detection (`static_slot_for`) already works against the graph's `preset_metadata.bindings` regardless of target — no change needed there.

### 3. state_sync `gen_params_to_config` extension

**Walk both tiers** the same way `effects_to_configs` does: append one `GenParamInfo` per `gp.user_param_bindings[j]` to the static prefix. Drivers/envelopes/Ableton lookup needs a new `GeneratorParamState::param_id_to_value_index` (mirroring `EffectInstance::param_id_to_value_index`) that resolves both tiers. Today the static-only `id_to_index` lookup is used in three call sites in `gen_params_to_config` plus the bridge handlers — all need to switch to the tier-aware helper.

OSC address surfacing for user-tail bindings: same as the effect side ships today (no address — generators address by `param_id` and the OSC dispatcher walks both tiers via `param_id_to_value_index`). Card label surfacing of the address is a follow-up.

### 4. JsonGraphGenerator user-binding consumption

This is the part the previous unification didn't cover and is the **most subtle**.

`JsonGraphGenerator.bindings: Vec<BindingResolution>` is computed once at construction from `preset_metadata.bindings`. The generator-renderer path (`generator_renderer.rs:347-350`) copies `gp.param_values[0..min(N, MAX_GEN_PARAMS)]` into `ctx.params`. So if we extend `param_values` with the user-binding tail, the **values reach `ctx.params`** automatically — but `JsonGraphGenerator.apply_param_values` will not push them anywhere because there's no `BindingResolution` pointing at the new `source_index`.

**Two options**, both viable:

(a) **Rebuild `JsonGraphGenerator.bindings` on user-binding change.** Bump `gp.user_param_bindings_version`; the renderer compares versions in its per-frame layer-state walk and calls a new `JsonGraphGenerator::set_user_bindings(&[UserParamBinding], &outer_param_index_for_user_tail)` that appends additional `BindingResolution`s. Construction-time `outer_param_index` map already keys by `id` against `preset_metadata.params` — we extend it with `user_param_bindings[j].id → static_count + j`. Per-binding `target_node`/`target_param`/`convert`/`default` resolve from the `UserParamBinding` itself against the graph's handle map. Re-resolution is cheap (`Vec<UserParamBinding>` is typically O(1-5) entries).

(b) **Rebuild the whole `JsonGraphGenerator`** on `user_param_bindings_version` change. Reuses the existing rebuild path that already handles per-card graph overrides. Heavier (rebuilds `Graph` + `ExecutionPlan` + `Executor`) but uses only existing machinery. Performance impact only at editing-time — not on the hot path.

**Recommendation: (a).** Lighter, surgical, easy to test. (b) is the fallback if (a) hits unexpected coupling.

### 5. PanelAction enum: leave forked for now

The Effect/Gen variants are structurally parallel but they differ in payload (`fx_idx` vs no `fx_idx`) and consumer (effect chain vs `Layer.gen_params_mut()`). Unifying them means introducing a `ParamTarget` enum and rewriting ~60 bridge match arms in [inspector.rs](../crates/manifold-app/src/ui_bridge/inspector.rs). That's a big diff with no behavioural change. **Defer to a separate refactor.** The naming convention is consistent enough that future-me can read it.

### 6. Card panel UI duplication (Phase 3)

**Recommendation: defer, but lay groundwork.** Real wins available:

- Per-row rendering (slider + D/E + driver config + envelope config + Ableton trim) is essentially identical and could be a shared `ParamRowBuilder` taking a `&ParamRowConfig` + `&mut ParamModState`. That's the bulk of the ~600 lines of per-row UI in each panel.
- Card shell (header + border) is genuinely different — effect has the enable toggle and DRV/ENV/ABL/MOD badges; gen has the change-type button and toggle-row variant for `is_toggle` params. Keep separate.

A `crates/manifold-ui/src/panels/param_card.rs` with `ParamRowBuilder` would shave ~600-800 lines and force one source of truth for slider styling, OSC label click, ableton mapping subsection. The two existing panels become card shells calling `ParamRowBuilder::build_row` in a loop.

But: this is an architectural cleanup with no behaviour change. **Defer to a dedicated refactor pass after Phase 2 ships and is verified on the live rig.** The bug fix is load-bearing; the panel unification is plumbing.

## Phase 8 status

`a7fd4698` shipped Steps 1-7 (graph-side unification). The follow-up commit `ebbff05e` (dropping the `effect_index` click guard) fixed the Inner-Node Parameters panel for generators. Together they make exposure-toggling visually responsive on the right panel for generators — but the outer card still doesn't render the resulting binding.

Step 8 remaining work (as scoped by this audit, not as worded in a7fd4698's commit body):

1. `GeneratorParamState.user_param_bindings: Vec<UserParamBinding>` + version counter + serde.
2. `param_values` length includes user tail (extend `migrate_to_registry_length`, `init_defaults_for_type`).
3. `GeneratorParamState::param_id_to_value_index` (tier-aware) + `append_user_binding` / `remove_user_binding_by_id` helpers + `ParamSource` trait surface updates if any.
4. `ToggleNodeParamExposeCommand` generator mirror (full undo with driver/envelope/Ableton prune).
5. `gen_params_to_config` walks both tiers; bridge handlers route through tier-aware lookup.
6. `JsonGraphGenerator::set_user_bindings(&[UserParamBinding])` plus a version compare on the per-frame layer-state walk.
7. Tests: round-trip serialization with user bindings; `ToggleNodeParamExposeCommand` Generator-target undo/redo with envelope cleanup; integration test that loads a project, toggles `node.render_lines:animate`, asserts card config grew, drives the new slider, asserts the JsonGraphGenerator's inner node sees the value, save+reload preserves the binding.
8. The `// Generator params are static-tier only today` comment at [state_sync.rs:1471-1473](../crates/manifold-app/src/ui_bridge/state_sync.rs#L1471) gets deleted.

**Not** part of Step 8: removing `EffectInstance.user_param_bindings` or `ParamSlot.exposed`. Those stay — see §1 above on why the commit body's framing of "migrate entirely into the graph" was too aggressive.

## Open questions

- Does `JsonGraphGenerator::set_user_bindings` need to be called from the content thread before the first frame after the version bump, or can the renderer detect the bump per-frame and re-resolve lazily? Need to check the existing `gp.user_param_bindings_version` → renderer cache pattern that effects use for the analogous case. Likely lazy is fine since the bindings array is small.
- Are there generator drivers/envelopes/Ableton mappings that today are silently dropped on save because the static-only `id_to_index` lookup fails? Worth a one-pass check before adding the user tail, so we know whether the new code paths regress any latent behaviour.
- For the `param_values` length extension: does the analyzer crate's analyzer-VST-plugin path touch generator params? Confirmed no — `manifold-audio` is a stub, and the analyzer is a separate workspace. No-op.
