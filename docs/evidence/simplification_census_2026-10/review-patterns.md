# Simplification census review: patterns

- **Reviewer:** Opus 5.5, read-only on code, part of BUG-yn979 (Simplification pass).
- **Files reviewed:** `patterns.md` and `patterns.proof.md`.
- **Tree:** main 8dd7675a7. `git diff --stat 37a9c180a HEAD -- crates` is empty, so every sweep line number still holds.
- **Method:** SIMPLIFICATION_CENSUS.md section 6 (Review), light version: no random sample. Proofs were re-run for every row over 200 lines (006, 020, 021, 022, 024, 032), every stage or prep row (004, 006 to 019, 022, 024, 031 to 037, 040), and the four "possible real bugs". Extra searches: the corpus for numeric node ids repeated across group scopes, `RENDERER_CRATE_SPLIT_DESIGN.md` for the registry seams, `git log -S` on the seams and the kill switches, and the worker atoms' receive paths.

## Grade changes

Three grade cells changed in `patterns.md`. The totals block there was left as written; with these changes B is 25 rows (about 2,830 lines) and C is 7 rows (about 1,370 lines plus the knob list).

| row | was | now | why |
|---|---|---|---|
| patterns-016 | B | C | `SceneExposureSource` is a named seam. RENDERER_CRATE_SPLIT_DESIGN.md section 6 (Decided — do not reopen), item 14, lists "Scene exposure" among the P1 registration seams, landed 2026-10-07 (99c47677e). The sweep's own layering rule calls a registry declared low and filled high a required seam, and this one is declared in node-engine and filled by nodes-scene. Folding it into core `NodeParamSource` reopens a settled design, so it is a decision, not cleanup. The `NodeParamProvider` half has no such conflict and can go on its own. |
| patterns-017 | B | C | The same item names `PresetCatalogSource` ("providers sort by name, first lookup hit wins") and the metadata-reload inventories ("provider names are required for deterministic ordering"). It was built for more than one catalog provider and landed 12 days ago in the do-not-reopen section. Merging the five into one core registry reverses that call. |
| patterns-040 | C | B | The open question ("a missing wire or a dead fn") is answered by the code. The audio dock is its own column: `layout.rs:107` takes `audio_setup_width` out of the content area, and `audio_setup()` (`layout.rs:184-191`) sits beside the inspector. A drag that starts in the dock and grabs nothing can't fall inside `tracks_rect`, so `resolve_drag_owner` returns nobody (`ui_root/drag.rs:79-84`) and nothing leaks to the timeline. It is a dead fn with a false doc. No call for Peter. |

Claims found wrong or incomplete where the grade still holds:

- **patterns-019:** the premise is wrong. Core `find_node` and `find_node_mut` (`effect_graph_def.rs:309-322`) already search nested groups, through `SceneNodeRef::locate_nodes` (`scene_modifier_preset.rs:83-106`), added in aee5ea900 ("shared group-aware node lookups"). All five copies match by stable `node_id`. None matches by numeric id, so the "must take both" gap is not real. The fix gets simpler: call core's `find_node`. The one difference is that core skips groups whose own `node_id` is empty. B holds.
- **patterns-033:** it misses a second rate arm, in clip triggers (`playback live_trigger.rs:257-261`), and the shape-equality check in editing `commands/audio_mod.rs:230`. Both go in the same deletion. B holds.
- **patterns-022:** the same command at the same commit counts 125 sites, not 131. The row is an estimate anyway, so C holds.
- **patterns-004:** the named gap is smaller than stated. `audit_rename.py`'s alias step edits `crates/manifold-nodes/src/effects/kaleidoscope.rs`, which no longer exists, and `dev.py:199-202` lists the script as internal or retired. Nothing has to switch to JSON in the same landing. B holds; it could be A once the stale line in `PRIMITIVE_LIBRARY_DESIGN.md:1119` is fixed.
- **patterns-009:** the compile gap is resolved by reading. Nine of the eleven trait methods forward to inherent methods of the same name (`instance.rs:1625-1655`), and method calls already resolve to those. No crate outside core imports the trait. Only `display_name` and `get_param_def` live on the trait alone. B holds.
- **patterns-010:** it also misses core `percussion_analysis.rs:461` (`Option<&mut dyn BeatTimeConverter>`). **patterns-014:** the app contract test `registered_assets.rs:9` already pins exactly one submission, which makes the in-code assert redundant. Both stay B.

Rows re-run and confirmed as written: 006 (41 trait methods, 41 overridden, `comm` empty), 007, 008, 011, 012, 013 (BUG-ns5k (Benchmark legacy-path MetalFX denoised scaler) still open), 015, 018 (seven product copies), 020 (30), 021 (73, and `FrameTime` has no `impl` at all), 024 (switch since 2026-07-18, 84 files, 0 of 182 projects mention relight), 031 (read once at construction, not per frame), 032 (never constructed; every `Some` branch unreachable), 034 to 037.

## Bugs, not slop

1. **Export's frame-perfect flush does nothing. Confirmed.**
   - `content_export.rs:1123-1127` calls `flush_all_background_work` "so export is frame-perfect". That goes through `content_pipeline.rs:4152` to `layer_compositor.rs:3745-3749`, which loops `plugin_warmups`.
   - The list is filled at `layer_compositor.rs:748` from `plugin_prewarm::prewarm_all` (`plugin_prewarm.rs:75`). `PluginPrewarm` has no submissions anywhere in the repo, so the list is always empty.
   - The worker atoms that replaced those workers don't make up for it:
     - `detect_regions.rs:567-578` blocks by design. `optical_flow_estimate.rs:516-524` blocks too, but only in its fixed-lag mode.
     - `blob_detect_ffi.rs:392-395` waits at most 2 ms, then gives up.
     - `depth_estimate_midas.rs:282` and `person_segment.rs:315` never wait.
     - No atom in nodes-image reads `sim_step.offline()`. The water atoms do.
   - On stage: live playback is fine, because a frame or two of latency is designed in. An exported video that uses MiDaS depth (Wireframe Depth), person segmentation, or blob tracking on a slow frame can lag what was authored and differ from one export to the next.
   - Fix shape: the worker atoms block when `ctx.sim_step.offline()` is set. Delete the dead flush chain: `Compositor::flush_all_background_work`, the pipeline wrapper, the export call and its comment.
   - Unverified by a run. Oracle: export one clip with Wireframe Depth twice and diff the frames.
2. **Env reads on the render hot path. Confirmed.**
   - `frame_preliminaries` (`render_scene.rs:5981`) runs every frame for every Render Scene node. It reads `MANIFOLD_DENOISE_PROBE` (:5988) and `MANIFOLD_PROBE` (:6412).
   - `MANIFOLD_DENOISE_SKIP` (:5642) is read on every frame with denoise on. `MANIFOLD_DISABLE_EMISSIVE_SAMPLER` (:3605) is read on every ray-traced frame.
   - Each read is a `getenv` under the standard library's environment lock, with no allocation while unset. The cost is small, but it is per-frame work that hot-path discipline forbids. `MANIFOLD_RT_SOURCE_TRACE` (:246) shows the right pattern: a `OnceLock`.
   - patterns-026 to 028 and 030 remove them.
3. **Clip triggers condition a value and throw it away. New.**
   - `live_trigger.rs:250-256` calls `shape.condition()` on every audio hop for every clip trigger and drops the result (`_conditioned`).
   - Its only lasting effects are two values. `smoothed` is never read; it is only reset, at :76. `prev_raw` feeds only the rate arm at :257, which nothing can switch on (patterns-033).
   - This is wasted work on the audio-trigger path, and it goes with patterns-033.
4. **The scene projection builds view models nobody reads.** patterns-034 to 037, confirmed as written. Every scene projection builds transform, material, environment and modulation rows, including one boxed modulation lookup per row.
5. **False invariant docs about node ids. New, and the real form of the "id collision" suspicion.** See the refutation below. `effect_graph_def.rs:111-113` says a numeric `id` "is unique within the document". `scene_object_migration.rs:144-146` says ids "are unique across the WHOLE document, group bodies included". Saved projects disprove both. One numeric nested lookup is still in production: core `effects/bindings.rs:289-300`, reached through the empty-scope fallback at app `ui_bridge/project.rs:1226-1232`. It searches depth-first, so it can return a node inside an earlier group that shares the id. The reach is narrow (imported nodes, empty scope). Oracle: a CPU test where a group body repeats a top-level id, resolving the top-level node's binding. Fix: rewrite both docs to "unique within its scope", and key that lookup by scope path or stable `node_id`.

Refuted:

- **`AudioSetupPanel::claims_drag` as a missing wire.** It is dead code, not a lost behavior (see patterns-040 above). The tests at `audio_setup_panel.rs:2266-2310` pin a claim production never makes, and the doc at :2069-2071 and the comment at `drag.rs:20-21` describe a wire that went when the dock stopped being an overlay. Delete all three.
- **Next-id helpers that scan only the top level can collide.** Numeric ids are per scope in practice.
  - In the corpus, 78 projects have graphs with groups. 43 of them already repeat numeric ids across scopes; in Liveschool Live Show V6 AUDIO, 15 of its 16 group-holding graphs do. Script: scratchpad `id_dupes.jq`.
  - The app says so in code: "Recipe-local numeric ids may repeat in nested groups; stable binding targets do not" (`ui_bridge/project.rs:1220-1221`).
  - Wires are per scope, flattening renumbers (`flatten.rs:161-162`), and bindings target the stable `node_id`.
  - So a helper minting a top-level id needs only the top-level max. `fragment_cuts.rs:117`, `liquid.rs:204`, `blob_bounds.rs:184`, `group_edit.rs:174` and :307 are correct.
  - The seven recursive `max_node_id` copies (patterns-018) are only conservative. When they merge, the merged doc must not restate the whole-document rule.

## Dispositions for every B and C row

| id | disposition | the deciding fact |
|---|---|---|
| patterns-004 | delete | 0 submissions; effects get aliases from JSON `param_aliases` (`preset_definition_registry.rs:637`); the rename tool's target file is already gone |
| patterns-005 | bead | the inventory `EffectMetadata` path fires only for 13 test submissions; moving tests to JSON fixtures is a test refactor, not a deletion |
| patterns-006 | delete | one impl overrides all 41 methods; app depends on compositor directly, so the trait is no seam |
| patterns-008 | delete | one unit-struct impl, held as a concrete field |
| patterns-009 | delete | 9 of 11 methods forward to same-named inherent methods; no crate outside core imports it; move the other two inherent |
| patterns-010 | delete | one product impl each; the second impls are test mocks |
| patterns-011 | delete | one FFI impl each, every caller names the `Ffi*` type; another platform ships another library behind the same loader, not another impl |
| patterns-012 | delete | one impl, no mock |
| patterns-013 | keep | BUG-ns5k is open and benchmarks the legacy path this trait wraps; it decides whether that path, and this trait, survive |
| patterns-014 | delete | one compile-time submission, already pinned by the app contract test `registered_assets.rs:9` |
| patterns-015 | fix | seven copies of take-one-and-check; one generic helper in core (about 15 lines, cheap) |
| patterns-016 | bead | decision: the crate-split design's do-not-reopen section names the scene-exposure seam; `NodeParamProvider` can go now, `SceneExposureSource` waits on that call |
| patterns-017 | keep | the crate-split design's do-not-reopen section names these seams and their provider-name ordering |
| patterns-018 | fix | seven copies of one walk; one `EffectGraphDef` method, without the false whole-document doc |
| patterns-019 | fix | core `find_node` already searches groups; replace the five copies |
| patterns-020 | fix | `ExecutionPlan` has no `output_resource`; 30 copies, test-only apart from two |
| patterns-021 | fix | `FrameTime` has no constructor; one testkit constructor replaces 73 copies plus the `frame_at` variants |
| patterns-022 | bead | about 125 hand recursions; needs a walker API on `EffectGraphDef` first, then a crate-at-a-time fold |
| patterns-023 | fix | the same seven vector helpers twice inside one crate |
| patterns-024 | bead | Peter's relight call (below), one decision with engine-046 and graph-026 |
| patterns-027 | delete | ablation for BUG-iadf (RT denoise 1:1 mode), closed 2026-08-08; read every denoised frame |
| patterns-030 | fix | per-frame env read in product code that exists for one GPU proof leg; the proof sets a param instead |
| patterns-031 | delete | the knob only turns off a default-on skip; set by nothing, documented nowhere |
| patterns-032 | delete | never constructed since the port on 2026-03-19; every `Some` branch is unreachable |
| patterns-033 | delete | nothing can switch it on; take the clip-trigger rate arm and its dead `condition()` call with it (bug 3) |
| patterns-034 | delete | built every projection, no production reader |
| patterns-035 | delete | built every projection with placeholder values, no production reader |
| patterns-036 | delete | only the variant and `hdri_file` are read |
| patterns-037 | delete | no reader once 034, 036 and app-ui-065/066 land; today a boxed lookup per row for nothing |
| patterns-038 | delete | built, never read |
| patterns-040 | delete | dead fn; the dock is its own column, so an unclaimed drag can't leak |
| patterns-041 | bead | Peter's knob list: the cheapest rule is that a knob named for a closed bug goes, and a knob with no doc goes unless he uses it |
| engine-049 | delete | `bench-timing` gates two wall-clock tests no gate runs |

### The two kill switches

**`RELIGHT_FEATURE_ENABLED`** (`feature_flags.rs:16`, off since 2026-07-18).

- **Turn it back on** (flip one const):
  - Every effect and generator card gets its "3D" toggle back, with six knobs and a "Height From" row.
  - Any 2D layer can be lit as a relief from its depth: normals from depth, ambient occlusion, shadows and specular.
  - It was switched off because it "renders poorly and is not ready for the stage". Acceptance never ran: the look sweep, the perf soak and Peter's sign-off.
  - Tests are gated both ways, so the feature still compiles, but no render of it has been checked in three months. Turning it on means doing that acceptance work.
- **Delete it:**
  - About 1,800 lines go across 84 files: the relight stage, the `RelightAugmentation` seam, the card branches, `SetRelightParamCommand` and the gated tests.
  - No saved project mentions relight (0 of 182), so no load migration is needed.
  - Bringing it back later means reverting that landing.

**`SHADOWS_ENABLED`** (`ui color.rs:148`, off since 2026-07-14).

- **Turn it back on:**
  - Soft black drop shadows (alpha 110, 18 px blur, 4 px drop) appear under floating panels: dropdowns, settings, modals (`tree_passes.rs:96-120`).
  - They also appear under selected timeline clips (`clip_draw.rs:64-85`).
  - It was switched off because dark-on-dark shadows read as smudge, not lift.
- **Delete it:**
  - About 100 lines go: `draw_shadow` (`ui_renderer.rs:1000-1028`), the shader's negative-border branch (`ui_renderer.rs:86-102`), six constants and two gated call sites (app-ui-061).
  - Neither choice changes anything on stage or in frame time. The shader branch is one compare that is never taken.

## What I did not check

- No random sample, per the light-review ruling. Rows at no risk and under 200 lines were not re-run: 001 to 003, 023, 025 to 030, 038, 039 and 041's knob table.
- The "Kept seams" list was not re-judged one by one, apart from the two catalog and exposure seams that changed grade.
- Export determinism of the worker atoms was not run (bug 1), and the numeric-id lookup was not tested (bug 5). Both name their oracles.
- Whether any default in the never-built percussion settings (patterns-032) differs from `create_default`.
- Per-scope duplicate ids in the corpus: that scan was cut short, so only cross-scope repeats were counted. Bundled presets showed no grouped graphs to check.
- No compile check: cascades are for the deleting lane's `cargo check`.
