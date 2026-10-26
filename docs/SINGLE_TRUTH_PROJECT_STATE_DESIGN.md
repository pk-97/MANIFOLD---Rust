# Single-truth project state

**Status:** in progress. Sprint 0 (this plan) is done once Peter approves it.

## What this fixes on stage

- A slider or clip you're dragging can't snap back when the content thread publishes.
- The screen never shows a state the render doesn't have.
- Scene panel meters move, and touching a scene slider picks its automation lane, like every other card.
- The UI thread stops copying the whole project after every edit and stops writing live values into a copy every frame.

## End state

1. The content thread is the only writer. Every edit is one `ContentCommand`, run once through `EditingService`.
2. The UI holds the content thread's `Arc<Project>` read-only. No clone, no local edits.
3. A control draws, in order: the drag in progress (`ScrubState.active` or `GesturePreview`), else the live value (`ModulationSnapshot`), else the project.
4. Every control carries its own target. Nothing decides ownership from the last-touched inspector tab.
5. One parameter card. The scene panel hosts `ParamCardPanel` the same way the inspector does.
6. Headless flows run edits through the same code the content thread uses.

## Settled

- One frame of click latency is fine. Drags stay instant because they draw from their own record.
- No new traits, flags, wrappers or modes. Pass concrete references: `&Project`, `Option<&ModulationSnapshot>`, `Option<&ResolvedScrub>`.
- Each sprint deletes what it replaces in the same commit. No fallbacks, no shims.
- The only split alive across sprints is `ContentCommand::Execute` (the UI already applied it) next to `ExecuteOnContent` (content thread only). It exists today and sprint 5 ends it.
- Merging `Param.base` and `Param.value` is out of scope. It's in `docs/IDEAS.md`.

## How a sprint runs

The lead writes `file:line` worker specs at the start of each sprint, because line numbers move between sprints. Workers run in parallel on separate files. Then one build, one test pass, one commit. Haiku workers run at medium effort, Sonnet workers at high. The lead runs the full tests and reads every diff.

## Sprint 1: live values are read, never written into the project

Today `ModulationSnapshot::apply` writes LFO and audio values into the UI's copy every frame, then `ScrubState::restore_dragged` puts the dragged value back on top.

- `ModulationSnapshot` gets reads by owner (`macros`, `values_of`) and loses `apply`. The UI keeps the newest snapshot until a newer one arrives.
- Every param slider and macro draws, in order: the drag in progress, the live value, your value. Params go through the one funnel `ui_translate::with_param_slots`. "Your value" means `base` when the instance tracks base, else `value`, the same rule `PresetInstance::get_base_param` uses.
- The drag-first read lands here for params and macros, because without `apply` a dragged slider would otherwise draw the content thread's value one frame late. Sprint 4 does the same for the other drag kinds.
- Macro undo baselines read the live macro value, since the content thread can move a macro (an Ableton knob) without the UI's copy hearing about it.
- The restore at snapshot acceptance stays until sprint 4: a structural snapshot still replaces the UI's copy until then.
- Workers: 2 Haiku, one after the other. Worker 1 adds the reads and compiles alone; worker 2 does the wiring and deletions on top.
- Deletes: `ModulationSnapshot::apply`, the restore call after it, the test `macro_drag_survives_a_mid_gesture_modulation_snapshot`.

### Sprint 1, worker 1 (Haiku, medium): read the snapshot and the drag

Files you own: `crates/manifold-app/src/content_state.rs`, `crates/manifold-app/src/ui_bridge/scrub.rs`, `crates/manifold-app/src/app_render.rs` (only the `impl BoundNodeParamDrag` block). Touch nothing else.

1. `content_state.rs`, in `impl ModulationSnapshot`, right after `capture_into` (ends just before the `/// Apply modulated values` doc at line 508), add:

   ```rust
   /// Live macro values in slot order; empty before the first capture.
   pub fn macros(&self) -> &[f32] {
       self.block_of(|owner| matches!(owner, BlockOwner::Macros))
           .map_or(&[][..], |(values, _)| values)
   }

   /// Live values of `owner`'s params in manifest order: an effect
   /// (`GraphTarget::Effect`) or a layer's generator (`GraphTarget::Generator`).
   /// `None` when no block was captured for that owner under `topology`, so a
   /// param added, removed or reordered since the capture never reads a
   /// neighbour's value.
   pub fn values_of(&self, owner: &GraphTarget, topology: u32) -> Option<&[f32]> {
       let (values, stamp) = self.block_of(|block| match (block, owner) {
           (BlockOwner::Effect(id), GraphTarget::Effect(want)) => id == want,
           (BlockOwner::GenParams(id), GraphTarget::Generator(want)) => id == want,
           _ => false,
       })?;
       (stamp == topology).then_some(values)
   }

   /// The first block whose owner matches, with its topology stamp.
   fn block_of(&self, is_owner: impl Fn(&BlockOwner) -> bool) -> Option<(&[f32], u32)> {
       let mut start = 0usize;
       for (block, owner) in self.block_owners.iter().enumerate() {
           let len = self.block_lens[block] as usize;
           if is_owner(owner) {
               return Some((&self.values[start..start + len], self.block_topos[block]));
           }
           start += len;
       }
       None
   }
   ```

   Import `manifold_core::GraphTarget` if the file doesn't already. Leave `apply` in place; worker 2 deletes it.

2. `content_state.rs` tests: rewrite the four `apply_*` tests (lines 1026, 1054, 1082, 1155) to check reads instead of `apply`. Keep each setup, drop the drift-then-apply lines, and assert with `values_of`:
   - `apply_skips_block_on_same_length_reorder` becomes `values_of_misses_after_a_same_length_reorder`: after the reorder, `values_of(&GraphTarget::Effect(id), <the reordered manifest's topology()>)` is `None`.
   - `apply_writes_block_when_topology_unchanged` becomes `values_of_returns_the_captured_block_when_topology_unchanged`: `Some(&[0.10, 0.20][..])`.
   - `apply_routes_by_id_when_layer_inserted` becomes `values_of_finds_blocks_by_id_after_a_layer_insert`: each effect's captured values are found by its id after the insert.
   - `apply_routes_by_id_when_effects_reordered` becomes `values_of_finds_blocks_by_id_after_effects_reorder`: same, after the reorder.
   - Add `macros_reads_slot_values`: set macro slots 0 and 1 to 0.3 and 0.6, capture, assert `macros()[..2] == [0.3, 0.6]`.

3. `ui_bridge/scrub.rs`, in `impl ResolvedScrub` (line 316), add above `fn restore`:

   ```rust
   /// The in-flight value of card param `param_id` on `target` while this
   /// gesture drags it. Drawing reads this before live values and the
   /// project, so a dragged slider shows where the pointer put it.
   pub(crate) fn live_param(&self, target: &GraphTarget, param_id: &str) -> Option<f32> {
       match self {
           ResolvedScrub::Param { target: t, param_id: p, live, .. }
               if t == target && p.as_ref() == param_id => Some(*live),
           ResolvedScrub::ParamRgb { target: t, param_ids, live, .. } if t == target => param_ids
               .iter()
               .position(|p| p.as_ref() == param_id)
               .map(|i| live[i]),
           ResolvedScrub::ParamGroup { target: t, cards, .. } if t == target => cards
               .iter()
               .find(|c| c.id.as_ref() == param_id)
               .map(|c| c.live),
           ResolvedScrub::BoundNodeParam(drag) => drag.live_card_value(target, param_id),
           _ => None,
       }
   }

   /// The in-flight value of macro `idx` while this gesture drags it.
   pub(crate) fn live_macro(&self, idx: usize) -> Option<f32> {
       match self {
           ResolvedScrub::Macro { idx: i, live, .. } if *i == idx => Some(*live),
           _ => None,
       }
   }
   ```

4. `app_render.rs`, in `impl BoundNodeParamDrag` (line 78), add:

   ```rust
   /// The outer card param's in-flight value, when this drag is on `target`'s `param_id`.
   pub(crate) fn live_card_value(&self, target: &manifold_core::GraphTarget, param_id: &str) -> Option<f32> {
       (self.target == *target && self.outer_param_id == param_id).then_some(self.current_value)
   }
   ```

5. `ui_bridge/scrub.rs`: add a `#[cfg(test)] mod tests` at the end of the file with `live_param_reads_only_the_dragged_param`: a `ResolvedScrub::Param` on `GraphTarget::Effect(EffectId::new("fx"))`, `"amount"`, live 0.9 returns `Some(0.9)` for that target and id, `None` for another id, `None` for `GraphTarget::Effect(EffectId::new("other"))`. Also a `ParamRgb` returns the right channel, and a `Macro { idx: 2, .. }` returns its live value for 2 and `None` for 1.

Test: `cargo nextest run -p manifold-app values_of macros_reads live_param`, then `cargo clippy -p manifold-app --tests -- -D warnings`.

### Sprint 1, worker 2 (Haiku, medium): draw from the reads, delete `apply`

Runs after worker 1. Files you own: `ui_translate.rs`, `ui_bridge/projection/cards.rs`, `ui_bridge/projection/scene.rs`, `ui_bridge/state_sync.rs`, `ui_frame.rs`, `editor_frame.rs`, `editor_bridge.rs`, `app_render.rs`, `content_state.rs`, `ui_bridge/scrub.rs` (macro Begin only), `ui_bridge/dispatch/params.rs` (`MacroReset` only), `ui_bridge/inspector.rs` (one test), `content_reaction_budget.rs`, `ui_snapshot/script.rs`, `ui_snapshot/mod.rs`, `ui_snapshot/input_matrix.rs`, `ui_snapshot/fixtures.rs`. All under `crates/manifold-app/src/`.

1. `ui_translate.rs:577`, `with_param_slots` takes the instance, its live values and the drag:

   ```rust
   pub fn with_param_slots<'p, R>(
       inst: &'p PresetInstance,
       live: Option<&[f32]>,
       dragged: impl Fn(&str) -> Option<f32>,
       f: impl FnOnce(&mut dyn Iterator<Item = (&'p str, UiParamSlot)>) -> R,
   ) -> R {
       let mut it = inst.params.iter().enumerate().map(|(i, p)| {
           let mut slot = param_slot_to_ui(p);
           let yours = if inst.base_tracked { p.base } else { p.value };
           slot.value = dragged(p.id())
               .or_else(|| live.and_then(|values| values.get(i).copied()))
               .unwrap_or(yours);
           (p.id(), slot)
       });
       f(&mut it)
   }
   ```

   Add to its doc: each slot's `value` is what the slider draws: the drag in progress, else the live value the content thread captured (`live`, this owner's block of the `ModulationSnapshot`), else your value (`base` when the instance tracks base, else `value`, as `PresetInstance::get_base_param` reads it).

2. `ui_bridge/projection/cards.rs:53`: `sync_card_values(ui, project, active_layer, live: Option<&ModulationSnapshot>, drag: Option<&ResolvedScrub>)`. For each master and layer effect:

   ```rust
   let owner = GraphTarget::Effect(effect.id.clone());
   let values = live.and_then(|s| s.values_of(&owner, effect.params.topology()));
   crate::ui_translate::with_param_slots(
       effect,
       values,
       |id| drag.and_then(|d| d.live_param(&owner, id)),
       |slots| card.sync_values(tree, slots),
   );
   ```

   The generator does the same with `GraphTarget::Generator(layer.layer_id.clone())` and `gp_state`. In the doc above the function, replace the sentences from "No drag guard needed here" to the end with: "Each slider draws the drag in progress, else the live value, else your value (see `with_param_slots`)." Use `crate::ui_bridge::scrub::ResolvedScrub` and `crate::content_state::ModulationSnapshot`.

3. `cards.rs` tests:
   - `project_param_change_reaches_card_value_text_via_sync_card_values` (line 967) becomes `live_value_from_the_snapshot_reaches_the_built_card`. Instead of writing `.value = 0.75` into `project`, clone the project, set the clone's `user_glow` value to 0.75, `capture_into` a `ModulationSnapshot` from the clone, then call `sync_card_values(&mut ui, &project, None, Some(&snap), None)` and assert `"0.75"`.
   - The range test at line 1007 passes `None, None`.
   - Add `dragged_param_draws_its_drag_value_over_the_live_value`: the same setup with the 0.75 snapshot, plus `ResolvedScrub::Param { target: GraphTarget::Effect(<the effect's id>), param_id: "user_glow".into(), baseline: 0.5, live: 0.9 }`. Assert `"0.90"`.
   - Add `without_a_live_value_the_card_draws_your_value`: `set_base_param("user_glow", 0.6)` on the effect, sync with `None, None`, assert `"0.60"`.

4. `ui_bridge/projection/scene.rs:518`: `sync_scene_row_values(ui, project, live, drag)` with the same two params, owner `GraphTarget::Generator(<the layer's layer_id>)`, instance `gp`. Update its doc to say rows draw the same way as cards.

5. `ui_bridge/state_sync.rs`:
   - `push_state` (line 140) and `push_live` (line 826) get a last param `drag: Option<&crate::ui_bridge::scrub::ResolvedScrub>`. `push_state` passes it to `push_live`, and `push_live` passes it to `push_live_values`.
   - In `push_live_values` (line 874), set `let live = content_state.modulation_snapshot.as_deref();`. The macro value at line 890 becomes `drag.and_then(|d| d.live_macro(i)).or_else(|| live.and_then(|s| s.macros().get(i).copied())).unwrap_or(slot.value)`.
   - The calls at lines 950 and 953 become `sync_card_values(ui, project, active_layer, live, drag)` and `sync_scene_row_values(ui, project, live, drag)`.

6. `ui_frame.rs`:
   - `PushInputs` (line 382) gets `pub drag: Option<&'a crate::ui_bridge::scrub::ResolvedScrub>`, documented as "The scrub gesture in flight, drawn over live values."
   - Line 643 passes `push.drag` to `push_live`. Line 647 passes `push.content_state.modulation_snapshot.as_deref(), push.drag` to `sync_scene_row_values`.

7. `editor_frame.rs`:
   - `EditorFrameInputs` (line 311) gets the same `drag` field, and line 420 passes `frame.drag` to `push_live`.
   - The constructors at `editor_bridge.rs:2162` and `:2828` pass `self.scrub.active.as_ref()`. If a borrow split blocks `self.scrub` there, take it into a local next to the existing `content_state` local.
   - The test constructor at `editor_frame.rs:1009` passes `None`.

8. `editor_bridge.rs:1860`: the mapping popover's live value reads the snapshot. Replace the `.and_then(...).map(|param| param.value)` chain with:

   ```rust
   self.editor_mapping_popover.target().and_then(|target| {
       let owner = crate::editing_host::to_graph_target(target);
       let instance = self.local_project.graph_target_owner(&owner)?;
       let id = self.editor_mapping_popover.binding_id();
       let param = instance.params.get(id)?;
       let live = self
           .content_state
           .modulation_snapshot
           .as_deref()
           .and_then(|s| s.values_of(&owner, instance.params.topology()))
           .and_then(|values| {
               instance.params.iter().position(|p| p.id() == id).and_then(|i| values.get(i).copied())
           });
       Some(live.unwrap_or(if instance.base_tracked { param.base } else { param.value }))
   })
   ```

9. `app_render.rs`:
   - Delete the block at lines 204 to 213, from `// Apply lightweight modulation snapshot` through the closing brace after `restore_dragged`. Keep the `restore_dragged` call at line 165.
   - At line 231, replace `modulation_snapshot: None, // consumed above` with `modulation_snapshot: state.modulation_snapshot.or_else(|| self.content_state.modulation_snapshot.take()), // the newest capture stays until a newer one arrives`.
   - The `PushInputs` built near line 2188 gets `drag: self.scrub.active.as_ref(),`.

10. `content_state.rs`:
    - Delete `apply`, its doc included (lines 508 to 574).
    - Then grep the file for `apply` and rewrite each comment that describes the deleted method: the `ModulationSnapshot` doc (lines 376 to 383), the `block_topos` and `block_owners` field docs (391 and 398), the `BlockOwner` doc (416), the macro comment inside `capture_into`, the `ModSnapshotPool` doc (586, which becomes "the UI drops its copy when a newer one arrives"), and the `ContentState::modulation_snapshot` doc (293). That last one becomes: "Live param and macro values the content thread captured, shared from its [`ModSnapshotPool`]. The UI keeps the newest until a newer one arrives and reads it when drawing."

11. Macro undo baselines read the live macro value first:
    - `ui_bridge/scrub.rs:1012` becomes `let value = ctx.content_state.modulation_snapshot.as_deref().and_then(|s| s.macros().get(idx).copied()).unwrap_or(ctx.project.settings.macro_bank.slots[idx].value);`.
    - `ui_bridge/dispatch/params.rs:274` (`MacroReset`'s `old`) reads the same way.

12. `ui_bridge/inspector.rs`: delete lines 598 to 646, the doc comment starting `/// Undo-race repro` and the test `macro_drag_survives_a_mid_gesture_modulation_snapshot`.

13. `content_reaction_budget.rs:391`:
    - `tick_and_capture` returns `(Project, Option<Arc<ModulationSnapshot>>)`: the engine project clone without `apply`, and the last published snapshot. Fix its comment.
    - The param assertion near line 418 reads the live value through the snapshot. Find the effect's index of `mirror_pid` in `fx.params`, then `snap.values_of(&GraphTarget::Effect(fx.id.clone()), fx.params.topology())`, and assert that value is 0.37.
    - The other uses take `(seen, _)`.

14. Headless harness:
    - `ui_snapshot/fixtures.rs:79`: rename the body of `build_cached` to a private `scene_data(scene, cache)`. `build_cached` then calls it, captures a `ModulationSnapshot` from `data.project` and sets `data.content.modulation_snapshot = Some(Arc::new(live))`, with the comment "the fixture's live values are what the content thread would have captured".
    - `ui_snapshot/script.rs`: `Runner` gets `mod_published: crate::content_state::ModSnapshotPool` (declare near line 595, `Default::default()` near line 725).
    - After the capture at line 327, set `data.content.modulation_snapshot = Some(runner.mod_published.publish(&runner.values_capture));`. Inside the `if` at line 1031, do the same with `self.`.
    - The `PushInputs` at line 1459 gets `drag: self.scrub.active.as_ref(),`.
    - `ui_snapshot/mod.rs:919`: `push_state(..., None)`.
    - `ui_snapshot/input_matrix.rs:141`, `:151`: `push_live(..., None)`. `:161`: `sync_scene_row_values(ui, &data.project, data.content.modulation_snapshot.as_deref(), None)`.

Test:
- `cargo nextest run -p manifold-app card scene modulation macro content_state reaction_budget`
- then `cargo nextest run -p manifold-app`
- then `cargo clippy -p manifold-app --tests -- -D warnings`
- then `scripts/dev.py flows work-modulated-step envelope-modulation t-drawer-step frame-align-lfo set-param-layer-opacity`

Report each failure with its output. Don't loosen a test to make it pass.

## Sprint 2: the scene panel uses the shared card

Scene rows are already built by the shared row core (`build_param_row`, `RowHost`). What's copied is the per-frame row behaviour. `ParamCardPanel` and `ScenePanel` each have their own `sync_live_shapes` and `sync_driver_frame_rates`, and only the card has `update_fire_meters` and the open fire-drawer send and band. That's why scene drawer meters never move, and why an open scene fire drawer doesn't steer the audio scope. The scene panel's layout (item blocks, control-group sections, the inline material block, outliner eyes) genuinely differs from a card and stays its own.

- The four per-frame row behaviours move onto `RowHost`, written once. Both panels call them, and both copies are deleted.
- `UIRoot` feeds the scene panel's meters and asks it for its open fire drawer, after the inspector.
- The audio side already keys meter levels by `(owner instance id, param id)`. Scene rows are the generator's params, and `ScenePanel.full_params.effect_id` is that owner.
- Workers: 1 Haiku.

### Sprint 2, worker 1 (Haiku, medium): one set of per-frame row behaviours

Files you own:
- `crates/manifold-ui/src/panels/param_slider_shared/row_host.rs`
- `crates/manifold-ui/src/panels/param_card/mod.rs`
- `crates/manifold-ui/src/panels/scene_setup_panel/value_sync.rs`
- `crates/manifold-ui/src/panels/scene_setup_panel/tests.rs`
- `crates/manifold-app/src/ui_root/mod.rs`

1. `row_host.rs`, in `impl RowHost`, add four methods, moved from `ParamCardPanel` (`param_card/mod.rs`):
   - `update_fire_meters(&self, tree, rows: &[ParamRow], owner: &str, fire_level: &dyn Fn(u64) -> Option<f32>, dt: f32)`: the body of `ParamCardPanel::update_fire_meters` (line 629), with `self.row_host` becoming `self`, `self.rows` becoming `rows`, and the owner taken from the parameter.
   - `open_fire_mode_drawer_send(&self, rows: &[ParamRow], mod_state: &ParamModState) -> Option<AudioSendId>` and `open_fire_mode_drawer_band(...) -> Option<AudioBand>`: the bodies at lines 671 and 685, with the private `open_fire_mode_drawer_row(&self, rows)` from line 658.
   - `sync_live_shapes(&mut self, tree, mod_state: &mut ParamModState, rows: &mut [ParamRow], shapes_of)`: the loop from `ScenePanel::sync_live_shapes` (`value_sync.rs:122`), which skips rows with an `affordance`. Only scene rows have one, so cards behave the same.
   - `sync_driver_frame_rates(&mut self, tree, mod_state: &mut ParamModState, rows: &mut [ParamRow], rate_of)`: the loop from either copy (they're identical).
   - Keep the doc comments, trimmed to current behaviour. Keep `ParamCardPanel::update_fire_meters`'s doc (minus history) on the new `update_fire_meters`.
2. `param_card/mod.rs`:
   - `ParamCardPanel::update_fire_meters`, `open_fire_mode_drawer_send`, `open_fire_mode_drawer_band`, `sync_live_shapes` (line 886) and `sync_driver_frame_rates` (line 860) each become a one-line call to the `RowHost` method, passing `&self.rows`, `&self.state.mod_state` and `self.effect_id.as_str()` as needed.
   - Delete `ParamCardPanel::open_fire_mode_drawer_row`.
3. `value_sync.rs`:
   - `ScenePanel::sync_live_shapes` (keep its `if !self.open` guard) and `sync_driver_frame_rates` become calls to the `RowHost` methods with `self.properties_card`'s `row_host`, `mod_state` and `rows`.
   - Add `ScenePanel::update_fire_meters(&self, tree, fire_level, dt)`, which returns when the panel is closed or `full_params` is `None`, else calls `row_host.update_fire_meters` with owner `full_params.effect_id`.
   - Add `open_fire_mode_drawer_send` and `open_fire_mode_drawer_band`, `None` when closed.
4. `manifold-app/src/ui_root/mod.rs`:
   - `update_fire_meters` (line 1796) also calls `self.scene_setup_panel.update_fire_meters(&mut self.tree, &|key| fire_meters.get(key), dt)`.
   - `open_fire_mode_drawer_send` and `open_fire_mode_drawer_band` return the inspector's answer, else the scene panel's (`.or_else(...)`).
5. Test in `scene_setup_panel/tests.rs`: `scene_fire_drawer_meter_shows_its_level`.
   - Build a live panel (`live_panel_with`) whose `full_params` surface has `effect_id` `"gen"` and one fire row, shaped like `param_card/mod.rs`'s `effect_config_with_fire` (`is_trigger: true`, `audio.trigger_mode_idx = 1`). The cube's `parameter_ids` lists that row's id.
   - Call `panel.update_fire_meters(&mut tree, &|key| (key == fire_meter_key_for_param("gen", <row id>)).then_some(0.8), 1.0 / 60.0)`.
   - Assert the row's drawer meter shows the level, read the way `drawer.rs`'s meter tests read a fill.
   - Assert `open_fire_mode_drawer_send()` is the row's send.

Test:
- `cargo nextest run -p manifold-ui scene_setup param_card fire`
- then `cargo nextest run -p manifold-app fire_meter`
- then `cargo clippy -p manifold-ui -p manifold-app --tests -- -D warnings`
- then `scripts/dev.py flows scene- material-inspector-textures audio-clip-trigger-add`

## Sprint 3: controls carry their own target

`Inspector::last_effect_tab` remembers which inspector tab you last touched. 15 production sites (plus 7 in the flow harness) use it to decide which layer or effect a control belongs to. That's why touching a scene slider doesn't pick its automation lane unless a layer card was touched first: the touch-to-select gate in `ui_bridge/scrub.rs` reads that tab. The tabs also disagree with each other: `InspectorTab::is_layer_scope` excludes Scene, while params, mapping and resolve treat Scene as a layer.

Two parts:
- **3a** fixes every action that already carries a target. Its decisions come from the target, never the tab.
- **3b** gives the index-only card actions (`EffectToggle(usize)` and kin, the browser popup, card copy and paste) the identity of their list, then deletes `last_effect_tab` and `editor_dispatch_context`. Its spec is written once 3a lands.

What 3a fixes on stage:
- Touching a scene slider picks its automation lane whatever tab the inspector last showed.
- "Map to Ableton" on a layer effect maps that effect's own layer, not whichever layer is active.
- An effect control acts even when the inspector last showed a different tab. Today `resolve_effect_by_id` drops the action when the effect isn't in that tab's list.

### Sprint 3a, worker 1 (Haiku, medium): decisions come from the target

Files you own:
- `crates/manifold-editing/src/commands/effect_target.rs`
- `crates/manifold-ui/src/panels/inspector/mod.rs`
- `crates/manifold-ui/src/panels/inspector/render.rs`
- under `crates/manifold-app/src/`: `ui_bridge/mod.rs`, `ui_bridge/dispatch/resolve.rs`, `ui_bridge/dispatch/params.rs`, `ui_bridge/dispatch/modulation.rs`, `ui_bridge/dispatch/mapping.rs`, `ui_bridge/dispatch/audio_setup.rs` (comments only), `ui_bridge/scrub.rs`, `ui_bridge/inspector.rs` (tests), `text_session.rs`, `editor_bridge.rs`, `ui_root/dropdowns.rs`

1. `effect_target.rs`: add to `impl EffectTarget` (add the `impl` if there is none):

   ```rust
   /// The chain that holds effect `id`: the master chain or its layer's chain.
   /// `None` for a clip-scoped effect or an unknown id.
   pub fn of_effect(project: &Project, id: &EffectId) -> Option<Self> {
       if project.settings.master_effects.iter().any(|fx| &fx.id == id) {
           return Some(Self::Master);
       }
       project
           .timeline
           .layers
           .iter()
           .find(|l| l.effects.as_ref().is_some_and(|e| e.iter().any(|fx| &fx.id == id)))
           .map(|l| Self::Layer { layer_id: l.layer_id.clone() })
   }
   ```

2. `ui_bridge/mod.rs:680`, `resolve_effect_by_id`: drop the `tab`, `active_layer` and `selection` params. Keep the editor-target early return. Otherwise return `project.find_effect_by_id(id).map(|fx| fx.id.clone())`, a lookup over master, layer and clip effects. Update its doc.

3. `ui_bridge/dispatch/resolve.rs`:
   - `resolve_graph_target` drops `tab` and `selection` (keep `active_layer`, which the bare `GraphParamTarget::Generator` needs).
   - `resolve_mod_target` drops `effective_tab` and `selection`.
   - `ableton_mapping_target(target, project, param_id)` drops `tab` and `active_layer`. An effect maps through `EffectTarget::of_effect`: `Master` gives `MasterEffect`, `Layer { layer_id }` gives `LayerEffect { layer_id, .. }`, and `None` (a clip effect) gives `None`.
   - Fix every call site the compiler shows. In the tests module, update the calls and drop the now-meaningless tab arguments.

4. `ui_bridge/scrub.rs`, the touch-to-select gate (`if effective_tab.is_layer_scope() && let Some(layer_id) = ...automation_owner_layer_id(...)`): drop `effective_tab.is_layer_scope() &&`. `automation_owner_layer_id` already answers `None` for master, clip and group effects.

5. `ui_bridge/dispatch/modulation.rs`: the envelope permission for an effect (`matches!(effective_tab, InspectorTab::Layer)`) becomes `super::params::automation_owner_layer_id(&ctx.project, &env_target).is_some()`, so a non-group layer owns the effect. Use the path the compiler accepts.

6. `ui_bridge/dispatch/params.rs`, the `AddMask` arm (`match effective_tab { InspectorTab::Master => ..., Layer | Group | Scene => ..., Clip => ... }`): with the resolved `GraphTarget::Effect(eid)`, use `EffectTarget::of_effect(&ctx.project, &eid)`, returning `handled()` on `None`.

7. `effective_tab` stays only where an index-only action needs it (3b's job). Remove any binding that's left unused.

8. Inspector card queries take no tab:
   - `Inspector::is_effect_ableton_mapped(effect_id, param_id)` and `effect_has_graph_mod(effect_id)` find the card in every scope (`self.effects.iter().flatten()`).
   - `rack_group_has_mask(group_id)` searches every scope's `rack_groups`.
   - Update their callers in `ui_root/dropdowns.rs` (around lines 1816, 1975, 2044).

9. Leave the "Show Automation" menu gate in `ui_root/dropdowns.rs` (around line 1765) alone. That menu has no project in reach, and an effect card's right-click always comes from the tab being shown. 3b handles it with the other index-only paths.

10. Tests:
    - `ui_bridge/inspector.rs`: add `touching_a_scene_slider_picks_its_lane_with_the_inspector_on_master`. Build `two_scene_layer_project` and a materialized scene param (`materialized_param`). Put the inspector on Master with `h.ui.inspector.configure_tabs(&[InspectorTab::Master, InspectorTab::Layer], InspectorTab::Master)` (or the harness's equivalent). Dispatch `PanelAction::Scrub(ValueRef::Param(GraphParamTarget::GeneratorOf(layer_a), pid), ScrubPhase::Begin)`. Assert `h.selection.chosen_automation_params().get(&layer_a)` names that param.
    - `ui_bridge/dispatch/resolve.rs` tests: add `ableton_target_for_a_layer_effect_names_the_effects_own_layer`. Two layers, each with one effect. Resolve layer B's effect, and assert `LayerEffect { layer_id: B, .. }`. There's no active layer to pass any more; that's the point.

Test:
- `cargo nextest run -p manifold-app dispatch inspector scrub resolve mapping modulation`
- then `cargo nextest run -p manifold-app -p manifold-ui -p manifold-editing`
- then `cargo clippy -p manifold-app -p manifold-ui -p manifold-editing --tests -- -D warnings`
- then `scripts/dev.py flows automation- rack- inspector- param-step-action`

## Sprint 4: drags draw from their own record

- Slider drags (the 24 `ResolvedScrub` kinds): Move updates `live` and sends `gesture_move` without writing the project. Commit builds the undo command from `(baseline, live)` and sends `ExecuteOnContent`. Drawing checks the active scrub first for the kinds sprint 1 didn't cover (opacity, gains, trims, envelopes, mappings).
- Timeline drags: `AppEditingHost` writes clip and lane drags only into `GesturePreview`. Its reads (`find_clip_by_id`, `clips_on_layer`, automation points) lay the preview over the project. The commit batch sends `ExecuteOnContent`.
- Flow harness: `ui_snapshot/script.rs` runs `MutateProject { gesture: true }` on its project. Other mutates are still applied by the UI until sprint 5.
- Lead first: a table of all 24 scrub kinds with where each is drawn, what `live` shows and what commit sends.
- Deletes: `ScrubState::restore_dragged`, `ResolvedScrub::restore`, `BoundNodeParamDrag::apply`, `GesturePreview::reapply`, and the tests of that machinery (`marker_drag_survives_snapshot_stomp`, `every_slider_gesture_undoes_cleanly_even_under_snapshot_stomp`, `mapping_drags_survive_snapshot_stomp`, `reapply_recovers_a_snapshot_mid_drag`, `reapply_after_a_snapshot_matches_what_the_viewport_shows`).
- Workers: 3 Haiku (`scrub.rs`, drawing readers, inspector tests), 1 Sonnet (timeline host).
- Tests: `cargo nextest run -p manifold-app scrub`, `preview`, `editing_host`, `inspector`, flows `drag-`, `automation-`, `rack-wet-dry`, `trim-clip-left`.

## Sprint 5: the cut

- `LocalProject(Project)` becomes `LocalProject(Arc<Project>)`. `frame_plan::take_project_snapshot` swaps the `Arc`.
- `ProjectRef` writes nothing. `execute`, `execute_batch`, `mutate` and `gesture_move` only send. Callers of `executed_by` and `with_sent` (create-clip helpers, importers, keyboard automation) are rewritten one by one; any that needs a new id back uses `ExecuteSelecting`. `preview` writes only the gesture preview.
- The pre-applied `ContentCommand::Execute` goes and `ExecuteOnContent` is renamed `Execute`.
- Delete `suppress_snapshot_until` and `suppress_snapshot_set_at` (`app.rs`, `app_render.rs`, `window_input.rs`). A project load sends `LoadProject` and the UI shows the new project when it arrives.
- One edit runner: the content thread's edit handling (execute, batch, mutate, gesture, undo, redo, selection, rejection) becomes one function that both `content_commands.rs` and `ui_snapshot/script.rs` call with their own `EditingService`. The harness's stand-in (the `UndoRedoManager` record path and the dropped mutates) is deleted.
- Unit tests that build a `ProjectRef` over a local project share one helper: dispatch, run the sent edits through the edit runner, check the project.
- The compiler is the checklist: once the UI has no `&mut Project`, a missed write doesn't build.
- Workers: 1 Sonnet (edit runner and harness), 4 to 5 Haiku (callers, split by file).
- Tests: `cargo nextest run -p manifold-app`, `cargo nextest run -p manifold-ui`, `scripts/dev.py flows --all`. A flow that fails here was relying on the UI's copy. Each one is a real bug: fix it or list it for Peter.

## Sprint 6: fewer, faster tests

- Work-count flows: re-record the ceilings once (`scripts/run_ui_flows.py --record-work work-`), keep 10, delete the other 34 with their `work_ceilings.json` entries and `path_triggers` mentions. Keep: `work-idle`, `work-playback`, `work-perform-idle`, `work-modulated-step`, `work-slider-drag`, `work-clip-drag`, `work-automation-point-drag`, `work-node-param-drag`, `work-zoom`, `work-values-undo`.
- Behaviour flows: drop any that only covered deleted machinery and merge duplicates.
- Speed: time `cargo nextest run -p manifold-app` and `scripts/dev.py flows --all` before and after this sprint. Sprints 1 to 5 don't aim at speed, so the before and after of this sprint is the comparison that matters. First suspect: a PNG written at every flow step.
- Workers: 1 to 2 Haiku.
