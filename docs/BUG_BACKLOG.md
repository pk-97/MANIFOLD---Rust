# Bug Backlog

<!-- index: Live, human-and-agent-facing tracker for known bugs not yet fixed. Each entry has a stable ID, a root-cause location, the user-visible symptom, a fix shape, and (when one exists) an #[ignore]'d test that goes green when fixed. -->

The repo had no bug tracker — bug knowledge lived only in agent memory, git history, and
session context. This file is the durable, in-repo home. It travels with the code, any agent
or human can read it, and it needs no external tool.

## How to use this file

- One entry per known bug, with a stable ID (`BUG-NNN`). Never renumber — IDs are referenced
  from commits, tests, and memory. (One historical exception: 2026-07-09 a duplicate `BUG-031`
  was split; the unreferenced audio-blip half became `BUG-081`.)
- **Status lives in one place: a `**Status:` line directly under each `### BUG-NNN` heading.**
  This is the single source of truth — the `## Open` / `## Fixed` section and the index table
  are *derived* from it, not authored in parallel (three copies of one fact is how this file
  drifted). Values: `OPEN` · `FIXED @ <sha>` · `PARTIAL` · `PARKED` · `DEFERRED` · `REOPENED` ·
  `SUPERSEDED`. `FIXED`/`SUPERSEDED` belong under `## Fixed`; everything else stays under
  `## Open` and in the index.
- **Tooling — `python3 .claude/hooks/bug_status.py`** checks the whole file for drift (a Status
  line that disagrees with its section, a resolved bug still in the index, an open bug whose
  named fix-design has SHIPPED, a duplicate id, an index row with no entry). `--write` inserts
  any missing Status lines and reflows entries into the right section behind a content-fidelity
  guard. The post-merge housekeeper (`design_status_check.py`) runs the same check and prints
  nudges — mirroring how design-doc statuses stay honest.
- The strongest form of an open entry is an **executable** one: an `#[ignore = "BUG-NNN"]`
  test that fails for the right reason. The bug is then self-documenting and self-closing —
  remove the `#[ignore]` when the fix lands and the suite enforces it forever.
- When you fix an entry, set its `**Status:` line to `FIXED @ <sha>` (add a **Fixed:** note on
  how) and run `bug_status.py --write` to reflow it into **Fixed**. Don't delete it — the
  history is the point.
- Severity is about the **instrument on stage**, not code aesthetics: `HIGH` = wrong output
  or silent data corruption a performer would hit; `MED` = reachable but narrow; `LOW` =
  latent / cosmetic / needs an unusual setup.
- **Escape analysis (added 2026-07-05):** a bug found in the app after an orchestrated
  landing carries one extra line in its entry — `Escaped: <wave/branch> · caught-by:
  <brief | gate | demo | held-out input | review>` — per `DESIGN_DOC_STANDARD.md` §10.
  Over time this is the empirical record of which orchestration stage leaks, so process
  fixes target the leaking stage instead of guessing.
- **Closed-bug archive (added 2026-07-12):** the `## Fixed` section below is a one-line
  pointer per entry — `- BUG-NNN (slug) — FIXED @ <sha or date> — full history in
  docs/archive/BUG_BACKLOG_CLOSED.md` — not the full write-up. The full body (root cause,
  fix, verification trail) moved verbatim to [docs/archive/BUG_BACKLOG_CLOSED.md](archive/BUG_BACKLOG_CLOSED.md)
  to keep this file's context cost proportional to what's still open. Grep the archive for
  the investigation history of any closed bug; IDs never change across the split.

---

## Index of open bugs (nickname → say this in chat)

| ID | Nickname | One line |
|---|---|---|
| BUG-298 | **slider-fill-under-modulation-pixel-unverified** | the `--script` harness now proves a modulation-changed param VALUE (text) across snapshots (BUG-234), but the slider FILL/thumb visual under modulation is only inferred-correct (rides `reconcile_state`'s `sync_param_value` off the same `effective` value) — never confirmed by a rendered PNG diff; this is the gap VD-031 originally mislabeled "BUG-235" before that id was reassigned to an audio-timing bug — LOW (verification gap) |
| BUG-290 | **gpu-proofs-test-binary-faults-gpu-firmware** | AGX firmware faults (BIF0 page fault + progress timeout) during GPU test runs freeze every display; hardening landed @ `7cacb965` (cross-process flock + sg_blur_dynamic clamp), conviction run owed with Peter present |
| BUG-297 | **multi-session-memory-exhaustion-freezes-machine** | concurrent-session lockups are THRASH, not jetsam-of-apps: rust-analyzers (~16GB) + gate builds + Spotlight re-indexing worktree `target/` dirs (mds_stores dirtied 8.6GB over the 2026-07-20 orchestration window) exceed RAM and the machine freezes with NO JetsamEvent/gpuEvent fingerprint; sampler now instrumented at `~/Library/Logs/manifold-bug297/` |
| ~~BUG-296~~ FIXED | **script-driver-effect-cards-never-rebuild-after-structural-dispatch** | FIXED 2026-07-21 — real root cause was `Runner.active_layer` never seeded from the fixture's active layer, not a missing rebuild; a structural dispatch was silently mutating the WRONG layer's chain. Seeded `runner.active_layer` from `data.active` in `script.rs::run` |
| ~~BUG-294~~ FIXED | **scene-setup-dock-scroll-headless-noop** | FIXED 2026-07-21 — `Gesture::Scroll` now routes into the Scene Setup dock's own `ScenePanel::handle_scroll` setter when the pointer falls inside its rect, and forces `needs_structural_sync` (mirrors `window_input.rs`'s explicit `needs_rebuild`) |
| ~~BUG-293~~ FIXED | **script-driver-discards-context-menu-actions** | FIXED 2026-07-21 — `drain_and_dispatch` now takes `host.pending_actions` and routes them through `apply_panel_actions`, mirroring `app_render.rs:1471` |
| BUG-283 | **manifold-app-clippy-tests-target-drift** | `cargo clippy -p manifold-app --tests -- -D warnings` fails on three pre-existing files (doc_lazy_continuation / cloned_ref_to_slice_refs / approx_constant); the standard gates never compile the test target so the drift is invisible — LOW |
| BUG-282 | **graph-canvas-unbound-param-scrub-floods-undo-stack** | dragging an unbound graph-node-face param/vec scrub pushes a fresh undo-worthy `Execute(SetGraphNodeParamCommand)` on EVERY pointer-move tick instead of batching to one entry at drag-end, unlike every other scrub family in the codebase — MED |
| BUG-281 | **graph-canvas-bound-param-scrub-unguarded-mid-gesture** | a graph-node-face param scrub on a card-bound row live-writes `local_project` every tick via `bound_node_param_drag`, but the snapshot-acceptance restore path only ever consults `active_inspector_drag` — a mid-gesture snapshot reverts the in-flight value — MED |
| BUG-280 | **marker-drag-unguarded-mid-gesture-race** | dragging a timeline marker writes its live beat directly into `project.timeline` every frame with no snapshot-suppression coverage (`MarkerDrag` lives outside `InteractionOverlay`'s `DragMode`, so `app_render.rs`'s `drag_active` check never sees it) — a mid-gesture content-thread snapshot reverts the drag — MED |
| BUG-248 | **gui-fps-degradation-persists-after-deleting-heavy-static-glb-layers** | GUI FPS degradation reported after importing/deleting heavy static (non-animated) glb layers; headless import render is clean (430 MB, converges frame 4) — root cause unknown, needs in-app profile — MED |
| BUG-247 | **gltf-import-peak-rss-residual-per-source-node-whole-file-parses-not-deduplicated** | glTF import peak RSS residual (dragon fixture: 1.45 GB measured vs ~0.6–0.7 GB predicted from the shared anim cache alone) — suspected cause: mesh/texture source nodes independently re-parse the whole file with no shared-document cache, unlike the anim cache's dedup — MED |
| BUG-246 | **unguarded-inspector-gestures-race-full-snapshot-acceptance** | audio-gain / mod-config / trim-style drags have no `ActiveInspectorDrag` guard, so a full project snapshot accepted mid-drag (data_version bump from any concurrent Execute) stomps the in-flight value — wrong or missing undo pair on release — MED |
| BUG-243 | **analyzer-false-fires-on-sustained-pads** | sustained pad/swell material fires transient + kick events with zero real hits — analyzer false positives on non-percussive material — MED |
| BUG-245 | **mapping-popover-trim-fields-dont-track-external-edits-after-open** | an open mapping popover's trim min/max fields don't track edits made elsewhere while it's open; reopen reseeds correctly — LOW |
| BUG-244 | **graph-canvas-apply-live-values-skips-non-numeric-param-kinds** | the editor canvas's per-frame live-value overlay is scalar-only; enum/color/vec/table on-face values stay frozen until a graph_version bump — LOW-MED |
| BUG-239 | **headless-script-harness-shows-stale-value-after-nontrivial-dispatch** | a `--script` flow's PNG/tree-dump keeps showing a param's PRE-write value after a real, correctly-dispatched write — headless-verification-only gap, live app unaffected — MED |
| BUG-233 | **gizmo-move-scale-x-axis-color-collides-with-viewport-grid-x-axis** | the move/scale gizmo's red X-axis handle and the viewport grid's red X-axis line are near-identical colors, so the handle is hard to pick out with both overlays on — legibility only — LOW |
| BUG-228 | **manifold-app-tests-gate-clippy-debt-recurrence** | `cargo clippy -p manifold-app --tests -- -D warnings` fails on 3 pre-existing unrelated lints, so the tests-profile clippy gate can't be enforced — LOW |
| BUG-227 | **madmom-onset-detector-not-gain-or-timestretch-invariant-on-real-mixes** | the madmom CNN onset detector fails two of three P1 metamorphic invariants (gain / time-stretch) on real mixes — measurement-trust issue for audio tuning — MED |
| BUG-226 | **golden-png-tests-overwrite-their-own-references-every-run** | animated/skinned/morph gpu-proof "golden" tests regenerate their checked-in reference PNGs unconditionally instead of diffing against them — the gate can never fail — MED |
| BUG-225 | **ui-flow-harness-first-gesture-free-rebuild-masks-missing-invalidation** | the headless flow harness gives the first dispatched gesture a free rebuild, so missing rebuild-invalidation bugs are undetectable in flows — harness gap — LOW-MED |
| BUG-219 | **abeautifulgame-interactive-import-crashes-doubled-full-gpu-build** | importing `ABeautifulGame.glb` through the running app's drag-drop path crashes (doubled full-GPU build); the same file renders fine headless — MED |
| BUG-235 | **manifold-own-kick-fixtures-systematic-adtof-timing-bias** | Raw ADTOF kick detections on the 5 `manifold_own` electronic kick fixtures show a tight, track-dependent, systematically-EARLY offset (20–125ms) vs. their hand-labeled truth — not random misses (feel_the_vibration_174bpm: all 15 predictions land 80–120ms early, near-monotonically). Confirmed NOT a scorer column-swap bug: `mix_time_s`/`drums_time_s` are byte-identical for 4/5 tracks, and bad_guy's existing column choice (`mix_time_s`) already carries the smaller offset. A pre-existing (P1-recorded, not P3-introduced) onset-convention gap between ADTOF and this pack's hand-labeled truth — found 2026-07-17 during AUDIO_ANALYSIS_ACCURACY P3, orchestrator diagnostic |
| BUG-232 | **harmonix-matched-audio-carries-multi-second-timing-offset** | YouTube-matched Harmonix audio (`eval/fetch/harmonix_audio.py`) commonly carries a multi-second constant timing offset vs. the Harmonix annotation reference (2.9s–7.3s seen across a 15-track sample) — raw beat_f1 against these matched tracks is near-zero (0.11 mean) despite Beat This tracking a plausible beat count; a difference-histogram shift estimator recovers beat_f1 to 0.65 mean once corrected. A data-alignment gap, not a detector bug — blocks using Harmonix-matched audio for beat/downbeat tuning until productized — found 2026-07-17 during AUDIO_ANALYSIS_ACCURACY P3 |
| BUG-231 | **beat-this-no-tempo-hint-api-for-octave-fix** | `beat_this`'s license-clean "minimal" postprocessing path has no BPM-range/tempo-prior parameter anywhere in its inference API — the `liveshow_integer` 115.4-vs-132-BPM (~7:8 ratio) mis-track from P2 can't be fixed by passing a hint; a future fix needs a post-hoc heuristic on Beat This's own output, not a detector-side parameter — found 2026-07-17 during AUDIO_ANALYSIS_ACCURACY P3 |
| BUG-230 | **beat-this-short-clip-bpm-convergence** | Beat This converges on the same ~142.86 BPM for 4 of 5 unrelated ~13s isolated-stem test clips (apricots/bad_guy/feel_the_vibration/tears; only inhale_exhale differs, at a plausible half-tempo) — found 2026-07-17 during AUDIO_ANALYSIS_ACCURACY P2 listen-list render |
| BUG-229 | **beat-this-frame-hop-exceeds-5ms-alignment-target** | Beat This's 50fps (20ms) frame resolution puts absolute click-to-beat alignment at a measured ~14ms median / ~26ms max (D14 gate wanted <=5ms) — a real model-precision floor, not tunable in P2 — found 2026-07-17 during AUDIO_ANALYSIS_ACCURACY P2 |
| ~~BUG-234~~ FIXED | **ui-snap-script-harness-never-runs-a-content-thread-tick** | FIXED 2026-07-21 — `Step`'s frame loop now calls the existing `manifold_playback::modulation::evaluate_modulation` (drivers + envelopes in one call, same entry the content thread's own tick uses) plus `reconcile_state` to resync the widget tree; `scripts/ui-flows/envelope-modulation.json` + the new `envmod` fixture prove it. |
| ~~BUG-218~~ FIXED | **modifier-commands-splice-at-dead-group-output-vertices-port** | FIXED — `walk_mesh_modifier_chain`/`splice_modifier_into_chain` now resolve the group's `node.scene_object` via the group output's `object` producer and walk/splice against its own `vertices` port, not the dead `system.group_output` `vertices` port; verified via `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-modifier-stack.json`. |
| ~~BUG-216~~ FIXED | **feedback-loop-into-final-output-freezes-at-depth-one** | FIXED @ f2684402 — `late_capture` now falls back to a format-bridge copy (blit/resize_sample) when the ping-pong swap refuses on a borrowed boundary slot, instead of dropping the frame's capture. |
| BUG-217 | **non-lerp-mix-alpha-passthrough-kills-trails-on-transparent-sources** | Max/Add feedback trails over an alpha-0-background source accumulate RGB but inherit the source's alpha, so the display culls them; `set_alpha` before the blend is the idiom — LOW |
| ~~BUG-215~~ FIXED | **conformance-sweep-panics-on-duplicate-mat-0-handle** | FIXED — a glTF material authored `"mat_N"` (e.g. `MetalRoughSpheresNoTextures.glb`, 98 such materials) collided with that object's own `mat_{k}` inner-node handle once SCENE_OBJECT_AND_PANEL_V2 P3 started stamping the group + scene_object with the material's raw name, panicking `Graph::add_node_named` on duplicate handle `"mat_0/mat_0"` — reddened the conformance sweep; independently hit by W1, W5, and W6, fixed by W5. |
| ~~BUG-212~~ FIXED | **duplicate-scene-object-string-bindings-dangle-on-imported-mesh** | FIXED — `DuplicateSceneObjectCommand` now clones every `string_bindings` entry whose target falls inside the duplicated subtree, re-targeted at the clone's fresh NodeId; undo restores the whole vec. |
| BUG-096 | **camera-rotate-sliders-jump-no-degrees** | FluidSim3D Rotate X/Y/Z sliders jump instead of rotating smoothly, no degrees readout — PARTIAL 2026-07-10 (legacy orbit phase + tilt sign restored in preset; degrees readout + jump investigation still open) |
| ~~BUG-211~~ FIXED | **conformance-harness-advancing-clock-cant-converge-animated-imports** | FIXED — `glb_conformance`'s render loop (and the `render-import` CLI's twin) advanced time every frame, so post-GLTF_ANIMATION auto-playing assets re-posed forever, never went byte-stable, and reported a phantom "black" frame (the 0.0000 is `last_fraction`'s initializer, never assigned); clock now frozen (`--time` flag on the CLI), six stale goldens regenerated for the legitimate BUG-205/206 reframes. |
| ~~BUG-207~~ FIXED | **materialless-skinned-mesh-silently-imports-static-at-node-scale** | FIXED — the default-material bucket (`nodes_by_material`'s `None` key) is now a first-class key resolved by the SAME shared functions a real material uses, so a materialless skinned/morphed/animated rig resolves its skin/morph/animation exactly like a materialed one. |
| BUG-209 | **animated-ancestor-above-joint-tree-sampled-statically** | root motion authored on a node ABOVE a skin's joint tree is frozen at its static TRS (`joint_root_world` is static by design; the rigid path that would have carried it is correctly excluded post-BUG-205) — LOW until a real asset exhibits it |
| ~~BUG-210~~ FIXED | **add-scene-object-command-emits-pre-migration-legacy-wires** | FIXED @ 6e8b00ba — `AddSceneObjectCommand`'s `catalog_default` now binds mesh/material/transform through an inner `node.scene_object`, wired to `render_scene`'s `object_k` port. |
| BUG-203 | **fluidsim2d-count-dims-display** | FluidSim2D: raising Particle Count dims the image instead of reading as more particles — MED |
| BUG-201 | **interaction-overlay-automation-callback-type-complexity** | `manifold-ui --all-targets` clippy fails on 4 `type_complexity` findings in `interaction_overlay.rs`, unrelated to BUG-112 — LOW (lint-only) |
| BUG-170 | **gltf-crate-missing-field-node-parse-failure** | five Khronos assets fail at `gltf::import()` itself with `missing field 'node'` — a crate-level JSON-shape parse gap, not an extension-support gap |
| BUG-173 | **nodeperformancetest-exceeds-object-safety-bound-by-design** | Khronos `NodePerformanceTest.glb` (10,000 materials) exceeds `OBJECT_SAFETY_MAX` (1024) and is correctly rejected, not silently truncated — GLB_CONFORMANCE_DESIGN's "any glb, 1:1" promise doesn't reach mega-scene stress-test assets |
| BUG-174 | **unlit-materials-import-as-lit-not-routed-to-unlitmaterial** | `gltf_import.rs` never reads `KHR_materials_unlit`; every imported glTF material becomes a lit (Phong-ish) material even when the source asset is unlit by design |
| BUG-177 | **glb-vertex-colors-not-wired-color0-never-read** | glTF's `COLOR_0` vertex attribute is never read anywhere in the mesh pipeline, so per-vertex color (the entire point of `BoxVertexColors.glb`) has no path from import to pixel |
| BUG-178 | **gltf-import-manual-is-multiple-of-clippy-lint** | `cargo clippy -p manifold-renderer --tests -- -D warnings` fails on two pre-existing `while len % 4 != 0` loops clippy's `manual_is_multiple_of` lint now flags |
| BUG-179 | **fusion-coverage-baseline-floor-stale-32-vs-33** | `node_graph::freeze::proof::fusion_coverage_baseline`'s D4/P6 ratchet floor (`fused_presets >= 33`) fails deterministically at HEAD (`d61eb73b`), pre-existing and unrelated to GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1 |
| BUG-180 | **large-glb-import-oom-risk** | importing a large glTF/GLB (multi-hundred-MB, dozens of images) can get the app SIGKILLed by the OS, and intermittently succeeds/fails on the same file |
| ~~BUG-185~~ FIXED | **e6-texture-completion-invalidates-two-stale-goldens** | FIXED with BUG-211's landing — CompareSpecular's golden re-baselined (render eyeballed), CompareVolume's G-R region moved to the bowl's thick lower interior where the thicknessTexture-driven tint actually lives (measured 10.13, floor 8→6); both were E6's legitimate render changes against stale expectations. |
| ~~BUG-186~~ FIXED | **sheenwoodleathersofa-webp-error-message-misattribution** | FIXED — IMPORT_ANYTHING_WAVE_DESIGN.md W1 shipped webp texture decoding; `SheenWoodLeatherSofa.glb` now imports cleanly instead of being rejected, so the misattributed-error-message question is moot. |
| ~~BUG-206~~ FIXED | **import-framing-crops-elongated-objects** | FIXED @ e182a391 — tall/thin imports overflow the synthesized orbit camera's frame; `distance` now the max of the old `2.2 * radius` diagonal floor and a per-axis fit distance `(extent/2) / tan(fov_y/2) * 1.15` over all three bbox axes, so an elongated object's dominant axis drives the framing instead of being swamped by the diagonal. Compact assets unaffected (floor unchanged; 56-asset conformance sweep still 56/56). |
| ~~BUG-205~~ FIXED | **skinned-import-double-transform-and-wrong-bbox-space** | FIXED @ 3aeafe4a — a skinned object got a rigid `gltf_animation_source` (resolved from an animated ANCESTOR above the joint tree) wired into its transform_3d, re-applying the ancestor chain (Sketchfab's Bip01 0.0254 scale) on top of the joint palette → skeleton rendered as a ~12px speck; the summary bbox also framed mesh-node-world space instead of bind-pose-skinned space (feet cropped). Skinned objects now skip the rigid anim source; summary bboxes skinned prims through bind-pose skin matrices. |
| ~~BUG-204~~ FIXED | **animated-glb-import-rejected-by-retrigger-card-lint** | FIXED @ 6d7cac31 — A4's Retrigger card param (is_trigger) bound to `trigger_count` declared `ParamType::Int`; card lint (d) rejected the assembled graph, so EVERY animated or rigged glb failed at import. All three animation nodes' `trigger_count` flipped to `ParamType::Trigger`; regression test on skeleton_animated.glb. |
| ~~BUG-184~~ FIXED | **automation-clear-lane-not-wired-to-ui** | FIXED 2026-07-17 (bugfix wave, Lane 2) — right-click on an automation lane opens a "Clear Automation"/"Remove Lane" context menu, dispatching the existing `ClearLaneCommand`/`RemoveLaneCommand`. |
| ~~BUG-183~~ FIXED | **fusion-coverage-baseline-slipped** | FIXED 2026-07-17 (BUGFIX_WAVE_2026_07_17_DESIGN Lane 5) — floors moved to 32/56/240 (measured 32/56/243), citing `a065dec4` (CinematicScene unbundled). |
| ~~BUG-199~~ FIXED | **audio-and-scene-setup-docks-have-no-working-scroll-input** | FIXED 2026-07-17 (BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 1) — `primary_mouse_wheel` (`window_input.rs`) routes wheel events over either dock through the generic `UIEvent::Scroll` pipeline; both panels' `handle_event` gained a `Scroll` arm. `scene-setup-add-fog-drag.json` green again; new `audio-dock-scroll.json` proves the audio dock. Clears VD-029. |
| ~~BUG-198~~ FIXED | **ui-automation-key-event-has-no-global-undo-seam** | FIXED @ 6318c9fb (BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 4) — the headless `Runner` now owns a real `UndoRedoManager`; Cmd+Z/Cmd+Shift+Z dispatch real `undo()`/`redo()`, any other modifier-bearing `Key` with no seam fails loudly instead of "ok". |
| ~~BUG-197~~ FIXED | **switch-texture-blocks-ibl-generation-gate** | FIXED 2026-07-17 (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b) — `mux_texture.rs` gains the same `last_key`/`mark_outputs_unchanged` gate as the other producers; AMG @4K unprofiled p50 13.554ms → 9.40-9.46ms. |
| ~~BUG-196~~ FIXED | **is-multiple-of-clippy-debt-gltf-import-render-scene** | FIXED (bugfix-wave-2026-07-17 Lane 3) — all `manual_is_multiple_of` sites in `gltf_import.rs` and `render_scene.rs` rewritten to `.is_multiple_of()`; `cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings` clean. |
| ~~BUG-195~~ FIXED | **scene-setup-merge-no-stored-object-radius-for-scale-sanity** | FIXED (bugfix-wave-2026-07-17 Lane 3) — `merge_import_into_graph` now prefers `max_known_source_bbox_radius` (BUG-194's stored `source_bbox_radius` provenance param) over the orbit-camera-distance proxy, which is kept only as the fallback for a scene with no known-radius mesh-source node. |
| ~~BUG-194~~ FIXED | **scene-setup-vertex-count-not-computable-from-def** | FIXED (bugfix-wave-2026-07-17 Lane 3) — `source_vertex_count`/`source_bbox_radius` declared as import-time-provenance params on `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source`, seeded by `gltf_import.rs` at import/merge time; `SceneVm::from_def` sums them (plus a closed-form table for `node.cube_mesh`/`node.grid_mesh`) into a new `header.vertex_count` / `header.vertex_count_exact` pair — unresolved contributions degrade to "≥ N", never a fabricated exact count. |
| ~~BUG-193~~ FIXED | **scene-setup-no-remove-object-command** | FIXED 2026-07-17 (bugfix wave, Lane 2) — new `RemoveSceneObjectCommand`/`RemoveSceneLightCommand` (decrement + renumber + delete, whole-level snapshot undo) wired to a per-row "✕" in the Scene Setup panel's Objects/Lights sections. |
| ~~BUG-192~~ FIXED | **ui-automation-under-text-flat-card-rows** | FIXED @ 6318c9fb (BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 4) — `under_text_matches` now climbs outward one enclosing level at a time, stopping at the nearest same-parent sibling with any text; resolves `param_card.rs`'s flat (`parent: None`) generator rows and fixes a previously-undetected cross-match risk in `layer_header.rs`'s shared-outer-scroll-clip shape. |
| BUG-191 | **perf-soak-start-seek-first-frame-spike** | `cargo xtask perf-soak <project> --start <beats>` shows a ~34-37ms content-thread frame right after the transport seeks to `--start`, tripping I1's 20ms hard-fail line on that one frame — confirmed pre-existing on unmodified `origin/main` (not a PERF_BUDGET_GATE_DESIGN P2 regression), root cause not investigated. MED (the gate can't yet soak a targeted mid-set passage via `--start` without a spurious I1 failure; `--seconds`-from-top runs are unaffected). |
| BUG-190 | **brainstem-24-skinned-objects-370ms-per-frame** | Original ~370ms/frame does NOT reproduce (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0/P5, re-measured twice). Residual: CPU-encode-wall ~20ms (GPU healthy ~4-8ms) — P4's CPU repair (format!/scan removal) only closed ~4-5% of it, so the dominant cause is still unattributed; NOT blocking A2 (never a named gate fixture; CesiumMan/Fox measure 5-7ms/frame). MED (blocks a real multi-skinned-character asset from being performable). |
| ~~BUG-189~~ FIXED | **import-graph-10ms-resolution-independent-gpu-floor** | FIXED 2026-07-17 (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0–P5, all phases) — shadow caching (P2) + IBL gating (P3/P3b) close the dirty-scene re-render waste. AMG GT3 final: @4K GPU p50 13.554ms → ~9.45ms (~30% drop), @1080p 9.830ms → ~5.73ms (~42% drop). Residual is `render_scene`'s main pass (real per-frame work, not waste) — now R4's (indexed-mesh-rendering, deferred) trigger. |
| BUG-188 | **meshprimitivemodes-non-triangle-primitive-blanks-whole-object** | `MeshPrimitiveModes.gltf`'s mesh mixes non-Triangles primitives (POINTS/LINES/etc) with one TRIANGLES primitive on the same material; the first non-Triangles primitive's error aborts the WHOLE object's geometry via `?`, so it renders fully black instead of drawing at least the TRIANGLES part. Found during G-P7 sidecar-fetch sweep. LOW-MED. |
| BUG-187 | **meshoptcubetest-khr-mesh-quantization-unsupported** | `MeshoptCubeTest.gltf` requires `KHR_mesh_quantization` (and `EXT_meshopt_compression`), neither implemented — correctly vetoed, not a misrender. Found during G-P7 sidecar-fetch sweep. LOW. |
| BUG-175 | **fluidsim2d-dead-black-after-live-resize** | FluidSim2D (and likely FluidSim3D/ParticleText — same aliased in-place particle buffer) renders permanent black after a live project-resolution change; the resize state-clear that fixed Cymatics (`b11e6511`) doesn't rescue it. Ignored gpu-proofs reproducer: `fluidsim2d_survives_live_resize`. MED-HIGH (live-rig: resolution change kills fluid layers). |
| ~~BUG-161~~ FIXED | **ui-snapshot-feature-fails-to-compile-canonical-def-arc-mismatch** | FIXED 2026-07-14 (bug-wave3 lane C) — `&view.canonical_def` / `(*view.canonical_def).clone()` at all 8 sites; unblocks BUG-160's prescribed oracle. |
| ~~BUG-163~~ FIXED | **freeze-codegen-region-fusion-gpu-tests-fail-with-badinput-standalone** | FIXED 2026-07-14 (Fable, same-day root-cause session) — `generate_fused`'s D3/P4a ExtKind loop classified inputs positionally (`idx >= tex_count` from `node_inputs`), so hand-built test regions with `node_inputs: &[]` made every External texture input parse as a spec-less array port → `BadInput`. Now keys on the explicit `InputAccess::BufferIndex` tag, matching `build_region`'s producer contract. All 161 `node_graph::freeze` tests green under `gpu-proofs` (was 6 red). |
| BUG-160 | **editor-window-unification-inspector-card-layout-regressions** | PARTIAL 2026-07-15 (Sonnet, `d85ab207`): P2 tick parity SHIPPED (fixes the reported card-HEIGHT overflow — `UIRoot::tick_inspector` wired into `present_graph_editor_window`, Author snap fork deleted); P1 PARTIAL (D1/D2 chevron-lane-reserved-in-both-contexts + shared `row_geometry()` helper, D7 width-policy widen shipped; D3 elide/chip-fit-at-every-width and the width-sweep containment test still owed — a dedicated follow-up session). Design: GRAPH_EDITOR_INSPECTOR_UNIFICATION.md **Change 4**. MED-HIGH (UI regression). |
| ~~BUG-158~~ FIXED | **mapped-param-edits-snap-back-no-two-way-binding** | FIXED — P1 (inverse reshape + dispatch reroute) 2026-07-14 `bc2f2c0b`; P2 (live wire values on node faces + driven-row treatment) 2026-07-15 `bug/158-two-way-p2`. docs/PARAM_TWO_WAY_BINDING_DESIGN.md all phases shipped. |
| BUG-157 | **editor-perf-hud-never-ticked-shows-dashes-forever** | PARTIAL 2026-07-15 (Sonnet, `d85ab207`): the shared root mechanism (editor `UIRoot` never `built`, so `update()` — which ticks both `perf_hud` and the inspector — always early-returned) is fixed for the INSPECTOR half via `UIRoot::tick_inspector`, called directly from `present_graph_editor_window`. The perf-HUD half remains open: `perf_hud.update(...)` still isn't called there, so it would still show permanent `"—"` if ever opened. Still currently unreachable (no keyboard/UI path opens the editor's own perf HUD today). LOW. |
| BUG-156 | **fluidsim3d-4k-perf-regression-suspect-bug066-fix** | FluidSim3D no longer holds smooth 60FPS at 4K — regressed, and the change under suspicion is the BUG-066 fix (`eebac94d`), which resized volume-node dispatch grids from the legacy 8³ workgroup to the codegen 4³ workgroup (8x more dispatched groups per volume kernel). Reported by Peter 2026-07-14. Not investigated. HIGH (live-rig performance). |
| BUG-155 | **camera-rotation-params-missing-smooth-360-wrap** | Camera orbit/tilt/rotation params jump at the wrap boundary instead of wrapping smoothly through 0/360 degrees, so a saw wave can't drive a clean continuous spin. Reported by Peter 2026-07-14. Root cause unknown — may share a cause with BUG-096. MED. |
| BUG-153 | **ui-snap-inspector-scene-172px-nondeterministic** | `cargo run --features ui-snapshot -- ui-snap inspector` is not run-to-run deterministic: two consecutive runs of the SAME unmodified binary differ in exactly 172 pixels, always the same bounding box (x 1258-1274, y 450-854 at 1536×1216 — a narrow vertical band, likely the inspector's scrollbar thumb or a hover/blend-state artifact). Confirmed pre-existing (reproduces identically on unmodified `origin/main`, unrelated to any diff) while byte-diffing `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1's before/after — the `timeline` and `states` scenes are NOT affected (byte-identical run-to-run). LOW (test-determinism only, no correctness impact — but silently defeats any future byte-identical regression check against this one scene). |
| ~~BUG-150~~ FIXED | **mute-chip-press-motion-teleports-hit-bounds-after-scroll** | FIXED @ `804ea043` — `tick_mute_motion`'s bounds write deleted, colour tween kept; `mute_base_y` and `ChipMotion::press_offset_y` removed. Class audit (inspector drag ghost, param_card badge/target-bar, interaction_overlay lift/ghost, drawer-height tween) found no other violation of the animations-never-move-hit-geometry rule. Solo confirmed to never have had this defect (no motion tick exists for it). |
| BUG-148 | **verification-debt-duplicate-vd-020-id** | Two unrelated `docs/VERIFICATION_DEBT.md` entries both claim ID VD-020 (PARAM_STORAGE_BOUNDARIES P2's calibration-drag gesture, and CINEMATIC_POST P5/P6's GTAO look-pass) — no merge conflict flagged it since they landed in non-overlapping regions. Fix shape: renumber one (check cross-references first), audit for other duplicates, add a uniqueness check alongside `bug_status.py`. LOW (tooling/bookkeeping only). |
| ~~BUG-146~~ FIXED | **render-scene-atom-pipelines-never-prewarmed** | FIXED (fusion-sweep worktree) — `GeneratorRegistry::prewarm_all` now sweeps every registered primitive via `prewarm_all_atom_codegen_pipelines`, compiling each atom's standalone kernel through a new dynamic mirror of `standalone_for_spec` (`codegen::standalone_for_node`). Structural, O(atom count) — 144 atoms covered. Measured: `node.cube_mesh` cold compile ~12-15ms → ~0.02-0.04ms; worst-case all-144-atoms-cold-in-one-frame ~1.0-1.1s → ~1-2ms. One residual: `node.variable_blur` (the sole atom using `wgsl_specialization`) isn't reachable generically — its specialization-token substitution values are bespoke per atom — stays a lazy first-use compile as before. |
| ~~BUG-143~~ FIXED | **macros-panel-ableton-trim-drag-outside-p7-inventory** | FIXED @ `d5ab1ae7` (UI_WIDGET_UNIFICATION P8) — folded onto `DragController<AbletonTrimDrag>`. |
| ~~BUG-141~~ FIXED | **import-graph-fused-region-linearize-depth-parse-fail** | FIXED this session (fusion-sweep worktree, lands with its commit) — same root cause and fix as BUG-135: `generate_fused`'s texture path now emits `node_includes`. |
| BUG-136 | **cinematic-post-motion-blur-no-visible-effect-despite-correct-wiring** | Peter (`SceneLadders.manifold`, glb auto-import wiring): orbiting the camera with `lens.shutter_angle=181` and `motion_blur.max_blur_px=128` shows no visible blur. Statically verified correct — not yet observed at runtime: the graph wiring is right (`camera` → `lens` → `render`, `lens.out` also feeds `motion_blur.camera`, `render.velocity` → `motion_blur.velocity`, `motion_blur` sits last before `final`, confirmed via `project.json`/`wires`), and `render_scene.rs`'s `prev_view_proj` frame-to-frame diff (the velocity source) only resets on a structural rebuild (`rebuild()`, object/light count change), not on an ordinary param edit like dragging Orbit — so camera-orbit motion should register. Root cause UNKNOWN pending runtime observation. Suspects: (a) the UI param-edit path may not be live-propagating slider drags into the running content-thread graph in real time (the codebase's known `ui-state-sync-path` bug class); (b) `node.motion_blur`'s fused-vs-standalone codegen routing may be silently mis-selecting a stale/pass-through kernel, same failure family as BUG-135's fused `wgsl_includes` gap; (c) the render loop may not be ticking continuously while scrubbing outside playback, collapsing `prev`/`current` camera state to the same value per redraw. Fix shape: reproduce live, add temporary `println!`s in `render_scene.rs`'s velocity computation and `motion_blur`'s `evaluate()`/derived-uniform recompute to confirm both are seeing nonzero values at runtime, then narrow. MED-HIGH — a shipped P3 feature with no observable effect. |
| BUG-134 | **bug-status-py-tail-boundary-hides-entries-past-the-appendix** | `bug_status.py`'s `parse()` stops entry-scanning at the first `## ` heading past `## Fixed` (the "Checked and safe" appendix) and copies the rest of the file verbatim — any `### BUG-NNN` entry appended after that point (BUG-094/095/096/097/103/126, this session) is invisible to `--check`. Concretely hid a real duplicate `BUG-097` id (one FIXED, one OPEN) and a `derive_status()` false positive (`\bFIXED\b` matches inside "found not fixed" — BUG-126). Fix shape: continue the entry scan across every appendix heading, or make an entry-outside-Open/Fixed a hard check failure; separately tighten the FIXED regex to exclude a preceding not/never. LOW (tooling only). |
| ~~BUG-123~~ FIXED | **mesh-edges-capacity-vs-active-count** | FIXED @ `1b854d45` — optional `active_count` scalar input mirroring `node.range` overrides the buffer-capacity-derived vertex count. |
| BUG-118 | **render-scene-fog-washes-out-instead-of-depth-grading** | DEFERRED — Peter 2026-07-14: "I don't want bug-118 worked on"; on hold at his call. CHARACTERIZED (VOLUMETRIC_LIGHT_DESIGN.md P1, 2026-07-13): `apply_fog` IS correctly distance-scaled; the "milk" symptom is saturation — a bounded subject's depth range is small relative to fog's `1/density` decay length, so the fog fraction barely varies across it (measured Δ 1.1–2.5 percentage points across a subject-scale depth slice at camDistance 9/30, vs 15–30% differentiation across a wide-range scene). Absorbed by the shafts design, which SHIPPED 2026-07-13 (P1–P3) — but whether it actually fixes this is UNVERIFIED (the shafts' own demos don't show a legible sculpting effect; no re-render of the original repro scene yet). | render_scene / atmosphere |
| BUG-116 | **fire-meter-display-ballistics-reads-as-low-fps** | Fire meters read as updating at low FPS despite a 60fps capture/snapshot/UI pipeline — `MeterIds::update`'s intentional peak-hold smoothing (BUG-109 P5: `PEAK_HOLD_SECONDS = 0.25`, `PEAK_DECAY_PER_SEC = 5.0`) trades "a millisecond transient stays visible" for a chunkier feel. Fix shape: tune the ballistics down, or split into an instant live bar + a separate thin peak-hold tick. Deferred by Peter 2026-07-11 — cosmetic only, the edge-detector reads the raw signal. LOW (deferred by design). |
| BUG-115 | **mux-multiblend-dynamic-arity-blocks-codegen-conversion** | `node.switch_texture` (5 presets) and `node.multi_blend` are fusion boundaries mid-chain: their dynamic port list (`num_inputs` rebuilds ports per instance; multi_blend synthesizes WGSL for N inputs at runtime) can't be expressed in the static `PrimitiveSpec` the freeze codegen reads. Half-day spike DONE 2026-07-14 (see detail below): the static-max-arity + optional-Coincident + `0u` use-flag shape works technically (already proven in production by `node.pack_rgba`) but costs a real 4x texture-sample increase for multi_blend's common 2–3-wired case and loses the editor's dynamic port-shrink UX; switch_texture is a harder, separate call (32-input vocabulary, loses its 5x→1x branch-pruning short-circuit). Peter's call owed on whether to pursue. LOW (working atoms, dispatch-cost only). |
| ~~BUG-114~~ FIXED | **draw-family-blocked-on-array-into-texture-codegen-read-path** | FIXED — `docs/FUSION_SOTA_DESIGN.md` P4a+P4b+P5. `InputAccess::BufferIndex` mechanism (P4a) + Vec3/Vec4/Color param-gate lift (P5) unblocked all six `draw_*` atoms + `blob_overlay`; `BlobTracking.json` measured 18→13 estimated dispatches (1 region, 6 members). Full writeup in the Fixed section below. |
| ~~BUG-108~~ FIXED | **effect-card-add-effect-button-floats-over-sectioned-rows** | FIXED @ `33fc99b8` — confirmed suspect (a): `ParamCardPanel::effect_body_natural_height`/`compute_height_generator` summed `param_info` linearly, blind to the D5 section-header bar every section run draws and to a folded section's rows painting nothing, so the "+ Add Effect" button (anchored at the summed `compute_height()`) landed mid-card. Fixed by walking `section_runs()` the same way `build_effect`/`build_generator`'s own draw loops do. Class-kill: `add_effect_button_does_not_overlap_sectioned_card_last_row` (an anchored bounds-overlap assertion over real painted rects, not the full P1 generic lint from `UI_LAYOUT_INVARIANT_LINTS_PROPOSAL.md`, which hasn't shipped yet). |
| BUG-107 | **text-rasterizer-draws-fallback-glyph-ids-with-base-font** | PARTIAL — layer 1 (correctness) FIXED @ `1d9dba9c`: the pinned file (`text_rasterizer.rs`) was the wrong one for the reported symptom (it's the Text-generator's content pipeline, never called from the graph canvas / inspector path the screenshots showed) — the actual bug lived in `native_text.rs`'s independent UI-chrome atlas, with the identical flatten-runs pattern. Fixed both: each CTLine run now draws with its own resolved font; `native_text.rs`'s glyph-atlas cache key also gained a font identity so a fallback run's glyph ids can't collide with Inter's. Layer 2 (prevention — extend the PUA icon atlas with ↳/chevrons, plus a check-time STATIC lint over `manifold-ui` string literals) remains OPEN; a runtime debug-assert was considered and rejected — both rasterizers draw live user text (layer/clip names) too, and panicking on any CoreText fallback would crash the rig on legitimate non-ASCII user content, not just an agent's stray literal. MED (class is unbounded until layer 2 lands). |
| ~~BUG-106~~ FIXED | **audio-mixdown-analysis-only-test-order-flaky** | FIXED @ `78e97d4a` — same root cause as BUG-090/BUG-074 (a `TestDir` temp-path collision, not order-dependent global state): `SystemTime::now()`'s nanosecond value isn't actually nanosecond-resolution on this machine, so concurrently-running tests sharing the `TestDir` prefix collided on the same directory and raced on the shared fixture file. Fixed with a per-process atomic sequence number in the path. |
| ~~BUG-105~~ FIXED | **graph-node-slider-no-right-click-reset** | FIXED @ `c41132dc` — confirmed exactly as pinned: `on_right_button_down` now splits by hit-zone like the card contract (label zone → mapping popover unchanged; track zone on a numeric ranged, non-wire-driven row → `SetGraphNodeParam`/`SetOuterParam` with `default_value`, matching the scrub's own write path). New `param_slider_track_x` hit-test helper mirrors `render.rs`'s slider geometry so the zone boundary can't drift. Same missing-intrinsic-reset class as BUG-070's remaining steppers/fader. |
| ~~BUG-104~~ FIXED | **audio-trigger-takes-over-shared-param-mod-goes-dead** | FIXED 2026-07-11 — root cause was the trigger-mux-replaces (not composes) shape on Lissajous's `mux_x`/`mux_y`, plus a graph-side StateStore trigger-latch reset gap with no path back to the user value. Five-part fix: (1) `is_trigger_latch()` + `clear_trigger_state()` release every trigger-edge-latch primitive on transport stop/project load; (2) `trigger_modulate` compose idiom documented (existing atoms, no new primitive — name unconfirmed by Peter); (3) Lissajous rewired to multiply the trigger-cycled ratio onto the continuous LFO path instead of replacing it; (4) every other trigger-driven `switch_value` in the library audited and recorded as an intentional discrete-replace in its own preset description; (5) a class-guard test + a live `PresetRuntime::errors()` warning at every generator build catch the class going forward. Snap feel on the rig and the idiom name are owed to Peter. |
| ~~BUG-102~~ FIXED | **mapping-popover-has-no-text-input-surface** | FIXED 2026-07-13 (UI_WIDGET_UNIFICATION P5c) — `MappingPopover` now embeds `TextEditModel` (P5a); `EditField::Label` and a new `EditField::Section` are both live, click/drag-select/type/commit-on-blur, wired to the already-shipped `EffectMappingSection` write path. See the detail entry below for the full landing note. |
| ~~BUG-100~~ FIXED | **gltf-fresh-import-renders-near-black-for-non-azalea-geometry** | FIXED 2026-07-11 — root cause was NOT the sun/material tuning originally suspected (verified: scaling sun position/intensity/disabling shadows changed nothing). Real cause: the `imported_azalea_renders_faithfully_to_png` test's convergence check (`fraction > 0.02`) is satisfied by the material's `ambient` floor before the async base-color texture decode lands, so it captured (and asserted on) an under-textured frame for apricot/lowe's slower-decoding textures. Fixed the harness to require 3 consecutive byte-identical frames before calling it converged; `assemble_import_graph`'s lighting rig is untouched. Both fixtures now render fully lit and textured. |
| ~~BUG-098~~ FIXED | **film-grain-drifts-and-reads-as-blocky-pixels** | FIXED (bug/wave2-lane-b-filmgrain) — re-roll via frame-count modulo instead of time-panned offset, resolution-relative cell scale, and a soft blur pass; see the Fixed-section entry for the full trail. |
| ~~BUG-097~~ FIXED | **ui-snap-render-overlay-pass-uses-wrong-traversal** | FIXED 2026-07-10 by construction (HARNESS_FIDELITY_INVARIANT §4 step 2): the harness's parallel overlay pass was DELETED along with `draw_immediate_passes`, and the overlay assembly now has one owner — `ui_frame::render_main_ui_passes` — which uses `render_sub_region` @ `Depth::OVERLAY`. Not point-fixed. Confirmed reproducible after all: `build_overlays` ALWAYS records `start` after the region root, so EVERY open overlay excluded its root (the "may be latent" caveat was wrong). Permanent proof: `overlay_fidelity_proof::bug097_...` (mod.rs) shows `render_tree_range` leaves the range byte-identical (blank) while `render_sub_region` + the seam draw it. See detail below. |
| ~~BUG-090~~ FIXED | **audio-mixdown-analysis-only-test-flakes-under-parallel-run** | FIXED @ `78e97d4a` — the named `TestDir`/temp-path-collision suspect was confirmed correct, not float summation-order non-determinism: `SystemTime::now()`'s nanosecond value isn't actually nanosecond-resolution here (~96% collision rate measured over 200k tight-loop calls), so tests sharing the `TestDir` prefix collided on the same directory under parallel execution and raced on the shared `tone.wav` fixture. Fixed with a per-process atomic sequence number in the path; 10/10 consecutive parallel runs green. |
| ~~BUG-088~~ FIXED | **pre-existing-clippy-tests-gate-dirty-since-f1-landing** | FIXED @ `78e97d4a` — the 3 `audio_mixdown.rs` lints (`cloned_ref_to_slice_refs` x2, `needless_range_loop`) rewritten with `std::slice::from_ref` / `.iter().zip().enumerate()`. `osc_timecode.rs:172`'s `doc_lazy_continuation` no longer reproduces under the current toolchain (file unchanged) — nothing to fix. Surfaced a separate, unrelated pre-existing `osc_receiver.rs` lint while isolating the gate — logged as BUG-110. |
| ~~BUG-086~~ FIXED | **recording-audio-track-under-covers-duration-on-longer-takes** | FIXED 2026-07-13 (recording-sync lane) — root cause was NOT the native encoder: `WriteAudioSamples`'s backpressure gate was instrumented with a counter 2026-07-11 and repeatedly measured 0 drops on runs that still fell short, falsifying it per the diagnosis protocol. The real cause was `recording_soak.rs`'s OWN synthetic-audio pusher: `push_realtime_audio_chunk` fed a bounded `ringbuf::HeapRb` (~5s capacity) via `push_slice`, discarded its return value, and advanced its `pushed_frames` bookkeeping by the INTENDED push amount regardless of what the ring actually accepted — so a transient overflow under unpaced/encoder-stress timing bursts was silently discarded, never retried, a harness-side loss with nothing recording it happened. Fixed by tracking the real accepted count (self-heals the backlog on the next call). Verified: 3x paced 2-min 1080p soaks post-fix measured audio_duration_s at 120.0087s/120.0102s/120.0115s (<0.01% off, both drop counters 0 throughout); two paced 1-min runs (720p/1080p) measured 60.0038s/60.0102s; the unpaced/encoder-stress 2-min run — previously the reliable repro — now measures 120.0007s. Landed together with BUG-085 (same silent-drop class rule: no path may return success on a dropped buffer). `LiveRecordingPlugin.m`'s audio backpressure gate was ALSO hardened while investigating (now bounded-spin-waits like the video path before counting a drop, and returns a real error code instead of `LR_OK` on drop) — real defect per the class rule, though it turned out not to be BUG-086's cause. |
| ~~BUG-085~~ FIXED | **recording-frames-recorded-overstates-async-append-drops** | FIXED 2026-07-13 (recording-sync lane) — `frames_recorded` no longer accumulates from `LiveRecorder_EncodeVideoFrame`'s synchronous LR_OK return (that only proves the frame was queued for async append, not that it landed). Native: a `videoFramesAppendDropped` atomic counter (+ `LiveRecorder_GetVideoFramesAppendDropped`) now counts every way the async `appendPixelBuffer:` call can fail (backpressure, writer not Writing, append returning NO, an exception). Rust: `recording_thread::run` reads that counter and calls `LiveRecorder_Finalize` (which drains the append queue before returning its count) for the ground truth, and returns a `RecordingStats{frames_recorded, frames_sync_failed, video_append_dropped}` instead of an untrustworthy synchronous tally; `LiveRecordingSession::stop()` sums every drop source (`frames_dropped` now includes async-append drops, not just pool exhaustion) so `frames_recorded + frames_dropped` always equals frames submitted. `pool_accounting_consistent`'s forced-backpressure test tightened from `pts.len() <= frames_recorded` to exact equality, plus `dropped > 0`; green 3x. |
| BUG-080 | **param-manifest-construction-not-a-unified-safe-gate** | The param manifest (an instance's live knob list) is built at deserialize AND rebuilt by a later `reconcile_param_manifests` pass, because deserialize can't see project-embedded presets yet. Consumers that read `.params` *between* the two — a direct `serde_json::from_str::<PresetInstance>`, the keep-don't-drop backstop, the legacy audio-trigger migration, ~18 tests — depend on the deserialize-time build being correct. It works today only because the double-build papers over the timing; it's a latent hazard, not SOTA: a future load path added without a reconcile silently inherits an empty/partial manifest (the BUG-036 class). Root cause: manifest construction has no single safe gate — "partially built" is an observable, readable state. Fix shape (design pass, NOT a patch): make a half-built manifest un-observable — one construction gate every load/paste/bare-read passes through, OR a type-state where params can't be read until reconciled, OR deserialize carries enough context to build complete in one shot. The naive "build once in reconcile" was tried this session and is unsafe for exactly the reasons above (design doc §2 D1 priced + rejected it; see the 2026-07-09 double-build escalation). MEDIUM (design-quality / latent-robustness). **Design WRITTEN 2026-07-14: `docs/PARAM_MANIFEST_GATE_DESIGN.md` — executes as its P1 inside bug-wave lane B; the doc is the brief, never patch this outside it.** |
| ~~BUG-079~~ FIXED | **missing-preset-fails-silently-no-onscreen-signal** | FIXED @ `834fdaa6` — an unresolved preset template now surfaces in the existing BUG-063 "opened with repairs" load-time toast instead of only an `eprintln`. |
| ~~BUG-076~~ FIXED | **inspector-scroll-underestimates-content-height** | Closed as not-reproducible, Peter's call 2026-07-14 — two investigations found no mechanism, fixture tests pass, doesn't reproduce on the rig. Reopen if a tall inspector stack ever won't scroll live. |
| ~~BUG-074~~ FIXED | **audio-mixdown-flaky-under-parallel-tests** | FIXED @ `78e97d4a` — same `TestDir` temp-path-collision root cause as BUG-090/BUG-106 (GPU-contention suspect ruled out: the mixdown render path is pure CPU decode/resample). Fixed with a per-process atomic sequence number in the `TestDir` path. |
| ~~BUG-072~~ FIXED | **audio-mixdown-all-targets-clippy-debt** | FIXED @ `78e97d4a` — same fix as BUG-088: `std::slice::from_ref` + `.iter().zip().enumerate()` rewrites in `audio_mixdown.rs`. |
| ~~BUG-046~~ FIXED | **low-band-kick-deafness-on-mixes** | resolved by the dedicated ridge-only Kick channel (KICK_SWEEP_EVENT P1/P2/P4/P5, shipped 2026-07-07) — reads the kick's FM sweep, breaks the bad_guy deafness at equal bass-false-fire cost; kick-triggering binds Kick now, not Low; Peter confirmed 2026-07-11; live feel-pass = design P3 |
| ~~BUG-101~~ FIXED | **setup-spectrogram-scroll-offset** | FIXED 2026-07-14 (bug-wave3 lane C) — `scope_rect` now shifts with the body scroll offset; `update_band_meters` fixed by the same change. |
| ~~BUG-039~~ FIXED | **saw-rotation-wrap** | FIXED 2026-07-11 — `ParamSpecDef.wraps` (explicit tag) + `constrain_to_range` helper, wired into driver evaluation and automation-lane sampling; 10 preset card params tagged periodic after auditing all 49 presets. Rendered saw sweep across the wrap boundary confirms no hitch. |
| BUG-045 | **gap-ring-down-chase** | tracker follows kernel ring-down down ~2-4 bins in note gaps; notes gate 87.6 vs 90 (LOW) |
| ~~BUG-035~~ FIXED | **authoring-hitch** | ~59ms frame every ~5s: clip-atlas f16 convert on content thread — FIXED @ `55faec0f` (moved to clip-thumb disk worker via `try_read_packed()` + `store_atlas()`), rig confirmation owed |
| BUG-037 | **glp-first-render-stall** | PARTIAL @ `dea66221`/`7fdf25d0` — `render_scene`/`gltf_texture_source` PSO compiles now prewarmed at startup; live trace shows a real ~37% frame-0 reduction but a large preset (BlossomField) still doesn't clear the 20ms bar. Remaining cost is elsewhere (scatter_on_mesh, mesh upload, shadow pass) — see full entry. (MED) |
| ~~BUG-038~~ FIXED | **ableton-log-spam** | FIXED @ `06bfd879` — throttled: warn once, then debug until reconnect, which logs once at info. |
| ~~BUG-111~~ FIXED | **fused-segment-inner-override-noop** | FIXED @ `d73b3e36` — `EffectSlot::card_prefix` translates both the `node_map` and `fused_retarget` lookups into the segment's `c{i}.`-prefixed namespace. |
| ~~BUG-015~~ FIXED | **inspector-overlap** | FIXED — stale-chrome class fixed 2026-07-08 (`738f4e94`/`4319eb8d`: incremental cache path falls back to full render on out-of-sub-region dirt); the original 2026-07-04 "sections interleaved" sighting never reproduced. Closed by Peter's call 2026-07-14 (staleness audit); reopen if it recurs. |
| ~~BUG-060~~ FIXED | **inspector-footer-overpaint** | FIXED @ `39836352` (landed `cc4eeb37`, rig-verified by Peter 2026-07-10, re-confirmed solved by Peter 2026-07-12; class-killed — clip bound per command at enqueue). History: REOPENED 2026-07-08. Opus 2nd pass: tree-geometry cause **ELIMINATED on the live cache path** (new `footer_leak_probe` test proves the inspector clips at footer_top through `traverse_flat_range`; footer's own render is correct) — the "inspector escapes into the footer" framing is wrong. Cause localized BELOW the tree, to the cache/dirty layer (tab-swap clears it = full recomposite). Artifact is **stale UI content** (UI colours / button fragments left behind), NOT clear/dark — the prior "footer goes dark, RGB 9-16" atlas dump was a HARNESS failure, not the symptom. Stale-pixel / dirty-clear bug, BUG-015 class. Needs live atlas+offscreen pixel dump. Cause still OPEN. **2026-07-10 (Fable + Peter):** Rig screenshots relocate the artifact — fragments accumulate at the scroll viewport's CLIP EDGES (bottom sliver above footer_top on both tabs, top sliver under the tab strip on Master), i.e. INSIDE the inspector panel rows, and build up per scroll step until tab-swap wipes them. Both existing probes are structurally blind there: `footer_leak_probe` checks geometry below footer_top, the P0 differential asserts rows [footer_top, footer_top+h) — the artifact rows were never asserted, so the harness "0 diff" results don't contradict the rig (stop extending the harness; observe the rig instead). Live dump tool BUILT + VALIDATED on branch `debug/bug-060-surface-dump` (worktree `bug060-dump`, e81696b4): `MANIFOLD_BUG060_DUMP=<N>` overwrites `/tmp/bug060_atlas.png` + `/tmp/bug060_offscreen.png` every N dirty-present frames (default 30) and logs sf + footer/inspector rects; readback verified against a live launch (real UI, sf=2 Retina confirmed, playhead-only atlas/offscreen delta proves the surfaces are independent). Next: Peter reproduces with the flag set, then one look at the atlas PNG splits cache-layer vs composite/present. **2026-07-10 VERDICT (live dump, Peter's audioTesting2 repro): the dirt is IN THE ATLAS — and it is not a stale copy, it is a LIVE UNCLIPPED DRAW.** Pixel measurement on the dump: the blue pill in the top sliver spans rows 170–197 physical, the pixel-exact position EdgeStretch's own ON pill would occupy if unclipped (Glitch reference: pill top = title top − 3), while the header bg + title around it are correctly scissored at the viewport line (~188). So the card-header toggle's bg fill draws WITHOUT the column clip; every scroll leaves the previous unclipped copy in territory the (clipped) self-clearing panel render can never repaint — that is the accumulation, and only `invalidate_all` (tab swap) wipes it. Bottom-edge fragments (slider fills) are the same class: once the clip is lost mid-card, later fill quads in the range draw unclipped too. The `traverse_flat_range` suspect was CLEARED by a clip-topology test (`bug060_every_card_node_renders_under_the_column_clip`, green — fresh-build clip chains are sound). **ROOT CAUSE FOUND + FIXED 2026-07-10 @ `39836352`** via a batch-flush band trace (`MANIFOLD_BUG060_TRACE=x0,y0,x1,y1`) on Peter's live repro: card-shaped rects logged as `immediate ... scissor=None` during the inspector pass. `push/pop_transform` and `push/pop_depth` cut the pending rect run via `flush_immediate_run` even mid-traversal, batching already-enqueued TREE rects under `immediate_clip` (`None`) — every card ON pill drawn before its card's **rotated chevron** (`UIStyle.transform`) lost its scissor. This is also why the 2026-07-08 trace swore all 858 draws were clipped: it observed the clip stack at `draw_node` time, upstream of the flush-time theft. Fix: context-aware `flush_pending_run` (tree clip stack while `in_tree_pass`, immediate clip otherwise); regression test `transform_boundary_keeps_tree_scissor_on_pending_batch` proven red-under-old-flush/green-now. Gates green (workspace, gpu-proofs 1248, clippy). **RIG-VERIFIED by Peter + LANDED on main @ `cc4eeb37` 2026-07-10** (dump/trace tooling landed env-gated with it). **CLASS-KILL follow-up (same day): clip bound per command at enqueue** — `RectCommand` now carries `(clip, depth)` captured at the push site (like `LineCommand`/`ImageCommand`/text `clip_bounds`/per-command depth `22c5d528` already did); batches derive in `prepare()` by run-scanning consecutive equal `(clip, depth)`; ALL flush-time scissor inference (`flush_immediate_run`/`flush_scissor_batch`/`flush_pending_run`/`in_tree_pass`) deleted, so the wrong-flush mistake is unrepresentable. Invariant recorded in `docs/DEVELOPMENT_REFERENCE.md` ("UI Renderer Invariant"). CLOSED. |
| ~~BUG-025~~ FIXED | **timeline-scissor-bleed** | FIXED (believed) — Peter attributes the one sighting to the since-fixed GPU-pressure/contention issue behind the timeline blue-flicker; never reproduced across three headless attempts. Closed by Peter's call 2026-07-14; reopen if seen on the rig. |
| BUG-026 | **popup-fade-freeze** | fix landed, running-app verification owed (MED) |
| BUG-050 | **ableton-anchor-yankback** | play-from-cursor snap-backs; anchor fix landed, rig confirmation owed via [ABL-SYNC] logs (HIGH) |
| ~~BUG-054~~ FIXED | **renderer-device-ptr-dangles** | FIXED @ `d447ec8d` — `Arc<GpuDevice>` replaces the cached raw pointer end-to-end (`GeneratorRenderer`/`VideoRenderer`/`ImageRenderer`/`MetalBackend`); `ContentThread::run()`'s repoint block and `journey_proof.rs`'s `rebind_gpu_device_pointers` workaround deleted as structurally unneeded. `rg '\*const GpuDevice' crates/` — zero code hits. |
| ~~BUG-048~~ FIXED | **arm-two-reds** | FIXED 2026-07-14 (bug-wave3 lane C) — armed = amber `STATUS_WARNING`, idle = neutral, never red vs red. |
| ~~BUG-049~~ FIXED | **child-row-right-indent** | FIXED 2026-07-14 (bug-wave3 lane C) — right-anchored x's use `right_pad = PAD`, not the indented `pad`; oracle updated. |
| ~~BUG-012~~ FIXED | **tex-rename-corrupt** | FIXED 2026-07-14 (bug-wave lane A) — fragment-form rename filtered to texture-typed ports, mirroring the sibling binding-key rename. |
| ~~BUG-018~~ FIXED | **catalog-stale** | FIXED @ `38ec595f` — regenerated; stale entry was the ApricotBloom `wireAmount` card (scene-3 morph revert leftover) |
| ~~BUG-081~~ FIXED | **audio-load-blip** | FIXED 2026-07-14 (bug-wave3 lane C) — voice built silent (`.volume(0.0)`) instead of played-then-paused. |
| ~~BUG-031~~ FIXED | **layer-menu-positional** | FIXED 2026-07-14 (bug-wave3 lane C) — Context*Layer family + `TextInputField::LayerName` now LayerId-keyed, resolved at dispatch time. |
| BUG-053 | **hdr-live-recording-structural** | PARKED — decision owed from Peter (does live HDR capture matter for the rig); fix turns out cheap (wire the existing `PqEncoder` export already uses) once he says yes (LOW today, blocks HDR capture) |
| BUG-034 | **atlas-uv-test-gap** | headless preview doesn't cover live atlas UV path (LOW) |
| BUG-014 / 030 | parked | NaN content-key hash · color-ratchet red |
| BUG-019 / 020 / 021 | deferred | group-fold gap · gen-card collapse · snap-back gap |
| ~~BUG-056~~ FIXED | **audio-mixdown-clippy-debt** | no longer reproduces — verified 2026-07-11: `cargo clippy --workspace -- -D warnings` green (9.8s warm), no `#[allow(clippy)]` in the file. Almost certainly rewritten away by the P1 offline-export mixdown refactor (`d207f94a`, 2026-07-07), the last substantive change to `audio_mixdown.rs`; not bisected to the exact commit (LOW stakes) |
| BUG-063 | **silent-load-repairs** | PARTIAL — load-repairs now surface as a non-blocking "opened with repairs" toast (P3, no longer silent); the heavier rescue path (blocking ack dialog + journal the pre-repair project.json to history/) is deferred (MED-HIGH) |
| ~~BUG-066~~ FIXED | **fluid3d-corner-drift** | FIXED 2026-07-10 @ `eebac94d`, Peter rig-confirmed 2026-07-11 — root cause: `edge_slope_3d`/`swirl_force_3d` sized dispatch for the legacy 8³ workgroup while codegen emits 4³, so forces only ever existed in the (0..64)³ octant of the 128³ volume, which projects to the TR quadrant. Class killed: all volume nodes size grids from codegen's exported `VOLUME_WORKGROUP_3D`, emission pinned by test. The same-day "root cause NOT found" correction predates the fix (15:03 vs 16:51); the entry's falsification record stands as history |
| ~~BUG-068~~ FIXED | **inspector-scene-cliphit-overlap** | FIXED 2026-07-14 (bug-wave3 lane C) — `inspector_scene`'s GLOW/PLASMA/RETURN clips shortened to clear the 600px-wide inspector column; regression test pins every clip's hit rect clear of `ui.layout.inspector().x`. |
| BUG-069 | **shipping-license-audit** | four license problems in shipped components: madmom models + ADTOF (both CC BY-NC-SA), rusty_link crate (GPL-2.0, viral, in manifold-playback), staged ffmpeg copied from the dev machine (likely GPL build); full sweep 2026-07-08, everything else clean (HIGH for commercialization, zero runtime impact) |
| ~~BUG-070~~ FIXED | **stepper-and-nonstandard-slider-reset** | ~~decay drawer slider~~ + Clip Trigger drawer sliders covered by the intrinsic-reset follow-through (@ 3a88f728). Remainder FIXED (AUDIO_SETUP_DOCK P4): Audio Setup gain `[−]value[＋]` steppers + the D7 overlay-drag value-label gain zone (not `BitmapSlider` tracks, so no `SliderReset` registration existed) now right-click-reset to unity via `PanelAction::slider_reset` replaying the existing `AudioSendGainDrag{Begin,Changed,Commit}` trio at 0.0 dB — the SAME gesture BUG-105 names as "every card/panel slider in the app." `feat/audio-dock-p4`. |

**Freeze-compiler adversarial bug hunt, 2026-07-03** — BUG-006–014 (some now Fixed) come from a
40-agent Sonnet workflow (`wf_73bb4ddf-885`; 10 finder lenses → every finding attacked by 2
independent skeptics). BUG-006–012 were **confirmed by both skeptics** with line-level evidence;
BUG-013/014 got split verdicts (judgment recorded per entry). Full verifier transcripts: the
workflow journal at
`~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/18511d71-15ae-4119-81cc-894a3f83d247/subagents/workflows/wf_73bb4ddf-885/journal.jsonl`.
System context for all of them: [FREEZE_COMPILER_MAP.md](FREEZE_COMPILER_MAP.md).

## Open

### BUG-298 (slider-fill-under-modulation-pixel-unverified) — the modulated param's slider FILL/thumb visual is inferred-correct but never confirmed by a rendered PNG — the gap VD-031 originally mislabeled "BUG-235" — found 2026-07-21, BUG-234 landing verification
**Status:** OPEN (logged 2026-07-21, verif-infra A-tail landing).
**Severity:** LOW — verification gap, not a known defect. The value-text half is now proven (BUG-234's `scripts/ui-flows/envelope-modulation.json`); the slider fill/thumb norm is driven by `reconcile_state`'s `sync_param_value` off the SAME `effective` value the text reads, so it is very likely correct — but "very likely" off a shared-value inference is not a pixel observation (the CLAUDE.md oracle rule: a green value-text assert is not a look).
**Symptom:** under a driver/envelope, the numeric value text updates across `--script` snapshots, but no test confirms the slider's rendered fill width / thumb position tracks it.
**Root cause:** none known — this is an unproven surface, not a convicted bug. It exists as a distinct id only because VD-031's original "BUG-235" pointer was reassigned to `manifold-own-kick-fixtures-systematic-adtof-timing-bias` (an unrelated audio-timing entry), leaving the slider-fill gap with no live id.
**Fix shape:** render the `envmod` flow's modulated frame to PNG and diff the slider fill region against the base frame (a real look, per BUG-234's `envelope-modulation.json` beats); close if the fill tracks, or convert to a real defect entry if it doesn't. No code change expected.

### BUG-297 (multi-session-memory-exhaustion-freezes-machine) — concurrent Claude Code sessions jetsam the machine; indistinguishable from BUG-290's GPU freeze from the user's seat
**Status:** OPEN (logged 2026-07-21). Mitigated same session: two stale rust-analyzer instances flagged for kill; no code fix landed.
**Recurrence 2026-07-21 (evening, Opus-orchestration session):** repeated HARD lockups while an Opus mid-orchestrator ran landing-review loads (full-workspace nextest + repeated `ui-snap` flow runs = Metal GPU renders) concurrently with Peter using the machine. Peter: "has been investigated and attempted to be fixed a few times" — the mitigations to date have NOT held. NEEDS A DEDICATED INVESTIGATION SESSION with the machine instrumented (memory_pressure log + gpuEve/JetsamEvent correlation DURING an orchestration wave, per this entry's and BUG-290's separation protocol) rather than another post-hoc guess. Until fixed, orchestration sessions should treat "Peter is actively using this machine" as a reason to serialize heavy gates (one workspace nextest at a time, no parallel flow sweeps).
**Severity:** HIGH — same "whole macbook locks up" symptom as BUG-290, and it MASKS BUG-290's conviction: a freeze during a test run proves nothing until the two mechanisms are separated.
**Symptom:** `JetsamEvent-2026-07-21-100109.ips` — machine froze while another agent session ran the full landing gate (workspace clippy + deny + nextest) with NO gpu-proofs involved and NO new gpuEvent. Top consumers at jetsam: FOUR rust-analyzer instances (4.56 + 4.09 + 4.04 + 3.08 GB ≈ 16 GB — one per live Claude Code session on this workspace) plus rustc + nextest process trees. Prior art: `JetsamEvent-2026-07-17-184945.ips` fired 9 min after that day's gpuEvent — both mechanisms were live that evening.
**Root cause:** each session's LSP plugin spawns a private rust-analyzer (~3–4.5 GB warm on this workspace); N sessions + one workspace-scale build/test run exceeds RAM, and macOS thrash + jetsam reads as a total lockup. Stale sessions (12–14 h old tmux panes) keep their analyzers resident.

**Instrumented-session findings (2026-07-21, Fable, morning after the 2026-07-20-evening lockups):**
- **The evening lockups left NO JetsamEvent and NO gpuEvent.** Neither convicted mechanism fingerprinted. The kernel DID enter memory pressure 20:46–20:55 (unified log: `killing_idle_process` osr_code 9 × 4, plus `knowledgeconstructiond` per-process kill) — but desktop macOS does not jetsam regular apps; it **thrashes** (compressor + swap saturation), which presents as a total hard lockup with no diagnostic report. That is the working mechanism for the "indistinguishable from BUG-290" freezes. GPU saturation from concurrent `ui-snap` Metal renders stalling WindowServer remains a possible co-mechanism (also reportless when no fault fires).
- **NEW convicted contributor: Spotlight.** `mds_stores_2026-07-21-000154.diag`: mds_stores dirtied **8.6 GB of file-backed writes over 20:57→00:00** — exactly the orchestration-gate window. Spotlight indexing is fully enabled with zero exclusions, and the repo has SIX worktrees each with its own `target/`; every gate build makes Spotlight re-index gigabytes of artifacts (CPU + IO + page-cache pressure on top of the build itself). `rustc` itself held a CPU-resource violation 20:23–21:31.
- The 10:01 JetsamEvent (morning) was `knowledgeconstructiond` hitting its own per-process limit with ~4 GB still free — pressure, not exhaustion; the four rust-analyzers (4.6/4.1/4.0/3.1 GB) are confirmed in its process table.
- `rust-analyzer-2026-07-21-102454.ips` is a codesigning **Launch Constraint Violation** SIGKILL (binary launched from `/private/tmp/*/rust-analyzer`) — unrelated to memory; note 0 analyzers were alive at 11:45 with 8 claude sessions open, so analyzers respawn on demand only.
- **Instrumentation now live:** `~/Library/Logs/manifold-bug297/sampler.sh` logs pressure level / swap / free% / top-12 RSS every 10 s to `pressure-YYYY-MM-DD.log` (7-day retention). Running detached (nohup); LaunchAgent install was permission-blocked, so it does NOT survive reboot — restart with `nohup /bin/zsh ~/Library/Logs/manifold-bug297/sampler.sh >/dev/null 2>&1 &` or install a LaunchAgent manually. On the next freeze, read the tail of that day's log for the second-by-second record, THEN check DiagnosticReports.
**Fix shape:** (a) protocol — only the landing session runs `--workspace` gates (already the rule; today's run by a second agent violated it); (b) hygiene — kill rust-analyzer in idle/stale sessions (respawns on demand); (c) structural — investigate capping rust-analyzer memory (`lru.capacity` / `RA_LRU_CAP` env, cache priming off) or sharing/disabling analyzers in worker sessions; (d) **exclude build dirs from Spotlight** — root option: `build.target-dir = "target.noindex"` in the workspace `.cargo/config.toml` (`.noindex` suffix is the reliable modern exclusion; needs a repo-wide audit for hardcoded `target/` paths first — Peter decision), quick option: Peter adds both repo roots to System Settings → Spotlight → Search Privacy; (e) when a freeze occurs, read `~/Library/Logs/manifold-bug297/pressure-<date>.log` first, then `/Library/Logs/DiagnosticReports` for gpuEvent vs JetsamEvent — thrash leaves NO report, so absence of both + pressure in the sampler log = this bug, not BUG-290.

### BUG-290 (gpu-proofs-test-binary-faults-gpu-firmware) — a `manifold_renderer` test binary triggered an AGX GPU fault/restart; whole-display freezes on Peter's machine correlate with GPU test runs
**Status:** OPEN, hardening LANDED 2026-07-21 @ `7cacb965` (cross-process flock in `test_device()` kills the concurrent-process trigger class; `sg_blur_dynamic` radius clamped at 2048 kills the wired-garbage watchdog class). Remaining to close: the conviction run — ONE isolated `cargo test -p manifold-renderer --features gpu-proofs smoke -- --test-threads=1` with Peter present (freezes-alone ⇒ single bad kernel still live; clean ⇒ concurrency was the trigger, downgrade + monitor).
**Severity:** HIGH — stage risk by class: a compute dispatch that can fault GPU firmware in a test could do the same from the live app mid-show. Also the likely cause of Peter's "whole screen freezes, machine still alive" reports.
**Symptom:** whole machine (all displays, WindowServer, ghostty/tmux) freezes during GPU test runs, recovers after seconds. Two distinct firmware fault classes on record (both bug_type 284):
- `gpuEvent-manifold_rendere-2026-07-17-184055.ips` — restart_reason 3 **"BIF0 page fault"**, `is_read=true`, address 4398052055936, guilty_dm 3: a kernel READ an unmapped GPU address (OOB read or GPU-side use-after-free). Directly attributed to a `manifold_renderer` test binary.
- `gpuEvent-<unknown>-2026-07-18-133620.ips` — restart_reason 2 **"progress timeout"**, command_buffer_trace_id -1: a dispatch outran the firmware watchdog (runaway/unbounded loop). Process unattributed.
**Root cause:** unknown; two independent classes to convict. Facts established 2026-07-21: `GPU_TEST_LOCK` (`manifold-renderer/src/lib.rs:87` `test_device()`) is a `static` in-process ReentrantMutex — **zero cross-process serialization**, so nextest (process-per-test) or two concurrent agent sessions running GPU work defeat it entirely; concurrent-dispatch pressure is a live trigger hypothesis (Peter observed another agent running nextest around freeze times). Readback paths use `commit_and_wait_completed` (no test callers of `commit_and_wait_scheduled`), so simple drop-before-complete on the readback path is NOT confirmed. Static audit (Codex gpt-5.4-mini pass + Fable verification, 2026-07-21): `wgsl_compute.rs` `evaluate()` (~:2324) dispatches user-supplied WGSL with no host-side bounds guard — the widest OOB surface by class; `separable_gaussian_body.wgsl:94` `sg_blur_dynamic` derives its loop bound from `i32(radius)` **unclamped** (linear branch clamps to 32 at :133) — verified real, but current gpu-proofs exercise only the clamped linear mode; Plasma's `octaves = 3u + u32(cx*2.0)` loop was flagged but is 3–5 iterations for in-range complexity (saturating cast) — weak. No specific kernel convicted yet. Prior art: `agx-0x78-crash` memory, `ui-present-content-gpu-contention`.
**Fix shape:** (1) enforce cross-process GPU test exclusion — OS-level file lock inside `test_device()` alongside `GPU_TEST_LOCK`, and/or hook-deny `nextest` with `gpu-proofs` — kills the concurrency trigger class regardless of which kernel faulted; (2) clamp `sg_blur_dynamic`'s radius like the linear branch; (3) convict with ONE isolated run (`cargo test -p manifold-renderer --features gpu-proofs smoke -- --test-threads=1`, nothing else touching the GPU): freezes-alone ⇒ single bad kernel, clean-alone ⇒ concurrency trigger. Requires Peter present — a successful repro freezes the machine. Until fixed: never run gpu-proofs while performing/recording or while another session may touch the GPU.

### BUG-283 (manifold-app-clippy-tests-target-drift) — `cargo clippy -p manifold-app --tests -- -D warnings` fails on three pre-existing files; the standard gates never compile the test target so the drift is invisible
**Status:** OPEN (logged 2026-07-21, surfaced by the widget-tree P4 lane while gating its own test-only diff).
**Severity:** LOW — no runtime impact; a gate-hygiene gap. The failures: `tests/tree_render_call_sites.rs` (doc_lazy_continuation), `src/app_lifecycle.rs` (cloned_ref_to_slice_refs), `src/text_input.rs` (approx_constant). None touched by recent widget-tree work (verified via git log — last modified by unrelated prior commits); likely clippy/toolchain version drift.
**Root cause:** the house clippy gates (`-p <crate> -- -D warnings` at commit; `--workspace -- -D warnings` at landing) don't enable `cfg(test)` compilation, so lints that only fire in test targets accumulate silently until someone runs `--tests`.
**Fix shape:** one cleanup pass over the three files, then decide whether the landing gate should add `--tests` (cheap on a warm checkout) — a gate-policy call for Peter/orchestrator, not a lane.

### BUG-282 (graph-canvas-unbound-param-scrub-floods-undo-stack) — dragging an unbound graph-node-face param scrub records one undo entry PER POINTER-MOVE TICK instead of batching to one per gesture
**Status:** OPEN (logged 2026-07-20, W2-C drag-surfaces survey).
**Severity:** MEDIUM — not data corruption, but breaks the undo contract every other drag family in the codebase honors: a single scrub gesture can push dozens of entries into the 200-entry-capped undo stack, evicting older history and making Undo require many presses to unwind one visual drag.
**Symptom:** hold-drag a numeric or vector param row directly on a graph-canvas node face (not the sidebar, not a card-bound row) — each pixel of pointer movement becomes its own undo entry.
**Root cause:** `CanvasDrag::ParamScrub`/`VecScrub`'s `on_pointer_move` handler (`crates/manifold-ui/src/graph_canvas/interaction.rs:504-539`, `:541-569`) pushes `GraphEditCommand::SetGraphNodeParam` on every call. `app_render.rs`'s unbound-row arm (`:2972-2980`) turns each one into a fresh `Execute(SetGraphNodeParamCommand::new(...))` — a full undo-worthy command — with no batching. Confirmed by the release handler's own comment (`graph_canvas/interaction.rs:1315-1323`): "the scrub emitted its value on each pointer move; nothing to finalize for an ordinary row" — `EndGraphNodeParamScrub` only closes out the BOUND-row case (`BoundNodeParamDrag`), so unbound rows have no drag-end coalescing step at all. Contrast every other scrub family surveyed in `docs/DRAG_SURFACES_SURVEY.md`: inspector sliders, card params, mapping range/affine (BUG-262), and the graph-canvas's own `NodeMove`/`WireFrom` all commit exactly once, on release.
**Fix shape:** either (a) stop executing `SetGraphNodeParamCommand` per-tick — mirror the bound-row pattern: live-write via `ContentCommand::MutateProjectLive` during the drag, then commit ONE undo-worthy command on release (would need a `pending_actions`-side "start of scrub" marker to capture the pre-drag value, same shape as `BoundNodeParamDrag`); or (b) give `SetGraphNodeParamCommand`/the undo manager a same-target-consecutive coalescing rule. (a) is closer to every other family's pattern and doesn't require new undo-manager infrastructure.

### BUG-281 (graph-canvas-bound-param-scrub-unguarded-mid-gesture) — a card-bound graph-node-face param scrub live-writes `local_project` every tick with no snapshot-stomp guard
**Status:** OPEN (logged 2026-07-20, W2-C drag-surfaces survey).
**Severity:** MEDIUM — same user-visible signature as the fixed BUG-246/BUG-262 cluster-C families: a mid-gesture content-thread snapshot reverts the in-flight value; the eventual `EndGraphNodeParamScrub` commit can then see old == new and record no undo entry.
**Symptom:** drag a graph-node-face param row that's bound to a card param while any concurrent edit bumps `data_version` (MIDI/OSC, another gesture, modulation, etc.) — the dragged value can visibly snap back mid-gesture.
**Root cause:** the bound-row branch of `SetGraphNodeParam` dispatch (`app_render.rs:2906-2971`, `PARAM_TWO_WAY_BINDING_DESIGN.md` D1) opens a `BoundNodeParamDrag` session and live-writes `self.local_project` on every tick (`:2955-2957`), same shape as every `ActiveInspectorDrag` family. But the snapshot-acceptance restore block that exists specifically to survive this (`app_render.rs:823-827` for full snapshots, `:878-882` for modulation snapshots) only ever calls `self.active_inspector_drag.apply(...)` — `bound_node_param_drag` was never added as a case there, and the graph canvas's own `CanvasDrag` state isn't wired into the `drag_active` suppression flag (`self.overlay.drag_mode()`) the way the timeline overlay's gestures are. Same root as BUG-262 before its fix, on a field added after that fix landed.
**Fix shape:** add a restore arm for `self.bound_node_param_drag` alongside `self.active_inspector_drag` in both snapshot-acceptance sites (`app_render.rs:823-827`, `:878-882`) — mechanical, mirrors the existing `ActiveInspectorDrag::apply` call site exactly. Alternative: fold `BoundNodeParamDrag` into the `ActiveInspectorDrag` enum as a new variant, consistent with how `MappingRange`/`MappingAffine` were added for BUG-262.

### BUG-280 (marker-drag-unguarded-mid-gesture-race) — dragging a timeline marker has no snapshot-suppression coverage; a mid-gesture content-thread snapshot reverts the live position
**Status:** OPEN (logged 2026-07-20, W2-C drag-surfaces survey).
**Severity:** MEDIUM — same class as BUG-246/BUG-262/BUG-281: visible snap-back mid-drag, and the eventual commit can see old == new (no undo entry) if the snapshot lands on the last tick before release.
**Symptom:** drag a marker on the timeline ruler while any concurrent edit bumps `data_version` — the marker can jump back to its pre-drag beat mid-gesture.
**Root cause:** `PanelAction::MarkerDragMoved` (`crates/manifold-app/src/ui_bridge/marker.rs:49-56`) writes `marker.beat` directly into `project.timeline` every frame the pointer moves, exactly the live-write pattern every `ActiveInspectorDrag` family uses. But marker dragging is driven by `ViewportDrag::MarkerDrag` (`panels/viewport/interaction.rs:126-176`), a state machine entirely local to `TimelineViewportPanel` — it is NOT part of `InteractionOverlay`'s `DragMode`. `app_render.rs`'s snapshot-suppression flag (`drag_active = self.overlay.drag_mode() != DragMode::None`, `app_render.rs:778-779`) therefore never sees a marker drag in progress, and there is no `ActiveInspectorDrag::Marker` variant to restore the value after a snapshot swap the way `Trim`/`AudioGain`/etc. do.
**Fix shape:** either (a) add a `Marker { marker_id, beat }` variant to `ActiveInspectorDrag` and set/clear it around the `MarkerDragStarted`/`MarkerDragEnded` dispatch in `ui_bridge/marker.rs`, mirroring the existing ten cluster-C families; or (b) fold marker-drag state into `self.overlay.drag_mode()`'s scope so it participates in the blanket `drag_active` suppression the timeline overlay's own gestures get (bigger refactor — `ViewportDrag` and `InteractionOverlay`'s `TimelineDrag` are currently separate controllers). (a) is the smaller, more consistent-with-precedent fix.

### BUG-248 (gui-fps-degradation-persists-after-deleting-heavy-static-glb-layers) — GUI FPS degradation reported after importing/deleting heavy STATIC (non-animated) glb layers; headless import render is clean — found 2026-07-18 during GLTF_ANIM_RUNTIME_V2_DESIGN.md P4 acceptance

**Status:** OPEN — symptom reported by Peter with `apricot_blossom_cluster_lod.glb`. Root cause unknown.

**Symptom:** deleting a heavy static (non-animated) glb layer in the live app leaves GUI FPS degraded — the degradation persists past the delete, not just present while the layer is loaded.

**Root cause:** unknown; headless import render of the same asset is clean (`render-import`, `/usr/bin/time -l`: 399,851,520 bytes / 0.40 GB peak RSS, converges frame 4, matches its own 430 MB baseline, no regression from anim-v2 work) — so the degradation is not visible in a headless import+converge measurement and needs an in-app profile to attribute.

**Suspects:** texture/buffer pool retention past layer delete (no eviction, or eviction keyed wrong); project-resolution render cost unrelated to the deleted layer; UI-thread-side retained state (undo snapshot, param card cache) not released on delete.

**Fix shape:** needs an in-app repro + profile (Instruments or the app's own `--profile` per-node breakdown) to attribute before any fix is proposed — not attempted this session (out of scope for a measurement-only P4).

### BUG-247 (gltf-import-peak-rss-residual-per-source-node-whole-file-parses-not-deduplicated) — glTF import peak RSS residual: mesh/texture source nodes independently re-parse the whole file — found 2026-07-18 during GLTF_ANIM_RUNTIME_V2_DESIGN.md P4 acceptance measurement

**Status:** OPEN — MED. Not blocking GLTF_ANIM_RUNTIME_V2_DESIGN.md's landing (the class that design targets — payload-in-def, per-object table duplication, delete-doesn't-free — is fixed; this is a different, smaller, previously-deferred class).

**Symptom:** the dragon fixture (`drogon__game_of_thrones_dragon/scene.gltf`, ~121 MB combined JSON+bin) measures 1.42–1.46 GB peak RSS post-anim-v2 (`/usr/bin/time -l` "maximum resident set size", 4 runs at different `--time` values) where the shared `GltfAnimCache` alone (D2, one `Arc<GltfAnimSet>` per file) would predict roughly 0.6–0.7 GB (baseline-floor ~0.4 GB from the blossom control + a modest anim-payload share).

**Root cause (suspected, unproven — not profiled this session):** each of `gltf_mesh_source`/`gltf_skinned_mesh_source`/`gltf_texture_source` background-parses the FULL file independently and concurrently on its own thread (the same `pending_load`/background-thread pattern D2 uses for the anim cache, but without D2's shared-cache dedup) — for the dragon's 2 objects × however many source nodes wire to the same file, that's N independent whole-file parses of a 121 MB document instead of one shared parse. GLTF_ANIM_RUNTIME_V2_DESIGN.md's own Deferred section named this class explicitly ("Mesh/texture payload dedup across objects — mesh sources are correctly gated; trigger: a measured mesh-side memory problem") and this measurement is that trigger firing.

**Fix shape:** extend the `GltfAnimCache` sharing pattern (`gltf_anim_cache.rs`, `Weak`-held `HashMap<PathBuf, Weak<T>>`, background-loaded) to a per-file parsed-document cache that mesh/texture source nodes share instead of each re-parsing independently. Not attempted this session — needs its own design/profiling pass to confirm the suspected mechanism before implementing (the number above is inference from file-size arithmetic, not a measured allocation breakdown).

### BUG-243 (analyzer-false-fires-on-sustained-pads) — sustained pad/swell material fires transient + kick events with zero real hits — found 2026-07-18, same session

**Status:** PARTIAL — 2026-07-18 (harness-gated tuning session, `crates/manifold-audio/src/analysis.rs`). Part A (transient median-criterion fires) FIXED: `SUPERFLUX_DELTA` 48.0 → 80.0 collapses the pad's transient fires from 6 (full) + 29 (low) to 3 + 3 (matching mid/high's pre-existing 3, which are novelty-criterion swell attacks, out of this knob's scope) — verified against {80, 100, 125, 150}, 80 is the minimal value tested and 100/125/150 measured identical. Part B (kick-ridge fires, 30 on the pad fixture) NOT FIXED — see Root cause/Fix shape below for why neither documented knob (`KICK_ABS_FLOOR`, `KICK_MIN_PEAK`) has a safe value. Net: pad total causal events 71 → 42 (target was ≤10; not met). All gates green at the new `SUPERFLUX_DELTA`: `edm_kit_128bpm` recall 0.714/precision 1.000 (≥0.70/≥0.99 target, unchanged), `kick_hat_128bpm` 0.785/1.000/0.646 (exact match to pre-change), `arp_16th_128bpm` 252 events (unchanged), `mod_harness --selftest` P3 false-fire guards held or improved (dive low 2→0, riser low 2→1; `kicks`/`busymix`/`densemix` low-band fire counts unchanged at 8/7/8, 7/8/7, 2/7/8), `cargo clippy -p manifold-audio -- -D warnings` clean, `cargo test -p manifold-audio --lib` 57 passed.

**Symptom:** 41 analyzer transient fires + 28–30 kick-ridge fires on a 20 s pad-only fixture (measured 30 kick-ridge fires this session, consistent with the originally reported 28 within fixture/scoring noise).

**Root cause (attributed per-event, `eval/odf_attribution.py` for A; `MANIFOLD_KICK_DEBUG` for B):** A — 26/41 transient fires were quiet-passage admissions, pad flux ~80 ODF units clearing the old `SUPERFLUX_DELTA` (48) while the median baseline is ~0; the rest are big swell attacks passing the novelty test (candidate ~1000+ vs ref ~0), unaffected by this fix. B — kick-ridge fires on the pad are coherent 6-hop descents (drop 10–16 bins, matching `KICK_DROP_BINS`) whose apex peaks (~86–109 raw tilted-column units) sit in the SAME magnitude range as `edm_kit`'s real kick-ridge apexes (~62–104, observed via the same debug tap on real material). Neither the absolute floor (`KICK_ABS_FLOOR`) nor the relative floor (`KICK_MIN_PEAK`) can separate them: sweeping `KICK_ABS_FLOOR` at 40/60/65/70/75/80 killed `edm_kit`'s `kick_low` (15→0) and the `kicks`/`busymix`/`densemix` selftest guards (8/7/8→0/0/0) at the same step that first touched the pad (40, the lowest tested); sweeping `KICK_MIN_PEAK` at 0.15/0.2/0.25/0.3 had ZERO effect on the pad's 30 fires even at 2.5x default, while 0.25+ started costing `densemix`'s guard.

**Fix shape:** Part A done via `SUPERFLUX_DELTA` (see Status). Part B needs a discriminator other than peak magnitude — the two constants swept this session are provably a dead end (see Root cause). A sustained-energy veto (the pad's ridge sits atop continuously-elevated Low-band energy, unlike a kick's transient rise from a quiet floor) or a birth-context gate (require the ridge's onset hop to itself be near a Low-band amplitude rise) are the remaining candidate shapes — neither implemented or swept this session; both are logic changes, out of this session's harness-gated-tuning-only scope.

### BUG-246 (unguarded-inspector-gestures-race-full-snapshot-acceptance) — audio-gain / mod-config / trim-style drags aren't in `ActiveInspectorDrag`, so a full snapshot accepted mid-drag stomps the in-flight value — found 2026-07-18 while fixing the macro/scene undo regression (`30712691`)

**Status:** PARTIAL — trim family FIXED 2026-07-18 (`ActiveInspectorDrag::Trim` covers all three trim kinds — driver / audio-mod / Ableton; regression test `driver_trim_range_survives_a_mid_gesture_snapshot`). This was the visible bug Peter reported: with playback running, the modulation trim handles vanished mid-drag and returned on release — the per-frame `sync_inspector_data` reconfigure (`app_render.rs` ~3373, UNGUARDED by `is_dragging`) reads the reverted `local_project` each frame, so `handle_drag` re-read the stale non-dragged edge and repositioned the bar to a bad spot. Restoring the in-flight range at the root removes it. STILL OPEN — MED: the non-trim families (audio-gain / mod-shape / step-amount / crossover) have no guard variant yet; same class, same fix shape.

**Symptom:** while dragging an audio-gain / mod-shape / step-amount / crossover / trim slider, a full project snapshot acceptance (`app_render.rs` ~line 808) replaces `local_project`; the restore at ~817 only covers `ActiveInspectorDrag` variants (MasterOpacity, LedBrightness, LayerOpacity, Param, Macro, SceneParam). Unguarded drags visibly snap back mid-gesture, and the commit handler's re-read of `new_val` from `local_project` produces a wrong or skipped undo command (same mechanism the macro fix's red test proves).

**Root cause:** the commit handlers derive `new_val` by re-reading a project that concurrent snapshot acceptance can rewrite; only guarded gesture kinds are restored.

**Fix shape:** either add guard variants for the remaining gesture families (mechanical, follows the Macro/SceneParam precedent in `30712691`), or the class-level version: commit handlers carry the gesture's final value instead of re-reading `local_project`. The two new dispatch tests in `inspector.rs` (`macro_drag_survives_a_mid_gesture_modulation_snapshot`, `scene_row_drag_survives_a_full_snapshot_replacement`) are the template for proving each family.

### BUG-244 (graph-canvas-apply-live-values-skips-non-numeric-param-kinds) — the editor canvas's per-frame live-value overlay only updates scalar params; enum/color/vec/table on-face values stay frozen until a `graph_version` bump — found 2026-07-18, param-desync campaign lane B (K3 readers lane)

**Status:** OPEN — LOW-MED (the value plane for non-numeric kinds silently falls back to structural-sync cadence; a driver or command writing an enum/color/vec param shows stale on the node face while the render is already using the new value).

**Symptom:** with the graph editor watching a graph, `GraphCanvas::apply_live_values` (manifold-ui `graph_canvas/model.rs`) applies `LiveNodeParams` — `Vec<(NodeId, Vec<(&'static str, f32)>)>` — to node-face rows, so only params representable as one `f32` update per frame. Enum/color/vec/table rows keep their built value until something bumps `graph_version` and forces a rebuild.

**Root cause:** the live tap (`preview_encoding.rs`'s `LiveNodeParams`) is scalar-only by type; non-scalar kinds were out of scope when the feed was built.

**Fix shape:** widen the live tap's value type to a small enum (scalar / vecN / color / enum-index) or add a parallel non-scalar feed, then match in `apply_live_values`; table stays rebuild-only. Scope it with the FREEZE_COMPILER_MAP / graph runtime owner — the tap is renderer-side.

### BUG-245 (mapping-popover-trim-fields-dont-track-external-edits-after-open) — an open mapping popover's trim min/max fields keep their seeded values when the mapping is edited from elsewhere; reopening reseeds correctly — found 2026-07-18, param-desync campaign lane B (K3 readers lane)

**Status:** OPEN — LOW (narrow: needs the popover open while another surface — other window, OSC, command — edits the same mapping's trim; closing and reopening the popover shows the right values).

**Symptom:** the mapping popover seeds its trim min/max text fields once at open; there is no per-frame or on-change resync of those fields against the project's mapping state, so external edits made while it is open are invisible until reopen.

**Root cause:** popover fields are seeded-at-open UI state with no membership in the per-frame value-sync plane (`sync_card_values` family).

**Fix shape:** include the popover's non-focused fields in the per-frame value sync (skip the actively-edited field, same drag-guard convention `sync_card_values` documents), or resync on a mapping-version bump.

### BUG-239 (headless-script-harness-shows-stale-value-after-nontrivial-dispatch) — a `--script` flow's PNG/tree-dump keeps showing a param's PRE-write value after a real, correctly-dispatched write — found 2026-07-17 during SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a

**Status:** OPEN — MED (headless-verification-only; does not affect the live app or the underlying data). Found, isolated, NOT fixed this session (out of C-P1a's scope — the design's own dispatch-correctness gates are satisfied by Rust-level tests instead). **Recurred 2026-07-18 (C-P1d):** the SAME shape hit `scene-setup-modifier-stack.json` (Modifier's Angle drag) and the new `scene-card-convergence-c-p1d-light-scrub-drawer.json`/`-roughness-scrub-drawer-undo.json` flows — dispatch logs confirm correct `ParamSnapshot`→N×`ParamChanged`→`ParamCommit` sequences in all three, but the displayed value-text stays at the pre-drag value in the same run's snapshot. Also observed: the harness's undo-key handler reports `undo_count=0` before and after every `Key{Z, command:true}` press in these same runs even though a real `ParamCommit`/`DriverToggle` had just dispatched — a related, possibly-identical, gap in the SAME harness (undo state also unobservable headlessly, not just display state) that wasn't previously called out explicitly; folded into this entry rather than opening a near-duplicate.

**Symptom:** `scripts/ui-flows/scene-setup-fog-density-card-row.json` (new this session) drags the converted Fog/Density card row's slider through the REAL dispatch path — the harness log confirms `ParamSnapshot` → 7× `ParamChanged` (0.5 → 1.0) → `ParamCommit` all fire with correct addressing and values — but the row's rendered value-text/slider fill in the SAME run's PNG/tree-dump stays at the PRE-drag value ("0.00"), even after two subsequent forced-structural rebuilds (re-clicking the outliner's World row, `SceneSetupSelectionChanged`, `structural=true`).

**Root cause:** unknown precisely — isolated to the `--script` harness (`crates/manifold-app/src/ui_snapshot/script.rs`), NOT the dispatch code: an equivalent sequence (`SceneSetupAddFog` via `dispatch_project`, then `ParamSnapshot`/`ParamChanged`×3/`ParamCommit` via `dispatch_inspector`, same `&mut Project`) reproduced directly in a Rust unit test (`ui_bridge::inspector::scene_card_convergence_tests::fog_density_write_persists_after_add_fog_then_drag_session`) PASSES — the write lands, value reads back correctly. Suspects: (a) the harness's documented `MutateProjectLive` channel is "held and never drained" — fine for the local-write half, but if any DISPLAY path incorrectly still expects that channel to apply the value, it would explain a permanently-stale read; (b) the same masking-bug FAMILY already logged against this harness for BUG-199's landing (`docs/BUG_BACKLOG.md`'s own note there: `needs_structural_sync` "stuck true" after first use, and `skip_to_settled`'s free first-frame rebuild) — this may be a third instance of the harness's invalidation-signal plumbing not fully covering every panel/dispatch combination.

**Fix shape:** instrument `Runner::drain_and_dispatch`/`apply_ui_frame_invalidations` (script.rs) to trace which `Project` reference a dispatched `PanelAction` mutates vs. which one the next `Snapshot`'s VM resync reads from — the two are suspected to diverge for this dispatch shape. Not attempted this session; C-P1a's own gates (undo-granularity, imported-def write, id-map totality) are proven at the Rust dispatch level instead, and the flow script itself is green (13/13 steps — every click/drag/assert resolves correctly; only the FINAL displayed value in the PNG is stale).

### BUG-233 (gizmo-move-scale-x-axis-color-collides-with-viewport-grid-x-axis) — the P6 move/scale gizmo's red X-axis handle and the P5 grid overlay's red X-axis line use near-identical colors, so with both visible the gizmo's X handle is hard to pick out — found 2026-07-17, `lane/realtime3d-viewport` P6 session, while eyeballing the gizmo demo PNGs

**Status:** OPEN — LOW (cosmetic; functionally the axes are still distinguishable by direction/geometry and the color IS correct per D8's lock-state contract — a unit test asserts the exact RGBA value — this is purely a legibility issue when both overlays render together).

**Symptom:** `viewport_overlay::grid_lines`'s X-axis tint (`GRID_AXIS_X_COLOR = [180, 70, 70, 255]`) and `viewport_gizmo`'s X-axis handle color (`X_COLOR = [225, 70, 70, 255]`) are close enough in hue/saturation that in a rendered frame with the grid on AND an object's Move/Scale gizmo showing, the gizmo's red X handle blends into the grid's red X-axis line running through the same general screen region. The P6 demo PNGs (`viewport_p6_demo.rs`) had to disable the grid overlay (`ViewportOverlayConfig { grid: false, .. }`) to get a legible gizmo screenshot — the production app renders both together with no such override.

**Root cause:** the two overlay systems (P5 `viewport_overlay` and P6 `viewport_gizmo`) picked their axis-tint colors independently, both converging on "reddish" for X (a reasonable per-module choice — X-as-red is the universal DCC convention) without checking for a value collision against the other overlay that's usually on at the same time.

**Fix shape:** either (a) desaturate/darken the grid's axis tint further so it reads as "chrome" rather than competing with gizmo saturation, or (b) shift the gizmo's axis palette a few degrees off pure DCC red/green/blue (e.g. a warmer/brighter red distinct from the grid's), or (c) auto-dim the grid opacity while a gizmo is showing (selection implies "I'm working on this object, not reading the grid"). (b) is probably the smallest, most contained fix — touches only `viewport_gizmo.rs`'s three color constants, no cross-module coupling. One short session; needs a quick visual check (headless PNG, eyeballed) rather than a new automated gate, since this is a legibility judgment call.

### BUG-235 (manifold-own-kick-fixtures-systematic-adtof-timing-bias) — raw ADTOF kick output is systematically early vs. the 5 manifold_own hand-labeled kick fixtures — found 2026-07-17, AUDIO_ANALYSIS_ACCURACY P3, orchestrator diagnostic

**Status:** OPEN — LOW-MED (measurement-correctness finding, not a functional regression; the ADTOF pipeline itself isn't broken — babyslakh's per-class scoring on the same detector gets kick F1 0.818 — but this fixture pack's own kick F1 numbers understate accuracy at the current 50ms tolerance).

**Symptom:** the P3 full-pack baseline scored raw ADTOF kick detections against the 5 `manifold_own` electronic kick fixtures at F1 0.067–0.75 (mean 0.238), anomalously low next to babyslakh's per-class ADTOF kick F1 of 0.818 on the same detector. The orchestrator flagged the right-count/zero-overlap signature (apricots: 1 TP of 14 pred/16 truth; feel_the_vibration: 0 TP of 15/16) as a possible label time-base mismatch — the same hazard class this session's own Harmonix finding (BUG-232) proved is live in this corpus.

**Diagnostic performed (median signed offset, predicted kick − nearest truth, against BOTH label columns):**

| Track | vs `mix_time_s` | vs `drums_time_s` | Columns identical? |
|---|---|---|---|
| apricots_128bpm | −125ms | −125ms | yes |
| bad_guy_128bpm | −20ms | +75ms | **no** (its own known tempo-warp caveat) |
| feel_the_vibration_174bpm | −100ms | −100ms | yes |
| inhale_exhale_145bpm | −62.5ms | −62.5ms | yes |
| tears_140bpm | −77.5ms | −77.5ms | yes |

Worst-case detail dump (`feel_the_vibration_174bpm`, all 15 predictions, ADTOF raw kicks vs `mix_time_s` truth): every single prediction is early, tightly clustered −80ms to −120ms with a slight drift across the track (−80, −80, −100, −100, −95, −80, −100, −95, −115, −105, −100, −110, −120, −100, −120) — not scattered/random misses, a clean systematic bias.

**Root cause:** confirmed this is NOT a scorer column-selection bug. Per `tests/fixtures/audio_labels/README.md`, "the other four tracks' stems and mix share a time base exactly" — `mix_time_s` and `drums_time_s` are byte-identical for apricots/feel_the_vibration/inhale_exhale/tears, so no column swap is possible or would change anything for them. `bad_guy_128bpm` is the ONLY track with genuinely different columns (its own documented tempo-warp caveat), and the scorer's existing choice (`mix_time_s`, scored against `mix.wav` predictions) already carries the smaller offset (−20ms) vs. the alternative (+75ms) — i.e. the current column choice is already correct everywhere it could possibly be wrong. What IS real: a systematic (tight, track-dependent, NOT random/non-constant) timing bias of 20–125ms between ADTOF's raw kick-onset picking convention and this fixture pack's hand-labeled truth convention (`audio_labels/README.md`: "onset = walk-back to 25% of the sub-envelope peak"). This is a pre-existing pipeline characteristic, not introduced by P3 — the identical low numbers (apricots 0.20, feel_the_vibration 0.0 via the full `analyze_percussion()` pipeline, not just raw ADTOF) already exist in P1's own committed `dev_2026-07-17.json`, untouched by this session. At the 50ms tolerance `eval/metrics.py` uses (D10-frozen — already wider than these 5 fixtures' own historical ±35ms convention per the labels README), most of these offsets still exceed tolerance and drive F1 toward zero despite ADTOF finding a kick near almost every truth kick. Plausible (not confirmed) explanation for why babyslakh scores so much higher on the same detector: Slakh's aligned-MIDI truth is the exact digital note-onset from synthesis, which may simply sit closer to ADTOF's own onset-picking convention than this pack's hand-crafted "25% envelope rise" rule does.

**Fix shape:** same class of problem D14's decode/detector-stage alignment work already solves (measure a per-stage/per-detector systematic bias, apply the correction once at a defined seam) — but applying it is tuning, out of scope for this measurement-only phase. A future phase should either (a) extend D14's methodology (`decoder_alignment.json`'s mechanism) to a per-onset-convention calibration for this fixture pack, or (b) re-derive these 5 fixtures' truth extraction to match ADTOF's own onset convention if the two are judged incompatible by design. No scorer or detector change made this session (measurement only, per the orchestrator's explicit instruction).

### BUG-232 (harmonix-matched-audio-carries-multi-second-timing-offset) — YouTube-matched Harmonix audio isn't timing-aligned with the annotation reference — found 2026-07-17, AUDIO_ANALYSIS_ACCURACY P3

**Status:** OPEN — LOW-MED (data-integrity gap in a newly-added fixture source; doesn't affect anything already shipping, but blocks using Harmonix-matched audio for beat/downbeat tuning as-is).

**Symptom:** P3 matched real audio to 107/129 Harmonix electronic-slice tracks via `eval/fetch/harmonix_audio.py` (yt-dlp) and scored Beat This against the existing `beats_and_downbeats/<id>.txt` annotations (`eval/full_pack_baseline.py::run_baseline_beat_downbeat_harmonix`). On a 15-track sample, raw beat_f1 came back 0.11 mean (near-zero) despite predicted/truth beat counts being the same order of magnitude and near-identical spacing — the signature of a constant time OFFSET, not a detector failure. Confirmed via a difference-histogram cross-correlation (`_estimate_constant_shift`): per-track offsets of 2.9–7.3s were found, each backed by 150–215 of ~200–450 truth beats (a clear majority). Applying the estimated offset recovers beat_f1 to 0.65 mean (up to 0.95 on individual tracks); downbeat_f1 after the same correction stays low (0.23 mean) — a secondary, likely separate downbeat-phase-lock issue (which beat is "1") worth its own look later.

**Root cause:** the specific YouTube upload `yt-dlp` matched for a given `youtube_urls.csv` entry is not guaranteed to be frame-accurate to whatever recording Harmonix's original annotators used — extra intro/outro content, a different edit, or ad-supported lead-in on the matched video all produce exactly this symptom (one track's audio duration was measured at 241.65s vs the annotation's ~150.38s coverage, consistent with extra unannotated content). This is intrinsic to matching arbitrary YouTube videos against pre-existing third-party annotations, not a bug in the matching script itself (which only checks that a download succeeded, not that it's timing-aligned — it can't check the latter without already having a working alignment step).

**Fix shape:** productize the offset estimator already built for measurement (`eval/full_pack_baseline.py::_estimate_constant_shift`) into an actual per-track alignment step run once at match time (store the estimated offset alongside the matched audio, e.g. a sibling `<track_id>_offset_sec.json`), analogous to D14's decode-stage correction table. Until that exists, Harmonix-matched audio should not be used for beat/downbeat tuning or acceptance gates — it's fine for anything that doesn't need frame-accurate timing (e.g. spot-listening, section-boundary sanity checks at bar-level tolerance may be more forgiving).

### BUG-231 (beat-this-no-tempo-hint-api-for-octave-fix) — `beat_this` has no BPM-range/tempo-prior parameter anywhere in its production-safe inference path — found 2026-07-17, AUDIO_ANALYSIS_ACCURACY P3

**Status:** OPEN — LOW-MED (diagnostic finding, not a functional regression; matters because it forecloses the simplest fix for the `liveshow_integer` octave mis-track P2 flagged as a P4/P5 target).

**Symptom:** P2's landing report recorded that Beat This locks onto ~115.4 BPM on `liveshow_integer` against a ~132 BPM ground truth (≈7:8 ratio, 57% over-detection) and named "BPM range hint 60–200" as a possible P4/P5 fix (Peter's directive). P3 checked whether the `beat_this` package's inference API (`beat_this/inference.py`: `File2Beats` → `Audio2Beats` → `Spect2Frames`) actually accepts any such parameter. It does not — `Audio2Beats.__init__` takes only `checkpoint_path`, `device`, `float16`, `dbn`; `dbn=False` (the "minimal" postprocessing this codebase uses and must keep, per `manifold_audio/beat_tracking.py`'s own comment) is pure peak-picking on framewise beat/downbeat logits with zero tempo awareness. The ONLY place a `min_bpm`/`max_bpm` parameter exists anywhere in the package is inside `Postprocessor.__init__`'s `dbn=True` branch (`beat_this/model/postprocessor.py:27-33`, hardcoded `min_bpm=55.0, max_bpm=215.0`, not even exposed as a constructor argument to the caller) — which lazily imports madmom's `DBNDownBeatTrackingProcessor`, exactly the dependency P2 removed (D1/D2) and `beat_tracking.py` explicitly forbids re-enabling. No diagnostic run with a hint was performed, since doing so would require `dbn=True`.

**Root cause:** Beat This's "minimal" (non-madmom) postprocessing path was designed as a stateless framewise peak-picker with no global tempo model at all — a tempo-range constraint is architecturally a DBN-only concept in this package, not a general inference option.

**Fix shape:** the octave/ratio confusion needs a POST-HOC heuristic applied to Beat This's own beat-time output, not a detector-side parameter: e.g. detect that the median-IBI-derived BPM sits near a small-integer ratio (2:1, 3:2, 4:3, 7:8, etc.) of a target-genre-plausible range (60–200 BPM per Peter's directive) and re-fold/re-interval-select accordingly — analogous to `manifold_audio.bpm._normalize_bpm`'s existing octave-folding, but that function only folds by powers of 2 and would NOT catch a 7:8 ratio (confirmed: 115.4 * 8/7 ≈ 131.9, matching truth almost exactly, but `_normalize_bpm` never tries non-power-of-2 ratios). A future P4/P5 phase implementing this should start from `_normalize_bpm` in `manifold_audio/bpm.py` and extend its ratio set, not invent a new grid-folding mechanism.

### BUG-230 (beat-this-short-clip-bpm-convergence) — Beat This reports the same ~142.86 BPM for 4 of 5 unrelated ~13s isolated-drum-loop test clips — found 2026-07-17, AUDIO_ANALYSIS_ACCURACY P2 listen-list render

**Status:** OPEN — LOW-MED (accuracy; affects short/isolated clips specifically, not full-length mixes — none of the P2 beat/downbeat scoring fixtures, which are full songs, showed this pattern).

**Symptom:** rendering the P2 listen-list (`python -m eval.render_beat_clicks`) over `tests/fixtures/audio/{apricots,bad_guy,feel_the_vibration,tears}_*bpm/mix.wav` (all four are 13.241395833333334s per `soundfile.info`, confirmed identical duration) produced BPM 142.857142857... for all four despite their directory-name BPMs being 128/128/174/140 respectively; `inhale_exhale_145bpm` (same duration) instead got 73.17 (≈142.86/1.953, a plausible near-half-tempo read). `liveshow_*` and `babyslakh_*` fixtures (full-length or much longer) in the same render run showed no such convergence — each got a distinct, plausible BPM.

**Root cause (suspect, not confirmed):** these five fixtures are short (~13s), tempo-warped, isolated drum-loop-style clips (per `audio_labels/README.md`'s existing caveat on `bad_guy_128bpm`: stems are unwarped while the mix is tempo-warped to 13.241s). Beat This is trained on full songs with harmonic + percussive content; on a ~13s isolated/warped drum loop it may lack enough context to lock onto the true tempo and default toward a narrow BPM band its training distribution favors. Not verified against Beat This's own training-data BPM histogram.

**Fix shape:** not a P2 fix (P2 scope is the tracker swap; investigating short-clip behavior further, or re-exporting these five fixtures un-warped at native length, is separate work). Options if revisited: (a) exclude sub-~20s clips from Beat This scoring and route them through the autocorrelation fallback instead (would need a duration-based routing rule in `analyzer.py`, a real code change, not a config tweak); (b) re-export `audio_labels`' warped fixtures at native (unwarped) length per their own README's existing "would remove the caveat" note; (c) accept it as a known short-clip limitation and exclude these 5 fixtures from any future beat/downbeat (not kick-onset) scoring role. Peter's ear on the listen-list is the fastest way to confirm whether this is audible/wrong or a scoring-only artifact.

### BUG-229 (beat-this-frame-hop-exceeds-5ms-alignment-target) — Beat This's 50fps frame resolution puts D14 absolute alignment at ~14ms median, not the ≤5ms the P2 brief named — found 2026-07-17, AUDIO_ANALYSIS_ACCURACY P2

**Status:** OPEN — LOW (measurement/precision-floor finding, not a functional bug — grid is still musically usable; matters only if a future phase wants sub-frame click-accurate triggering from the beat tracker specifically).

**Symptom:** `python -m eval.beat_tracker_alignment` (new in P2, `tools/audio_analysis/eval/beat_tracker_alignment.py`) — a periodic 128 BPM percussive click track (40 clicks, sample-accurate truth positions) run through Beat This — measures median absolute offset 14.375ms, max 26.25ms vs the known click positions (`eval/scoreboard/d14_beat_tracker_alignment_report.json`). The per-beat offsets form a clean ascending sawtooth (12.5, 3.75, 15.0, 6.25, 17.5, ...) — classic frame-quantization aliasing, not random jitter: click spacing (468.75ms) isn't an integer multiple of Beat This's 20ms (50fps) frame hop, so each click's true position falls at a different sub-frame phase and gets snapped to the nearest 20ms-grid frame. Cross-format (mp3/aac vs wav) comparison shows 42/43 beats bit-identical (0ms) with exactly 1/43 beats flipping by one full frame (20ms) — an isolated tie-break at a frame-boundary click, not systematic format drift.

**Root cause:** this is D14's own predicted "model hop quantization (10-23ms frames, center-vs-start conventions)" bias category (AUDIO_ANALYSIS_ACCURACY_DESIGN.md §2 D14), now measured for Beat This specifically instead of the old madmom onset CNN. It is an inherent property of any 50fps frame-based peak-picker, not a defect in the P2 integration — the original (non-periodic) D14 click fixture from P1 can't even measure it (Beat This correctly detects zero beats on non-periodic tone bursts, confirmed 2026-07-17: it is a musical beat tracker, not a general transient detector, so P2 added a second, periodic click fixture — see `build_periodic_click_fixtures` in the same file).

**Fix shape:** D14's own architecture is the fix: the measured per-stage bias becomes a stamped, applied correction at the analysis-input seam (same mechanism as the existing `decoder_alignment.json` decode-stage table) — that wiring is P5's job (`onset_compensation_seconds` → zero, `percussion_settings.rs:42`), not P2's. P2's job was to measure and report, which this entry and `d14_beat_tracker_alignment_report.json` do. If a future phase wants this stamped as an applied correction rather than diagnostic-only, extend `manifold_audio/decoder_alignment.json`'s mechanism (or a sibling table) with a per-tracker entry, keyed the same way, and apply it where beat times are consumed.

**P3 update (2026-07-17):** P2's landing report recorded a hypothesis — quantization noise should average out once you build the FITTED regular grid (median-then-agreement-filtered-mean IBI + an all-beats-averaged phase anchor, reusing `manifold_audio.bpm._build_regular_beat_grid` verbatim) instead of scoring each raw quantized beat individually — and asked P3 to measure it. Measured for real (`eval/beat_tracker_alignment.py`'s `fit_regular_grid_from_beats`/`measure_fitted_grid_alignment`, `eval/scoreboard/d14_beat_tracker_alignment_report.json`) at both 128 and 174 BPM: it does **not** clear 5ms and is not reliably better than raw. 128 BPM: raw median/max 14.38/26.25ms vs fitted 14.16/17.28ms (roughly a wash — better max, same median). 174 BPM: raw 5.86/16.21ms (already the closer-to-gate case) vs fitted 7.98/22.6ms (**worse** on both). Root cause: Beat This over-detects relative to truth (43 detected vs 40 true clicks at 128 BPM) and its own median-IBI tempo estimate drifts slightly off the true tempo (fitted BPM 130.4 vs true 128, and 176.5 vs true 174) — a single regular grid built from a slightly-wrong period accumulates drift across the track instead of averaging out pure per-beat quantization noise, which is what the hypothesis assumed was the only error source. A synthetic unit test confirms the fitting math itself is correct and does average out *pure* quantization noise (median error <5ms when the only error source is simulated 50fps rounding) — the gap is real detector error beyond quantization, not a flaw in the fitting approach. **This rules out "just fit a grid" as P5's correction** — P5 needs to decide between accepting the ~14ms floor as a stamped correction (same mechanism as the decode-stage table) or investigating why Beat This over-detects/drifts on this fixture shape before trying a grid-level fix again. Status unchanged (OPEN — LOW); this closes off one candidate fix, doesn't change severity.

### BUG-228 (manifold-app-tests-gate-clippy-debt-recurrence) — `cargo clippy -p manifold-app --tests -- -D warnings` fails on 3 pre-existing, unrelated lints — found 2026-07-17, lane/import-responsiveness P3 session, while scoping this phase's clippy gate — LOW (lint-only, same class as BUG-088/BUG-110)

**Status:** OPEN — LOW. Confirmed pre-existing and unrelated to this session's changes: `git stash` reproduced all three on the unmodified `HEAD` (`6af40d52`) before any P3 edits landed.

**Symptom:** `cargo clippy -p manifold-app --tests -- -D warnings` (with or without `--features journey-proofs`) fails with 3 unrelated hits: (1) `crates/manifold-app/tests/tree_render_call_sites.rs:24-27` — `doc_lazy_continuation`, an unindented markdown list continuation in the module doc comment; (2) `crates/manifold-app/src/app_lifecycle.rs:1520` — `cloned_ref_to_slice_refs` on `build_video_import_batch(&[fake_webm.clone()], ...)` inside a `#[cfg(test)]` block, fixable with `std::slice::from_ref(&fake_webm)`; (3) `crates/manifold-app/src/text_input.rs:699` — `approx_constant`, a hand-written `0.7853981` literal clippy recognizes as `FRAC_PI_4`. None reproduce under the plain (non-`--tests`) scoped clippy this phase's own gate used, which is why P3 could ship clean against its own gate.

**Root cause:** unknown per-lint — likely toolchain/clippy-version drift re-surfacing the same lint classes BUG-088 fixed elsewhere (`cloned_ref_to_slice_refs`, `doc_lazy_continuation`), now in different files, plus one new lint (`approx_constant`) BUG-088 never covered. Not investigated further — out of scope for this session per the same file-ownership convention BUG-110 used.

**Fix shape:** all three are mechanical, no behavior change: indent the list continuation lines in `tree_render_call_sites.rs`'s doc comment; replace `&[fake_webm.clone()]` with `std::slice::from_ref(&fake_webm)` in `app_lifecycle.rs`; replace the `0.7853981` literal in `text_input.rs` with `std::f32::consts::FRAC_PI_4` (or an explicit `#[allow(clippy::approx_constant)]` if the literal is deliberately not the exact constant). One short session — belongs to whoever owns these files' next change, or the next `--tests`-gate sweep session (BUG-088/BUG-110's precedent).

### BUG-227 (madmom-onset-detector-not-gain-or-timestretch-invariant-on-real-mixes) — the current onset detector (madmom CNN, pre-P2/P6) fails two of the three P1 metamorphic invariant checks on real babyslakh mixes — found 2026-07-17, AUDIO_ANALYSIS_ACCURACY_DESIGN.md P1, by the eval harness's own metamorphic suite (`eval/metamorphic.py`) run against real audio for the first time

**Status:** OPEN — MED (accuracy, not correctness — the pipeline still runs; but it means onset timing/counts are measurably gain- and tempo-sensitive on real polyphonic material, which the P1 gate literally asks the metamorphic suite to rule out and it currently does not).

**Symptom:** `python -m eval.run --set dev` (scoreboard: `tools/audio_analysis/eval/scoreboard/dev_2026-07-17.json`) shows, on 3 babyslakh tracks: `noise_floor_silence` passes cleanly on all 3; `gain_invariance` fails on 2/3 (max matched onset-time shift 80-210ms under ±6dB, well past a 21ms tolerance already loosened once to absorb the detector's own 10ms frame quantization); `time_stretch_invariance` (±5%) fails on all 3 (matched-pair shift 244-250ms, alongside event-count drops of -2 to -12 out of 50-88 base events — i.e. 4-18% of onsets disappear or relocate under a 5% speed change). Diagnosed directly (not inferred): Track00001 goes from 66 detected onsets at normal speed to 54 at +5% speed, with several closely-spaced onset pairs collapsing into one detection.

**Root cause:** the madmom CNN onset activation (100fps, 10ms hop) has real per-onset sensitivity to both absolute level and playback-rate changes on dense polyphonic material — plausibly the peak-picking threshold/combine window interacting with closely-spaced onsets whose relative spacing shifts under time-stretch, or gain shifting which of two nearby candidate peaks wins. Not a harness bug: the two implementation bugs the harness itself had (a noise-floor renormalization step that defeated its own test, and an unbounded nearest-neighbor matcher that force-paired unrelated events into a fake "shift") were found and fixed in the same session — see `eval/metamorphic.py`'s `check_noise_floor_silence` and `_max_matched_time_shift`. What's left after those fixes is the finding above.

**Fix shape:** this is exactly what P4 (Precision post-processing pass, D6: rolling-median adaptive baseline + refractory windows applied to onset activations, non-causally) is designed to fix, and/or what the P2/P6 detector swaps (Beat This, SuperFlux) are expected to improve on incidentally. No action needed before P2 lands — P4's gate should re-run this exact metamorphic suite and treat "gain/time-stretch invariance now green" as evidence the precision pass is working, in addition to its named P/R/F1 gate. Escalate to Peter only if P4 lands and these still fail.

### BUG-226 (golden-png-tests-overwrite-their-own-references-every-run) — the animated/skinned/morph gpu-proof "golden" tests regenerate their checked-in reference PNGs unconditionally instead of diff-gating against them — found 2026-07-17, lane/glb-triage (BUG-221 session), by reading the writer code after 16 goldens showed up modified in an unrelated run

**Status:** OPEN — MED (verification infra; a "golden" that rewrites itself on every run gates nothing — a regression in skinned/morph rendering would silently update the reference instead of failing).

**Symptom:** running the animated/skinned/morph gpu-proof tests leaves 16 modified files under `tests/fixtures/gltf/goldens/` (box_animated, cesiumman_skin, fox_skin, animatedmorphcube_morph, morphstresstest_morph) even when rendering is unchanged — and would do the same if rendering were BROKEN.

**Root cause:** the tests' writer path unconditionally writes the rendered PNG to the golden path every run (confirmed by reading the writer code, 2026-07-17); no compare-then-fail branch exists.

**Fix shape:** standard golden pattern — compare against the checked-in reference with a tolerance (the conformance suite's comparator is the precedent), fail on mismatch, regenerate only behind `MANIFOLD_UPDATE_GOLDENS=1`; add a meta-check that the goldens dir is clean after a suite run. One mechanical session.

### BUG-225 (ui-flow-harness-first-gesture-free-rebuild-masks-missing-invalidation) — the headless flow harness cannot detect missing rebuild-invalidation bugs: the first dispatched gesture gets a free full rebuild, and `needs_structural_sync` never resets once set — found 2026-07-17, lane/panel-interaction-bugs, while root-causing BUG-223

**Status:** OPEN — MED (verification infra; every L3 flow gate that asserts "the screen changed after an input" is potentially proving less than it claims for invalidation-class bugs).

**Symptom:** BUG-199's two landing flows stayed green while the scroll fix they gated was visibly broken in the app (BUG-223). The harness masked it twice over: `Runner::advance_frame` forces one full `ui_root.build()` on the first dispatched gesture of any script (via `Inspector::skip_to_settled`), and `needs_structural_sync`, once set by any earlier step, is never reset to `false` — so every subsequent step also rebuilds. A flow can therefore never distinguish "this input correctly requested a rebuild" from "the harness rebuilt anyway."

**Root cause:** harness-side rebuild gating diverges from the app's dirty-flag-gated rebuild (`apply_ui_frame_invalidations`) — the exact fidelity-by-construction drift `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` exists to kill; this is a new instance at the invalidation layer.

**Fix shape:** make the harness honor the app's real invalidation gating (no free first-gesture rebuild; reset `needs_structural_sync` per frame exactly as the app does), then add a negative meta-flow that scrolls WITHOUT the fix's `needs_rebuild = true` and asserts the screen does NOT move — proving the harness can now see this bug class. Evidence and mechanism detail: BUG-223's Escaped note (same file), found by instrumented divergence-diffing, 2026-07-17.

### BUG-217 (non-lerp-mix-alpha-passthrough-kills-trails-on-transparent-sources) — BUG-181's "non-Lerp blends pass `a`'s alpha" makes feedback trails invisible whenever the source has a transparent background — found 2026-07-17 during the depth-relight look probe

**Status:** OPEN — LOW (documented idiom shipped @ f2684402; the alpha-mode enum on `node.mix` is still undecided — see below).

**Symptom:** a Max/Add feedback blend over a generator source with alpha-0 background (any SDF shape on transparent black) accumulates trails in RGB but every trail pixel outside the current source's alpha footprint carries alpha 0, so the display path culls them — the preset renders as if feedback were off.

**Root cause:** deliberate BUG-181 contract — non-Lerp `node.mix` modes are RGB-only and pass `a`'s alpha through so an AO map's alpha=1 can't overwrite a display chain's real alpha. Correct for masks; for feedback accumulation it means the blend's alpha never widens to cover the trail RGB it just wrote.

**Fix shape:** don't revert BUG-181. D7's cheap fix (the `set_alpha`-before-blend idiom, documented in `node.mix`'s and `node.feedback`'s `composition_notes` @ f2684402) is shipped and is the current answer for anyone who hits this. Still open/undecided: an explicit `alpha` mode enum on `node.mix` (PassA / Lerp / Max) so accumulation graphs can opt into alpha-max without the extra `set_alpha` node — Peter's call, no consumer has asked for it since the idiom shipped.

### BUG-209 (animated-ancestor-above-joint-tree-sampled-statically) — root motion authored on a node above a skin's joint tree is frozen — promoted from BUG-205's "known remaining approximation" note, 2026-07-17

**Status:** OPEN — LOW (no real asset observed exhibiting it: Mixamo roots motion inside the rig at Hips; the Sketchfab Bip01 case bakes animated values ≈ its static TRS).

**Symptom (predicted):** a rig whose ancestor prefix genuinely animates (translation/rotation/scale keyframes with values that CHANGE) plays its pose correctly but stays pinned in place — the prefix motion is dropped.

**Root cause:** `gltf_skeleton_pose` roots joint worlds at `joint_root_world` — the STATIC ancestor-chain product (`static_world_matrix`), by design (GLTF_ANIMATION_DESIGN.md A2). Post-BUG-205 the rigid `gltf_animation_source` no longer re-applies the chain (that was a double-transform, not a fix for this), so the animated prefix has no carrier at all.

**Fix shape:** extend the pose primitive with prefix tracks — sample the ancestor chain's animated TRS per frame (one extra Table keyed like the joint tracks, joint index -1 or a dedicated `prefix_tracks` param) and compose it in place of the static `joint_root_world`. Gate with a shelf fixture whose prefix translates across the frame (make_hostile_rig.py variant: keyframe the `UnitConversion` empty).

### BUG-191 (perf-soak-start-seek-first-frame-spike) — `cargo xtask perf-soak --start <beats>` produces a ~34-37ms content-thread frame right after the transport seeks, tripping I1 on that one frame — found 2026-07-16 during PERF_BUDGET_GATE_DESIGN.md P2, confirmed pre-existing

**Status:** PARTIAL FIX @ Lane 6, 2026-07-17 — the `node.spawn_from_image` contributor is closed (prewarmed, gated green); the dominant remaining contributors (`node.wgsl_compute` × several instances, `node.blob_tracker`) are ATTRIBUTED but not fixed — fix shape queued for next wave.

**Symptom:** `cargo xtask perf-soak "Liveschool Live Show V6 LEDS.manifold" --seconds 10 --start 400 --profile` shows a single ~52ms GPU / frame-0 spike immediately after the transport seeks to the `--start` beat, well over I1's 20ms hard-fail line.

**Diagnosis (Lane 6, this session):** the `--profile` per-node breakdown on frame 0 (the post-seek frame) directly attributes the cpu_us outlier: before the fix, `node.spawn_from_image` (`seed_particles_from_texture.rs`, type_id `node.spawn_from_image`) alone cost **66.4ms cpu_us** on frame 0 — its four hand-written compute pipelines (`count_main`/`scan_main`/`compact_main`/`place_main`) were being lazily compiled on this project's first live use of that node. Root cause: this primitive is a barriered multi-pass hand-written-pipeline atom (same class as `node.scatter_on_mesh`, exempt from the codegen path per CLAUDE.md's fusion rule), so BUG-146's codegen-sweep prewarm never reached it — and no bundled preset happens to reference `node.spawn_from_image` either, so `GeneratorRegistry::prewarm_all`'s bundled-preset loop never exercised it. It had **no prewarm coverage at all**, identical to BUG-037's original `scatter_on_mesh` gap, just never closed for this sibling primitive.

**Fix shipped:** `SeedParticlesFromTexture::prewarm_pipelines(device)` (`crates/manifold-renderer/src/node_graph/primitives/seed_particles_from_texture.rs`), mirroring `ScatterOnMesh::prewarm_pipelines` exactly (compiles all four entry points against the fixed, asset-independent shader source), wired into `GeneratorRegistry::prewarm_all` (`crates/manifold-renderer/src/generators/registry.rs`) alongside `RenderScene`/`ScatterOnMesh`. Gated: new `gpu_tests::prewarm_pipelines_populates_the_shared_compute_cache` test (mirrors `scatter_on_mesh`'s identical gate) — green. Verified against the actual repro: re-running the same perf-soak post-fix, `node.spawn_from_image` no longer appears anywhere in frame 0's per-node cpu_us breakdown.

**Remaining gap (fix shape for next wave, NOT this session's scope):** frame 0's cpu_us is still dominated by several `node.wgsl_compute` instances (79.2ms / 30.5ms / 23.6ms / 13.2ms / 10.8ms / 10.2ms cpu_us on the six worst — clearly six distinct node instances, each a distinct pipeline compile) plus `node.blob_tracker` (28.6ms cpu_us) — worst frame is still ~53ms GPU post-fix. Unlike `spawn_from_image`, `node.wgsl_compute`'s shader source is genuinely PROJECT-authored data (arbitrary WGSL text baked into this project's own JSON, not a fixed asset), so there is no generic app-startup `prewarm_all` target — BUG-037's precedent doesn't transfer directly. The correct fix shape is a **project-load-time prewarm sweep**: when a `.manifold` project loads, walk its graphs for every `node.wgsl_compute` instance and compile its specific pipeline before playback begins (once per unique (shader_src, entry_point) — the device's `compute_cache` already dedupes by hash, so a naive sweep over every instance is safe and cheap to reason about even if some instances share source). `node.blob_tracker`'s 28.6ms cpu_us is a separate, unexamined contributor this session did not dig into — likely a DNN/FFI init cost on first use, same first-use-resource-creation class, needs its own attribution before fixing. Neither of these was attempted this session (out of the "obvious and small, <~50 lines" bar for this lane) — whoever picks this up next should NOT re-derive the diagnosis, it's already attributed above; go straight to designing the project-load sweep + attributing blob_tracker.

### BUG-190 (brainstem-24-skinned-objects-370ms-per-frame) — `BrainStem.glb` (24 separate skinned objects, 78 materials total) renders a flat ~370ms/frame from frame 0 — 18x over the 20ms hot-path budget — found 2026-07-16 during GLTF_ANIMATION_DESIGN.md A2's hot-path gate
**Status:** OPEN — RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0–P5 SHIPPED 2026-07-17 without fixing this bug (in scope as diagnosis-only per that design's D3; P4's CPU repair was the design's one shot at this fixture and only modestly improved it — see below). The originally-filed ~370ms/frame does NOT reproduce on the current tip (confirmed twice, P0 and again this session — ~30ms max on the original harness). What remains is CPU-side (~20ms encode wall, GPU side is healthy at ~4-7ms) and is a **named, unresolved follow-up** — NOT render_scene's format!/scan pattern (P4 fixed that class and it barely moved the needle here). NOT blocking A2 (BrainStem was the design doc's joint-count re-derivation note, never a named gate fixture — the actual A2 gate fixtures, `CesiumMan.glb` and `Fox.glb`, measure 5–7ms/frame, comfortably inside budget).

**Symptom:** `node_graph::gltf_import::tests::skinned_import_hot_path_stays_under_20ms_per_frame` (gated `#[cfg(feature = "gpu-proofs")]`) measured `BrainStem.glb` at a steady ~365-400ms per frame across 30 measured frames (10-frame warmup, no downward trend — not a one-time cold-parse cost). `CesiumMan.glb` (single skinned object, 14016 vertices, 19 joints) measures 5.3-7.5ms/frame on the identical harness, so the per-object skin_mesh dispatch + skeleton-pose CPU sampling is NOT the obvious culprit at this scale.

**Root cause:** unknown. Re-derived this session: `BrainStem.glb` is not a high-joint-count stress case as GLTF_ANIMATION_DESIGN.md's D2 assumed — it's 24 separate skinned objects sharing an 18-joint skeleton, so it exercises 24 concurrent `node.gltf_skeleton_pose` + `node.gltf_skinned_mesh_source` + `node.skin_mesh` chains (72 nodes) inside one `render_scene` pass. Suspects, in rough priority order: (a) a pre-existing many-object `render_scene` scaling cost unrelated to skinning — BUG-189 already measured `mercedes-amg_gt3` (302k tris, 78 materials, unskinned) at a ~10ms resolution-independent floor per frame, so a 78-material asset being expensive is not new, but 370ms is 37x BUG-189's floor, not just "a bit more" — needs its own attribution; (b) 24x redundant `node.gltf_skinned_mesh_source` background-thread work (each of the 24 objects re-parses `BrainStem.glb` from disk via `load_gltf_skinned_mesh` on its own thread — should only fire once per object at `key` change, not every frame, but not verified under a profiler this session); (c) shadow-casting-light re-render × 24 draw calls; (d) something in the per-frame CPU pose-sampling scaling worse than expected across 24 concurrent `node.gltf_skeleton_pose` primitives (each is O(rows) linear scans over its own Tables — should be cheap, not verified against BrainStem's actual per-joint keyframe-track row counts this session).

**Fix shape (superseded — see P0 diagnosis below):** profile first — same designated instrument BUG-189 names — then decide between (a)–(d).

**P0 diagnosis (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0, 2026-07-17, diagnosis only, no fix attempted):** `cargo xtask perf-soak`, the design's designated oracle, **cannot measure BrainStem at all** — its warmup gate requires 3 consecutive byte-identical frames before the measured window starts (D7, ported from `render_import.rs`), and BrainStem's import graph animates continuously (confirmed by instrumenting the warmup loop directly: `io_pending` is false every frame — no stuck decode — but `byte_stable` is false every single frame across 300 warmup frames; this is a structural property of any continuously-animated glTF import, not specific to BrainStem being slow). Worked around for diagnosis only (fixed-length warmup instead of convergence-gated, reusing the tool's own measured-window functions verbatim, not committed — no product code changed): GPU p50=6.8ms, p95=8.85ms, max=10.1ms over 300 frames @1920×1080 (comfortably inside the 20ms budget — the GPU side is fine); CPU encode wall p50=21.4ms, p95=22.4ms, max=23.6ms (the actual budget-buster, ~3× the GPU cost). Cross-checked against the ORIGINAL harness (`skinned_import_hot_path_stays_under_20ms_per_frame`, BrainStem temporarily re-added to its asset list, run, then reverted — not a permanent test change): **avg 30.27ms, max 32.36ms over 30 frames @512×512 — not ~370ms.** The originally-filed ~365-400ms figure does not reproduce on today's tip (`a036cfb5`); whatever drove it appears to have already been resolved by intervening work between BUG-190's filing (during A2) and now (post-A3 merge, `a6168ce8`) — no fix was attempted this session and the responsible commit was not identified. What today's numbers rule out: suspect (a) (a BUG-189-class GPU floor) and (c) (shadow re-render cost) are both ruled out — the GPU number alone is 6.8-8.85ms, nowhere near a 20-30ms problem. The remaining ~20-32ms is CPU-side and roughly 3× the GPU cost, consistent with (b) (redundant background work) or (d) (per-frame CPU pose-sampling across 24 concurrent chains) rather than (a)/(c) — still unknown which, not investigated further this session (diagnosis only, per D3).

**Fix shape:** unknown until the remaining CPU-side cost (~20ms, still over budget) is attributed — profile the CPU side directly (a CPU flame graph, or per-node `cpu_us` from `--profile`'s existing per-node breakdown, would separate (b) from (d)). Measurement harness: `cargo test -p manifold-renderer --features gpu-proofs --lib skinned_import_hot_path_stays_under_20ms_per_frame -- --nocapture` (currently asserts only on CesiumMan/Fox; re-add BrainStem to the asset list with a raised or removed budget once the remaining cost is diagnosed, so it stays a live regression guard rather than a one-time measurement). Separately, `cargo xtask perf-soak` itself cannot exercise any continuously-animated glTF import (this is a tool-level gap in the shared oracle, not scoped to fix in this design — noted here for whoever picks up BrainStem's remaining cost next, since they'll hit the same non-convergence wall).

**P4 finding (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P4, 2026-07-17) — the CPU-side residual is NOT render_scene's per-object `format!`+scan pattern.** P4 shipped exactly that fix (per-object port-index tables built once at `rebuild()`, replacing ~21 per-frame `format!` allocations + a linear `iter().find` scan per object per frame with O(1) indexed lookups) and, per its own brief, gained BrainStem's committed `--warmup-frames` tool flag so this fixture could be measured reproducibly. Before/after on BrainStem (this flag, 1920×1080, 300 frames): CPU-encode-wall p50 21.4ms (P0's uncommitted measurement) → 20.33ms (P4/this session's measurement, `--warmup-frames 30`) — **only a ~4-5% improvement**, GPU side unaffected (already healthy, ~4-8ms). The repaired code path was real waste and worth fixing on its own terms (it scales as O(objects²) and would bite harder on a larger scene than BrainStem), but it was **not** BrainStem's dominant cost. **Root cause of the remaining ~20ms CPU-encode wall is still unattributed** — this design's scope closed at P4 (D3/D3b: diagnosis-only, no invented fix); suspects (b) redundant background re-parse and (d) per-frame CPU pose-sampling scaling from §Root cause above remain open and untested. This is now a standalone follow-up for whoever picks up BrainStem next — do not re-attempt a fix by guessing; profile the CPU side directly first (per-node `cpu_us` from `--profile`, or an actual CPU flame graph) before touching code.

**P5 final re-measure 2026-07-17 (full landed tree):** BrainStem @1920×1080, `--warmup-frames 30`, 300 frames: GPU p50=4.003ms p95=8.174ms max=9.277ms (healthy); CPU-encode-wall p50=20.330ms p95=21.296ms max=22.862ms (still ~3× the GPU cost, still over the 16.6ms/frame budget at p50). This entry stays OPEN as the tracker for the unattributed residual CPU cost — no new bug ID needed; RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md's own scope is closed (P0-P5 SHIPPED), this bug is not.

**Lane 6 diagnosis (2026-07-17, ATTRIBUTED — suspect (d), not (b)):** `--profile`'s existing per-node `cpu_us` breakdown doesn't discriminate here — `node.gltf_skeleton_pose` and `node.gltf_skinned_mesh_source` are both CPU-only (`boundary_reason: NonGpu` / no GPU pass), so neither gets its own profiled span; their cost gets folded into whichever GPU-tracked node runs next (`node.render_scene`), which is why render_scene's own cpu_us looked anomalously large and noisy across frames. Per the lane brief's fallback, instrumented both suspect paths directly with `Instant`-based timers (temporary, reverted before commit — not shipped):
- `node.gltf_skinned_mesh_source` (suspect (b), the 24×/59× background re-parse): **avg 2.2-3.9us/call**, confirming the background thread only fires once per (path, material_index) key change (its `pending_load`/`last_key` gate works as designed) and is negligible in steady state. Suspect (b) is RULED OUT as the dominant cost.
- `node.gltf_skeleton_pose` (suspect (d), per-frame CPU pose-sampling): **avg ~6.9ms/call**, run twice per frame per skinned object (main + shadow pass) — this is the entire CPU-encode wall, confirmed directly. Suspect (d) is the root cause.

**Root cause (found):** `row_range_for_compound_key` and `mat4_from_table` (`crates/manifold-renderer/src/node_graph/primitives/gltf_anim_shared.rs` + `gltf_skeleton_pose.rs`) each do a full **O(n) linear scan over the entire track table from index 0**, called once per joint (18× for BrainStem's skeleton) across three track tables (translation/rotation/scale) plus two topology tables (root_world/inverse_bind), every single frame — for data that is STATIC after import: the row range for a given (clip_index, joint) pair never changes frame-to-frame, only the sampled time `t` within that range does. This is O(joint_count × total_track_rows) repeated work that should be O(joint_count) with a one-time O(total_track_rows) precompute, multiplied by 59 concurrent skinned-object instances × 2 passes/frame.

**Fix shape (not attempted — exceeds this lane's "obvious and small, <~50 lines" bar):** cache each joint's `(start, end)` row range per (clip_index, joint) pair in `GltfSkeletonPose`'s `extra_fields` (a `Vec<(usize,usize)>` per track table, indexed by joint, keyed on `clip_index`), computed once on first `run()` after a clip_index/table change instead of re-scanning every frame. Requires: (a) cache invalidation logic (clip_index change, or the tables themselves changing — they come from `ctx.params`, static post-import in practice but not provably so from inside this primitive), (b) updating `mat4_from_table`'s two topology-table lookups similarly (small tables, ~18 rows, likely NOT the bottleneck — verify before caching those too, since they add cache-invalidation surface for probably-negligible gain), (c) a GPU parity test proving byte-identical output before/after caching kicks in (same shape as this file's existing `frame2_matches_frame1_on_static_asset` pattern in the sibling `gltf_skinned_mesh_source.rs`). This is real, scoped work for next wave — not a guess-and-patch job.

**Note 2026-07-18 (GLTF_ANIM_RUNTIME_V2_DESIGN.md P4):** `lane/gltf-anim-v2` replaced this bug's own root cause — the O(n) per-frame linear scan over `row_range_for_compound_key`/`mat4_from_table` table rows — with binary search over a shared, file-backed, flat-slice cache (D3); `row_range_for_key`/`row_range_for_compound_key` are deleted on that branch. The 370ms/20ms figures above were measured on the pre-anim-v2 table-scan architecture and have NOT been re-measured against it — no in-app BrainStem hot-path number exists post-anim-v2 yet (only a `render-import` import+convergence measurement, 0.55 GB / 0.69s, which is not the same oracle as this bug's `skinned_import_hot_path_stays_under_20ms_per_frame` harness). Re-measure with that harness once anim-v2 lands before closing or updating this bug's Status.

### BUG-188 (meshprimitivemodes-non-triangle-primitive-blanks-whole-object) — a mesh mixing POINTS/LINES/LINE_STRIP/LINE_LOOP/TRIANGLE_STRIP/TRIANGLE_FAN alongside TRIANGLES renders fully black instead of drawing at least the TRIANGLES primitives — found 2026-07-16 during GLB_CONFORMANCE_DESIGN.md G-P7 sidecar-fetch sweep
**Status:** OPEN — found while wiring `MeshPrimitiveModes` (Khronos glTF-Sample-Assets) into the conformance manifest; xfail'd as `xfail:BUG-188` rather than blocking.

**Symptom:** `render-import tests/fixtures/gltf/khronos/MeshPrimitiveModes/MeshPrimitiveModes.gltf` never converges (300 frames, non-black fraction stays 0.0000 the whole time) — the object never draws anything, even though one of its seven primitives (mode 4, TRIANGLES) is fully within spec.

**Root cause:** `MeshPrimitiveModes.gltf` has one mesh with seven primitives, one per glTF primitive mode (0=POINTS .. 6=TRIANGLE_FAN), all assigned to the same (single) material. `flatten_primitive` (`gltf_load.rs:462`) returns `Err` for any primitive whose `mode() != Triangles`; `walk_gltf_node` (`gltf_load.rs:581`) propagates that `Err` with `?` from inside its per-primitive loop, so the FIRST non-Triangles primitive it iterates aborts `load_gltf_mesh` for the entire material/object — including the five/six other primitives that would have flattened fine. The asset's one true TRIANGLES primitive never gets a chance to render because a sibling primitive earlier in iteration order fails first.

**Fix shape:** two independent, composable pieces — (1) the narrower root fix: `walk_gltf_node` should not let one primitive's failure blank an entire object; catch `flatten_primitive`'s `Err` per-primitive, log/report it (e.g. into `ImportReport.report_lines`, the existing informational-gap channel `MandarinOrange`'s normal-scale note already uses), and continue accumulating the primitives that DO flatten. This alone would make `MeshPrimitiveModes` render its TRIANGLES primitive instead of nothing. (2) the broader feature gap: actually drawing POINTS/LINES/LINE_STRIP/LINE_LOOP topology needs a real point/line rendering path (not a triangle-list mesh) — out of scope for the mesh-flattening loader alone, a `render_scene`/primitive-topology design question. TRIANGLE_STRIP/TRIANGLE_FAN, by contrast, are mechanically convertible to a triangle list in `flatten_primitive` itself (standard fan/strip-to-triangles index expansion) and would be the cheapest partial win. Not attempted this session — found during a fetch-and-classify sweep, not a rendering-feature session.

### BUG-187 (meshoptcubetest-khr-mesh-quantization-unsupported) — `MeshoptCubeTest`'s glTF variant requires `KHR_mesh_quantization`, which MANIFOLD does not implement — found 2026-07-16 during GLB_CONFORMANCE_DESIGN.md G-P7 sidecar-fetch sweep
**Status:** OPEN — correctly rejected (no silent misrender), xfail'd as `xfail:BUG-187`.

**Symptom:** `render-import tests/fixtures/gltf/khronos/MeshoptCubeTest/MeshoptCubeTest.gltf` fails immediately: `extensionsRequired[..] = "KHR_mesh_quantization": unsupported extension (MANIFOLD does not import this extension)`.

**Root cause:** the asset's `extensionsRequired` list names `KHR_mesh_quantization` (normalized/quantized vertex attribute encoding — separate from `EXT_meshopt_compression`, which this same asset also uses for its buffer views and which is likewise unimplemented). `gltf_load.rs`'s extensionsRequired veto correctly identifies and rejects both as unsupported before attempting to parse geometry, working as designed (`EXT_texture_webp` was the same clean-veto path until IMPORT_ANYTHING_WAVE_DESIGN.md W1 implemented webp decoding and closed BUG-186 — that extension is no longer vetoed). No misrender risk; the asset is simply unrenderable until one or both extensions are implemented.

**Fix shape:** implement `KHR_mesh_quantization` (dequantize normalized integer attribute accessors per the extension spec — read as normalized ints, scale to float range, same accessor pipeline just with a decode step) and/or `EXT_meshopt_compression` (meshopt buffer-view decompression, a well-known open-source algorithm) in `gltf_load.rs`'s buffer/accessor resolution path. Neither is in `GLB_CONFORMANCE_DESIGN.md` D5's originally-scoped deferred-extension list (sheen/iridescence/anisotropy/volume/Draco/KTX2/meshopt IS actually named there as deferred — meshopt itself was already a known gap; `KHR_mesh_quantization` is the new addition this session's fetch surfaced). Low priority — no other manifest asset depends on either extension.

### BUG-175 (fluidsim2d-dead-black-after-live-resize) — FluidSim2D renders black after a live project-resolution change, even after the resize state-reset fix — MED-HIGH, found 2026-07-16 while verifying the Cymatics resize fix
**Status:** OPEN — reproducer test exists and is `#[ignore]`d pending fix: `preset_runtime::generator_runtime_tests::fluidsim2d_survives_live_resize` (gpu-proofs).

**Symptom** — after `PresetRuntime::resize()`, FluidSim2D's output is max luma 0 (dead black) and never recovers. Cymatics had the same symptom and is FIXED by the resize-site state clear (`b11e6511`: resize wipes all pinned bindings including Array<T> wire buffers; clearing graph state re-arms every seed bootstrap). That fix rescues Cymatics (seed wired straight from `spawn_particles`) but NOT FluidSim2D — its re-seed path does not come back.

**Provenance (git, 2026-07-16)** — regression introduced by `22e8ac06` (2026-06-09, "array_feedback: zero-copy in-place fast path, FluidSim 11.8ms → 5.5ms"): before it, the 1-frame-delay state lived in a private `prev` GpuBuffer in the StateStore, which `resize()` never wipes — sims survived resolution changes by continuing from the private copy. The fast path moved state into the aliased wire buffer (the thing resize wipes) to kill the copy cost. NOT related to the fusion-compiler work. Peter's "it used to work perfectly fine" is consistent: anything before 2026-06-09 survived resize.

**Root cause** — the in-place state loss is established (above); what remains unexplained is why the b11e6511 state-clear's re-seed doesn't re-arm for FluidSim2D the way it does for Cymatics. Suspects, in order: (1) the `aliased_array_io` in-place particle-buffer alias (in/out on ONE physical slot — FluidSim/FluidSim3D/ParticleText) may not be re-established by resize's `pre_allocate_resources` re-run, leaving `array_feedback` on a different code path than at construction; (2) `spawn_particles(OnceOnReset)` + the wgsl seed-pattern chain may not re-emit after `clear_state()` without a trigger edge; (3) the fluid field ping-pong `wgsl_compute` passes with hand-tuned per-slot formats may re-allocate at the new size but carry stale build-time dims in uniforms. FluidSim3D and ParticleText share the aliased-buffer shape and are UNTESTED for this — assume affected until proven otherwise.

**Fix shape (Peter-approved direction, 2026-07-16)** — the fundamental fix: `MetalBackend::resize`/`pre_allocate_resources` must stop wiping capacity-sized Array<T> buffers. Only canvas-sized resources (density accumulators, textures) depend on resolution; particle buffers are sized by `max_capacity` with UV-normalized positions and are valid across a resolution change. Preserve them and every particle sim — seeded or not, in-place or not — survives resize WITHOUT restarting (strictly better live behavior than re-seeding). The plan already distinguishes canvas-sized resources (`canvas_sized_array_outputs`). When this lands: un-ignore `fluidsim2d_survives_live_resize`, clone it for FluidSim3D/ParticleText, and REVERT the `b11e6511` state-clear in `PresetRuntime::resize` — it becomes actively wrong (needlessly restarts sims that could have survived). The Cymatics + FluidSim2D resize tests are the acceptance gate.

### BUG-160 (editor-window-unification-inspector-card-layout-regressions) — inspector cards no longer lay out properly (buttons and controls don't fit) after the editor-window-unification landing — MED-HIGH UI regression, reported by Peter 2026-07-14
**Status:** PARTIAL — P2 (tick parity, D4) SHIPPED 2026-07-15 (Sonnet, `d85ab207`, bug/160-layout-invariance): `UIRoot::tick_inspector` extracted and wired into `present_graph_editor_window`, fixing the reported card-HEIGHT-overflow defect (rows drawing past the card's bottom edge) — the editor's `UIRoot` now advances drawer/collapse tweens every frame it presents, and the Author snap-vs-ease fork is deleted (also retires BUG-157's inspector half, see that entry). P1 PARTIAL in the same landing: D1 (chevron lane reserved in both contexts) + D2 (one shared `row_geometry()` helper replacing the two duplicated inline lane-arithmetic sites in `build_effect_sliders`/`build_generator`) + D7 (`Dock::editor()`'s `right_range` widened to the shared `MIN_INSPECTOR_WIDTH..MAX_INSPECTOR_WIDTH` policy) shipped. **Still owed**: D3 (elide-to-width labels + choice-chip fit/wrap across every row width — the secondary width-fit defect from Peter's screenshot, the eight-chip Feature strip clipping captions) and the width-sweep containment test (`inspector_rows_fit_card_bounds_across_widths`) — genuinely out of scope for this landing (Change 4's own P1 phase brief sizes it at "one session" and D3's chip-elision/wrap mechanism alone is that scope); a dedicated follow-up session should pick these up. Root cause, per Peter's screenshot (Stylized Feedback card, audio-mod drawer): **card content does not fit the card's width** — the eight-chip Feature strip clips its own captions, the title truncates into the AUD badge, sliders crush to stubs; the shared row layout has no fit logic (no elide/min-width/wrap for chips and labels), and the defect surfaces at the editor lane's narrower default (340 vs the main window's 500). The same class independently reproduced in `ui-snap editor` on tip `bbc30bce` (Fluid Sim 2D "Clip Trigger Mode" label clipped). The initially-suspected Author chevron lane (`param_card.rs:2469`/`:2702` subtract lane width only in Author) is real but SECONDARY — an identity violation Change 4 unifies, not the fit bug. Peter's rulings, both verbatim in Change 4 D7: "FUNDAMENTALLY the same object in code so they can't drift or differ by design" — then, superseding on width only, "the width can differ as the user may want different widths for the editor and main page, everything else should be fundamentally the same." Compounding: the editor's `UIRoot` is never ticked (`update()` is `built`-gated, BUG-157's mechanism), so Author cards carry a snap-instead-of-ease motion fork (`param_card.rs:830`, `:1080-1090`) as a workaround. Peter's directive (2026-07-14, verbatim in Change 4): editor cards IDENTICAL to main-window cards, mapping chevron the only extra, drift structurally impossible, and gates must be tree asserts because "png checks are not reliable for Sonnet agents". Bisection is no longer the plan — the fix is structural, not archaeological. This session's code-reading pass, for the record: `InspectorCompositePanel::build_in_rect` (the editor host, `inspector.rs:2041`) shares the same per-card build logic as `build` (the main-window host, `:2349`) — no obvious divergent code path found. Width isn't an obvious culprit either: the editor's card-lane range (`Dock::editor()`'s `right_range = (240.0, 560.0)`, default `EDITOR_RIGHT_DEFAULT = 340.0`) sits inside the main window's own tested range (`MIN_INSPECTOR_WIDTH = 232.0` .. `MAX_INSPECTOR_WIDTH = 900.0`, default `500.0`) — if cards render correctly at the main window's 232px floor, 340px shouldn't be new territory, so a width-only theory doesn't explain the symptom on its own. Genuinely unconfirmed without pixels on screen — do not treat the above as ruling anything out, just as what didn't pan out on a code-only pass.

**Symptom** — after EDITOR_WINDOW_UNIFICATION landed (P1–P3, on main 2026-07-14, merge
`a0eba10c`), inspector cards show layout misfits: buttons and controls don't fit their
cards properly. Peter attributes it to the unification work ("the editor unification
work also introduced new bugs where the inspector cards don't fit properly with their
buttons etc"). Exact scenes and cards not yet enumerated.

**Root cause** — unknown, not investigated. Suspect surface: the P1 shared-pass
extraction (`tree_passes.rs::render_tree_overlay_passes`) and any width/metrics
divergence between the main-window inspector path and the unified path. Note P1's own
I4 verification byte-diffed the `timeline`/`states`/`inspector` ui-snap scenes and
called them equivalent (modulo BUG-153's nondeterminism) — so either the regression
came in with P2/P3, or it lives in a configuration the fixtures don't cover. That
discrepancy is itself a lead.

**Fix shape** — bisect the affected scene against the pre-unification tip
(`ui-snap` PNG diff is the oracle); fix at the layout source per
`single-source-y-layout`, never per-widget nudges; regression = PNG diff on the
affected scenes pinned into the fixture set (extend a fixture to cover the failing
configuration if the current ones render clean).

### BUG-157 (editor-perf-hud-never-ticked-shows-dashes-forever) — the graph-editor window's own `perf_hud` overlay, if opened, would render permanently blank "—" values — LOW (currently unreachable: no keyboard/UI path opens it on the editor's own `UIRoot` today)
**Status:** PARTIAL — found 2026-07-14 during `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P2, while building the phase's perf-HUD-in-editor acceptance demo. The shared root mechanism — the editor's `UIRoot` never sets `built`, so its own `update()` (which is what ticks `perf_hud`, along with the inspector) always early-returned — is fixed as of `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4 P2 (`d85ab207`, 2026-07-15): `UIRoot::tick_inspector` is now called directly from `present_graph_editor_window`, so the INSPECTOR half of this mechanism (drawer tweens, collapse, value-flash) advances every frame in the editor. The perf-HUD half remains open: that phase wired only `tick_inspector` + `update_fire_meters`, not `self.perf_hud.update(...)` — `perf_hud` is still never ticked on the editor's `UIRoot`, so if it's ever opened there it would still render permanent `"—"` values. Still currently unreachable in the live app (no keyboard/UI path opens it on the editor's own `UIRoot`), so it still blocks nothing — but the fix, if picked up, is now precedented: mirror `tick_inspector`'s call shape for `perf_hud.update(&mut self.tree)`.

**Symptom** — `PerfHudPanel::build_at_xy` seeds every value row with the placeholder text `"—"`; real numbers only appear once `PerfHudPanel::push_values(tree)` runs (called via `Panel::update` → `UIRoot::update()` on the MAIN window's `self.ws.ui_root`, `app_render.rs:3087`). The graph editor's own `UIRoot` (`self.graph_editor.as_mut().ui_root`, a separate instance) never gets `.update()` called on it — and even if it did, `UIRoot::update()` early-returns on `if !self.built`, which the editor's `UIRoot` never sets (it's built via `build_overlays_for_screen`, never the main-window-only `UIRoot::build()`). So if the editor's own `perf_hud` were ever toggled visible, it would render its full chrome (background, rows, graph bars) but every value would sit at `"—"` forever, un-ticking.

**Root cause** — two compounding gaps: (1) no call site anywhere in `manifold-app` invokes `perf_hud.push_values()` outside the `built`-gated `UIRoot::update()` path; (2) the editor's `UIRoot` is permanently `!built` by design (P1's `build_overlays_for_screen` wrapper deliberately avoids the main-window-only `build()`), so the one existing route to `push_values` can never fire for it.

**Currently unreachable, so LOW:** confirmed via `rg "toggle_performance_hud"` — the only call site is `input_host.rs`'s `AppInputHost`, constructed exclusively with `self.ws.ui_root` (the main window) inside `window_input.rs`'s `is_primary`-gated `InputHandler` shortcut-dispatch block. There is no keyboard shortcut, button, or other path that opens the perf HUD on the *editor's own* `UIRoot` instance today — this was only surfaced by directly calling `ui_root.perf_hud.toggle()` in the P2 headless demo harness (`ui_snapshot/render.rs`'s new `open_perf_hud` param), which also had to call `push_values` explicitly to get real numbers into the demo PNG (`docs/landings/EDITOR_WINDOW_UNIFICATION_P2_perf_hud_in_editor.png`).

**Fix shape** — either (a) give the editor window its own perf-HUD toggle path plus a per-frame `ws.ui_root.perf_hud.push_values(&mut ws.ui_root.tree)` call in `present_graph_editor_window` (gated on `is_visible()`, no `built` dependency needed since it's a targeted call, not the whole `update()`), if a live editor-window perf HUD is ever wanted; or (b) leave it unreachable and out of scope until that's asked for. Low priority either way — no live path exercises it.

### BUG-156 (fluidsim3d-4k-perf-regression-suspect-bug066-fix) — FluidSim3D no longer holds smooth 60FPS at 4K — HIGH, reported by Peter 2026-07-14
**Status:** OPEN — found not fixed 2026-07-14, reported by Peter during live-rig use.

**Symptom** — FluidSim3D used to run smoothly at 60FPS at 4K output resolution; it no longer does. Peter suspects this is a regression from the BUG-066 fix.

**Root cause** — unknown, not investigated this session, but the suspected culprit is well-formed: BUG-066's fix (`eebac94d`, see the closed entry in `docs/archive/BUG_BACKLOG_CLOSED.md`) resized `edge_slope_3d`/`swirl_force_3d`'s (and the other volume nodes') dispatch grids from `div_ceil(8)` (legacy 8×8×8 workgroup) to `div_ceil(4)` against the freeze codegen's actual `@workgroup_size(4,4,4)` for 3D-volume kernels. Correcting the grid size to match the real workgroup was necessary for correctness (forces were only landing in 1/8th of the volume before), but going from an 8³ to a 4³ workgroup at the same volume resolution means 8x more dispatched workgroups per volume kernel — a real, expected throughput cost of the fix, not an incidental one. Whether that 8x is actually what's landing on the frame budget (vs. some other change since, or the 4³ workgroup being genuinely suboptimal occupancy for this GPU) is unverified.

**Fix shape** — profile FluidSim3D at 4K with the profiler (`manifold-profiler`) to confirm the volume-node dispatches (`edge_slope_3d`, `swirl_force_3d`, and any other 3D-volume kernel sized off `VOLUME_WORKGROUP_3D`) are where the frame time went, comparing against a pre-`eebac94d` build if needed to isolate it from unrelated changes since. If confirmed, the fix is not "revert BUG-066" (that reintroduces the top-right-quadrant forces bug) — it's making the corrected dispatch cheap again: a larger workgroup size for the 3D volume kernels (if occupancy/shared-memory allows), or reducing per-workgroup overhead, while keeping the grid sized correctly against whatever workgroup size is actually emitted.

### BUG-155 (camera-rotation-params-missing-smooth-360-wrap) — camera orbit/tilt/rotation controls don't smooth-wrap at 360 degrees, so a full rotation can't be modulated cleanly via a saw wave — MED, reported by Peter 2026-07-14
**Status:** OPEN — found not fixed 2026-07-14, reported by Peter during live-rig use.

**Symptom** — the camera orbit, tilt, and rotation params jump/discontinue at their wrap boundary instead of wrapping smoothly through 0/360 degrees. A saw-wave LFO driving a full rotation (the standard way to drive continuous spin from a modulation source) hits the seam and snaps instead of reading as continuous rotation.

**Root cause** — unknown; not investigated. Likely candidates: the param's range/wrap handling doesn't treat 0 and 360 (or -1/1 in normalized units) as identified endpoints, or downstream consumers (orbit-to-radians conversion, Euler composition) don't handle the wrap consistently across all three rotation axes — related to the BUG-096 orbit/tilt phase investigation already on the backlog.

**Fix shape** — audit each rotation param (orbit, tilt, and any other rotation axis in the camera/orbit primitives) for wrap behavior; ensure all use a consistent smooth-wrap convention (e.g. modulo into 0..360 with identified endpoints) so a saw wave bound to the param produces a continuous, seamless spin. Cross-check against BUG-096 (rotate-sliders-jump) — may share root cause or fix site.

### BUG-153 (ui-snap-inspector-scene-172px-nondeterministic) — the `ui-snap inspector` headless scene is not run-to-run deterministic — LOW (test-determinism only, no correctness impact)
**Status:** PARKED (partial progress) 2026-07-14 (bug-wave3 lane D) — attempted, root cause not isolated within budget. Prerequisite found+fixed first: `--features ui-snapshot` didn't even compile on this tip (`view.canonical_def` changed from `EffectGraphDef` to `Arc<EffectGraphDef>` in an unrelated session, 8 call sites in `ui_snapshot/mod.rs` never updated) — fixed as BUG-162, logged separately since it's a distinct regression, not this bug. With the binary building, reproduced the exact 172-pixel/same-bbox diff on this tip. **Ruled out (verified, not guessed):** (1) the `update_fire_meters(&|_| Some(0.9), 1.0/60.0)` call the `inspector` scene makes after the base build — diff is byte-identical with that call stubbed out entirely, so it's not the meter peak-hold path. (2) `InspectorCompositePanel::motion_last_tick` being seeded to `Instant::now()` at construction, so the FIRST `update()` call measured "however long scene setup took" as its `dt_ms` instead of 0 — this was a real bug (fixed in this session, see the `motion_last_tick: Option<Instant>` change in `inspector.rs`, kept even though it didn't close this one) but had zero effect on the diff, so it isn't the source either. Both rule-outs used the actual harness re-run (`cargo run --features ui-snapshot --bin manifold -- ui-snap inspector`, twice, byte-diffed), not inference. **Still open:** the nondeterminism lives somewhere in `sync_build`/`reconcile_state`'s build path itself (structural sync → zoom → build → push-state), before either ruled-out point — candidates not yet checked: HashMap-backed registry iteration order feeding param/card construction order (would shift adjacent-row anti-aliasing at seams without moving whole rows, matching the sparse 172-pixel/full-bbox-not-solid pattern), or float-accumulation order in row-y-offset stacking. Next step: bisect by diffing the tree dump (`--dump`) between two runs to see which node's bounds/style differs, rather than guessing at the render level.

**Symptom** — `cargo run --features ui-snapshot --bin manifold -- ui-snap inspector`, run twice in a row with NO code changes in between, produces two PNGs that differ in exactly 172 pixels, always the same bounding box: x 1258-1274, y 450-854 at the scene's 1536×1216 render size — a narrow vertical band. Confirmed on unmodified `origin/main` (same 172-pixel, same-bbox diff reproduces there too) — unrelated to the P1 diff. The `timeline` and `states` scenes do NOT show this (byte-identical across repeated runs).

**Root cause** — unknown; not investigated beyond isolating it. The narrow x-band and full-height y-span suggest a scrollbar thumb, a hover-state color blend, or some other element whose color depends on a source of nondeterminism (timing, an uninitialized/stale value, float rounding on a borderline hover test) rather than the deterministic layout inputs the rest of the scene uses.

**Fix shape** — reproduce with a diff script (`PIL`/numpy pixel diff, or the repo's own `readback.rs` machinery) pointed at the bbox, then trace what draws in that rect (likely `inspector.rs`'s scroll container or a param-card element) and find the nondeterministic input. Low priority (doesn't affect the live app — only a headless test scene — and doesn't block any other test), but worth fixing before this scene is ever used as a byte-identical regression gate for something else, the way `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1 tried to use it.

### BUG-148 (verification-debt-duplicate-vd-020-id) — two unrelated `docs/VERIFICATION_DEBT.md` entries both claim ID VD-020 — LOW (tooling/bookkeeping only)
**Status:** OPEN — found 2026-07-13 landing VOLUMETRIC_LIGHT_DESIGN P1–P3, while picking a free ID for a new entry.

**Symptom** — `docs/VERIFICATION_DEBT.md` has two `### VD-020` headers: one for PARAM_STORAGE_BOUNDARIES P2's calibration-drag gesture (line ~118), one for CINEMATIC_POST P5/P6's GTAO look-pass (line ~374, added by the AO-quality lane landing `3e774a36`, 2026-07-13). Both predate this session; the collision produced no merge conflict because the two entries were appended to different, non-overlapping regions of the file, so nothing flagged it until a human/agent grepped for the ID.

**Root cause** — `VERIFICATION_DEBT.md`'s rule ("IDs are stable, never renumbered") has no machine check for uniqueness — unlike `BUG_BACKLOG.md`, which at least has `bug_status.py --check` (itself limited per BUG-134). Two concurrent landing sessions each independently computed "next free VD id" from a stale read of the file and picked the same number.

**Fix shape** — renumber one of the two (whichever is less cross-referenced elsewhere — check landing reports/memory for "VD-020" hits before choosing) and grep-audit the rest of the file for any other duplicate `### VD-NNN` headers while in there. Small mechanical fix, but do it as its own dedicated pass, not folded into an unrelated landing (this one included) since it touches two other sessions' entries. Consider adding a duplicate-ID check alongside `bug_status.py` (same class of gap as BUG-134) so this can't recur silently.

### BUG-134 (bug-status-py-tail-boundary-hides-entries-past-the-appendix) — `bug_status.py`'s parser silently excludes any `### BUG-NNN` entry written after the first appendix section, hiding a real duplicate ID and a status-derivation false positive — LOW (tooling, not runtime)
**Status:** OPEN — found 2026-07-12 during the docs(backlog) archive-split session (`docs-git-sync` worktree), auditing every `### BUG-NNN` heading in the file against what `bug_status.py --check` actually validates.

**Symptom** — `bug_status.py`'s `parse()` treats the *first* `## ` heading after `## Fixed` (currently `## Checked and safe (coverage proof)`) as the start of an unparsed `tail`, and copies everything from there to EOF verbatim without looking for further `### BUG-NNN` entries. Six entries (BUG-094, 095, 096, 097, 103, 126) were appended after that appendix by later sessions that didn't know the boundary existed, so `--check` never validates their Status line, section placement, index membership, or ID uniqueness — even though it reports "clean".

**Concretely hidden by this:**
1. **Duplicate ID** — `BUG-097` is used by two unrelated entries: "ui-snap-render-overlay-pass-uses-wrong-traversal" (FIXED, now archived to `docs/archive/BUG_BACKLOG_CLOSED.md`) and "fluidsim2d-count-dims-display" (OPEN, still in this file). The duplicate-id check (`seen[bug_id] > 1`) never saw both because one lived past `tail_i`.
2. **`derive_status()` false positive** — its `FIXED` classifier is `re.search(r"\bFIXED\b", heading.upper())`, which matches the word "FIXED" inside "found **not** **fixed**" (BUG-126's own heading: "...LOW, found not fixed 2026-07-12..."). Because BUG-126 has no explicit `**Status:` line and sat past `tail_i`, this misclassification was silent; had it been in the checked region it would have wrongly filed a still-open bug under `## Fixed`. Manually confirmed OPEN and left untouched in this session's archive-split (not moved).

**Fix shape** — two independent, small fixes: (a) extend `parse()`'s entry-scan to continue past every `## ` heading in the appendix region instead of stopping at the first one (or: require all `### BUG-NNN` entries to live within the Open/Fixed span as a `--check` invariant, and fail loudly if one is found in the tail); (b) tighten `FIXED`'s regex to exclude a preceding "not"/"never" within a few words (e.g. negative lookbehind or an explicit `NOT\s+FIXED` exclusion checked first). Neither touched this session — logged per the archive-split's own audit, out of scope for a doc-reorg change.

### BUG-118 (render-scene-fog-washes-out-instead-of-depth-grading) — atmosphere fog reads as uniform washout, not distance-graded haze — MED look-quality / render_scene
**Status:** DEFERRED — Peter, 2026-07-14: "I don't want bug-118 worked on." On hold at his call; no session touches it until he revives it. Prior state, kept for that day: root cause CHARACTERIZED by the V6 sweep (2026-07-13, VOLUMETRIC_LIGHT_DESIGN.md P1, numbers below); the shafts design (P1–P3) has now SHIPPED (2026-07-13), but whether it actually resolves this bug's original complaint is UNVERIFIED, not confirmed: P2/P3's own acceptance demos don't produce a visually legible light-driven sculpting effect (see that design's landing report and its status line) — no session has re-rendered the original Apricot Weather macro-scale repro scene with shafts on to confirm the milk-washout symptom is actually gone in practice. Do not mark this FIXED until that re-render happens and someone looks. **Absorbed-by: `docs/VOLUMETRIC_LIGHT_DESIGN.md` (2026-07-13, D4/P1–P3)**.
Verified-open-despite: VOLUMETRIC_LIGHT_DESIGN (shipped, but its acceptance demos never re-rendered the original Apricot Weather repro, and Peter froze this bug 2026-07-14 — the verify+mark step is done: still open by his call).

**Symptom** — `node.atmosphere` fog at even low density (0.04) washes out the whole
frame uniformly: near geometry loses contrast as much as far geometry, so fog reads
as a milk filter instead of depth. Seen live in Apricot Weather (macro scale, camera
distance ~9), 2026-07-11. Stopgap: fog card removed from the preset, density zeroed.

**Root cause (CONFIRMED by V6 measurement, 2026-07-13)** — `apply_fog` IS correctly
distance-scaled (`fog = 1-exp(-density·dist)`, `shaders/render_scene.wgsl:516-526`);
the washout isn't a math bug, it's saturation: the visible depth RANGE across a
bounded subject (a photoscanned plant a few world-units deep, viewed from ~9 units
away) is small relative to fog's characteristic decay length `1/density` (25 units
at density=0.04), so the fog fraction barely changes across the subject's near/far
extent — reading as uniform milk — even though the SAME density differentiates
strongly across a scene with a wide depth range (e.g. a ground plane extending to
the horizon).

Measured via `render-generator-preset` (a temporary scratch preset — grazing-angle
200×200 ground plane, orbit camera, one overhead Sun, `node.atmosphere` at
density 0.04 vs 0.0 — deleted after the sweep, not committed) at camera distances 9
and 30, `--size 640x360 --frames 3`:

| camDistance | wide-range near/far ratio-of-ratios (bottom-of-frame vs near-horizon band) | narrow-band ratio (two adjacent bands near the bottom of frame, ~subject-scale depth) |
|---|---|---|
| 9  | 1.1828 | near=0.9797, far=0.9689 → Δ=0.0108 (1.1%) |
| 30 | 1.2951 | near=0.9355, far=0.9102 → Δ=0.0253 (2.5%) |

The wide-range columns confirm the formula differentiates near vs far when the
depth range is large (far attenuates 15–30% more than near across the full scene).
The narrow-band columns are the diagnostic: across a subject-scale depth slice near
the camera, the attenuation delta is only 1–2.5 percentage points — exactly the
"reads as flat milk" signature Peter saw, and it gets WORSE (not better) at the
farther camDistance=30, because both near and far surfaces are then farther from
the camera (more overall fog) while their relative depth range stays the same.

**Fix shape** — not a fog-curve tweak (rejected, D4): the analytic exponential fog
is correct and stays. The fix is VOLUMETRIC_LIGHT_DESIGN's marched light shafts,
which read the scene per-light rather than as a single constant-color blend —
light-driven inscatter can sculpt depth on a bounded subject where a distance-only
fog curve saturates. P2/P3 land the march; this entry closes when Peter's look-pass
on the P2/P3 acceptance demo confirms the "milk" complaint is resolved.

### BUG-116 (fire-meter-display-ballistics-reads-as-low-fps) — fire meters read as updating at low FPS despite a 60fps capture/snapshot/UI pipeline — LOW (deferred by design)
**Status:** DEFERRED — Peter 2026-07-11 ("leave this slow update for now"), found while adding the producer/consumer fire-meter round-trip test (`bug/fire-meter-unification`).

**Symptom** — a card's/clip-trigger's Amount meter visibly reads as updating slowly, more like a
peak-hold VU meter than a live level bar, even though nothing downstream of the content-thread
capture is actually throttled.

**Root cause — intentional, not a bug in the pipeline itself.** The pipeline is 60fps end-to-end:
the audio-analysis worker's hop is ~5.3ms, `evaluate_all_audio_mods`/`LiveTriggerState::evaluate`
push a fresh `FireMeterCapture` every engine tick, the `ContentState` snapshot carries it to the UI
thread every tick, and `ParamCardPanel`/`AudioTriggerSection::update_fire_meters` write it into the
meter every UI tick. What the performer actually sees is display-side peak-hold smoothing added
deliberately by BUG-109 P5: `MeterIds::update` (`crates/manifold-ui/src/panels/drawer.rs` ~245-306)
snaps a rising level up instantly, then HOLDS it for `PEAK_HOLD_SECONDS = 0.25` before decaying at
`PEAK_DECAY_PER_SEC = 5.0` (full-scale 1.0→0.0 in ~200ms) — added specifically so a millisecond-
scale transient's shaped envelope (which decays faster than the UI's own tick cadence can sample)
stays visible at all between snapshots, per that fix's own comment. The tradeoff is exactly the
symptom: a steady/fast-changing signal reads as chunkier and slower than the raw 60fps capture
underneath it.

**Fix shape** — not a wiring bug, so no root-cause removal; two ballistics directions if revisited:
1. Tune the constants down (shorter `PEAK_HOLD_SECONDS`, faster `PEAK_DECAY_PER_SEC`) — trades away
   some of BUG-109 P5's "the transient stays visible" guarantee for a snappier feel.
2. The pro-audio split: an instant, unsmoothed live bar (tracks `level` every tick, no hold) plus a
   separate thin peak-hold TICK mark riding above it (only the tick keeps BUG-109 P5's hold/decay).
   Keeps both properties instead of trading one for the other, at the cost of a second draw
   primitive per meter.

**Instrument impact:** cosmetic/feel-only — the underlying signal a mod fires on is unaffected (the
edge-detector reads the raw conditioned value, never the display-smoothed one); this is purely
"does the meter look as fast as the audio is," which is why Peter deferred it rather than folding
it into the round-trip test session.

### BUG-115 (mux-multiblend-dynamic-arity-blocks-codegen-conversion) — dynamic port count can't be expressed in the static spec the codegen reads — LOW (decision recorded: stay as-is)
**Status:** DEFERRED — Peter's call, 2026-07-14: leave `multi_blend` AND `switch_texture` as dynamic-arity
fusion boundaries. The static-max conversion's runtime costs (always-8 texture samples on multi_blend's
common 2-wired case; 32 always-bound slots + loss of the 5x→1x branch-pruning short-circuit on
switch_texture) plus the loss of the shrink-to-fit editor affordance outweigh fusability here — the
current dynamic kernels are the more optimal shape for live performance. Remains a tracked codegen gap
(dynamic-arity support), NOT a de-facto exemption from the codegen mandate; revisit only if dynamic-arity
codegen support is designed for real or the fusion-boundary cost shows up on the rig. Spike evidence below
kept as the record.

**Symptom** — `node.switch_texture` (mux_texture, 5 shipped presets — and mid-chain by its
nature, it selects between texture chains) and `node.multi_blend` are fusion boundaries.

**Root cause** — both have a dynamic port list: `num_inputs` rebuilds the ports per instance, and
multi_blend synthesizes its WGSL for N inputs at runtime (multi_blend.rs ~124). The freeze
codegen reads a static `PrimitiveSpec` (`standalone_for_spec::<Self>()` is type-level), which
can't express variable arity.

**Fix shape** — half-day spike first: convert at a fixed max arity (declare the max as optional
`Coincident` texture inputs; the texture-region machinery already folds unwired optional
coincident inputs as `0u` use flags per FREEZE_COMPILER_MAP §4 region gates), body selects/sums
over the wired flags. If the spike shows dynamic ports can't square with the static spec, growing
dynamic-arity codegen support becomes a design decision for Peter — flag it, don't improvise.

**Spike (2026-07-14)** — done, half-day scope, no landing. Evidence:
`crates/manifold-renderer/tests/bug115_dynamic_arity_spike.rs` (two `#[ignore]`d tests, run with
`cargo test -p manifold-renderer --test bug115_dynamic_arity_spike -- --ignored --nocapture`).

*Spike verdict: yes-with-caveats.* The static-max-arity + optional-`Coincident` + `0u` use-flag
shape is not a new mechanism to invent — it already ships in production for `node.pack_rgba`
(`pack_channels.rs`: 4 always-present optional `Coincident` texture inputs, a `use_<name>: u32`
flag per input injected into the uniform, body falls back to `default_*` when unwired). The spike
built a throwaway 8-input `MultiInputCoincident` spec (mirroring `multi_blend`'s sum-not-pack
semantics) directly against `generate_standalone` — bypassing the `primitive!` macro so no fake
node registers in the palette — and confirmed: (a) `generate_standalone` accepts it and emits
valid WGSL naga parses cleanly; (b) region-fusion already admits this shape into a region without
new work — `region.rs`'s own comment at the unwired-optional branch says explicitly "this is what
lets `pack_channels` fuse with only r/g wired," so multi_blend/mux converted this way would fuse
exactly like pack_rgba does today, no region.rs changes needed.

**The real tradeoff for Peter's call:**
- **Perf, measured, not asserted.** The codegen wrapper pre-reads every DECLARED texture input
  unconditionally (`textureSampleLevel` per input, before the body's `if use_N != 0u` gate even
  runs) — this is separate from the use-flag folding, which only gates the *contribution*, not the
  *sample*. The spike's generated 8-input kernel has exactly 8 `textureSampleLevel` calls and 11
  `@binding` declarations (1 uniform + 1 sampler + 8 textures + 1 output) — always, regardless of
  how many of the 8 are actually wired. Today's `multi_blend::shader_for(k)` samples exactly `k`
  textures and binds exactly `2+k+1` slots. A live-show preset with 2 wired inputs (the common
  case per multi_blend.rs's own doc comment — "the summing shader is generated for the number of
  *wired* inputs... so a 2-input blend... compiles a tight kernel with no dead taps") would go from
  2 samples/dispatch today to 8 samples/dispatch always-8 — a 4x texture-sample increase with no
  possible naga/backend DCE rescue, since the use-flag is a runtime uniform value, not a
  compile-time constant, so the sample call can never be proven dead. `switch_texture`'s case is
  worse in shape (not degree, since it already short-circuits to 1 dispatch via
  `selected_input_branch`): its `MAX_INPUTS = 32`, so a static-max conversion would mean 32 always-
  bound texture slots per instance — the perf hit isn't sampling (mux only reads the selected one
  per the executor's branch-pruning) but the sheer binding-table size and uniform layout, and 32
  static ports is a much bigger vocabulary jump than multi_blend's 8.
- **UI/editor implications — a real blocker, not a nuisance.** The blanket `EffectNode` impl for
  any `Primitive` (`primitive.rs` line ~549) reads `inputs()` straight off `P::INPUTS`, a
  compile-time `&'static` const array — there is no per-instance override hook once a node goes
  through the `primitive!` macro / `Primitive` trait. `MultiBlend`/`MuxTexture` currently hand-roll
  `EffectNode` precisely so `reconfigure()` can mutate the live `inputs: Vec<NodeInput>` field and
  visually grow/shrink the node in the editor as `num_inputs` changes. Converting to the codegen
  path via `PrimitiveSpec::INPUTS` freezes that list at compile time — `standalone_for_spec::<Self>()`
  always reads the same 8 (or 32) ports. It's *possible* to keep a hand-rolled `EffectNode` for the
  live authoring surface while separately implementing `PrimitiveSpec` for codegen (nothing enforces
  the two agree), but that means hand-maintaining two divergent pictures of the node's shape — the
  editor's dynamic list and codegen's static 8 — which is exactly the kind of invariant this
  codebase avoids elsewhere. The `pack_rgba` precedent doesn't have this problem because it never
  had a dynamic port count to begin with: its 4 ports are always visible, wired or not. Converting
  multi_blend/switch_texture this way means giving up the "the node shrinks as I dial down
  num_inputs" editor affordance and replacing it with "the node always shows all 8 (or 32) sockets,
  wire only the ones you need" — a real UX change for an authoring surface Peter uses live.
- **Codegen-side complexity if pursued for real:** low. No changes needed to `region.rs` or the
  fusion-admission logic — the unwired-optional-Coincident path is already generalized and proven
  by `pack_rgba`. The work is entirely in the primitive files: rewrite `multi_blend.rs` off its
  hand-rolled `EffectNode` (dynamic ports, runtime `shader_for(k)`) onto the `primitive!` macro
  with 8 static optional Coincident inputs + a `wgsl_body` fragment, prove generated-vs-hand parity
  (mirroring the `pack_channels`/`trig_texture` parity tests in `codegen.rs`'s test module), and
  decide the num_inputs/UI question above. `switch_texture`'s conversion is harder in a different
  way — its `selected_input_branch` executor-level branch-pruning optimization (5-mode case: 5x → 1x
  render cost) has no equivalent in the static-Coincident-sum shape; a mux converted this way would
  need a different body strategy (a `select`/switch chain over 32 pre-read texture samples, which
  reintroduces the "sample all N always" cost multiplied by mux's much higher `MAX_INPUTS`) or would
  lose the short-circuit optimization entirely. That makes `switch_texture` a materially different,
  harder problem than `multi_blend`, not just a bigger version of the same one.

**Recommendation (spike input, not a decision):** worth doing for `multi_blend` if Peter accepts
the UX tradeoff (always-8 static ports, no more shrink-to-fit) and the preset(s) using it don't hit
the common 2–3-wired case hard enough for a 4x sample-count increase to matter on stage — the
codegen-side work is small and the mechanism is already proven in production via `pack_rgba`.
`switch_texture` is a harder call: its dynamic short-circuit (5x → 1x via branch pruning) is a real
perf win today that a naive static conversion would give up, and its `MAX_INPUTS = 32` makes the
always-bound-N cost much larger than multi_blend's 8 — recommend treating it as a separate, later
decision from multi_blend's, not bundled into the same conversion.

### BUG-107 (text-rasterizer-draws-fallback-glyph-ids-with-base-font) — any character the UI font lacks renders as a wrong real glyph (mangled "ủ"-style symbols) — MED
**Status:** PARTIAL — layer 1 (correctness) FIXED 2026-07-11 (`bug/wave2-lane-a-cardui` 1d9dba9c). Layer 2 (prevention: PUA atlas extension + a static lint) remains OPEN — see below. Reported by Peter 2026-07-10 (screenshots of mangled prefix glyphs on row labels; likely the graph canvas's D6 "↳ <outer label>" mirror rows from the gltfeditor scene).

**Symptom:** UI strings containing a character outside the base font's coverage draw a real-but-wrong glyph — e.g. an "ủ"-like glyph where "↳" was intended. This is a class, not one string: agents keep writing raw Unicode symbols into UI text, and the current non-ASCII inventory in `manifold-ui` string literals includes ↳ → ← › − — … (find them with `rg '[^\x00-\x7F]'` over string literals).

**Root cause — CORRECTED against current main (the pinned file was the wrong one for this symptom):** the backlog entry pinned `TextRasterizer::shape_line` (`crates/manifold-renderer/src/text_rasterizer.rs`) — that file DOES have the described bug, but it's the Text-generator's own content-pipeline rasterizer (in-scene rendered text), and is never called from the graph canvas / inspector row-label path the screenshots actually showed. The reported symptom renders through a completely separate, independent glyph pipeline: `native_text.rs`'s UI-chrome atlas (`crates/manifold-renderer/src/native_text.rs`), which has the IDENTICAL bug pattern in its own `shape_line` + `rasterize_glyph`: CoreText's `CTLine::glyph_runs()` was flattened into one glyph-id list, discarding each run's own resolved font; when the base font (embedded Inter) lacks a character (e.g. the section header's "▾"/"▸" disclosure triangle, U+25BE/U+25B8 — a geometric shape Inter doesn't cover), CoreText splits a fallback run whose glyph ids index a DIFFERENT font's glyph table, and the atlas rasterized every id with the base Inter font anyway (`GlyphKey` carried no font identity, only `(glyph_id, size, weight)`) — an arbitrary wrong glyph, deterministic for every uncovered character. Confirmed by rendering the real `gltfeditor` ui-snap fixture before/after (see Verified).

**Fix (layer 1 — correctness, both files):**
1. `text_rasterizer.rs`: `shape_line` now returns per-run `GlyphRun{font, glyphs, positions}` (reading each run's own `kCTFontAttributeName`) instead of a flat `(Vec<u16>, Vec<CGPoint>)`; each run draws with its own resolved `CTFont`.
2. `native_text.rs`: same per-run split (`ShapedRun`), plus the glyph atlas's `GlyphKey` gained a `GlyphFont{Base(weight)|Fallback(hash)}` discriminant so a fallback run's glyph ids can never collide in the atlas cache with a same-numbered glyph id from Inter — `FontManager` interns fallback `CTFont`s by a hash of their PostScript name (a stable identity; a `CTFont` pointer isn't).

**Layer 2 (prevention) — still OPEN, deliberately not attempted this session:**
- Extending the PUA icon atlas (`crates/manifold-ui/src/icons.rs`) with ↳/chevrons/arrows and a check-time lint over `manifold-ui` string literals against a declared coverage set is real follow-up work — curating the actual icon set and writing the lint is its own scoped task, not a corner to cut inside a 3-bug wave session.
- A blanket **runtime** `debug_assert!` on "this line produced a fallback run" was in the brief's fix shape as an alternative but is the WRONG mechanism here on inspection: both rasterizers draw live USER data too (layer names, clip names, generator text-param values), not just hardcoded UI chrome strings — a project with a non-ASCII layer name (Japanese, emoji, accented Latin outside Inter) would hit a real CoreText fallback legitimately, and panicking on that would crash the live rig on ordinary user content, not just catch an agent's stray Unicode literal. The rasterizer has no way to distinguish "hardcoded chrome string" from "user data" at that layer. The check-time STATIC lint (source-level, over `manifold-ui`'s own literals only, never touching runtime text) is the correct shape for the class-kill described; a runtime assert is not, and shipping one to check a box would trade this bug for a live crash-on-user-content bug — worse than the mojibake.

**Instrument impact:** authoring-surface legibility today (graph canvas rows, now fixed) and Text-generator content (perform surface, also fixed) — the class is unbounded until layer 2 lands; any future agent-authored raw-Unicode literal can still ship mojibake (rendered correctly now, but still visually wrong if it's an unintended fallback), which is why layer 2 stays open.
**Verified:** `cargo test -p manifold-renderer --lib` (1050 passed) and `-p manifold-ui --lib` (670 passed); clippy clean. Visual: rendered the real `gltfeditor` ui-snap fixture's section headers before/after — before the fix, the "▾" triangle renders as a mangled glyph prefixing "QS1694-W02-1-1"/"Material.001"; after, a clean triangle. Confirmed by temporarily reverting `native_text.rs` and re-rendering.
**Still owed to Peter's own eyes:** look at any UI surface carrying real non-ASCII user content (a layer/clip name with an emoji or CJK text) live — the fixtures only exercise the section-header triangle and the app's own hardcoded symbol literals, not the user-data path, though the fix is structural (per-run font, not per-string) so it should generalize.

### ~~BUG-102~~ FIXED (mapping-popover-has-no-text-input-surface) — the calibration popover can't render an editable text field for `label` or the new `section` — LOW
**Status:** FIXED 2026-07-13 (Sonnet, UI_WIDGET_UNIFICATION P5c) — `MappingPopover` now embeds `crate::text_edit::TextEditModel` (P5a) in place of the old `edit_buffer: String`; `EditField::Label` and a new `EditField::Section` both route through it — click places the caret (pre-selected on entry, D16), click-drag selects a range (`byte_at_field_x`/`active_field_geometry`, hit-tested via the same `draw::text_width` the row renders with, so hit-testing and drawing never disagree), typing replaces the selection, Enter/blur commits (a press on a DIFFERENT popover field blur-commits the one being edited, D16), Esc cancels. `EffectMappingSection` (the P3-shipped write path) is now reachable: committing an empty Section buffer clears the field back to unsectioned (`Option<String>` outer=touched/inner=value-or-clear). `resolve_canvas_binding`/`open_mapping_popover`/`MappingPopover::open` thread the binding's current `section` through so the popover opens pre-seeded. 17 unit tests (11 pre-existing re-verified green + 6 new: Section type/clear, pre-selected-seed replace-on-type, in-field-click repositions the caret without resetting the buffer, in-field drag grows a selection, cross-field blur-commit). Module doc's stale "label is read-only" paragraph rewritten. Gates: `manifold-ui --lib` 741/741, clippy -D warnings clean, `rg -n 'edit_buffer' crates/manifold-ui/src` → zero (I7). One gate-wording note: the doc's I7 gate also expected `rg -n 'fn insert_char|fn backspace' crates/manifold-ui/src crates/manifold-app/src` to hit ONLY `text_edit.rs`, but `manifold-app/src/text_input.rs` (P5b, already on main) keeps thin same-named wrapper methods that one-line-delegate to the model — correct encapsulation for its ~76 call sites, but the grep's literal pattern doesn't distinguish a wrapper from a reimplementation, so it shows extra hits there. The underlying invariant (one place owns the actual mutation logic) holds; only the grep pattern doesn't fully rule out method-name reuse.

**Symptom:** `crates/manifold-ui/src/graph_canvas/mapping_popover.rs`'s own module doc (line ~24) says label editing is "intentionally deferred: a real text field on the immediate-mode canvas would need caret/selection/IME handling that doesn't exist on this surface yet" — the label is shown read-only in the popover header, and `EditField::Label` is unused groundwork waiting for that surface. P3 needed the SAME kind of text field for the new `section` property and hit the identical wall: there is nowhere on this popover today that accepts typed text for any string field.

**Root cause:** `MappingPopover` draws via `Painter` immediate-mode primitives (no `UITree`), and the host app never built caret/selection/IME handling for that draw model — a structural gap in the popover surface itself, pre-dating P3.

**What P3 shipped anyway:** the write path is real and tested at the command layer — `BindingMappingEdit::section: Option<Option<String>>` (outer = touched, inner = new value/clear), `EditParamMappingCommand::execute`/`undo` apply/restore it on the manifest spec only (BOUNDARIES D4), and `PanelAction::EffectMappingSection { binding_id, section }` + its `app_render.rs` dispatch arm route it end-to-end. Any future caller (a different surface, or this popover once text input exists) can reach it today.

**Fix shape:** build the caret/selection text-input primitive once (`TextEditModel`, no IME — see Status), host it in `MappingPopover` (shared by `label` + `section` + any future string field), then wire both `EditField::Label` and a new `EditField::Section` through it — the committed spec is UI_WIDGET_UNIFICATION P5a (model) + P5c (this popover). LOW severity — no live gesture is broken by its absence (section can still be seeded by expose + the rename-sweep; it just can't be hand-typed from this popover yet), but it's the second deliverable now blocked on the same missing primitive.

### BUG-080 (param-manifest-construction-not-a-unified-safe-gate) — manifest construction has no single safe gate; "partially built" is an observable state — MED (design-quality / latent-robustness; wants an Opus design pass)
**Status:** OPEN — design WRITTEN 2026-07-14: `docs/PARAM_MANIFEST_GATE_DESIGN.md` (Fable, same-day session — supersedes the "dedicated Opus session" plan settled 2026-07-11; Peter asked for the design now so the bug wave can execute it). Executes as P1 of that doc inside bug-wave lane B. Still not a patch-in-a-sweep item — the design doc is the brief; do not fix this outside it.

The param manifest (an instance's live knob list) is built at deserialize AND rebuilt by a later `reconcile_param_manifests` pass, because deserialize can't see project-embedded presets yet. Consumers that read `.params` *between* the two — a direct `serde_json::from_str::<PresetInstance>`, the keep-don't-drop backstop, the legacy audio-trigger migration, ~18 tests — depend on the deserialize-time build being correct. It works today only because the double-build papers over the timing; it's a latent hazard: a future load path added without a reconcile silently inherits an empty/partial manifest (the BUG-036 class). Root cause: manifest construction has no single safe gate — "partially built" is an observable, readable state. **Fix shape (design pass, NOT a patch):** make a half-built manifest un-observable — one construction gate every load/paste/bare-read passes through, OR a type-state where params can't be read until reconciled, OR deserialize carries enough context to build complete in one shot. The naive "build once in reconcile" was tried 2026-07-09 and is unsafe for exactly those reasons (design doc §2 D1 priced + rejected it).

### BUG-069 (shipping-license-audit) — four license problems in shipped components — HIGH at commercialization, latent until then
**Status:** OPEN — PARTIALLY RESOLVED 2026-07-17: madmom beat/downbeat/tempo model arms DELETED (AUDIO_ANALYSIS_ACCURACY P2, Beat This MIT code+weights verified at fetch). Remaining: madmom CNN onsets (P6), ADTOF (active bake-off — P3 baselines committed), rusty_link, ffmpeg staging.

**Found 2026-07-08 (Fable, audio-analysis design session; full sweep same day — Python
runtime deps read from `requirements.runtime.mac.txt`, all Rust crate licenses swept via
`cargo metadata`, staging script read).** Peter's ruling, verbatim: *"Using for dev only
isn't good enough, what we build is what the users should have"* — every item below is
release-gating for the commercial cut, not optional.

1. **madmom model files — CC BY-NC-SA 4.0** (source is BSD; models say "commercial use
   requires contacting Gerhard Widmer"). Shipped via `bpm.py` / `onset_detection.py`.
   Fix in flight: AUDIO_ANALYSIS_ACCURACY P2 (Beat This, MIT code+weights) + P6 (full
   madmom removal), both with `rg 'madmom'` zero-hit deletion gates.
2. **ADTOF — CC BY-NC-SA 4.0** (code + model; we ship the `adtof-pytorch` port, which
   inherits it). Drum stage of the pipeline. **Peter's direction (2026-07-08): do NOT
   email Zehren yet** — replace instead. Full two-stage approach (DSP stem detectors
   now, own trained drum-stem model to compete — trained on demucs-separated permissive
   data, weights ours) is captured in AUDIO_ANALYSIS_ACCURACY_DESIGN Deferred #1;
   trigger = commercialization v1.0 gate or drum work resuming. Fresh off-the-shelf
   search when work starts (Magenta E-GMD model / Omnizart were the permissive options
   as of 2026-07, both mid).
3. **rusty_link 0.4 — GPL-2.0-or-later** (`crates/manifold-playback/Cargo.toml:17`,
   used by `link_sync.rs`). GPL is viral for a closed-source binary — this is the only
   non-permissive crate in the whole Rust tree. Ableton Link itself is dual-licensed
   and Peter's proprietary Link license is already pending (competitive-steal-pass),
   but that grant covers Link, NOT the community GPL Rust wrapper. Fix (Peter's
   direction 2026-07-08): once the Ableton proprietary Link license lands, write a
   thin clean-room FFI binding over Ableton's official `abl_link` C wrapper (~a day;
   `link_sync.rs` is the only consumer). **Never copy rusty_link source — copying GPL
   code inherits GPL.** Do not reimplement the Link network protocol; the licensed
   library carries it.
4. **Staged ffmpeg is whatever the dev machine has** (`stage_runtime_mac.sh:253–273`
   resolves `command -v ffmpeg` — a Homebrew build, i.e. `--enable-gpl`). Fix: stage a
   deliberate LGPL-configured decode-only ffmpeg build (the sidecar only decodes) and
   pin its source/offer per LGPL. The future app-side FFmpeg door (MEDIA_BACKEND)
   must pin the same constraint.

**Clean (verified 2026-07-08):** torch/torchaudio (BSD-3), numpy (BSD), demucs (MIT),
basic_pitch (Apache-2.0), librosa (ISC), soundfile (BSD), pretty_midi (MIT); every
other Rust crate is permissive (r-efi is MIT-or-Apache-or-LGPL — choose MIT). Minor
watch: the `lameenc.py` shim / LAME (LGPL — fine as subprocess/dylib, patents expired);
demucs htdemucs weight file license (⚠ verify at commercialization review). Datasets
are NOT affected — eval-only, never bundled.

COMMERCIALIZATION_DESIGN's license review must consume this entry wholesale.

### BUG-063 (silent-load-repairs) — load-time repairs delete project data with log-only notice — MED-HIGH (silent data alteration; compounds BUG-062)
**Status:** PARTIAL

**Visibility shipped 2026-07-09 — PROJECT_FILE_INTEGRITY P3 (@ 05247ab1).** Load-time repairs
(unknown-effect strip, overlap-repair, orphan purge, missing-media) now accumulate a
`LoadReport` (a `#[serde(skip)]` transient field on `Project`) and, when non-empty, raise a
**non-blocking toast** naming what changed ("Opened with repairs: 1 unknown effect removed,
1 overlapping clip repaired"). The *silent* half of the bug — the core complaint — is closed.
**Still open (PARTIAL):** the heavier rescue path from the original fix shape — a *blocking*
acknowledge dialog AND journaling the pre-repair `project.json` into `history/` as a labeled
"before load repair" snapshot so the original is one restore away. Consciously deferred (design
Deferred §6); revival trigger: a repair found to drop data a user wanted back.

**Found 2026-07-07 by the PROJECT_IO_MAP read (§9 E2).** Three load steps mutate the project
destructively and report only to the log: `repair_overlapping_clips` (loader.rs:282) removes
the shorter clip of every overlapping pair, `purge_orphaned_references` removes clips and
MIDI mappings, `strip_unknown_effects` drops whole effects. The user believes they opened
the file they saved; the next save persists the altered state and the pre-repair original
ages out of the 50-autosave history cap. **Fix shape:** aggregate a `LoadRepairReport`
across the pipeline; any nonzero count raises a dialog naming what changed, and the
pre-repair `project.json` gets journaled into `history/` as a labeled snapshot ("before load
repair") so the original is one restore away.

**Correction 2026-07-09 (verified against the code, after conflating two mechanisms in chat).**
A **missing media file on disk does NOT remove any clip** — that was wrong when stated. Two
distinct things:
- `validate_clips` ([video.rs:118](../crates/manifold-core/src/video.rs#L118)) checks whether
  each clip's `file_path` exists on disk; a missing file is only **logged as a warning**
  ([loader.rs:207](../crates/manifold-io/src/loader.rs#L207)). Nothing is deleted. Move a project,
  break the paths → every clip stays put.
- `purge_orphaned_references` ([project.rs:1468](../crates/manifold-core/src/project.rs#L1468))
  removes a timeline clip only when its `video_clip_id` is **absent from the project's video
  library entirely** — a dangling internal reference, not a missing file. A clip whose file is
  missing on disk still has its library entry, so its id stays valid and the clip is kept. Purge
  fires only on structurally broken state normal authoring can't produce.
So the only load-time repairs that remove real content are `repair_overlapping_clips` (drops the
shorter of two overlapping clips — can't happen on projects saved by current builds, overlap being
a write-time invariant) and this dangling-reference purge. Peter's hard requirement — "missing
media must never delete a clip" — is **already the behavior**; the rescue-path priority drops
accordingly (a *relink* prompt for missing media would be the higher-value follow-up if any).

### BUG-053 (hdr-live-recording-structural) — HDR live recording cannot work: pool format mismatches the native pixel buffer, and nothing PQ-encodes — LOW today (UI can't reach it), blocks any HDR-capture ambition
**Status:** PARKED 2026-07-14 (bug-wave3 lane C) — root-fix contract routes this to Peter, not
code, since it's a capability/roadmap call ("does live HDR capture matter for the rig") not just
an engineering tradeoff. Fable re-audit this session found the picture is cheaper than the
original 2026-07-07 framing assumed:

**Root cause reconfirmed against current code, unchanged:** pool still unconditionally
`Bgra8Unorm` (`session.rs:105`), converter still unconditionally linear→sRGB
(`format_converter.rs`), native blit dest is `RGBA16Float` when `isHDR`
(`LiveRecordingPlugin.m:395`) — format-mismatched blit, first HDR frame dies. Guard test
`hdr_blocked_by_bug_053` (`tests/recording_proofs.rs:583`) pins it; `recording_soak.rs:61` also
hard-blocks `--hdr`.

**What's changed since the original framing:** the "PQ-encode compute stage or linear handoff"
choice the original entry left open ("decide at design time") turns out to be a false choice —
linear handoff isn't viable (AVFoundation's HEVC HDR delivery has no linear transfer tag; the
writer config demands PQ or HLG). More importantly, **a finished, shipping PQ encoder already
exists**: `manifold-renderer/src/pq_encoder.rs` + `linear_to_pq_compute.wgsl`, used today by the
OFFLINE HDR export path (`content_pipeline.rs:3216`'s `pq_encode_for_export`, paper_white=200,
max_nits=10000), and the native plugin already tags HDR output correctly (BT.2020 + ST.2084,
`LiveRecordingPlugin.m:218-219`). The only missing piece for LIVE recording is
`content_pipeline.rs`'s live-capture block (~line 2668) dispatching the existing `FormatConverter`
unconditionally instead of `PqEncoder` when `config.hdr`, plus the pool format following the
flag. So this is no longer "design a PQ pipeline" — it's "wire an existing, proven component into
a second call site," a small mechanical change, not a design project.

**The actual decision left for Peter:** given the fix is now cheap, is live HDR capture worth
building at all — does the rig need it, on what timeline? If yes: wire `PqEncoder` into live
recording (reuse export's nits constants), flip `hdr_blocked_by_bug_053` to the HDR twin of
`nominal_video_only` as the acceptance test. Note for whoever picks this up: the PQ shader
encodes without a BT.709→2020 gamut matrix (slightly oversaturated under a 2020 tag) — a
pre-existing gap shared with export, not new to this fix, worth flagging separately. If no: this
entry can be downgraded to DEFERRED or the HDR flag path removed outright as dead code.
Recommendation (engineering, not product): (a), wiring the existing encoder — but the yes/no is
Peter's, not mine to improvise.


**Found 2026-07-07 by Fable during the LIVE_RECORDING_PROOFS design audit (statically
derived, not yet observed — no runtime repro attempted).** The recording texture pool is
unconditionally `Bgra8Unorm` (`crates/manifold-recording/src/session.rs:60`, comment
says "format conversion done in content thread"), but the native HDR path wraps its
CVPixelBuffer as `RGBA16Float` and blits pool → buffer
(`crates/manifold-recording/native/LiveRecordingPlugin.m:378`); Metal forbids blits
between 4-byte and 8-byte texel formats, so the first HDR frame should fail with
`LR_ERR_BLIT_FAILED`. Independently, the HDR writer config declares PQ/BT.2020 but no
stage in the pipeline applies a PQ transfer (the only converter is linear→sRGB,
`format_converter.rs`) — so even with matching formats the file would carry linear
values labeled PQ. Effectively the HDR path was never finished. **Stage impact today:
none** — the UI always records SDR (`app_render.rs:1257` uses `default_to_desktop()`,
hdr=false, and never sets the flag). **Fix shape:** pool format and converter must
follow `config.hdr` (Rgba16Float pool, PQ-encode compute stage or handoff of linear
values with correct color tagging — decide at design time), then replace the
`hdr_blocked_by_bug_053` guard test with the HDR twin of `nominal_video_only`, which is
this bug's acceptance test. See `docs/LIVE_RECORDING_PROOFS_DESIGN.md` §2 D7.

### BUG-050 (ableton-anchor-yankback) — Play-from-cursor: Ableton repeatedly snaps back to the gesture beat, then MANIFOLD clock-dragged after retries exhaust — HIGH (live transport; partial fix landed 2026-07-07, rig confirmation owed)
**Status:** PARTIAL

**Found 2026-07-07 by Peter, first L4 run of the ABLETON_TRANSPORT_SYNC wave (checklist
step 1).** Symptom: press play in MANIFOLD; Ableton keeps snapping back to the gesture
position (~once per retry interval); MANIFOLD's playhead holds for a few seconds (the
pending suppression working as designed), then snaps back when retries exhaust and MIDI
clock reasserts. Root defect (proven, fixed): the pending expectation froze its target
beat — the ack was a point match against a position both engines run away from, and
every retransmit re-seeked Ableton back to the stale anchor
(`transport_sync.rs`, fixed by the moving-anchor amendment — design doc deviation 5;
regression: `t5b`/`t7b` red pre-fix, `f8` pins the property). **Still open:** WHY acks
starved across several retries on the real rig — retry queries (`get/is_playing` +
`get/current_song_time`) should have acked by retry 1 even pre-fix, and the harness
cannot reproduce the starvation (its fake acks too fast in every plausible
configuration; see f8's honesty note). Suspects, unranked: real listener/query reply
latency under load; a reply-routing gap only manifesting live; beat-space offset
between MANIFOLD's timeline and Live's song time in Peter's set. **Oracle:** the
`[ABL-SYNC]` info logs added with the fix — gesture/retry/ack/degrade each dump the
observed snapshot (playing/song_time+age/tempo, or UNOBSERVED). One play-from-cursor
on the rig answers it. **Escaped:** ABLETON_TRANSPORT_SYNC wave, P2 stage — the
harness's FakeAbleton was fixture-overfit (instant first listener report, atomic
play+seek apply, prompt query replies); no scenario modeled a starved ack channel.

### BUG-045 (gap-ring-down-chase) — Tracker chases the transform's kernel ring-down during inter-note gaps — LOW (2.4 points on the notes gate; real-clip impact small)
**Status:** PARKED 2026-07-14 (bug-wave3 lane D, 1-attempt timebox per session brief) — reproduced and confirmed unchanged: `cargo run -p manifold-audio --example mod_harness --release -- --selftest` still reads `P2c notes: pct_sounding_hops_within_1st_of_gt_post_acquisition=87.6481 (gate >= 90) FAIL`, exact match to this entry (no drift since 2026-07-06). Did NOT attempt the two named do-not-retry tunings (already swept, confirmed dead ends), and did not attempt the untried value-trend discriminator either — the entry itself declines that direction as "knife-edge" (a new tuned constant between two distributions with only ~2x separation, where a genuine musical fade-out sits on the wrong side of it) needing either "a plateau-demonstrated sweep on real material or a smarter shape," neither of which fits a 1-attempt timebox without risking exactly the untested-magic-constant anti-pattern the entry warns against. No code changed.

**Found 2026-07-06 while fixing BUG-042** (its remaining accuracy misses after the
re-acquire-window fix). After every note release, the VQT's kernel memory presents a
DESCENDING salience artifact (energy decays slower in lower/longer kernels, so the
apex slides down: measured 149→144→133→118→100 Hz over ~6 hops on `notes`). The
early part of that slide moves at ≤ MAX_SLEW bins/hop, so continuation legitimately
follows it 2–4 bins down during the gap; the next attack then starts ~1–4 st low
until the onset re-acquire window rescues (~5 hops). Two partial guards shipped with
BUG-042: super-slew+moving continuation candidates are refused (hold instead of
clamp-chase), and a static super-slew peak in the MAX_SLEW..SLEW_RADIUS dead zone is
snapped to (tremolo-trough recovery). What remains is the sub-slew early chase.

**Oracle:** `P2c notes` accuracy line (87.6% vs gate 90 — the only known-failing
selftest line). **Fix direction (untried):** a value-trend discriminator —
ring-down decays ~0.90/hop at kernel rate while tremolo decays ~0.985/hop and a
real glide holds value — but that bar is a NEW tuned constant between two measured
distributions with ~2× separation, and a genuine fade-out slide (musical) sits on
the wrong side of it. Declined this session as knife-edge; needs either a
plateau-demonstrated sweep on real material or a smarter shape. Do NOT re-try:
raising SETTLE_STREAK (swept 2/3/4 — 69.2/87.6/86.1, K=3 is the plateau), or
re-clamping super-slew continuation (resurrects the 7-st gap-chase).

### BUG-037 (glp-first-render-stall) — First render of a glTF scene layer stalls the content thread ~37ms (warm-up on the frame, not at load) — MED
**Status:** PARTIAL, further reduced 2026-07-14 (bug-wave lane B) — `render_scene`'s and `gltf_texture_source`'s GPU pipeline compiles (both asset-independent, fixed shader source) are prewarmed at app startup via `GeneratorRegistry::prewarm_all` (`dea66221`). BUG-146 (landed after this entry was first written, not previously cross-referenced here — supersession-sweep gap, now closed) added `prewarm_all_atom_codegen_pipelines`, sweeping all ~144 codegen-path atoms and independently cutting BlossomField's frame 0 to ~95ms. This session found and closed the next-largest remaining gap: `node.scatter_on_mesh` (a barriered multi-pass scan/reduce, exempt from the codegen path per CLAUDE.md, so BUG-146's sweep never reached its three hand-written pipelines) was still compiling `area_main`/`scan_main`/`place_main` lazily on first `run()` — confirmed the dominant remaining cost via `freeze-profile attribute BlossomField` (steady-state: `node.render_scene` 80.8%, `node.scatter_on_mesh` 15.0% of frame time). Fix: `ScatterOnMesh::prewarm_pipelines` (`crates/manifold-renderer/src/node_graph/primitives/scatter_on_mesh.rs`), wired into `GeneratorRegistry::prewarm_all` alongside the other two. The shadow-pass pipeline (`ensure_shadow_pass`) was *already* covered by `RenderScene::prewarm_pipelines` from an earlier VOLUMETRIC_LIGHT_DESIGN P3 pass — the backlog's "remaining gap" list above was stale on that item. Fresh `MANIFOLD_RENDER_TRACE=1` frame-0 measurement on BlossomField: **95.1ms → 40.6ms**. Full chain: 308.5ms (original) → 194.5ms (P1) → 95.1ms (BUG-146) → 40.6ms (this session). Still over the 20ms bar. **Remaining gap:** `push_mesh`/`mesh_edges`/`gltf_mesh_source` were not contributors on this trace (mesh_edges isn't even in this preset's graph); the per-asset mesh/texture buffer upload (`gltf_mesh_source.rs`, `gltf_texture_source.rs`) is genuinely per-asset and already backgrounds via a spawned thread — only amortizable via an "arm before play" phase, not a startup prewarm. Test: `scatter_on_mesh::gpu_tests::prewarm_pipelines_populates_the_shared_compute_cache` (order-independent per the BUG-144 cross-test-ordering class).

**Symptom** — trace run 2026-07-06 (`meshImportTests.manifold`): the first frame after the
project's glp layer became active showed `generators=37.1ms` (RENDER_TRACE frame=421) —
one-off, distinct from the recurring BUG-035 spike. On stage this means launching a glp
clip mid-set drops ~2 frames on its first render.

**Root cause (probable, unmeasured beyond the one trace line)** — first-touch work in the
generator path: glTF texture decode hand-off / mesh buffer upload / pipeline+PSO creation
happens lazily on the first rendered frame instead of at load/schedule time. The repo
already has the machinery pattern for this class (`plugin_prewarm.rs`, generator pipeline
pre-warm at startup, pipeline archive).

**Fix shape** — pre-warm at project-load / clip-schedule time: when a glp generator clip
is loaded (or armed on a timeline), run its first-frame resource creation off the hot
path so frame 1 of the clip renders at steady-state cost. Verify with the same
MANIFOLD_RENDER_TRACE run: no >20ms frame on first clip render.

### BUG-034 — Headless preview verification doesn't cover the live atlas UV path — LOW (test-coverage gap, follow-up to BUG-027)
**Status:** PARTIAL 2026-07-14 (bug-wave3 lane D) — did step (1) of the entry's own fix shape: factored the atlas-cell-UV math out of `app_render.rs`'s inline block into `content_pipeline::atlas_cell_uv(cell, monitor_aspect) -> [f32; 4]`, a pure function, with 3 unit tests (square-aspect no-letterboxing, cell-index grid-position decomposition, wide-monitor vertical letterboxing) — `cargo test -p manifold-app --bin manifold atlas_cell_uv` 3 passed. `app_render.rs`'s live call site now calls the shared helper instead of duplicating the math inline. Step (2) — building the synthetic-atlas harness scene that packs per-node textures + a matching `node_atlas_layout` and drives previews through this helper for a whole-graph PNG proof — NOT done this session (real harness-authoring work, timeboxed out rather than rushed). The math itself is unit-tested now, which is strictly more coverage than before (previously zero), but the "wrong cell chosen" class of bug (a `node_atlas_layout` mismatch, not a UV-formula bug) is still unverified headless. Verify: `cargo build --bin manifold` clean; `cargo clippy -p manifold-app -- -D warnings` clean; `cargo test -p manifold-app --bin manifold` 174 passed.

**Gap** — the inline node-preview fix (BUG-027) is pixel-verified headless only through the
per-node-texture path (`ui_snapshot/render.rs`, whole-texture UV `[0,0,1,1]`). The LIVE app packs
every preview into one rotating atlas and samples a per-cell UV with letterbox/aspect trim; that
cell-picking math lives inline in [app_render.rs](../crates/manifold-app/src/app_render.rs) and is
NOT exercised by any headless render (the atlas is filled by the content thread). So a subtle cell
or aspect error would show wrong/offset/squashed previews in the running editor but pass every test.

**Fix shape** — (1) factor the atlas-cell-UV math out of `app_render.rs` into one shared helper;
(2) in the harness, pack the already-rendered per-node textures into a synthetic atlas + build the
matching `node_atlas_layout`, register it under the atlas handle, and drive previews through that
shared helper. Then a single graph PNG proves the live cell math, not a copy of it. Not large.
Gated behind BUG-033 (the `ui-snapshot` harness doesn't compile on trunk).

### BUG-030 — Design-token ratchet red on trunk: raw `Color32::new(` count 201 vs baseline 200 — LOW (parked, not param-storage)
**Status:** PARKED

**Root cause** — a UI landing added one raw `Color32::new(` literal in `crates/manifold-ui/src`
without tokenizing it or bumping the ratchet. [design_tokens.rs:40](../crates/manifold-ui/tests/design_tokens.rs#L40)
sets `COLOR_BASELINE = 200`; the actual scan count is 201.

**Symptom** — `cargo test -p manifold-ui --test design_tokens` fails (`no_new_raw_color_literals`,
201 > 200). **Fails identically on origin/main (58bc2d43)**: `crates/manifold-ui/src` is
byte-identical between that commit and the P2 branch, and `scan()` reads only that directory, so
the drift predates and is independent of P2.

**Found during** — PARAM_STORAGE P2 (2026-07-05), full-workspace sweep after merging origin/main.
Two pre-existing trunk failures surfaced (this + the stale node catalog, which P2 regenerated) —
a signal that a recent UI landing skipped the full workspace test.

**Fix shape** — the UI/design-token owner tokenizes the offending literal (a `color::` token, or
`// design-token-exempt: <reason>`); the ratchet then returns to green at 200. Left red on purpose
rather than bumping the baseline, which would silently bless the drift the ratchet exists to catch.
Unrelated to param storage.

### BUG-014 — Content key collapses NaN/±Inf param values to one hash — LOW (parked)
**Status:** PARKED

**Root cause** — [install.rs:205-215](../crates/manifold-renderer/src/node_graph/freeze/install.rs#L205-L215):
`def_content_key` hashes `serde_json::to_vec(def)`, and serde_json writes non-finite floats
as `null`, so defs differing only in a non-finite param share a key while the fuse bakes the
raw f32.

**Status** — split verdict, judged UNREACHABLE today: the second skeptic traced every write
path into node params (scrub handlers clamp to finite ranges; JSON round-trips reject
non-finite). Parked as a hardening note — if a new param write path ever skips the clamp,
this becomes live. Cheapest closure: reject non-finite values at the `SerializedParamValue`
boundary (the eliminate-bug-class-at-storage-layer pattern).

### BUG-019 — Motion "group fold" (D17) has no UI surface to fold — DESIGN GAP (deferred)
**Status:** DEFERRED

**Symptom** — found 2026-07-04 completing UI motion P2. D17 lists "group fold: children
collapse into header," but the animation has nothing to animate: `EffectGroup.collapsed`
exists at the model layer (`crates/manifold-core/src/effects.rs:3194`) with zero rendering
surface — no group header, no collapse toggle, no child-card grouping by `group_id` in the
inspector (`rg EffectGroup crates/manifold-ui/src` → 0 hits).

**Root cause** — the design assumed a foldable effect-group UI in the inspector that was
never built. Group fold is a *new feature* (group header + child-card filtering + collapse
toggle), not an animation retrofit — correctly out of the motion layer's scope.

**Fix shape** — build the effect-group inspector UI first (own small design: header row,
`group_id`-keyed child filtering, collapse toggle), THEN the fold animation is a `FlipList`
+ exit-state retrofit like the other P2 collapses. Needs a design/build decision from Peter.

### BUG-020 — Card collapse animates effect cards but not generator cards — LOW (deferred)
**Status:** DEFERRED

**Symptom** — found 2026-07-04 (UI motion P2 batch 1). Effect cards collapse/expand with the
`collapse_anim` reflow; generator cards do not — their rows parent at root (`None`) in
`ParamCardPanel::build_generator`, so there is no `ClipRegion` seam to clip the collapsing
body the way `build_effect` has.

**Fix shape** — give `build_generator` the same parent/clip-region seam `build_effect` uses,
then reuse the existing `collapse_anim`. Small, localized to `param_card.rs`.

### BUG-021 — Value snap-back is Perform-inspector only, not the graph-editor param cards — LOW (deferred)
**Status:** DEFERRED

**Symptom** — found 2026-07-04 (UI motion P2 closer). Right-click value-reset eases the fill
(EASE_SNAP) on Perform-context inspector cards; the graph editor owns a separate
`ParamCardPanel` instance not reachable from the `ParamRightClick` dispatch site
(`ui_bridge/inspector.rs:1140`), so its value resets snap without the settle.

**Fix shape** — thread the snap-back trigger to the graph-editor's `ParamCardPanel` too, or
lift the reset-with-settle into shared `ParamCardPanel` logic both dispatch sites reach.

### BUG-026 — Batch-2 popups: entrance fade freezes at t=0 (transparent bg) until an input re-dirties the frame — MED — FIX LANDED, running-app verification owed
**Status:** OPEN

**Symptom** — reported by Peter 2026-07-05 (before/after screenshots): opening the Add Effect
browser renders the search field, filter chips, and preset cells floating directly over the
timeline — the popup's dark background panel is missing. Moving the mouse over the popup makes
the background appear and it then looks correct.

**Root cause (FOUND)** — not the alpha math, a missing animation-poll in the dirty-driven
renderer. The batch-2 popups (browser / ableton picker / settings) run a D17 entrance tween:
`enter_anim` starts at `t=0` and, while `t<0.999`, `BrowserPopupPanel::build` multiplies the
modal container's background + border alpha by `t` (browser_popup.rs:451,469-474) — so frame 0
draws the panel fully transparent while the cells (opaque, not `t`-gated) float on top. The
tween is ticked inside each popup's `update()`, which only re-runs while the frame stays dirty.
The inspector drawer + panel-split tweens self-sustain via a `needs_rebuild` poll after
`UIRoot::update()` (app_render.rs ~2927), but the batch-2 popups were added to `update()` and
never to that poll. Opening a popup dirties exactly one frame (drawing it invisible); nothing
re-dirties it, so the fade freezes at `t=0` until an unrelated input (mouseover) re-dirties the
frame — the "no background until mouseover" symptom.

**Fix (LANDED)** — added `is_animating()` to each batch-2 popup and the matching poll in the
app motion block, mirroring `drawer_anim_active` exactly. Gate: clippy `-D warnings` clean;
`manifold-ui --lib` 604/604. Commit `01c15213` (branch `fix/popup-enter-anim`).

**Verification owed (L4)** — the headless `--script` driver has no frame loop and its
`enter_anim` ticks off wall-clock, so it cannot exercise this timing bug; a running-app check
(open the Add Effect browser, confirm the background is present immediately without moving the
mouse) is the remaining proof. Tracked in VERIFICATION_DEBT (VD-006).

### BUG-136 — CINEMATIC_POST motion blur has no visible effect despite correct wiring — MED-HIGH
**Status:** OPEN

**Symptom** — Peter, live in `SceneLadders.manifold` (glb auto-import's physical-camera/
cinematic-post wiring): with `lens.shutter_angle = 181.05` and `motion_blur.max_blur_px = 128`,
orbiting the camera produces no visible motion blur.

**Verified correct, NOT the cause** — the graph wiring itself, read directly from the saved
project (`project.json`'s `wires` array): `camera` (`node.orbit_camera`) → `lens`
(`node.camera_lens`) → `render` (`node.render_scene`); `lens.out` also feeds
`motion_blur.camera` directly (so `motion_blur` reads the same lens-modified Camera, not a
bypassed one); `render.velocity` → `motion_blur.velocity`; `motion_blur` sits last in the chain
before `final`. Also confirmed the velocity source itself: `render_scene.rs`'s `prev_view_proj`
frame-to-frame diff (`render_scene.rs:1010-1011`) is only reset by `rebuild()` (object/light count
change, `render_scene.rs:456`), never by an ordinary param edit — so camera-orbit motion should
register as nonzero velocity independent of whether it's playback- or slider-driven.

**Root cause — UNKNOWN, needs runtime observation** (static/code-read verification stops here;
this needs the render observed live, not just re-derived). Suspects, not yet ruled out:
1. The UI param-edit path may not be live-propagating a dragged slider into the running
   content-thread graph on every frame (the codebase's known `ui-state-sync-path` bug class —
   see the memory of the same name).
2. `node.motion_blur`'s fused-vs-standalone codegen routing may be silently selecting a
   stale/pass-through kernel — same failure family as BUG-135's fused `wgsl_includes` gap,
   unconfirmed whether `motion_blur` is affected.
3. The render loop may not tick continuously while scrubbing a slider outside of active
   playback, collapsing `prev_view_proj`/current into the same value on each redraw.

**Fix shape** — reproduce live in the running app with the exact `SceneLadders.manifold` values
above; add temporary `println!`/`eprintln!` in `render_scene.rs`'s velocity fragment output and
in `node.motion_blur`'s `evaluate()`/derived-uniform recompute path to confirm both are actually
seeing nonzero `shutter_angle` and nonzero velocity per frame at runtime; narrow from whichever
one is flat when it shouldn't be.

**Static-read addendum (2026-07-13, Fable)** — also verified correct, NOT the cause: the atom's
smear math itself (`motion_blur_body.wgsl:62-72` — exact D4 formula; the clip-vs-texture y-sign
mismatch is provably invariant under the symmetric ±smear/2 tap layout, per the shader's header
note), and the prev-matrix bookkeeping (`render_scene.rs:1024-1025` stores prev_view_proj every
evaluate; camera-only orbit IS a valid velocity source — moving the model is not required).
Load-bearing design fact: `shutter_angle = 0` makes the shader an EXACT no-op (every tap
collapses onto the same texel), so a zero silently arriving anywhere in the chain produces
precisely this symptom with no error. The three suspects above therefore reduce to two runtime
values to probe: (a) `shutter_angle` at uniform-pack time, (b) one velocity texel during an
orbit. Retest caveat: glb auto-import is SSAO-only since `72135693` (2026-07-12 — lens/DoF/
motion nodes removed from the import graph), so a fresh import has no motion_blur node at all;
reproduce via `CinematicScene` or the saved `SceneLadders.manifold`. Owned by the
dof-polish lane (see `CINEMATIC_POST_DESIGN.md` status line, 2026-07-13 amendment).

**Runtime-probe addendum (2026-07-13, Sonnet 5, dof-polish worktree) — ESCALATION, not
fixed. Both of the addendum's two probe values check out clean across the whole shipped
pipeline; the bug does not reproduce in `CinematicScene` headlessly.** Method: temporary
`eprintln!`s in `node.motion_blur`'s `run()` (standalone path) and its D7 fused-recompute
closure (`motion_blur.rs`), plus a temporary GPU readback of `node.render_scene`'s velocity
resolve target added inline in `evaluate()` (`render_scene.rs`, immediately after
`ctx.outputs.texture_2d("velocity")`); rendered via `render-generator-preset` against a
throwaway copy of `CinematicScene.json` (`CinematicSceneProbe.json`, deleted after the probe,
never committed) with one extra wire, `system.generator_input.time -> cam.orbit`, so the camera
actually moves frame-to-frame (the shipped preset's `orbit_camera` has no time input and is
otherwise static — not itself a bug, see below) — `cargo run -p manifold-renderer --bin
render-generator-preset -- CinematicSceneProbe --size 320x180 --frames 30 --param
shutter_angle=181.05` (Peter's own repro value). Printed evidence, representative frames:
```
BUG136-RS view_proj_delta_sum=0.073722 velocity_wired=true
BUG136-RS velocity_center_texel=(9.25e-5, 5.16e-5) nonzero_texels=7103 max_mag=0.0102 max_at=(97,130)
BUG136-MB run() shutter_angle=181.05
```
— repeated every frame, 30/30. `shutter_angle` is nonzero from frame 1 onward (probe (a)
clean); the velocity buffer has thousands of nonzero texels with realistic magnitude away from
the orbit's look-at point (probe (b) clean — the near-zero *center* texel is correct physics,
not a bug: `orbit_camera`'s target is the world origin, so a vertex at screen-center sits on the
rotation axis and legitimately has ~zero NDC velocity; off-center texels show the real motion).
`node.motion_blur` ran via its **standalone codegen path 30/30 frames, 0 calls to the D7 fused
recompute closure** — confirms the shipped `CinematicScene` never routes this atom through the
fusion mechanism at all (consistent with D7's own honest-scope note: a Gather atom's input can
never fuse with its producer, so `motion_blur`/`variable_blur`/`bokeh_gather` are always
standalone in practice), which rules out suspect 2 (fused-vs-standalone routing) outright for
this preset. Closing the loop past the two committed probes: diffed two full headless renders at
640x360/30 frames, `shutter_angle=0` vs `shutter_angle=181.05`, everything else identical —
`ImageChops.difference` bbox `(188,116,478,293)`, max channel delta 7/255, nonzero mean —
a real, if subtle (the synthetic 1 rad/sec orbit rate is far slower than a live drag), visual
delta produced by the actual shipped shader dispatch, not just the uniform pack.

**Conclusion: the graph wiring, the shader math, the prev-matrix bookkeeping, the
derived-uniform packing, AND the velocity buffer are now ALL runtime-confirmed correct on the
exact path `CinematicScene` ships. The symptom does not reproduce headlessly.** This pushes the
remaining suspect space outside what a headless graph-execution probe can observe, onto the live
app's interactive/scheduling layer — the two original suspects this addendum could not exonerate:
(1) whether a dragged card slider's value (`shutter_angle`, or whatever drives the camera orbit
live) reaches the content-thread graph on every frame vs. only on drag-end/batched (the
`ui-state-sync-path` bug class named in the original entry) — our probe drove params through the
same `ParamManifest`/binding mechanism the UI card path uses, which somewhat weakens this
suspect but cannot rule out a UI-thread-specific propagation gap our headless harness has no
analog for; (2) whether the content-thread render loop ticks continuously (and thus keeps
`prev_view_proj` current) while scrubbing/orbiting outside active transport playback, or only on
discrete redraw requests — untestable without the live app. Escalating: this needs either a live
repro session (watch the actual param/frame traffic while Peter orbits) or a design decision on
which of (1)/(2) to instrument permanently, neither of which is a shallow code fix inside this
worktree. Status stays OPEN; not changed to FIXED. No code changes shipped this phase — all
temporary instrumentation and the scratch preset were removed before commit (`git status`
clean).

### BUG-096 (camera-rotate-sliders-jump-no-degrees) — FluidSim3D Rotate X/Y/Z sliders jump instead of rotating smoothly, no degrees readout — PARTIAL 2026-07-10 (legacy orbit phase + tilt sign restored in preset; degrees readout + jump investigation still open)
**Status:** PARTIAL — legacy orbit phase + tilt sign restored in the preset 2026-07-10; the degrees readout and the slider-jump observation pass remain open.
**Symptom:** dragging Rotate X/Y/Z on the Fluid Sim 3D card makes the view jump rather than turn continuously; values display as raw -1..1 floats (F2), not degrees. Reported by Peter 2026-07-10 (screenshot session).
**Root cause:** unknown — suspects: orbit param snapping through the binding path, the orbit camera pole at tilt=+-0.5 (cos(tilt) sign flip makes the view flip 180 deg), or slider quantisation interacting with the 90-degree orbit phase offset vs the legacy camera (orbit_perspective puts orbit=0 on +X; the legacy Euler camera sat on +Z — tilt also runs inverted vs legacy).
**Fix shape:** observe first (drag while logging orbit/tilt values); add a degrees formatString to the rotate params; consider re-phasing orbit_perspective (or the tilt/orbit_to_rad scale_offsets in the preset) so rot=0 matches the legacy +Z view and direction.

### BUG-203 (fluidsim2d-count-dims-display) — FluidSim2D: raising Particle Count dims the image instead of reading as more particles — MED
**Status:** OPEN — found 2026-07-10 (Peter screenshot session). (Renumbered from BUG-097 2026-07-17 — id collision with the archived ui-snap overlay-traversal bug, which keeps 097.)
**Symptom:** same as FluidSim3D's count-dimming (fixed 2026-07-10): more particles = same total splat light spread thinner, so the image dims.
**Root cause:** per-particle display energy normalized ~1/count (legacy design). NOTE: the 2D graph differs from 3D — `scaled_energy_calc` (Resolution Scaling id 2) computes `active_count * 4.096e-6 + 0.5` (energy apparently ∝ count?!), one `scatterEnergy` feeds the Render Density group (which is BOTH the force field and the display source), and Display gets only `intensity`/`zoom`. Read the whole graph with the probe before changing anything — the observable (dimming) contradicts the naive reading of that formula, so something else divides by count downstream.
**Fix shape:** mirror the 3D fix at the DISPLAY stage only: forces must stay count-invariant, display light should scale ~sqrt(count), anchored at the default count so the stock look is unchanged. node.math now has Sqrt (op 14). The 3D recipe: count binding → sqrt node → energy divisor, constant retuned by 1/sqrt(default_count). For 2D, if sim and display share one density, apply the sqrt slope to the display `intensity` instead of the splat energy.
**Also open (same family):** BUG-096 remainder (rotate degrees readout + slider-jump observation); param-surface dual source of truth (preset JSON params vs core generator_metadata_submissions.rs, which still lists the pre-turb_detail surface — reconcile or delete one).

### BUG-201 (interaction-overlay-automation-callback-type-complexity) — `manifold-ui --all-targets` clippy fails on 4 `type_complexity` findings in `interaction_overlay.rs`, unrelated to BUG-112 — LOW (lint-only)
**Status:** OPEN — found 2026-07-14 during bug-wave3 lane D (renumbered from BUG-161 2026-07-17 — id collision with the FIXED ui-snapshot compile bug, which keeps 161) while re-running BUG-112's exact gate (`cargo clippy -p manifold-ui --all-targets -- -D warnings`) to verify that fix.

**Symptom:** 4 `clippy::type_complexity` errors in [`src/interaction_overlay.rs`](../crates/manifold-ui/src/interaction_overlay.rs) at lines 2914 (`automation_point_moves: Vec<(UiGraphTarget, ParamId, (Beats, f32, UiSegmentShape), (Beats, f32, UiSegmentShape))>`), 2920 (`automation_segment_drag_commits`, same shape with an extra `f32`), 2926 (`automation_group_move_commits: Vec<Vec<(UiGraphTarget, ParamId, Beats, f32, f32, UiSegmentShape)>>`), and 2927 (`automation_draw_commits`) — all fields on `GestureTestHost`, a `#[cfg(test)]`-only fixture struct inside `mod p1_4_gesture_integrity_tests` (test-only, not production), so only `--all-targets`/`--tests` compiles this code; none touched by this session (confirmed: `git diff --stat -- crates/manifold-ui/src/interaction_overlay.rs` is empty at `c3113703`).

**Root cause:** unknown/not investigated — out of scope for BUG-112, which named only `audio_setup_panel.rs`/`graph_canvas/tests.rs`; this file wasn't scanned until this session ran the exact same `--all-targets` gate after fixing those two.

**Fix shape:** mechanical — factor each repeated `(UiGraphTarget, ParamId, ...)` tuple family into named `type` aliases (e.g. `AutomationPointMove`, `AutomationSegmentDragCommit`, `AutomationGroupMoveCommit`, `AutomationDrawCommit`) near the function signature. No behavior change.

### BUG-170 (gltf-crate-missing-field-node-parse-failure) — five Khronos assets fail at `gltf::import()` itself with `missing field 'node'` — a crate-level JSON-shape parse gap, not an extension-support gap
**Status:** OPEN, deferred to GLTF_ANIMATION_DESIGN.md — crate-bump pre-flight run 2026-07-16 during GLB_XFAIL_BURNDOWN_DESIGN P2, verdict: no bump exists. `cargo info gltf` confirms 1.4.1 is the latest published version on crates.io (no newer 1.x at all — `cargo update -p gltf --dry-run` correctly reports "Locking 0 packages"); this isn't a case of a fix being available and unapplied, there is nothing to bump to. Re-repro'd 2026-07-16 against the new `import_glb` slice-based parser (which still calls the crate's own `Gltf::from_slice_without_validation` + JSON deserialization) — `AnimatedColorsCube.glb` fails identically: `gltf parse failed: missing field 'node' at line 1 column 750`. The failure is in the crate's serde deserialization of the JSON itself, upstream of any validation or extension gate D1 touches, so D1's slice-based import change cannot and does not affect this bug. Per D8, the three assets (`AnimatedColorsCube.glb`, `CubeVisibility.glb`, `LightVisibility.glb`) move to `GLTF_ANIMATION_DESIGN.md`'s scope (pointer-targeted animation / `KHR_animation_pointer` / `KHR_node_visibility`) rather than staying open here — no JSON surgery attempted (forbidden move, D8). Extended 2026-07-16 during GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6's deferred-3 sweep: `AnimationPointerUVs.glb` and `PotOfCoalsAnimationPointer.glb` (both `KHR_animation_pointer` assets, found in the material-extensions manifest's leftover `deferred-3` bucket, not the animation-focused sweep that found the original three) fail identically — `missing field 'node'` at a different byte offset each, same crate-level parse gap, so folded into this bug rather than filed separately. Five assets total now: `AnimatedColorsCube.glb`, `CubeVisibility.glb`, `LightVisibility.glb`, `AnimationPointerUVs.glb`, `PotOfCoalsAnimationPointer.glb`.

**Root cause (identified 2026-07-16 during GLTF_ANIMATION A1, absorbed from the duplicate BUG-200 at the 2026-07-17 dedup):** `KHR_animation_pointer` channels legally OMIT `target.node` (the pointer replaces node-targeting entirely, per the extension's own spec). The pinned `gltf-json` 1.4.1's `animation::Target` struct declares `pub node: Index<scene::Node>` with no `#[serde(default)]`, so serde hard-fails deserializing any channel target that omits it — a crate-level gap one step upstream of `import_glb`'s validation-filter trick (that trick only patches `json::Root::validate()`'s output; this failure happens at raw `serde_json` deserialize).

**Fix shape:** ownership is back in this backlog. GLTF_ANIMATION_DESIGN shipped 2026-07-17 without this (its D5 deferred `KHR_animation_pointer` property targets; A1's held-out smoke test substituted `InterpolationTest.glb` and documented the substitution) — ownership is back here. Two real options, either is scoped work, not a one-liner: (a) patch/fork the crate to make `Target::node` an `Option<Index<Node>>` with a hand-rolled `Deserialize`; or (b) pre-process the raw glb JSON, injecting a dummy `"node": 0` into any channel target carrying `extensions.KHR_animation_pointer` before `Gltf::from_slice_without_validation`, then detect and skip-report those synthetic-node channels downstream (matches the existing raw-JSON-sniff doctrine for clearcoat/sheen/iridescence, but for a structural field). Queued behind Peter's call on GLTF_ANIMATION follow-ups; crate bump ruled out (1.4.1 is the latest published).

### BUG-173 (nodeperformancetest-exceeds-object-safety-bound-by-design) — Khronos `NodePerformanceTest.glb` (10,000 materials) exceeds `OBJECT_SAFETY_MAX` (1024) and is correctly rejected, not silently truncated — GLB_CONFORMANCE_DESIGN's "any glb, 1:1" promise doesn't reach mega-scene stress-test assets
**Status:** OPEN (informational — not a defect) — found 2026-07-15 during GLB_CONFORMANCE_DESIGN G-P7 full-suite classification. Re-confirmed unaffected 2026-07-16 during GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6: the object-safety bound is unrelated to material/texture shading, `NodePerformanceTest.glb` still XFAILs on the same by-design rejection.

**Symptom:** `render-import` on `NodePerformanceTest.glb` errors "10000 materials with geometry exceeds the 1024-object safety bound — this asset cannot be imported 1:1 without risking a runaway port-list" — exactly the D4-designed behavior (`GLB_CONFORMANCE_DESIGN.md` D4: `>1024` objects errors loudly, never truncates). This is the safety net working as intended, not a bug in the conventional sense — logged so the gap between "drop in any glb" and "this specific renderer-performance stress-test asset" is durable and traceable (the conformance manifest needs a named reason to classify it `xfail`, and this is that name) rather than silently invented.

**Fix shape:** none owed — raising `OBJECT_SAFETY_MAX` to cover 10k-object scenes would reintroduce the exact runaway-port-list risk D4 was written to prevent, for an asset class (renderer stress tests) outside Peter's actual show library (`typical-project-scale`: 53 layers, 2928 clips — not 10k static mesh objects in one glb). Revisit only if a real show asset needs it.

### ~~BUG-175~~ (filmgrain-fused-stencil-absorption-compile-explosion) — FUSED FilmGrain froze the app: stencil-tier chain absorption had no compile-cost gate, inlining ~860 KB of WGSL into one kernel
**Status:** FIXED 2026-07-16 (Fable, same-day session, `bug/filmgrain-fused-hang`) — `MAX_VIRTUAL_INLINE_BYTES` gate (256 KB) added to `chain_is_absorbable` (`freeze/region.rs`); oversized absorptions are refused and the producer runs as its own dispatch. FilmGrain now renders fully unfused (its only region was the refused absorption); Watercolor's warp-into-blur (~75 KB, largest shipped absorption) still fuses, byte-identical WGSL. Golden snapshot regenerated (FilmGrain entry dropped, nothing else changed). Proof: `filmgrain_noise_absorption_refused_by_inline_budget` + existing `watercolor_inloop_chain_fusion_matches_unfused`.

**Symptom:** adding FilmGrain to a card (first time since its BUG-098 rewrite made it fusable) froze the app on the next chain rebuild — content thread stuck for tens of seconds to minutes, UI thread alive (sliders move). Looked like a hard crash at the rig.

**Root cause:** the stencil tier absorbs a producer chain into the consuming blur's `fetch_in` (recomputed per tap corner). `MAX_VIRTUAL_CHAIN = 1` prices the *runtime* ALU of taps × 4 corners × chain, but nothing priced the *code size*: FilmGrain absorbs the noise atom (~6 KB body, the largest in the library) into `gaussian_blur` (35 fetch sites, the most in the library) → ~860 KB of WGSL after `InlineExhaustive`, ~50 s of synchronous spirv-opt + SPIRV-Cross + Metal compile on the content thread — twice, because static-param specialization (`SPEC_STABLE_FRAMES = 1`) recompiles the specialized variant one frame later. Measured: 110 s for the 8-render proof harness pass, 51 s with `MANIFOLD_WGSL_SPECIALIZE=0`, ~1.5 s unfused.

**For the instrument:** FilmGrain is safe on a card again — it costs its honest 15 small dispatches instead of one giant kernel compile mid-set. Residual (not this bug): fused-kernel compiles still run synchronously on the content thread; any future kernel near the budget still pays its compile there. Moving fused compiles off-thread is a separate, pre-existing gap.

### BUG-174 (unlit-materials-import-as-lit-not-routed-to-unlitmaterial) — `gltf_import.rs` never reads `KHR_materials_unlit`; every imported glTF material becomes a lit (Phong-ish) material even when the source asset is unlit by design
**Status:** OPEN — found 2026-07-16 during GLB_XFAIL_BURNDOWN_DESIGN P2, while gating BUG-166's fix.

**Symptom:** `UnlitTest.glb` (`extensionsRequired: ["KHR_materials_unlit"]`) now imports successfully after BUG-166's parse-layer fix (P2's `import_glb` + MANIFOLD's own extension gate no longer veto it) and renders non-black — but the rendered cubes show clear directional shading (a lit gradient across each face), not flat unlit color. `rg "unlit|Unlit" crates/manifold-renderer/src/node_graph/gltf_import.rs` returns zero hits: the importer's material-wiring code never inspects `material.extensions` for `KHR_materials_unlit` and never routes to the existing `UnlitMaterial` primitive (`crates/manifold-renderer/src/node_graph/primitives/unlit_material.rs`, `node_graph/material.rs` — the shading mode genuinely exists in the render graph, per `MATERIAL_SYSTEM_DESIGN.md`). GLB_XFAIL_BURNDOWN_DESIGN.md's P2 gate assumed "unlit renders via existing unlit-ish path" without this being verified true; it was not — there is no existing wiring from the glTF importer to `UnlitMaterial`. The gltf crate's `KHR_materials_unlit` cargo feature is also not enabled in `Cargo.toml` (only the manual `extensionsRequired`-allowlist entry lets it past import; the crate's typed `Material::unlit()` accessor isn't available without the feature).

**Root cause:** known and localized — `gltf_import.rs`'s per-material wiring (the same site that already branches on `alpha_mode()` for Blend/Mask) has no branch for `KHR_materials_unlit`; it unconditionally builds whatever the default lit material path is regardless of what the source material actually declares.

**Fix shape:** enable the `KHR_materials_unlit` feature on the `gltf` dependency in `Cargo.toml` (typed `Material::unlit()` accessor, mirrors the `KHR_materials_specular`/`KHR_materials_ior` precedent already there), then in `gltf_import.rs`'s per-material wiring, when `material.unlit()` is true, route to `UnlitMaterial` (base-color factor/texture only) instead of the PBR/Phong path — same shape as D2's spec-gloss conversion (BUG-167), a small per-material conditional at the existing wiring site. Out of GLB_XFAIL_BURNDOWN_DESIGN.md P2's scope (D1 is parse-layer only); candidate for a future phase of this doc or a follow-up bug fix session — low effort, one Khronos asset in the suite (`UnlitTest.glb`) but a real-world hazard for any flat/toon-shaded imported prop.

### BUG-177 (glb-vertex-colors-not-wired-color0-never-read) — glTF's `COLOR_0` vertex attribute is never read anywhere in the mesh pipeline, so per-vertex color (the entire point of `BoxVertexColors.glb`) has no path from import to pixel
**Status:** OPEN — found 2026-07-16 during GLB_XFAIL_BURNDOWN_DESIGN P3 (D4), while verifying the doc's own ⚠ VERIFY-AT-IMPL note ("confirm `flatten_primitive` reads COLOR_0; if not, wiring it is in-scope for the same phase").

**Symptom:** `BoxVertexColors.glb` now imports successfully (P3/D4's synthetic default-material fix — the primitive is no longer silently dropped) and renders, but as a flat uniform gray box — no per-vertex color variation is visible. `render-import` on the fixture confirms this visually (`/tmp/p3_boxvc.png`, GLB_XFAIL_BURNDOWN_DESIGN.md P3 execution).

**Root cause:** known and structural, not a small wiring gap. `flatten_primitive` (`gltf_load.rs`) reads POSITION/NORMAL/TEXCOORD_0 only — no `reader.read_colors(0)` call exists. The reason it wasn't a one-line fix: `MeshVertex` (`crates/manifold-renderer/src/generators/mesh_common.rs`) — the struct a color field would need to land on — is the SHARED 48-byte GPU vertex format for the entire node-graph mesh-primitive family (~30 primitives: `render_scene`, `render_3d_mesh`, `render_instanced_3d_mesh`, `bend_mesh`, `displace_mesh`, `twist_mesh`, `scatter_on_mesh`, etc., each with its own hand-authored WGSL `struct Vertex { ... }` matching the same 48-byte layout), not a gltf-import-local type. Adding a color field changes `MeshVertex`'s size/layout, `MESH_VERTEX_SPECS` (the freeze-codegen channel spec), and every hand-copied WGSL vertex struct across that primitive family — real GPU-ABI blast radius, not a private-function change.

**Fix shape:** add a `color: [f32; 4]` field to `MeshVertex` (default `[1,1,1,1]` when COLOR_0 is absent, so every existing mesh source is byte-identical after the change — same "unwired = neutral" doctrine every other optional channel in this codebase follows), wire `flatten_primitive`'s `reader.read_colors(0)` into it, thread it through `render_scene.wgsl`'s `resolve_albedo` (multiply into the sampled/factor base color, matching glTF's own spec — COLOR_0 multiplies `baseColorFactor`), and update every other WGSL vertex struct + `MESH_VERTEX_SPECS` in lockstep so the shared ABI stays consistent. This is a `DECOMPOSING_GENERATORS.md`/`ADDING_PRIMITIVES.md`-scale change (touches the freeze-codegen path CLAUDE.md's "every barrier-free per-element GPU atom" rule governs) — genuinely out of GLB_XFAIL_BURNDOWN_DESIGN.md P3's scope per DESIGN_DOC_STANDARD.md's escalate line ("changing a public API shape the doc doesn't specify"); needs its own phase brief (or a `MESH_VERTEX_COLOR_DESIGN.md`) rather than an in-session improvisation. Escalated rather than attempted.

### BUG-178 (gltf-import-manual-is-multiple-of-clippy-lint) — `cargo clippy -p manifold-renderer --tests -- -D warnings` fails on two pre-existing `while len % 4 != 0` loops clippy's `manual_is_multiple_of` lint now flags
**Status:** OPEN — found 2026-07-16 during GLB_XFAIL_BURNDOWN_DESIGN P4, while running the phase's clippy gate.

**Symptom:** `cargo clippy -p manifold-renderer --tests -- -D warnings` (the `--tests` variant, not the plain lib-only gate CLAUDE.md's standing rule runs) fails at `gltf_import.rs:1576` and `gltf_import.rs:1718`, both `while json_padded.len() % 4 != 0 { ... }` (glb chunk-padding loops in test helper code from `df143400f`, 2026-07-15 — predates this session, unrelated to D3/D6). `cargo clippy -p manifold-renderer -- -D warnings` (lib-only, no `--tests`) passes clean — the lint only fires on the test-target compile. Not introduced by GLB_XFAIL_BURNDOWN_DESIGN.md P4; discovered incidentally.

**Root cause:** known and trivial — `clippy::manual_is_multiple_of` (stabilized recently, part of the pinned toolchain's current lint set) prefers `!json_padded.len().is_multiple_of(4)` over `len % 4 != 0`. Cosmetic, not a logic issue.

**Fix shape:** two one-line replacements (`while json_padded.len() % 4 != 0` → `while !json_padded.len().is_multiple_of(4)`) at the two named lines. Trivial; out of P4's scope (unrelated to the phase's D3/D6 changes) — logged per CLAUDE.md's "bug found but not fixed this session" rule rather than folded into an unrelated commit.

### BUG-179 (fusion-coverage-baseline-floor-stale-32-vs-33) — `node_graph::freeze::proof::fusion_coverage_baseline`'s D4/P6 ratchet floor (`fused_presets >= 33`) fails deterministically at HEAD (`d61eb73b`), pre-existing and unrelated to GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1
**Status:** OPEN — found 2026-07-16 during GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1's gate run.

**Symptom:** `cargo test -p manifold-renderer --features gpu-proofs` fails at `freeze/proof.rs:2101`: `expected ≥33 bundled presets to fuse, got 32 — partition regressed?` (54 regions / 224 atoms, vs the floor's documented 55/225). Confirmed via a clean `git clone --local` of `origin/main` at the exact tip E1 branched from (`d61eb73b`, zero diff) — same 32/54/224 measured with NO code changes at all, so this is not something E1's uniform-growth/parse-plumbing work caused. E1's own gate (clippy clean, full nextest lib suite green, `glb_conformance` sweep green, this one pre-existing failure) is otherwise clean.

**Root cause:** unknown — some preset that used to fuse into its own 1-region/1-atom group (per the P6 landing comment's "33/55/225") no longer partitions into a fusable region. Suspect: an unrelated change between the P6 landing and `d61eb73b` nudged one preset's boundary classification (a primitive gaining a new `boundary_reason`, a JSON preset edit changing its topology, or a `PrimitiveRegistry` registration change) without updating this ratchet's floor. Not diagnosed further this session — out of scope for E1 (a glTF-import phase, not a fusion/freeze phase).

**Fix shape:** bisect from the P6 landing commit to `d61eb73b` for the commit that dropped one preset's fusability (the printed `[freeze coverage]` detail list names all 32 currently-fusing presets — diff against the P6-era list to find the missing one), then either restore that preset's fusability or lower the floor to 32 with an updated rationale comment (same discipline the existing comment documents for prior ratchet moves). Someone touching `freeze/`, `region.rs`, or a bundled preset JSON should own this — flagging for Peter/the next fusion-focused session per CLAUDE.md's "bug found but not fixed this session" rule.

### BUG-180 (large-glb-import-oom-risk) — importing a large glTF/GLB (multi-hundred-MB, dozens of images) can get the app SIGKILLed by the OS, and intermittently succeeds/fails on the same file
**Status:** OPEN — found 2026-07-16, reported by Peter (not caused by GLTF_MATERIAL_EXTENSIONS_DESIGN E1/E2, landed the same session — confirmed the reproducing asset uses only `KHR_materials_pbrSpecularGlossiness`, no transmission/volume extension, so E1/E2's new code paths never activate for it).

**Symptom:** `tests/fixtures/gltf/abandoned_warehouse_-_interior_scene.glb` (114.8MB, 35 objects, 31 images) killed `cargo run --release` (bare `zsh: killed`, no panic message, no MANIFOLD-side crash log written for the event — `~/Library/Logs/com.latentspace.manifold/` had nothing newer than 2026-07-05) on one run; an immediate rerun of the identical file imported successfully (~7s) with no code changes in between. Points to a memory-headroom race rather than a deterministic bug: the import likely holds every source image fully decoded (CPU + GPU) at once with no downsampling/streaming, so it sits close enough to the machine's available memory that unrelated system load tips it into an OS-level kill.

**Also observed (same logs, same on both the failing and succeeding run, so NOT the differentiator — checked before treating it as one):** `[presets] hot-reload applied; catalog generation = 1` immediately followed by `catalog generation = 2` in the same second right after the glb import. This is the STOCK preset catalog (effects/generators) hot-reloading twice, not the glb re-importing — `[Import] Added 3D model` appears exactly once in both logs. Worth a look on its own (possibly the import triggering two filesystem-watch events), but ruled out as the cause of this crash.

**Fix shape:** cap imported texture resolution (downsample oversized source images before GPU upload — this is what lets browser glTF viewers handle the same files comfortably), and/or stream/free CPU-side image buffers as soon as each GPU upload completes instead of holding all 31 decoded images in memory simultaneously. Next: profile actual peak RSS during import of a large asset to confirm the headroom theory numerically before picking a fix, and check whether the double preset hot-reload on import is wasteful (separate, smaller fix).

### BUG-219 (abeautifulgame-interactive-import-crashes-doubled-full-gpu-build) — importing `ABeautifulGame.glb` (43MB, 15 materials) through the running app's drag-drop path crashes; the same file renders correctly headless through the identical production import graph — MED-HIGH (reported live by Peter)
**Status:** OPEN

## Fixed

### BUG-234 (ui-snap-script-harness-never-runs-a-content-thread-tick-so-driver-envelope-modulation-is-headlessly-unobservable) — the `--script` flow harness cannot show a driver/envelope-modulated value changing across `Snapshot`s, for ANY param, effect-card or scene-panel — found 2026-07-17, UX-P3a build (`wave/scene-panel-ux-p3-build`), while gating "assign an LFO → the row's value visibly modulates (transport running, two snapshots)"

**Status:** FIXED 2026-07-21 (lane/bug234-modulation-tick). Wired the harness's existing public `manifold_playback::modulation::evaluate_modulation` (the same call `PlaybackEngine::tick_playing`/`tick_non_playing` make, `engine.rs:911`/`:1020`) directly into `Step`'s per-frame loop against `data.project` — it internally computes `compute_active_clip_timing` itself (a private helper of `evaluate_modulation`, not something the harness needs to call), so the 2026-07-21 assessment's "`compute_active_clip_timing` is private, blocking envelopes" premise was moot: driver AND envelope phases both run through this one call, zero `manifold-playback` API changes. Also had to add a `super::reconcile_state(ui, data)` call after the Step loop's `apply_ui_frame_invalidations` (mirroring what `advance_frame` already does) — without it, `evaluate_modulation`'s write to the param's effective value never reached the widget tree a `Dump`/`Snapshot` reads (structural rebuild gating and value-text resync are separate concerns). New `envmod` fixture (`fixtures.rs`) + `scripts/ui-flows/envelope-modulation.json` exercise both drivers-family and the envelope/ADSR half specifically: Bloom `amount` (default 0.50) pulls to 4.85 one frame after the clip's rising edge, decays back to exactly 0.50 once elapsed clears 1 beat — proven to fail (assert fails, exit 1) with the wire commented out, pass (exit 0) with it in.
**2026-07-21 assessment (verif-infra-flow-driver lane, superseded by the fix above):** traced further, still not fixed. `evaluate_all_drivers(project, current_beat)` (`manifold-playback/src/modulation.rs:160`) alone is a clean, small, one-call add to `Step`'s frame-advance. But the LFO/driver half is only half the bug — `evaluate_all_envelopes` (line 246) additionally needs a per-layer `active_clip_timing: &[(Beats, Beats)]`, which the only existing computation for (`compute_active_clip_timing`, line 795) is `fn`-private to `manifold-playback`, not `pub`. Wiring envelopes properly therefore needs either (a) making `compute_active_clip_timing` `pub` (a second-crate API change, out of a mechanical lane's scope) or (b) duplicating its elapsed-into-clip walk in `manifold-app` (drift risk, violates "fix at the root"). Doing drivers alone would half-fix the bug (LFO-driven values would move; envelope/ADSR-driven values still wouldn't) and silently misrepresent the gate as closed — worse than leaving it open. Deferred, unfixed this session; real fix shape: expose `compute_active_clip_timing` as `pub` in `manifold-playback`, then wire both `evaluate_all_drivers` + `evaluate_all_envelopes` into `Step`'s frame-advance (`ui_snapshot/script.rs`, the `AutomationAction::Step` arm) against `data.project` at the advancing beat.

**Symptom:** in `cargo xtask ui-snap gltfscene --script <flow>`, clicking a card's Driver-tab toggle (`PanelAction::DriverToggle`, which does append a real `ParameterDriver` — confirmed via the dispatch log) followed by `Pointer{transport.play}` + `Step{frames: N}` + `Snapshot`/`Dump` never moves the target param's displayed value text, no matter how many frames are stepped or what beat division/waveform the driver uses. Reproduced on both the scene panel's own value cell (`scene_setup.object.roughness_value`) and the SAME param's mirror on the generator card (the plain numeric label next to its T/∿/A strip) — so this is not scene-panel-specific.

**Root cause (suspect, not fully traced):** `ui_snapshot/script.rs`'s own module doc states the harness's drag path "needs no live content thread... the actual clip mutation happens directly on `SceneData.project`" — i.e. `Runner`'s `Step` action advances a deterministic UI clock/filmstrip counter for animation purposes, but does not appear to invoke `manifold_playback::modulation::evaluate_all_drivers`/`evaluate_all_envelopes` (the functions that actually write a driver's per-frame output into `PresetInstance.params`) against the driver's `SceneData.project`. Not fully traced to the exact `Step` handler code this session — the next agent should start at `Runner::advance_frame`/wherever `Step{frames}` is handled in `script.rs` and check whether it calls into `manifold-playback`'s modulation pipeline at all.

**Fix shape:** either (a) wire `Step`'s frame-advance to call `evaluate_all_drivers`/`evaluate_all_envelopes` against the driver-owned `SceneData.project` at the advancing beat (mirroring what the live content thread does every tick), or (b) if that's structurally awkward for the headless harness (no real `PlaybackEngine`), add a dedicated `AutomationAction` (e.g. `EvaluateModulationAt { beat: f64 }`) that a script can call explicitly between `Step`s — cheaper, more surgical, and makes the harness's modulation-evaluation an explicit script step rather than an implicit (and currently missing) side effect of `Step`.

**Workaround used this session:** UX-P3a's "value visibly modulates" gate was proven at the mechanism level instead — `crates/manifold-playback/src/modulation.rs`'s `exposed_generator_param_is_driver_modulated_across_beats` and `..._survives_save_reload_and_still_modulates` construct the same `ToggleNodeParamExposeCommand` a mod-button click dispatches, attach a `ParameterDriver`, and assert `evaluate_all_drivers` moves the value directly — a stronger, faster proof than a UI screenshot diff, but it doesn't exercise the UI render path the way an L3 flow does. `scripts/ui-flows/scene-panel-ux-p3a-expose-modulate.json` still proves everything UP TO driver assignment headlessly (exposure, naming, driver-toggle dispatch) and documents this gap inline via `SCENE_PANEL_UX_DESIGN.md`'s UX-P3a "as built" note.

### BUG-296 (script-driver-effect-cards-never-rebuild-after-structural-dispatch) — a structural `PanelAction` (e.g. `EffectReorder`) mutates the project but the `--script` driver's inspector cards stay stale forever — found 2026-07-21, WIDGET_TREE P5 flow sweep (VD-034 attempt)
**Status:** FIXED 2026-07-21 (verif-infra-flow-driver lane) — the suspected root cause below (never rebuilds) was instrumented and DISPROVED; real root cause: `Runner.active_layer` (`crates/manifold-app/src/ui_snapshot/script.rs`'s `Runner` struct) is `None` unless a `LayerClicked` gesture ran this script; `resolve_effect_target` (`ui_bridge/mod.rs:857-864`) then falls back to `project.timeline.layers.first()`, so a structural dispatch like `EffectReorder` silently mutated the WRONG layer's (often empty) effect chain while the display kept rendering the actually-selected layer (display syncs from `data.active`/`data.selection`, independent of `Runner.active_layer`). Fix: in `pub fn run`, seed `runner.active_layer` from `data.active` immediately after `Runner::new()`. Verified: `dispatched EffectReorder(0, 2) (structural=true)` in `scripts/ui-flows/inspector-card-drag-reorder.json`, reorder confirmed via post-drag rect asserts.
**Severity:** LOW-MED — harness gap, not a live-app bug (the live app rebuilds on the content-thread round trip). Blocks the VD-034 card-drag L3 flow and any future flow asserting a structural inspector change (add/remove/reorder effect).
**Symptom:** in the `inspector` fixture scene, dragging Mirror's drag-handle past Bloom onto Strobe fires the real input path — harness log shows `dispatched EffectReorder(0, 2) (structural=true)` — and the dispatch mutates `data.project` synchronously, but every subsequent Dump (3+ forced frames) still shows Mirror/Bloom/Strobe at their original node ids and rects. No assertion proving reorder can pass against a structurally stale tree.
**Root cause (suspected):** the driver's `advance_frame` (`crates/manifold-app/src/ui_snapshot/script.rs:844-880`) never rebuilds the cached `Vec<ParamCardPanel>` from the mutated project after a `structural=true` dispatch — the `needs_structural_sync` path that the live app's frame loop honors is not exercised (same family as BUG-234's missing content-thread tick and BUG-293's dropped `pending_actions`).
**Fix shape:** in `advance_frame`, after a structural dispatch, run the same card-rebuild the live app runs (or honor `needs_structural_sync` before the next Dump). Sibling flow-driver gaps BUG-234/293/294 — a single verification-infra lane should take the family.
**Repro note (for the eventual flow):** dropping onto the immediately-next card is a designed no-op (`to_fx != from + 1`, inspector.rs) — a reorder flow must drop past the adjacent card.

### BUG-294 (scene-setup-dock-scroll-headless-noop) — the Scene Setup dock's own scroll is a no-op under the `--script` driver; content below the dock viewport is unreachable by flows — found 2026-07-21, SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md R1/R2 lane
**Status:** FIXED 2026-07-21 (verif-infra-flow-driver lane) — `Gesture::Scroll` now checks `ui.layout.scene_setup().contains(center) && ui.scene_setup_panel.is_open()` before the generic fallback and calls the dock's own `ScenePanel::handle_scroll(delta.y)` setter directly (the same one `ScenePanel::handle_event`'s `UIEvent::Scroll` arm calls when the live app routes the generic pipeline), then forces `self.needs_structural_sync = true` — mirroring `window_input.rs`'s explicit `needs_rebuild = true` after routing scene/audio-setup scroll (the generic `UIEvent::Scroll` pipeline alone never bakes the new offset into built node positions, same BUG-223 gap class). No new scroll mechanism, no new shared state — reuses the dock's existing setter.
**Severity:** LOW-MED — harness gap, not a live-app bug. Blocks any flow that needs to scroll the Scene Setup dock to reach a row below the fold (e.g. `scene-panel-card-convergence-c-p1b-object-scrub`'s `pos_x` row, per prior lane notes) — the flow can select an item and dispatch actions, but can't scroll the dock to bring an off-screen row's slider into view/drag range.
**Symptom:** a `--script` flow's `Scroll` gesture moves the timeline viewport correctly but has no effect when targeted at the Scene Setup dock — rows past the visible fold stay unreachable regardless of how many `Scroll` events the script dispatches.
**Root cause:** `Scroll` gesture handling in the headless driver is wired only for the timeline viewport panel; the Scene Setup dock (`manifold-ui/src/panels/scene_setup_panel.rs`) has its own internal scroll offset for the properties card list, but nothing in the `--script` event dispatch path (`ui_snapshot/script.rs`) routes a `Scroll` event at the dock into it — the dock's scroll state only moves in the live app, driven by winit's real pointer-wheel input, which the script driver doesn't synthesize for this panel.
**Fix shape:** wire `UIEvent::Scroll` in the script driver to the Scene Setup dock's scroll-offset setter when the pointer position falls inside the dock's rect, mirroring however the timeline viewport's own `Scroll` handling resolves target-panel-under-pointer. Not attempted this session — found while proving the R1 fog re-pointed flows, out of this lane's scope (root-cause the two exposure regressions, not the harness).

### BUG-293 (script-driver-discards-context-menu-actions) — the `--script` driver's overlay dispatch drops `host.pending_actions` on the floor instead of routing them like the live app does — found 2026-07-21, SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md R1/R2 lane
**Status:** FIXED 2026-07-21 (verif-infra-flow-driver lane) — `drain_and_dispatch` now does `std::mem::take(&mut host.pending_actions)`, drops `host` to end its borrow, then routes the taken actions through `self.apply_panel_actions` — mirroring `app_render.rs:1471`. **Caveat found while gating this fix:** `scripts/ui-flows/scene-setup-modifier-stack.json` (this bug's originally-named acceptance flow) still fails after this fix, unchanged from its pre-fix failure — confirmed by running both binaries against the same flow. Its failure is NOT a pending_actions drop: "+ Add Modifier"'s click resolves at `(736.0, 3430.0)`, far below `LOGICAL_H` (1216.0), and never produces a dropdown. This is a separate, not-yet-diagnosed gap (fixture/layout drift, possibly BUG-294-adjacent) — see VD-035's 2026-07-21 update. This bug (the actual `pending_actions` drop) is fixed; the flow's OTHER problem is not.
**Severity:** LOW-MED — harness gap, not a live-app bug. Blocks any flow that needs a context-menu action (right-click → menu item), e.g. the modifier-stack "add modifier" flow if it's ever re-authored to go through the context menu instead of a direct button dispatch.
**Symptom:** a `--script` flow that right-clicks to open a context menu, then expects a menu-item action to take effect, sees no effect — the click registers (the overlay's `on_pointer_click` runs) but whatever `PanelAction` the click queued never executes.
**Root cause:** `ui_snapshot/script.rs:780`, `let _ = host.pending_actions; // context-menu actions: no proving script needs these yet` — the script driver's per-event loop explicitly discards `host.pending_actions` after every `UIEvent`, unlike the live app's own dispatch loop (`app_render.rs:1471`, `actions.append(&mut host.pending_actions);`) which drains them into the real `PanelAction` queue every frame. The comment marks this as a deliberate, not-yet-needed deferral at the time it was written, not an oversight — but it means ANY flow depending on a context-menu-originated action is unrunnable headless today.
**Fix shape:** mirror `app_render.rs:1471` in the script driver's event loop — drain `host.pending_actions` into the same `actions` vec the driver already routes through `ui_bridge::dispatch` for direct `PanelAction`s, instead of discarding them. Mechanical, one-line-shaped fix; not attempted this session (harness-only, out of this lane's scope).

### BUG-295 (live-structural-scene-edit-params-never-reconcile-into-preset-instance) — a freshly-added scene node's stamped exposure is invisible in the Scene Setup panel until save+reload — found 2026-07-21, SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md R1/R2 lane
**Status:** FIXED (this lane, 2026-07-21) — `refresh_manifest_from_graph` wired into the five scene-structural commands' (`AddSceneObjectCommand`/`AddSceneLightCommand`/`AddSceneFogCommand`/`AddSceneEnvironmentCommand`/`InsertMeshModifierCommand`) `execute` AND `undo`, so `PresetInstance.params` resyncs from `graph.preset_metadata.params` immediately after any structural mutation instead of only at load time. Acceptance proof: the 3 re-pointed fog flows (`scene-setup-add-fog-drag`, `scene-setup-fog-density-card-row`, `scene-setup-fog-undo-removes-fog`) exercise add → live row appears → drag → undo (row vanishes) → redo (row reappears) against the converged card-row surface, all green.
**Severity:** HIGH — this is the DEEPER layer of R1's "structurally invisible" symptom; the manifest-stamping fix (this lane's `AddSceneFogCommand`/`AddSceneEnvironmentCommand`, and the pre-existing shipped `AddSceneObjectCommand`/`AddSceneLightCommand`/`InsertMeshModifierCommand`) is necessary but NOT sufficient — none of those commands' newly-exposed params reach the live UI until a save/reload round trip.
**Symptom:** click "+ Add Fog" (or "+ Light", confirmed to reproduce identically) in a live/headless session — the button correctly disappears (the graph IS wired) and `def.preset_metadata.params` correctly gains the new node's exposure entries (verified via direct trace: `world_sections`/section computation for a fresh Light 2 test resolved to the CORRECT section strings), but the Scene Setup panel's properties card shows ZERO rows for the new node — not even a section header. Reloading the SAME project from disk (not attempted this session, but implied by the mechanism below) would show it correctly, since load-time reconcile picks up `preset_metadata.params` fresh.
**Root cause:** `manifold_core::effects::PresetInstance.params: ParamManifest` is a STORED, one-time-built field. `build_param_manifest` (`crates/manifold-core/src/effects.rs:1076`) — the only function that (re)derives it from `graph.preset_metadata.params` — is called from exactly three places: the `Deserialize` impl (load), and `PresetInstance::reconcile_manifest` (`effects.rs:1745`), itself gated on `self.pending_wire.is_some()` — a field ONLY ever set from a serialized wire at load time. `manifold-app`'s ONLY call to the project-level `reconcile_param_manifests()` outside test fixtures is `manifold-io/src/loader.rs:221` (load path). `EditingService::execute` (`crates/manifold-editing/src/service.rs:69`, the SOLE mutation gateway per this repo's own invariant) bumps `data_version` and runs debug-only overlap/tree-order checks — it never touches `params`/manifest reconciliation for ANY command. So a command that mutates `def.preset_metadata.params` at runtime (any of the five Add/Insert scene commands, all funnel through `with_target_graph_mut`, which bumps a GRAPH version counter — a different counter, never wired to `PresetInstance.params`) has no path back into the live `inst.params` the UI actually reads (`param_surface()`/`gen_params_to_surface`, `state_sync.rs`). Confirmed NOT a manifest-stamping gap: this lane's own debug trace showed `def.preset_metadata.params` and `world_sections`'s section-string resolution BOTH correct for a freshly-added Fog node AND a freshly-added "Light 2" (via the shipped `AddSceneLightCommand`) — the row still didn't render for either.
**Fix shape:** `PresetInstance` needs a `pending_wire`-independent resync method — e.g. `resync_params_from_graph()` calling `build_param_manifest(is_generator, effect_type, &graph, None)` and assigning the result to `self.params` (dropping the wire-application half `reconcile_manifest` needs) — and something needs to call it after any command that touches a Generator's `preset_metadata` at runtime. Where to call it from is the real design question (a per-command call, a generic post-`with_target_graph_mut` hook, or a broader "any Generator command bumps `graph_version` → resync params" rule in `EditingService::execute` or `ContentPipeline`'s tick) — needs care around what happens to existing card state (drag caches, automation-lane indices, OSC addressing) keyed on the OLD `params` ordering when it's replaced mid-session. Explicitly NOT attempted this session — this is an `EditingService`/mutation-gateway-adjacent change (one of the most sensitive choke points in the codebase per this repo's own hard rules) discovered while gating a Sonnet low-effort lane's scoped fix; needs its own design pass, not a one-line patch from this lane.
**Escaped:** SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md P1 (the original `AddSceneObjectCommand`/`AddSceneLightCommand`/`InsertMeshModifierCommand` stamping work, commits `c82ab7ae`/`944b0697`) · caught-by: this lane's flow-level proof attempt (a state-level-only test suite never exercises the live `PresetInstance.params` projection, so P1's own tests couldn't have caught this).

- BUG-265 (inspector-card-drag-indicator-stale-geometry) — FIXED 2026-07-20 on `lane/w2b-bug265-drag-geometry` (`8cb1c437` + `94632d65` card_y removal, merged `f2ac71d9`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-266 (inspector-tab-pin-dies-on-incidental-selection-change) — FIXED 2026-07-20 on `lane/w1c-bug266-tab-pin` (`fcd4c084`, merged `43c9d3d1`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md; residue: pin resurrection-on-reselect quirk for Peter's feel-pass
- BUG-267 (inspector-duplicated-card-lists) — FIXED 2026-07-20 on `lane/w1d-bug267-card-vecs` (`717f8910`, merged `726de5a0`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-240 (scrub-fine-flow-tests-a-retired-shift-fine-delta-drag-gesture) — FIXED 2026-07-20 (script deleted per its own fix shape, W1-B) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-186 (sheenwoodleathersofa-webp-error-message-misattribution) — FIXED @ IMPORT_ANYTHING_WAVE_DESIGN.md W1, 2026-07-17 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-001 (pasting-effect-shares-sources-effectid) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-002 (clip-clone-new-id-doesnt-regenerate-nested-effect) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-003 (duplicating-grouped-effect-leaves-group-id-pointing-sources) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-004 (effect-paste-carries-ableton-automation-bindings-generator-paste) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-005 (macro-targets-cant-disambiguate-two-same-type-effects) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-006 (param-edits-undo-fused-away-nodes-silently-no) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-007 (particle-loop-fusion-exclusion-blind-configured-node-wgsl) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-008 (fused-buffer-region-mismatched-array-lengths-reads-out) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-009 (segment-stateless-gate-misses-statestore-held-scalar-state) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-010 (wgsl-compute-silently-dispatches-first-multiple-entry-points) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-011 (fused-fused-output-buffer-sized-max-all-array) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-013 (commit-wait-completed-never-checks-command-buffer-status) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-016 (imported-glb-layers-are-black-boxes-no-card) — FIXED (2026-07-04) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-017 (docs-index-sync-docs-dir-red-main-two) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-018 (catalog-stale) — FIXED @ 38ec595f — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-022 (main-window-browser-popup-escape-while-search-field) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-023 (no-new-raw-color-literals-red-main-real) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-024 (generator-preset-thumbnails-render-white-background-unrepresentative) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-024-ORIG (original-analysis-generator-thumbnails-white-background) — SUPERSEDED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-027 (graph-editor-node-previews-composite-wrong-z-layer) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-028 (file-drop-targeting-cant-read-live-pointer-during) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-029 (profiling-feature-doesnt-compile-rotted-against-beats-bpm) — FIXED (2026-07-06) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-032 (gltf-import-model-2-materials-fails-load-unknown) — FIXED (2026-07-05) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-033 (ui-snapshot-feature-build-broken-manifold-core-effects) — FIXED (2026-07-07) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-035 (authoring-hitch) — FIXED @ 55faec0f — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-036 (param-manifest-construction-not-a-unified-safe-gate) — FIXED (2026-07-06) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-039 (saw-rotation-wrap) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-040 (v13-import-migration-drop) — FIXED (2026-07-09) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-041 (superflux-glide-fire) — FIXED (2026-07-06) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-042 (onset-settle-grab) — FIXED (2026-07-06) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-043 (deep-bass-floor-anchor) — FIXED (2026-07-06) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-044 (mix-trigger-deafness) — FIXED (2026-07-06) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-046 (low-band-kick-deafness-on-mixes) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-047 (setup-panel-overflow) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-051 (trigger-clear-unwired) — FIXED @ 3089e0a3 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-052 (sample-rate-dependent-detection) — FIXED @ 6e0e8988 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-055 (eval-harness-stale-time-grid) — FIXED (2026-07-07) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-056 (audio-mixdown-clippy-debt) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-057 (ui-snapshot-dead-blit-pipeline) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-058 (drag-end-consumable) — FIXED (2026-07-08) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-059 (band-line-grab-falls-through) — FIXED (2026-07-08) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-060 (inspector-footer-overpaint) — FIXED @ 39836352 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-061 (slider-reset-per-panel-lottery) — FIXED @ 480acf63 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-062 (no-forward-version-guard) — FIXED @ 1e349bf5 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-064 (save-rename-before-fsync) — FIXED @ 050e3fd7 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-065 (save-dedup-history-identity-key-6-hex-chars) — FIXED @ 050e3fd7 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-066 (fluid3d-corner-drift) — FIXED @ eebac94d — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-067 (ui-snapshot-dead-blit-pipeline) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-070 (stepper-and-nonstandard-slider-reset) — FIXED @ 3a88f728 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-071 (ui-snap-dump-stale-parent) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-072 (audio-mixdown-all-targets-clippy-debt) — FIXED @ 78e97d4a — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-074 (audio-mixdown-flaky-under-parallel-tests) — FIXED @ 78e97d4a — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-075 (timeline-drag-end-never-finalizes) — FIXED (2026-07-08) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-077 (test-fixtures-not-region-wrapped) — FIXED (2026-07-09) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-078 (generator-runtime-reshapes-from-stale-meta-params) — FIXED (2026-07-09) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-082 (trigger-fire-mode-level-features-near-dead) — FIXED @ 12fbc37d — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-083 (video-export-has-no-progress-display) — FIXED (wave2 lane C, 2026-07-11 — sha pending at archival time) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-084 (recording-drop-counter-never-surfaced) — FIXED (wave2 lane C, 2026-07-11 — sha pending at archival time) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-087 (osc-timecode-receiving-flag-false-positive-at-startup) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-088 (pre-existing-clippy-tests-gate-dirty-since-f1-landing) — FIXED @ 78e97d4a — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-090 (audio-mixdown-analysis-only-test-flakes-under-parallel-run) — FIXED @ 78e97d4a — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-091 (osc-drop-frame-timecode-uses-approximate-divisor) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-092 (gltf-import-caps-render-scene-objects-at-8-stale-mirror) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-093 (ui-snapshot-fixtures-unnecessary-cast-clippy-debt) — FIXED @ a56f641a — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-094 (fluidsim3d-clip-trigger-turbulence-mux-double-wire) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-095 (fluidsim3d-boot-seed-center-cluster-not-random) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-097 (ui-snap-render-overlay-pass-uses-wrong-traversal) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-098 (film-grain-drifts-and-reads-as-blocky-pixels) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-099 (design-tokens-raw-color-literal-count-drifted-past-baseline) — FIXED @ 54a80448 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-100 (gltf-fresh-import-renders-near-black-for-non-azalea-geometry) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-103 (outer-routings-drop-bindings-that-target-a-node-inside-a-group) — FIXED @ 9384d080 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-104 (audio-trigger-takes-over-shared-param-mod-goes-dead) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-105 (graph-node-slider-no-right-click-reset) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-106 (audio-mixdown-analysis-only-test-order-flaky) — FIXED @ 78e97d4a — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-108 (effect-card-add-effect-button-floats-over-sectioned-rows) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-109 (fire-meter-dead-in-all-transport-states) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-119 (timeline-layer-flickers-intermittently) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-261 (card-slider-drags-record-no-undo-entry) — FIXED 2026-07-19 (`573b50ea`, lane/undo-redo-baseline) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-253 (shininess-and-tone-map-uniform-order-drift-weird-tints) — FIXED 2026-07-18 (P7 lane, `lane/depth-relight-p7-impl`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-249 (scene-panel-modulation-is-decorative-synth-pids-never-resolve-at-runtime) — FIXED 2026-07-18 (`lane/bug-249-expose-then-arm`, pending landing) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-250 (scene-panel-enum-value-cells-dead-after-convergence-removed-enum-click-path) — FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`311bfb2a` + `d28bfff4`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-251 (scene-and-audio-dock-scroll-inverted-vs-every-other-surface) — FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`ea812c24`). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-252 (eight-scene-flow-scripts-dead-at-step-2-on-stale-outliner-assert) — FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`f101a585` + flow retargets in `d28bfff4`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-214 (ext-mesh-gpu-instancing-missing-from-supported-extensions-allowlist) — FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`a1963ffa`). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-213 (no-report-line-for-unimplemented-optional-material-extensions) — FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`a1963ffa`). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-242 (live-trigger-edge-rearm-hostage-to-shape-release) — FIXED 2026-07-18 (same evening; Peter approved) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-238 (scene-setup-camera-world-light-eye-toggle-reads-as-dead) — FIXED 2026-07-18 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-237 (scene-setup-camera-world-light-param-scrub-does-nothing-live) — FIXED 2026-07-18 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-182 (hdri-exr-files-fail-or-fail-silently) — FIXED 2026-07-18 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-241 (stage1-dsp-onset-frontend-misses-loud-real-kicks-track-dependent) — FIXED 2026-07-18 (Fable, same-day follow-up on the lane) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-224 (scene-setup-close-button-bypasses-shared-toggle-action) — FIXED 2026-07-17 (`lane/panel-interaction-bugs`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-223 (scene-setup-dock-scroll-state-updates-but-never-repaints) — FIXED 2026-07-17 (`lane/panel-interaction-bugs`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-216 (feedback-loop-into-final-output-freezes-at-depth-one) — FIXED @ f2684402 (DEPTH_RELIGHT_DESIGN.md P4, D6(b)). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-218 (modifier-commands-splice-at-dead-group-output-vertices-port) — FIXED 2026-07-17 (lane/scene-bugfixes) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-212 (duplicate-scene-object-string-bindings-dangle-on-imported-mesh) — FIXED 2026-07-17 (lane/scene-bugfixes) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-185 (e6-texture-completion-invalidates-two-stale-goldens) — FIXED with BUG-211's landing (2026-07-17) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-215 (conformance-sweep-panics-on-duplicate-mat-0-handle) — FIXED same session — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-210 (add-scene-object-command-emits-pre-migration-legacy-wires) — FIXED @ 6e8b00ba — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-211 (conformance-harness-advancing-clock-cant-converge-animated-imports) — FIXED (this entry's landing commit, `lane/bugfix-210-conformance-frozen-time` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-207 (materialless-skinned-mesh-silently-imports-static-at-node-scale) — FIXED @ f3358d00 (`lane/bugfix-207-default-material-skin`). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-208 (skin-plus-morph-drops-morph-silently) — FIXED @ 50db9369 (`lane/bugfix-208-skin-morph`). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-206 (import-framing-crops-elongated-objects) — FIXED @ e182a391. — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-205 (skinned-import-double-transform-and-wrong-bbox-space) — FIXED @ 3aeafe4a (`lane/bugfix-skinned-import-scale`). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-204 (animated-glb-import-rejected-by-retrigger-card-lint) — FIXED @ 6d7cac31 (`lane/bugfix-gltf-retrigger-trigger-type`). — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-198 (ui-automation-key-event-has-no-global-undo-seam) — FIXED @ 6318c9fb — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-192 (ui-automation-under-text-flat-card-rows) — FIXED @ 6318c9fb — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-183 (fusion-coverage-baseline-slipped) — FIXED 2026-07-17 (Sonnet, BUGFIX_WAVE_2026_07_17_DESIGN Lane 5) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-196 (is-multiple-of-clippy-debt-gltf-import-render-scene) — FIXED (bugfix-wave-2026-07-17 Lane 3) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-195 (scene-setup-merge-no-stored-object-radius-for-scale-sanity) — FIXED (bugfix-wave-2026-07-17 Lane 3) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-194 (scene-setup-vertex-count-not-computable-from-def) — FIXED (bugfix-wave-2026-07-17 Lane 3) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-236 (scene-setup-flows-assert-stale-outliner-text) — FIXED 2026-07-18 (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1d closing session) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-193 (scene-setup-no-remove-object-command) — FIXED 2026-07-17 (bugfix wave, Lane 2, `lane/bugfix-removal-commands`) against the CURR... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-184 (automation-clear-lane-not-wired-to-ui) — FIXED 2026-07-17 (bugfix wave, Lane 2, `lane/bugfix-removal-commands`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-199 (audio-and-scene-setup-docks-have-no-working-scroll-input) — FIXED 2026-07-17, BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 1 (`lane/bugfix-dock-scroll`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-166 (gltf-crate-vetoes-extensionsrequired-we-already-support) — FIXED 2026-07-16 (parse layer, GLB_XFAIL_BURNDOWN_DESIGN P2; the residual unlit-materia... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-200 (khr-animation-pointer-channels-fail-to-deserialize) — SUPERSEDED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-189 (import-graph-10ms-resolution-independent-gpu-floor) — FIXED (residual documented) 2026-07-17 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-197 (switch-texture-blocks-ibl-generation-gate) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-158 (mapped-param-edits-snap-back-no-two-way-binding) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-202 (freeze-codegen-region-fusion-gpu-tests-fail-with-badinput-standalone) — FIXED 2026-07-14 (Fable root-cause session, branch `bug/163-freeze-extkind-probe`; renu... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-154 (removing-group-with-slider-bound-nodes-leaves-stale-effect-card) — FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-120 (grid-terrain-winding-disagrees-with-vertex-normals) — FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-117 (render-generator-preset-silently-under-renders-async-loaded-presets) — FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-147 (bokeh-gather-cpu-reference-helpers-dead-without-gpu-proofs) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-144 (prewarm-cache-tests-flake-under-full-lib-parallel-run) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-142 (fire-meter-capture-bench-flakes-under-parallel-load) — FIXED 2026-07-14 (bug-wave3 lane D), same fix as BUG-113 — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-124 (mesh-primitive-tests-clippy-debt-under-tests-features) — FIXED 2026-07-14 (bug-wave3 lane D), same fix as BUG-126 (this and BUG-126 named the id... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-110 (osc-receiver-test-type-complexity-clippy-debt) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-113 (param-manifest-get-bench-flakes-under-parallel-load) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-112 (manifold-ui-all-targets-clippy-debt-audio-setup-panel-graph-canvas-tests) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-089 (live-clip-pending-tick-queue-dead-on-all-live-paths) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-073 (ui-snap-script-drawer-tween-never-ticks) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-159 (timeline-scroll-past-playhead-violent-snapback) — FIXED 2026-07-14 (bug-wave lane B) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-161 (ui-snapshot-feature-fails-to-compile-canonical-def-arc-mismatch) — FIXED 2026-07-14 (bug-wave3 lane C) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-049 (child-row-right-indent) — FIXED 2026-07-14 (bug-wave3 lane C) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-048 (arm-two-reds) — FIXED 2026-07-14 (bug-wave3 lane C) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-101 (setup-spectrogram-scroll-offset) — FIXED 2026-07-14 (bug-wave3 lane C) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-081 (no-slug) — FIXED 2026-07-14 (bug-wave3 lane C) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-031 (no-slug) — FIXED 2026-07-14 (bug-wave3 lane C) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-068 (inspector-scene-cliphit-overlap) — FIXED 2026-07-14 (bug-wave3 lane C) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-125 (preset-runtime-generator-picks-first-final-output-nondeterministically) — FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`), option (a) from the fix shape — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-122 (graph-editor-node-face-loses-type-name-when-custom-named) — FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-121 (graph-editor-effect-card-missing-mapping-drawer-chevron) — FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-012 (no-slug) — FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-076 (inspector-scroll-underestimates-content-height) — FIXED (closed as not-reproducible) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-015 (no-slug) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-025 (no-slug) — FIXED (believed) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-151 (graph-editor-node-browser-container-fill-not-drawn) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-152 (ui-snapshot-render-graph-node-textures-arc-migration-miss) — FIXED (in the `feat/editor-window-unification` P1 diff, uncommitted at session end pend... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-150 (mute-chip-press-motion-teleports-hit-bounds-after-scroll) — FIXED @ `804ea043` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-114 (draw-family-blocked-on-array-into-texture-codegen-read-path) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-146 (render-scene-atom-pipelines-never-prewarmed) — FIXED (fusion-sweep worktree, this session) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-141 (import-graph-fused-region-linearize-depth-parse-fail) — FIXED this session (fusion-sweep mechanical-sweep phase 1 worktree; lands with its commit) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-149 (glb-import-fog-slider-per-world-unit-cliff) — FIXED @ `ee16c3b5` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-145 (shaft-pipelines-not-in-prewarm-first-frame-cold-start-spike) — FIXED (this session, VOLUMETRIC_LIGHT_DESIGN P3) for the shaft/shadow-pipeline half of ... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-127 (decode-worker-silent-drop-wedges-export-flush) — FIXED @ `450f01c4` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-128 (sdr-video-export-gamma-diverges-from-display-and-stills) — FIXED @ `63937590` (encode side `b692bb9a`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-129 (export-fractional-fps-silently-rounds) — FIXED @ `8a814c23` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-130 (export-audio-mux-fails-late-and-leaks-temp) — FIXED @ `2c829eaf` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-131 (video-decode-hardcodes-bt709-video-range) — FIXED @ `87427ec0` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-132 (video-decode-nearest-neighbor-scaling) — FIXED @ `2b3e15e1` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-133 (video-extension-list-overpromises-webm-avi) — FIXED @ `5711f65c` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-143 (macros-panel-ableton-trim-drag-outside-p7-inventory) — FIXED @ `d5ab1ae7` (UI_WIDGET_UNIFICATION P8, 2026-07-13) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-111 (no-slug) — FIXED @ `d73b3e36` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-054 (renderer-device-ptr-dangles) — FIXED @ `d447ec8d` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-123 (mesh-edges-capacity-vs-active-count) — FIXED @ `1b854d45` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-079 (missing-preset-fails-silently-no-onscreen-signal) — FIXED @ `834fdaa6` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-038 (ableton-log-spam) — FIXED @ `06bfd879` — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-086 (recording-audio-track-under-covers-duration-on-longer-takes) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-085 (recording-frames-recorded-overstates-async-append-drops) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-138 (no-slug) — FIXED @ 8659c11a (2026-07-13, Sonnet 5, `dof-polish` worktree, branch `feat/dof-polish`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-137 (no-slug) — FIXED 2026-07-13; confirmation waived by Peter 2026-07-16 (verification-debt burn-down,... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-139 (bug-status-rebuild-drops-fixed-pointer-lines) — FIXED (2026-07-13) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-140 (glb-import-non-square-aspect-distortion) — FIXED (2026-07-12) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-126 (manifold-renderer-tests-clippy-debt-under-gpu-proofs) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-135 (fused-texture-codegen-drops-wgsl-includes) — FIXED this session (fusion-sweep mechanical-sweep phase 1 worktree; lands with its comm... — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-162 (ui-snapshot-feature-canonical-def-arc-regression) — FIXED 2026-07-14 (bug-wave3 lane D) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-163 (amg-livery-black-body-carpaint-extension-and-texture-cap) — FIXED 2026-07-15 (GLB_CONFORMANCE_DESIGN G-P2, `909976d2`) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-164 (material-maps-force-one-repeat-sampler-ignores-per-texture-wrap) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P4, D3) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-165 (boombox-multi-texture-never-converges) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-167 (spec-gloss-pbrspecularglossiness-entirely-unhandled) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P3, D2) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-168 (ext-mesh-gpu-instancing-unhandled) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P4, D6) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-169 (metalroughspheresnotextures-renders-fully-black) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-171 (boxvertexcolors-no-material-primitive-skipped-entirely) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P3, D4) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-172 (recursiveskeletons-no-default-scene-rejected) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN P2, D5) — full history in docs/archive/BUG_BACKLOG_CLOSED.md
- BUG-181 (import-ao-mix-flattens-alpha) — FIXED 2026-07-16 (same day) — full history in docs/archive/BUG_BACKLOG_CLOSED.md

## Checked and safe (coverage proof)

Audited during the 2026-06-23 duplication sweep; these duplicate correctly. Recorded so the
audit boundary is auditable.

- **Graph-node copy/paste** — `PasteNodesCommand` ([graph.rs:1985-2110](../crates/manifold-editing/src/commands/graph.rs#L1985-L2110)) mints fresh runtime ids + fresh `NodeId`s, remaps internal wires, starts pasted nodes un-exposed. Has regression tests (`paste_node_clones_with_fresh_identity_and_undo_removes`, `paste_remaps_internal_wires_to_the_new_node_ids`). **This is the reference implementation** for the BUG-001/002 fixes.
- **Generator paste** — `PasteGeneratorCommand` overwrites the target layer's single generator in place, addressed by `LayerId`. No id minted, no collision.
- **Markers** — created fresh via `TimelineMarker::new` (fresh `MarkerId`, [marker.rs:20-27](../crates/manifold-core/src/marker.rs#L20-L27)); no copy/paste/duplicate-marker path exists (markers are timeline-level, untouched by layer/clip dup).
- **New-clip-from-scratch paths** (MIDI/percussion/live-trigger/browser-drop) — construct fresh clips, not duplicates of existing ones.

## Blast radius — id-keyed resolvers that a duplicate `EffectId` breaks

All first-match-wins; all used by both editing and undo/redo:
- `Project::find_effect_by_id_mut` — [project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947) (master + layer + clip effects)
- `Project::find_effect_by_id` — [project.rs:711](../crates/manifold-core/src/project.rs#L711)
- `GraphTarget::Effect` / `set_base_param_by_id` paths that wrap them
- Renderer chain rebuild `harvest_state_from` — [preset_runtime.rs:1667](../crates/manifold-renderer/src/preset_runtime.rs#L1667) (per-card GPU state migration)

**Not** in the blast radius: macros (`(layer, type, param)`-addressed — see BUG-005),
markers, generators (`LayerId`-addressed).

## The pattern behind all of this

Duplicating an id-bearing entity must mint a fresh identity for itself **and** every nested
id-bearing child, or id-keyed first-match resolution collides. The graph-node path enforces
this with a test and never regressed; the paths without a test (effect paste, clip clone)
did. The durable fix for the class is a test per duplication path, not a doc note.

Related agent-memory notes: `feedback_hidden_field_dependencies` (the mirror — removing a
field silently breaks identity), and `project_invariant_audit` (its "Positional identity"
category is marked *already fixed*; BUG-001/002 are live counterexamples — correct that claim
when one is fixed).


### BUG-254 (imported-scene-AO-not-durably-removable-rebuilds-from-canonical-template) — stripping the SSAO/ambient-occlusion group from an imported scene's graph does not survive editing/saving; the scene rebuilds AO back from its canonical def, and there is no per-scene AO toggle
**Status:** OPEN
**Severity:** MED (perf: ~9ms/frame across three 4K scenes on `MeshAudio`; and the only way to reclaim it today is a hand-edit the app silently reverts)
**Symptom:** Every imported GLB scene bakes a cinematic AO chain into its graph (`gltf_import.rs` ~2350–2418: `ssao_gtao → bilateral_blur ×2 → mix`, the "Ambient Occlusion" group). Deleting those nodes from the saved `project.json` works at load/render (measured ~9ms reclaimed at 4K across three scenes, 34→25ms p50), but the removal is NOT durable: after editing a scene and saving, AO reappears on the edited scenes (verified — a file delivered clean at AO=0 came back AO=2 on the two scenes Peter renamed/edited; the untouched third stayed clean).
**Root cause:** the scene graph is rebuilt against its canonical default (`with_target_graph_mut` lifts/uses `catalog_default`, `manifold-editing/src/commands/graph.rs:39-64`), and that template always contains the AO group. Edits that touch a scene re-materialize it from the template, re-adding AO; save persists the in-memory (re-AO'd) graph (`saver.rs` is a plain serde dump, no graph logic). Exact edit-op that triggers the rebuild not fully traced (candidates: scene-param path / scene-setup-panel / a None→catalog_default lift).
**Fix shape:** a per-scene "Ambient Occlusion" toggle param on the canonical def. A plain runtime `switch_texture` is NOT sufficient — the executor runs every statically-reachable node (`execution_plan.rs:330`), so a switched-off AO branch still executes and reclaims nothing; the toggle must make the AO group ABSENT from the assembled graph when off (param-aware conditional assembly in `gltf_import`), so a rebuild reproduces the gated-off graph and the persisted param keeps it off through saves. Default ON (preserve the current look — Peter values the AO/GI subtle lighting cues, see the taste note; do not strip by default). A few hours + verification: perf-reclaim (AO nodes pruned) AND save round-trip survival.

### BUG-255 (shadows-off-below-4K-renders-near-empty) — turning shadows off on the imported scenes at a non-4K output resolution produces a near-empty render (p50 collapses to ~3ms with periodic 200–500ms spikes)
**Status:** OPEN
**Severity:** MED (a valid-looking config renders wrong; also a measurement trap)
**Symptom:** `MeshAudio (AO off)` with `cast_shadows=0` on all suns renders correctly at 4K (~21.8ms p50, full scenes) but at 2560×1440 the unprofiled soak reports p50 ~3ms with occasional 200–500ms max frames — the pattern of a mostly-empty render, not a real speedup. A profiled run of the same config shows 28 nodes and a 217ms frame, so the scene CAN render; it mostly doesn't. 4K+shadows-off and 1440p+shadows-ON both render fine — only the shadows-off × sub-4K combination misbehaves.
**Root cause:** unknown. Suspects: a resolution-dependent code path in the render_scene shadow-disabled branch (depth buffer / light setup / a size-keyed cache), or a perf-soak sampling artifact specific to this combination. NOT visually confirmed — needs a headless rendered frame to look at (the visual oracle), which perf-soak can't provide.
**Fix shape:** first reproduce visually (render one frame of 1440p+shadows-off to PNG and inspect for empty/black), then bisect the shadows-off render_scene path against output resolution. Do not trust the ~3ms as a perf result.

### BUG-256 (project-switch-locks-to-first-loaded-shared-type-ids) — loading a second project after launch keeps rendering the FIRST-loaded project's version of any generator/preset whose type id it shares; the app appears "locked" to the first project
**Status:** FIXED 2026-07-19 (K3) — root cause found + class fix + regression test; landed via `lane/bug256-entry-fix`
**Severity:** HIGH (silent — the performer edits/compares projects and sees stale content with no error; confounds any in-app A/B done without relaunching)
**Symptom:** Reported by Peter 2026-07-18: loading projects back-to-back in one Manifold session, the app "stays locked to the one first loaded after launch." Two projects that share generator type ids (e.g. the same `cc0_*` flower scans) but carry DIFFERENT embedded graphs (light vs original mesh, AO on vs off) render as the first-loaded version even after loading the second. Relaunching the app between loads shows the correct project. Objective `perf-soak` numbers are unaffected (each run is a fresh process), so this is an in-app cache/registry staleness, not a load-format bug.
**ROOT CAUSE (2026-07-19, K3 — confirmed by fail-without-fix regression test):** NO project-boundary invalidation existed for renderer id-keyed state. `GeneratorRenderer.layer_generators` is keyed by `LayerId` and gates rebuilds on the project's SERIALIZED `graph_version`/`graph_structure_version` u32 counters (`generator_renderer.rs:604`). Both the ids AND the counters collide across two projects derived from the same template (same layers, same edit depth, different graph JSON), so after loading project B the old generator instances kept serving B's layers — counters equal → no rebuild, param versions equal → no param push. `ClipRenderer::release_all()` (`generator_renderer.rs:1273`) was written for exactly this (its own comment: "to prevent GPU memory leaks across project switches") but had ZERO callers — dead code. `PlaybackEngine::initialize` never stopped clips either, so the engine's own `active_clip_renderers`/`active_clip_ids` (keyed by `ClipId`) also survived. Effect chains were NOT affected: `dispatch_chain` rebuilds on `is_compatible(effects,…)` — a content check (`chain_dispatch.rs:186`), and the catalog-generation check.
**FIX (the class, not the instance):** the engine now owns the boundary — `PlaybackEngine::initialize` (`engine.rs:488`) calls `stop_all_clips()` + `release_all()` on EVERY registered renderer before installing the new project, so present and future renderers get boundary invalidation by construction rather than by remembering to override a hook. `GeneratorRenderer::release_all` additionally clears `thumb_gens` (ClipId-keyed parked thumbnails — same collision class). `release_all`'s trait doc now states the contract. Regression: `crates/manifold-playback/tests/project_boundary_release.rs` — fails without the fix, passes with it.
**Research 2026-07-18 (Fable, code-read; live repro NOT yet run — findings are static-trace):**
- **CONFIRMED — the project IS swapped on load.** `ContentCommand::LoadProject` (`content_commands.rs:412`) calls `engine.initialize(*project)`, and also `clear_all_effect_state()` (feedback/bloom textures), `clear_generator_trigger_state()` (BUG-104 latches), `resize()`, re-`prewarm_project_chain_segments(p)`, MIDI/OSC/LED re-init. So the layer graphs (`genParams.graph`) in the new `Project` do reach the content thread.
- **RULED OUT — the effect/chain fusion cache.** Chain segments are **content-keyed**: `def_content_key: u64` = "content key of the card's EFFECTIVE def (edited graph or canonical) at build time" (`preset_runtime.rs:663-673`); `prewarm_chain_segments` is documented "enqueue-only and content-keyed" (`:875`). Two projects with divergent graphs for the same type id hash differently → cannot alias here. The lock is NOT in effect/chain fusion, and generators run through the same content-keyed `PresetRuntime` path (`:2284`), so likely not there either.
- **RETRACTED 2026-07-19 (K3 re-verification) — the "CONFIRMED GAP" below is wrong: the registry IS reinstalled on every load.** The original trace grepped direct callers of `set_project_presets` and missed the indirection: `open_project_from_path` (`project_io.rs:347`) calls `load_project_with(path, install_embedded_presets)` (`project_io.rs:365`), and `install_embedded_presets` (`project_io.rs:44`) is the loader's post-deserialize hook that calls `set_project_presets` (`project_io.rs:58`) — running before `Project::reconcile_param_manifests`, before `LoadProject` is even sent. The snapshot-restore paths wire the same hook (`app_lifecycle.rs:1045,1075`), `refresh_preset_overlay_if_changed` (`content_commands.rs:54`) keeps it synced on fork edits, and the `project_local_preset_reload.rs` integration test covers the swap. (Method lesson: negative caller claims need LSP `findReferences`, not grep — the hook is passed as a function pointer.) ~~CONFIRMED GAP — project-preset registry is never installed/cleared on load. `set_project_presets` / `clear_project_presets` (`preset_loader.rs:220,247`; its own doc: "call with empty vecs on project close/switch so a stale preset doesn't persist") has NO non-test caller except the internal delegation inside `clear_project_presets`; `LoadProject` never calls either. The load-time "preset template unresolved — project-local preset not registered yet?" warnings come from `PresetTypeId` resolution against this un-updated registry. This is a real bug on its own (browser overlay + any type-id template resolution), and the leading suspect for the lock.~~
- **NOT YET PINNED — the exact stale cache serving the first project's render.** Since effect/generator compilation is content-keyed (safe), the remaining suspects are (a) generator template resolution falling back to the un-updated project-preset registry by type id, and (b) a GPU **mesh/resource cache keyed by node/layer id** (which are stable across two projects both derived from the same scene template — node 8 = render_scene in both), NOT by path/content, so project B's node reuses project A's loaded mesh. The mesh loads by *path* (`gltf_mesh_source`), which argues against (b) for the light-vs-original swap, but the id-keyed-cache class is not ruled out. → *2026-07-19: pinned — it was the id-keyed class, in `GeneratorRenderer` itself (see ROOT CAUSE above); the mesh/anim caches are path-keyed (`gltf_anim_cache.rs:299`) and were innocent.*
**Workaround (pre-fix builds):** relaunch Manifold between project loads.

### BUG-257 (trim-and-target-bars-teleport-on-drag-after-inspector-scroll) — scroll the inspector, then drag a modulation trim handle or envelope target bar: the bars jump to where the slider was BEFORE the scroll
**Status:** FIXED 2026-07-19 (lane/trim-scroll-stale-rect, Fable; regression tests `trim_bars_follow_the_track_after_scroll` / `env_target_bar_follows_the_track_after_scroll` in param_card.rs)
**Severity:** MEDIUM (visual detachment only — the dragged VALUE stays correct because x is scroll-invariant; but the handles landing on another card's row reads as the modulation UI being broken)
**Symptom:** Reported by Peter 2026-07-19: with the inspector scrolled, dragging a modulator's trim handles shows the tiny fill + handles "move off the slider to somewhere else on the page." Independent of the BUG-246 trim fix (that one was value-revert under playback snapshots; this one is geometric and needs no playback).
**Root cause:** two sources of layout truth. In-place scroll (`ScrollContainer::offset_content`, scroll_container.rs:120) shifts every content node's tree bounds by `delta_y` — no rebuild — but `ParamCardPanel` also caches each slider's `track_rect` at build time, and nothing refreshes that copy. The trim drag and envelope-target drag paths fed the cached rect's stale **y** into `tree.set_bounds` (`reposition_trim_bars`; the `bar_y = track_rect.y - 2.0` target-bar write), teleporting the overlay nodes back to the pre-scroll row. The plain slider fill never had the bug because `BitmapSlider::update_value` (slider.rs:446) already reads live bounds via `tree.get_bounds(ids.track)`.
**Fix:** the two y-writing drag paths now read live bounds from the tree (`tree.get_bounds(track)`), same as `update_value` — no new sync mechanism (refreshing the cache on scroll would be a second synchronization path, rejected). All other cached-`track_rect` uses are x/width-only (`x_to_normalized`, proximity zones) and provably safe: scroll shifts only y, and any relayout rebuilds the card and refreshes the cache. Field comment on `SliderNodeIds::track_rect` now states the x-only contract.
**Residual class note:** the cached-rect pattern itself is the inviting structure (single-source-y-layout violation). If a horizontal-scroll or non-rebuilding x-shift is ever added, the x-only uses break too; the durable end-state is deleting the cached field, which requires threading `&UITree` into `handle_pointer_down` — deferred as not worth the blast radius today.


### BUG-258 (trim-geometry-math-duplicated-across-four-sites) — the pixel math mapping a trim [min,max] to bar positions is re-derived inline in at least four places; the hit-zones can silently drift from the visible bars
**Status:** FIXED 2026-07-19 (lane/bug-258-259-fix, Fable): `trim_bar_rects` / `target_bar_rect` in param_slider_shared.rs are now the single geometry source for build, reposition, drag-writes, and hit-zones; the macros panel's 4th inline copy was deleted (it also carried the BUG-257 teleport — fixed by routing through `reposition_trim_bars` on live bounds)
**Severity:** LOW today (constants match, so zones and bars agree), HIGH drift potential — one edit to `OVERLAY_INSET` or the bar layout that reaches only some copies makes the grabbable area disagree with the drawn handle, and nothing fails loudly
**Symptom:** none user-visible yet — code-quality/stability entry logged from the BUG-257 fix session.
**Root cause:** the trim geometry (`base_x = track.x + OVERLAY_INSET`, `usable = width − 2×inset`, bar x = `base + t*usable − TRIM_BAR_W/2`) is computed independently in `build_trim_handles`, `build_trim_handles_explicit`, `reposition_trim_bars` (param_slider_shared.rs), and the inline proximity catch-zones in `handle_pointer_down` (param_card.rs ~4193/4243, same pattern for the target handle). The doc comment on `reposition_trim_bars` already claims "the single copy they all share" — the pointer-down copies prove that false. They also read the cached `track_rect` while reposition now reads live bounds, so the two families disagree about *which* truth they consult (x-only today, so harmless — see BUG-259).
**Fix shape:** one pure geometry function — `(track_rect, min, max) -> (fill_rect, min_bar_rect, max_bar_rect)` — used by build, reposition, AND hit-testing alike. Test: bar rects from the builder equal the rects the hit-zone math grabbable-implied, for a few (min,max) points.

### BUG-259 (pointer-down-has-no-tree-access-forces-cached-layout) — `handle_pointer_down` runs hit-testing and proximity math with no `&UITree`, so it can only consult build-time cached rects; the structural half of the BUG-257 class
**Status:** FIXED 2026-07-19 (lane/bug-258-259-fix, Fable): `handle_pointer_down` now takes `&UITree` and every param-card hit/drag path reads live bounds; the cached `track_rect: Rect` became `track_span: TrackSpan` (x + width only — y/height unrepresentable, so the BUG-257 mistake no longer compiles). `SliderNodeIds` construction, the widget drag state, and all x-only readers use the span; build-time code and y-positioning read `tree.get_bounds(track)`. Deliberate deviation from the original fix shape: instead of threading tree through every panel's press path AND deleting the field, the span type makes the dangerous half of the field unrepresentable — tree-free panels (scene_setup) keep a legal x-only cache.
**Severity:** LATENT — safe exactly as long as scroll stays vertical-only and every x-shift rebuilds the cards. A horizontal scroll, a zoom, or any future in-place x-shift breaks every pointer-down path in every panel at once, with the same teleport signature as BUG-257 but on drag-START (wrong gesture target, wrong initial value) rather than mid-drag
**Symptom:** none today — this is the un-closed half of the BUG-257 root class (its Residual class note points here).
**Root cause:** `ParamCardPanel::handle_pointer_down(node_id, pos)` takes no tree, so proximity zones and initial `x_to_normalized` must use the cached `SliderNodeIds::track_rect` captured at build time. BUG-257 fixed the mid-drag writers; the pointer-down readers remain cache-dependent by signature, not by oversight.
**Fix shape:** thread `&UITree` (or a read-only bounds view) into `handle_pointer_down` across the panels that need bounds, then DELETE the cached `track_rect` field from `SliderNodeIds` so the tree is the single layout truth and the BUG-257 mistake becomes unrepresentable. Blast radius is the pointer-down signature across panels — schedule as its own sweep, never folded into feature work. Pair with BUG-258: once the tree is reachable at pointer-down, the shared geometry function can answer hit-zones from live bounds too.

### BUG-260 (scene-panel-bound-rows-display-stale-def-value-not-live-slot) — every scene-panel row whose param is covered by a binding (all importer pre-exposed params: camera, sun, environment — plus anything exposed by arming modulation) displays the def's frozen value, not the live slot: scrubs look dead in the panel even when the render responds
**Status:** FIXED 2026-07-19 (lane/scene-audit, Fable): `sync_scene_row_values`'s `resolve` (state_sync.rs) now checks the binding's instance slot first (`binding_id_for_node_param` → `binding_id_for_node_param_in` for tracking instances → `get_base_param`), falling back to the def walk for unbound rows — mirroring the structural build's `display_value`, which was already slot-aware. Conviction test `sync_scene_row_values_tests::bound_row_display_reads_the_binding_slot_not_the_def` (failed pre-fix, green post).
**Severity:** HIGH — the panel's most-touched rows (every imported scene's Camera/Sun/Environment, every param the user arms modulation on) read as dead or snapping-back in real use; the write path worked, so the render moved while the row lied about it.
**Symptom:** scrub an exposed scene row (e.g. an imported glb's Sun Intensity): the preview responds but the row's fill + value never move (or snap back on release). A driver modulating an exposed scene param animates the card but reads frozen in the panel.
**Root cause:** asymmetry between the write path and the per-frame read path. BUG-237's fix (`scene_bound_slot`, inspector.rs) routed bound-row WRITES to the binding's instance slot ("a def write on a bound param is structurally dead"), and the structural build's `display_value` (state_sync.rs) reads the slot — but `sync_scene_row_values`'s `resolve` closure, which overwrites every built row's fill + text EVERY frame, walked only the def. Bound-row writes never touch the def, so the per-frame sync pinned the row to the def's import-time value mid-drag, post-commit, and under modulation. Likely also the real mechanism behind some observations logged under BUG-239 (harness "stale value after real write" — the write was correct, the read path was reading the wrong store; not re-litigated here).
**Fix shape (applied):** slot-first resolve, def fallback — one closure, mirrors `display_value`'s existing rule. The deeper class note: the scene panel now has THREE value-read paths (structural build, per-frame sync, drag guard) that must agree on bound-vs-unbound; a future cleanup should funnel all three through one resolver.

### BUG-262 (mapping-range-affine-drags-unguarded-mid-gesture) — graph mapping range/affine drags (`EffectMappingRange*`/`EffectMappingAffine*`) have no `ActiveInspectorDrag` guard; a mid-gesture full-snapshot acceptance kills their undo entry
**Status:** FIXED 2026-07-19 (lane/undo-redo-baseline, follow-up to `573b50ea`). Last family of the undo-audit cluster C. Added `MappingRange { target, param_id, min, max }` and `MappingAffine { target, param_id, scale, offset }` variants to `ActiveInspectorDrag`; their `apply` arms restore the in-flight reshape through the same `build_mapping_command` + `seed_def_for_project` write `preview_mapping` lands each tick. Guard set in the two Snapshot arms, updated in the two Changed arms, cleared in the two Commit arms (`app_render.rs` mapping trio, ~1662–1830). Proven by two stomp regression tests (`mapping_undo_baseline` in `ui_bridge/inspector.rs`): given the guard a live drag installs, a stale pre-drag snapshot comes back carrying the dragged range/affine, so the commit sees new != old and records one undo. Whole undo-baseline suite 46/46 green; clippy clean.
**Severity:** HIGH — same user-visible signature as the undo-audit cluster C fixed in `573b50ea`: drag a mapping range/scale on the graph-editor mapping sidebar while anything bumps `data_version` (playback, another commit, MIDI phantom), and the gesture records NO undo entry — the "undo doesn't respond" report.
**Symptom:** undo after a mapping-sidebar range or scale/offset drag intermittently does nothing (depends on a snapshot landing mid-drag).
**Root cause:** the last unfixed family of the 2026-07-19 undo audit's cluster C. The drag trios live in `app_render.rs`'s pending_actions loop (`EffectMappingRangeSnapshot/Changed/Commit`, `…Affine…` at ~1650-1800) with `mapping_range_snapshot`/`mapping_affine_snapshot` fields, but no `ActiveInspectorDrag` variant covered them, so the commit's `watched_reshape(binding_id)` read saw the stomped (pre-drag) value: old == new → no command.
**Note on the test:** these two families dispatch through app_render's pending_actions loop, not the inspector host the `undo_baseline` matrix drives, so `trio_cycle` can't reach them. The regression proves the load-bearing fix directly — the `ActiveInspectorDrag::apply` restore that the whole bug reduces to — rather than the set/update/clear wiring (mechanical mirror of the ten cluster-C families). A full app-level harness driving the pending_actions loop end-to-end is still owed if that wiring ever needs coverage.

### BUG-291 (p1-exposure-bindings-cross-wired-onto-the-wrong-node-for-gltf-imports) — some P1-stamped scene-exposure bindings for glTF-imported scenes target the WRONG node, surfacing one item's params under another's selection in the new section-filtered scene panel (slice 2a)
**Status:** FIXED (2026-07-21) — `sections_for_doc_ids` (`crates/manifold-app/src/ui_bridge/state_sync.rs`) now attributes each exposed param by the doc-id PREFIX of its OWN `id` (reading `meta.params` directly), never by walking `meta.bindings` to their targets — fan-out bindings no longer misattribute. Confirmed single-path: `world_sections`/`camera_sections` have exactly one computation site (state_sync.rs), and `build_world_properties` only renders the passed-in `vm.world_sections`, so the fix shape's "second path" concern doesn't apply — no second contributor exists. Regression pins: `sections_for_doc_ids_tests::{world_sections_exclude_the_fanned_out_sun_section, the_lights_own_item_still_includes_its_section}`.
**Severity:** MED (not a crash or data-loss; a real-data correctness/legibility issue surfaced by P2 slice 2a's new render path, not introduced by it)
**Symptom:** Found via headless PNG while building P2 slice 2a (`docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md`): on the `gltfscene` fixture (`cc0__oomurasaki_azalea_r._x_pulchrum.glb`), selecting **World** in the scene panel shows an extra "Sun" section (Mode/Position X/Y/Z/Aim X/Y/Z/…) alongside the real "Environment" section — the light's own params, not World's. Selecting an **Object** shows a spurious "Environment" section (one row, "Ambient") mixed into its Material/Transform sections. Traced with a temporary debug print (removed before commit): `meta.bindings` contains an entry with id `7_pos_x` (the `7_` prefix matches the SUN light's own doc id, 7) whose `BindingTarget::Node.node_id` is `"envmap"` (the bake_environment node's node_id, doc id 1) — i.e. a binding literally minted with the light's id-prefix but targeting the environment node. Similarly a `scene_ambient` binding targets the material node (`mat_0`) but carries `section: "Environment"`.
**Root cause (ISOLATED 2026-07-19, Opus review — corrects the worker's original hypotheses):** NOT a stamping bug. The `7_pos_x → envmap.sun_x` and `1_intensity → hdri_gain.gain` bindings are the glTF importer's INTENTIONAL fan-out macros — one card control driving several nodes (the sun-direction macro moves both the sun light AND the envmap's sun-disc; the env-intensity macro drives both the envmap and the HDRI gain; documented at `gltf_import.rs:~2208`). The bug is entirely in slice-2a's section-attribution helper `sections_for_doc_ids` (`crates/manifold-app/src/ui_bridge/state_sync.rs`): it decides which scene item a param belongs to by walking every binding and taking any whose TARGET node is in the selected item's doc-id set. A fan-out param has extra bindings targeting nodes OUTSIDE its own item, so the sun macro (`7_pos_x`, section "Sun") gets attributed to World (whose set includes the envmap node the macro also targets), and vice-versa. Verified via a throwaway dump of the assembled azalea def: the only "mismatches" are these two known fan-outs; no node_id collision, no cross-wired stamp.
**Fix shape:** attribute each exposed param to its scene item by the doc-id PREFIX of its `id` (P1 stamps `{node_doc_id}_{param}` — a stable, fan-out-proof primary-node identity), NOT by walking bindings to their targets. Localised to `sections_for_doc_ids`. NOTE: a first attempt at exactly this (Opus, 2026-07-19) did not visibly resolve the World→Sun leak in a re-render — either the doc-ids World passes (`e.intensity_addr.node_doc_id`) don't line up as expected, or `build_world_properties`/`world_sections` contributes the "Sun" section by a second path; the leak's final mechanism is NOT confirmed. A resuming session should instrument `sections_for_doc_ids` (print `doc_ids` + returned sections for the World selection) AND check whether `build_world_properties` pulls the sun section independently, before assuming the id-prefix change alone fixes it. Fix belongs in the panel/state_sync (2a), never in `gltf_import.rs`. Out of scope for the render-path slice — flagged, partially diagnosed, not fixed.

### BUG-292 (scene-panel-card-path-rows-write-via-active-layer-not-the-panels-bound-layer) —
<!-- Renumbered 2026-07-21 at the convergence-lane merge: the lane logged these two as BUG-265/266 on 2026-07-19, but main independently allocated 265/266/267 to the inspector drag/tab findings on 2026-07-20 (both since FIXED). Lane ids 265→291, 266→292; the agent-execution-playbook pre-allocation rule exists for exactly this. --> P2 slice-2a scene rows dispatch through the generator card path, which resolves the target through the app's `active_layer` instead of the scene panel's own bound layer — a wrong-layer-write regression vs. the old scene rows
**Status:** FIXED (2026-07-21) — added `GraphParamTarget::GeneratorOf(LayerId)` (`crates/manifold-ui/src/panels/mod.rs`), resolved in `resolve_graph_target` (`inspector.rs`) straight from the carried `LayerId`, never `active_layer`. Every scene-panel row dispatch site in `scene_setup_panel.rs` now emits `GeneratorOf(vm.layer_id)`/`GeneratorOf(self.live_layer_id())` instead of plain `Generator`; every other `Generator` construction site (perform-mode param cards, relight, macros) is unchanged. `LayerId` isn't `Copy`, so `GraphParamTarget` lost its `Copy` impl — a compiler-driven ripple of `.clone()`s through the shared row/dropdown builders (`param_card.rs`, `param_slider_shared.rs`, `ui_root.rs`, `app.rs`, `app_render.rs`); two new non-exhaustive matches in `ui_root.rs` (Ableton-mapped / has-graph-mod card-menu checks) got an explicit `GeneratorOf` arm since those queries are layer-agnostic card state. Regression pin: `inspector.rs`'s `bug_292_scene_row_writes_target_the_panels_bound_layer_not_active`.
**Severity:** MED-HIGH (show-surface correctness regression, introduced by P2 slice 2a, currently only on `lane/scene-panel-exposure-convergence`, NOT on main). A scene-row edit can land on the wrong layer when the scene panel's bound layer isn't the active layer.
**Symptom:** the pre-convergence scene rows routed every write through `resolve_scene_write` → `ui.scene_setup_panel.live_layer_id()` — an explicit invariant documented at `inspector.rs:~251` ("scene rows always [use] `live_layer_id`, never the active-layer context"). Slice 2a's new rows emit the plain `GraphParamTarget::Generator` (a unit variant, `crates/manifold-ui/src/panels/mod.rs:97`), which `resolve_graph_target` (`inspector.rs:~238`) resolves via `resolve_active_layer_index(active_layer, …)`. So when the scene panel is bound to layer X (`live_layer_id`) while the app's `active_layer` is Y, a scrub/commit/toggle on a scene row writes to Y.
**Root cause:** the convergence deliberately reuses the card dispatch path (design §5.6), but the card path targets the active layer while scene rows must target the panel's bound layer. The two were reconciled by `resolve_scene_write`'s explicit `live_layer_id` routing, which the new path bypasses.
**Fix shape:** preserve explicit `live_layer_id` routing for scene-panel param writes — orthogonal to (and must survive) slice 2b's deletion of the synthetic-id funnels. Either carry the panel's layer on the dispatch (e.g. give `GraphParamTarget::Generator` an optional `LayerId`, or add a scene-sourced target variant) or keep a thin scene-write layer-resolution that injects `live_layer_id` before invoking the real card command with the real `param_id` (no synth-id translation). MUST be fixed before slice 2a/2b lands on `main`. Found 2026-07-19 (Opus review of slice 2a).

### BUG-264 (param-step-action-ui-flow-stale-asserts) — `scripts/ui-flows/param-step-action.json` step 6/last assert an "A"/"S" button `under_text: "Amount"`; finds 0 on MAIN (pre-existing, fails identically before the param-drawer unification)
**Status:** OPEN (found 2026-07-19, param-drawer-unification lane). Repro: `cargo xtask ui-snap inspector --script scripts/ui-flows/param-step-action.json` — steps 0–5 pass, step 6 `Count(1)` gets 0 on main AND on the lane.
**Severity:** LOW — acceptance-flow rot, not an app bug: the "A" audio button exists (the flow's own earlier steps arm and use the drawer), the spatial/text query no longer matches anything.
**Symptom:** the param-step-action acceptance flow always fails at step 6.
**Root cause:** unknown — NOT the 2026-07-11 "Amount"→"Sensitivity" slider relabel (retried with `under_text: "Sensitivity"`, still 0). `under_text` semantics in the query engine vs. where the "A" button actually sits (param row, not drawer row) needs someone to read the query matcher, not guess labels. Suspects: stale assumption from the pre-drawer-redesign row layout.
**Fix shape:** read the `under_text` matcher in the ui-snapshot query engine, find what the "A" button is actually "under" in the current layout, update the two asserts — or replace them with the drawer-row-aware query the newer flows use.
### BUG-263 (no-app-level-harness-for-pending-actions-gesture-paths) — mapping drags (and the whole `pending_actions` loop in `app_render.rs`) are untestable end-to-end; only the `ActiveInspectorDrag::apply` mechanism has coverage
**Status:** OPEN (logged 2026-07-19, Fable; deferred by Peter — covered mechanism-level in BUG-262, harness build deferred until more gesture-lifecycle work lands).
**Severity:** MEDIUM (test-coverage gap, not a live defect) — the wiring (set/update/clear of the drag guard in the Snapshot/Changed/Commit arms) has no test that exercises the real path.
**Symptom:** a regression in the `pending_actions` mapping-drag wiring (guard set on Snapshot, updated on Changed, cleared on Commit, `commit_mapping_with_reverse` bookkeeping) would not be caught by any test; the 46-test `undo_baseline` matrix drives the inspector host, which the mapping trios bypass.
**Root cause:** the mapping drags dispatch through the `pending_actions` loop on the monolithic `Application` struct (`app_render.rs` ~1650–1830), reaching for `self.watched_reshape`, `self.mapping_target`, `self.commit_mapping_with_reverse`. No test can stand up enough of `Application` to drive that loop, and no mid-gesture `data_version` bump can be injected.
**Fix shape:** an app-level harness — construct `Application` (or a factored mapping-drag slice of it) headless with a real `EditingService` + `Project`, feed the Snapshot/Changed/Commit action sequence with an injected mid-gesture snapshot acceptance, assert one undo entry. Cheaper now that the pattern (44+2 tests) exists in `ui_bridge/inspector.rs`. Justified when the next gesture-lifecycle bug lands in this area; until then the `apply` round-trip tests (BUG-262) cover the mechanism.

