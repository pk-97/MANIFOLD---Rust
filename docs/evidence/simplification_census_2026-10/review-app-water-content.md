# Simplification census review: app-ui, water, content

- **Reviewer:** Opus 5.5, read-only on code, part of BUG-yn979 (Simplification pass).
- **Files reviewed:** `app-ui.md`, `water.md`, `content.md` and their `.proof.md` files.
- **Tree:** main 090efbfa5. `git diff --stat 37a9c180a HEAD -- crates` is empty, so every sweep line number still holds.
- **Method:** SIMPLIFICATION_CENSUS.md section 6 (Review), cut down by the light-review ruling: no random sample. App-ui grade A rows were checked when they are over 200 lines, at stage or prep risk, or look like the symptom of a missing wire. Every B and C row gets one disposition line; stage and prep rows had their proofs re-run first.

## Grade changes

None. No grade A row in `app-ui.md` failed, and none turned out to be a bug in disguise. The sweep files are unchanged.

What was checked, with the extra search each got:

| row | why checked | re-run and extra search | result |
|---|---|---|---|
| app-ui-023 | over 200 lines (228) | `rg` for `driver_waveform` without word bounds over the whole repo, hidden dirs included; `pub use` lines in `lib.rs`; `git log -S driver_waveform_icons`; open beads | only `pub mod` names the module; the matches are `driver_waveform_idx` fields; no glob re-export; the history is the port (cf499ae20) and the census commit. Stays A |
| app-ui-002 | effect-group rename looked like a missing wire | `rg` for every effect-group rename producer; `bd show BUG-6in` (effect-group headers, folding and collapse animation) | no effect-group UI exists (BUG-6in is DEFERRED); `InspectorParam` replaced `EffectParam` and `GenParam`. The dead `GroupRename` arm would also address the wrong group (see the bugs list), so it is not worth reviving. Stays A |
| app-ui-004 | "Application wrapper isn't wired yet" | `rg DroppedFile` in the app | dropping a `.manifold` or `.json` file already opens it (`app.rs:2373`). Stays A |
| app-ui-017 | a cleared state never cleared? | the `set_secondary_lighting` call site | `render_scene.rs:3697` sets it every frame before dispatch, so nothing goes stale. Stays A |
| app-ui-020 | layer bitmaps never trimmed? | `upload_layer` and `render_layers` | textures for removed trailing layers linger until reuse; bounded by the peak layer count, and `render_layers` draws only the rects it is given. Not a bug. Stays A |
| app-ui-031 | focus never cleared? | `focused_widget` writes in `input.rs` | a click elsewhere clears focus (`input.rs:565`). Stays A |
| app-ui-032 | hover stuck when the pointer leaves? | the `CursorLeft` arm | `app.rs:2197` clears clip hover and automation feedback inline. Stays A |
| app-ui-038 | header text never synced? | `cached_header_text` reads | the inspector header falls back to the constant "Layer" for an empty layer name; layers carry names, so this is cosmetic. The lane can make the fallback a literal. Stays A |
| app-ui-042 | string params on cards never refreshed? | the `string_params` flow | values arrive with the card config in the structural projection (`cards.rs:150-160`, `inspector.rs:1040`). Stays A |
| app-ui-030, 039, 040, 045, 051 | possible missing wires | the live path for each | each has a live twin: the cursor mapping (`window_input.rs:74`), layer-name rebuild, the exit-path button keyed by node, selection and hover via `UIState`, insert-cursor clears at `ui_state.rs:699,767`. Stay A |
| app-ui-027, 031, 033 to 035, 037, 041 | common names, risk of a hidden caller | `rg` for call forms and glob imports (`components::*`, `components::{`) | no caller; the live siblings `state_button_skinned` and `dropdown_trigger_style` must stay. Stay A |
| app-ui-065, 066 | confirmed deletes | the proof's reader scans re-run; `item_header.rs:76-103` and `keyboard.rs:119` read | every `LightKnownRow` reader takes only `index`, `node_doc_id`, `name`; the camera payloads are read only as `_` or `matches!`. Stay A |

## Dispositions for every B and C row

Stage and prep rows (app-ui-006, 008, 012, 013, 019, 043, 061; water-012; every content row except content-018) had their proof commands re-run. All matched the sweep. The one extra search that found something: `scripts/gate_policy.py:631-632` names the `copy_positions` and `wave_field_3d` paths (content-010), which the content sweep's Rust-only searches missed.

### app-ui

| id | disposition | the deciding fact |
|---|---|---|
| app-ui-005 | delete | `flash_save` is written once on save-as and read by nothing; no save flash was ever drawn, so the show loses nothing |
| app-ui-006 | delete | only `TexturePane::local` is ever built (`ui_frame.rs:1163`, the VQT spectrogram pane); the Bridged path has no producer and no design names one |
| app-ui-007 | delete | stored, never read, and EDITOR_WINDOW_UNIFICATION_DESIGN.md D4 and I2 forbid the render passes from ever branching on it |
| app-ui-008 | keep | SESSION_MODE_DESIGN.md is in progress and these six commands are exactly what its P4 grid UI sends; the handlers and the engine side are built |
| app-ui-010 | keep | VULKAN_BACKEND_DESIGN.md is approved and cross-platform is a product requirement; 45 lines of Phase 1 scaffold |
| app-ui-011 | bead | only a compile can say which of the 542 `unsafe` blocks are redundant; a chore for a lane with cargo, not a census delete |
| app-ui-012 | keep | SCENE_MODIFIER_RT_DESIGN.md line 262 has every BLAS retain its triangle descriptor; 9 lines on a GPU object-lifetime path |
| app-ui-013 | bead | BUG-l7t4 (audition-grid page fault) is open at P1; delete the probe when it closes. The probe cannot do the job its comment claims (bugs list, item 1), and its `unsafe` Send impls sit in BUG-9xyo (unsafe Send/Sync inventory, manifold-gpu) |
| app-ui-015 | delete | three setters with no call; BUG-ns5k (benchmark the legacy-path MetalFX denoised scaler) measures the denoise cost, which these setters do not touch |
| app-ui-019 | delete | neither has a caller; the lane edits VULKAN_BACKEND_DESIGN.md line 108 and GBUFFER_DESIGN.md I4, whose gate counts call sites of a forwarder that has none |
| app-ui-024 | delete | the slider's own zone table already carries the same three gestures (`slider.rs:278`, `SliderZone::ValueCell`); nothing calls `intent_for`; amend SCENE_PANEL_UX_DESIGN.md line 46 |
| app-ui-043 | delete | right-click reset works through `SliderReset`; only the eased fill went with BUG-061 (param right-click reset folded into SliderReset), so resets have been instant since. The tick still runs every frame for nothing (bugs list, item 3). If Peter wants the ease back, it is one call in the reset path |
| app-ui-049 | delete | no caller; edit the two UI_INVALIDATION_DESIGN.md lines that list it |
| app-ui-050 | delete | no caller; the forbidden-pattern needle goes with it, because the compiler then forbids the call |
| app-ui-052 | delete | the gap is closed: the timeline projection reads hide, pin and order (`ui_frame.rs:617`, `projection/timeline.rs:40-115`), and lane move is wired through `swap_automation_lane_ranks` (`editing.rs:431-448`). The four fns are test-only duplicates; the row could move to A |
| app-ui-058 | delete | test-only; keep the `Icon::Layer*` variants, because atlas ids follow declaration order |
| app-ui-060 | delete | BUG-6in (effect-group headers, folding and collapse animation) is deferred with no group UI designed; 100 lines waiting on an undesigned feature, and git keeps them. Peter's call, since an open bead names it |
| app-ui-061 | delete | shadows have been off by design since July (a dark shadow reads as a smudge on the near-black palette); the switch keeps about 100 lines compiled for a look already rejected |
| app-ui-062 | keep | moving 21 test accessors behind `cfg(test)` removes no lines; churn, not cutting |
| app-ui-063 | keep | same for 10 GPU accessors, and three are named in VULKAN_BACKEND_DESIGN.md |
| app-ui-064 | bead | the environment-variable list is input for the patterns sweep; nothing to judge here |

### water

| id | disposition | the deciding fact |
|---|---|---|
| water-008 | delete | test-only; the publish gate reads the stats on the GPU; drop it from the export list in WATER_CRATES_DESIGN.md section 4.2 (the liquid contract) |
| water-009 | keep | a test seam: deleting it removes 26 lines but makes 8 tests build an evaluated frame to reach `_at_source`, so test code grows |
| water-010 | delete | already ruled dead on BUG-hkbdp.6.5 (Water Cargo hygiene), part of BUG-hkbdp (Renderer crate split epic); the lead counts the roughly 100 lines once across the graph and water files |
| water-011 | delete | no caller since its example went (82ccfa3ab), and the physics boundary design lists creation API, not this tuning knob; re-add it with a caller. Peter's call (engine API) |
| water-012 | keep | PHYSICS_ENGINE_BOUNDARY_DESIGN.md line 132 lists `add_triangle_mesh` in the public creation API, and a static triangle mesh is the one collider convex hulls cannot give (concave ground). Prep risk |
| water-013 | delete | the only product user calls `with_capacity` (`water-rigid/src/physics.rs:439`); the growing branch is test-only |
| water-014 | delete | landed in an unreviewed checkpoint, test-only, and no doc or bead names them |
| water-015 | keep | BUG-hkbdp.6.1 (CPU FLIP decision) keeps the reference engine; these setters are how its proofs configure it |
| water-016 | bead | the coupling probes are our own two-way coupling reference (FLIP Fluids has no body coupling), not engine parity; decide with BUG-65r39 (manifold-fluids proof-only) whether that reference stays |
| water-017 | bead | the same decision as water-016 |
| water-018 | bead | the app links 2,430 lines of probe C++ it never calls; BUG-65r39 is the change that stops the link |
| water-019 | bead | BUG-hkbdp.6.9 (Consolidate water test-only compilation) records the feature failing to compile and BUG-2fmpk (whitewater engine oracle) owns the oracle; fix it and put it on a gate, or delete it with them (bugs list, item 8) |
| water-020 | delete | no script, doc, hook or bead names either example; git keeps them |

### content

| id | disposition | the deciding fact |
|---|---|---|
| content-001 | delete | on stage, Watercolor is the atom-built preset; the wrapper node is in no preset and no project, and it takes the then-empty conversion-debt machinery with it. Load gap: an online-only show that saved an edited Watercolor graph before the rebuild |
| content-002 | delete | a shelf atom whose one wiring made the fluid pooling worse; never in any preset, so nothing on stage reaches it |
| content-003 | delete | `render_scene` instancing replaced it; it drops advanced material fields (BUG-5l4o (Material fidelity follow-through) item 8), so deleting it removes a renderer that cannot show the materials |
| content-004 | delete | GPU FLIP folded the solve into one node; both atoms postdate every online-only project, so no saved show can hold them; the face-grid testkit needs another stand-in pair |
| content-005 | delete | a Math View authoring leftover, out of its preset the day it landed |
| content-006 | delete | four load-only aliases become four `TYPE_ID_MIGRATIONS` rows with seeded `type` or `octaves`; OilyFluid and the 4 corpus projects keep loading through the migration |
| content-007 | keep | no port-rename migration exists, so an old graph wired to `a` to `e` loads only through this node; it rides Peter's old-format call |
| content-008 | delete | created after every online-only project, and its only use was dropped the same day; amend OCEAN_SURFACE_DESIGN.md D7, which keeps it with no user. Peter's call |
| content-009 | keep | open BUG-yqce (scene_mirror re-home after kind-framework removal) plans its home |
| content-010 | keep | the 3D geometry family Peter is building; the Phong retirement removed presets, not atoms. If it is ever deleted, `scripts/gate_policy.py:631-632` goes too |
| content-011 | delete | the Film Grain preset builds its grain from noise and mix and never used this atom: two grains, one shipped |
| content-012 | split | keep `node.peak`: a driver that turns the brightest spot of an image into a knob value, live vocabulary. Delete `node.blob_overlay`: a debug box drawer; Blob Track V2 has the full view |
| content-013 | delete | move to fixtures: the three MPM water scenes are test scenes, Add Fluid authors GPU FLIP only, and every loader is a test, testkit, probe or catalog `examples` string (the catalog only prints `examples`, nothing validates them) |
| content-014 | delete | move to fixtures: its builder is already test-only, and the shipped Dam Break's Physics View toggle shows the same particles |
| content-015 | fix | two `displayName` strings lose "(GPU FLIP)"; the ids stay, so every save loads; a label edit, no code |
| content-016 | keep | hidden, with ids kept for old LED shows; the LED show Liveschool V5 is online-only and was not read |
| content-017 | keep | hidden for saved-project compatibility, and two are repaired at load; old projects may hold them |
| content-018 | delete | the preset loader never scans this folder and Peter shelved the look; close BUG-9wa4 (ReactionDiffusion runs 4 substeps per display frame) with it |
| content-019 | bead | Peter's call: three light types, each with only its own params, and Type goes; 68 corpus projects hold `node.light`, so it needs a migration. A restructure, not a cut |
| content-020 | bead | the water presets and Lantern author non-default bias and resolution, so one constant changes their shadows; needs a look check before cutting |
| content-021 | bead | merge: the environment's sun should follow the scene's Sun light (the glTF importer already binds the two); the five params then retire |
| content-022 | bead | Peter's call: one studio mode or two nodes; only MetallicGlass uses Gradient, and folding it changes MetallicGlass's look |
| content-023 | keep | the correctness oracle for four RT proofs, already hidden in the Scene tab; cutting it costs those proofs a replacement environment whose result is unproven |
| content-024 | bead | box Count sits on the performer's card (`scene_surface.rs:163`) and does nothing unless a copies body is wired (bugs list, item 4); moving the spawner onto the copies body is a restructure |
| content-025 | delete | Off and Removed shade the same (`pbr_material.rs:1227-1236`), so folding Off into Removed and dropping the hand-built Enabled row changes no look; 0 corpus projects store Off or Removed |
| content-026 | fix | mark the 178 import-only sampler fields `hidden` in the scene table, as `rigid_body` already does; the params stay on the node for the importer |
| content-027 | keep | each control has a recent load migration (`solve_level_card_v1180`, `pressure_cap_card_v1190`) and a design control contract; scene-author settings shown in the Scene tab only, never on the card |
| content-028 | bead | Peter's call: these fourteen match the FLIP Fluids engine on purpose, so parity argues keep; hiding them is one table edit |

**Counts.** 62 rows: delete 31, keep 15, bead 13, fix 2, split 1 (content-012). App-ui 21 rows: 13 delete, 5 keep, 3 bead. Water 13 rows: 6 delete, 3 keep, 4 bead. Content 28 rows: 12 delete, 7 keep, 6 bead, 2 fix, 1 split.

## Bugs, not slop

1. `crates/manifold-gpu/src/metal/defer_drop.rs:159`: `pump_deferred_drops` has no caller, yet every `GpuTexture` and `GpuBuffer` drop goes through the probe (`types.rs:47-53`, `:212-214`), and the comment there calls it "the regression oracle". Arm it with `MANIFOLD_DEFER_DROP_FRAMES` or `MANIFOLD_DEFER_DROP_NOOP` and every matching texture and buffer is queued forever: the oracle leaks instead of probing. Disarmed, it costs one retain and release per drop. Row app-ui-013, BUG-l7t4.
2. `crates/manifold-app/src/ui_bridge/projection/inspector.rs:583-675` and `:681-756`: every scene projection builds 14 modulated rows per light, plus full camera and lens payloads, that nothing reads. Line 595 holds a stale light-type list, `["Sun", "Point"]`, with no Spot. Rows app-ui-065 and 066 (BUG-bjk6 (Dead LensRowVm)); deleting them is the fix.
3. `crates/manifold-ui/src/panels/param_card/state.rs:565` and `render.rs:2230`: the eased value snap-back is ticked and checked every frame on every card, but nothing starts it since BUG-061, so resets are instant. Wire it or delete it; the per-frame cost is small. Row app-ui-043.
4. `crates/manifold-nodes-scene/src/node_graph/scene_surface.rs:163`: the physics world's box Count is a card row, so it sits on stage even when no copies body is wired, and then it does nothing (`physics_world.rs:433-463`). Row content-024; walkthrough 1:49 and 10:25.
5. `crates/manifold-app/src/text_session.rs:846-849`: the dead `GroupRename` arm builds `EffectTarget::Layer` for every non-Master tab, so a clip's effect group would be renamed on the layer instead. It is not live; it goes with app-ui-002 and should not be revived as written.
6. content-003: `node.render_copies` uploads only the older material subset (BUG-5l4o item 8), a renderer that silently drops material fields. Nothing uses it, so deleting it is the fix.
7. content-018: BUG-9wa4 tracks a substep bug in a preset the loader never reads; the bead closes with the deletion.
8. water-019: the `whitewater-oracle` feature does not compile (BUG-hkbdp.6.9 notes) and no gate builds it, so the oracle broke unseen.

Checked and dismissed: app-ui-004, 017, 020, 031, 032, 038 and 042 (the grade table above gives the live path for each).

## Notes for the overnight app-ui lanes

- app-ui-002: after the `GroupRename` arm goes, `manifold_editing::commands::effect_groups::RenameGroupCommand` has no app caller; leave it for the engine-model cascade.
- app-ui-027: keep `state_button_skinned`, `StateButtonSkin`, `dropdown_trigger_style`, `segment_style` and `toggle_style`; they are live.
- app-ui-038: once `sync_header_text` goes, `cached_header_text` is always "Layer".
- app-ui-045: after the deletion, `CoordinateMapper::set_zoom_by_index` has only test callers.

## What I did not check

- App-ui grade A rows outside the classes above were not re-run, per the no-sampling ruling: 001, 003, 009, 014, 016, 018, 021, 022, 025, 026, 028, 029, 036, 044, 046 to 048, 053 to 057, 059.
- Non-stage B and C rows (app-ui 005, 007, 010, 011, 015, 024, 049, 050, 052, 058, 060, 062 to 064; water 008 to 011, 013 to 020; content-018) were not fully re-run; each disposition rests on the fact named in its line, which was checked.
- No compile, test or GPU run: every cascade and every "nothing else breaks" is for the deleting lane's `cargo check`, clippy and the landing gate.
- The 67 online-only Dropbox projects were not opened. The load gaps named in content-001, 002, 003, 005 and 011 stand.
- Hidden presets the content sweep did not list: `MriVolume` (5 corpus projects, `rg` finds it in the glTF importer and `preset_type_id.rs`) and `Scene`. Both look code-built, like `MaskBlob`; not judged.
- The content sweep's param rows were checked against the code lines they cite, not for look; a cut or merge there needs a look check.
