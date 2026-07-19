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
| BUG-248 | **gui-fps-degradation-persists-after-deleting-heavy-static-glb-layers** | GUI FPS degradation reported after importing/deleting heavy static (non-animated) glb layers; headless import render is clean (430 MB, converges frame 4) — root cause unknown, needs in-app profile — MED |
| BUG-247 | **gltf-import-peak-rss-residual-per-source-node-whole-file-parses-not-deduplicated** | glTF import peak RSS residual (dragon fixture: 1.45 GB measured vs ~0.6–0.7 GB predicted from the shared anim cache alone) — suspected cause: mesh/texture source nodes independently re-parse the whole file with no shared-document cache, unlike the anim cache's dedup — MED |
| BUG-246 | **unguarded-inspector-gestures-race-full-snapshot-acceptance** | audio-gain / mod-config / trim-style drags have no `ActiveInspectorDrag` guard, so a full project snapshot accepted mid-drag (data_version bump from any concurrent Execute) stomps the in-flight value — wrong or missing undo pair on release — MED |
| BUG-243 | **analyzer-false-fires-on-sustained-pads** | sustained pad/swell material fires transient + kick events with zero real hits — analyzer false positives on non-percussive material — MED |
| BUG-245 | **mapping-popover-trim-fields-dont-track-external-edits-after-open** | an open mapping popover's trim min/max fields don't track edits made elsewhere while it's open; reopen reseeds correctly — LOW |
| BUG-244 | **graph-canvas-apply-live-values-skips-non-numeric-param-kinds** | the editor canvas's per-frame live-value overlay is scalar-only; enum/color/vec/table on-face values stay frozen until a graph_version bump — LOW-MED |
| BUG-240 | **scrub-fine-flow-tests-a-retired-shift-fine-delta-drag-gesture** | `scene-setup-scrub-fine.json` asserts a Shift-held "fine drag" ratio on a scene param row that no row family has supported since the card convergence — flow-script rot, not a live-app defect — LOW |
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
| BUG-234 | **ui-snap-script-harness-never-runs-a-content-thread-tick** | the `--script` flow harness's `Step` never invokes `evaluate_all_drivers`/`evaluate_all_envelopes`, so no L3 flow can show a driver/envelope-modulated value changing across `Snapshot`s, for ANY param — LOW-MED, found 2026-07-17 during UX-P3a build |
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

### BUG-240 (scrub-fine-flow-tests-a-retired-shift-fine-delta-drag-gesture) — `scene-setup-scrub-fine.json` asserts a Shift-held "fine drag" ratio on a scene param row that no family has supported since C-P1a, and none supports at all as of C-P1d — found 2026-07-18 during SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1d closing session, re-verifying BUG-236's two flows

**Status:** OPEN — LOW (flow-script rot describing a genuinely-retired gesture, not a live-app defect or regression this session introduced — the gesture was already gone for World's own Fog row since C-P1a; C-P1d's Modifier conversion just removed the LAST family that still had it, making the flow's premise fully, not partially, obsolete).

**Symptom:** after fixing BUG-236's stale `"Outliner"` selector (this session), `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-scrub-fine.json` clears step 2 but fails at step 14: it drags `scene_setup.fog.density_value` (the Fog Density row's VALUE-TEXT cell) expecting the old `ValueDrag` delta-drag semantics (Shift = 0.1× rate), asserting the displayed value lands at a specific fractional read (`"0.30"`) after a plain drag and a smaller one (`"0.03"`) after a Shift-held drag. Fog moved onto the card protocol's `SliderDragState` (absolute-position track drag, no Shift-fine concept) back in C-P1a — the value-text cell isn't even drag-armable anymore (`SliderDragState::try_start_drag` only arms on `ids.track`), so the drag silently lands wherever a click-not-drag would, never reaching the asserted values.

**Root cause:** design intent, not a bug in the shipped code — D4 (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md) deliberately replaced the old per-family delta-drag protocol (Shift = fine, `ValueDrag`) with the card's own absolute-position track-drag (no fine-shift mode) for every family, one family at a time (World/Object in C-P1a/b, Light/Camera in C-P1c, Modifier — the last holdout — in C-P1d). This flow was never updated across any of those four phases; C-P1d's own unit-test twin (`shift_drag_applies_a_tenth_the_delta`) was deleted this session for the identical reason (its target, Modifier's own bespoke delta-drag path, no longer exists to test).

**Fix shape:** either delete `scene-setup-scrub-fine.json` outright (the gesture it tests is gone, not moved — there's no "fine drag" affordance left anywhere in the panel to re-target the flow at), or repurpose it to test something else entirely (e.g. the Shift-held fine-drag semantics of a DIFFERENT, still-live dock like `AudioSetupPanel`'s gain steppers, if that's a flow worth having). Not attempted this session — the flow isn't one of C-P1d's four required re-verify targets (fog/object-scrub/select-updates/modifier-stack), only its stale-text half was in scope via BUG-236.

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

### BUG-234 (ui-snap-script-harness-never-runs-a-content-thread-tick-so-driver-envelope-modulation-is-headlessly-unobservable) — the `--script` flow harness cannot show a driver/envelope-modulated value changing across `Snapshot`s, for ANY param, effect-card or scene-panel — found 2026-07-17, UX-P3a build (`wave/scene-panel-ux-p3-build`), while gating "assign an LFO → the row's value visibly modulates (transport running, two snapshots)"

**Status:** OPEN — LOW-MED (verification infra; blocks any future L3 flow gate phrased as "the value visibly modulates" — same family as BUG-225, different mechanism).

**Symptom:** in `cargo xtask ui-snap gltfscene --script <flow>`, clicking a card's Driver-tab toggle (`PanelAction::DriverToggle`, which does append a real `ParameterDriver` — confirmed via the dispatch log) followed by `Pointer{transport.play}` + `Step{frames: N}` + `Snapshot`/`Dump` never moves the target param's displayed value text, no matter how many frames are stepped or what beat division/waveform the driver uses. Reproduced on both the scene panel's own value cell (`scene_setup.object.roughness_value`) and the SAME param's mirror on the generator card (the plain numeric label next to its T/∿/A strip) — so this is not scene-panel-specific.

**Root cause (suspect, not fully traced):** `ui_snapshot/script.rs`'s own module doc states the harness's drag path "needs no live content thread... the actual clip mutation happens directly on `SceneData.project`" — i.e. `Runner`'s `Step` action advances a deterministic UI clock/filmstrip counter for animation purposes, but does not appear to invoke `manifold_playback::modulation::evaluate_all_drivers`/`evaluate_all_envelopes` (the functions that actually write a driver's per-frame output into `PresetInstance.params`) against the driver's `SceneData.project`. Not fully traced to the exact `Step` handler code this session — the next agent should start at `Runner::advance_frame`/wherever `Step{frames}` is handled in `script.rs` and check whether it calls into `manifold-playback`'s modulation pipeline at all.

**Fix shape:** either (a) wire `Step`'s frame-advance to call `evaluate_all_drivers`/`evaluate_all_envelopes` against the driver-owned `SceneData.project` at the advancing beat (mirroring what the live content thread does every tick), or (b) if that's structurally awkward for the headless harness (no real `PlaybackEngine`), add a dedicated `AutomationAction` (e.g. `EvaluateModulationAt { beat: f64 }`) that a script can call explicitly between `Step`s — cheaper, more surgical, and makes the harness's modulation-evaluation an explicit script step rather than an implicit (and currently missing) side effect of `Step`.

**Workaround used this session:** UX-P3a's "value visibly modulates" gate was proven at the mechanism level instead — `crates/manifold-playback/src/modulation.rs`'s `exposed_generator_param_is_driver_modulated_across_beats` and `..._survives_save_reload_and_still_modulates` construct the same `ToggleNodeParamExposeCommand` a mod-button click dispatches, attach a `ParameterDriver`, and assert `evaluate_all_drivers` moves the value directly — a stronger, faster proof than a UI screenshot diff, but it doesn't exercise the UI render path the way an L3 flow does. `scripts/ui-flows/scene-panel-ux-p3a-expose-modulate.json` still proves everything UP TO driver assignment headlessly (exposure, naming, driver-toggle dispatch) and documents this gap inline via `SCENE_PANEL_UX_DESIGN.md`'s UX-P3a "as built" note.

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

### BUG-261 (card-slider-drags-record-no-undo-entry) — `ParamCommit`'s scene guard consumed `drag_snapshot` before the scene check, so every exposed card slider drag and param type-in recorded NOTHING; plus EnvelopeToggle and 10 unguarded drag families — Peter's 2026-07-19 "undo/redo broken across the UI" report

**Status:** FIXED 2026-07-19 (`573b50ea`, lane/undo-redo-baseline). Four root-cause clusters, all fixed at the root with a 44-test baseline matrix (`undo_baseline` in `ui_bridge/inspector.rs`) enforcing every undoable gesture family + mid-gesture snapshot stomp: **A** the scene guard reads without `take()` (only the scene branch consumes) — this was the 100%-reproducible "sliders don't undo"; **B** EnvelopeToggle routes through `AddEnvelopeCommand` / new `ToggleEnvelopeEnabledCommand` instead of `MutateProject`; **C** ten new `ActiveInspectorDrag` guard variants (AudioGain, EnvelopeTarget/Decay, AudioModShape, AudioModStepAmount, AudioTriggerShape, AudioSendGain, AudioCrossover, RelightParam, AbletonMacroTrim) so a mid-gesture full-snapshot acceptance can't revert the in-flight value; **D** the Param guard restores via `set_base_param` (was effective-only `set_param`; commit reads base). Last unfixed family of cluster C: BUG-262.

**Symptom:** undo/redo "broken, out of order, or just don't respond" across sliders, buttons, toggles, clips, trims — worst during playback (every frame is a data_version bump, so every unguarded drag lost its undo entry mid-gesture).

**Root cause:** gesture lifecycle, four shapes (above). Clips were never broken — the host batching + composite path was sound; baseline proved it and Peter's clip report was the same stomp class seen from the timeline side.

**Class guard:** the baseline matrix runs in the default suite; any NEW snapshot/changed/commit trio added without an `ActiveInspectorDrag` variant fails its stomp test as soon as someone writes its row — the matrix doc-comment says to add one.

### BUG-253 (shininess-and-tone-map-uniform-order-drift-weird-tints) — hand-packed uniform structs disagree with the codegen kernel's PARAMS-order layout — found 2026-07-18 by the P7 fused-vs-unfused relight parity test; matches Peter's live "weird tints with 3D Shading" report

**Status:** FIXED 2026-07-18 (P7 lane, `lane/depth-relight-p7-impl`). `BlinnUniforms` reordered to PARAMS order AND its color moved to the generated layout's offset (four consecutive f32 after `power`, no vec4 alignment pad); `ToneMapUniforms` reordered (curve/mode were after the nit fields). Hand-oracle WGSL kernels unchanged — the gpu_tests now pack hand-layout bytes explicitly instead of reusing the Rust struct. All 155 codegen-path atoms audited by script for the same drift: these two were the only real hits (gradient_ramp flagged but false-positive — Table params lay out specially). Proven by the P7 parity test going green at strict tolerances and both atoms' generated-vs-hand gpu_tests.

**Symptom:** `node.shininess` (used by the 3D Shading relight template's specular, tinted by source) rendered with scrambled uniforms — the kernel read view=(48,0,0) and power=1.0 — producing broad wrong-colored specular: the "sometimes things get weird tints" report. `node.tone_map` similarly read curve/mode from the wrong words, so non-default curve/mode combinations selected the wrong transfer.

**Root cause:** these atoms' `run()` dispatches the codegen-generated kernel (`standalone_for_spec`) but packs uniforms via a hand-written `#[repr(C)]` struct whose field order matched the *old hand kernel*, not the generated PARAMS-order layout. The per-primitive parity tests never caught it because they pack per-kernel bytes independently of the Rust struct — the struct itself was untested.

**Class guard:** the audit script is one-shot; a durable meta-test (assert each `*Uniforms` struct field order == PARAMS order for standalone_for_spec dispatchers) needs source-level reflection we don't have — the real class kill is packing uniforms from ParamDefs generically (param-storage territory). Logged here so the next uniform-struct author greps this entry.

### BUG-249 (scene-panel-modulation-is-decorative-synth-pids-never-resolve-at-runtime) — every modulation affordance on scene panel rows arms state that the runtime silently drops — found 2026-07-18, scene-panel audit after Peter's "modulation not working" report

**Status:** FIXED 2026-07-18 (`lane/bug-249-expose-then-arm`, pending landing) — Peter's design call: option (b), expose-then-arm. `resolve_mod_target` (`inspector.rs`) funnels ALL 19 modulation-family actions: a scene synth id translates to the real exposed binding id (`PresetInstance::binding_id_for_node_param`, bundled + user bindings), materializing the exposure via the same `ToggleNodeParamExposeCommand` on first arm (metadata from the primitive's `ParamDef` table); read-back translates the same way (`scene_row_modulation`, state_sync.rs — including the density row's panel-key/graph-key mismatch). Gate: `scene_row_driver_toggle_arms_a_real_exposed_param` proves driver lands in `inst.params` namespace, read-back reports armed, re-toggle reuses the binding; expose+arm = two undo entries (documented trade-off, mirrors the two clicks). Was: OPEN — HIGH, the "modulation not working" half of the 2026-07-18 scene-panel mess report; shipped green because BUG-234/BUG-239 made modulation unobservable in every flow-script gate.

**Symptom:** clicking a scene row's D/E/A button opens the drawer, shows the armed color, and persists a driver/envelope/audio-mod — but nothing on screen ever modulates, for any family (World/Object/Light/Camera/Modifier).

**Root cause (traced end-to-end):** scene rows are keyed by synthesized ids (`synth_world_param_id` → `scene.{node_doc_id}.{param_key}`, `scene_setup_panel.rs:816`). Only THREE actions get the id_map interception that translates a synth id into a real `SetGraphNodeParamCommand` write (`ui_bridge/inspector.rs:1185/1248/1314` — ParamChanged/ParamSnapshot/ParamCommit). `DriverToggle` (inspector.rs:1372), `EnvelopeToggle`, `AudioModToggle`, `DriverConfig`, and every `AudioModSet*` fall through to the shared card path, which stores the modulation on the generator `PresetInstance` keyed by the synth id — and captures a garbage `base_value` via `inst.get_param(synth_id)` on a param that doesn't exist. At runtime, `modulation.rs` applies drivers/envelopes/audio-mods only via `inst.params.get_mut(param_id)` (lines 109/213/533+); synth ids are never in `inst.params`, so evaluation silently no-ops. Meanwhile `state_sync.rs`'s `row_modulation_for_id` reads armed-state back by the same synth id (state_sync.rs:1654-1660), so the UI confirms the arm it just stored — a closed loop that never touches the render.

**Fix shape:** root fix, not per-action patching: scene-row modulation must target a param the runtime can resolve. Either (a) route ALL card-shaped actions through the scene id_map and store scene-row modulation against the real inner-node address (needs a driver/envelope/audio-mod runtime that can write `RowAddr`-shaped targets — the same write path `SetGraphNodeParamCommand` uses), or (b) make arming a scene row's modulation first materialize a REAL exposed instance param bound to the inner node (the existing `SceneSetupExposeParam` mechanism) and hang the driver off that. (b) reuses the entire existing modulation runtime unchanged and is the shape the design's "same widgets, same systems" directive implies. Blocked-on-nothing; needs a design call on (a) vs (b).

### BUG-250 (scene-panel-enum-value-cells-dead-after-convergence-removed-enum-click-path) — enum rows lost their click interaction in C-P1c/d — found 2026-07-18, same audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`311bfb2a` + `d28bfff4`) — enum click dispatch restored; follow-up root fixes: value_text INTERACTIVE flag, scene writes target panel's own layer, enum/int/bool reads. 15/16 scene flows green; `scene-setup-empty-states.json` stale (PLASMA/NOISE FIELD layers not in gltfscene fixture) — retarget or retire.

**Symptom:** clicking the value cell of any enum row (Light's Cast Shadows On/Off, Shadow Softness Hard/Soft/VerySoft/Contact, Light Mode, Modifier Axis X/Y/Z) dispatches no action and changes nothing — verified headless: a name-targeted click on `scene_setup.light.cast_shadows_value` emits zero PanelActions. The only way to change an enum is to drag the row's slider track.

**Root cause:** the P4/D9 mechanism for enum interaction was `PanelAction::SceneSetupEnumClicked` (click value → dropdown/cycle). C-P1c/d moved enum rows onto the card core's `value_labels` path and deleted every `SceneSetupEnumClicked` producer — the landing report flags the variant as "dead weight" cleanup, not realizing the producers WERE the interaction. `value_labels` in the shared card core is display-only (`format_param_value`, `param_slider_shared.rs:838`); `match_param_row_click` has no variant for a value-cell click, and the card's type-in path explicitly excludes `value_labels` params (`param_card.rs:1085`). So the regression is inherent to the conversion, not a wiring miss.

**Fix shape:** give the shared card core a real enum-cell interaction (click cycles a 2-state, 3+ opens the dropdown — the behavior SCENE_OBJECT_AND_PANEL_V2 D9 committed to), emitting through the existing ParamSnapshot/Changed/Commit trio so the scene id_map interception and undo granularity come free — and so the inspector card's own `value_labels` params gain the same affordance. Do NOT resurrect `SceneSetupEnumClicked` (bespoke, panel-only — the convergence was right to kill it, wrong to replace it with nothing).

### BUG-251 (scene-and-audio-dock-scroll-inverted-vs-every-other-surface) — both docks negate the shared wheel delta — found 2026-07-18, same audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`ea812c24`).

**Symptom:** mouse-wheel/trackpad scroll in the Scene Setup and Audio Setup docks moves content in the opposite direction from the inspector, browser, and every other scrolling surface.

**Root cause:** `window_input.rs` hands the same normalized `dy` to every consumer. Inspector: `apply_scroll_delta(delta)` (inspector.rs:953). Scene dock: `handle_scroll` negates — `apply_scroll_delta(-delta)` (`scene_setup_panel.rs:3863`); `audio_setup_panel.rs:567` has the identical negation (BUG-199's fix apparently copied the wrong sign convention and the scene panel copied the audio panel).

**Fix shape:** drop both negations; add a shared-direction assertion or a flow that scrolls two surfaces and compares offsets so a third panel can't re-introduce it.

### BUG-252 (eight-scene-flow-scripts-dead-at-step-2-on-stale-outliner-assert) — most scene-panel flow coverage silently never ran at the convergence landing — found 2026-07-18, same audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`f101a585` + flow retargets in `d28bfff4`). `scene-setup-empty-states.json` follow-up RESOLVED 2026-07-18 (BUG-249 lane): the flow was never rot — it targets the default `timeline` fixture (PLASMA/NOISE FIELD live there, not in `gltfscene`); verified 7/7 green via `cargo xtask ui-snap timeline --script scripts/ui-flows/scene-setup-empty-states.json`, both empty-state asserts matching for real. Run it against `timeline`, not `gltfscene`.

**Symptom:** 8 of the 21 scene flow scripts (`add-fog-drag`, `eye-toggle`, `fog-undo-removes-fog`, `heldout-merge-snapshot`, `light-cast-shadows-toggle`, `light-intensity-drag`, `numeric-typein-box`, `shadow-softness-dropdown`) fail at step 2 on `Assert Query(text="Outliner")` — the header was renamed "Objects" — so every later step (the actual button under test) never executes. The landing fixed the three scripts in its own gate list and left the rest dead. `scene-setup-empty-states.json` is additionally broken at step 0 (expects a "PLASMA" layer the `gltfscene` fixture doesn't have). With the assert patched, `eye-toggle` (9/9), `fog-undo` (17/17), `numeric-typein-box` (8/8), `heldout-merge` (3/3) pass; `cast-shadows-toggle` and `shadow-softness-dropdown` fail for the real reason (BUG-250); `add-fog-drag`/`light-intensity-drag` fail only their final displayed-value assert (BUG-239 class, dispatch verified). BUG-240 (scrub-fine) is one instance of this same rot, already logged.

**Fix shape:** sweep all scene flows to the "Objects" header + a fixture that satisfies empty-states; then a meta-gate: a landing that claims flow re-verification must run EVERY `scene-*` flow, or a nightly/pre-landing runner that executes the whole `scripts/ui-flows/` directory and fails on any script that can no longer reach its last step.

### BUG-214 (ext-mesh-gpu-instancing-missing-from-supported-extensions-allowlist) — `EXT_mesh_gpu_instancing` is fully implemented but absent from `MANIFOLD_SUPPORTED_EXTENSIONS` — found 2026-07-17, IMPORT_ANYTHING_WAVE Lane W6 extension roadmap audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`a1963ffa`).

**Symptom (would-be):** an asset that lists `EXT_mesh_gpu_instancing` under `extensionsRequired` (spec-legal — an exporter may mark it required when the asset's geometry depends on the instance transforms to exist at all) is rejected at import with "unsupported extension (MANIFOLD does not import this extension)" — even though MANIFOLD fully parses and renders this extension (`gltf_load.rs:278-394`, instance transform composition via `mat4_mul`, non-`F32` accessor and sparse-accessor guards, buffer-bounds checks).

**Root cause:** `MANIFOLD_SUPPORTED_EXTENSIONS` (`gltf_load.rs:34-53`) is the allowlist the `extensionsRequired` veto (`gltf_load.rs:110-116`) checks membership against. It was populated from the crate's typed-feature list plus the raw-JSON-sniffed material extensions (`GLTF_MATERIAL_EXTENSIONS_DESIGN.md` E1-E6); `EXT_mesh_gpu_instancing` support landed separately (`GLB_XFAIL_BURNDOWN_DESIGN.md` D6, BUG-168) and was never added to this array, since none of that work's own fixtures marked it `extensionsRequired`.

**Fix shape:** add `"EXT_mesh_gpu_instancing"` to `MANIFOLD_SUPPORTED_EXTENSIONS`. One-line, no behavior change for any asset that doesn't mark it required (the extension is already read via `node.extension_value(...)` regardless of the required-list check). Verify with a fixture that lists it under `extensionsRequired` rather than only `extensionsUsed` (the local `SimpleInstancing.glb` and hostile-shelf instancing fixtures use the looser `extensionsUsed` form and don't currently exercise this path).

### BUG-213 (no-report-line-for-unimplemented-optional-material-extensions) — MANIFOLD never reads `document.extensions_used()`, so any unimplemented *optional* extension silently degrades with no report line — found 2026-07-17, IMPORT_ANYTHING_WAVE Lane W6 extension roadmap audit

**Status:** FIXED 2026-07-18 on `lane/bug-sweep-250-252` (`a1963ffa`).

**Symptom:** an asset carrying `KHR_materials_diffuse_transmission` (4 Khronos assets: `DiffuseTransmissionPlant/Teacup/Test.glb`, `ScatteringSkull.glb`, currently `xfail:diffuse-transmission-deferred`) imports and renders as a plain opaque material — correct in that nothing crashes or looks broken, but the user gets no indication the translucency effect they authored is missing. The same silent-degrade will recur for any future ratified extension MANIFOLD doesn't implement yet, since nothing currently detects it.

**Root cause:** `document.extensions_used()` — the glTF field listing every *optional* extension an asset carries (as opposed to `extensionsRequired`, which lists only the ones the asset cannot render correctly without) — has zero call sites anywhere in `crates/manifold-renderer/src/node_graph/`. The only unsupported-extension detection MANIFOLD has is the `extensionsRequired` veto (`gltf_load.rs:110-116`), which by spec definition never fires for optional extensions. `ImportReport::report_lines` (`gltf_import.rs:93`, fed today only by the animation-drop paths in `gltf_load.rs`) is the existing channel this should feed into.

**Fix shape:** after import, diff `document.extensions_used()` against a `MANIFOLD_RECOGNIZED_EXTENSIONS` set (the union of `MANIFOLD_SUPPORTED_EXTENSIONS` plus every raw-JSON-sniffed extension read without a required-list entry, e.g. `EXT_mesh_gpu_instancing` — see BUG-214) and push one `report_lines` entry per unrecognized name. This is a one-time generic fix that retroactively covers `KHR_materials_diffuse_transmission` and every future gap, rather than a one-off patch per extension — see `docs/GLTF_EXTENSION_ROADMAP.md` for the full writeup (this is that doc's top-ranked finding). The full `KHR_materials_diffuse_transmission` BTDF lobe itself (as opposed to just naming its absence) is separate, lower-priority follow-on work — not required to close this entry.

### BUG-242 (live-trigger-edge-rearm-hostage-to-shape-release) — dense-material trigger recall collapses because edge re-arm depends on the visual envelope release — found 2026-07-18, causal-detection diagnosis session

**Status:** FIXED 2026-07-18 (same evening; Peter approved). `TransientEdge` now advances on the sensitivity-scaled RAW impulse (no attack/release smoothing) in both consumers — `live_trigger.rs` clip triggers and `modulation.rs` trigger-gates; the conditioned envelope is unchanged for meters/modulation. Measured at DEFAULT shape: edm_kit generic-hit recall 0.204 → 0.714 @ P 1.000; kick_hat byte-identical (0.785/1.000/0.646); 617 unit tests green. Known cost, accepted deliberately: sustained_pad trigger fires 46 → 71 — the envelope was MASKING the analyzer's pad false fires; the trigger layer is now faithful, so BUG-243 (the analyzer-level pad fix) is the sole remaining owner of that symptom.

**Symptom:** on dense material (`self_render/edm_kit_128bpm`, 196 truth hits) the causal trigger path fires only ~21% of hits at the default trigger shape, despite the SuperFlux analyzer firing on 194/196 (99%) at the detection level (proven via `MANIFOLD_ODF_DEBUG` attribution, landed `726d81b4`).

**Root cause:** `TransientEdge::advance(conditioned, 0.5)` re-arms only when the shape-conditioned envelope falls below 0.5, and the default `AudioModShape` release (120 ms) exceeds dense hit spacing (~80 ms), so the edge never re-arms between hits. Measured: `--release-ms 20` on the dumper takes generic-hit recall 0.204 → 0.673 at precision 1.000, timing 4.6 ms; sparse fixture (`kick_hat`) unchanged.

**Fix shape:** decouple trigger re-arm from the modulation shape's release — give `TransientEdge` its own re-arm criterion (fixed short re-arm window ≈ the analyzer's 32 ms refractory, or hysteresis on the RAW impulse rather than the conditioned envelope) so the visual envelope can stay long while the trigger stays fast. Parameter-level stopgap: short release on trigger routes (works today, but couples visual feel to detection).

### BUG-238 (scene-setup-camera-world-light-eye-toggle-reads-as-dead) — the Scene Setup outliner's dimmed eye glyph on Camera/World/Light rows reads as a broken button, not a disabled one — found 2026-07-17, Peter live-testing the dock ("The params and visibility buttons for all of these cameras, world, lights, etc don't work either. They do nothing currently.")

**Status:** FIXED 2026-07-18 — SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1c (`wave/scene-card-convergence`). `EyeSlot::Dimmed` deleted from `scene_setup_panel.rs`; the Camera/World/Light outliner rows now use `EyeSlot::Empty` (draws nothing, keeps the reserved slot width) — Custom Light rows use the same empty slot, Custom Object rows keep the dimmed glyph (Object DOES carry a real `visible` param, this instance just isn't addressable).

**Symptom:** Camera/World/Light rows in the outliner show a dimmed eye glyph in the trailing affordance slot. Clicking it does nothing.

**Root cause:** this is BY DESIGN, not a bug in the click/dispatch path — `SCENE_PANEL_UX_DESIGN.md` D5 deliberately renders a non-interactive dimmed eye glyph on any row whose scene item has no `visible`/enable param (only Object rows carry `scene_object.visible`; Camera/World/Light have no equivalent per their `ParamDef`s traced in `scene_vm.rs` — `SceneLightVm`/`CameraVm`/`EnvironmentVm`/`AtmosphereVm` carry no visibility-style address at all). The row template reserves the SAME slot width for every row and fills it with either a live eye (Object) or a dimmed placeholder (everything else) for visual uniformity (`feedback_no_conditionally_visible_ui`). That uniformity choice reads, to a user clicking it, as a broken button rather than an absent one.

**Fix shape (orchestrator amendment to SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1, overriding D5's dimmed-placeholder choice):** rows with no real visible/enable param get NO eye at all — an empty trailing slot, not a dimmed glyph. Rows with a real param keep the live eye, verified to actually flip the render.

### BUG-237 (scene-setup-camera-world-light-param-scrub-does-nothing-live) — Peter reports Camera/World/Light parameter rows in the Scene Setup panel don't do anything when scrubbed, in the running app — found 2026-07-17, Peter live-testing the dock

**Status:** FIXED 2026-07-18 — SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1c (`wave/scene-card-convergence`), with a render-level mechanism proof (not just dispatch coverage). Root cause was (a) from the "what remains unproven" list below: the bespoke per-family Light/Camera click/drag routing (`build_light_numeric_row`/`build_light_triplet_row`/`build_light_enum_row`/`build_camera_numeric_row`/`build_camera_triplet_row`, and their `light_value_cells`/`camera_value_cells`/`light_steppers`/`camera_steppers`/`light_enum_cells` hit-testing) — deleted and replaced by the shared card row core (`build_light_card_row`/`build_camera_card_row` → `build_param_row`/`match_param_row_click`, the same infrastructure Object rows already proved correct in C-P1b). Hypotheses (b) (stale node ids on a diverged layer) and (c) (content-thread re-render) were checked and ruled out by the render-level proof below — the fix was purely the dispatch-routing layer, as C-P1c's own brief predicted.
**Closing proof (two halves, both required):** (1) dispatch-level — `inspector.rs::scene_card_convergence_tests::light_intensity_commit_writes_the_layer_instance_def_at_root_scope` / `camera_orbit_commit_writes_the_layer_instance_def_at_root_scope` drive a REAL card-row `ParamSnapshot`→`ParamChanged`→`ParamCommit` sequence through `dispatch_inspector` and read the committed value back out of `layer.generator_graph()`'s `EffectGraphDef`, proving the write lands (mirrors C-P1b's Object-family proof; Light/Camera are root-scoped by construction, so there is no group-scope case like Object's D12 bug). (2) render-level — `manifold-renderer/tests/gpu_proofs/bug237_light_camera_commit_render_proof.rs` (`sun_intensity_commit_visibly_changes_the_render` / `camera_orbit_commit_visibly_changes_the_framing`, gpu-proofs feature) renders SceneStarter before/after a def mutation shaped exactly like `SetGraphNodeParamCommand::execute`'s own write, through the real `PresetRuntime`, and asserts + the session LOOKED AT the PNG pair: sun intensity 1.0→8.0 visibly brightens the cube (mean_abs_diff 0.09), camera orbit +π/2 visibly reframes it a quarter-turn (mean_abs_diff 0.06). Together: card row → command → def (proven in `manifold-app`) → def → pixels (proven in `manifold-renderer`) — the full BUG-237 chain, closed with mechanism, not just "the dispatch log looks right."

**Original diagnosis (superseded by the closing proof above, kept for history):** Diagnosed session 1; a session 2 resume (2026-07-17) narrowed the C-P1 integration architecture further but still did not implement the fix — see the design doc's status note (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md) for the full write-up. Short version: `resolve_graph_target(GraphParamTarget::Generator, …)` already resolves the exact `GraphTarget` `SetGraphNodeParamCommand` needs, and that command already self-captures `previous_value` (a real commit-with-prior-value shape) — so neither of D2/D4's `⚠ VERIFY-AT-IMPL` escalation triggers fired; what's missing is ordinary (if nontrivial) wiring: three new branches in `dispatch_inspector`'s `ParamSnapshot`/`ParamChanged`/`ParamCommit` arms that resolve through the scene panel's id map instead of `with_preset_graph_mut` when a synthesized scene param id is seen, plus `ScenePanel` growing `build_param_row`-shaped per-frame state and a real Begin/Changed/Commit drag sequence in place of its current per-move-event dispatch.

**Symptom:** scrubbing a Light/Camera/World (Environment/Fog) numeric row in the Scene Setup panel produces no visible change in the running app, per Peter's live report. No prior test exercised this path past "an action gets built" — `scene_setup_panel.rs`'s own tests only assert a drag/click produces the right `PanelAction`, never that dispatching it changes anything.

**Diagnosis this session (value-level, not dispatch-log-level, per the escalation brief):** added `crates/manifold-app/src/ui_bridge/project.rs`'s `scene_setup_param_changed_writes_light_intensity_to_def` / `..._writes_camera_orbit_to_def` / `..._writes_fog_density_to_def` — each drives the REAL `dispatch_project(PanelAction::SceneSetupParamChanged(...))` entry point against a freshly-materialized `SceneStarter` layer and reads the resulting `EffectGraphDef`'s param value back out (not just that dispatch returned `handled`). **All three pass** — for a fresh, undiverged layer, `RowAddr` (root scope + `node_doc_id` traced off the same `SceneVm::from_def` `state_sync.rs` walks) is correct and `SetGraphNodeParamCommand` genuinely writes the new value into the def, for all three families. This rules out the addressing/dispatch layer (state_sync's `RowAddr` tracing + `project.rs`'s `SceneSetupParamChanged` arm) as the root cause — it is proven correct where BUG-218's suspicion class (stale/wrong scope) would have shown up.

**What remains unproven / next steps:** the def-level write is confirmed sound; the live "nothing happens" symptom therefore lives ABOVE this layer — most likely candidates, none confirmed this session: (a) the bespoke per-family click/drag routing in `scene_setup_panel.rs` (`build_light_numeric_row`/`build_light_triplet_row`/`build_camera_numeric_row`/the World rows' equivalents, and their `light_value_cells`/`camera_value_cells`/stepper hit-testing) has a live-only geometry/hit-test bug no unit test catches (unit tests assert the action shape, not that a simulated pointer at real screen coordinates reaches the right widget); (b) something specific to a layer whose `generator_graph()` has ALREADY diverged from the bundled default (a saved/edited project, unlike this session's fresh-layer test) traces stale node ids; (c) the content thread doesn't re-render after a non-structural `ContentCommand::Execute` for these param families specifically (untested this session — the diagnosis tests only check the UI-thread's local `Project` copy, not a real running content thread). Not narrowed further — no interactive GUI access this session to reproduce the live click.

**Fix shape:** SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md's C-P1 (card-row convergence) deletes the entire bespoke per-family click/drag routing layer named in (a) above and replaces it with the card's own proven `build_param_row`/`match_param_row_click` path — the same infrastructure Object rows use, which Peter has NOT reported as broken. If (a) is the root cause, C-P1 fixes it as a structural side effect of the row swap. If (b) or (c) turn out to be the cause, C-P1 does not fix them and they need separate root-causing. C-P1 was NOT implemented this session (see design doc status) — this diagnosis is the input to whoever executes it next, and that session's acceptance gate must include a render-level (pixels-changed) or def-level (post-swap) proof for these three families specifically, not just dispatch-log coverage.

**Escaped:** SCENE_PANEL_UX P3a/P3b-i (landed `76251784`) — those flows proved dispatch and exposure for these rows but never the render effect, exactly the gap this entry closes the visibility on.

### BUG-182 (hdri-exr-files-fail-or-fail-silently) — Peter's real .exr HDRI files don't work through `node.hdri_source`, despite the atom's claimed .exr support — MED (glb-import lighting / HDRI env_mode)
**Status:** FIXED 2026-07-18 — root cause (`8294ac0a`, lane/hdri-card-binding-clobber — investigated AND implemented by a Kimi K3 agent via cc-fleet, verified + landed by the lead session). The live-app "doesn't do anything" mechanism was neither of the prior suspects: `apply_string_params` fell back to each string binding's declared default for keys absent from `clip.string_params` and re-ran every frame, so the card's empty `hdri_file` binding clobbered `node.hdri_source`'s node-level `path` (set via the graph editor's picker) every frame; `apply_string_defaults` did the same at construction. Fixed at the class level in `preset_runtime.rs`: present-keys-only writes + construction seeding with precedence host value → def node param → binding default; 3 CPU regression tests. **Still open:** (Fix B) the graph-editor 3D viewport renders with `ParamManifest::default()` (`viewport_session.rs:169/:304`) so env_mode can NEVER reach that surface, and its dirty-gated rendering starves async HDRI decodes — HDRI must be judged in the compositor output, not the viewport, until fixed. (Fix C, prior (b)/(c)) card-level error surface + card file picker. Plus a flagged UI follow-up: clearing a card text field (key removal) no longer reverts to the binding default — the clear gesture should commit `Some("")` if explicit clearing is wanted. Peter's live visual confirmation in the compositor still owed.

**Symptom:** Loading one of Peter's own .exr environment maps into `node.hdri_source` (the `hdri_file` string binding, env_mode = HDRI) doesn't produce the expected lit result. No specific failing file is on record yet.

**Investigation this session:** the two committed fixtures (`tests/fixtures/hdri/hdri_float32.exr`, `hdri_half16.exr`) decode cleanly through `load_hdri` with HDR range intact (max R > 3.0, max G > 6.0 — new CPU test `committed_hdri_fixtures_decode_with_hdr_range_intact`) — they don't carry whatever trait breaks Peter's files. Reproduced two of the original suspects directly against real Blender 4.5.2 renders: **DWAA compression decodes fine** (ruled out — the `image` crate 0.25.10's OpenEXR decoder has added DWAA support since the doc-comment era it was flagged in). **Multi-layer/multi-part EXR reliably fails** — a Blender "OpenEXR MultiLayer" render (Combined + Z + Normal passes) reproduces the exact `image::ImageError` the crate's OpenEXR decoder gives when no header has a flat top-level R/G/B triple (`"image does not contain non-deep rgb channels"` — color is nested under a named render-pass layer like `"ViewLayer.Combined.R"` instead). This is the most plausible real-world cause: HDRI creation/compositing workflows commonly default to multilayer export. Committed as `tests/fixtures/hdri/multilayer_unsupported.exr` with a new regression test (`multilayer_exr_reports_an_actionable_cause_not_a_bare_decode_error`).

**Fixed this session:** `load_hdri`'s error path (`hdri_source.rs`) now detects this specific crate error and rewrites it into an actionable message naming the cause in plain terms and suggesting the fix ("re-export as plain OpenEXR, not MultiLayer") instead of forwarding only the crate's internal wording. This closes suspect (a) for the multi-layer case specifically — the next real failing file that hits this path will name its own cause in the log, per the no-silent-fallbacks doctrine. Suspect (a) is NOT fully closed: only the multi-layer/multi-part shape was reproduced and named; other exotic shapes (deep images, luminance-chroma encodings) would currently fall through to the generic `image::open(...): {e}` message, which is at least always path-qualified and forwards the crate's own text, but hasn't been individually verified as user-actionable.

**Still open — not attempted this session (out of Lane W5's scope, which targeted decode-path instrumentation only):** (b) the log line still isn't surfaced anywhere in the running app's UI — a decode error is invisible without a terminal/log file open, so an unsupported file is still indistinguishable from "HDRI does nothing" to a user at the rig. This needs a card-level error state (a small UI feature, not a log-line change) — no such per-node error-surface mechanism exists yet anywhere in the graph editor to hook into (checked: `node.gltf_texture_source` has the identical `log::error!`-only gap). (c) the outer CARD's `hdri_file` field is still a bare text field with no file picker (`GenStringParamClicked` → text input only) — separate from the GRAPH NODE's own `path` row, which now has a working native file picker (2026-07-18, see below).

**Investigation 2026-07-18 (Peter reported live: "Loading a HDRi file into the node and using the HDRi environment mode on the card doesn't do anything"):** two things landed/confirmed this session.

1. **Fixed:** the graph node's own `path` row (`node.hdri_source`, clicked directly in the graph editor — distinct from the card's `hdri_file` field in (c) above) opened a native FOLDER picker (`rfd::FileDialog::pick_folder()`), which cannot select a file at all — the literal cause of "I can't select an .exr file from the file browser." Root cause: `BrowseGraphNodePath`'s handler (`app_render.rs`) always called `pick_folder()` regardless of the param's shape; only `node.image_folder`'s `folder` param actually wants a directory. Now routes on param name (`folder`/`dir` → `pick_folder()`, `path`/`file` → `pick_file()`). Also fixes the equivalent row on `node.gltf_texture_source`/`gltf_mesh_source`/`gltf_morph_deltas_source`/`gltf_skinned_mesh_source`, which had the identical bug. Landed `2ce8bcf2`.

2. **Ruled out as the cause of "doesn't do anything":** ran Peter's actual `kloppenheim_07_puresky_4k.exr` (74MB) through the real production path end to end — `load_hdri` decodes it cleanly (no multi-layer/DWAA failure, no error logged), `render-import` converges (took until frame 158 — the async decode is not instant on a 74MB file), and the rendered result visibly changes: on `tests/fixtures/gltf/hostile/mixamo_like.glb`, mean object-pixel brightness drops 147→104 (max per-channel diff 92/255) between `env_mode=0` (Softbox) and `env_mode=1` + this HDRI, matching the documented "real HDRI reads ~4× dimmer than the softbox default" behavior. Also traced the live-switch path specifically (not just a fresh graph built with `env_mode=1` from frame 0): `execution.rs`'s `compute_live_steps` (which prunes `node.hdri_source`'s branch off `node.switch_texture` while `env_mode=0`) re-runs every frame from live params, and `render_scene`'s IBL cache key hashes the envmap's write-generation + texture identity, so a live `env_mode` toggle on an already-running executor is not structurally different from a fresh build — this was a designed live-perform-mode-switch path, not new code. **So the backend/graph mechanism is confirmed working for this exact file.** Not yet verified: whether the live GUI app itself (as opposed to the headless `render-import` harness) actually shows the change promptly — remaining suspects are (i) the ~74MB decode's real wall-clock latency being mistaken for "nothing happened," or (ii) a live-app-specific UI/content-thread propagation gap in the same class as BUG-136. Needs a hands-on-app repro to settle; out of this session's reach (no interactive display access).

### BUG-241 (stage1-dsp-onset-frontend-misses-loud-real-kicks-track-dependent) — the bake-off Stage-1 DSP drum detector's onset front-end fails to fire on loud, clearly-present kicks on some tracks while nailing them on others — found 2026-07-18, Fable audio-accuracy follow-up (ADTOF bake-off B3 post-mortem)

**Status:** FIXED 2026-07-18 (Fable, same-day follow-up on the lane) — root cause was the B1-era backtrack refinement itself: `librosa.onset.onset_backtrack` against the broadband RMS envelope walks each flux peak to the preceding loudness minimum, which on dense material is the PREVIOUS hit's tail, not this hit's attack — peaks landed 60–140ms early, track-dependently (sparse mixes have a clean pre-attack RMS dip, dense ones don't; hence apricots fine / feel_the_vibration broken). Neither of the triage's killed suspects; found by stage-wise accounting (curve-peak → threshold → backtrack → merge) on the apricots-vs-feel minimal pair: backtrack was the stage that took feel from 8/16 to 2/16. Fix: backtracking removed entirely from `detect_onsets` (band-own-energy backtrack was also measured — strictly worse than none); the flux peak's frame-center time already carries B1's timing correction. Post-fix any-onset kick recall: feel 8/16, inhale_exhale 11/14, tears 6/10, bad_guy 10/17 (up from 2/16, 2/14, 1/10, 8/17), apricots unchanged 15/16; kick-LABELED recall tracks closely (8/16, 9/14, 6/10, 9/17, 11/16). All 121 eval tests green incl. the exact-truth timing tests (no reintroduced bias). **Owed:** ~~bake-off scoreboard re-run~~ DONE same day: 75-track re-run (with the 0.075 threshold tune) read electronic kick 0.311 → **0.493** vs ADTOF's unchanged 0.702 — the bug had overstated the gap by ~0.18; verdict unchanged (0.493 < the 0.5 bar, all classes still trail). Full re-read incl. the hat-labeling regression it surfaced: AUDIO_ANALYSIS_ACCURACY_DESIGN.md §"B3 kick-line re-read".

**Symptom:** on the 5 `tests/fixtures/audio/<track>_<bpm>bpm/drums.wav` electronic stems, kick recall against the fixtures' own `drums_time_s` labels (±50ms), measured directly through `manifold_audio.stage1_dsp_detection.detect_drums_stage1` (Event attr is `.type`, NOT `.label` — a wrong-attr spot-check produced spurious 0s during triage, don't repeat): apricots **15/16** (front-end works), feel_the_vibration **2/16**, inhale_exhale **2/14**, tears **1/10**, bad_guy 8/17 (bad_guy separately misaligned — 15.0s stem vs 13.24s mix, its own README caveat, exclude it). Recall measured against ANY detected onset regardless of class label, so this is the ONSET stage failing, upstream of the cluster-labeling weakness the B3 verdict already documented. Kicks confirmed real: all 16 feel_the_vibration labeled times sit at ~2x track-median RMS. The 58 onsets it does find land 60-140ms from the kicks — densely detects other content, steps around the kicks.

**Root cause:** UNKNOWN. Two plausible causes TESTED AND KILLED this session (record so nobody re-runs them): (1) whole-track normalization crushed by a single giant peak — feel_the_vibration has a max-amplitude 1.0 transient at 12.83s, but clipping it changed recall 2->2, and analyzing only the clean first 12s still gave 3/15; (2) sub-kick-without-click (flux blind to low-freq-only onsets) — killed by spectrum check: caught vs missed kicks have near-identical profiles (all 64-77% energy <150Hz, centroids 1300-2677Hz; the MISSED feel is BRIGHTER than the CAUGHT apricots). Neither normalization-vs-peak nor kick timbre. Suspects for the real hunt: per-band flux ODF weighting/threshold vs these tracks' overall spectral balance; the multi-band picker's adaptive threshold; frame-center vs frame-start conversion (a ~25ms bias was fixed in B1 — verify it's applied on this path).

**Fix shape:** dedicated debug session on the Stage-1 onset front-end (`manifold_audio/stage1_dsp_detection.py` + the `spectral.py` per-band flux it calls) — instrument the per-band ODF and adaptive threshold on a minimal pair (apricots works vs feel_the_vibration fails), dump why the kick frames don't cross threshold. Whole-track normalization is a suspected wrong design choice for transient onset detection (Peter's instinct, shared) — a causal/local adaptive baseline is the likely correction, but PROVE the mechanism before changing it. Re-run the bake-off after: Stage-1's real kick numbers are unknown until this is fixed, and the B3 verdict's kick line should be re-read then (not the whole verdict — labeling was independently weak). Full triage narrative: the 2026-07-18 Fable follow-up chat.

### BUG-224 (scene-setup-close-button-bypasses-shared-toggle-action) — the Scene Setup dock's × button doesn't visibly close the panel — found 2026-07-17, Peter live-testing the dock ("The close button on the scene panel also doesn't work")

**Status:** FIXED 2026-07-17 (`lane/panel-interaction-bugs`) — `ScenePanel::handle_event`'s `Click` arm for `close_id` now returns `(true, vec![PanelAction::OpenSceneSetup])` instead of calling `self.close()` directly, mirroring `AudioSetupPanel::handle_event`'s already-correct close arm (which has its own passing unit test, `close_toggles_audio_dock`, documenting the "one toggle path" pattern). Regression test added: `scene_setup_panel::tests::close_button_click_routes_through_the_shared_toggle_action`. L3 proof: new `scripts/ui-flows/scene-setup-close-button.json` (opens the dock, asserts "Outliner"/"×" exist, clicks "×", asserts both "Outliner" and "Scene Setup" are gone — green against the `gltfscene` fixture; the dispatch log shows `dispatched OpenSceneSetup (structural=true)` firing on the × click, confirming the correct action now flows).

**Symptom:** clicking the × in the Scene Setup dock's title bar had no visible effect — the dock stayed open with all its content.

**Root cause:** `ScenePanel::handle_event`'s `Click` arm for `close_id` called `self.close()` directly and returned `(true, Vec::new())` — no `PanelAction`. That only flips the panel-local `open` bool. It never told the app to (a) reset `ui_root.layout.scene_setup_width` back to 0 — the dock's actual screen-space allocation, owned separately by `UIRoot`/`toggle_scene_dock()`, not the panel — so the region kept its footprint; (b) run any rebuild — `app_render.rs`'s dispatch loop (`self.needs_rebuild = true` etc.) only fires off a returned `PanelAction`, and an empty action Vec means nothing downstream ever reacts; (c) sync the header's Scene toggle-button highlight (`header.set_dock_toggle_state`), which only `toggle_scene_dock()` calls. The sibling `AudioSetupPanel` never had this bug — its `handle_event`'s close arm already emits `PanelAction::OpenAudioSetup`, the same action the header button and Escape key use, routing through the single owning toggle path (`ui.toggle_audio_dock()` + `DispatchResult::structural()` in `ui_bridge/mod.rs`). `ScenePanel` was supposed to mirror `AudioSetupPanel` throughout (SCENE_SETUP_PANEL_DESIGN D2 says so explicitly at several other call sites) but this one spot diverged — parity was never actually implemented for the close button, just assumed.

**Escaped:** SCENE_OBJECT_AND_PANEL_V2's P4/P5 (outliner + properties panel, landed 2026-07-17, `e78d97d2`) rebuilt this panel's body extensively but never added a close-button test or flow — the existing `scene-setup-add-fog-drag.json`/other flows all open the dock and never close it via the × (Escape/header-toggle paths, which route correctly through `toggle_scene_dock()`, were exercised instead). No flow or unit test ever drove `handle_event(Click { close_id })` before this session.

### BUG-223 (scene-setup-dock-scroll-state-updates-but-never-repaints) — mouse-wheel scroll over the Scene Setup (and Audio Setup) dock updates internal scroll state but the screen never visibly moves — found 2026-07-17, Peter live-testing the dock ("Scrolling is still not working on the scene panel"), reopens BUG-199 (closed 2026-07-17 same day, `2f7c6331`) for the real-mouse path BUG-199's own headless proof never reached

**Status:** FIXED 2026-07-17 (`lane/panel-interaction-bugs`) — `window_input.rs`'s `primary_mouse_wheel` dock-scroll branch (added by BUG-199) now sets `self.needs_rebuild = true` after `process_scroll`, matching the inspector branch's own precedent three lines below it. Stale "the dock rebuilds every frame" comments in `scene_setup_panel.rs`/`audio_setup_panel.rs` corrected in the same diff.

**Symptom:** scrolling the mouse wheel over an overflowing Scene Setup (or Audio Setup) dock body does nothing visible — content stays put.

**Root cause:** BUG-199's fix (landed earlier the same day, `2f7c6331`) routed dock wheel-scroll through `ui.input.process_scroll()` → `UIEvent::Scroll` → the panel's `handle_scroll` → `ScrollContainer::apply_scroll_delta` — which correctly updates the panel's internal `scroll_offset`. But nothing in that branch sets any of the flags `app_render.rs`'s `apply_ui_frame_invalidations` (`ui_frame.rs`) reads to decide whether to actually re-run `ui_root.build()` (`needs_rebuild`/`needs_structural_sync`) or `rebuild_scroll_panels` (`scroll_dirty.any()`) — and `rebuild_scroll_panels` wouldn't touch the dock's tree region even if triggered (it's scoped to the timeline viewport/layer-headers, not the Base-tier dock region `ScenePanel::build_docked` writes into). Without a triggered rebuild, `build_docked` — the only place that bakes `scroll_offset` into the tree's actual node Y-positions (`self.scroll.offset_content(tree, -offset)`) — never runs again, so the state changes but the screen doesn't. BUG-199's own code comment stated the (false) assumption directly: "the docks rebuild every frame, which is enough to re-apply the new scroll offset" — untrue under the app's dirty-flag-gated rebuild.

**Escaped:** BUG-199's landing verification (`scripts/ui-flows/audio-dock-scroll.json`/`scene-setup-add-fog-drag.json`, both green then and still green now) never caught this because the headless `--script` harness has its own masking bug: `ui_snapshot/script.rs`'s `Runner::advance_frame` builds its `UiFrameSignals` from `self.needs_structural_sync || ui.inspector.skip_to_settled(&mut ui.tree)` — and `skip_to_settled` returns `true` on the very first `advance_frame` call of ANY script (it force-settles the inspector's intro pop/spawn tweens), forcing one free full rebuild regardless of what the dispatched gesture actually flagged. Both BUG-199 flows dispatch their `Scroll` gesture as their first (or, for the fog-drag flow, an early) mutating action, so they always got this free rebuild and could never distinguish "the fix sets needs_rebuild" from "the fix sets nothing." Confirmed empirically this session: a two-scroll probe script showed the first scroll's rebuild (riding the free settle) but the second scroll (after `needs_structural_sync` — itself a separate harness-only bug: never reset to `false` after firing once, so it stays permanently "stuck true" and masks every later step too) produced no further content movement query mismatch, and reading `ui_frame.rs` directly confirmed the gating logic. This harness gap (the free first-frame rebuild, and separately the stuck-true `needs_structural_sync`) is real and not fixed by this session — logged here as a known blind spot for future scroll/rebuild-signal bugs in ANY dock/panel tested via `--script`, not just this one.

### BUG-216 (feedback-loop-into-final-output-freezes-at-depth-one) — a `node.feedback` loop whose blend output feeds `system.final_output` directly silently degrades to a one-frame loop with per-frame stderr spam — found 2026-07-17 during the depth-relight look probe (headless `PresetRuntime` path)

**Status:** FIXED @ f2684402 (DEPTH_RELIGHT_DESIGN.md P4, D6(b)).

**Symptom:** author a standard feedback graph (`mix → feedback → transform → … → mix`, mix out → `final_output`). Trails never accumulate — every frame shows only the current source — and stderr prints `texture swap out<->in failed (unbound or shadowed slot) — feedback state did NOT advance this frame` per frame.

**Root cause (observed headless, in-app chain path unverified):** the boundary output's resource carries a borrowed shadow (`MetalBackend::replace_texture_2d` — the mechanism `PresetRuntime::install_target` uses to install the host's canvas texture over `final_output.in` each frame); when the loop's capture source shares that resource, `MetalBackend::swap_texture_2d` (`crates/manifold-renderer/src/node_graph/metal_backend.rs:624`) refuses the ping-pong because a borrowed shadow is present. Its comment says "caller falls back to copies", but the executor's `late_capture` (`crates/manifold-renderer/src/node_graph/execution.rs:1689`) had no copy fallback — it printed the error and dropped the frame's capture, freezing the loop at depth 1.

**Fixed:** `late_capture` now falls back to a format-bridge copy (blit via `copy_texture_to_texture` when producer/state formats+dims match, `resize_sample` when only dims differ — same contract as `node.feedback`'s own `copy_with_format_bridge` in `temporal.rs`) landing `in`'s fresh content into `out`'s persistent texture, instead of dropping the frame. The eprintln now fires only when no copy is possible at all (missing texture, or a genuine format mismatch neither blit nor resize can bridge — a narrower, documented residual gap, not the common case). Regression test: `node_graph::execution::bug_216_gpu_tests::feedback_direct_to_final_output_accumulates_trails` (gpu-proofs feature) builds the exact repro shape (`mix` Add-blending a constant source against its delayed output, wired straight to `mix→feedback.in` AND `mix→final_output`, `final_output`'s resource installed via `replace_texture_2d` to reproduce the real borrowed-shadow condition) and asserts the readback value compounds monotonically frame over frame; verified failing (frozen at the alloc-frame value forever) with the fix reverted.

### BUG-218 (modifier-commands-splice-at-dead-group-output-vertices-port) — the D6 modifier-stack commands still target the pre-D12 splice point, so "Add modifier" silently no-ops on every real grouped object — found 2026-07-17, SCENE_OBJECT_AND_PANEL_V2 P5 flow-script verification

**Status:** FIXED 2026-07-17 (lane/scene-bugfixes) — `walk_mesh_modifier_chain`/`splice_modifier_into_chain` (`crates/manifold-editing/src/commands/graph.rs`) now resolve the group's `node.scene_object` via the group output's `object` producer (`find_scene_object_at_group_output`, mirrors `scene_vm.rs::find_scene_object_in_group`) and walk/splice against ITS `vertices` port instead of the dead `system.group_output` `vertices` port. Test fixture `object_group_scene` rebuilt to the real D12 shape (mesh → modifiers → `node.scene_object.vertices`, `scene_object.object` → group_output); all existing insert/remove/move inverse-pair tests now exercise the real shape and pass. Verified end-to-end: `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-modifier-stack.json` — modifier rows now appear after Add/Bend clicks and undo restores correctly (previously a silent no-op).
**Escaped: lane/scene-bugfixes · caught-by: landing workspace sweep** (`manifold-app::ui_bridge::project::tests::insert_modifier_on_scene_starter_lands_in_the_object_group_body`) — the initial fix above only handled the import shape (scene_object INSIDE the group); it broke the OTHER legitimate D12-era shape, `migrate_scene_object_wires`'s output (e.g. the bundled `SceneStarter.json`), where the minted `node.scene_object` stays a ROOT-level sibling of the mesh group and the group still exports `vertices` directly (`scene_vm.rs:617-618`). Root cause of the escape: the focused gate covered `manifold-editing` + `manifold-renderer` only; this consumer test lives in `manifold-app`. Fixed same day: `walk_mesh_modifier_chain`/`splice_modifier_into_chain` now branch on shape per-call — if the group output's `object` port resolves to a `node.scene_object` (import shape), walk/splice against its `vertices` input; otherwise (migrated/starter shape) walk/splice against the group output's own `vertices` port directly (the original pre-fix behavior, restored for this shape only). New inverse-pair tests `insert_modifier_on_migrated_shape_splices_at_group_output_and_undo_restores` + `remove_and_move_modifier_on_migrated_shape_splice_at_group_output_and_undo_restores` (`manifold-editing`) cover the migrated shape directly; `insert_modifier_on_scene_starter_lands_in_the_object_group_body` (`manifold-app`) now passes. Gate widened to include `manifold-app` for this fix.

**Symptom:** clicking a modifier "Add" chip (e.g. "Twist") in the Scene Setup dock's Properties body dispatches `SceneSetupAddModifier` → `InsertMeshModifierCommand::execute`, which returns without mutating anything — no modifier appears in the stack, no undo entry is pushed, no error, no log. Reproduced via `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-modifier-stack.json` against the real `cc0__oomurasaki_azalea_r._x_pulchrum.glb` fixture: the dispatch log shows `SceneSetupAddModifier(LayerId, 15, "node.twist_mesh")` firing, but the rebuilt tree still shows zero modifier rows (just the "Add modifier:" chip row, unchanged).

**Root cause:** `InsertMeshModifierCommand::execute` (`crates/manifold-editing/src/commands/graph.rs:3796`) does `let out_id = nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?.id;` then `walk_mesh_modifier_chain(nodes, wires, out_id)` — walking backward from `system.group_output`'s **`vertices`** port. That port existed pre-D12 (the group interface re-exported `vertices`/`material`/`transform` directly). D12's `build_import_graph`/`AddSceneObjectCommand` shape wires the mesh chain into `node.scene_object`'s own `vertices` INPUT instead, and the group boundary now only exports `object` (`gltf_import.rs`: `group_wires.push(wire(scene_object_id, "object", out_id, "object"))` — no `vertices` wire to the group output at all). `walk_mesh_modifier_chain` finds no producer for `out_id`'s `vertices` port, returns `None` on its very first lookup, and the whole command silently declines — matching the D6 "unparseable chain refuses the insert" tolerance doctrine by construction, but for the WRONG reason (a stale splice point, not a genuinely malformed chain). `RemoveMeshModifierCommand`/`MoveMeshModifierCommand` (same file, share `walk_mesh_modifier_chain`/`splice_modifier_into_chain`) have the identical bug. This is exactly the class of gap `feedback_no-silent-fallbacks` warns about, except the "fallback" here is an editing command's own stale assumption, not a rendering path.

**Fix shape (not applied this session — out of P5's blast radius, which was scoped to `manifold-ui`/`manifold-app`/`scene_vm.rs` only):** re-target `walk_mesh_modifier_chain`'s entry point at the group's `node.scene_object`'s own `vertices` INPUT port instead of `system.group_output`'s `vertices` OUTPUT port (find the scene_object the same way `scene_vm.rs::find_scene_object_in_group` now does — via the group_output's `object` producer — then walk backward from ITS `vertices` input, not the group boundary's). `splice_modifier_into_chain`'s re-wire target needs the same swap (the newly-spliced modifier's output must feed `scene_object.vertices`, not `group_output.vertices`). Needs its own inverse-pair unit tests using the D12 group shape (scene_vm.rs's own test fixtures in this session's diff are a ready template — `grouped_scene_object_def`). Also update this doc's Status line and the P5 landing report's escalations list once fixed.

### BUG-212 (duplicate-scene-object-string-bindings-dangle-on-imported-mesh) — `DuplicateSceneObjectCommand`'s fresh NodeIds break the "Model File" string binding on a cloned glTF-imported object's mesh source, so the clone has no path and loads no geometry — found 2026-07-17, SCENE_OBJECT_AND_PANEL_V2_DESIGN P3, via the render-path proof (`duplicate_demo_pair_renders_original_then_original_plus_offset_copy`, manifold-renderer gpu-proofs)

**Status:** FIXED 2026-07-17 (lane/scene-bugfixes) — `deep_clone_with_fresh_ids` now also collects an (old NodeId, new NodeId) map across the whole cloned subtree; `DuplicateSceneObjectCommand::execute` uses it to clone every `string_bindings` entry whose target falls inside the duplicated subtree (same `id`/`label`/`default_value`, re-targeted at the clone's fresh NodeId), via `resolve_target_instance` at the same undo-unit boundary `RenameSceneObjectCommand`'s D5 sweep uses; undo restores the whole `string_bindings` vec (whole-snapshot convention). `bindings`/`exposed_params` remain excluded per D11. The gpu-proofs demo test `duplicate_demo_pair_renders_original_then_original_plus_offset_copy` (`gltf_import.rs`) had its manual post-clone path-stamping workaround replaced with the real fix's shape (clone matching `string_bindings` entries via a node-id map), proving the actual mechanism rather than a demo-only stopgap.

**Symptom:** click Duplicate on an imported object (e.g. a glTF mesh). The command succeeds (undo/redo, wiring, `objects` count, transform offset all correct — proven by
`duplicate_scene_object_command_clones_grouped_object_with_fresh_ids_and_undo_restores`), but the clone renders nothing: its mesh source node has no geometry.

**Root cause:** the importer's "Model File" card control is one `StringBindingDef` per file-dependent node (`BindingTarget::Node { node_id, param: "path" }`, `default_value` = the resolved file path) — addressed by stable `NodeId`, not doc id. `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source`/`node.gltf_texture_source` never carry a literal `path` param in the def; it only ever arrives via this binding's `default_value` resolving against the node's `NodeId`. `DuplicateSceneObjectCommand`'s `deep_clone_with_fresh_ids` (`crates/manifold-editing/src/commands/graph.rs`) correctly mints a FRESH `NodeId` on every cloned node per D11 ("fresh NodeIds make cloned bindings dangle by construction") — but D11's own text was written about CARD exposes (`exposed_params`/`UserParamBinding`, performer-controlled sliders), which are legitimately meant to dangle. `string_bindings` is a different, non-performer-facing infra mechanism (keeps every file-dependent node in an import pointed at the same physical file) that hits the exact same "addressed by NodeId" mechanism unintentionally — dropping it isn't a deliberate tradeoff, it silently breaks mesh loading for the shipped importer's own object shape.

**Fix shape:** `DuplicateSceneObjectCommand` should also clone (with the SAME `default_value`, re-targeted at the clone's fresh NodeId) every `string_bindings` entry whose target NodeId falls inside the duplicated subtree — mirroring what it already does NOT do for `bindings`/`exposed_params` (those stay excluded, per D11). Needs `PresetInstance`/`preset_metadata` access at the SAME undo-unit boundary the D5 rename sweep (`RenameSceneObjectCommand`) already reaches via `resolve_target_instance` — likely the same shape, appending fresh `StringBindingDef`s instead of rewriting `section`. A synthetic-mesh object (no `string_bindings`, e.g. `AddSceneObjectCommand`'s cube) is unaffected and needs no fix. Demo-only workaround used to produce a real render proof without this fix: `render_once`'s caller in `gltf_import.rs`'s `duplicate_demo_pair_renders_original_then_original_plus_offset_copy` manually copies the source's resolved `path` onto the clone's mesh nodes post-clone — NOT present in the shipped command, do not mistake it for the fix.

### BUG-185 (e6-texture-completion-invalidates-two-stale-goldens) — `CompareSpecular.glb` and `CompareVolume.glb` genuinely regress in `glb_conformance_sweep` after E6's texture-completion sweep wires `specularTexture`/`specularColorTexture`/`thicknessTexture` for the first time — expected consequence of fixing the gap, not a shading bug
**Status:** FIXED with BUG-211's landing (2026-07-17) — the visual-confirmation call this entry was waiting on was made by eyeballing both renders: CompareSpecular's golden re-baselined (glossy spheres correct), CompareVolume's `region_green_minus_red_above` region moved from the bowl's upper interior (legitimately thin/clear once E6 honored `thicknessTexture`) to the thick lower interior where the Beer-Lambert tint lives (measured G-R 10.13; floor 8 → 6 for margin, ~0 with volume off). Manifest note carries the same rationale.

**Symptom:** `cargo test -p manifold-renderer --test glb_conformance --features gpu-proofs` fails two previously-passing `expect_pass` cases: `CompareSpecular.glb: golden mismatch: mean_abs_diff 3.34 > tol 2` and `CompareVolume.glb: region mean G-R 3.05 <= floor 8`. Both goldens/region-checks were captured/calibrated in G-P7 (`37c81fba`, 2026-07-15) — before E6 existed — against the OLD factor-only rendering (no `specularTexture`/`specularColorTexture`/`thicknessTexture` support).

**Root cause (confirmed by rendering + inspecting each asset's actual texture, not guessed):** both assets carry the exact texture E6 now newly honors, and turning it on legitimately changes the render:
- `CompareSpecular.glb`'s "glTF Logo Specular" material sets `specularColorFactor=[10,10,10]` (deliberately >1, meant to be scaled down by a texture) with BOTH `specularTexture` and `specularColorTexture` pointing at the SAME image (a red damask decorative texture whose ALPHA channel is a separate, spatially-varying strength mask averaging ~0.49, and whose RGB is reddish, not white). The stale golden was rendered with the factor `[10,10,10]` applied UNIFORMLY (texture ignored) — a strong, broad, near-white highlight. The new (correct) render modulates per-texel by that alpha mask and tints by the reddish RGB, producing a dimmer, spatially-varying, reddish highlight — verified visually (`/tmp/compare_specular_e6.png` vs the checked-in golden) and confirmed against the raw extracted PNG's actual RGBA content.
- `CompareVolume.glb`'s "glTF Volume" material's `thicknessTexture` (G channel) has a near-zero band that lands almost exactly on the bowl's THIN GLASS RIM (physically correct — rims are thinner than the base) — exactly where the test's calibrated region `[0.502,0.458,0.555,0.49]` samples. The stale calibration assumed the flat `thicknessFactor=0.75` applied everywhere (no texture), giving strong uniform Beer-Lambert tinting in that region; the new (correct) render has near-zero attenuation at the thin rim, dropping the measured region's G-R from >8 to 3.05.
- Also found and FIXED in the same session (not the cause of either failure above, but adjacent and real): `wire_map_texture`'s `map_tex_cache` was keyed by `tex_index` ALONE (`gltf_import.rs`) — safe for the base five maps (any shared index always wants the same decode, e.g. ORM) but wrong once one extension family (KHR_materials_specular here) legally reuses the SAME image index under TWO DIFFERENT decodes (linear-alpha vs sRGB-rgb). Fixed by keying on `(tex_index, color_space, channel_mode)`.

**Fix shape:** this is a re-baselining call, not a code fix — visually confirm the NEW renders are correct (they read as spec-compliant per the diagnosis above), then regenerate `tests/fixtures/gltf/goldens/compare_specular.png` and move `CompareVolume.glb`'s region (to a thicker part of the bowl) or its G-R floor to match the texture-aware rendering, same discipline as every prior family's Compare-asset re-certification in this design doc. Whoever lands E6 next should do this as the final "certification" step E6's own brief describes (manifest re-classification + status-doc arithmetic) — flagging per CLAUDE.md's "bug found but not fixed this session" rule.

### BUG-215 (conformance-sweep-panics-on-duplicate-mat-0-handle) — a glTF material named like its own inner handle (`"mat_0"`) panics `Graph::add_node_named` on duplicate handle — found 2026-07-17 during IMPORT_ANYTHING_WAVE Lane W6's landing gate, independently rediscovered and fixed by Lane W5

**Status:** FIXED same session. Not caused by Lane W5's own work (the .exr HDRI decode fix) — found only because the wave's gate requires the conformance sweep green before landing, and it wasn't. Lane W6 diagnosed this first (Fable-advisor-confirmed) but is analysis-only by design and left it OPEN with a fix-shape recommendation; Lane W5 independently hit the same red gate, consulted its own Fable advisor per the wave's escalation clause, and re-verified the mechanism directly against the code and the asset's own JSON before landing the fix below. The two diagnoses agree on the root cause; see W6's original write-up (superseded by this entry) for the fuller three-handle account.

**Symptom:** `cargo test -p manifold-renderer --features gpu-proofs --test glb_conformance` panics on `MetalRoughSpheresNoTextures.glb`:
```
thread 'glb_conformance_sweep' panicked at crates/manifold-renderer/src/node_graph/graph.rs:137:13:
Graph::add_node_named: duplicate handle 'mat_0/mat_0' (already mapped to NodeInstanceId(9), just tried to remap to NodeInstanceId(11)). Handles must be unique within a graph.
```

**Root cause (confirmed by reading the code and the asset's own JSON, not guessed):** `MetalRoughSpheresNoTextures.glb` has 98 materials literally named `"mat_0"` through `"mat_97"` (verified: parsed the GLB's JSON chunk directly). `build_object_group` (`gltf_import.rs`) names its inner `node.pbr_material` handle `format!("mat_{k}")` — for object 0, `"mat_0"`. `unique_group_name(m.name.as_deref(), k, ...)` — used for BOTH the object's group-box handle AND, since SCENE_OBJECT_AND_PANEL_V2 P3 (commit `1a5786cb`, landed on `origin/main` before this lane branched), the inner `node.scene_object` node's handle (D6: "the object IS its scene_object node; the name is its handle") — took the material's raw name verbatim, so for this asset it also produced `"mat_0"`. Both nodes live inside the SAME group body; `flatten_groups` prefixes each inner handle with the group's own handle (`{group_handle}/{inner_handle}`), so the group handle `"mat_0"` containing an inner `"mat_0"` (the pbr_material) AND the scene_object node ALSO handled `"mat_0"` (identical to the group's own handle) both flatten to the literal string `"mat_0/mat_0"` — two distinct nodes, same flattened name. `Graph::add_node_named` (`graph.rs:137`), invoked at graph-load time (`graph_loader.rs`), rejects the second one. Pre-P3, `build_object_group` created no `scene_object` node at all (verified: `git show 1a5786cb^ -- crates/manifold-renderer/src/node_graph/gltf_import.rs`), so this collision is new in P3 and was never exercised by the conformance sweep until this session's run (P3 landed same-day, just ahead of this lane).

**Fix:** `unique_group_name` (`gltf_import.rs`) now also dedupes against a new `collides_with_object_group_inner_handle(name, k)` helper naming the object's own deterministic inner-handle vocabulary (`mesh_{k}`, `mat_{k}`, `pose_{k}`, `skinmesh_{k}`, `morphweights_{k}`, `morphdeltas_{k}`, `morphblend_{k}`, `transform_{k}`, `anim_{k}`, `tex_{k}`, and the literal `"output"` group-output boundary handle) — a colliding material name now gets the same numeric-suffix treatment `unique_group_name` already gives sibling name collisions. This differs from W6's originally-recommended fix shape (rename the colliding *inner* node instead of the group/scene_object name) — both approaches close the collision; this one keeps `unique_group_name` as the single place that owns group-name uniqueness. Regression test `material_named_like_its_own_inner_handle_does_not_collide` (synthetic `GltfImportSummary` with a material named `"mat_0"`, asserts the flattened graph has no duplicate handles) — verified it fails without the fix and passes with it; this also closes W6's first follow-on (a GPU-free regression test in the default sweep). Conformance sweep reran green after this fix.

**Still open (W6's second follow-on, not attempted here):** `RenameSceneObjectCommand` (`manifold-editing/src/commands/graph.rs:3390`) sets a scene object's handle with no visible collision guard against sibling inner handles — a performer renaming an object to e.g. `"mesh_0"` at runtime could still hit the same class of panic on next graph rebuild, since that path doesn't go through `unique_group_name`.

### BUG-210 (add-scene-object-command-emits-pre-migration-legacy-wires) — `AddSceneObjectCommand`'s `catalog_default` still emits `mesh_k`/`material_k`/`transform_k`-shaped wires into `render_scene`, which no longer reads them post-SCENE_OBJECT_AND_PANEL_V2 P2 — found 2026-07-17 landing P1+P2

**Status:** FIXED @ 6e8b00ba — P3. `catalog_default`'s spliced group now binds mesh/material/transform through an inner `node.scene_object`, exposing a single `Object` interface port wired to `render_scene`'s `object_k` port; `add_scene_object_command_bumps_count_builds_group_and_undo_restores` updated to assert the new shape (5 body nodes incl. scene_object bind, single `object` interface output, `object_2` top-level wire).

**Symptom:** clicking "+ Object" in the scene panel bumps `render_scene`'s `objects` count and creates a group node, but the group wires its mesh/material/transform outputs to the legacy `mesh_k`/`material_k`/`transform_k` ports — `render_scene` v2 (P2, `object_{i}`-only surface) has no such ports anymore, so the added object is invisible and casts no shadow.

**Root cause:** `AddSceneObjectCommand` (`crates/manifold-editing/src/commands/graph.rs`) was not touched by P1/P2 — its `catalog_default` (the JSON template spliced in on Add) predates the Object-wire model. `RemoveSceneObjectCommand`'s mirror-image break (found by the same landing's full workspace sweep, when a concurrent lane's same-day BUG-193 fix collided with P2's port deletion) was fixed in the same landing; Add was flagged by a Fable advisor consult but deliberately left alone — it's SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P3's own committed deliverable ("`AddSceneObjectCommand`'s `catalog_default` emit scene_object-shaped objects"), not a same-session patch.

**Fix shape:** exactly P3's brief — retarget `catalog_default` to splice a `node.scene_object` (or its enclosing group, matching D5's placement rule) wired to the next free `object_{i}` port, instead of the legacy triplet. No new mechanism; P3 was always going to build this.

### BUG-211 (conformance-harness-advancing-clock-cant-converge-animated-imports) — every animated Khronos asset "never converged after 300 frames (last non-black fraction 0.0000)" in glb_conformance while rendering perfectly — found 2026-07-17 (surfaced by BUG-207's lane running the full gpu-proofs sweep; diagnosed same session)

**Status:** FIXED (this entry's landing commit, `lane/bugfix-210-conformance-frozen-time` — branch named before BUG-210 was claimed by the scene-object session's finding).

**Symptom:** 6 assets (CesiumMan, Fox, BrainStem, RiggedFigure, RiggedSimple, AnimatedMorphCube) failed `glb_conformance_sweep` with "never converged … last non-black fraction 0.0000", plus 3 golden mismatches (AlphaBlendModeTest, CompareSpecular, RecursiveSkeletons). Deterministic across runs — not device-contention flake.

**Root cause:** two independent staleness effects, neither a render bug. (1) `render_asset`'s convergence loop advances `time`/`beat` every frame and requires 3 byte-identical frames before it ever measures blackness; since GLTF_ANIMATION A1–A4 imports auto-play, an animated asset re-poses every frame, never goes byte-stable, and `last_fraction` keeps its 0.0 initializer — the reported "black" is a phantom (the same loop in `src/bin/render_import.rs` had the identical gap). (2) The golden mismatches were legitimate reframes: BUG-205 moved skinned assets' bbox into bind-skinned space and BUG-206's per-axis fit reframed wide/tall assets — the goldens were captured pre-fix (RecursiveSkeletons' old golden was visibly cropped top and bottom; the new one frames the full object).

**Fix:** freeze the clock across both convergence loops (`time`/`beat`/`anim_progress` constant, `frame_count` still advancing for the decode/io paths — byte-stability still catches late texture swaps, which was the loop's real job); `render-import` gained `--time SECONDS` to pick the animation moment. Six goldens regenerated and individually eyeballed (walking CesiumMan, posed BrainStem/RiggedFigure, fully-framed AlphaBlendModeTest wall, CompareSpecular spheres, uncropped RecursiveSkeletons pillar); the other 64 within-tolerance golden rewrites the update pass produced were deliberately reverted, not committed. Sweep runtime dropped 701s → ~180s (no more 300-frame timeout per animated asset).

### BUG-207 (materialless-skinned-mesh-silently-imports-static-at-node-scale) — a rigged mesh with no material renders invisible (~100× wrong-sized) with nothing in the report — found 2026-07-17 (Fable seam-hunt session; hostile-shelf probe, Blender-exported materialless rig)

**Status:** FIXED @ f3358d00 (`lane/bugfix-207-default-material-skin`).

**Symptom:** import succeeds, report shows `default_material_vertex_count > 0`, frame renders black (lit fraction exactly 0.0000). The geometry IS imported (BUG-171's `DEFAULT_MATERIAL_MESH_PARAM = -2` path loads all vertices — verified: static DefaultOnly loader returns the full 372) but as a STATIC mesh at the wrong size.

**Root cause:** two stacked seams. (1) `gltf_load.rs` skin/morph/animation resolution keys `nodes_by_material.get(&Some(material_index as usize))` — materialless primitives live in the `None` bucket and the synthetic default-material entry carries the `u32::MAX` sentinel, so `skin`/`morph` NEVER resolve for it (the synthetic entry is also hardcoded `skin: None`); `find_skinned_node_for_material` has the same `Some(...)` comparison. (2) With the skin lost, the object falls to `node.gltf_mesh_source`, which world-transforms by the mesh node's transform — a transform glTF skinning IGNORES (§3.7.3.3), and which Blender exports expect to be ignored (skinned POSITIONs are written in bind-meters). Measured on the probe fixture: mesh-node-world bbox 0.01 tall vs bind-skinned 1.0 tall — the render is 1% of the framed size. No report line fires (the only signal is a console warn, whose text — "v1 does not import these" — is itself stale since BUG-171 DID make them import).

**Fix:** the default-material bucket (`nodes_by_material`'s `None` key) is now a first-class key, not a special case. `gltf_load.rs`'s three duplicated skin/morph/animation resolution blocks (previously inline in `gltf_import_summary`'s per-material loop, each keyed `Some(material_index as usize)`) are factored into shared `resolve_skin_for_key`/`resolve_morph_for_key`/`resolve_animations_for_key` functions parameterized by `key: Option<usize>`; the per-material loop calls them with `Some(idx)` exactly as before, and the synthetic default-material entry now calls the SAME functions with `key: None` instead of hardcoding `skin: None, morph: None, animations: Vec::new()`. A new `primitive_material_matches` helper (sentinel-aware `Option<usize>` comparison) replaces every bare `primitive.material().index() != Some(material_index as usize)` in `flatten_skinned_node`, `find_skinned_node_for_material`, `find_mesh_node_for_material`, and `load_gltf_morph_deltas`'s primitive loop. `gltf_import.rs`'s `build_object_group` now translates `m.material_index == DEFAULT_MATERIAL_SENTINEL` to `DEFAULT_MATERIAL_MESH_PARAM` (-2) for BOTH `node.gltf_skinned_mesh_source`'s and `node.gltf_morph_deltas_source`'s `material_index` params (mirroring the pre-existing static-mesh branch) — without this, the sentinel `u32::MAX as i32` would collide with -1, the params' own "unset" convention. Both primitives' `material_index` `ParamDef` ranges widened to `(-2.0, 1024.0)` and their `run()` gates changed from `material_index >= 0` to `material_index >= 0 || material_index == DEFAULT_MATERIAL_MESH_PARAM`, translating -2 back to `DEFAULT_MATERIAL_SENTINEL` before calling `load_gltf_skinned_mesh`/`load_gltf_morph_deltas` — the same two-sentinel-space split `gltf_mesh_source.rs` already used. Merged after BUG-208 landed on main; the sentinel translation for `node.gltf_morph_deltas_source` was reconciled against BUG-208's new `skinned: bool` parameter on `load_gltf_morph_deltas`/`node.gltf_morph_deltas_source` (both changes compose — the sentinel selects WHICH primitives contribute, `skinned` selects WHICH coordinate space, orthogonal concerns). The stale "v1 does not import these" warn (`assemble_import_graph`, BUG-171 made it false) reworded to describe what actually happens.

**Regression:** `tests/fixtures/gltf/hostile/mixamo_like_nomat.glb` — a materialless variant of the existing `mixamo_like.glb` skinned rig, added to the hostile shelf (picked up automatically by `hostile_fixtures_*`). `hostile_fixtures_render_within_framing_invariants` (GPU) is the direct regression proof: measured lit fraction 0.1820, byte-for-byte identical to `mixamo_like.glb`'s own 0.1820, not cropped — confirmed via a temporary debug print, reverted before commit. Confirmed the bug reproduces pre-fix even more severely than "renders black": with the fix reverted, the harness panics (`assembled graph has no node.gltf_skeleton_pose with a duration_s param`) because the materialless object never got a skeleton pose node at all — it fell all the way through to the static-mesh branch, as the root-cause analysis predicted. Full gates (post-merge with BUG-206 and BUG-208): `cargo test -p manifold-renderer --lib` 1369/1369; `cargo test -p manifold-renderer --lib --features gpu-proofs hostile` 3/3; `cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings` clean; `cargo nextest run --workspace` 3564/3564. `docs/node_catalog.json` regenerated for the two widened param ranges.

### BUG-208 (skin-plus-morph-drops-morph-silently) — an object with both a skin and morph targets imports with its morph animation gone and no report line — found 2026-07-17 (Fable seam-hunt session; code-confirmed, no fixture yet)

**Status:** FIXED @ 50db9369 (`lane/bugfix-208-skin-morph`).

**Symptom:** body skinning animates, face/blendshape animation is absent; the import report says nothing (D9 "every unmapped feature is a report line, never a silent drop" violation).

**Root cause:** `gltf_import.rs` `build_object_group`: `morphed_vertices_source` was only built when `skinned_vertices_source.is_none()` (A3's documented out-of-scope call, "re-derive if a future asset needs both") — the skip branch pushed no report line and dropped the morph wiring entirely. A second, deeper seam sat behind the documented one: `node.gltf_morph_deltas_source`'s loader (`load_gltf_morph_deltas`) always world-transforms its deltas by the mesh-owning node's world matrix — correct for the rigid (non-skinned) path, where `node.gltf_mesh_source` world-transforms its base vertices the same way, but WRONG for a skinned object, whose base vertices (`flatten_skinned_node`) are emitted in UNTRANSFORMED bind-pose/local space per the A2 doctrine (a skinned object's positioning comes entirely from its joint palette). Naively chaining the two without addressing this would have put the deltas and the base vertices in different coordinate spaces.

**Fix:** real fix shipped, not just the minimum report line. glTF applies morph THEN skin (§3.7.2): `node.morph_targets_blend` now chains between `node.gltf_skinned_mesh_source`'s vertices and `node.skin_mesh`'s `in` when an object carries both. `load_gltf_morph_deltas` gained a `skinned: bool` parameter (threaded through a new `node.gltf_morph_deltas_source` `skinned` Bool param, set by `gltf_import.rs` from `skinned_vertices_source.is_some()`) that skips the world-transform entirely (identity matrices) for the skinned case, keeping the deltas in the same untransformed space as the skinned base vertices — `flatten_skinned_node` and `flatten_primitive_morph_deltas` share the identical triangle-index expansion, so per-vertex order lines up 1:1 without further changes. New report line names the composed chain and cites BUG-208. Regression: `tests/fixtures/gltf/hostile/skin_morph.glb` (Blender armature-skinned cylinder + keyframed "Bulge" shape key, both a skin and morph targets with animation channels for each) — new CPU test `skin_and_morph_combination_composes_instead_of_dropping` asserts the report line, the `node.morph_targets_blend` → `node.skin_mesh` wire, and the deltas source's `skinned` param; the existing hostile-shelf sweep tests (`hostile_fixtures_assemble_validate_and_build`, `hostile_fixtures_merge_into_existing_scene`, `hostile_fixtures_render_within_framing_invariants`) pick the new fixture up automatically and pass (framing/lit-fraction invariants hold). `cargo test -p manifold-renderer --lib --features gpu-proofs morph`/`skinned` green.

### BUG-206 (import-framing-crops-elongated-objects) — tall/thin imports overflow the synthesized camera's frame — found 2026-07-17 (first synthetic hostile-fixture run: Blender-generated skinned cylinder, 10:1 aspect, cropped top AND bottom at default framing)

**Status:** FIXED @ e182a391.

**Symptom:** an elongated import (bind-skinned bbox ~1.0 tall × 0.2 wide) renders correctly sized but overflows the default frame vertically — lit pixels touch both top and bottom edges. Compact objects (azalea, helmet, skeleton_animated post-BUG-205) frame fine.

**Root cause:** `assemble_import_graph`'s `distance = 2.2 * radius` (gltf_import.rs, near the BUG-165/169 near-clip comment) uses the bbox half-DIAGONAL as `radius`. For an object dominated by one axis the diagonal barely exceeds that axis, so the frame's vertical span (`2 * distance * tan(fov_y/2)` ≈ 1.08 × height at fov_y 0.9) contains the object with almost no margin — and camera tilt (0.3) plus perspective (near surface projects larger) push it past the edges.

**Fix:** `distance` is now `(2.2 * radius).max(per_axis_fit)`, where `per_axis_fit` is the max over the three bbox axes of `(extent/2) / tan(fov_y/2) * 1.15` (`gltf_import.rs::assemble_import_graph`, ~line 2001) — `fov_y` hoisted into one `let` shared by both the framing-distance computation and the camera node's `fov_y` param, so the two can't drift. The render aspect is unknown at import time, so the horizontal half-angle is conservatively treated as equal to the vertical one (square-aspect assumption, never under-frames a wider-than-tall render). The `2.2 * radius` floor keeps every compact asset's framing identical to before (golden-stability guarantee — confirmed by the 56-asset conformance sweep staying 56/56). Regression: `hostile_fixtures_render_within_framing_invariants`'s `EDGE_XFAIL` list (previously `["mixamo_like.glb"]`) is now empty and the sweep additionally checks phase 0.0 (rest pose, the worst case for an elongated rig) alongside the original 0.25 — both phases pass un-xfailed.

### BUG-205 (skinned-import-double-transform-and-wrong-bbox-space) — imported rig renders as a tiny speck, feet cropped below frame — found 2026-07-17 (Peter, skeleton_animated.glb, "the skeleton is TINY")

**Status:** FIXED @ 3aeafe4a (`lane/bugfix-skinned-import-scale`).

**Symptom:** an animated+rigged glb imports (post-BUG-204) but renders ~12px tall at the synthesized framing distance — correctly shaped and animating, just ~40× too small; after fixing that, the skeleton still sat low with its feet cropped below frame.

**Root cause:** two sites treated a skinned object as static, both violating the importer's own A2 doctrine (skinned positioning comes ENTIRELY from the joint palette, glTF 2.0 §3.7.3.3). (1) `resolve_object_animation` walks the mesh node's ancestor chain; skeleton_animated.glb animates `Bip01` — an ancestor ABOVE the joint tree whose static scale (0.0254, the FBX inch→meter conversion) is already inside the joint worlds via `joint_root_world` — so the resulting rigid `node.gltf_animation_source` wired into the object's transform_3d applied the whole chain a second time: 0.0254² ≈ 1/1550 of authored area. (2) `summarize_node` bboxed skinned primitives at mesh-node-world positions (y 0.36..2.22), a space glTF skinning ignores; the render lives at bind-pose-skinned positions (y -0.57..1.20), so the camera framed/recentered a box the mesh never occupies. CesiumMan/Fox never caught either: no scaled/animated ancestor above their rigs, near-coincident spaces.

**Fix:** `build_object_group` skips the rigid animation source when `skinned_vertices_source` is `Some`; `summarize_node` bboxes skinned primitives through new `bind_pose_skin_matrices` (static joint world × inverseBind — the same product `node.skin_mesh` applies). Known remaining approximation, unchanged by this fix: an ancestor that genuinely ANIMATES above the joint tree is still sampled statically (`joint_root_world`); composing an animated prefix into the pose path is a tracked gap, not regressed here. Regressions: `skinned_import_gets_no_rigid_animation_source` + `skinned_import_summary_bbox_is_in_bind_skinned_space` (both CPU, real fixture); headless snapshot goes speck → full-frame (non-black fraction 0.0001 → 0.0716); snapshot test now writes its PNG before asserting so failures leave the frame on disk.

### BUG-204 (animated-glb-import-rejected-by-retrigger-card-lint) — every animated or rigged glb fails at import: "card param '…_retrigger' is marked is_trigger but its binding targets '….trigger_count' … which is not a trigger-typed param" — found 2026-07-17 (Peter, skeleton_animated.glb)

**Status:** FIXED @ 6d7cac31 (`lane/bugfix-gltf-retrigger-trigger-type`).

**Symptom:** `[Import] glTF import failed … assembled graph failed validation: card param 'pose_0_retrigger' is marked is_trigger but its binding targets 'pose_0.trigger_count' (node.gltf_skeleton_pose), which is not a trigger-typed param` (and the same for `anim_0_retrigger` / `node.gltf_animation_source`). Static meshes import fine; anything with an animation clip or a rig is rejected — the exact assets A4 shipped to make performable.

**Root cause:** a four-day collision between two landings. Card lint (d) (`validate.rs`, `c6d809f0` 2026-07-13, graph-tooling P4) requires an `is_trigger` card param to bind to a `ParamType::Trigger` inner param. GLTF_ANIMATION_DESIGN.md A4 (`2a9e8808` 2026-07-17) then added the Retrigger card param bound to `trigger_count` — declared `ParamType::Int` on all three animation nodes (the pre-lint way of writing the monotonic-counter convention). A4's gate never ran the assembled import def through `check_card_lints`, so the conflict only surfaced at app import time.

**Fix:** `trigger_count` flipped from `Int` to `Trigger` on `gltf_animation_source`, `gltf_skeleton_pose`, `gltf_morph_weights` — the declaration the convention already meant (`image_folder`'s `next`/`prev` precedent); runtime semantics unchanged (Trigger params carry the same Float counter, coercion identical). Regression: `animated_and_rigged_import_passes_card_lints` runs `tests/fixtures/gltf/skeleton_animated.glb` through `assemble_import_graph` + `check_card_lints` (now `pub(crate)`) — verified red on the Int declaration, green on Trigger. Node catalog regenerated.

### BUG-198 (ui-automation-key-event-has-no-global-undo-seam) — headless `AutomationAction::Key { key: Z, modifiers: { command: true } }` never triggers Undo — found 2026-07-17 during SCENE_SETUP_PANEL_DESIGN.md P5 (modifier stack)

**Status:** FIXED @ 6318c9fb — `BUGFIX_WAVE_2026_07_17_DESIGN.md` Lane 4 (`lane/bugfix-ui-automation-harness`). The headless `Runner` now owns a real `manifold_editing::undo::UndoRedoManager` (same 200 cap); every `ContentCommand::Execute`/`ExecuteBatch` sent over the harness's `content_tx` is drained and `record()`-ed after each step (never re-executed — `AppEditingHost`/`ui_bridge` already mutate `data.project` synchronously before sending). The `Key` arm intercepts Cmd+Z/Cmd+Shift+Z and dispatches real `undo()`/`redo()`; any OTHER modifier-bearing key with no seam now FAILS LOUDLY instead of returning "ok" (mirrors the `Text` arm's fail), killing the silent-no-op class. New flow `scripts/ui-flows/scene-setup-fog-undo-removes-fog.json` proves it end to end (add fog → Cmd+Z → Density row gone → Cmd+Shift+Z → Density back); `scene-setup-modifier-stack.json`'s 4× Cmd+Z now genuinely walks `undo_count` down (9→8→7→6) instead of no-op'ing.

**Symptom:** Cmd+Z in the real app is a global menu-bar/window-level shortcut (`app_render.rs`'s `M::Undo` menu-action handling); the headless script driver's `Key` step only calls `UIRoot::key_event` → `InputSystem::process_key`, which does nothing unless a widget currently holds text-input focus (`process_key` only pushes a `KeyDown` event when `self.focused_node(tree)` is `Some`). No panel/dock in this codebase focuses a text field by default, so every headless `Key Z` step is a silent no-op: it "succeeds" (status `ok`, no error) while doing nothing. Confirmed directly this session: `scripts/ui-flows/scene-setup-modifier-stack.json` sends 4× Cmd+Z after inserting 2 modifiers + 1 param drag + 1 reorder — the modifier count is unchanged afterward (verified via a temporary post-undo assertion that failed with "found 2", then removed once the cause was confirmed, per the "never claim a green gate you didn't get" rule).

**Root cause:** `AutomationAction::Key`'s headless synthesis path only reaches `UIRoot`, which owns per-widget key handling (text fields, keyboard-drag-nudge, etc.) but NOT the app-level global shortcut table — that lives in `Application`/`app_render.rs`, outside anything `UIRoot::key_event` can reach, mirroring the exact gap `AutomationAction::Text` already documents in its own doc comment ("No headless injection seam exists yet: text editing lives entirely in `Application::text_input`... `UIRoot` can't reach").

**Fix shape:** extend the P3 live-door (`UI_AUTOMATION_DESIGN.md`, the TCP JSON-lines thread already wired for live injection) or the headless script driver itself to recognize `Cmd+Z`/`Cmd+Shift+Z` and dispatch the SAME `M::Undo`/`M::Redo` path `app_render.rs` uses, OR give `AutomationAction::Key` a documented escape hatch that calls `EditingService`'s undo directly against the driver's own `SceneData.project` (mirroring how `Pointer`/`Drag` already bypass the content thread and mutate `SceneData.project` in-process). Until then: every phase gate that wants "undo restores state" proven headlessly must prove it at the COMMAND level (`execute()` + `undo()` unit tests, byte-equal graph assert) — which P5 already does for all three new commands — never by trusting a `Key Z` step's "ok" status.

### BUG-192 (ui-automation-under-text-flat-card-rows) — `SelectorQuery.under_text` never resolves against `param_card.rs` slider rows — found 2026-07-16/17 building GLTF_ANIMATION_DESIGN.md A4's L3 flow

**Status:** FIXED @ 6318c9fb — `BUGFIX_WAVE_2026_07_17_DESIGN.md` Lane 4 (`lane/bugfix-ui-automation-harness`). Confirmed root cause via test-first repro (two new synthetic-tree tests in `automation.rs`, both failing before the fix): (1) the zero-match case this entry names — `param_card.rs`'s generator rows parent flat to the literal tree root (`parent: None`), so a `None`-parented node's ancestor chain is empty and neither the literal-ancestor nor common-ancestor check could ever fire; (2) a second, previously-undetected cross-match risk — `layer_header.rs`'s real shape nests every row's container under ONE shared outer scroll clip, and the old "any shared ancestor, however far up" walk could tunnel through that shared container and match a DIFFERENT row's label (`under_text_walks_ancestors`'s existing test never had a shared outer ancestor to expose this). Fixed `under_text_matches` to climb outward one enclosing level at a time, stopping at the nearest same-parent sibling carrying ANY text (build order puts a row's own label before the rest of that row) and only climbing further when a level has no textful sibling at all — resolves flat-sibling rows without ever crossing into an unrelated row. Both new tests green (`under_text_resolves_flat_sibling_rows`, `under_text_layer_header_shared_scroll_clip_does_not_cross_match`), plus the existing `under_text_walks_ancestors` (tight-container case) unaffected. The `gltf-clip-scrub-retrigger.json` widget-id/`nth` workaround is left in place per the directive's "keep the workaround if flaky" clause — not re-pointed this session.

**Symptom:** A ui-flow script using `{"type": "Button", "under_text": "<row label>"}` (or `under_text` alone) against any `gltf_scene`/`gltfanimscene`-style generator card returns zero matches, even when the labeled row and its value widget both visibly exist (`{"text": "<row label>"}` alone resolves fine). Confirmed empirically: `under_text: "Rate"` and `under_text: "Rate"` + `type: "Button"` both return 0 matches against `gltfanimscene`'s Rate/Clip/Loop Mode/Retrigger rows, while `type: "Button"` alone (no `under_text`) returns 126 matches in the same tree.

**Root cause:** `under_text_matches` (`crates/manifold-ui/src/automation.rs:367`) resolves "under" via a shared-ancestor walk, documented against `layer_header.rs`'s row shape (`layer_header.mute` and its "PLASMA" label are direct children of one tight per-row container `111`). `param_card.rs`'s slider rows don't have that per-row container — every row's label, value badge, and slider body are flat siblings directly under ONE shared card-content parent (verified via `ui-snap --script`'s own dump: node `71` is every row's `parent`, from `Rate`'s label at row y=229 through `Camera Orbit` at y=379). Under the doc comment's own stated "common-ancestor" semantics this should make EVERY row's widgets satisfy `under_text` for EVERY other row's label simultaneously (since they all share ancestor `71`) — instead it resolves to zero for all of them, meaning either the implementation doesn't match its own documented semantics for this topology, or the automation resolver's `nodes[]`/`parent_id` view diverges from what `--dump` prints as `p=`. Not diagnosed further — the fix needs stepping through `under_text_matches` against a live `param_card` dump, which wasn't done this session (schedule pressure on the A4 landing).

**Fix shape:** either (a) fix `under_text_matches` so it genuinely implements "nearest labeled row" (e.g. stop the ancestor walk at the first node with ANY text, not just the queried one, so two same-parent rows don't both "share" an unrelated ancestor) — the general, root-level fix; or (b) give `param_card.rs` genuine per-row containers (a structural change with its own blast radius across every existing card-targeting flow). (a) is the smaller, correctly-scoped fix. No `#[ignore]`-able Rust test exists yet since this is a `manifold-ui`/`ui-automation` behavior, not covered by the crate's own `#[cfg(test)]` suite — a repro belongs in `manifold-ui/src/automation.rs`'s own test module, built against a synthetic flat-sibling tree matching `param_card`'s actual shape (the existing `under_text_walks_ancestors` test at `automation.rs:503` only proves the tight-container case).

**Workaround (shipped):** `scripts/ui-flows/gltf-clip-scrub-retrigger.json` (A4's L3 flow) uses `AutomationTarget::Widget(<id>)` — a raw widget id read from a same-session dump — for the Rate slider drag (no literal text on a slider body), and `{"text": "▶", "nth": 1}` for the Retrigger click (disambiguating three same-text buttons by position, confirmed stable across repeated runs of the same deterministic fixture). Both work but are fragile to future card-layout changes in a way a working `under_text` wouldn't be.

### BUG-183 (fusion-coverage-baseline-slipped) — `fusion_coverage_baseline` fails on main: 32 bundled presets fuse, floor asserts ≥33
**Status:** FIXED 2026-07-17 (Sonnet, BUGFIX_WAVE_2026_07_17_DESIGN Lane 5) — floors lowered/raised to the new bundled reality (presets ≥32, regions ≥56, atoms ≥240) in `crates/manifold-renderer/src/node_graph/freeze/proof.rs`'s `fusion_coverage_baseline`, comment rewritten citing `a065dec4`. Measured at tip `1a161d91`: 32 presets / 56 regions / 243 atoms — matches the root-cause investigation below exactly. Test green.

**Symptom:** `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::freeze::proof::fusion_coverage_baseline` fails with "expected ≥33 bundled presets to fuse, got 32". Isolated as pre-existing: the agent restored `mix.wgsl`/`mix_body.wgsl` to their HEAD content and the test failed identically, so the BUG-181 alpha fix is not the cause — some landing on or before `02c5fbd5` dropped one preset out of fusion coverage without lowering (or noticing) the baseline.

**Root cause (identified 2026-07-17, Fable, verified by running the test + `git show`):** NOT a partition regression — commit `a065dec4` (2026-07-16) unbundled eight 3D-infra presets to `assets/reference-presets/`, and CinematicScene (which fused — its fused-WGSL golden was deleted in that same commit) left the bundled set, dropping the bundled fused-preset count 33 → 32. Fusion itself RATCHETED UP across the same window: measured at tip `9a7a7fa2`, 32 presets / 56 regions / 243 atoms vs the P6 floors 33/55/225. The earlier "do NOT just lower the floor" instruction assumed a regression and is superseded by this evidence.

**Fix shape:** update the floors to the new bundled reality (presets ≥32, regions ≥56, atoms ≥240 with the test's usual churn headroom) and rewrite the floor comment citing `a065dec4`. Directive: BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 5.

### BUG-196 (is-multiple-of-clippy-debt-gltf-import-render-scene) — `cargo clippy --features gpu-proofs --tests -- -D warnings` fails on 6 pre-existing `manual_is_multiple_of` lints outside this phase's touched files — found 2026-07-17 during RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1

**Status:** FIXED (bugfix-wave-2026-07-17 Lane 3) — all 8 sites found by `rg "% 4 != 0|% 2 == 0"` across `gltf_import.rs` (7, count drifted up from the 5 originally named here) and `render_scene.rs` (1) rewritten to `.is_multiple_of()` / `!….is_multiple_of()`. No behavior change. `cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings` and `cargo clippy -p manifold-renderer -p manifold-gpu --features gpu-proofs --tests -- -D warnings` both clean.

**Symptom:** `cargo clippy -p manifold-renderer -p manifold-gpu --features gpu-proofs --tests -- -D warnings` fails with 6 `clippy::manual_is_multiple_of` errors: `gltf_import.rs:2368`, `:2510`, `:5191`, `:5196`, `:5235` (all `while <len>.len() % 4 != 0 { … }` padding loops) and `render_scene.rs:4493` (`if band % 2 == 0 { … }`). The plain scoped clippy P1 actually gates on — `cargo clippy -p manifold-renderer -p manifold-gpu -- -D warnings` (no `--features gpu-proofs --tests`) — is clean; these lints only surface once the gpu-proofs feature's test binaries pull that code into a lint pass, which P1's phase brief didn't require but running the full gpu-proofs test suite for the four touched glTF source primitives incidentally exercised.

**Root cause:** a clippy version bump added the `manual_is_multiple_of` lint (stabilized `u32::is_multiple_of` in a recent Rust release); the 6 sites predate the lint and were never touched by any session since.

**Fix shape:** mechanical — replace `x % n != 0` with `!x.is_multiple_of(n)` (and `x % n == 0` with `x.is_multiple_of(n)`) at the 6 named sites. No behavior change. Whoever next touches `gltf_import.rs` or `render_scene.rs` for an unrelated reason should fold this in rather than opening a dedicated session for 6 one-line rewrites.

### BUG-195 (scene-setup-merge-no-stored-object-radius-for-scale-sanity) — SCENE_SETUP_PANEL_DESIGN.md D5's merge scale-sanity rule has no literal "scene reference radius" to read

**Status:** FIXED (bugfix-wave-2026-07-17 Lane 3) — BUG-194 shipped first, so the real fix named below (a stored per-object size signal) was available: `max_known_source_bbox_radius` reads the largest KNOWN `source_bbox_radius` (BUG-194's provenance param) recursively across the target def's existing mesh-source nodes and prefers it over the orbit-camera proxy, which remains only as the fallback for a def with no known-radius mesh-source node at all. Unit test `merge_scale_sanity_prefers_stored_radius_over_camera_proxy` (`gltf_import.rs`) proves the stored value — not the proxy — drives the decision when the two disagree.

**Symptom:** D5 commits to: "iff the incoming bbox radius differs from the scene's reference radius (the largest existing object's) by more than 10× in either direction, the seeded `scale` is `ref_radius / incoming_radius`". Reading `merge_import_into_graph`'s inputs (`&EffectGraphDef`, a freshly-parsed `GltfImportSummary`, the target `.glb` path), there is no per-object bbox/radius stored anywhere in the def to read "the largest existing object's" size from — the identical gap BUG-193/194 found for object counts and vertex counts, just for size this time.

**Root cause:** design-time assumption — D5 was written assuming a per-object size signal existed or could be trivially derived; on inspection, `manifold_core::effect_graph_def::EffectGraphNode`/`GroupDef` carry no geometry metadata at all (procedural mesh generators are Rust-side formulas, imported meshes' bbox is consumed once at import time to seed the recenter transform and then discarded).

**Fix shape (shipped as a defaulted proxy, not blocked on):** `merge_import_into_graph` derives a "scene reference radius" from the target's own top-level `node.orbit_camera`'s `distance` param, inverted through the EXACT formula `build_import_graph` used to seed it (`distance = 2.2 * radius`) — the camera's framing distance already encodes "how big is everything in this scene" as of the last import/creation that mattered. When no such camera exists at the top level, normalization is skipped entirely (native units) rather than guessed. Known blind spots, both honest and low-severity: (1) a user who hand-retunes Camera Distance on the card shifts this proxy without changing the actual scene scale; (2) a hand-built or non-importer-shaped scene (no top-level `node.orbit_camera`, or one with a hand-set `distance`) gets no normalization at all, ever. Real fix (generalizes BUG-193/194's option (a)): stash a real per-object size signal (e.g. bbox radius as a node param) at import/generation time and read THAT instead — Peter's call on scope, since it's bigger than a P4-only change.

### BUG-194 (scene-setup-vertex-count-not-computable-from-def) — Scene Setup panel's header "vertex count" (D4) has no honest source — mesh geometry isn't stored in the graph def

**Status:** FIXED (bugfix-wave-2026-07-17 Lane 3) — fix shape (a): `source_vertex_count` (Int, default -1 = unknown) and `source_bbox_radius` (Float, default -1.0 = unknown) declared as DECLARED node params on `node.gltf_mesh_source` and `node.gltf_skinned_mesh_source` (never read by `run()` — import-time provenance only), seeded by `gltf_import.rs`'s `build_object_group` (shared by both `build_import_graph` and `merge_import_into_graph`) from `GltfMaterialInfo::vertex_count` and the import's whole-scene bbox radius. `SceneVm::from_def` sums every resolved object's terminal mesh-source node's count (plus a closed-form `procedural_vertex_count` table covering `node.cube_mesh`/`node.grid_mesh`) into a new `SceneHeaderVm::vertex_count` / `vertex_count_exact` pair — an unresolved contribution (unmapped procedural generator, malformed def, unparseable modifier chain) sets `vertex_count_exact = false` so the panel can render "≥ N" rather than a fabricated exact number. Piping counts through `ContentState` (option (b)) was rejected per the design's D3 purity contract. UI wiring (`manifold-ui`/`state_sync.rs` consuming the new header fields) is a follow-up, not done this session — out of this lane's enumerated fix list.

**Symptom:** D4's header row commits to "object/light/vertex counts + shadow-caster count (static, from the Vm — the honest cheap cost line)". Object/light/shadow-caster counts are real (`SceneHeaderVm`, already shipped P1). A vertex count is not: mesh-source nodes are either procedural generators (`node.cube_mesh`, `node.generate_grid_mesh`, …, whose vertex counts are Rust-side constants/formulas, never stored as a graph-def param) or `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source` (which read a `.glb` from disk at RUN time — the importer's own `GltfImportSummary.vertex_count` is import-time metadata, never stashed back onto the mesh-source node's `params`). `SceneVm::from_def` is a pure function of `EffectGraphDef` alone (D3's own architectural constraint, enforced by §4's negative gate) with no GPU/mesh access — so no code path can produce a real vertex count without either violating that purity or loading assets.

**Root cause:** design assumed a "cheap proxy" count was available; on inspection, no per-node vertex metadata exists anywhere in the def, and fabricating one (e.g. counting resolved mesh-source nodes, which would just re-state `object_count`) would be a dishonestly-labeled number, not a proxy.

**Fix shape:** two real options, both bigger than a panel-only change: (a) stash `vertex_count` as a node param on mesh-source nodes at import/generation time (touches `gltf_import.rs` + every procedural mesh-generator primitive — a real, scoped feature); (b) compute it from loaded mesh data at content-thread render time and pipe it back as part of `ContentState` (crosses the UI/content boundary `SceneVm` was built to avoid). Until one ships, the header row omits "vertices" — P2 shipped objects/lights/shadow-casters only (already present from P1), which is what's genuinely computable today.

### BUG-236 (scene-setup-flows-assert-stale-outliner-text) — two flow scripts assert a literal "Outliner" text label that no longer exists anywhere in `scene_setup_panel.rs` — found 2026-07-17 during SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1b

**Status:** FIXED 2026-07-18 (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1d closing session). Both flows' step-2 selector changed from `Query(text="Outliner")` to `Query(text="Objects")` (the outliner's own top-level section header is `"Scene"`, per `build_outliner`'s first `add_label`; `"Objects"` is the nearer, unambiguous sub-section label the same section always renders, and the one every other converted family's flow already keys its own "outliner rendered" assertion off). `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-add-object.json` now runs green 8/8. `scene-setup-scrub-fine.json` also gets the text fix and clears that same step 2, but fails LATER (step 14) for an unrelated, newly-discovered reason — logged separately as BUG-240; that flow is not one of C-P1d's own required re-verify targets, so it's not driven to full green this session.

**Symptom:** `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-setup-add-object.json` and `scene-setup-scrub-fine.json` both fail at step 2, `Assert { selector: Query(text="Outliner"), check: Exists }` — "expected a match for query{text=\"Outliner\"}, found none". `rg '"Outliner"' crates/manifold-ui/src/panels/scene_setup_panel.rs` → 0 hits: no section label or heading in the current panel renders that literal text.

**Root cause:** the outliner section's header text was renamed to `"Scene"` (`build_outliner`'s own top label) at some point pre-dating C-P1a without sweeping these two flow scripts' selectors — never precisely dated; not investigated further, the fix (swap to a text that DOES exist) doesn't require knowing exactly which historical commit did the rename.

**Fix shape:** `rg '"Objects"' crates/manifold-ui/src/panels/scene_setup_panel.rs` confirms the section renders that literal text unconditionally whenever the outliner builds — swapped both flows' step-2 selector to it.

### BUG-193 (scene-setup-no-remove-object-command) — Scene Setup panel's Objects section has no "Remove" affordance — no composite command exists to dispatch

**Status:** FIXED 2026-07-17 (bugfix wave, Lane 2, `lane/bugfix-removal-commands`) against the CURRENT parallel-wire object model. **V2 P3 NOTE:** `docs/SCENE_OBJECT_AND_PANEL_V2_DESIGN.md`'s P3 phase was independently scoped the same day to build this same removal affordance against its new `node.scene_object`/`Object`-wire model — that design was written without visibility into this concurrent lane. Advised resolution (Fable, 2026-07-17, consulted at landing): land Lane 2's fix as-is; it's real, tested, and closes a gap Peter has hit directly. P3's deliverable becomes *porting* `RemoveSceneObjectCommand`/`RemoveSceneLightCommand` to the `scene_object`/Object-wire shape (reusing their inverse-pair execute+undo tests), not authoring removal from scratch. New `RemoveSceneObjectCommand`/`RemoveSceneLightCommand` (`crates/manifold-editing/src/commands/graph.rs`), shaped exactly as this entry's fix shape: whole-level snapshot/restore undo (mirrors `AddSceneObjectCommand`/`AddSceneLightCommand`), deletes the object's group node + its 3 root wires (or the light's bare node + its single wire), decrements `objects`/`lights`, and renumbers every wire whose object/light-port index exceeded the removed one down by one (`shift_indexed_ports_down` helper). Wired to a per-row "✕" in both the Objects and Lights sections of `scene_setup_panel.rs` (new `PanelAction::SceneSetupRemoveObject`/`SceneSetupRemoveLight`, dispatched through `ui_bridge::project::dispatch_project` exactly like the existing Add actions). Gates: `remove_scene_object_middle_deletes_group_and_renumbers_survivors` (proves the renumbering claim on a 3-object fixture, removing the middle one) + `remove_scene_light_only_light_removes_node_and_zeroes_count`, both execute+undo byte-equal; panel-level click tests proving each "✕" carries its row's own index; `dispatch_project`-level tests proving the panel action reaches the real command against a `SceneStarter`-based project. `cargo nextest run -p manifold-editing -p manifold-ui -p manifold-app` 1262/1262 passed; `cargo clippy -p manifold-editing -p manifold-ui -p manifold-app -- -D warnings` clean.

**Symptom:** SCENE_SETUP_PANEL_DESIGN.md's D4 table and P2 phase brief both call for a per-object "remove (delete-group + decrement composite, the existing path)" — the brief's own VERIFY marker (`rg -n "delete.*group|RemoveScene" crates/manifold-editing/src/commands/graph.rs`) turns up nothing: no composite command decrements `render_scene`'s `objects` count, renumbers the remaining `mesh_k`/`transform_k`/`material_k` wires above the removed index, and deletes the group subtree as one undo unit. `RemoveGraphNodeCommand` (generic node+wire removal) exists but does NOT touch the `objects` param or renumber subsequent object ports — using it alone would leave a gap (e.g. removing object 1 of 3 leaves `objects=3` with `mesh_1` unwired, showing as a phantom Custom row, while `mesh_2` stays wired at the wrong index). `RemoveSceneCommand` (`manifold-editing/src/commands/session_commands.rs`) is a different concept entirely (SESSION_MODE clip-launch scenes, not 3D scenes).

**Root cause:** design-time assumption gap — SCENE_SETUP_PANEL_DESIGN.md assumed a removal composite already shipped (mirroring `AddSceneObjectCommand`/`AddSceneLightCommand`), but no prior phase (SCENE_BUILD P5 built add-only) ever built the remove side.

**Fix shape:** a new `RemoveSceneObjectCommand` (and `RemoveSceneLightCommand`), each one undo unit, shaped like `AddSceneObjectCommand`: delete the object's group node + its 3 root-level wires (`mesh_k`/`transform_k`/`material_k` → `render_scene`), decrement `objects`, and renumber every wire whose port index was above the removed one (`mesh_{k+1}` → `mesh_k`, etc. — lights are simpler, no renumbering needed since `node.light` wires directly with no per-object port shift beyond the single `light_k` slot). This is a genuine new composite, not one of SCENE_SETUP_PANEL_DESIGN's five named additions (env/fog add, D5 import, D6 ×3) — Peter's call needed on whether it's in-scope for this design or its own small follow-up. P2 shipped without a Remove control rather than invent this unreviewed.

### BUG-184 (automation-clear-lane-not-wired-to-ui) — no UI affordance clears a lane's automation once it's set

**Status:** FIXED 2026-07-17 (bugfix wave, Lane 2, `lane/bugfix-removal-commands`) — right-click on an automation lane's strip/segment/dot now opens a two-item context menu ("Clear Automation" → `ClearLaneCommand`, "Remove Lane" → `RemoveLaneCommand`), using the same `DropdownItem`/`open_context` infrastructure `ClipRightClicked`/`TrackRightClicked` already use. New `TimelineEditingHost::on_automation_lane_right_click` method, called from `interaction_overlay.rs`'s `on_pointer_click` right-click branch (resolved BEFORE the left-click automation handler so a right-click never mutates a point); `AppEditingHost` pushes `PanelAction::AutomationLaneRightClicked`, `ui_root.rs` builds the two-item menu, and `ui_bridge::project::dispatch_project` executes the chosen command (removed-lane index resolved by `param_id` lookup against `automation_lanes`, same pattern `remove_automation_point` already uses). Gates: existing `ClearLaneCommand`/`RemoveLaneCommand` unit tests in `manifold-editing` (unchanged, already covered execute+undo); new `interaction_overlay` test `right_click_on_automation_dot_opens_lane_context_menu_not_point_logic` proves the right-click routing decision itself (a right-click on a lane dot calls `on_automation_lane_right_click` exactly once and never touches `ui_state.selected_automation_point`, i.e. never falls into the left-click point-select/delete path); `cargo nextest run -p manifold-editing -p manifold-ui -p manifold-app` 1263/1263 passed; `cargo clippy -p manifold-editing -p manifold-ui -p manifold-app -- -D warnings` clean.

**Symptom:** `ClearLaneCommand` and `RemoveLaneCommand` exist in `manifold-editing` (`crates/manifold-editing/src/commands/automation.rs:306`, `:197`) but neither is referenced anywhere in `manifold-ui` or `manifold-app` — confirmed via `rg -n "ClearLaneCommand|RemoveLaneCommand" crates/manifold-ui crates/manifold-app` returning zero hits. The only shipped point-level edits are the AUTOMATION_LANES_DESIGN.md §7 vocabulary: double-click a dot deletes it, marquee-select + Delete removes a selection. There's no one-click "clear this lane" or "remove this lane" button/menu item/keybinding.

**Fix shape:** wire `ClearLaneCommand` (clear all points, keep the lane) and `RemoveLaneCommand` (delete the lane entirely) to a UI trigger — most likely a right-click/context-menu item on the lane header, per the lane-header re-enable click precedent already in the design doc (§7 "State affordances"). Design doc `docs/AUTOMATION_LANES_DESIGN.md` is otherwise implemented per its status board entry; this is a gap in that landing, not a new design.

### BUG-199 (audio-and-scene-setup-docks-have-no-working-scroll-input) — neither utility dock's `ScrollContainer` ever receives a real scroll gesture; content past the window height is unreachable — found 2026-07-17 landing SCENE_SETUP_PANEL_DESIGN.md P5 (wave close), confirmed by direct testing

**Status:** FIXED 2026-07-17, BUGFIX_WAVE_2026_07_17_DESIGN.md Lane 1 (`lane/bugfix-dock-scroll`). `primary_mouse_wheel` (`window_input.rs`) now routes wheel events over either dock — gated on `layout.scene_setup().contains(pos)` / `layout.audio_setup().contains(pos)`, which is already the open-check since a closed dock's rect is `Rect::ZERO` — through the SAME generic `UIEvent::Scroll` → `pending_events` pipeline the open-dropdown branch already used (no per-panel in-place-offset special case). Both `ScenePanel::handle_event` and `AudioSetupPanel::handle_event` gained a `UIEvent::Scroll` match arm calling their existing `handle_scroll`, mirroring `dropdown.rs`'s "always consume while open" pattern; no position re-check needed inside the panel since `window_input` already gated on the dock rect. The real app and the headless `Gesture::Scroll` harness now share one path. Verified: `scripts/ui-flows/scene-setup-add-fog-drag.json` green again (15/15, one `Scroll` step added before the "+ Add Fog" click); new `scripts/ui-flows/audio-dock-scroll.json` (5/5) proves the audio dock scrolls against a purpose-built overflow fixture (`audio_sends_scene()` gained 24 extra Send-A consumer rows, `fixtures.rs`); `audio-setup-hygiene.json` re-run green (11/11, unaffected). Clears VD-029.

**Symptom:** `scripts/ui-flows/scene-setup-add-fog-drag.json` (P1's own L3 flow, previously green at P1's landing) now fails: clicking "+ Add Fog" by name resolves to `acted at (736.0, 1244.0)` but no `SceneSetupAddFog` action is dispatched, and the resulting "Density" row never appears. The window's rendered surface is only 1216px tall; by P5's landing the azalea fixture's Objects section (2 objects × transform/material/modifier rows, each object now carrying its own "Modifiers"/"Add modifier:" section per P5) pushes Lights/Environment/Fog/Camera to y≈960–1250+, i.e. genuinely past the physical window height, not just past a notional sub-viewport. The `ScrollContainer` exists (`scroll_container.rs`, wired into `scene_setup_panel.rs` at build time) but nothing ever drove it in the real input path.

**Root cause:** `crates/manifold-app/src/window_input.rs`'s `primary_mouse_wheel` explicitly branched on `inspector_rect.contains(pos)` and `tracks_rect.contains(pos)` (plus the dropdown-open case) — there was no branch for the scene-setup or audio-setup dock rects at all, so a real mouse wheel over either dock did nothing. Confirmed empirically, not just by reading the dispatch table: a `Gesture::Scroll` at the dock's body point, at deltas from -400 to -5000, left the "+ Add Fog" button's resolved screen position completely unchanged (1244 before and after); a `Drag` gesture on the dock's own scrollbar-thumb widget (`scroll_container.rs`'s track/thumb pair, identified via a forced-failure `--dump`) had the same null effect. The generic `UIEvent::Scroll` consumer list (`rg -n "UIEvent::Scroll" crates/manifold-ui/src/panels/`) covered only `dropdown.rs` and `browser_popup.rs` — neither utility dock was in it.

**Fix shape:** see Status above.

### BUG-166 (gltf-crate-vetoes-extensionsrequired-we-already-support) — `gltf::import` hard-fails any asset that lists a required extension the crate's own validator doesn't recognize, even when MANIFOLD's importer downstream already handles that extension — blocks otherwise-supported assets before our code ever runs
**Status:** FIXED 2026-07-16 (parse layer, GLB_XFAIL_BURNDOWN_DESIGN P2; the residual unlit-material-fidelity gap is BUG-174's, not this bug's) — `import_glb()` (`gltf_load.rs`) replaces `gltf::import()` at all 3 production call sites + the azalea test harness; parses via `Gltf::from_slice_without_validation` + re-runs the crate's own structural validation directly (`json::Root: Validate`) with only the `extensionsRequired`-`Unsupported` errors filtered out, then checks `extensionsRequired` against MANIFOLD's own `MANIFOLD_SUPPORTED_EXTENSIONS` list. `UnlitTest.glb` now imports and renders (converges non-black, frame 4, fraction 0.1788); `ClearCoatCarPaint.glb` now imports (parse-layer only — its render correctness was already shipped by GLB_CONFORMANCE_DESIGN, unaffected). Caveat found during this fix, logged separately as BUG-174: `UnlitTest.glb`'s render is geometrically correct but NOT shaded unlit — `gltf_import.rs` never reads `KHR_materials_unlit` and always builds a lit (Phong-ish) material; the crate-level import veto (BUG-166's actual defect) is fixed, but full unlit material fidelity was never in this doc's D1 scope and remains open.

**Symptom:** Khronos `ClearCoatCarPaint.glb` (`extensionsRequired: ["KHR_texture_transform", "KHR_materials_clearcoat"]`) and `UnlitTest.glb` (`extensionsRequired: ["KHR_materials_unlit"]`) both fail at `gltf::import()` itself with `invalid glTF: extensionsRequired[N] = "...": Unsupported extension` — never reaching `assemble_import_graph`'s own logic. Both extensions are ones MANIFOLD already has real support for elsewhere: clearcoat factors render correctly on `ClearCoatTest.glb` (G-P5, `extensionsUsed` not `extensionsRequired` there — the crate only vetoes *required* extensions it doesn't know), and `MATERIAL_SYSTEM_DESIGN.md` names `unlit` as a supported Material shading mode. The gate is upstream of MANIFOLD's code — the `gltf` crate (v1.4.1, pinned) validates `extensionsRequired` against its own internal known-extension list at `gltf::import()` time and refuses to proceed if an entry isn't on it, independent of what MANIFOLD does with the parsed data afterward.

**Root cause:** not investigated past confirming the crate-level veto (empirically: identical extension listed under `extensionsUsed` imports fine; the same extension listed under `extensionsRequired` hard-fails). Whether the pinned `gltf` crate version exposes a validation-strategy knob (e.g. a lower-level `Import`/`Root::from_slice` entry point that skips extension-requirement validation) is unverified.

**Fix shape:** likely swap `gltf::import(path)`'s convenience call for the crate's lower-level slice-based import (parse JSON + buffers/images ourselves, as `gltf_load.rs` already partially does) with extension validation disabled or pre-filtered — strip the specific extensions MANIFOLD supports out of `extensionsRequired` before validation, or vendor a permissive validator. Affects any spec-legal glb that correctly marks a load-bearing extension `extensionsRequired` (the compliant authoring choice) rather than `extensionsUsed`.

### BUG-200 (khr-animation-pointer-channels-fail-to-deserialize) — duplicate of BUG-170, id burned
**Status:** SUPERSEDED — same crate-level gap as BUG-170 (`gltf-json` 1.4.1's `Target::node` has no `#[serde(default)]`; `KHR_animation_pointer` channels legally omit it). Filed independently as BUG-187 during GLTF_ANIMATION A1, renumbered to 200 at the 2026-07-17 dedup, then recognized as BUG-170's five-asset class. This entry's root cause and fix options were folded into BUG-170, which is canonical (the conformance manifest's five `xfail:BUG-170` assets point there). Id stays burned — do not reuse 200.

### BUG-189 (import-graph-10ms-resolution-independent-gpu-floor) — glb import graph burns ~10 ms of GPU time per frame independent of resolution; 4K lands at 13.5 ms median / 22.7 ms p95, over the 60 fps budget at p95 — found 2026-07-16 measuring 4K60 feasibility for the AMG GT3 on M4 Max 36GB
**Status:** FIXED (residual documented) 2026-07-17 — RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0–P5 SHIPPED. The shadow+IBL re-render waste this bug named is closed (P2 shadow caching + P3/P3b IBL gating); the resolution-independent "floor" is gone as a dirty-scene phenomenon — what remains is `render_scene`'s main pass (draw calls + shading), which is real per-frame work, not waste, and cannot be dirty-gated on stage (the camera moves every frame). See the P5 final re-measure below for the closing numbers.

**Symptom:** `render-import` on `mercedes-amg_gt3__www.vecarz.com.glb` (302k tris, 166 primitives, 78 materials), true GPU execution time per frame via `commit_and_wait_completed_timed` (`GPUEndTime − GPUStartTime`), steady-state medians after decode convergence, back-to-back frames (no inter-frame sleep): 9.8 ms @1920×1080, ~9.8 ms @2560×1440, 13.5 ms @3840×2160 (p95 22.7 ms). Only ~3.7 ms scales with pixels across a 4× pixel-count jump; ~10 ms is a fixed per-frame floor. CPU encode is ~4.5 ms/frame on top (overlappable in the pipelined engine, serial in the harness).

**Root cause:** unknown. Suspects, in order: (a) shadow-map pass re-rendering the full 166-draw-call scene every frame at fixed shadow resolution; (b) per-pass/encoder overhead across the import graph's many sequential passes (dome fill, IBL, mips, tonemap) with GPU dead time between them; (c) something regenerating per-frame that should be cached (mips, environment). The 4K p95 spikes (22–37 ms) are a second unknown riding on top.

**Attribution (PERF_BUDGET_GATE_DESIGN.md P2b `--profile`, `cargo xtask perf-soak tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb --size 3840x2160 --profile`, 2026-07-16):** unprofiled confirms the floor — GPU min=13.293 p50=13.541 p95=13.815 max=14.220 ms over 300 frames (matches the earlier measurement). Profiled mode (forced-serial, per D6's honesty contract — its totals run higher than the gate numbers above and are not comparable to them) surfaced the worst frame's node breakdown:

| Node (tag) | type_id | GPU ms | Share of frame |
|---|---|---|---|
| `import:s294` | `node.render_scene` | 11.225 | 48.8% |
| `import:s295` | `node.ssao_gtao` | 1.927 | 8.4% |
| `import:s296` | `node.bilateral_blur` | 0.631 | 2.7% |
| `import:s297` | `node.bilateral_blur` | 0.568 | 2.5% |
| `import:s113` | `node.gltf_texture_source` | 0.522 | 2.3% |

`render_scene` alone is ~half the frame's GPU time and the only candidate anywhere near the ~10ms floor's magnitude — this points at suspect (a) (the 166-draw-call scene, most plausibly its shadow-map re-render) over (b)/(c), which would show up as many small passes each taking a slice, not one dominant node. `ssao_gtao` (8.4%) and the two `bilateral_blur` passes (2.7%/2.5%, its blur pair) are the next tier — real but an order of magnitude smaller. No untagged/uninstrumented GPU time in this frame (0.000ms). Capacity check: 198/2048 sampler spans used on the busiest frame, no overflow. Separately, `--profile`'s worst-frame selection surfaced an anomalous early frame (frame 6, just past the 6-frame warmup convergence point) whose GPU total and per-node shares don't reconcile cleanly (per-node shares summing past 100% of a reported frame time lower than their sum) — looks like a first-use pipeline-compile artifact near the warmup boundary, not a steady-state signal; noted here rather than folded into this bug's numbers, worth a look if it recurs on other assets.

**Fix shape (superseded below):** the attribution above narrowed this to `render_scene`'s shadow pass (suspect (a)) — that guess is now OVERTURNED, see the P0 refinement immediately below.

**Attribution refinement (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P0/D4b, 2026-07-17):** the `--profile` tool previously merged `render_scene`'s internal shadow/IBL/main-pass dispatches into one row (`node.render_scene`'s tag matched a live executor step, so distinctly-*labeled* sub-passes silently summed together — a latent join bug, not the design's originally-suspected `None`/unmatched-arm collapse). P0 fixed this: each node row now carries a nested `passes` array (`{label, gpu_ms, share_of_frame}` per distinct GPU pass label under that node's tag), verified against real profiled runs on this fixture (`crates/manifold-app/src/perf_soak_import.rs`). Unprofiled anchors (300-frame steady-state, back-to-back, no readback/sleep): @3840×2160 GPU p50=13.554ms p95=13.869ms; @1920×1080 GPU p50=9.830ms p95=11.768ms — both consistent with the original floor. Corrected per-pass composition of `render_scene`'s own GPU time (its own total as denominator — frame-level shares are inflated past 100% under D6's stage-boundary profiling overhead and are not the right denominator for internal composition), two consecutive profiled runs per resolution, rank order stable both times:

| Pass label | @3840×2160 (run1 / run2) | @1920×1080 (run1 / run2) |
|---|---|---|
| main pass (`node.render_scene`) | 12.184ms (54.6%) / 12.150ms (54.7%) | 9.589ms (51.7%) / 9.679ms (53.0%) |
| `… ibl prefilter mip` + `… ibl irradiance` | 8.892ms (40.5%) / 9.272ms (41.7%) | 8.214ms (44.3%) / 7.771ms (42.6%) |
| `… shadow` | 0.905ms (4.1%) / 0.805ms (3.6%) | 0.756ms (4.1%) / 0.799ms (4.4%) |

**This overturns the earlier attribution's conclusion.** Shadow is a small share (~4%) at both resolutions, not the dominant cost as previously guessed from the collapsed single-row number — **IBL convolution (prefilter + irradiance) is the dominant internal cost, ~40–44% of `render_scene`'s time, roughly 10× shadow's share.** Main pass (draw calls + shading) is the largest single component (~52–57%). RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md's own P2 (shadow caching)/P3 (IBL gating) phase order should be read against this: P3's IBL gating has the larger ceiling of the two dirty-signal fixes, not P2's shadow caching, contrary to this doc's D1 framing ("shadow... is the headline win") which was written before this corrected breakdown existed.

**Fix shape:** unchanged in mechanism (dirty-signal caching, per RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P2/P3) but re-prioritize: IBL gating (P3) closes more of the floor than shadow caching (P2) alone. Measurement harness: `cargo xtask perf-soak <glb> --size WxH [--profile]` (PERF_BUDGET_GATE_DESIGN.md P1+P2b, landed 2026-07-16, `7afcb059`; D8 amendment for the per-label `passes` breakdown, 2026-07-17) — no longer a throwaway patch, it's the standing tool.

**P3 landed 2026-07-17 but did not close this bug — see BUG-197.** RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3 implemented exactly its brief (bake_equirect_envmap + hdri_source producer gating, render_scene consumption gating on the envmap slot's write generation) and every one of its own correctness gates (I2 animated-envmap parity, I4 static bit-identity, per-producer parity tests) passes on real GPU hardware. But the AMG's actual import graph wires `node.bake_environment → node.switch_texture (env_mode select) → node.render_scene`, and `switch_texture` copies its selected branch into its own output every frame without ever declaring `mark_outputs_unchanged` (BUG-197) — so `render_scene`'s envmap generation never stabilizes on a real import and the gate never hits. Re-measured after P3: AMG @3840×2160 unprofiled p50 13.554ms → 13.333ms, a ~0.22ms/1.6% drop — not the multi-ms/~41% drop this bug's floor implies is available. The floor is still ~13.3ms; only shadow's ~4% (P2) plus a fraction of a percent (P3, real-import path) has actually closed. Unblocked once BUG-197 lands.

**P3b landed 2026-07-17 — this bug's headline floor is now closed for the AMG fixture (Softbox AND HDRI env modes).** BUG-197's fix (`mux_texture.rs` evaluate-path gate + the `execution.rs` alias-path generation propagation) let `render_scene`'s IBL cache key actually stabilize on a real glTF import. Re-measured on the AMG @3840×2160, two consecutive unprofiled runs: p50 9.403ms / 9.456ms (down from the pre-P3b 13.554ms floor — a ~4.1ms/~30% drop, within tolerance of P0's ~41%-IBL-share prediction). HDRI mode measures the same (p50 9.400ms) — the `node.exposure` hop some feared would keep a residual floor does not. Residual floor (~9.4ms) is now dominated by `render_scene`'s main pass (~54% of ITS OWN GPU time per P0's breakdown), which cannot be dirty-gated on stage (the camera animates every frame) — this is R4's (indexed-mesh-rendering) target, per this doc's Deferred section; P5 will re-measure and record the exact residual main-pass share R4's revival trigger needs. `mercedes-amg_gt3` @1080p not re-measured this session (P5's job); the @4K number above is the one this bug was filed against.

**P5 final re-measure 2026-07-17 (full landed tree: P0–P4 + P3b, all phases in), full before/after for the whole workstream, AMG GT3:**

| Stage | @3840×2160 GPU p50 | @1920×1080 GPU p50 |
|---|---|---|
| P0 baseline (pre-fix) | 13.554ms (p95 13.869ms) | 9.830ms (p95 11.768ms) |
| Post P2 (shadow caching) | ~13.3–13.5ms (shadow ~4% share, inside run-to-run noise per D1b — not separately re-measured, P2's own gate note) | — |
| Post P3 (IBL gate, undischarged — BUG-197) | 13.333ms (~1.6% drop only — mux passthrough broke the cache key) | not measured |
| Post P3b (BUG-197 fix, IBL gate discharged) | 9.403ms / 9.456ms (two runs) | not measured |
| **Post P4 / P5 final (this re-measure, fresh two-run pairs)** | **9.454ms / 9.449ms** | **5.744ms / 5.716ms** |

Total drop from the P0 baseline: @4K 13.554ms → ~9.45ms (**~4.1ms / ~30%**); @1080p 9.830ms → ~5.73ms (**~4.1ms / ~42%** — a bigger proportional win at 1080p than 4K, since the fixed IBL convolution cost the fixes removed is resolution-independent while the residual main pass does scale with pixel count somewhat). A profiled sanity run at both resolutions shows the `node.render_scene` tag now carrying a **single pass row** in steady state (no separately-labeled `shadow`/`ibl prefilter`/`ibl irradiance` rows at all — both fully gated away): 9.10ms @1080p / 13.6ms-worst-frame @4K, i.e. the entire remaining cost is main pass, confirming D1b's ~54%-of-render_scene forecast in the strongest possible way — it's now ~100% of render_scene's GPU time, because everything else is gated to zero on a static scene. This residual is real per-frame work (draw calls + shading for 302k tris / 78 materials), not staleness — it is R4's (indexed-mesh-rendering, deferred) target; see the Deferred-section note below for the exact trigger number.

BrainStem (`khronos/BrainStem.glb`, 1920×1080, `--warmup-frames 30` per P4's tool addition, since this fixture never converges): GPU p50=4.003ms/p95=8.174ms (healthy, unrelated to this bug — BUG-190's territory). Not part of BUG-189's own claim; recorded here because it ran on the same fully-landed tree as this bug's closing measurement.

### BUG-197 (switch-texture-blocks-ibl-generation-gate) — `node.switch_texture` breaks the envmap write-generation chain between the bake/hdri producers and `node.render_scene`, so P3's IBL re-convolution gate never hits on a real glTF import — found 2026-07-17 measuring RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3's before/after on the AMG fixture
**Status:** FIXED — RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b, 2026-07-17. `mux_texture.rs`'s `evaluate()` now gates its copy dispatch (and the clear-fallback) on a hash of (effective selector index, selected source slot's write generation, selected source texture identity, output texture identity, executor rebuild epoch) — full match declares `mark_outputs_unchanged()`; the `execution.rs` alias-skip path additionally propagates generation-unchanged through the param-driven (`skip_passthrough`) alias fast path (fenced to `!data_skip`), which is the path the AMG's inline-selector `env_select` actually takes. Re-measured on the AMG @3840×2160: unprofiled p50 13.554ms (pre-P3b) → 9.403–9.456ms (post-P3b, two consecutive runs) — a ~4.1ms/~30% drop, within the ±30% tolerance band around P0's measured ~41% IBL share prediction (5.55ms theoretical). Profiled sanity: neither an `ibl prefilter`/`ibl irradiance` labeled row nor a `node.switch_texture` row appears anywhere in the captured frames (both fully gated/aliased away). HDRI-mode observation (temporary local probe, reverted before commit — not a shipped ablation flag): AMG in HDRI mode measures p50 9.400ms, matching Softbox — the `node.exposure` (hdri_gain) hop does NOT reintroduce the floor, so no residual note is needed on BUG-189 for that path.

**Symptom:** After RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3 landed (bake_equirect_envmap/hdri_source producer gating + render_scene consumption gating, all correct and gate-passing in isolation — see BUG-189's note above), the AMG GT3 import's measured unprofiled perf-soak delta is ~0.22ms/1.6% (@3840×2160, p50 13.554ms → 13.333ms), not the multi-ms/~41%-of-render_scene-GPU-time drop P0's profiled bisection measured as available and P3's own phase gate requires (±30% tolerance on a multi-ms delta).

**Root cause (confirmed by reading `mux_texture.rs`, the file `node.switch_texture` aliases to):** every glTF import's D6 env-mode wiring (`gltf_import.rs` ~671–715, ~2029–2032) is `node.bake_environment → node.switch_texture (env_select) → node.render_scene`'s `envmap` port — never a direct wire. `MuxTexture::run()` (`mux_texture.rs` ~355–370) unconditionally dispatches a copy-compute pass every frame it runs (selecting/copying whichever branch the selector picks into its own output texture) and never calls `ctx.mark_outputs_unchanged()` anywhere in the file. Per D5's conservative-default rule, a node that never declares unchanged always bumps its output slot's write generation — so `render_scene`'s `ctx.inputs.slot_generation("envmap")` (which resolves to `switch_texture`'s output slot, not `bake_equirect_envmap`'s) changes every single frame regardless of whether the upstream bake actually re-ran. P3's `ibl_cache_key` therefore misses every frame on any glTF import, even though `bake_equirect_envmap` itself is correctly skipping its own dispatch (confirmed: its gpu-proofs parity tests pass, and the real per-node saving is real — just tiny, since baking one 512×256 texture is cheap compared to convolving it).

**Fix shape:** give `node.switch_texture` the same `last_key`-style gate P1/P3 gave the other sources: skip the copy dispatch and call `mark_outputs_unchanged()` when (selected branch's slot generation, selector value, output texture identity) are all unchanged since the last frame it actually copied. This is the same pattern, a different file — no new mechanism, just the 4th producer in the generation-signal chain. Scope note: `switch_texture`/`mux_texture` is a shared primitive (also used by Plasma's 8-pattern cycling, ConcentricTunnel's 6-shape select, Infrared's 10-ramp palette per its own module doc) — any gate must preserve its existing "prune unselected branches" cost-model doc comment and pass its own existing tests (`selected_input_branch` unit tests) unchanged; this needs its own §2.5-style audit before touching it (CLAUDE.md decomposition-audit rule), not a copy-paste of P1's pattern without reading the file end to end first.

### BUG-158 (mapped-param-edits-snap-back-no-two-way-binding) — a param mapped to a card slider or driven by another port can't be adjusted in the graph editor; the edit snaps back — MED-HIGH (authoring surface), reported by Peter 2026-07-14
**Status:** FIXED — P2 SHIPPED 2026-07-15 (Fable orchestrating two Sonnet agents, `bug/158-two-way-p2`): the live-value tap (`live_node_params`) now reports wire-resolved values via a new executor `live_scalar_inputs` capture instead of the stale param map, and wire-driven rows render as honest non-interactive readouts (whole-slider dim, tinted jack halo, hover names the driving source, click highlights the feeding wire, wired+bound rows keep both attributions per D6). Scrub prevention itself predated P2 (already in-tree at `interaction.rs:873` with its unit test). L2 PNG gate passed (graph scene, Tesseract — wired row visibly dead, bound row visibly live on the same node). One unverified edge, logged in the design doc: per-node live values inside a FUSED region rest on FREEZE_COMPILER_MAP cut rule 10 (control producers survive fusion) by doc-level reasoning, not an empirical fused-graph run; soft-fails to the param map if wrong. P1 (inverse machinery + dispatch-layer reroute) SHIPPED 2026-07-14 (Sonnet, `bc2f2c0b`). `docs/PARAM_TWO_WAY_BINDING_DESIGN.md` is the authority for what's built vs. remaining. Deviation logged there and in the P1 commit: D10 said P2 must not trail a shipped P1 across a session boundary, but only one phase fit this session's budget and P1's implementation does not touch wired-param behavior at all (a wired param still runs the pre-existing, unchanged `SetGraphNodeParamCommand` path), so the harm D10 guards against — wire-driven snap-back reading as newly/more broken — does not occur. P2 still owed for the full fix (driven readout, scrub prevention, fan-out badge tooltip). Prior investigation record, kept for context: root cause REFINED (both paths located) by bug-wave lane A, escalated for design per the lane's contract. `SetGraphNodeParamCommand::execute` (`crates/manifold-editing/src/commands/graph.rs:797`) does successfully write the direct node-face edit into `def.nodes[node_id].params` — confirmed by reading it, no gating for card-bound params. The stomp happens one layer downstream: `apply_bindings` (`crates/manifold-renderer/src/node_graph/param_binding.rs:566`) runs on every chain rebuild and unconditionally re-writes `binding.apply(graph, handle, <current outer/card value>)` into the freshly-built `Graph`'s param slot — the card binding is the sole authority the render ever sees, independent of `scalar_or_param`. So the original mechanism note (`scalar_or_param`, `effect_node.rs:358`) correctly explains the WIRE-DRIVEN half of the symptom but not the CARD-SLIDER-MAPPED half — those are two distinct code paths that produce the same visible snap-back. Both point the same direction: the fix has to live at the binding/mapping layer (write-back through the inverse for card-bound params, a legible "driven" visual for wire-driven ones with no inverse) exactly as the backlog's own fix-shape already said — that's real product-UX design (how "driven" reads on the node face), not a patchable bug, so it's parked for a design pass rather than improvised here.

**Fable design consult (2026-07-14, lane's one consult, spent here)** — verified the code citations above independently, then proposed a concrete direction. **Card-slider case**: reroute, don't dual-write — a node-face drag on a bound param should NOT issue `SetGraphNodeParamCommand` at all; intercept at the editor input layer, invert through the reshape (new `invert_card_reshape` beside `apply_card_reshape` so preview/forward/inverse can't drift), and issue the existing "set outer card param" command instead. The existing forward path (`apply_bindings`) then propagates it back into the `Graph` for free — and the `LastAppliedCache` question this session flagged dissolves entirely, since nothing writes the `Graph` param behind the cache's back anymore. Node face must display the *effective* value (manifest value pushed through the forward reshape), not the (currently shadowed, possibly stale) `def.nodes[...].params`. **Wire-driven case**: prevent at the input layer, not allow-then-revert — replace the draggable slider with a live-value readout (dimmed track, fill animated by the actual driven value each frame) plus a tinted input-jack glyph; non-interactive for drag, hover shows the source, click highlights the wire. **Sequencing**: land the wire-driven "driven" treatment first or simultaneously — shipping the card-slider fix alone would make wire-driven params' remaining snap-back read as MORE broken (Peter's mental model becomes "two-way works" right up until it silently doesn't). **Risks flagged that this session's investigation missed**: a param can be BOTH wire-connected and card-bound at once — `scalar_or_param`'s wire-shadows-everything means the driven treatment must win regardless of binding, and a reverse-write there would move the card slider with zero visible render effect; fan-out bindings (one `source_id`, multiple targets, already handled elsewhere per `param_binding.rs`'s own doc) mean a reverse-write from one target moves every sibling target too — expected but should be legible in the UI (name the card param in the badge tooltip); `wraps_angle` params aren't invertible across periods (take the principal value, don't try to round-trip exactly); non-monotonic `MacroCurve` variants must route to a read-only fallback rather than a wrong inverse; existing projects carry stale shadowed `def` param edits from before this fix that should be cleared/normalized if a binding is ever removed, or they'll surface as years-old surprise values. Full consult transcript not preserved verbatim — this is the distilled direction for whoever picks this up next; re-run the consult or re-derive from the code citations above if finer implementation detail is needed.

**Symptom** — in the graph editor, dragging a param that is already mapped to an effect-card
slider (or wired from another port) appears dead: the control snaps back to the mapped/driven
value, so the only editable end of a mapping is the source. Peter's expectation: two-way
behaviour between the node param, the card slider, and other ports — turning either end moves
both, like a DAW control surface.

**Mechanism (located)** — the port-shadows-param convention:
`EffectNodeContext::scalar_or_param` (`crates/manifold-renderer/src/node_graph/effect_node.rs:358`)
resolves a wired scalar input unconditionally before falling back to the param, so while a
wire/mapping is connected the param write lands in the model but never reaches the render, and
the UI re-reads the driven value — visual snap-back. This resolution order is the deliberate
control-wire design (`control-wires-port-shadows-param` memory), so the fix is a binding-layer
behaviour change, not a bug in `scalar_or_param`.

**Fix shape** — two-way binding where an inverse exists: editing the node param on a
card-slider mapping writes back through the inverse mapping and moves the slider, keeping both
ends consistent. For signal-driven ports (LFO, audio, envelope, another node's live output)
there is no inverse — those should show a legible "driven" state on the param control instead
of silently snapping back. Implement once at the binding/mapping layer, not per widget. The
driven-state presentation (how a driven param looks on the node face) is worth one screenshot
to Peter before landing.

### BUG-202 (freeze-codegen-region-fusion-gpu-tests-fail-with-badinput-standalone) — six `freeze::codegen` GPU tests fail deterministically, even isolated — not a parallel-contention flake
**Status:** FIXED 2026-07-14 (Fable root-cause session, branch `bug/163-freeze-extkind-probe`; renumbered from BUG-163 2026-07-17 — id collision with the AMG-livery bug, which keeps 163 since every external reference means that one) — one-line fix in `generate_fused`: the D3/P4a (BUG-114, `ae9ab74c`, landed 2026-07-14 12:10 — the same day these failures were first seen) ExtKind-resolution loop classified any member input at `idx >= tex_count` as an array port, with `tex_count` counted from `node.node_inputs`; the six hand-built test regions use `node_inputs: &[]`, so every `InputSource::External` texture input resolved as an array port with no spec → `BadInput` (`codegen.rs:2326`). The loop now keys on the explicit `InputAccess::BufferIndex` tag — the same discriminator the body-rewrite loop directly below already used, and exactly what `build_region` produces (it only packs BufferIndex-tagged array entries and pushes the tag itself, `region.rs:1615-1635`), so production regions are unaffected by construction. Verified: the failing test reproduced on the unmodified tip, then all 161 `node_graph::freeze` tests green under `--features gpu-proofs` with the fix (was 6 red); `clippy -p manifold-renderer` clean; 1232 default lib tests green. The six existing tests are the regression coverage.

**Symptom** — `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::freeze::codegen::gpu_tests -- --test-threads=1` (module-scoped, serial — rules out device contention) fails 6 of 38 tests, all with the same error shape, a `.fuses(...)` call returning `BadInput`:
- `cross_resolution_external_sampled_at_uv` — `codegen.rs:3591`, `"cross-res region fuses: BadInput"`
- `fused_colorgrade_generated_matches_hand_kernel` — `codegen.rs:4229`, `"fuse ColorGrade region: BadInput"`
- `fused_fanout_emits_two_dst_bindings` — `codegen.rs:4023`, `"fan-out region fuses: BadInput"`
- `fused_gather_binds_sampler_and_passes_texture` — `codegen.rs:3944`, `"gather region fuses: BadInput"`
- `fused_prelude_carries_and_dedups_top_level_consts` — `codegen.rs:3524`, `"a region whose body declares a const fuses: BadInput"`
- `fused_texture_region_carries_and_dedups_wgsl_includes` — `codegen.rs:3672`, `"coc_from_depth + Gain region fuses: BadInput"`

**Confirmed pre-existing** — `git stash`ed this session's BUG-120 diff (only touches `triangulate_grid.wgsl`/`triangulate_grid_body.wgsl`/`triangulate_grid.rs`, nowhere near `freeze::codegen`) and re-ran `cross_resolution_external_sampled_at_uv` isolated against the unmodified tree: identical failure, same line, same message. The other 5 weren't individually re-verified against the stashed tree, but share the exact same error class (`.fuses(...)` → `BadInput`) from the same run — very likely the same root cause, not independently confirmed.

**Not BUG-144's class** — BUG-144's two prewarm-cache tests are a genuine parallel-only race (pass isolated). Re-checked this session: `render_scene::gpu_tests::prewarm_pipelines_populates_the_shared_render_cache` failed in the full unfiltered parallel run but passed clean when isolated — consistent with BUG-144, not a new finding. These 6 `freeze::codegen` failures are different: deterministic, serial, module-scoped, and don't touch a shared cache at all.

**Root cause** — not investigated. `BadInput` from a `.fuses(...)` call at 6 different call sites, all in `codegen.rs`, suggests something upstream of region-fusion is now producing a shape the fuser rejects across the board (a shared helper's output changed, or a validation the fuser performs got stricter) rather than 6 independent bugs — but that's a hypothesis, not a finding.

**Fix shape** — needs its own investigation session: start at `codegen.rs:3591` (`cross_resolution_external_sampled_at_uv`, the smallest/most specific-sounding test) and work out what input shape `.fuses()` is rejecting; check whether the same root cause explains all 6 before assuming it does. Given every hit is `--features gpu-proofs`-gated and none of the 6 tests were in this session's narrow `-p manifold-renderer node_graph::primitives::triangulate_grid` gate, this is invisible to the routine focused-test workflow CLAUDE.md recommends — worth a full `--features gpu-proofs --lib` sweep as part of any FUSION_SOTA / freeze-compiler landing to catch it early next time.

### BUG-154 (removing-group-with-slider-bound-nodes-leaves-stale-effect-card) — deleting a node group that has nodes assigned to card sliders doesn't update the effect card: no warning shown, and the stale slider isn't removed — MED, reported by Peter 2026-07-14
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Root cause confirmed and closed at the class level, per the lane's contract (BUG-154 named explicitly as "stale bindings on ANY node-removal path, not just group delete"): a group deletion IS a single-node removal (the group container, via `RemoveGraphNodeCommand`) — but `remove_exposures_for_node` only ever matched the ONE node id it was called with, and it was called with the group container's own id. A card slider bound to a node NESTED inside the removed group's subgraph (the common case — a group wraps existing nodes, one of which is exposed) was never matched, so its binding/param-spec/value-slot/automation survived the deletion as dangling state. Fixed by walking the removed subtree: new `subtree_node_ids()` helper (`graph.rs`, recurses into nested `GroupDef.nodes`, handling groups-of-groups) collects every node id in the removed node's tree (itself, for a plain node removal — the existing single-node-delete behavior is now just the one-element case of the same call), and `RemoveGraphNodeCommand::execute` prunes exposures for each. `undo` needed no change — it already restores however many `RemovedExposure`s were captured. Regression test `remove_group_node_prunes_card_slider_bound_to_a_nested_node` builds a group wrapping a slider-bound node and asserts the binding/param/value are pruned on delete and restored on undo; verified to fail without the fix (reverted the loop back to the single-id call, confirmed red, restored). Gated: `cargo clippy -p manifold-editing -- -D warnings` clean; `cargo nextest run -p manifold-editing` 217/217 passed.

**Symptom** — when a group is deleted from a node graph and that group contained a node that had been assigned to an effect card slider, the effect card doesn't react correctly: it should either warn that the binding is now dangling or remove the slider, but currently does neither — the stale slider is left on the card with no indication its underlying node is gone.

**Root cause** — unknown; not investigated. Likely the group-deletion path doesn't walk card/slider bindings the way node-level deletion does — suggests group deletion isn't routed through the same binding-cleanup logic as deleting the same nodes individually (compare against however single-node deletion updates card sliders on removal).

**Fix shape** — trace group deletion (`EditingService`/command path for group removal) and compare it against the single-node deletion path's card-binding cleanup; route group deletion through the same cleanup so it either surfaces the warning or removes the slider, matching single-node-delete behavior.

### BUG-120 (grid-terrain-winding-disagrees-with-vertex-normals) — terrain triangle winding contradicts vertex normals — LOW, consumer-side fixed
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`) — confirmed at the emitter and fixed at the class level, per the fix shape's first option. `node.triangulate_grid` (both `triangulate_grid.wgsl` and its fusable-body twin `triangulate_grid_body.wgsl`) wound every triangle CW-from-above while its own finite-difference normal (`compute_normal`/`tg_compute_normal`) declared +Y for a flat grid — swapping corners 1↔2 and 4↔5 in the per-vertex corner→(col,row) mapping flips the winding to agree, with the triangle coverage and normal computation both unchanged. New GPU test `flat_grid_triangle_winding_agrees_with_vertex_normal` builds a flat XZ grid, dispatches the hand kernel, and asserts every emitted triangle's winding-derived face normal (`cross(v1-v0, v2-v0)`) agrees with its declared vertex normal — verified to fail (`face normal [0,-1,0] disagrees with [0,1,0]`) on the pre-fix layout, confirmed to pass after. The existing `generated_triangulate_matches_hand_kernel` parity test still passes (both shader files changed identically, so generated-vs-hand agreement is untouched). Gated: `cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::triangulate_grid` 5/5 passed; `cargo clippy -p manifold-renderer -- -D warnings` clean; full default `cargo nextest run -p manifold-renderer` 1265/1265 passed.

**Symptom** — scatter_on_mesh align_to_normal placed ~98% of instances upside-down (up mapped to -Y), rendering them under the terrain: BlossomField showed ~25 of 420 flowers; Garden showed 44 of 140. GPU test `align_on_flat_ground_keeps_instances_upright_and_finite` reproduced it deterministically on a hand-built flat quad.

**Root cause (consumer, FIXED)** — scatter's align path trusted the winding-derived face normal; the terrain's triangles wind -Y-facing while vertex normals declare +Y. Fixed in scatter_on_mesh.wgsl by flipping the face normal into the hemisphere of the triangle's vertex normals (mesh-declared outward), with flat + sloped GPU tests.

**Root cause (emitter, UNVERIFIED)** — whether grid_mesh/make_triangles genuinely emit -Y winding (vs the test data coincidence) has not been checked at the emitter. If real, every future winding consumer hits it.

**Fix shape** — read make_triangles' emission order against grid_mesh row-major layout; if winding is inverted, either fix the emission order (check draw paths that might depend on current order) or write the engine-wide rule "vertex normals are authoritative, winding is not" into DEVELOPMENT_REFERENCE.md.

### BUG-117 (render-generator-preset-silently-under-renders-async-loaded-presets) — the look-dev CLI has no wait-for-convergence signal, so a slow-decoding preset can write an incomplete PNG with no warning — LOW (tooling gap, not a runtime bug)
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Ported BUG-100's fix pattern exactly as the fix shape specified: `render_generator_preset.rs` now keeps rendering past the requested `--frames` warm-up, comparing raw (pre-tonemap) readbacks each additional frame, until 3 consecutive frames are byte-identical or a new `--max-frames` cap (default 300) is hit — printing a warning to stderr if the cap is hit before convergence, same as BUG-100's harness did. New `readback_raw_halves` helper does the comparison readback cheaply (no tonemap/composite math) separate from the final PNG-writing readback. Verified by actually running the tool (`cargo run -p manifold-renderer --bin render-generator-preset -- TrivialPassthrough --size 320x180 --frames 5 --out ...`): converged and printed `converged on frame 7 (stable for 3 frames)`, wrote a valid 320×180 RGBA PNG — not just a compile check. Gated: `cargo clippy -p manifold-renderer --bin render-generator-preset -- -D warnings` clean.

**Symptom** — `cargo run -p manifold-renderer --bin render-generator-preset -- ApricotWeather --frames 30` (and even `--frames 90`) sometimes wrote a PNG showing only the ground plane, or only some of the tree's 3 material-filtered objects, with zero indication anything was still loading. Re-running the identical command with `--frames 3000` (real wall-clock ~27s) reliably showed the complete scene. The result depended on wall-clock timing of a background thread, not on any `--frames`/`--param` value the caller controls in a predictable way.

**Root cause** — `node.gltf_mesh_source` (and `node.gltf_texture_source`, `node.image_folder`, `node.depth_estimate_midas`, etc.) parse/decode on a background thread and, per their own documented contract, leave the pre-bound output buffer untouched ("nothing parsed yet ... leave the pre-bound buffer's existing contents") until the background job completes — correct runtime behavior, not a bug. But `render-generator-preset` (`crates/manifold-renderer/src/bin/render_generator_preset.rs`) just runs `--frames N` simulated frames and dumps whatever is in the target texture at the end; each *fresh process* re-triggers the parse from scratch (no cross-process cache) and the tool has no signal for "still loading" vs "fully converged," so the wall-clock race is invisible to the caller. This is the same underlying class BUG-100 hit (FIXED) for `imported_azalea_renders_faithfully_to_png` — that fix added a 3-consecutive-identical-frames convergence check to ONE test harness; `render-generator-preset` (a general dev tool explicitly documented as "the iteration loop for shader work inside a preset: edit JSON → render → Read the PNG") never got the equivalent fix and has the identical race, worse here because a multi-material glTF preset re-parses the same large file once per `node.gltf_mesh_source` instance (4× the wall-clock cost for a 4-material scan).

**Fix shape** — port BUG-100's convergence pattern into `render_generator_preset.rs`: after the requested `--frames`, keep rendering and comparing consecutive readbacks until N (e.g. 3) are byte-identical (or a `--max-frames` safety cap is hit), and print a warning if the cap is hit before convergence. Cheaper alternative: expose a "pending background loads" count off `PresetRuntime`/`EffectNodeContext` that the bin can poll and block on before the final readback. Either removes the silent-partial-render trap for every current and future async-loading primitive, not just gltf.

**Instrument impact:** dev-tooling only — no runtime/show-time path is affected (the primitives' own async-load behavior is correct and by design); this only bit an offline authoring/look-dev session. Worth fixing before Scene 2/3 (or any future large-asset preset work) burns the same wall-clock-guessing cycle again.

### BUG-147 (bokeh-gather-cpu-reference-helpers-dead-without-gpu-proofs) — a `#[cfg(test)]` CPU-reference-parity helper module emits `dead_code` warnings under a plain (no `gpu-proofs`) test compile — LOW (cosmetic, no correctness impact), now confirmed SYSTEMIC across at least 2 primitives
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — gated both `bokeh_gather.rs::cpu_reference` and `bilateral_blur.rs::cpu_reference` behind `#[cfg(all(test, feature = "gpu-proofs"))]` (was plain `#[cfg(test)]`), per the entry's own fix shape. Audited the whole primitives directory for the same shape (`#[cfg(test)] mod cpu_reference`/`cpu_ref`) — found two more hits, `motion_blur.rs` and `ssao_gtao.rs`, but both have a SEPARATE plain-`#[cfg(test)]` module (`analytic_sanity`/an unnamed hand-check module) that consumes the same `cpu_reference` helpers without `gpu-proofs`, so gating those two would break their default-sweep tests — correctly left alone; not the same bug. Verify: `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` clean (0 errors, was 17); `cargo test -p manifold-renderer --lib` still 1224 passed (default sweep unaffected).
**Symptom:** rustc/rust-analyzer flags `BOKEH_N`, `BOKEH_GOLDEN_ANGLE`, `bokeh_hash_angle`, `Plane4` (+ its `texel`/`sample` methods), and `bokeh_gather_texel` in `bokeh_gather.rs` as unused. **Confirmed 2026-07-13, same landing session:** the identical shape reappeared in the just-landed `bilateral_blur.rs` (`K9`, `Fixture`, `depth_at`/`color_at`, `bilateral_texel`) the moment it compiled in the main checkout post-merge — this is a reflex of the CPU-reference-parity authoring pattern itself (`docs/ADDING_PRIMITIVES.md`'s I1-style precedent), not a one-off in one file. Any future primitive following the same pattern will reproduce it.
**Root cause:** these items live in the outer `#[cfg(test)]` module but are only consumed by the nested `#[cfg(all(test, feature = "gpu-proofs"))]` submodule; a compile of test code without `gpu-proofs` builds the outer module and never calls them. The standard scoped gate (`cargo clippy -p manifold-renderer -- -D warnings`, no `--tests`) doesn't compile test code at all, so it stays clean — this is only visible via `--tests` or IDE diagnostics, same blind spot as BUG-126.
**Not caused by this session's diff (for `bokeh_gather.rs`):** untouched by the `bilateral_blur` commit; `git log` shows its last change is P4's original `d85c6dc0`. The `bilateral_blur.rs` instance IS this session's own diff, logged here rather than fixed because it's the same one-line-fix-shape issue as the `bokeh_gather.rs` case and worth batching.
**Fix shape:** move each file's outer CPU-reference-parity helpers under the same `#[cfg(feature = "gpu-proofs")]` gate as the submodule that uses them (or nest them directly inside it). Mechanical, no behavior change. Given it's now confirmed systemic, worth a `docs/ADDING_PRIMITIVES.md` note in the I1-parity-test pattern itself so new primitives don't reintroduce it a third time.

### BUG-144 (prewarm-cache-tests-flake-under-full-lib-parallel-run) — two shared-pipeline-cache prewarm tests race each other under the full parallel `--lib` run — LOW (flaky-gate, not functional)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — per the entry's second fix option: both tests ("entry exists after" instead of "count increased by exactly one" — the assertion changed from `after > before` to `after > 0`, so a prior test having already populated the shared entry no longer fails the delta check. Order-independent by construction now, no cache-key or mutex changes needed. Verify: `cargo test -p manifold-renderer --features gpu-proofs --lib prewarm` → 6 passed, including both previously-racing tests. (Also ran the full unfiltered `--lib` gate: 6 unrelated pre-existing `BadInput` fusion-codegen failures in `freeze/codegen.rs`, untouched by this lane — out of scope, not fixed.)
**Symptom** — `cargo test -p manifold-renderer --features gpu-proofs --lib` (the full GPU suite) fails two tests: `node_graph::primitives::render_scene::gpu_tests::prewarm_pipelines_populates_the_shared_render_cache` and `node_graph::primitives::gltf_texture_source::gpu_tests::prewarm_pipeline_populates_the_shared_compute_cache`, both with a `before=N, after=N` panic (the count didn't grow). Both pass cleanly filtered to just that one test (`--test-threads=1` or an exact-name filter).

**Root cause** — the two tests race to prewarm the SAME process-global pipeline cache (the same cache `GeneratorRegistry::prewarm_all` populates at real startup). Whichever runs second in the parallel test binary finds its target entry already populated by the other's prewarm call moments earlier — an order-dependency the tests don't guard against (no reset of the shared cache between tests, no isolation of the specific pipeline key they check) — so its own before/after delta reads zero. Reproduced deterministically on the unmodified tree via `git stash`/`test`/`stash pop` (VOLUMETRIC_LIGHT_DESIGN P1), ruling out any relationship to that phase's Atmosphere/render_scene edits (the `RenderSceneUniforms` size change, 448→464 bytes, doesn't touch the pipeline-cache path at all).

**Fix shape** — either give each test a cache key/scene shape unique to itself (so the other test's prewarm can't satisfy its assertion), check for "entry exists after" instead of "count increased by exactly one" (weaker but order-independent), or add a named mutex/serial-test guard between the two so they never interleave. Cosmetic scope: the two named `--test gpu_proofs` runs and any single-module `--features gpu-proofs <module>::gpu_tests` filter (the CLAUDE.md-recommended narrow-run pattern) never hit this, and it's fully outside the default `nextest` sweep (gpu-proofs-gated) — only a full unfiltered `cargo test -p manifold-renderer --features gpu-proofs --lib` run is affected.

### BUG-142 (fire-meter-capture-bench-flakes-under-parallel-load) — a hard µs/tick ceiling on fire-meter capture cost fails under contention, same class as BUG-113 — LOW (flaky-gate)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D), same fix as BUG-113 — gated `worst_case_capture_cost_is_negligible_against_the_20ms_frame_budget` behind `manifold-core`'s new `bench-timing` feature (off by default). Verify: `cargo test -p manifold-core --lib` (default, test absent) and `cargo test -p manifold-core --lib --features bench-timing worst_case_capture` (runs, passes) both green.

**Symptom** — `manifold-core::audio_trigger::fire_meter_tests::worst_case_capture_cost_is_negligible_against_the_20ms_frame_budget` asserts a hard microseconds-per-tick ceiling for 512 fire-meter configs and panics when it's exceeded: `fire-meter capture: 512 configs/tick, 254.54 us/tick (budget: 20000 us/frame)` — well under the stated 20ms/frame budget in absolute terms, but over whatever internal ceiling the test itself asserts. An isolated rerun (`cargo nextest run -p manifold-core --lib <test>`) passed immediately after, and a clean full-workspace rerun moments later passed 3192/3192 including this test.

**Root cause (known, by analogy)** — same shape as BUG-113: a wall-clock micro-benchmark with a hard ceiling, run inside the normal correctness sweep, sensitive to CPU contention from nextest's parallel thread pool and whatever else the machine was doing (this session had just finished a `cargo clippy --workspace` compile moments before). `crates/manifold-core/src/audio_trigger.rs` was not touched by this session's changes.

**Fix shape** — same remedy BUG-113 names: give the ceiling real margin under parallel/loaded conditions, retry once before asserting, or move wall-clock ceiling assertions out of the default nextest sweep entirely (a dedicated feature/bin, same convention as `gpu-proofs`). Worth fixing both BUG-113 and this one together since they're the same underlying gate-design gap, not two unrelated bugs.

### BUG-124 (mesh-primitive-tests-clippy-debt-under-tests-features) — clippy fails on `-p manifold-renderer --tests --features gpu-proofs` in files unrelated to any recent change — LOW, gate-scope gap
**Status:** FIXED 2026-07-14 (bug-wave3 lane D), same fix as BUG-126 (this and BUG-126 named the identical 12 findings) — rewrote each flagged index loop to `iter().enumerate()`/`.skip()`/`.take()` in `push_along_normals.rs`, `scatter_on_mesh.rs`, `taper_mesh.rs`, `twist_mesh.rs`, `revolve_curve.rs`, plus `bend_mesh.rs`, `facet_normals.rs`, `gltf_mesh_source.rs`, `morph_mesh.rs`. Re-running the exact gate surfaced 5 MORE pre-existing findings beyond the original 12 (an inner/outer doc-attribute conflict in `coc_from_depth.rs`, an inconsistent-digit-grouping + excessive-precision pair on the same literal in `ssao_gtao.rs`, and a manual `.is_multiple_of()` in `blinn_specular.rs`) — fixed all 5 too rather than leave the gate red; folded into this fix since they're the same mechanical-debt class. Verify: `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` clean (0 errors, was 17); `cargo test -p manifold-renderer --lib` 1224 passed (unaffected); `cargo test -p manifold-renderer --features gpu-proofs --lib prewarm` unaffected. **Merge note (bug-wave3 lane A, folded in from a stale duplicate OPEN copy this same day):** a FUSION_SOTA P4a session had already hit this bug once and logged a precursor compile-error fix (`wgsl_compute.rs`'s `mod gpu_tests` referenced `Marker::StaticParam` with no `use` in scope, a plain `E0433` blocking the gpu-proofs test binary from compiling at all — fixed with one `use` line) before this bug's original 12 lint errors reappeared unchanged underneath it. Preserved here since the duplicate entry carrying it was removed as part of a multi-lane merge dedupe.

**Symptom** — `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` fails with 12 errors (`needless_range_loop`, `manual_range_contains`, `identity_op`) in `push_along_normals.rs`, `scatter_on_mesh.rs`, `taper_mesh.rs`, `twist_mesh.rs`, `revolve_curve.rs` test modules — none touched by the P9 session. The plain `cargo clippy -p manifold-renderer -- -D warnings` (lib+bins, no `--tests`) stays clean, which is why this debt went unnoticed: the CLAUDE.md-specified per-phase gate omits `--tests`, so nobody runs the stricter form routinely.

**Root cause (known)** — ordinary clippy debt in `#[cfg(test)]` code (index-loop patterns, manual range checks, a `1 * COLS` identity-op) that accumulated because the test-scope clippy variant isn't part of the routine gate.

**Fix shape** — mechanical: apply the suggested rewrites (`iter().enumerate()`, `RangeInclusive::contains`, drop the `1 *`) in the five listed files. Small, isolated, no behavior change. Optionally fold `--tests --features gpu-proofs` into the landing-time full-workspace clippy sweep so it doesn't silently drift again.

**Addendum (2026-07-14, FUSION_SOTA P4a session)** — hit again running this phase's `cargo test --features gpu-proofs` gate. One PRECURSOR bug surfaced first and was fixed in this session (out of P4a's scope, but blocking even compiling the gpu-proofs test binary at all): `wgsl_compute.rs`'s `mod gpu_tests` referenced `Marker::StaticParam` at 3 call sites with no `use` import in scope — a plain compile error (`E0433`), not a lint, present at `feat/fusion-sota`'s tip (`8bb94ea6`) before this session touched anything. Fixed with one `use crate::node_graph::freeze::markers::Marker;` line inside that module. Once that compiled, this bug's original 12 lint errors reappeared unchanged (still the same five files, still none touched by this session) — confirming BUG-124 needs no update beyond this note; the compile gap was simply masking it from anyone running `--features gpu-proofs` before this session.

### BUG-110 (osc-receiver-test-type-complexity-clippy-debt) — `manifold-playback`'s `--tests` clippy gate fails on `osc_receiver.rs`, unrelated to any of this session's changes — LOW (lint-only)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — factored `Arc<Mutex<Vec<(String, Vec<f32>)>>>` into a local `type RecordedOsc` alias at both sites, per the entry's fix shape. Verify: `cargo clippy -p manifold-playback --tests -- -D warnings` clean.

**Symptom:** `cargo clippy -p manifold-playback --tests -- -D warnings` (and `--all-targets`) fails
`clippy::type_complexity` twice in [`src/osc_receiver.rs`](../crates/manifold-playback/src/osc_receiver.rs)
at lines 366 and 368: `fn recording_receiver(address: &str) -> (OscReceiver, Arc<Mutex<Vec<(String, Vec<f32>)>>>)`
and its matching `let log: Arc<Mutex<Vec<(String, Vec<f32>)>>> = ...` binding.

**Root cause:** unknown/not investigated — out of scope for this session (BUG-088/072 name only
`osc_timecode.rs` and `audio_mixdown.rs`). Confirmed pre-existing and unrelated to this session's
edits: `git diff dd31cde4 -- crates/manifold-playback/src/osc_receiver.rs` is empty; the file's
last touching commit is `e4f51459` ("F3: external-sync test net"), an unrelated session.

**Fix shape:** trivial — factor the repeated `Arc<Mutex<Vec<(String, Vec<f32>)>>>` into a local
`type` alias (e.g. `type RecordedOsc = Arc<Mutex<Vec<(String, Vec<f32>)>>>;`) at both sites.
Mechanical, no behavior change. Left open per the same file-ownership convention BUG-088 used —
belongs to whoever owns `osc_receiver.rs`'s next change.

### BUG-113 (param-manifest-get-bench-flakes-under-parallel-load) — `bench_resolve`'s hard ns/op ceiling fails under nextest's parallel thread pool — LOW (flaky-gate)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — gated `bench_resolve` behind a new `bench-timing` cargo feature on `manifold-core` (off by default, same convention as `gpu-proofs`), so the wall-clock ceiling no longer runs in the default nextest sweep. Same class fix applied to BUG-142. Verify: `cargo test -p manifold-core --lib` (default, test absent from output) and `cargo test -p manifold-core --lib --features bench-timing bench_resolve` (runs, passes) both green.

**Symptom:** `crates/manifold-core/src/params.rs`'s `params::tests::bench_resolve` times
`ParamManifest::get`'s worst case (40 params, id last) and asserts `best_ns_per_op <= 271.5`
(a 2x ceiling over an old baseline). Under `cargo nextest run --workspace`'s default sweep,
run right after a heavy build and (in this session's case) a real 2-minute recording-soak
process that had just finished, it measured 333.25 ns/op then 398.98 ns/op on consecutive runs
and failed both times; an isolated `cargo test -p manifold-core --lib
params::tests::bench_resolve` immediately after measured 215.02 ns/op and passed, and a
subsequent clean full-workspace nextest run passed 3052/3052 including this test.

**Root cause:** the test is a wall-clock micro-benchmark with a hard-coded nanosecond ceiling,
run inside the normal correctness-test sweep — inherently sensitive to CPU contention from
nextest's shared thread pool and any other load on the machine (the failing runs in this
session followed heavy sequential cargo builds and a real recording capture). Confirmed
unrelated to the change being landed: `crates/manifold-core/src/params.rs` was not touched.

**Fix shape:** give the ceiling real margin for a loaded/parallel run, retry once before
asserting (take the best of N *sequential process* runs, not just N in-process rounds — the
in-process `ROUNDS` loop already exists but can't out-run sustained *external* contention), or
move this out of the default nextest sweep entirely (behind a feature or a dedicated bin, same
convention as `gpu-proofs`) since a wall-clock ceiling assertion doesn't belong in a "safe to
run freely, always green" default suite per CLAUDE.md's own testing-scope description.

### BUG-112 (manifold-ui-all-targets-clippy-debt-audio-setup-panel-graph-canvas-tests) — `manifold-ui`'s `--all-targets` clippy gate fails on two pre-existing, unrelated lints — LOW (lint-only)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — dropped the two unneeded `&` before `format!(...)` in `audio_setup_panel.rs` and replaced the `vec![WireView {...}, ...]` fixture in `graph_canvas/tests.rs` with a plain array literal, per the entry's fix shape. Re-running the exact gate (`cargo clippy -p manifold-ui --all-targets -- -D warnings`) surfaced a THIRD, unrelated `type_complexity` debt in `interaction_overlay.rs` — logged separately as BUG-161 rather than folded in here (different lint, different file, out of this entry's stated scope).

**Symptom:** `cargo clippy -p manifold-ui --all-targets -- -D warnings` fails on two lints that
have nothing to do with this session's changes:
1. `clippy::needless_borrows_for_generic_args` twice in
   [`src/panels/audio_setup_panel.rs`](../crates/manifold-ui/src/panels/audio_setup_panel.rs) —
   lines 2494 and 2498, both `LayerId::new(&format!(...))` where the borrow is unneeded
   (`LayerId::new` already accepts an owned `String` generically).
2. `clippy::useless_vec` once in
   [`src/graph_canvas/tests.rs`](../crates/manifold-ui/src/graph_canvas/tests.rs) — line 2391, a
   `vec![WireView { .. }, ...]` fixture that clippy wants as a plain array.

**Root cause:** unknown/not investigated (test-target lint debt, out of scope for this session).
Confirmed pre-existing and unrelated: `git diff HEAD -- crates/manifold-ui/src/panels/audio_setup_panel.rs
crates/manifold-ui/src/graph_canvas/tests.rs` is empty; both files' last-touching commit is
`f1a35270` ("feat(audio-dock-p4): D7 readability + D8 hygiene (P4)"), an unrelated session. The
scoped, non-`--all-targets` gate this session actually ran (`cargo clippy -p manifold-app -p
manifold-ui -p manifold-recording -- -D warnings`, per CLAUDE.md's worktree convention) is clean
— this only surfaces when test/bench targets are included, same pattern as BUG-110.

**Fix shape:** trivial and mechanical, no behavior change — drop the `&` before each
`format!(...)` argument at the two `audio_setup_panel.rs` sites; replace the `vec![...]` literal
in `graph_canvas/tests.rs` with a plain array literal (`[WireView { .. }, ...]`). Left open per
the same file-ownership convention BUG-110 used — belongs to whoever owns these files' next
change.

### BUG-089 (live-clip-pending-tick-queue-dead-on-all-live-paths) — `LiveClipManager`'s tick-based pending-launch queue can never be populated in production — LOW (dead code, correctness-neutral)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — deleted the whole subsystem per the entry's fix shape: `pending_by_tick`/`pending_by_layer`/`pending_by_clip_id` fields, `PendingLiveLaunch`, `queue_pending`, `remove_pending_by_clip_id`, `activate_due_pending_launches`, `activate_due_pending_launches_at_tick`, `has_pending_activations`, `pending_launch_count`, the `engine.rs` tick-3 call site, and the dead cancellation arm in `commit_live_clip` (now just `if !self.live_slots.contains_key(&layer_index) { return; }`). `trigger_live_clip`/`trigger_live_generator_clip` now call `activate_live_slot_now` unconditionally (the `event_absolute_tick >= 0` branch that used to queue was always false in production, confirmed by this entry's own grep). Deleted `tests/live_clip.rs::pending_launch_queue_activates_at_tick` (the test that only exercised this) and the now-meaningless `pending_launch_count() == 0` assertion in `midi_launch_with_release_before_snap_still_gates_correctly`-family test, plus the now-dead `MockHost::at_tick` helper. Deletion gate: `rg` for every listed symbol across `crates/**/*.rs` returns zero hits. Verify: `cargo test -p manifold-playback --lib --tests` 236+9+10+23+2+8+5+4 passed, 0 failed; `cargo clippy -p manifold-playback --tests -- -D warnings` clean.

Found 2026-07-10 while implementing F2 (MIDI launch quantize, CORE_ENGINE_MAP-adjacent). F2's
brief specifically flagged `activate_due_pending_launches_at_tick` as a deletion candidate and
asked for a caller grep before removing it. That grep turned up more than the one function:
`queue_pending` (`live_clip_manager.rs`) — the only writer of `pending_by_tick` /
`pending_by_layer` / `pending_by_clip_id` and the only place `PendingLiveLaunch.target_tick` is
set — only runs when its caller's `event_absolute_tick >= 0`. Every live producer of that value
traces back to `MidiNoteEvent.absolute_tick`, and `midi_input.rs`'s midir callback (the *only*
constructor of `MidiNoteEvent` in the whole workspace — confirmed by grep, not inference) always
sets it to `-1`. `fire_layer_oneshot` (the audio-trigger path) also always passes `tick = -1`
explicitly. So `pending_by_tick` can never be non-empty on any live path today. Its one live
reader, `activate_due_pending_launches_at_tick` (`engine.rs:803`, called every tick with
`self.last_frame_count as i32` — a frame counter, not a real MIDI clock tick), is therefore an
unconditional no-op in production (`if self.pending_by_tick.is_empty() { return false; }` fires
every call). The sibling beat-based `activate_due_pending_launches` and `has_pending_activations`
have no live caller at all — only `tests/live_clip.rs` exercises them. `commit_live_clip`'s
"pending launch cancellation" branch (the `!self.live_slots.contains_key(&layer_index)` arm) is
similarly unreachable live, since nothing ever queues a launch that skips straight to `live_slots`.

**Fix shape:** delete the whole subsystem — `pending_by_tick`, `pending_by_layer`,
`pending_by_clip_id`, `PendingLiveLaunch` (and its `target_tick` field), `queue_pending`,
`activate_due_pending_launches`, `activate_due_pending_launches_at_tick`,
`has_pending_activations`, the `engine.rs:803` call site, and the dead cancellation arm in
`commit_live_clip` — plus the `tests/live_clip.rs` coverage that only exercises it
(`pending_launch_queue_activates_at_tick`). Left open rather than done as part of F2: the
footprint is a full subsystem across two files and a test, wider than the single function F2 was
scoped to evaluate for deletion, and removing it correctly (without leaving `queue_pending`'s
write side orphaned, or silently changing `commit_live_clip`'s NoteOff behavior for some future
native-clock caller) deserves a dedicated pass with its own review, not a rider on a launch-
quantize fix. F2 left this code untouched and unexercised by its own changes.

### BUG-073 (ui-snap-script-drawer-tween-never-ticks) — the headless `--script` driver has no per-frame animation tick, so a mod armed mid-script renders an unclickable, zero-height drawer — LOW (found 2026-07-08 during PARAM_STEP_ACTIONS P3)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — built fix shape (b): `ParamCardPanel::skip_to_settled(&mut self, tree) -> bool` (settles drawer height, tab-ink, collapse, spawn-pop, delete-fade, value-flash, and value-snap-back tweens in one call, reusing `tick_drawers`/`tick_value_flash`'s own tick logic with a huge `dt_ms` rather than duplicating the settle math; returns whether anything was actually mid-flight) and `InspectorCompositePanel::skip_to_settled` (walks all cards, bubbles the bool up). Wired into `script.rs`'s `Runner::advance_frame` — the ONE seam every dispatch (`Key`/`Pointer`) runs through before `Snapshot`/`Dump` read the tree, since those two don't rebuild themselves — called unconditionally, forcing a rebuild only when something was actually settled (so a script with nothing armed keeps its prior cache-hit behavior; verified this doesn't regress `apply_ui_frame_invalidations`'s `needs_structural_sync` semantics). This is stronger than "audit existing flows for a missing `Step`" (the entry's other fix option): every flow is now correct by construction, no per-script opt-in needed. Verify: re-ran `scripts/ui-flows/param-step-action.json` (uses the pre-arm-in-fixture workaround this bug's report named) and `inspector-drawer-filmstrip.json`/`audio-clip-trigger-add.json`/`select-and-inspect.json` — all still pass; `cargo test -p manifold-ui --lib` 754 passed, `cargo test -p manifold-app --bin manifold` 174 passed; `cargo clippy -p manifold-ui -p manifold-app -- -D warnings` clean.

**2026-07-10 (UI_HARNESS_UNIFICATION P2):** the root symptom — "nothing calls
`tick_drawers`/`Panel::update` with real elapsed time" — is no longer true.
`script.rs`'s `Runner` was repointed at the shared render seam
(`crate::ui_frame::apply_ui_frame_invalidations` +
`composite_main_ui_frame`), and its `AutomationAction::Step` handler now
does a REAL `std::thread::sleep(DT)` + `ui.update()` per stepped frame
(mirroring `cache_path_full_render`'s P0 drawer-tween loop), so a script
that inserts `{"Step": {"frames": N}}` after arming a drawer now genuinely
ticks it toward settlement — confirmed working: `scripts/ui-flows/
inspector-drawer-filmstrip.json` (a fresh 12-frame `Step` after a compact-
toggle click) settles and its filmstrip shows the drawer visibly changing
across tiles. This is fix-shape (a)'s mechanism, just opt-in per script
rather than automatic on every dispatch — **not fully closed**: an EXISTING
flow (e.g. `param-step-action.json`) that doesn't add a `Step` after arming
still hits the original symptom, and this session didn't retrofit it or
build the unconditional auto-settle option (b) (`skip_to_settled()`/
`finish_all()` on `ParamCardPanel`). Revive to CLOSED by either auditing
existing flows for the missing `Step` or building (b).

**Symptom:** in a `cargo xtask ui-snap <scene> --script <flow>.json` run, a
click that newly arms a param's audio mod (or otherwise grows an EXISTING
card's drawer row count) dispatches correctly (confirmed via
`ui_bridge::dispatch` debug instrumentation — the right `PanelAction` fires
and mutates the project), but the drawer's own P1 reveal tween
(`ParamCardPanel::drawer_height_anim`, ticked by
`InspectorCompositePanel::update`'s `tick_drawers`) never advances: the
driver's `AutomationAction::Step` only increments a local `self.clock` field
used for input-event timestamps, nothing calls `tick_drawers`/`Panel::update`
with real elapsed time. The clip region sizing the reveal stays pinned at its
t=0 height (0, if the card is easing from unarmed) forever, so subsequent
rows in that clip region are invisible in the PNG AND unreachable by
`ui.pointer_event`'s hit-test (confirmed: `dump_tree_ex` still reports the
clipped nodes' raw, pre-clip rects with `VISIBLE | INTERACTIVE` flags, so the
dump looks fine while both the render and the click silently no-op — a
"dump says it's there, nothing else agrees" trap worth remembering before
trusting a dump alone against a freshly-armed drawer).

**Consequence for evidence-gathering:** any headless script that arms a
config-drawer-bearing param FOR THE FIRST TIME on an EXISTING card (one that
already went through one build with a smaller drawer) mid-script will show a
believable-looking `PNG`/dump pair with a truncated drawer. The workaround
used in `scripts/ui-flows/param-step-action.json`: pre-arm the mod directly
in the fixture (`ui_snapshot::fixtures::param_steps_scene`) so the card's
*very first* `configure()` call snaps `drawer_height_anim` straight to its
settled target (`param_card.rs`'s own comment: "a *new* param... snaps so it
never stalls half-open") — no tween in flight, no clipping. A REAL in-script
click that only changes content WITHIN an already-open, unarmed-row-count-
stable drawer (e.g. selecting a different Action/Mode segment on a param
that's already armed) is unaffected — confirmed working in the same flow.

**Fix shape:** either (a) give the `--script` driver a `self.rebuild`-adjacent
call that also ticks `ui.inspector`'s drawer/value-flash animations by a
large synthetic `dt` (e.g. `color::MOTION_MED_MS * 2.0`) after every
dispatch that sets `structural_change`, fully settling in one call instead of
requiring many small real-time-gated ticks; or (b) expose a
`skip_to_settled()`/`finish_all()` on `ParamCardPanel` the driver calls
unconditionally before every `Snapshot`/`Dump`/`Pointer`. Either closes the
gap for every future script that arms something mid-flow, not just this one.

### BUG-159 (timeline-scroll-past-playhead-violent-snapback) — scrolling past the playhead during playback violently snaps the view back; should be a smooth edge limit like Ableton — MED (performance-surface feel), reported by Peter 2026-07-14
**Status:** FIXED 2026-07-14 (bug-wave lane B). Root cause: `check_auto_scroll`
(`crates/manifold-app/src/ui_bridge/state_sync.rs`) unconditionally overwrote the
viewport's scroll offset every playback frame with zero suppression mechanism —
not a broken existing flag, there was none. Fix: `TimelineViewportPanel` tracks
the last user-driven horizontal scroll gesture (`note_user_scroll_x`/
`user_scroll_x_recent`, `crates/manifold-ui/src/panels/viewport.rs`), noted from
both wheel-scroll write sites in `window_input.rs` (Shift+scroll pan, native
trackpad horizontal swipe); `check_auto_scroll` yields (returns without moving
the viewport) while a scrollbar drag is active OR a gesture happened within an
800ms grace window — re-engage is automatic and implicit (the very next call
after the grace window elapses resumes following, no separate event). Tests:
`viewport::tests::user_scroll_x_recent_reflects_a_note_then_expires`,
`state_sync::bug159_auto_scroll_yield_tests::{auto_scroll_moves_when_no_user_gesture_is_active,
auto_scroll_yields_to_a_recent_user_scroll_gesture}`. Feel (the 800ms grace
value, whether it matches Ableton closely enough) is Peter's call on the rig —
not test-provable, flagged for his pass.

**Symptom** — during playback, manually scrolling the arrangement past the playhead fights the
playhead-follow auto-scroll: the view violently yanks back to the playhead instead of yielding.
Reference behaviour (Ableton and other professional DAWs): a user scroll takes over — or eases
against a soft limit — and follow re-engages predictably, never mid-gesture.

**Root cause** — unknown, not investigated. Suspect surface: the playhead-follow auto-scroll
writing the viewport offset unconditionally every frame during playback, racing the user's
in-progress scroll gesture instead of being suppressed or eased while one is active.

**Fix shape** — TBD after reading the follow logic; likely a follow-yields-to-gesture rule
(suppress auto-follow while a user scroll gesture is active, plus an explicit re-engage rule)
or an eased soft clamp at the playhead edge. Pin the exact feel against Ableton's behaviour;
acceptance is Peter's hands on it, not a test.

### BUG-161 (ui-snapshot-feature-fails-to-compile-canonical-def-arc-mismatch) — the headless `ui-snap` tool's own compile is broken — LOW-effort mechanical fix, but blocks the prescribed oracle for UI-regression bugs
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — found independently by two concurrent lanes the
same day (lane A while trying to bisect BUG-160, lane C while capturing before/after PNGs for
BUG-048/049/068); this is one bug, lane C's fix landed first. At each of the 8 `E0308` sites in
`ui_snapshot/mod.rs`: `view.canonical_def` → `&view.canonical_def` (6 by-reference call sites) or
`(*view.canonical_def).clone()` (2 by-value sites) — pure borrow/deref through the `Arc`, no
semantic change. `cargo build`/`cargo clippy --features ui-snapshot -- -D warnings` clean.
**Unblocks BUG-160**: its prescribed oracle (`ui-snap inspector` bisection between `a0eba10c` and
its parent) can now actually run.

**Symptom** — `cargo check -p manifold-app --features ui-snapshot --bin manifold` fails with 8 `E0308` mismatched-type errors in `ui_snapshot/mod.rs` (lines 454, 504, 581, 638, 660, 670, 896, 917): `view.canonical_def` is passed where callees (`render::render_graph_to_png`, `render::render_graph_editor_to_png`, others) expect `&EffectGraphDef`, but `canonical_def` is now `Arc<EffectGraphDef>` — a plain `&` no longer coerces. The whole `ui-snap` binary target fails to compile, so no headless UI scene can be rendered at all right now.

**Root cause** — unknown which change flipped `canonical_def` from an owned `EffectGraphDef` to `Arc<EffectGraphDef>` (likely a preset-loading or graph-caching change elsewhere that made the canonical def shared); `ui_snapshot/mod.rs`'s 8 call sites were never updated to match. Confirmed pre-existing on `origin/main` (identical content on the commit this lane branched from) — this session's diff never touches `ui_snapshot/mod.rs`. Not caught by the default `nextest` sweep because `ui-snapshot` is a separate cargo feature, not compiled by a plain build.

### BUG-049 (child-row-right-indent) — Group-child header rows double-pay the indent on right-anchored controls — LOW (visual misalignment, ~20px)
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — `layer_header.rs` split `pad` (left-anchor,
still `PAD + CHILD_INDENT`) from a new `right_pad = PAD` used by every right-anchored x/width in
`compute_layer_row`/`compute_audio_row`: `handle_x`, `dd_w` (Blend dropdown), and both
`right_edge` computations (routing form + audio Gain/Send row). Class-swept: `rg '\bpad\b'` in
the file found no other right-anchored use outside these five. `layout_matches_frozen_oracle`'s
hand-copied oracle updated identically in the same commit (mirrors the live fix rect-for-rect);
`manifold-ui --lib` green (26/26 in the file).

**Found 2026-07-07 by the label-collision fix worker (timeline-ux pass), verified in the
Liveschool after-PNG.** `layer_header.rs:489`: `handle_x = w - pad - HANDLE_W - 8.0` uses
`pad = PAD + CHILD_INDENT`, but the indent only moves the card's LEFT edge — so child
cards get a ~28px interior right margin vs 8px on top-level rows. Drag handles and Blend
chips sit ~20px left of their top-level siblings, and the collapsed name budget is 20px
tighter than necessary (it contributed to how early BUG-fixed label truncation kicks in).

### BUG-048 (arm-two-reds) — Automation ARM idle vs armed are both red, distinguished only by shade — LOW (stage-legibility; behavior-changing mode)
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — ARM no longer shares REC's red pair.
`transport.rs::automation_group`: idle = `BUTTON_INACTIVE_C32` (matches sibling LANES/BACK idle
chrome), armed = `STATUS_WARNING` (amber) — never red, so it can't be mistaken for REC's
recording/not-recording pair at stage distance. Class-swept (`rg RECORD_RED RECORD_ACTIVE`):
the only other survivors are REC itself (a genuine on/off pair, correctly red) and BACK's
override-latch (red vs neutral gray, not two reds against each other — no ambiguity). UX call
made without Peter (he's away): amber/`STATUS_WARNING` chosen over reusing
`AUTOMATION_LINE_COLOR` because that token is itself `RED_ACTIVE` (would have reintroduced the
exact bug). `automation_state_updates_in_place` test updated to pin the new colors; before/after
PNGs saved for Peter's look-pass (see session report).

**Found 2026-07-07 (timeline-ux headless audit).** `transport.rs::automation_group`:
idle ARM = `RECORD_RED`, armed = `RECORD_ACTIVE` — a deliberate mirror of the REC
active/idle pair. But REC's two states are "recording or not", while ARM's decide what
touching a param DOES (override the lane vs punch automation INTO the arrangement) —
a wrong read on stage silently writes automation into the show.

### BUG-101 (setup-spectrogram-scroll-offset) — Docked Audio Setup spectrogram blit doesn't follow the body scroll offset — LOW
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — `audio_setup_panel.rs::build_nodes` now shifts
`self.scope_rect` by the same `-offset` applied to the scrolled tree content, right after
`self.scroll.offset_content(tree, -offset)`. `update_band_meters` derives its geometry from
`scope_rect()` too, so the 2026-07-11 follow-up note (band meters sharing this root cause) is
fixed by the same one-line change. New regression test
`scope_rect_follows_body_scroll_offset` scrolls the docked body via `handle_scroll` and asserts
`scope_rect().y == unscrolled_y - offset`; still dormant in the shipped app per the existing
note (`AudioSetupPanel::handle_scroll` has zero call sites), so this closes the mechanism ahead
of it being wired up.

**Found 2026-07-10 during AUDIO_SETUP_DOCK P1** (worker shortcut #3, orchestrator-logged
at landing). The spectrogram waterfall is a GPU blit positioned by a CPU-side `scope_rect`
computed at build time in `audio_setup_panel.rs`, and that rect does not add the
`ScrollContainer` scroll offset. At `scroll_offset == 0` (the default, and everything the
P1 gate exercises) it's correct; once the docked body is scrolled, the waterfall draws at
its pre-scroll position while the rows around it move. **Symptom:** spectrogram visually
detaches from its section header when the panel body is scrolled. **Fix shape:** offset
`scope_rect` by the scroll delta (or parent the blit rect to the scroll content like the
rows), and clip it to the scroll viewport. **Oracle:** not reproducible headless (the blit
doesn't run in the snapshot harness) — needs the live app or a harness that runs the scope
blit; a scrolled-body render test would guard it.

**Update 2026-07-11 (fire-meter-unification pass):** confirmed the same root cause also
explains `SendRowIds`'s per-send level meter (fixed this session — `meter_track: NodeId`
now read live instead of cached `meter_x/y/w/h`) and `AudioSetupPanel::update_band_meters`
(`audio_setup_panel.rs`, still open) — both derive from geometry captured in `build_nodes`
before its own `self.scroll.offset_content()` call. The send-meter case had a track node to
anchor to, so it's fixed. The scope/band-meter case roots in `self.scope_rect` (`pub fn
scope_rect(&self) -> Option<Rect>`, no `&UITree` param) — making it scroll-live needs a
signature change threading `&UITree` through every caller (the present-pass blit, both
hit-tests, `update_scope_lane_labels`), which is this bug's real fix shape and is out of
scope for a geometry-sourcing-only pass. Currently dormant either way:
`AudioSetupPanel::handle_scroll` has zero call sites anywhere in the app (grepped), so
`scroll_offset()` is always 0 in the shipped build and neither symptom is user-reachable yet.

### BUG-081 — Audible blip when an audio clip's voice is built (play-then-pause leaks ~10ms of the file's start) — LOW
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — `make_voice` now applies `.volume(0.0)` to the
`StaticSoundData` before `manager.play`, so the voice is built silent instead of played-then-
paused; the per-tick sync path already restores the real volume via `set_volume(volume,
declick())`, so activation is unaffected. Kills the whole class, including the race window a
pause-based fix wouldn't close. `manifold-playback` builds clean.

**Symptom** — a very subtle pop/click from the speakers at the moment an audio file is
loaded onto the timeline (e.g. Finder drag-drop). Reported by Peter 2026-07-05.

**Root cause** — [audio_layer_playback.rs:171-179](../crates/manifold-playback/src/audio_layer_playback.rs#L171-L179):
`make_voice` calls `manager.play(data)` at full volume and only then
`handle.pause(Tween::default())`. kira's `pause` is a fade-out — and `Tween::default()`
is a **10ms** linear fade (kira-0.9.6 `tween.rs:110`), not instantaneous — so the first
~10ms of the file renders audibly before the voice reaches its "start paused at 0" state.
Any file whose first samples carry signal produces the blip. (The 5ms `declick()` tween
used everywhere else in this module doesn't apply here; this is the one edge built on
kira's default tween.)

**Fix shape** — build the voice silent instead of pausing it after the fact: apply
`.volume(0.0)` to the `StaticSoundData` before `manager.play`, keep the pause+seek. The
per-tick sync path already restores the real volume via `set_volume(volume, declick())`,
so activation is unaffected. This kills the whole class including the race where an audio
callback fires between play and pause. One-line-ish, `manifold-playback` only.

### BUG-031 — Layer context-menu + rename still address layers positionally — LOW (follow-up to the LayerId migration `877852a9`)
**Status:** FIXED 2026-07-14 (bug-wave3 lane C) — both clusters now carry `LayerId` end to end.
`ContextAddVideoLayer/GeneratorLayer/AudioLayer/DeleteLayer/DuplicateLayer/PasteAtLayer/
ImportMidi/Ungroup/SetLayerColor` and `DropdownContext::LayerContext` all switched from `usize`
to `LayerId`; consumers re-resolve the live index via `project.timeline.find_layer_by_id`/
`find_layer_by_id_mut` at dispatch (execution) time instead of baking in the index at menu-open
time — mirrors the pattern `DeleteLayerClicked(LayerId)` already used. `TextInputField::
LayerName(usize)` became a bare `LayerName` variant with the id stored on `TextInputState::
layer_id` (mirrors the existing `MarkerName`/`marker_id` and `AudioSendLabel`/`audio_send_id`
idiom already on this `Copy` enum) — no `Copy`-dropping cascade needed. Class-swept the
`TrackRightClicked` menu-build site too (a second, separate constructor for the same shared
`Context*` actions) — it now resolves a `LayerId` from the row-under-cursor before building menu
items, closing the same window there. `cargo check --workspace --tests` and `cargo clippy -p
manifold-ui -p manifold-app -- -D warnings` clean; `manifold-app` test suite 189/189.

**Root cause** — the primary layer-header actions were migrated to carry a stable `LayerId`
(commit `877852a9`, kills the panel-index-vs-live-model collision). Two related clusters were
deliberately left positional to keep that diff bounded:
- The **`Context*Layer` right-click-menu family** (`ContextPasteAtLayer`, `ContextImportMidi`,
  `ContextAddVideoLayer/GeneratorLayer/AudioLayer`, `ContextDuplicateLayer`, `ContextUngroup`,
  `ContextDeleteLayer`, `DropdownContext::LayerContext`) still carry a `usize`. `LayerHeaderRightClicked`
  now carries the id and `ui_root` resolves it to the current row synchronously when the menu opens,
  so there's no regression — but the menu ITEMS bake in that index, leaving a (rare) stale window
  between menu-open and item-click.
- **`TextInputField::LayerName(usize)`** (layer rename): the enum derives `Copy`, and `LayerId`
  isn't `Copy`, so migrating it forces dropping `Copy` and cascades through the whole text-input
  subsystem (`app.rs` field handling). The double-click intercept resolves id→index locally, so the
  rename has the same (unchanged) stale window it always had.

**Symptom** — none observed; latent. A context-menu action or a rename committed after the layer
list changed under it (another command, undo/redo, MIDI phantom layer) could hit the wrong layer.
Same bug class as the migration killed for the primary controls.

**Fix shape** — carry `LayerId` in the `Context*Layer` family (thread it from
`LayerHeaderRightClicked` through the menu items) and switch `TextInputField::LayerName` to
`LayerId` (drop `Copy` from `TextInputField`, fix the fallout in `app.rs`). Mechanical, compiler-driven.

### BUG-068 (inspector-scene-cliphit-overlap) — `inspector` ui-snap fixture clip/panel hit overlap — LOW
**Status:** FIXED 2026-07-14 (bug-wave3 lane C)

**Found 2026-07-08 during DRAG_CAPTURE P1 L3 authoring; pre-existing at `b9304330`.** The
`inspector` snapshot scene forces a generous `inspector_width` (600px of the 1536px canvas, set
in `ui_snapshot::mod` for the inspector-subject scenes) so the selected layer's param cards have
room — at the fixed 24px/beat zoom that leaves only ~29 beats of clip area before the inspector
column starts. GLOW's clip (48 beats), PLASMA's clip (48 beats), and TEXT BOT L's RETURN clip (24
beats starting at beat 24) all extended past that boundary. `TimelineViewportPanel::
visible_clip_rects` only culls clips fully outside the tracks rect — it returns each surviving
clip's FULL, unclamped pixel width (the comment notes "the GPU scissor clamps partials at the
edges" for *rendering*, but the returned hit-test rect is unclamped) — so those three clips' hit
rects genuinely overlapped the inspector column's screen region, meaning no clip in the scene was
simultaneously uniquely-labeled and safely draggable near the inspector edge. This is why P1's
`drag-clip-release-over-inspector.json` flow proves position-independence on the `timeline` scene
(drag past the tracks' right edge) instead. Fixture-only, no app runtime impact.

**Fix (FIXED @ this session):** shortened GLOW's and PLASMA's clips from 48 to 20 beats and
TEXT BOT L's two clips from 24+24 to 10+10 beats (`fixtures.rs::inspector_scene`) — every clip
now ends by x≈710, well clear of the inspector's left edge at x=936 (226px margin). Regression
test `bug068_clip_panel_overlap::inspector_scene_clips_clear_the_inspector_column`
(`ui_snapshot/mod.rs`) builds the real scene, reads `ui.viewport.visible_clip_rects`, and asserts
every clip's right edge stays left of `ui.layout.inspector().x` — fails red on the pre-fix
48-beat clips, green post-fix. **Class swept:** `bug060`/`gltfscene`/`bug047`/`paramsteps` share
the same `inspector_width = 600.0` override and could in principle grow a similarly long clip,
but none of them are used for clip-drag/hit-test flows today (their subject is inspector
scroll/param display) — not touched; revival trigger is a future script that drags a clip in one
of those scenes.

### BUG-125 (preset-runtime-generator-picks-first-final-output-nondeterministically) — a generator preset JSON with more than one `system.final_output` node has its tracked output picked via `AHashMap` iteration order, not graph position — LOW today (no shipped preset has two), but a real correctness trap
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`), option (a) from the fix shape — reject at load. `PresetRuntime::from_def`'s generator path (`preset_runtime.rs:2617`) now counts `FINAL_OUTPUT_TYPE_ID` matches before resolving the tracked output; a count > 1 returns a new `JsonGeneratorLoadError::MultipleFinalOutputs { count }` instead of silently picking one via `.find()`. Threaded through `graph_tool validate`'s error → `ValidationIssue` conversion too (`validate.rs:216`), so a bad preset JSON is caught by the pre-flight tool as well as the runtime loader — the invariant is enforced at both entry points, not just one. Regression test `dual_final_output_is_rejected_at_load` builds a real two-`final_output` generator graph and asserts the new error. Gated: `cargo clippy -p manifold-renderer -- -D warnings` clean; `cargo nextest run -p manifold-renderer` full crate sweep 1265/1265 passed.

**Symptom** — `PresetRuntime::from_def`'s generator path resolves its ONE tracked output via `graph.nodes().find(|inst| inst.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID)` (`preset_runtime.rs` ~line 2566). `Graph::nodes()` iterates the graph's `AHashMap<NodeInstanceId, NodeInstance>`, whose iteration order is not insertion order and not guaranteed stable across runs (random-seeded hash). A JSON graph with TWO `system.final_output` nodes (e.g. one authored to inspect a second Texture2D output alongside the primary) gets one of the two picked *nondeterministically per process*, and `render()`'s per-frame `replace_texture_2d` installs the host's canvas render target onto whichever one won — silently overwriting that node's real producer-allocated texture (format included) with the canvas's format. Reproduced empirically: a scene wiring both `color` (Rgba16Float) and a second `R32Float` output each to their own `system.final_output` sometimes rendered correctly and sometimes corrupted the second output's texture with the canvas's Rgba16Float data, occasionally triggering a genuine Metal command-buffer fault (`kIOGPUCommandBufferCallbackErrorInnocentVictim`) when the mismatched byte layout confused a subsequent GPU pass — this looked exactly like a device-contention flake until isolated to specifically the dual-final-output configuration (single-final-output graphs, rebuilt in the same tight loop, never flaked).
**Root cause** — `.find()` over an unordered map with more than one matching element is inherently ambiguous; the generator-path data model (`PresetIo::Generate { final_output_input_resource: ResourceId, .. }`, singular) was never designed to support more than one `system.final_output` and doesn't validate that assumption at load time.
**Fix shape** — either (a) `from_def`'s generator path errors loudly (`JsonGeneratorLoadError`) when it finds more than one `system.final_output` node instead of silently picking one, or (b) if multi-output generators are a real desired feature, design a named/keyed output surface (a stable `handle`, not `.find()`'s first hash-map hit) and thread it through `PresetIo::Generate`. Until then: document (here, and in a `from_def` doc comment) that generator JSON must carry exactly one `system.final_output`; a second output's texture is only safely inspectable via `dump_textures_all` fed by a WIRE to any non-`FinalOutput` sink (dead-end consumer — the resource still gets a step-output binding per `execution_plan.rs`'s `consumed_outputs`, without touching the ambiguous single-output tracking at all). `crates/manifold-renderer/tests/gpu_proofs/gbuffer_depth.rs`'s module doc documents and works around this exact trap.

### BUG-122 (graph-editor-node-face-loses-type-name-when-custom-named) — node cards show only the custom name, type name shows nowhere — LOW/MED authoring legibility
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Generalized the `(WGSL)` marker's existing append-not-replace precedent, per the backlog's own suggested fix shape: `display_label` (`snapshot.rs:838`) now renders `"<author title> — <friendly type>"` when they differ, and just the plain label when they'd be identical (no "Blur — Blur" on a no-op rename). Single-function fix — `render.rs`'s header draw already elides long titles to the node's width, so the compound form degrades gracefully with no render-side change needed. 4 new unit tests on `display_label` (compound form, identical-skip, no-title passthrough, WGSL marker still appends after the compound). Gated: `cargo clippy -p manifold-renderer -- -D warnings` clean; `cargo nextest run -p manifold-renderer` full crate sweep 1264/1264 passed.

**Symptom** — In the graph editor, once a node has a custom (author-assigned) title, its face shows only that name. The node's actual type (e.g. `node.blur`, `node.mix`, `node.scatter_on_mesh`) is nowhere on the card — no subtitle, no badge, no tooltip fallback — so a graph with several renamed nodes becomes unreadable as to what each node actually is.

**Root cause (found)** — `display_label()` (`crates/manifold-renderer/src/node_graph/snapshot.rs:838-848`) computes the header title as: the author title if set, else the friendly palette label, else a prettified type id. When an author title exists, it's returned as-is — the friendly/type label is dropped entirely, with the sole exception of a `(WGSL)` suffix appended for `wgsl_compute` nodes. `render.rs:625` then draws only this single `title` string on the node face; there's no second field carrying the type id anywhere for display. This has been the shipped behavior since `ebd48cde` ("Node titles: honor an author title on any node, keep (WGSL) marker", 2026-06-03) — not a recent regression, just newly bothering Peter now that graphs carry more custom-named nodes (Scene 2/3 authoring).

**Fix shape** — `display_label` (or its caller) should combine both when an author title is set, not choose one: e.g. a small secondary type line under the header, a tooltip that always surfaces the type id, or a "Custom Name — Friendly Type" compound header. The `(WGSL)` marker's append-not-replace pattern for `wgsl_compute` is the precedent to generalize.

### BUG-121 (graph-editor-effect-card-missing-mapping-drawer-chevron) — sideways slider-mapping drawer chevron missing from the effect/generator card — HIGH authoring surface (users can't edit mappings at all)
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Root-caused live and closed the whole class: (1) `InspectorCompositePanel` (the shared inspector/card-lane host, used by BOTH the main window and the graph-editor window's own `UIRoot` instance) had no `CardContext` plumbing at all — every card it built via `ParamCardPanel::default()` always defaulted to `CardContext::Perform`, confirming the backlog's suspected lead. Fixed at the host: added `InspectorCompositePanel::set_card_context`/`card_context` field, applied in `reconcile_cards`/`configure_gen_params` to every card built (new AND already-held, including mid-collapse `dying` ones); `Workspace::new` calls `ui_root.inspector.set_card_context(CardContext::Author)` once for `WorkspaceKind::GraphEditor`, never for `Main` — a single, structural fix, not a per-card patch. (2) A second, independent gap: even with the chevron rendering, its click emits `PanelAction::OpenCardMapping`, which `ui_bridge::mod.rs` routed to "handled in app_render.rs" — but app_render.rs never actually handled it (`self.editor_mapping_popover.open()` had zero call sites app-wide). Wired it: `app_render.rs`'s editor-card-action loop now resolves the watched target's reshape (`watched_full_reshape`) and the clicked card's own chevron anchor (`InspectorCompositePanel::mapping_chevron_rect`, pre-existing but never called from production) and opens `editor_mapping_popover` there. Host-level regression test added (`inspector.rs::author_context_host_draws_resolvable_mapping_chevron`) proving an Author-context host draws a resolvable chevron and a Perform-context host never does — guards the exact gap that shipped (the widget's own unit tests already passed while unreachable app-wide). Gated: `cargo clippy -p manifold-ui -p manifold-app -- -D warnings` clean; `cargo nextest run -p manifold-ui -p manifold-app` 980+189 passed. Not independently confirmed live in the running app (no GUI session this pass) — Peter's eyes still owed; the click path is otherwise fully exercised by the new test down to the resolvable anchor rect.

**Symptom** — The graph editor's effect card (and, by the same code path, the generator card) has lost the right-edge chevron that opens the sideways drawer for a param's slider mapping / range (drag trim range, Ableton range, etc.).

**Root cause (suspected, strong lead)** — The chevron and its click action are gated by `author && info.mappable` (`param_card.rs:2578`, `:2792`), where `author = self.context == CardContext::Author` (`param_card.rs:1863` etc.). `context` defaults to `CardContext::Perform` (`param_card.rs:654`) and only ever changes via `set_context()` (`param_card.rs:1205`), whose doc comment says "the host sets it once on its dedicated panel." A repo-wide search finds **zero** production call sites for `set_context(CardContext::Author)` — every call is inside `param_card.rs`'s own `#[cfg(test)]` module. `info.mappable` itself isn't the problem: `manifold-app/src/ui_bridge/state_sync.rs:2098` sets it unconditionally `true` for every row built from the live param manifest. If no live host actually calls `set_context(Author)`, the mapping drawer (`PanelAction::OpenCardMapping`, only ever emitted from these two gated chevron-click handlers) is unreachable everywhere in the shipped app, not just missing on the effect card specifically. Not yet confirmed which struct is "the host" from the doc comment, nor whether its `set_context` call was removed or never wired up — needs a live repro plus git blame on the actual host construction site before calling this fully root-caused.

**Fix shape** — find the panel host that owns the graph-editor's Author-context `ParamCardPanel` instance and confirm/restore its `set_context(CardContext::Author)` call; add a regression test at the host level (not just param_card's internal unit tests, which already correctly cover Author-context behavior in isolation) so a missing wire-up like this fails loudly again.

### BUG-012 — Fragment `tex_` port-rename corrupts scalar params named `tex_*` — LOW
**Status:** FIXED 2026-07-14 (bug-wave lane A, `bug/wave3-lane-a`). Root fix at the class level: `wgsl_compute.rs`'s fragment-form input rename (`:581`) now filters to texture-typed ports (`matches!(inp.ty, PortType::Texture2D | PortType::Texture2DTyped(_))`) before stripping `tex_`, mirroring the sibling binding-key rename's existing filter (`:586`, `BindingKind::SampledTexture`) — the two renames can no longer disagree. Regression test `fragment_scalar_param_named_tex_prefixed_is_not_stripped` proves a `@param: tex_speed` scalar keeps its full port name and its param-manifest entry. Gated: `cargo clippy -p manifold-renderer -- -D warnings` clean; `cargo nextest run -p manifold-renderer node_graph::primitives::wgsl_compute` 32/32 passed.

**Root cause** — [wgsl_compute.rs:544-548](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L544-L548):
the fragment-form rename loop strips a literal `tex_` prefix from EVERY input port name with
no type filter (the sibling texture-binding rename at 549-561 IS filtered to
`SampledTexture`). A scalar `@param: tex_speed` exposes port `speed` while the uniform layout
and params stay keyed `tex_speed`; the dispatch-time wire lookup misses and the live wire is
silently ignored.

**Symptom** — a wired LFO/Ableton control on such a param renders as connected but never
moves the value. Latent — no shipped preset uses a `tex_`-prefixed param name.

**Fix shape** — filter the rename to texture-typed ports, mirroring lines 549-561. One-line.

### BUG-076 (inspector-scroll-underestimates-content-height) — `try_inspector_scroll` clamps to a tiny max_scroll on genuinely tall content — LOW (found 2026-07-08 during UI_CLIP_AND_Z_OWNERSHIP_DESIGN P1)
**Status:** FIXED (closed as not-reproducible) — Peter's call 2026-07-14: "I also think this is not a real bug, I don't think I can even reproduce it." For the record: the original 2026-07-08 log carried a concrete measurement (~13–20px max_scroll on a stack ~1200px too tall), so something was observed once — but two investigations found no mechanism, every fixture-level test passes, and it doesn't reproduce on the rig. Reopen if a tall inspector stack ever won't scroll live. History: 2026-07-13 (`8d37d5e0`): the drawer-tween undercounting theory below was built and tested (a `ParamCardPanel`-level and an `InspectorCompositePanel`-level test with a 9-card stack, several audio-mod drawers armed at `configure()` time, zero `tick_drawers` calls) and RULED OUT — a card's `drawer_height_anim` is already snapped (not eased) to its settled target on first `configure()` for the single-configure/no-tick path, so `compute_height()` does not undercount there. Root cause remains open; next place to look is the real `state_sync`/`build` call ordering in `manifold-app`, or a card-reuse scenario the single-configure fixture doesn't cover. Regression tests for the ruled-out theory are kept as coverage. 2026-07-14: not to be confused with BUG-060 (inspector scroll ARTIFACTS, rig-verified FIXED + class-killed) — Peter asked whether this was that; it isn't. This entry is the scroll-RANGE clamp on tall content (can't scroll far enough), still unexplained.

**Symptom:** built a headless gate scene (`ui_snapshot/fixtures.rs`'s `bug060_scene`, added this
session) with 9 stacked effect cards, several with audio-mod drawers open — visibly, per the
unscrolled render (`target/ui-snapshots/bug060/bug060.png`), several cards extend well past the
1216px-tall canvas. Calling `UIRoot::try_inspector_scroll` (the same method
`window_input.rs`'s real mouse-wheel handler calls) with a delta of 300, 1000, or 1_000_000 all
converge to the SAME ~13-20px of movement and then stop — as if `max_scroll()` were computed as
roughly 20px, not the ~1200px the visible overflow implies.

**Root cause:** unknown — suspected but not confirmed. `ScrollContainer::apply_scroll_delta`
clamps against `self.content_height`, set via `InspectorCompositePanel::update_scroll_bounds`'s
`right_column_height()` -> `layer_column_height()`, which sums `card.compute_height()` per
effect card. Suspect: `compute_height()` reads a drawer-open-tween-animated height
(`drawer_height_anim`, see `param_card.rs`'s `drawer_open_tween_reserves_interpolated_height_
clips_then_settles` test) that starts at/near 0 and needs `tick_drawers(dt)` calls to reach its
settled value — a card configured with its audio mod ALREADY armed (as `bug060_scene` does, no
"click to open" step) renders its FULL drawer immediately (the build path uses the target
height directly) but `compute_height()` may still be reading the un-ticked animation state,
undercounting every card's height by its drawer's contribution. Not verified: whether
`configure()` seeds the animation at its target when armed from a cold build, or always starts
from 0.

**Fix shape:** instrument `right_column_height()`/`card.compute_height()` directly (a
`manifold-ui` unit test asserting `layer_column_height() ≈ sum of settled per-card heights` for
a 9-card, all-drawers-open fixture) to confirm or rule out the animation-state theory; if
confirmed, seed `drawer_height_anim` at its target value on first configure when the mod is
already armed (mirroring how the card already renders it), not just on a later toggle.

**Impact on this session:** blocked producing a scrolled-to-bottom PNG for
`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1's BUG-060 acceptance demo — worked around by deciding the
stopgap-removal question via a direct unit test (`InspectorCompositePanel::try_scroll_in_place`
called with a 1,000,000 delta, `manifold-ui`'s own suite, no PNG round-trip needed) instead of
the headless CLI harness. Also found and partially fixed en route, independent of this bug: the
L3 script runner's `Gesture::Scroll` never reached the inspector at all before this session
(routed only through the generic `UIEvent::Scroll` pipeline, which is real for the
dropdown/timeline but a no-op for the inspector's direct-call scroll path) — `script.rs` now
branches on `ui.layout.inspector().contains(center)` and calls `try_inspector_scroll` directly,
matching `window_input.rs`'s real dispatch. That fix is real and committed; this bug is what's
left after it.

### BUG-015 — Inspector sections render overlapping / at stale offsets after scroll — MED (repro needed)
**Status:** FIXED — closed by Peter's call 2026-07-14 (staleness audit). The root-cause fix for the stale-chrome-state class shipped 2026-07-08 (`738f4e94`/`4319eb8d`, `fix/bug-015-out-of-region-dirt`: the incremental cache path falls back to a full render on out-of-sub-region dirt), with tests and gates. The original 2026-07-04 "sections interleaved" sighting was never reproduced (headless attempts 2026-07-05 and 2026-07-07 ×2). Reopen if the sighting recurs.

**Symptom** — observed once by Peter, 2026-07-04, right after the timeline-P0 / multi-select
UX changes landed: the layer inspector drew its sections interleaved — the MIDI block
(MIDI / CHANNEL / DEVICE) and the audio-send block (send dropdown, +0.0 dB) overlapping
each other with a dead band between them, and the "No audio input" header clipped mid-panel.
Described as "a scrolling bug with the UI timeline updates". Screenshot lives in the
2026-07-04 session transcript.

**Root cause** — unknown. Suspect surface: inspector section Y-layout vs. scroll offset
(the `single-source-y-layout` invariant) or a stale subregion scissor
(`subregion-scissor-invariant`) going stale when timeline updates force a rebuild while the
inspector is scrolled.

**Repro** — not yet pinned. First step is reproducing: select a generator layer, scroll the
inspector, then trigger timeline churn (clip drag / multi-select updates) and watch for
section overlap.

**Fix shape** — TBD after repro. If it's the known invariant class, the fix is at the layout
single-source, not per-section patches.

**Repro attempt 2026-07-07 (timeline-ux headless audit)** — scroll-seeded `states` render
(101px) + driven inspector scroll on the `inspector` scene: sections stay correctly laid
out in both. Not reproduced. The missing ingredient per the symptom is timeline churn
DURING a scrolled state (rebuild-while-scrolled); the `--script` driver can now interleave
scroll + clip-drag + snapshot in one flow (post real-dispatch fix, this branch), so a
dedicated repro flow is now writable when this bug is next picked up.

**Sighting + concrete progress 2026-07-07 (Opus session)** — Peter hit inspector
artifacts again: on a Fluid Simulation generator (Master tab), stale fragments at the
panel's left edge (one is a patch of viewport/video showing through) plus a clipped sliver
above the Layer/Master tab strip. Screenshot in this session's transcript. May be this bug
or a close sibling — same suspect surface (stale inspector content), same repro difficulty.

_Ruled out this session:_
- NOT the just-merged trigger-gate drawer (§9): the drawer is CLOSED in the repro
  screenshot. Two proposed mechanisms for it — a "Mode row" escaping its clip parent, and an
  unbalanced Overlay paint-layer push — are both refuted by the code (every node in
  `build_toggle_trigger_row` parents to one `parent`; there is zero paint-layer manipulation
  in the card/drawer path).
- NOT a settled-state containment error: built the armed trigger-gate card into a real
  `UITree` and measured — one root node, max node bottom == the card's reserved height
  exactly, zero overflow. Height accounting (incl. the Mode row via `audio_config_height(true)`)
  is exact.

_New concrete suspect (stronger than the two above):_ the inspector's incremental atlas
cache. `UICacheManager::render_dirty_panels`
([ui_cache_manager.rs:175](../crates/manifold-renderer/src/ui_cache_manager.rs#L175))
repaints only dirty CARD sub-regions and trusts `LoadOp::Load` for everything else. The
sub-regions are the cards only
([inspector.rs:506 `sub_region_ranges`](../crates/manifold-ui/src/panels/inspector.rs#L506)) —
section backgrounds, tab strip, padding, inter-card gaps and margins sit in NO sub-region, so
an incremental frame never repaints them and a stale pixel there survives until the next full
render. The guard `extents_unchanged`
([:282](../crates/manifold-renderer/src/ui_cache_manager.rs#L282)) approximates each card's
painted extent by its FIRST node's bounds, so anything a card paints OUTSIDE its frame is
untracked.

_Measured seed candidate:_ `build_toggle_trigger_row`
([param_slider_shared.rs:1532](../crates/manifold-ui/src/panels/param_slider_shared.rs#L1532))
lacks the `drawer_reveal` reveal-clip that `build_param_row` has
([:2005-2017](../crates/manifold-ui/src/panels/param_slider_shared.rs#L2005)), so its drawer
paints ~120px below the card frame, unclipped, for the whole open/close tween (measured:
0 clip regions, 119.5px overflow, vs the slider path's 1 clip region that contains its
overflow).

_The crack in the hypothesis (why this is a reasoning problem, not a quick fix):_
`extents_unchanged` keys on the frame's bounds, and the trigger-drawer overflow exists only
while the frame is ALSO resizing (mid-tween), which changes the frame bounds → guard trips →
full self-clearing render → no ghost. So the guard MAY already prevent this exact ghost class.

_The one open reasoning question:_ is there any realistic in-place edit that keeps a card's
first-node (frame) bounds stable while changing what it paints outside that frame — OR that
paints into the never-repainted margins outside all sub-regions? If yes, the ghost is real;
fix = always repaint the inspector's opaque full-rect background before the dirty sub-regions
AND clip every card render to its own frame. If no, the guard covers the card case and the
culprit is the margins / a panel-boundary atlas-staleness issue (which fits the left-edge +
video-bleed fragments better).

_Repro difficulty:_ `render_ui_to_png` renders the tree directly and bypasses
`UICacheManager` entirely
([render.rs:44-51](../crates/manifold-app/src/ui_snapshot/render.rs#L44)), so NO existing
headless snapshot can show this class — every snapshot is a clean full render. A repro needs
a new harness driving `render_dirty_panels` across full→edit→incremental and reading back the
atlas. Blast radius is contained: only the inspector passes `sub_regions`; every other panel
full-renders when dirty and can't ghost this way. Handed to Fable as a reasoning task
(2026-07-07). Same family as BUG-025 (timeline-scissor-bleed).

**Verdict (Fable reasoning pass, 2026-07-07) — hypothesis REFUTED for the card-ghost class;
a different, real hole found; the video fragment exonerates the atlas entirely.**

_1. The card-ghost class cannot occur — three independent seals, verified in code:_
- **Every card-geometry tween runs under full invalidation, never the incremental path.**
  `tick_drawers` ([param_card.rs:1401](../crates/manifold-ui/src/panels/param_card.rs#L1401))
  bubbles collapse, spawn-pop, delete-fade, drawer-height AND tab-ink tweens into
  `drawer_anim_active`, which the app polls every frame
  ([app_render.rs:2940](../crates/manifold-app/src/app_render.rs#L2940)) → `needs_rebuild` →
  `invalidate_all()` ([:2842](../crates/manifold-app/src/app_render.rs#L2842)) → whole-atlas
  clear + full self-clearing renders. The trigger-drawer's unclipped ~120px overflow therefore
  never meets `LoadOp::Load` at all — the guard doesn't even need to catch it.
- **Bounds-stable-but-paints-outside edits don't exist in the card path.** Searched: the
  chevron's `Affine2::rotate` pivots about its own small rect (contained); slider fill/thumb/
  value-flash writes are contained under the card's opaque frame, which the incremental path
  always redraws first (`dirty_only=false`,
  [ui_cache_manager.rs:228](../crates/manifold-renderer/src/ui_cache_manager.rs#L228)).
- **The scroll-clip hole is already patched.** `traverse_flat_range` pre-pushes ancestor
  `CLIPS_CHILDREN` bounds for mid-tree ranges
  ([tree.rs:737-756](../crates/manifold-ui/src/tree.rs#L737)) — an incremental card repaint
  IS clipped by the scroll viewport. And the inspector's first node is a genuine full-rect
  opaque background ([inspector.rs:1892](../crates/manifold-ui/src/panels/inspector.rs#L1892)),
  so every full render self-clears the margins. The proposed fix direction ("repaint the
  background before dirty sub-regions") is actively WRONG: the background would overpaint the
  tab strip/chrome, which no sub-region would then redraw.

_2. The real hole (different from the hypothesis): out-of-sub-region dirt is silently
dropped._ The incremental path
([ui_cache_manager.rs:212-238](../crates/manifold-renderer/src/ui_cache_manager.rs#L212))
fires when ANY sub-region is dirty and repaints ONLY dirty sub-regions — it never checks for
dirt in the panel range that belongs to NO sub-region (tab strip, cog/Collapse controls,
scrollbar, all built directly in `build_in_rect`). `rendered_ranges` clears only the card
ranges, and the end-of-frame blanket `tree.clear_dirty()`
([app_render.rs:4807](../crates/manifold-app/src/app_render.rs#L4807)) then wipes the
remaining flags — erasing the evidence, so the fallback-to-full-render ("dirty list empty
next frame") never fires. The comment at
[app_render.rs:3870](../crates/manifold-app/src/app_render.rs#L3870) ("Deferred panels keep
their dirty flags") is falsified by :4807. **Trigger:** an in-place chrome mutation
co-occurring with card dirt — guaranteed whenever any param is audio-modulated (per-frame
card dirt), e.g. hover/unhover a tab or the Collapse button while a modulated generator
plays → the un-hover repaint is dropped and the stale hover state persists until the next
rebuild. This produces stale chrome STATES in place (ghost highlights, stale scrollbar) —
real, but probably NOT the screenshot's fragments. **Fix shape (root):** the incremental
path must detect dirt in the complement of the sub-regions and fall back to the full panel
render, and dirty-flag clearing for panel ranges should be owned by the cache manager (the
blanket `clear_dirty` may only touch the overlay region). **Sequencing: land BUG-060's clip
container first** — it bounds what this cache can be wrong about; order rationale in the
BUG-060 entry. This half is Opus-grade (fast-path regression risk), not Sonnet-mechanical.

_3. The video-bleed fragment cannot be atlas staleness at all._ The atlas never contains
compositor pixels: composite order is clear-to-black → atlas blit (pass 2,
[app_render.rs:3972](../crates/manifold-app/src/app_render.rs#L3972)) → compositor video
into `layout.video_area()` (pass 3, [:4001](../crates/manifold-app/src/app_render.rs#L4001),
opaque, aspect-fit INSIDE the rect) → timeline passes (4) → overlays (5, drawn straight into
the offscreen, [:4587](../crates/manifold-app/src/app_render.rs#L4587)). An atlas failure
shows black/transparent, never video. **Resolved by Peter 2026-07-07: the "video patch" in
the screenshot is just the preview window — legitimately there, not a bug.** (The composite
reasoning above stands for any future genuine video-over-UI sighting: it would implicate a
post-atlas pass, the BUG-025 class, never this cache.) The 2026-07-04 "sections interleaved"
sighting should be re-examined against hole #2 + rebuild-while-scrolled rather than the card
cache. The footer-overlap symptom this investigation started from is now its own entry with
a grounded root cause: **BUG-060** (no inspector-level pixel clip + the trigger-row drawer's
missing reveal clip).

**Outcome (Opus, 2026-07-08) — hole #2 (out-of-sub-region dirt dropped) FIXED at the root.**
BUG-060's clip container confirmed on `main` (@ `27557d18`) before starting, per the sequencing
note. Two-part structural fix:

1. _Incremental path now falls back on out-of-sub-region dirt._ New
`UITree::has_dirty_outside_ranges(start, end, covered)`
([tree.rs](../crates/manifold-ui/src/tree.rs)) reports DIRTY nodes in the panel range that lie
in no sub-region. The cache manager's incremental branch gates on a new `incremental_path_safe`
helper (`extents_unchanged` AND `!has_dirty_outside_ranges`)
([ui_cache_manager.rs](../crates/manifold-renderer/src/ui_cache_manager.rs)) — chrome dirt
(tab strip, cog/Collapse, scrollbar) now forces the full, self-clearing panel render the same
frame, so the stale-chrome ghost never paints. No one-frame lag: the fallback is same-frame,
not deferred.

2. _Panel-range dirty-flag clearing moved to the cache manager; blanket clear narrowed to the
overlay region._ The incremental path now returns the FULL panel range (safe: it only fires
when there's no out-of-sub-region dirt), so `clear_dirty_range` over `rendered_ranges`
([app_render.rs:3871](../crates/manifold-app/src/app_render.rs#L3871)) owns all panel-range
clearing. The end-of-frame blanket `clear_dirty()` at :4807 became
`clear_dirty_range(overlay_region_start, count)` — it no longer erases out-of-sub-region panel
dirt before the fallback can fire. The now-false comment at :3870 ("Deferred panels keep their
dirty flags") was corrected.

_Fast-path safety (the CRITICAL constraint):_ traced the tree layout — the 7 panels contiguously
tile `[0, overlay_region_start)` with zero gaps (build order: transport→header→footer→inspector
are back-to-back; the two split/resize handles are the SplitHandles catch-all
`[inspector_end, scroll_panels_start)`; layer_headers then viewport run to `overlay_region_start`;
`node_range() == (first, first+count)`). So clearing all rendered panel ranges + the overlay
range clears every node, exactly as the old blanket clear did — `has_dirty_in_range(0, panel_end)`
still settles to false and the `offscreen_dirty` idle fast path
([app_render.rs:3915](../crates/manifold-app/src/app_render.rs#L3915)) stays reachable. Both the
old and new end-of-frame clears run only on slow-path (dirty) frames — the fast path returns at
:3961 before either — so there is no clearing-parity change on idle frames. Since every dirty
panel is always rendered (no deferral in `render_dirty_panels`), no panel-range dirt can survive
a slow frame. Fast-path preservation is by reasoning + the tiling verification above, NOT a live
trace (the app is an interactive GPU rig I can't idle-observe headlessly here; `render_ui_to_png`
bypasses the cache).

_Verification:_ new device-free unit tests at the cache-manager helper layer —
`out_of_subregion_dirt_forces_full_render` (chrome dirt rejects the incremental path while
`extents_unchanged` still passes, isolating out-of-region dirt as the sole cause) and
`incremental_used_when_only_card_dirt` (in-card dirt stays on the fast path). Gate:
`cargo test -p manifold-renderer -p manifold-app -p manifold-ui` (993 + 158/10/1/3 + 646 passed)
+ `cargo clippy -p manifold-renderer -p manifold-app -p manifold-ui --all-targets -D warnings`
(clean; only pre-existing manifold-media Obj-C deprecation warnings). Shipped on
`fix/bug-015-out-of-region-dirt`. **Note:** this closes the stale-chrome-STATE class (ghost hover,
stale scrollbar). The 2026-07-04 "sections interleaved" sighting (hole #2 + rebuild-while-scrolled)
is a separate open thread if it recurs.

### BUG-025 — Timeline layer/header scissoring: clip content bleeds across row bounds — MED (repro needed)
**Status:** FIXED (believed) — closed by Peter's call 2026-07-14: he attributes the sighting to the GPU-pressure/contention issue behind the timeline blue-flicker, since fixed (the UI-present vs content GPU contention work). Never reproduced across three headless attempts (2026-07-05, 2026-07-07 ×2 — the second with a genuinely-applied scroll). Reopen if seen on the rig.

**Symptom** — reported by Peter 2026-07-05 (screenshot in session transcript) as "layer and
header scissoring": in the arrangement view, the bottom layer's purple clip body renders far
beyond its row — a solid block filling the timeline from its row down to the window edge —
while the layer-header column at bottom-left shows the Plasma MIDI drawer (MIDI / CHANNEL /
DEVICE) overlapping into that region. Clip content and header-column content are not being
mutually clipped to their rows/panes.

**Root cause** — unknown. Suspect surface: the per-row scissor rect for clip bodies (last or
expanded row), the `track-header-invariant` / `single-source-y-layout` class, or a stale
subregion scissor (`subregion-scissor-invariant`). Likely same family as BUG-015 (inspector
sections at stale offsets) — both smell like Y-layout/scissor divergence after the recent
timeline waves.

**Repro** — not pinned; NOT reproduced headless (2026-07-05 Opus). Snapshotted the `states`
and `timeline` scenes (both carry a selected generator layer with an open MIDI/CHANNEL/DEVICE
drawer, the closest fixtures to Peter's screenshot) — both render correctly: every clip body is
scissored to its row, every header drawer stays in the left column, group nesting clips fine.
A scroll-down + re-snapshot on `timeline` also did not reproduce (and scroll may not be fully
wired in the headless tracks path). So the general scissoring path is sound; the bug is
state-specific. Triage narrows it to a config the fixtures don't hit — most likely the
*last* row being a selected generator whose clip fills the remaining viewport height, and/or a
live scroll offset. Pin it with either a targeted fixture (selected generator as the final
layer) or a running-app repro from Peter's project.

**Repro attempt 2026-07-07 (timeline-ux audit)** — the 07-05 note's "scroll may not be fully
wired in the headless tracks path" is now explained: `--scroll` was seeded AFTER the base
render (fixed this branch), so every prior "scrolled" base PNG was actually unscrolled. With
scroll genuinely applied (via the interact after-render), headers + lanes offset together and
clip bodies stay scissored to their rows — still not reproduced. The state-specific triage
above stands.

**Fix shape** — TBD after repro. If it's the invariant class (likely, given BUG-015 is the same
family), fix at the single Y-layout source, not per-widget patches.

### BUG-151 (graph-editor-node-browser-container-fill-not-drawn) — the graph editor's node-spawn browser renders its cell rows but not the popup container fill/scrim, so the graph and inspector show through between the cells — MED (authoring surface looks broken; main-window instance of the same component is fine)
**Status:** FIXED — `docs/EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1, completed 2026-07-14 in two passes: the first pass (`9e3d710e`) landed D1 (the shared `tree_passes.rs::render_tree_overlay_passes`) but hit a genuine escalation on D2, logged below and superseded by this fix. Second pass (this commit) closed the gap D1 exposed: `UIRoot::overlay_draw`/`overlay_region_start` are populated only inside `UIRoot::build_overlays()`, which was reachable only from `UIRoot::build()` — a MAIN-window-only method the editor's `Workspace::ui_root` never called (it clears + lays out the entire main-window panel set, which would stomp the editor's per-frame tree). Fix: a new `pub(crate) fn build_overlays_for_screen(&mut self, w, h)` wrapper on `UIRoot` (`ui_root.rs`) sets `screen_width`/`screen_height` (the editor's `UIRoot` never gets `resize()` either, so this was also load-bearing) then calls the existing `build_overlays()` unchanged — safe standalone since it only reads screen size and the live open-overlay set. The editor's per-frame render (`app_render.rs`'s `present_graph_editor_window`) now calls this wrapper in place of the old hand-rolled `begin_region`/`browser_popup.build`/`end_region` block that bypassed the overlay system entirely; `editor_frame.rs`'s `composite_editor_frame` now narrows its base tree render to `[0, ui_root.overlay_region_start)` (D2, now meaningful) so the node browser renders ONLY through the shared tree-overlay pass, region-aware at OVERLAY depth — identically to the main window. Verified with a headless PNG (`ui-snap editor --open-picker`, saved at `docs/landings/BUG-151_editor_after_open_picker.png`): opaque popup container with search bar + full node grid over the graph, not bare cells.

**Root cause, corrected** — the original hunt (below) suspected a missing modal-dim-background scrim as a candidate explanation. **That hypothesis is wrong, confirmed by reading `BrowserPopupPanel::modality()`**: it returns `Modality::Modal { dim_background: false }` specifically because the popup builds its OWN full-screen backdrop node inside `popup_shell::build` — the driver deliberately does not add a second scrim for this overlay. There was never a missing scrim. The actual root cause is exactly what P1's first pass found and escalated: the editor's popup nodes were never registered as an overlay at all (`overlay_draw` was permanently empty for the editor), so the shared tree-overlay pass (already landed by D1) had nothing to draw — cells painted because the flat root-scan swept them up at CONTENT depth, but the popup's own backdrop/container lived inside the SAME un-recorded region and rendered exactly the same way, i.e. it should also have appeared under the old flat scan; the visually "missing" fill was actually cells-over-graph with no z-order enforcement between the popup's internal draw order and the rest of the flat-scanned tree, not a missing node. Fixed at the root by making the editor register overlays through the same driver the main window uses, not by adding a scrim.

**Symptom** — opening the node browser inside the graph editor shows floating search bar + cell rows with the graph canvas and the inspector panel bleeding through between and behind them. The SAME component opened from the main window (+ Add Effect) draws correctly: opaque `MODAL_BG` well, scrim, border.

**Design-doc correction** — `EDITOR_WINDOW_UNIFICATION_DESIGN.md` §1's audit (row 6 / line ~9) asserted "the editor's `build()` populates `overlay_draw` exactly like the main window's" — this was false (only the main window ever called `UIRoot::build()`; the editor built its tree by hand each frame and never recorded overlays). Corrected in the doc itself as part of this fix.

### BUG-152 (ui-snapshot-render-graph-node-textures-arc-migration-miss) — `ui_snapshot/render.rs`'s `MetalBackend::new` call was missed by the BUG-054 `Arc<GpuDevice>` migration, breaking the `ui-snapshot` feature build — MED (build-breaking for the whole feature; zero default-sweep blast radius)
**Status:** FIXED (in the `feat/editor-window-unification` P1 diff, uncommitted at session end pending the BUG-151 escalation) — found 2026-07-14 during `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1, trying to run the `ui-snapshot`-feature test gate (`bug097_…`, `editor_window_harness`) that phase's brief requires. Unrelated to that design but small/mechanical/isolated to the harness, so fixed inline rather than just logged, to unblock verifying the actually-in-scope P1 work. **Fixed:** the two `let device = GpuDevice::new();` sites in `render_graph_editor_to_png`/`run_graph_preset`'s render fn that flow into `render_graph_node_textures` now wrap in `std::sync::Arc::new(...)`; `render_graph_node_textures` takes `&std::sync::Arc<GpuDevice>` and calls `std::sync::Arc::clone(device)` at the `MetalBackend::new` call site. Verified: `cargo check -p manifold-app --features ui-snapshot --tests` compiles clean; `cargo test -p manifold-app --features ui-snapshot --bin manifold` — 177 passed, 0 failed, 2 ignored.

**Symptom** — `cargo check -p manifold-app --features ui-snapshot` (with or without `--tests`) fails: `crates/manifold-app/src/ui_snapshot/render.rs:736`, `MetalBackend::new(device, GW, GH, GFMT)` — expected `Arc<GpuDevice>`, found `&GpuDevice`. Confirmed present on unmodified `origin/main` via `git stash` (not caused by the P1 diff). Blocks the entire `ui-snap` CLI (`cargo run --features ui-snapshot -- ui-snap ...`) and every `#[cfg(test)]` module gated on that feature, including `ui_snapshot::mod::overlay_fidelity_proof::bug097_render_sub_region_draws_root_excluding_overlay_that_render_tree_range_blanks` and `ui_snapshot::mod::editor_window_harness::node_the_fixture_places_renders_at_its_declared_screen_rect` — both load-bearing regression proofs that currently cannot run at all under `cargo nextest`/`cargo test` (they aren't `#[ignore]`d; the whole binary just fails to compile with the feature on).

**Root cause** — `d447ec8d` (BUG-054, "renderer-device-ptr-dangles — Arc<GpuDevice> replaces cached raw pointers") changed `MetalBackend::new`'s signature from `&GpuDevice` to `Arc<GpuDevice>` and updated every call site in the workspace (`rg 'MetalBackend::new\('` shows ~60 hits, all using `device.arc()` in tests or `Arc::clone(device)` in production code) except this one — the sole caller reachable only behind the `ui-snapshot` cargo feature, which the default `nextest`/`clippy` sweeps never build, so the migration's own gate never saw it.

**Fix shape** — `render_graph_node_textures(device: &GpuDevice, ...)` (`render.rs:673`) needs an owned/cloneable `Arc<GpuDevice>` to hand `MetalBackend::new`. Its sole caller, `render_graph_editor_to_png`, constructs `let device = GpuDevice::new();` (`render.rs:336`) and passes `&device` to several other calls (`UIRenderer::new`, `RenderTarget::new`, `composite_editor_frame`, this function) that only need `&GpuDevice` and would keep compiling via `&Arc<GpuDevice>`'s `Deref` coercion — so the minimal fix is likely: change that one `let device = GpuDevice::new();` to `let device = std::sync::Arc::new(GpuDevice::new());`, thread `Arc<GpuDevice>`/`&Arc<GpuDevice>` through `render_graph_node_textures`'s signature, and call `Arc::clone(device)` at line 736. Verify no OTHER caller of `render_graph_node_textures` exists with a differently-shaped `device` before committing to that shape (`rg` it — only one call site was seen at P1 time, `render.rs:420`, but re-check at fix time). Small, mechanical, but touches `manifold-app`'s ui-snapshot harness — do it as its own dedicated pass with the `ui-snapshot`-feature test gate actually green afterward as proof, not folded into an unrelated landing.

### BUG-150 (mute-chip-press-motion-teleports-hit-bounds-after-scroll) — the mute chip's press animation re-applies a stale build-time Y after the scroll fast-path moves rows, teleporting its draw + hit bounds off the row — HIGH (perform-surface control fails first click)
**Status:** FIXED @ `804ea043` — `tick_mute_motion`'s bounds write deleted, colour tween kept; `mute_base_y` and the now-orphaned `ChipMotion::press_offset_y` removed. Root-level rule applied class-wide (Peter, 2026-07-14): animations never move hit geometry, colour-only. Class audit found no other violation — `inspector.rs` card-drag ghost/indicator are non-interactive (`add_panel`, no `INTERACTIVE` flag); `param_card.rs`'s badge reposition and target-bar drag both re-derive from live layout each call, not a cached absolute; `interaction_overlay` lift/ghost offsets apply only to a per-frame draw scratch (`clip_rect_scratch`) that `hit_test_clip` never reads (hit-testing goes through `beat_to_pixel`, an independent path); `param_card`'s drawer-height tween is a genuine layout reveal — the app forces a full rebuild every animating frame, so downstream row positions are always freshly recomputed, never cached. Solo has no motion tick at all (no `solo_motion`, `solo_base_y`, or `tick_solo_motion` anywhere in `layer_header.rs`) — confirmed by reading `Self::new()` and `update()` in full: it never had this defect.

**Symptom** — clicking Mute (Peter reports Solo too) on the layer headers sometimes toggles in one click, sometimes requires clicking twice — the first click selecting the layer instead. Separately observed: the "M" chip visibly drifts off its row during a scroll. Show-stopper class: a mute that needs two clicks on stage.

**Root cause** — `tick_mute_motion` (`crates/manifold-ui/src/panels/layer_header.rs:1062`) writes the chip's bounds every animated frame as `mute_base_y[i] + press_offset_y`, where `mute_base_y` is captured ONLY at build time (`layer_header.rs:2373`). The scroll fast-path `try_update_vertical_scroll` (`layer_header.rs:2193`) is a set-only frame: it offsets every row node's bounds but never updates `mute_base_y`. First hover after a scroll starts the press/hover tween, which re-applies the stale Y — snapping the chip's bounds (shared by renderer and hit test) back by the full scroll delta. The click aimed at the visible M hits the row background → `LayerClicked` → selection → structural rebuild recaptures `mute_base_y` → next click works. Trace-confirmed (MANIFOLD_INPUT_TRACE probes, branch `probe/first-click-instrumentation`): consecutive clicks at (22,689) resolved to an unnamed row node firing `LayerClicked`, then to `layer_header.mute` firing `ToggleMute`. Solo has no motion tick — its reported involvement is unconfirmed; plausibly the displaced mute chip overlapping the solo position, or observation blur. Verify during the fix.

**Fix shape** — Peter's decision (2026-07-14), root-level for the whole class: **animations never move hit geometry; press/hover feedback is colour-only.** Delete the 1px `press_offset_y` bounds write from `tick_mute_motion` (keep the colour tween), drop `mute_base_y` entirely, and audit every other animation-driven `set_bounds` against the same rule — known sites: inspector drawer (`inspector.rs:1603/1637`), param_card (`param_card.rs:2907-2915/3906`), interaction_overlay lift/ghost (non-interactive drag visuals, likely exempt), browser/settings popup enter anims (being deleted anyway by the popup pass). Container reveals that genuinely relayout must re-derive positions from current layout per frame, never from a cached absolute.

### BUG-114 (draw-family-blocked-on-array-into-texture-codegen-read-path) — `draw_*` atoms pass the codegen-mandate scope test but the compiler can't express them — LOW (tracked codegen gap)
**Status:** FIXED — `docs/FUSION_SOTA_DESIGN.md` P4a+P4b+P5. P4a (`ae9ab74c`) built the
`InputAccess::BufferIndex` mechanism (classify variant, region-grow rule, standalone+fused codegen
struct synthesis from a port's `Channels[…]` layout, `buf_<port>` binding) and proved it on
`node.draw_dots`. P5 (`1b013b0e`) lifted the Vec3/Vec4/Color param gate the six atoms' `color`
param independently tripped. P4b converts the remaining five `draw_*` atoms
(`draw_markers`/`draw_ticks`/`draw_gauge`/`draw_scanlines`/`draw_connections`) + `blob_overlay` per
the ADDING_PRIMITIVES recipe (`wgsl_body` fragment + `fusion_kind`/`input_access` + generated-vs-hand
parity oracle), removing every `boundary_reason: Blocked`. `draw_connections` additionally proves
the BufferIndex mechanism generalizes to TWO tagged array inputs on one atom (`detections` +
`edges`) — P4a only exercised one. `draw_scanlines` needed no BufferIndex tag at all (no array
input) — it was purely gated by the Color param P5 lifted. Measured on `BlobTracking.json` (the
real HUD preset all six/seven atoms and their overlay chain live in): `graph-tool fusion` — before
18 nodes / 0 regions / 18 estimated dispatches; after 18 nodes / **1 region (6 members: both
draw_markers instances + draw_dots + draw_gauge + draw_ticks + draw_connections) / 13 estimated
dispatches**. `draw_scanlines` stays isolated in this preset (topologically separated from the HUD
chain by two `value_overlay` draw-call boundaries, not a param/array gap) — a genuine, expected
non-fusion, not a regression. `docs/node_catalog.json`/`NODE_CATALOG.md` and
`docs/fusion_census.md` regenerated (buffer-index-shaped family: 22→16 refusals, 12→10
dispatches-saved-if-lifted — the six/seven converted atoms leaving the bucket). Logged 2026-07-11
while sharpening the codegen-mandate scope test.

**Symptom** — the six `draw_*` atoms (draw_dots/markers/ticks/gauge/scanlines/connections) remain
plain-WGSL fusion boundaries despite being per-element in shape: each dispatches one thread per
OUTPUT PIXEL (`[w/16, h/16]`, e.g. draw_dots.rs ~161) and indexes a marks `Array` (blob
detections) inside the body — a gather, not a scatter. An overlay chain costs one dispatch per
atom where a fused run would cost ~1.

**Root cause** — a codegen capability gap, not an atom defect: texture-domain codegen has no
read-path for an input storage `Array`. Classify cut rule 9 (FREEZE_COMPILER_MAP §4) makes any
wired Array input on a texture atom a Boundary, and the buffer-region path requires no texture
output, so the shape fits neither. `freeze/classify.rs` names the needed kind — `BufferIndex`,
"read element i from a storage buffer" — as planned-but-not-built (additive: one codegen
read-path + one region-grow rule, per its own comments).

**Fix shape** — build the `BufferIndex` read-path for texture-domain bodies, then convert the six
atoms per the ADDING_PRIMITIVES recipe (wgsl_body + markers + `standalone_for_spec` + parity
oracle). Per the mandate's scope test #5 these are BLOCKED, not exempt — the debt lives in the
compiler. Severity LOW: each atom sits in exactly 1 shipped preset (overlay/HUD vocabulary), so
the unfused cost only bites in stacked per-pixel overlay chains.

### BUG-146 (render-scene-atom-pipelines-never-prewarmed) — a scene layer's first frame pays every atom's lazy codegen-pipeline compile (node.cube_mesh confirmed; likely every `primitive!` atom no bundled preset happens to exercise structurally) — LOW-MED (first-frame stall, not steady-state)
**Status:** FIXED (fusion-sweep worktree, this session) — option (b) from the original fix shape: a registry-wide "every atom prewarms its own codegen pipeline once, unconditionally" sweep, structural rather than atom-by-atom. Found 2026-07-13 as the residual left over after BUG-145's shaft/shadow prewarm fix, during VOLUMETRIC_LIGHT_DESIGN P3's `MANIFOLD_RENDER_TRACE` content-thread perf gate.

**Symptom** — with BUG-145's fix applied AND a control scene where every light has `cast_shadows: false` and `node.atmosphere`'s `shaft_intensity: 0` (so neither the shadow pass nor any shaft pipeline ever runs), the SAME two-pillar scene's frame 0 still measured ~41.5ms on the content thread — over the 20ms budget, with no shafts or shadow-casting lights anywhere in the graph.

**Root cause** — `GeneratorRegistry::prewarm_all` (`crates/manifold-renderer/src/generators/registry.rs`) only (a) builds every BUNDLED preset's graph *structure* via `PresetRuntime::from_json_str_with_device`, which never calls any node's `run()` (the comment there says so explicitly), and (b) as of BUG-037, explicitly prewarms `RenderScene::prewarm_pipelines` (now also covering BUG-145's shadow/shaft pipelines) + `GltfTextureSource::prewarm_pipeline`. Any atom whose GPU pipeline is compiled lazily inside its OWN `run()` (the `primitive!` codegen path's `self.pipeline.get_or_insert_with(...)`, e.g. `node.cube_mesh`'s `GenerateCubeMesh::run` at `generate_cube_mesh.rs:97`) is never touched by either mechanism unless some bundled preset's *rendered* first frame happens to hit it — and prewarm never renders a frame. `node.cube_mesh` is the confirmed example named in the original diagnosis; investigating it during the fix found neither DigitalPlants.json nor NestedCubes.json actually wires `node.cube_mesh` in yet (it's a decomposition building block, not currently reachable via any shipped preset's structure) — the ~41.5ms residual the symptom above measured is a SEPARATE scene hitting the same class of gap via other codegen-path atoms in that scene's graph, which is exactly why the fix generalizes rather than special-casing cube_mesh.

**Fix (landed this session)** — option (b): `prewarm_all_atom_codegen_pipelines` (`crates/manifold-renderer/src/generators/registry.rs`, called from `prewarm_all`) walks every `type_id` in `PrimitiveRegistry::with_builtin().known_type_ids()` (the same enumeration `freeze/classify.rs`'s meta-tests use), constructs a fresh instance, and compiles its standalone kernel via a new `codegen::standalone_for_node` (`node_graph/freeze/codegen.rs`) — a dynamic (type-erased) mirror of `standalone_for_spec::<Self>()`. This was possible with NO new trait method: every const `standalone_for_spec` needs (`WGSL_BODY`, `INPUTS`, `OUTPUTS`, `PARAMS`, `INPUT_ACCESS`, `DERIVED_UNIFORMS`, `WGSL_INCLUDES`, `ATOMIC_OUTPUTS`, `FUSION_KIND`, `STENCIL_FETCH`) was already exposed as a same-shaped `&dyn EffectNode` method by the existing blanket `impl<P: Primitive> EffectNode for P` (`primitive.rs`) — the trait already carried everything codegen needs, just behind dynamic dispatch instead of a compile-time type parameter. O(atom count) — 144 atoms — not O(bundled presets × render cost), and needs no GPU inputs/fixtures (`standalone_for_spec` only needs WGSL text + a `PrimitiveSpec`'s consts, never bound resources), so the "render one throwaway frame per preset" alternative was correctly avoided. Atoms with no `wgsl_body` (hand-written pipelines, `wgsl_compute`, `draw_*`/BUG-114) return `CodegenError::NoBody` and are skipped — nothing to prewarm there generically.

**Measured** (fresh, uncached `GpuDevice`, this session — direct pipeline-compile timing, not an end-to-end scene, since no shipped preset currently reaches `node.cube_mesh` to drive a MANIFOLD_RENDER_TRACE repro): `node.cube_mesh` alone compiles cold in ~12-15ms vs ~0.02-0.04ms once `prewarm_all_atom_codegen_pipelines` has run; the worst case of touching every one of the 144 codegen-path atoms cold in one frame (the shape of the original BUG-037/145/146 diagnosis) sums to ~1.0-1.1s cold vs ~1-2ms prewarmed — both comfortably under the 20ms/frame budget after the fix, both far over it before.

**Residual, not silently dropped** — `node.variable_blur` (`gaussian_blur_variable_width.rs`) is the sole atom declaring `wgsl_specialization` (`QUALITY_LEVEL`/`WEIGHTING_MODE` free identifiers in its generated text, resolved only by its own `run()` via `device.create_specialized_compute_pipeline` with live param values). Plain `standalone_for_node` + `create_compute_pipeline` fails a real naga parse on it (confirmed: `WGSL parse error: no definition in scope for identifier: WEIGHTING_MODE`) — the substitution values and their string encoding are genuinely bespoke per atom, not derivable from `PrimitiveSpec`'s const data the way everything else in this sweep is. Detected dynamically via `EffectNode::wgsl_specialization()` (non-empty) and skipped; stays a lazy first-use compile (up to 6 variants, `quality` × `weighting_mode`) same as before this fix. If a future atom adopts `wgsl_specialization`, it lands in this same skip bucket automatically.

**Test** — `generators::registry::gpu_tests::prewarm_populates_the_shared_cache_for_representative_converted_atoms` (`crates/manifold-renderer/src/generators/registry.rs`), scoped to one atom from each of this session's three conversion waves (`node.grid_mesh`, `node.shininess`, `node.rotate_coordinates`), same before/after + idempotent + cache-hit shape as the sibling BUG-037 tests. Written order-independent per BUG-144's documented cross-test-ordering hazard on the same shared, process-global test device.

### BUG-141 (import-graph-fused-region-linearize-depth-parse-fail) — glb import's fused region fails WGSL parse (`no definition in scope for identifier: linearize_depth`), card silently renders unfused — LOW-MED (perf, not visual)
**Status:** FIXED this session (fusion-sweep mechanical-sweep phase 1 worktree; lands with its commit) — same mechanism and fix as BUG-135 below: `generate_fused` (`freeze/codegen.rs`, the texture-domain multi-atom fusion path) now collects and prepends each region member's `node_includes` (deduped), mirroring the block `generate_fused_buffer` already had. Proven end-to-end by a new proof test, `coc_from_depth_fuses_with_pointwise_neighbor_and_matches_unfused` (`freeze/proof.rs`) — builds the real glb-import-shaped region (`node.coc_from_depth`, a `linearize_depth`-calling camera-derived atom, fused with a pointwise `node.invert`), asserts `fuse_canonical_def` no longer falls back to `None`, the fused kernel contains `fn linearize_depth`, and fused vs unfused render match within the out-of-loop tolerance. Verified the test reproduces the pre-fix symptom exactly (`fuse_canonical_def` panics with the fix reverted) before restoring the fix.

**Symptom** — Loading a project with an imported glb layer logs `[freeze] fused region 0 failed to parse: ParseError { message: "no definition in scope for identifier: linearize_depth" ... }`. The freeze install falls back to unfused execution for that card, so the import graph's per-element tail (SSAO chain / mix) pays N dispatches instead of a fused one. Output is correct — the fallback works as designed — but the fusion win is silently lost on exactly the heavy-scene cards that need it.

**Root cause** — confirmed: a fused region includes a node whose `wgsl_body` calls the shared `linearize_depth` helper (`depth_common.wgsl` — `node.coc_from_depth` and/or `node.ssao_gtao`, both declare `wgsl_includes: [DEPTH_COMMON]`) but `generate_fused`'s texture path never emitted `node.node_includes` into the generated kernel — see BUG-135's root-cause writeup below, same code.

**Fix shape (applied)** — see BUG-135.

### BUG-149 (glb-import-fog-slider-per-world-unit-cliff) — the importer's Fog Density slider maps 1:1 onto a per-world-unit density, so on real imports it's a cliff: light fog flattens the mesh grey and god rays blow out the frame — MED-HIGH
**Status:** FIXED @ `ee16c3b5` — found 2026-07-14, Peter live on `SceneLadders.manifold` (apricot-blossom glb, one session after `59010e84` added the sliders); fixed same session. **Fixed:** the card binding now scales the slider by `3.0 / framing_distance` (optical depth at the subject: 1.0 ≈ 95% fogged, 0.1 ≈ 26% haze), pinned by a test assertion against the `cam_dist` card default. Applies to NEW imports only — an already-imported card (e.g. the SceneLadders apricot) keeps its baked `scale: 1.0` binding until re-imported. Visual confirm on the rig owed by Peter.

**Symptom** — With the glb card's Fog Density at just 0.13, the whole model renders as a flat fog-grey silhouette (screenshot confirmed); adding God Rays 0.21 on top blows out the entire frame, empty sky included. Fog and shafts each work as designed — the control range is what's broken.

**Root cause** — Unit mismatch, in the importer, not the atmosphere node. `node.atmosphere`'s `fog_density` is per-world-unit (`1 − exp(−density · distance)` in `render_scene.wgsl`; the node's own composition notes call ~0.05 "a light haze over tens of units"). `gltf_import.rs`'s card wiring (`card_param("fog_density", …, 0.0, 1.0, …)` → atmosphere `fog_density`, scale 1.0) passes the 0–1 slider straight through, ignoring scene scale — even though the importer auto-frames the camera from mesh bounds and therefore *knows* the scale. On the apricot fixture (framing distance 27.87 units), slider 0.13 ⇒ optical depth ≈ 3.6 ⇒ 1 − e^−3.6 ≈ 97% fog: every fragment is ~pure fog color regardless of its depth, hence "flat grey mesh" (background stays black since fog only applies to geometry). The shaft march then accumulates in-scattering ∝ `fog_density × sun color × intensity` per step along every camera ray (`shaft_march.wgsl`), so with an effectively opaque air column and Sun Intensity 3.5, shaft slider 0.21 makes the whole volume glow — full-frame blowout. Usable Fog Density on this model is roughly 0.00–0.04; on a differently-scaled GLB it'd be some other unknowable sliver.

**Fix shape** — Normalize the fog slider by the importer's own framing scale: map slider → optical depth at the subject, i.e. `fog_density = slider_value / framing_distance` (or bounds radius), via the card mapping's scale factor at import time. Slider 0.5 then means "about half fogged at the subject" on any model — a perceptual fader that behaves the same on a bonsai and a cathedral, which is what a live card control has to be. God Rays needs no separate fix; it inherits sane density once fog is scaled. Consider the same normalization for the bundled CinematicScene preset if it exposes raw `fog_density` at fixed camera framing. Related: BUG-118 (fog saturation on bounded subjects — same `1/density` decay-length physics, different surface).

### BUG-145 (shaft-pipelines-not-in-prewarm-first-frame-cold-start-spike) — FIXED (shaft/shadow half); residual first-frame cost from an UNRELATED, broader gap now tracked as BUG-146 — was LOW-MED
**Status:** FIXED (this session, VOLUMETRIC_LIGHT_DESIGN P3) for the shaft/shadow-pipeline half of the original finding; the residual first-frame cost this entry also measured is a SEPARATE, pre-existing bug — see BUG-146, do not re-attribute it here.

**Symptom (as first found)** — `MANIFOLD_RENDER_TRACE=1` on a headless `ContentThread` running a scene with 4 shadow-casting Point lights and `shaft_quality` High (32 steps) printed exactly one trace line: `frame=0 total=79.9ms | generators=79.6 ...`. No other frame across a 120-frame run printed (the trace only fires above 20ms), so frames 1–119 were already under budget — a one-time first-frame cost, not a sustained regression.

**Root cause (fixed half)** — `RenderScene::ensure_shadow_pass` and `RenderScene::ensure_shaft_pipelines` (`crates/manifold-renderer/src/node_graph/primitives/render_scene.rs`) lazily compile the shadow depth-only pipeline and the shaft march/downsample/composite compute pipelines via `Option`-cache-on-first-use. `RenderScene::prewarm_pipelines` — the fix BUG-037 added specifically to close this class of gap for the MATERIAL render pipelines — did not call either, so the first frame with a shadow-casting light or `shaft_intensity > 0` paid the full compile cost for 4 shader pipelines synchronously on that frame.

**Fix (landed this session)** — `RenderScene::prewarm_pipelines` now also compiles the shadow depth-only pipeline and all 3 shaft pipelines (asset-independent, same fixed-source shape as the BUG-037 fix it extends). Measured before/after on the SAME scratch scene: **79.9ms → 42.0ms** on frame 0. `GeneratorRegistry::prewarm_all` already calls `RenderScene::prewarm_pipelines`, so no registry-level wiring was needed — the fix is entirely inside the one function BUG-037 already established as the extension point.

**Residual — do NOT re-fix here** — a CONTROL run of the identical scene with every light's `cast_shadows` forced to `false` and `shaft_intensity` forced to `0` (so neither pipeline this fix touches ever runs) STILL measured ~41.5ms on frame 0. That remaining cost predates this design and is unrelated to shafts/shadows specifically — tracked as **BUG-146**, its own root cause and fix shape.

**Verification trail** — measured via `crates/manifold-app/src/vol_light_p3_perf_verify.rs` (a scratch `journey-proofs` harness, same pattern as `bug035_verify.rs`/`bug037_verify.rs`, deleted before this session's landing — not a committed regression guard) against a scratch bundled preset (`VolLightP3PerfScratch.json`, also deleted before landing). Re-run recipe if this needs re-measuring: rebuild an equivalent scene (2 occluders, 4 Point lights all `cast_shadows: true`, `node.atmosphere` with `shaft_quality` High) as a headless `ContentThread` generator layer, call `RenderScene::prewarm_pipelines` (mirroring real startup) before ticking, and drive `MANIFOLD_RENDER_TRACE=1 cargo test -p manifold-app --features journey-proofs <test_name> -- --nocapture`.

### BUG-127 (decode-worker-silent-drop-wedges-export-flush) — missing-handle decode jobs get no reply, `decode_pending` never clears, export flush blocks forever — MED-HIGH
**Status:** FIXED @ `450f01c4` — missing-handle arms now reply `DecodeResultStatus::Error`, plus a bounded-wait backstop in `flush_pending_decodes`. Found 2026-07-12 during the MEDIA_EXPORT_MAP.md mapping pass (full read of `manifold-media`). See that map §12 for the pipeline context of BUG-127..133.

**Symptom** — `decode_scheduler.rs` worker arms for `Prepare`/`Seek`/`DecodeNext` are `if let Some(handle) = active.get_mut(&clip_id) { ... }` with no else — a job for a clip the worker doesn't hold (its `Open` failed on a missing/corrupt file, since `start_clip` inserts the `ActiveVideoClip` eagerly and submits `Open`+`Prepare` back-to-back) is dropped with no result sent. App-side `decode_pending` (set true at submit) then never clears. `VideoRenderer::flush_pending_decodes` loops on `recv_results_blocking()` until no clip is pending — and `ContentThread::export_one_frame` calls it every export frame (`content_export.rs:458`). Failed-open clip + one `Seek` (a scrub, a loop restart, export warmup) = the export loop wedges on the content thread with no way out (there is also no cancel-export UI). In live playback the same state just leaves the clip permanently black (no flush, so no hang).

**Root cause (known)** — the worker protocol has no "job refused" reply; the `decode_pending` invariant assumes every submitted job produces exactly one result.

**Fix shape** — make the missing-handle arms send `DecodeResultStatus::Error("no handle for <job>")` so the pending flag always resolves; consider a bounded wait in `flush_pending_decodes` as a second fence. Small, contained in `decode_scheduler.rs`.

### BUG-128 (sdr-video-export-gamma-diverges-from-display-and-stills) — export bakes `pow(1/2.2)`, display/stills use true sRGB — MED (release deliverable fidelity)
**Status:** FIXED @ `63937590` (encode side `b692bb9a`) — shared `manifold_srgb_encode`/`manifold_srgb_decode` in `ColorTransferFunctions.h`, ported literally from `still_exporter.rs`'s tested constants, now used by both the SDR export shader and the decoder's linearization. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass. Full transfer-function table: MEDIA_EXPORT_MAP.md §7.

**Symptom** — the SDR encoder copy shader (`MetalEncoderPlugin.m`, `kCopyShaderSDR`) applies a plain 2.2 power curve to the linear compositor output; the live display applies the true piecewise sRGB function at scanout, and `still_exporter::linear_f16_rgba_to_srgb8` applies the true function too. The same frame therefore has three subtly different tones — video export darkest in the shadows (2.2 vs sRGB diverge most below ~0.04 linear). The decoder's inverse (`pow(2.2)` in `MetalVideoDecoderPlugin.m`) makes video→export→import self-consistent, but everything video diverges from what Peter sees on stage and in stills.

**Root cause (known)** — approximate gamma chosen in both native shaders; stills got the correct function later (still_exporter is documented and tested against sRGB) and the video shaders were never aligned.

**Fix shape** — one shared sRGB OETF/EOTF definition used by both native shaders (piecewise, matching `still_exporter.rs`); EDR handling stays hard-clip unless the still exporter's rolloff is wanted for parity. Behavior change is subtle-but-visible: worth one before/after export Peter can eyeball.

### BUG-129 (export-fractional-fps-silently-rounds) — integer CMTime timebase mistimes 23.976/29.97 exports — LOW-MED
**Status:** FIXED @ `8a814c23` — Option A (Peter's call): exact rational timebase. `fps_to_rational()` maps f32 fps to (num, den), passed across FFI instead of a rounded int; `AVAssetWriter`'s track `mediaTimeScale` also had to be set explicitly (it silently re-rounded to 600 otherwise) — verified end-to-end with a real export + `ffprobe` showing `r_frame_rate=30000/1001`. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — `MetalEncoder_CreateInternal` stores `fpsNum = (int)(fps + 0.5)` and stamps frames `CMTimeMake(frameIndex, fpsNum)`. `ProjectSettings::frame_rate` accepts any f32 ≥ 1.0, so a 29.97 project exports at a 30 fps timebase: frame count is computed from the true fps (`ExportSession`), but presentation times use the rounded one — duration shrinks ~0.1% and the ffmpeg-muxed audio (correct wall-clock) drifts ~60 ms/min against picture.

**Fix shape** — rational timebase (`CMTimeMake(frameIndex * 1001, 30000)`-style, derived from the f32), or clamp/validate `frame_rate` to integers at the settings layer and say so in the UI. Decide which; don't leave the silent mismatch.

### BUG-130 (export-audio-mux-fails-late-and-leaks-temp) — ffmpeg resolved only at finalize; temp video left behind on mux failure — MED
**Status:** FIXED @ `2c829eaf` — `ffmpeg_preflight` runs before frame 0 when audio is present and aborts immediately if ffmpeg is missing; on mux failure the temp video is renamed to `<output>.video-only-audio-mux-failed.mp4` (preserved, not deleted) with the failure reason surfaced. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — `run_export` calls `AudioMuxer::resolve_ffmpeg` only in the finalize block, after every frame has been encoded: a machine without ffmpeg renders a full multi-minute export and then fails at the last step. Separately, `ExportSession::finalize` deletes the `<output>.video_only.mp4` intermediate only after a *successful* mux — on `MuxError` the temp stays on disk next to the (absent) final file.

**Root cause (known)** — fail-fast check missing at export start; cleanup only on the happy path.

**Fix shape** — resolve ffmpeg before frame 0 when `config.has_audio()` and abort with a clear error; on mux failure either delete the temp or (better for a failed long render) rename it to the output path with a "video-only, mux failed: <reason>" report so the render isn't lost. The second half is a product call — flag to Peter.

### BUG-131 (video-decode-hardcodes-bt709-video-range) — one YCbCr matrix for every source — LOW-MED
**Status:** FIXED @ `87427ec0` — reads `kCVImageBufferYCbCrMatrixKey` (via `CVBufferCopyAttachment`) to select 601/709/2020 coefficients per-frame, falling back to the SD/HD height convention when untagged; verified against ffmpeg-generated 601/709/2020-tagged fixtures in-session (instrumented smoke test, then removed as a one-off manual check, not a permanent gate). Full-range-vs-video-range sources remain an unverified secondary, unchanged from the original bug note. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — the NV12→RGBA shader in `MetalVideoDecoderPlugin.m` applies BT.709 video-range constants unconditionally. BT.601-tagged SD sources (old footage, some phone/web encodes) and BT.2020 sources get a visible hue/saturation shift (601-vs-709 green/magenta skew). The CVPixelBuffer's colorimetry attachments (`kCVImageBufferYCbCrMatrixKey` etc.) are never read. Unverified secondary: full-range sources — the reader requests video-range NV12, so VideoToolbox probably normalizes; confirm with a full-range fixture before trusting it.

**Fix shape** — read the attachments on the decoded buffer and pick 601/709/2020 constants (function-constant variants or a matrix uniform). A 601-tagged fixture clip is the proof.

### BUG-132 (video-decode-nearest-neighbor-scaling) — unfiltered scale in the convert shader — LOW-MED visual
**Status:** FIXED @ `2b3e15e1` — manual 4-tap bilinear blend (`bilinear_read_r`/`bilinear_read_rg`) replaces truncated-coordinate nearest-neighbor sampling on Y and CbCr planes independently. Code-verified (shader compiles, exercised by decoder tests); pixel-level before/after on a resolution-mismatched clip is **Peter-owed** (no GPU readback harness for this path in-crate). Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — the convert shader does the FitInside scale with `texture.read()` at a truncated source coordinate: nearest-neighbor. Any resolution mismatch between source and canvas (1080p file on a 4K canvas, 4K file on a 1080p canvas) gets blocky upscaling or shimmering downscaling — on the live rig's portrait towers most video content is resolution-mismatched, so this is the common case, not the edge.

**Fix shape** — bilinear: sample Y and CbCr planes with a linear sampler (or manual 4-tap around the fractional coordinate). One shader change; eyeball a mismatched-resolution clip before/after.

### BUG-133 (video-extension-list-overpromises-webm-avi) — import gate accepts what the decoder can't open — LOW
**Status:** FIXED @ `5711f65c` — Peter's call: extension list stays broad; the existing probe-failure path (previously `log::warn!` + silent skip) now routes through the same `alerts::error` dialog used for other import/save/load failures, naming the file and the codec problem. Found 2026-07-12, MEDIA_EXPORT_MAP.md pass.

**Symptom** — `metadata::SUPPORTED_EXTENSIONS = [".mp4", ".mov", ".webm", ".avi"]`, but decode is AVFoundation, which has no VP8/VP9 and patchy AVI support: the import gate accepts the file, then `VideoDecoder_Open`/probe fails per-clip later, surfacing as a mystery-black clip instead of an import-time rejection.

**Fix shape** — either trim the list to `.mp4`/`.mov` (honest), or keep the broader list and make `import_video_clip`'s existing probe failure reject the import with a "codec not supported" message (better). One-file change either way.

### BUG-143 (macros-panel-ableton-trim-drag-outside-p7-inventory) — `MacrosPanel`'s Ableton-range trim-bar drag is a hand-rolled sentinel machine, outside every P7 fold — LOW
**Status:** FIXED @ `d5ab1ae7` (UI_WIDGET_UNIFICATION P8, 2026-07-13) — `dragging_ableton_trim: i32` + `dragging_ableton_trim_is_min: bool` folded onto `DragController<AbletonTrimDrag>` (struct payload); pinning test green before and after; `manifold-ui --lib` 759/759; negative gate (`rg 'dragging_ableton_trim' crates/manifold-ui/src`) zero hits.

**Symptom** — none. The gesture (dragging a macro's Ableton min/max range trim bars, `handle_press`/`handle_drag`/`handle_release` in `panels/macros_panel.rs`) worked correctly before and after. This was a drag-lifecycle-unification gap, not a behavior bug.

**Root cause** — `dragging_ableton_trim: i32` (−1 = idle sentinel) + `dragging_ableton_trim_is_min: bool` (`macros_panel.rs:70-71`) was exactly the pre-P7.1 `ParamDragState` shape — a discriminant-by-sentinel plus a parallel bool, the disease D8 exists to kill. It was distinct from `MacrosPanel`'s own per-macro VALUE sliders (`self.sliders`, already `SliderDragState`/`DragController`-backed), so P7's original design-pass audit never surfaced it. Found only by P7.6's own closing `rg -n 'dragging'` inventory.

**Fix shape (as landed)** — `struct AbletonTrimDrag { index: usize, is_min: bool }` (chosen over an enum: the call sites already carry index and a min/max bool as separate values, so the struct converts one-for-one with no match-arm rewrites), `DragController<AbletonTrimDrag>` replacing the two fields, `handle_press`/`handle_drag`/`handle_release` updated to `start`/`payload`/`release`.

### BUG-111 — In-place inner-param edits on a fused SEGMENT card never reach the live kernel — MED
**Status:** FIXED @ `d73b3e36` — `EffectSlot::card_prefix` (`c{i}.` for a segment member, `""` otherwise) threads through a new `BoundGraph::apply_inner_overrides_prefixed`, translating both the `node_map` lookup (surviving nodes) and the `fused_retarget` lookup (fused-away nodes) into the segment's prefixed namespace; the segment slot's `bound.fused_retarget` is now also populated from `SegmentView::retarget` at build time. New gpu-proofs test `fused_segment_inner_override_reaches_live_kernel` (`preset_runtime.rs`) proves it on a real 2-card fused ColorGrade segment — independently reconfirmed red (`over=0/65536`) on the pre-fix code path and green with the fix restored.

**Root cause** — the fused-segment build path (preset_runtime.rs ~977–1064) builds one `node_map`
from the concatenated segment def, whose node ids carry the `c{i}.` per-card prefix
(`freeze::segment::card_prefix`). The per-frame in-place override (`run` → `apply_inner_overrides`,
preset_runtime.rs ~1863) passes each card's OWN def (`fx.graph`), whose node ids are UNPREFIXED.
So `apply_inner_param_overrides` misses every node: a surviving node `foo` is `c{i}.foo` in the
map, and a fused-away node isn't there at all. The BUG-006 retarget fix doesn't help — the
segment's `EffectSlot.bound.fused_retarget` is left empty (the segment retarget map is prefix-keyed
too), and even a prefixed retarget wouldn't cover the surviving-node miss.

**Symptom** — a value/position edit to a card that is part of a fused segment (multi-card fusion)
never lands in place; the old value keeps rendering until an unrelated rebuild. Same silent-stale
class as BUG-006 but scoped to segments. Narrower than BUG-006 (needs a multi-card segment that
fused), hence MED. Stateless-only cards today (segment eligibility), so no state-loss compounding.

**Fix shape** — populate the segment slot's `bound.fused_retarget` from the segment view's
prefix-keyed retarget with the `c{i}.` prefix normalized to the per-card def's key space, AND
translate surviving-node overrides by prefixing the def node id before the `node_map` lookup (or
apply overrides against a per-card-prefixed view of the def). Pair the two so both surviving and
fused-away nodes resolve. A focused test mirroring `inner_override_routes_fused_away_node_through_retarget`
but over a 2-card segment would pin it.

### BUG-054 (renderer-device-ptr-dangles) — renderers cache a raw `*const GpuDevice` that only `ContentThread::run()` repoints — MED (latent; every new headless/embedded consumer of ContentThread hits it)
**Status:** FIXED @ `d447ec8d` — `Arc<GpuDevice>` (approved sharing, device is internally synchronized, no `Arc<Mutex<_>>` introduced) replaces the cached raw pointer end-to-end: `ContentPipeline` and the UI-thread `GpuContext` own it from construction; every renderer clones it. Beyond the three renderers named above, `MetalBackend` also cached the same raw pointer and needed the same migration — threaded through `PresetRuntime`'s constructors, `GeneratorRegistry`, preset-thumbnail/graph-tool/freeze-profile/render-generator-preset bins, and the freeze GPU-parity test harness. `ContentThread::run()`'s repoint block and `journey_proof.rs`'s `rebind_gpu_device_pointers` workaround deleted — the invariant they existed to paper over no longer exists. Negative gate: `rg '\*const GpuDevice' crates/` — zero code hits (one doc-comment mention narrating the fix). Full workspace nextest (3250/3250), full workspace clippy, and the full `gpu-proofs` suite (1488/1490 — the 2 failures are BUG-144, a pre-existing order-dependent flake, confirmed unrelated) all independently reverified by the orchestrator, not just the worker.

**Found 2026-07-07 by the OFFLINE_AUDIO_REACTIVE_EXPORT P3 harness (first code path ever to
drive `run_export` outside the app's thread spawn).** `GeneratorRenderer` / `VideoRenderer` /
`ImageRenderer` cache a raw device pointer set at construction
(`generator_renderer.rs:126,180`); it dangles as soon as the owning `ContentPipeline` moves.
The running app is safe only because `ContentThread::run()` repoints every renderer once,
after all moves are complete (`content_thread.rs:300-328`) — a load-bearing, undocumented
ordering invariant. Any new consumer (headless export/journey harness, future preview
contexts, tests) that constructs a `ContentThread` and calls methods without replicating that
exact repoint gets an ObjC nil-receiver panic or a straight segfault, as the P3 build did
twice before finding the correct point. **Workaround shipped:** `journey_proof.rs`
`rebind_gpu_device_pointers` runs after the struct reaches its final binding — correct but a
second copy of the invariant. **Fix shape (root):** remove the self-referential raw pointer —
either pass `&GpuDevice` per render call (renderers already receive per-call context), or
hold the device behind a stable heap indirection owned above the pipeline so moves can't
invalidate it. Blast radius: renderer call signatures; no behavior change. Until then, any
brief that constructs `ContentThread` outside `Application::resumed()` must name the repoint
step.

### BUG-123 (mesh-edges-capacity-vs-active-count) — node.mesh_edges emits edges for the full buffer capacity, not the loaded vertex count — LOW visual artifact, tracked v1 limitation
**Status:** FIXED @ `1b854d45` — added an optional `active_count` scalar input (port-shadow, mirrors `node.range`'s convention) that overrides the buffer-capacity-derived vertex count when wired; unwired graphs are unaffected. 5 new tests in `edges_from_mesh.rs`.

**Symptom** — When a `gltf_mesh_source` feeding `node.mesh_edges` has `max_capacity` larger than the asset's actual flat-vertex count, the zero-filled buffer tail produces degenerate edges that draw as a bright dot artifact at vertex 0's projected position in `draw_lines`.

**Root cause (known)** — Array wires carry buffer capacity but `mesh_edges` has no runtime active count; it derives edge count from `buffer.size / size_of::<MeshVertex>()`. Presets work around it by sizing `max_capacity` exactly to the asset (BlossomWire: 9210 for the confetti cut).

**Fix shape** — An optional `active_count` scalar input on `mesh_edges` mirroring `node.range`, or a mesh-wire active-count convention (curve/particle families already have one). Small, isolated.

### BUG-079 (missing-preset-fails-silently-no-onscreen-signal) — an unresolvable preset def degrades safely but with no on-screen signal — LOW
**Status:** FIXED @ `834fdaa6` — reuses the BUG-063 P3 `LoadReport`/"opened with repairs" toast mechanism: an unresolved preset template now adds to the same load-time report instead of only an `eprintln`, so it surfaces in the existing non-blocking toast. No new notification mechanism. Covered by `crates/manifold-io/tests/load_report.rs`.

Loading a project that references an unresolvable preset def (deleted, unregistered, or missing on this machine) degrades *safely but silently*: saved params are kept on a placeholder (keep-don't-drop, `effects.rs:940`) and the effect falls back to **source passthrough** (`preset_runtime.rs:808`) — but the ONLY signal is a console `eprintln`; nothing shows on screen. A performer sees the layer render without its effect (a missing *generator* layer likely renders empty — inferred, unconfirmed) with no visible reason. **Fix shape:** surface unresolvable presets in-app (a card/badge or a load-time notice).

### BUG-038 (ableton-log-spam) — AbletonBridge retries + WARN-spams every ~1.5s forever when Live isn't running — LOW (log hygiene)
**Status:** FIXED @ `06bfd879` — warns once on first OSC-send failure, downgrades repeats to DEBUG, logs a single INFO "reconnected" on the next success. Throttle decision is a pure `note_send_outcome` state machine, unit-tested.

**Symptom** — any session without Ableton running logs
`[AbletonBridge] OSC send failed for /live/song/get/num_tracks: Connection refused` at
WARN level every ~1.5s indefinitely (see any 2026-07-06 trace-run log).

**Fix shape** — warn once on first failure, then downgrade repeats to debug until a send
succeeds (state flip logs "reconnected" at info). Optionally back off the poll while
refused. `manifold-playback/src/ableton_bridge.rs`, small.

### BUG-086 (recording-audio-track-under-covers-duration-on-longer-takes) — the recorded audio track can silently fall short of the intended duration on longer takes, no counter, root cause unknown — MED
**Status:** FIXED

**FIXED 2026-07-13 (recording-sync lane).** Diagnosis protocol (per the brief): added
counters first, ran the paced 2-minute 1080p soak 3x. All 3 measured `audio_frames_dropped
= 0` while duration was full-length — falsifying the native backpressure gate as this bug's
cause (consistent with the 2026-07-11 observation below, now confirmed with a clean signal).
Checked the named suspects in order with ffprobe: before reaching AAC priming/fragment-flush
discriminators, instrumented `recording_soak.rs`'s OWN synthetic-audio pusher
(`push_realtime_audio_chunk`) by making `push_audio_chunk` return `ringbuf::Producer::push_slice`'s
actual accepted count instead of discarding it — and found the real defect immediately: the
bounded `HeapRb` (~5s capacity) the soak bin pushes into can transiently fill under unpaced/
encoder-stress timing bursts, and the OLD code advanced its `pushed_frames` bookkeeping by the
INTENDED push amount regardless of what the ring actually accepted, so an overflow was silently
discarded rather than retried next call — a harness-side loss, not a native-encoder one.
Root-fixed by tracking the real accepted count (self-heals: the next call's `to_push` naturally
includes whatever didn't fit). Verified directly: 3x paced 2-min 1080p soaks measured
`audio_duration_s` at 120.0087s / 120.0102s / 120.0115s (<0.01% off, intended 120.0s), two
paced 1-min soaks (720p/1080p) measured 60.0038s / 60.0102s, and the previously-reliable
unpaced/encoder-stress 2-min repro now measures 120.0007s — full coverage, no shortfall,
across three separate reruns. `LiveRecordingPlugin.m`'s `WriteAudioSamples` backpressure gate
was ALSO hardened while investigating (bounded spin-wait matching the video path, `LR_OK` never
returned on a drop — a real defect under the session's class rule, landed together with
BUG-085, though it turned out not to be this bug's cause). `docs/LIVE_RECORDING_PROOFS_DESIGN.md`
doesn't need a status change (P1/P2 already SHIPPED; this is a post-ship bug-fix pass, not a
phase).

Found 2026-07-10 building `LIVE_RECORDING_PROOFS` P2's `recording-soak` self-check gate
(`crates/manifold-recording/src/bin/recording_soak.rs`). Sequence: the soak bin's synthetic
audio was originally paced one chunk per video frame-loop iteration (media-time-locked); an
unpaced 4K/1080p run compresses many minutes of "media time" into a few seconds of wall time,
which triggered the native audio input's real-time backpressure gate
([`LiveRecordingPlugin.m:546-547`](../crates/manifold-recording/native/LiveRecordingPlugin.m#L546):
`if (!state->audioInput.isReadyForMoreMediaData) return LR_OK; // drop samples rather than
block`) and lost ~91% of the audio (10.8s decoded out of an intended 120.0s) — worse than
BUG-085's video path in one respect: this returns `LR_OK` (success) on drop and never logs
anything, so there isn't even a warning, let alone a counter. Root-fixed the soak's OWN pacing
(decoupled audio production from the video loop, paced to real wall-clock time instead, plus a
post-loop real-time catch-up phase — matches how production audio actually arrives, from a
real-time CoreAudio callback, never frame-coupled) which recovered the overwhelming majority of
the loss (10.8s → 118.4s of 120.0s). A **residual, VARIABLE shortfall remains and is
unexplained**: three repeated 2-minute 1920x1080 unpaced runs measured `audio_duration_s` at
116.0s, 118.4s, and 118.5s against an intended 120.0s (1.3%-3.3% short, run to run — not a
stable fixed percentage), while two independent 1-minute runs (1280x720 and 1920x1080) both
measured exactly 60.0s — so whatever's causing it is **duration-dependent, not
resolution-dependent**, onset is somewhere between 60s and 120s of continuous writing, and the
magnitude varies (possibly with system load/contention — not isolated). Ruled out: a "still
queued in the ring buffer at `stop()`" race — inserting a 500ms settle delay before calling
`session.stop()` changed the measured shortfall by <0.1s, so the loss is happening *during* the
run, not at shutdown. **Fix shape:** unknown without native-side instrumentation (out of P2's
scope — proof-harness/soak-authoring work, not native FFI investigation). Suspects: sustained
real-time backpressure that only manifests past some duration/data-volume threshold (disk I/O
contention as the fragmented MOV grows, fragment-flush cadence interacting with the audio
append queue, or AAC's own internal encoder buffering not being fully flushed by the periodic
drain before a threshold is crossed). Wire an `appendedFrameCount` counter for audio analogous
to BUG-085's video-side fix, or add NSLog-level visibility on the `LR_OK`-drop path at minimum,
so this stops being silent. Given the observed variance, the soak's own audio-coverage check
(`recording_soak.rs`, "Audio coverage sanity gate" comment) does NOT gate PASS/FAIL on a tight
tolerance — a made-up number would misrepresent confidence that doesn't exist yet — it gates
only a coarse 50% floor (catches a genuine collapse like the original 91%-loss defect) and
prints a non-gating stderr warning past 2% short, naming this bug. **Unknown whether this loss
scales worse over a full 20-minute take** — Peter's first full-scale soak run (P2's Deferred
item, per design §6 P2) will be the first real data point at show scale; if the shortfall grows
materially at that scale, this bug's severity should be revisited upward.

**Orchestrator disambiguation 2026-07-10 (Opus, at P2 landing):** ran the soak in `--realtime`
mode (submissions paced to wall clock — the true show proxy) for 2 minutes at 1920×1080 on an
idle machine: `audio 120.0s` exactly, full coverage, versus `audio 118.6s` on the same-size
unpaced run moments earlier. This is strong evidence the shortfall is an **unpaced-stress-mode
artifact**, not a show-path defect: unpaced video encodes at 100% duty and the synthetic-audio
catch-up floods the native audio input's real-time gate, which cannot happen in a real 60fps
show where audio arrives wall-clock-paced from CoreAudio (exactly what `--realtime` replicates).
Severity for the SHOW path is therefore LOW; the bug is real but lives in the soak's unpaced
audio-feed pacing under encoder saturation. **Still worth the silent-drop fix** (the `LR_OK`-on-drop
path with no counter/log is the actual defect worth removing, per BUG-085's sibling shape).
Peter's first full-scale 20-minute run remains the confirming data point, but the show-relevance
concern is now much reduced.

**Observation 2026-07-11 (Lane C, wave2 export/recording sweep):** the silent-drop fix named
above landed as an instrument — `LiveRecordingPlugin.m`'s `WriteAudioSamples` now counts and
NSLogs every sample-frame it drops on the `isReadyForMoreMediaData` backpressure gate (an atomic
`audioFramesDropped`, read live via `LiveRecorder_GetAudioFramesDropped` /
`LiveRecordingSession::audio_frames_dropped()`, surfaced end-to-end through `ContentState`
(`recording_dropped_audio_frames`) onto the layer-header Record button, and printed by
`recording_soak` next to its existing audio-coverage check). Ran a real unpaced 2-minute
1920×1080 soak (`recording-soak --width 1920 --height 1080 --fps 60 --minutes 2`, the same shape
as the original repro): `audio_frames_dropped = 0` while `audio_duration_s` still measured
118.8s against the intended 120.0s (1.2s / ~1.0% short — inside this run's own non-gating 2%
warning threshold, so no WARNING printed, but still the same class of shortfall this bug tracks).
**This is a real data point, not a fix**: the native backpressure gate reported zero drops on a
run that still fell short, so for THIS run the gate is ruled out as the cause — the shortfall is
happening somewhere the counter can't see (consistent with the standing suspects: AAC encoder
internal buffering, fragment-flush cadence, or disk I/O contention, none of which the backpressure
gate would catch). Only one run was captured this session (time-boxed); the counter is now in
place for whoever runs the confirming full-scale 20-minute soak to check whether it ever fires
at show scale.

### BUG-085 (recording-frames-recorded-overstates-async-append-drops) — `frames_recorded` can overstate the file's real packet count under sustained backpressure — MED accounting / LOW practical likelihood
**Status:** FIXED

**FIXED 2026-07-13 (recording-sync lane).** `frames_recorded` no longer accumulates from
`LiveRecorder_EncodeVideoFrame`'s synchronous `LR_OK` return — that return only proves the
frame was queued for the async `appendPixelBuffer:` call, not that it landed. Native: a new
`videoFramesAppendDropped` atomic counter (+ `LiveRecorder_GetVideoFramesAppendDropped`,
mirroring the existing audio counter) now counts every way the async append can fail
(backpressure, writer not Writing at append time, `appendPixelBuffer:` returning NO, or an
Objective-C exception) — all previously silent. Rust: `recording_thread::run` polls that
counter (before `Finalize`, which frees native state) and uses `LiveRecorder_Finalize`'s
return value — the native `videoFramesAppended` ground truth, read only after the append
queue is fully drained — for `frames_recorded`, instead of the untrustworthy synchronous
tally. `run()` now returns a `RecordingStats { frames_recorded, frames_sync_failed,
video_append_dropped }`; `LiveRecordingSession::stop()` sums every drop source into
`RecordingResult.frames_dropped` (session-level pool/channel drops + native sync failures +
native async-append drops), so `frames_recorded + frames_dropped` always equals frames
submitted, and no path reports success on a drop (the class rule this bug and BUG-086 share).
`pool_accounting_consistent`'s forced-backpressure test tightened from `pts.len() <=
frames_recorded` to exact equality plus `assert!(frames_dropped > 0)`; green across 3
consecutive runs.

Found 2026-07-10 building `LIVE_RECORDING_PROOFS` P1's `pool_accounting_consistent` test
(`crates/manifold-recording/tests/recording_proofs.rs`), during a bounded-retry-recovery variant
that deliberately holds pool slots un-released to simulate a slow encoder. `session.stop()`
reported `frames_recorded: 107`; the file the harness's independent ffprobe oracle actually
opened had 106 video packets. Root cause, in
[`LiveRecordingPlugin.m`](../crates/manifold-recording/native/LiveRecordingPlugin.m) around
line 490: `LiveRecorder_EncodeVideoFrame` returns `LR_OK` (line 519) as soon as the *synchronous*
GPU blit into the CVPixelBuffer finishes — but the actual `[adaptor appendPixelBuffer:...]` call
happens **later, asynchronously**, on `state->appendQueue` (`dispatch_async`, lines 490-516).
Inside that async block, if `videoIn.isReadyForMoreMediaData` is false at the moment it runs
(real VideoToolbox backpressure), the frame is silently dropped —
`NSLog(@"[LiveRecorder] VideoToolbox backpressure — dropped frame at %.3fs", ...)` — with **no
counter incremented anywhere Rust can see**. Rust's `frames_encoded` (→
`RecordingResult::frames_recorded`) only reflects the synchronous return value, so it can never
observe this drop. The container file itself stays completely valid (PTS strictly monotonic,
no corruption) — this is purely an accounting gap: a post-set "N frames recorded" readout could
overstate the truth by however many frames VideoToolbox silently dropped under backpressure.
**Fix shape:** wire `atomic_int* appendedCounter` (already tracked at line 489, incremented at
line 500 on real success) back out through the FFI — e.g. a `LiveRecorder_AppendedCount(handle)`
query at `stop()`/finalize time, or have `LiveRecorder_Finalize`'s return value report the true
appended count instead of (or alongside) the synchronous-call count — and have
`LiveRecordingSession::stop()` prefer it. **Practical severity is LOW**: this needs genuinely
sustained `isReadyForMoreMediaData == false` backpressure, which the harness's artificial
fence-holding produces on purpose but a real 60fps show submission rate is very unlikely to
sustain (VideoToolbox's ProRes proxy encode is comfortably faster than realtime at these
resolutions). No `#[ignore]`-able regression test yet — `pool_accounting_consistent`'s current
gate (`frames_recorded + frames_dropped == frames_submitted_total`, tracked entirely
Rust-side) is internally consistent and doesn't touch this gap; a future test would need to
assert `probe(file).pts.len() <= frames_recorded` under intentional backpressure instead.

### BUG-138 — `node.variable_blur` fixed tap count looks blocky at large CoC radius — FIXED 2026-07-13 (CINEMATIC_POST DoF)
**Status:** FIXED @ 8659c11a (2026-07-13, Sonnet 5, `dof-polish` worktree, branch `feat/dof-polish`)

**Root cause** — the Gaussian kernel is a fixed 9/17/25 taps (`QUALITY_LEVEL` specialization,
`gaussian_blur_variable_width_body.wgsl`), but tap *spacing* scales directly with the per-pixel
CoC radius (same `step_size` line as BUG-137). At a large blur radius — e.g. `CinematicScene`'s
`DoF Blur Radius` card at 64px — the fixed tap count spreads across a wide span with visible gaps
between the actual samples, so heavily out-of-focus areas render as discrete rings rather than a
smooth blur, instead of the graceful falloff a real lens produces.

**Fix shape** — P4 (`node.bokeh_gather`, already designed in `CINEMATIC_POST_DESIGN.md` D5, a
true 32-tap 2D disc gather rather than a sparse separable 9/17/25-tap kernel) will likely reduce
this substantially just by construction, though it hasn't been built or verified against this
specific symptom. It will NOT fix BUG-137's dilation/bleeding gap on its own — that's a separate
mechanism. If P4 alone doesn't resolve the blockiness, the fallback is scaling tap count with
radius rather than holding it fixed. **Demoted to secondary 2026-07-13:** P4 is escalated to the
DoF root fix (CINEMATIC_POST status amendment) and `CinematicScene` stops using the gaussian pair
once it lands — the tap-scaling fix then only matters for the still-user-wireable
`variable_blur` atom itself, at the dof-polish lane's tail.

**2026-07-13 update (P4 landed):** `CinematicScene` now runs `node.bokeh_gather` (true 32-tap 2D
disc gather, `crates/manifold-renderer/src/node_graph/primitives/bokeh_gather.rs`) in place of the
two `variable_blur` H/V nodes this bug names — the `variable_blur` atom itself is untouched and
still ships/wireable elsewhere, only the preset swap happened. Whether the blockiness this bug
describes is actually resolved is a look-pass question, not a gate question (the numeric gate
proves the atom matches its own committed D5 spec, not that it looks better than the old kernel)
— look-pass waived by Peter 2026-07-16 (verification-debt burn-down, same pass as BUG-137's); if the blockiness shows on a real scene it comes back as a fresh sighting.

**2026-07-13 fix (dof-polish lane tail, `node.variable_blur` atom itself):** Built the literal
fallback named above — tap count now scales with the per-pixel CoC radius instead of holding
fixed. `gaussian_blur_variable_width_body.wgsl` and its hand parity oracle
`gaussian_blur_variable_width.wgsl` both changed identically: each of the fixed 9/17/25 logical
taps now densifies into up to 4 evenly-spaced sub-samples filling the gap back toward the previous
tap (`vbw_subtap_count(step_size)`, weight split evenly across the sub-samples) once `step_size`
exceeds an 8px threshold; below that threshold — including the documented `max_radius = 6.0` DoF
parity setting (`step_size` ≤ 7px there) — the kernel is byte-identical to the original
single-sample-per-tap arithmetic, so the "matches legacy DoF blur byte-for-byte" claim in
`composition_notes` still holds. At the bug's own 64px repro (High quality), effective tap count
goes from the old fixed 25 to 97 (25 → 4× density, capped). **§2.5-equivalent audit finding worth
recording:** `node.gaussian_blur` (`separable_gaussian_body.wgsl`'s `sg_blur_linear`) already ships
an adaptive-radius, runtime-loop, analytically-weighted Gaussian on the same codegen path — a
different (fancier) shape than the literal "scale tap count with radius" fix asked for here; it
was surveyed and deliberately NOT reused wholesale because `variable_blur` carries per-tap CoC
weighting (`WEIGHTING_MODE`/`coc_weight`) that `sg_blur_linear` doesn't have, and re-deriving
per-tap Gaussian weights analytically would have been the "fancier invented algorithm" the task
explicitly said to avoid — the fixed-table + sub-sample-densification shape stays closest to the
bug's own literal fix note. Cost tradeoff stated explicitly (not hidden): worst-case per-pixel tap
count is capped at 4× the original (SUBTAP_CAP=4) — this reduces but does not fully eliminate
visible banding at extreme radii (65px spacing → ~16px worst-case gap); a full elimination would
need a disc-gather redesign at `variable_blur`'s own granularity, which is exactly what
`bokeh_gather` already is for `CinematicScene` and is out of scope for this atom-level fix. Low
tiers (Low/Medium quality) are unaffected at their own default radii — the fix only escalates cost
when `step_size` is genuinely large, never degrades a tier to cheapen another. Gate green: 6 new
+existing unit tests (`gaussian_blur_variable_width.rs` `tests` module, including 3 new BUG-138
numeric proofs), `generated_gaussian_blur_variable_width_matches_original` (I4 generated-vs-hand
parity) green at the new algorithm, `fused_variable_width_blur_matches_unfused` green, full
`manifold-renderer --features gpu-proofs` sweep green, `cargo nextest run -p manifold-renderer
--lib` 1153 passed, scoped clippy clean, `check-presets` 57/57 (unchanged param shape —
`DepthOfField.json`, the remaining user-facing preset using `node.variable_blur`, still loads with
its existing defaults). No PNG look-pass required this phase (numeric-gated atom, not wired into
any gated demo chain per the CINEMATIC_POST precedent) — `variable_blur` is no longer in
`CinematicScene`'s chain (moved to `bokeh_gather`), so there is no look-pass gate for this atom.

### BUG-137 — `node.variable_blur` has no CoC dilation; hard cutoff at depth discontinuities — MED (CINEMATIC_POST DoF)
**Status:** FIXED 2026-07-13; confirmation waived by Peter 2026-07-16 (verification-debt burn-down, VD-020-CINEMATIC — reopen as a fresh sighting if the seam shows live) — `node.coc_dilate` (fixed 3x3 neighborhood-max, `crates/manifold-renderer/src/node_graph/primitives/coc_dilate.rs`) built and wired into `CinematicScene` (`coc_from_depth.out -> coc_dilate.in -> bokeh_gather.width`, replacing the direct `coc_from_depth -> variable_blur` wires, then re-pointed at `bokeh_gather` when P4 landed the same session) 2026-07-13 (Sonnet 5, `dof-polish` worktree, branch `feat/dof-polish`). Gate green (I1 generated-vs-hand parity + flat-field no-op gpu_tests, full `manifold-renderer --features gpu-proofs` sweep, focused nextest, scoped clippy clean, `check_presets` 57/57, I5 load-smoke). Orchestrator PNG look-pass confirmed the silhouette-bleed halo is visibly gone post-fix (see the note below) — but `CinematicScene`'s test geometry (one flat mesh) has no real foreground/background depth split, so Peter's own look at a richer scene (`SceneLadders.manifold` or similar) is still the real exit per the amended demo rule.

**Root cause** — `node.variable_blur` picks its per-pixel gather radius from *only the center
pixel's own* CoC (`step_size = center_coc * max_radius + 1.0`,
`gaussian_blur_variable_width_body.wgsl:77`). There is no dilation / max-CoC pre-pass, so a
heavily-blurred pixel never borrows a wider radius from a neighboring high-CoC pixel, and a sharp
pixel is never bled into by an adjacent blurred one. At any depth discontinuity — the silhouette
of an in-focus subject against a blurred background, or vice versa — this produces a hard seam
right at the edge instead of a soft transition. Peter's description: "like the blur is applied to
a plane." Confirmed `CinematicScene` runs `weighting_mode: 0` (plain averaging, every neighbor
weighted equally) — the CoC-comparison step function (`coc_weight()`, same file) isn't even
active in the shipped preset, so the hard edge is purely the missing dilation, not the weighting
mode.

**Fix shape** — add a CoC-dilation atom: spread the maximum CoC found in a small neighborhood
(e.g. one tile) outward before the two `variable_blur` passes consume it — the standard technique
used by most real-time DoF implementations to get soft depth-edge blending from an otherwise
naive per-pixel-radius gather. New primitive, `Gather`-family input access, CPU-reference
testable like every other atom this wave. **Scoping decision COMMITTED 2026-07-13 (Fable, design
session): a standalone atom (`node.coc_dilate`, neighborhood max of the CoC texture) — folding a
neighborhood read into `coc_from_depth` would change that atom's Pointwise fusion classification
and cost its fusability, so the fold option is dead.** The dilated CoC feeds whichever gather
consumes it: the shipped `variable_blur` pair today, `node.bokeh_gather` after CINEMATIC_POST P4
(which needs the dilation equally — P4 does not make this bug obsolete).

**2026-07-13 update (P4 landed):** `node.bokeh_gather` (`crates/manifold-renderer/src/node_graph/
primitives/bokeh_gather.rs`) built and swapped into `CinematicScene` in place of the two
`variable_blur` H/V nodes, still reading `coc_dilate`'s dilated output (`coc_from_depth.out ->
coc_dilate.in -> bokeh_gather.width`, `coc_from_depth`/`coc_dilate` wires unchanged from the
BUG-137 fix above) — per this same note's own prediction, P4 does not obsolete the dilation, and
the wiring confirms it still feeds the gather. Gate green (I1 generated-vs-CPU-reference +
generated-vs-hand parity, I2 zero-CoC pass-through, full `manifold-renderer --features gpu-proofs`
sweep 1463 passed, focused nextest 1150 passed, scoped clippy clean, `check_presets` 57/57, I5
load-smoke `bundled_cinematic_scene_loads_and_compiles`).

**Orchestrator PNG look-pass (2026-07-13, Sonnet 5):** rendered `CinematicScene` before/after via
`render-generator-preset` (1280x720, 90 frames) at the wire state immediately before vs. after the
`bokeh_gather` swap. Visible difference: the pre-swap render shows a soft glow/halo bleeding
outward from the plane's silhouette into the black background; the post-swap render's silhouette
is crisp with no bleed — consistent with D5's occlusion-aware `step()` weighting suppressing
cross-edge contribution. This is a real, looked-at improvement, but the test scene (`CinematicScene`'s
single flat mesh) has no foreground/background depth split, so it does not exercise BUG-137's
literal "in-focus subject against blurred background" seam scenario end-to-end. **Status downgraded
to FIXED, pending Peter's own confirmation on a richer scene** (same posture as BUG-119 in the
`scene-ladder-state` memory) rather than closed outright.

### BUG-139 (bug-status-rebuild-drops-fixed-pointer-lines) — bug_status.py's parse() mis-bucketed the ## Fixed archive-pointer lines (rebuild drop + false check noise) — FIXED 2026-07-13
**Status:** FIXED (2026-07-13) — pointers are now a first-class parse() bucket (never entry body, never strays); rebuild() re-emits them after the resolved entries under ## Fixed; write() grew a pointer-fidelity guard that refuses to write if any pointer line would change. check()'s archive cross-check reads the pointer bucket, killing the ~78 false "no ## Fixed pointer" warnings per landing. Regression tests: .claude/hooks/test_bug_status.py (both shapes: pointers with and without a leading full entry). log_bug.py's splice-not-rebuild rationale updated; its own insert path was already safe.

**Symptom:** Running 'bug_status.py --write' in a worktree reconstructs docs/BUG_BACKLOG.md via rebuild(head, entries, tail), which only re-emits parsed ### BUG-NNN Entry objects. The one-line closed-bug pointers under ## Fixed (e.g. '- BUG-078 (slug) — FIXED ... — full history in docs/archive/BUG_BACKLOG_CLOSED.md') are not Entry objects — parse() buckets them as 'strays' — and rebuild() never re-inserts strays. A --write run would silently delete all ~74 archive-pointer lines from the live file, breaking the archive cross-check bug_status.py --check itself performs. write() does print a stderr note listing dropped stray lines, so it is not fully silent, but it reads as a low-priority note rather than a content-loss warning, and nothing stops the write from completing.
**Root cause:** parse() classifies every non-blank, non-heading, non-'## ' line between ## Open and the next appendix heading as either an Entry body line (if under a ### heading) or a stray. The ## Fixed pointer lines match neither pattern (POINTER_RE recognizes them for the archive cross-check in check(), but rebuild() never consults POINTER_RE or the strays list at all).
**Second symptom (2026-07-13, widget-unification landing):** the same mis-bucketing makes check() print ~78 false "`archived ... but has no ## Fixed pointer here`" lines at every landing. With a full `### BUG-NNN` entry at the top of ## Fixed (BUG-140 currently), parse()'s block loop swallows the entire pointer list that follows it into that entry's *body* (it collects until the next ### heading), so `pointer_ids(strays)` sees zero pointers despite all of them existing in the file. Same root cause, opposite bucket. Side effect: while swallowed into a body, the pointers survive `--write` (verified no-op 2026-07-13) — the content-loss half above only fires when ## Fixed has no leading full entry. Fixing parse() to classify POINTER_RE lines as pointers wherever they appear kills both symptoms.
**Fix shape:** Have rebuild() re-emit the pointer strays verbatim in their original relative position within ## Fixed (sort resolved Entries and pointer strays together, or just append all Fixed-section strays after the resolved Entries, matching current file order). Add a regression test: parse a fixture with a resolved Entry + a pointer stray under ## Fixed, run write(), assert the pointer line survives.

### BUG-140 (glb-import-non-square-aspect-distortion) — imported glb scenes rendered at the envmap's 1024×1024 and stretched to canvas — FIXED 2026-07-12
**Status:** FIXED (2026-07-12) — root cause was the plan compiler, not the import path. Probe-confirmed (runtime eprintln): render_scene's color intermediate was allocated at the envmap's fixed **1024×1024** while the final target was project res — the scene rendered square then got stretch-sampled to canvas by the SSAO mix (distortion + ~8× resolution loss at 4K). Cause: `ExecutionPlan`'s default output-dims policy is "max of texture input dims", which is wrong for rasterizers whose texture inputs are scene resources (envmap, base-color maps), not screen buffers; the import graph is the only graph wiring concrete-dims textures into a render node, which is why only imports broke and Tesseract/basic shapes were clean. Fix: producer-declared `output_canvas_scale` now takes priority over the max-of-inputs heuristic in plan build (execution_plan.rs), and render_scene / render_3d_mesh / render_instanced_3d_mesh declare `(1, 1)` (canvas-sized outputs). Verified by Peter in-app at 1440v and 3840×2160. Residual class risk (recorded decision, not fixed): the max-of-inputs default is still a trap for any FUTURE rasterizer-style node that forgets the declaration — the deeper fix is a port-level screen-image vs resource distinction so the planner only sizes from screen inputs; belongs with the codegen/primitive-contract work. ADDING_PRIMITIVES carries the authoring rule.

**Symptom:** Peter screenshot of an imported glb scene (flower photogrammetry scan) shows the reference color-checker chart rendered visibly stretched/non-square. Follow-up observation (same day): landscape project squashes vertically, portrait squishes horizontally — i.e. the render ignores output aspect.
**Root cause:** see Status — plan dims policy sized the rasterizer's output from its resource-texture inputs.
**Fix shape:** shipped as above.

### BUG-126 (manifold-renderer-tests-clippy-debt-under-gpu-proofs) — 12 pre-existing clippy findings in `manifold-renderer`'s test code, only visible under `--tests --features gpu-proofs` — LOW, found not fixed 2026-07-12 (CINEMATIC_POST P0 fusion-layer session)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — same fix as BUG-124 (identical 12 findings, one fix covers both entries); see BUG-124's Status line for the fix shape, the 5 extra findings caught by the same gate re-run, and verification commands.
**Symptom:** `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` fails with 12 errors; the STANDARD gate (`cargo clippy -p manifold-renderer -- -D warnings`, no `--tests`, no feature) is clean and is what every prior session's gate ran, so this debt accumulated unnoticed.
**Root cause:** ordinary `needless_range_loop` / `manual_range_contains` / `identity_op` clippy lints in `#[cfg(test)]`/`gpu_tests` modules that only compile under `--tests --features gpu-proofs` — never exercised by the standard scoped clippy invocation.
**Sites:** `primitives/bend_mesh.rs:526`, `primitives/facet_normals.rs:324`, `primitives/gltf_mesh_source.rs:434`, `primitives/morph_mesh.rs:489`, `primitives/push_along_normals.rs:527,559`, `primitives/scatter_on_mesh.rs:551-553`, `primitives/taper_mesh.rs:512`, `primitives/twist_mesh.rs:505`, `primitives/revolve_curve.rs:334` — none touched by this session's D7/P0 diff (verified via `git status`/`git diff --stat`); found only because this session additionally ran `--tests --features gpu-proofs` clippy beyond its specified gate.
**Fix shape:** mechanical — rewrite each flagged loop to an iterator (`enumerate()`/`skip()`/`take()`) or `RangeInclusive::contains`; no behavior change. Low priority (lint-only, no correctness impact); worth a dedicated pass rather than folding into an unrelated phase's diff.

### BUG-135 (fused-texture-codegen-drops-wgsl-includes) — the FUSED multi-atom texture region codegen (`generate_fused` in `freeze/codegen.rs`) never emits a member's `wgsl_includes` — LOW, found not fixed 2026-07-12 (CINEMATIC_POST P1, `coc_from_depth` session)
**Status:** FIXED this session (fusion-sweep mechanical-sweep phase 1 worktree; lands with its commit).
**Symptom:** a texture-domain Pointwise atom that declares `wgsl_includes` (e.g. `node.coc_from_depth`, whose body calls `depth_common.wgsl`'s `linearize_depth`) and lands in a FUSED region with a pointwise neighbour would generate a kernel missing the shared helper's definition, failing naga parse — the install-time "fused kernel fails naga" refusal (`FREEZE_COMPILER_MAP.md` §4) makes this fail CLOSED (falls back to unfused, still renders correctly), so it is a missed-fusion perf gap, not a correctness bug. Confirmed to be BUG-141's exact root cause.
**Root cause:** `RegionNode.node_includes` (`freeze/codegen.rs:1270`, populated at `freeze/install.rs:1256` via `node.wgsl_includes()`) is read by `generate_fused_buffer` (`freeze/codegen.rs:1591`, `for inc in node.node_includes`) but NEVER read by `generate_fused` (the texture-region path, `freeze/codegen.rs:2084`–2628) — asymmetric with the STANDALONE texture path, which this same P1 session fixed (`generate_standalone_ext` gained an `includes: &[&str]` parameter, threaded from `standalone_for_spec` via `P::WGSL_INCLUDES`; see the fix's own commit). The FUSED texture path's `prelude`/`helpers`/`bodies` (`split_fns`-based) emission never merges in `node.node_includes` at all.
**Why not fixed at the time:** `coc_from_depth`'s neighbors in `CinematicScene` (P1's only consumer at the time) never formed a fusable region with it — its upstream `depth` input came from `node.render_scene` (always `Boundary`, a draw call) and its downstream consumer `node.variable_blur` read it via a Gather wire (gather-consumed wires never union per the union gates), so `coc_from_depth` was always an isolated single-node region there. BUG-141's glb-import graph hit a different topology where the fused region actually forms, surfacing the gap.
**Fix (applied):** mirrored `generate_fused_buffer`'s dedup-and-prepend `includes: Vec<&'static str>` loop inside `generate_fused`'s texture path (`crates/manifold-renderer/src/node_graph/freeze/codegen.rs`) — collects `node.node_includes` across `region.nodes` in the existing per-node loop (dedup by value), prepends the joined text right before the shared-prelude emission (same relative position `generate_fused_buffer` uses, before body emission). Two new tests: `codegen.rs::gpu_tests::fused_texture_region_carries_and_dedups_wgsl_includes` (unit-level: fuses `node.coc_from_depth` + `Gain`, asserts `fn linearize_depth` appears exactly once and the kernel parses through naga) and `freeze/proof.rs::coc_from_depth_fuses_with_pointwise_neighbor_and_matches_unfused` (end-to-end: the real glb-import-shaped region — `coc_from_depth` fused with `node.invert` — asserts `fuse_canonical_def` no longer falls back to `None`, both D7/P0 markers are present, and fused vs unfused render match within the out-of-loop tolerance). Both tests were verified to fail with the fix reverted (reproducing the exact pre-fix symptom: naga's "no definition in scope for identifier: linearize_depth" / `fuse_canonical_def` panicking) before the fix was restored.

### BUG-162 (ui-snapshot-feature-canonical-def-arc-regression) — `--features ui-snapshot` doesn't compile: `GraphView::canonical_def` changed to `Arc<EffectGraphDef>`, 8 call sites in `ui_snapshot/mod.rs` never updated — LOW (build-only, no runtime impact)
**Status:** FIXED 2026-07-14 (bug-wave3 lane D) — found as a blocker while investigating BUG-153 (needed a working `ui-snap` binary to reproduce). `cargo build --features ui-snapshot --bin manifold` failed with 8 `E0308` mismatches: `render::render_graph_to_png`/`render_graph_editor_to_png` expect `&EffectGraphDef` but received `view.canonical_def` (now an `Arc<EffectGraphDef>` per an unrelated session's change — `crates/manifold-app/src/ui_snapshot/mod.rs` was untouched by that session, confirmed via `git log`), and `AddSceneObjectCommand::new`/`AddSceneLightCommand::new` expect an owned `EffectGraphDef` but received `view.canonical_def.clone()` (an `Arc` clone, not a value clone). Fixed all 8 sites: 6 call sites now pass `&view.canonical_def` (auto-deref through the `Arc`), 2 now pass `(*view.canonical_def).clone()` (deref then clone to get an owned value). Mechanical, no behavior change once it compiles. Verify: `cargo build --features ui-snapshot --bin manifold` clean; `cargo run --features ui-snapshot --bin manifold -- ui-snap inspector` runs and writes a PNG.

### BUG-163 (amg-livery-black-body-carpaint-extension-and-texture-cap) — AMG GT3 body renders black: livery/base-color lives in an unmapped carpaint extension material and 14 textures drop over the per-object cap — MEDIUM (hero-asset fidelity)
**Status:** FIXED 2026-07-15 (GLB_CONFORMANCE_DESIGN G-P2, `909976d2`) — confirmed by the orchestrating session rendering the real AMG fixture through the landed code: `render-import` on `mercedes-amg_gt3__www.vecarz.com.glb` now reports `material_count: 78, object_count: 78, textures_wired: 39` (was `29` with `dropped_over_cap: 14`), and the rendered PNG shows the correct silver/NASA livery on the body panels — no black panels. `render_scene`'s clamp rose from `OBJECT_SLIDER_MAX = 64` to a real 1024 safety bound; `ImportReport.dropped_over_cap` is gone; the importer wires every material 1:1.

**Symptom:** `mercedes-amg_gt3__www.vecarz.com.glb` imports with `material_count: 78, textures_wired: 29, dropped_over_cap: 14` and report lines including `EXT_Carpaint_Inst: KHR…` — the body panels render pure black while rims/glass/interior read correctly. The store-page silver NASA livery never appears.

**Root cause (diagnosed, not fixed; corrected same day after reading the glb's JSON directly):** the "carpaint extension" framing was wrong — "EXT_Carpaint_Inst" is just the material's NAME. Its actual extension is standard `KHR_materials_clearcoat` (clearcoatFactor 1.0), which only adds gloss and is already IMPORT_FIDELITY Deferred #1 (this asset was predicted to fire that trigger). The livery/base color is an ORDINARY `baseColorTexture` (image index 3) in the core material — so the black body is caused by `dropped_over_cap: 14`: the per-object texture-wire cap drops 14 maps on this 78-material asset, and the body's base-color map is evidently among them.

**Fix shape:** primary — revisit the texture cap for many-material assets: raise it, or prioritize base-color maps when rationing wires (importer-side only; gets the silver car). Secondary, separate trigger — Deferred #1's clearcoat lobe for the paint's lacquer gloss (shader work, priced in the design doc).

### BUG-164 (material-maps-force-one-repeat-sampler-ignores-per-texture-wrap) — every material map samples through ONE hardcoded REPEAT sampler; a glTF texture's own wrap/filter settings (CLAMP_TO_EDGE, MIRRORED_REPEAT, NEAREST) are parsed but never reach the GPU sampler — LOW (found via the glb conformance harness, not yet judged against a hero asset)
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P4, D3) — per-map-family samplers (5 bindings replacing 1) each built from the glTF texture's own wrapS/wrapT + min/mag filter; `TextureSettingsTest.glb` flipped to expect_pass.

### BUG-165 (boombox-multi-texture-never-converges) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P1)
**Status:** FIXED — root cause was NOT the texture-decode/wiring hypothesis this entry originally carried. Diagnosed via a new `--trace` flag on `render-import` (prints non-black fraction + `io_pending` every frame, not just after a stable streak): `io_pending` goes `false` by frame 1 and the frame stays byte-stable-black from then on — ruling out both prior hypotheses (a decode race, and an ORM-texture wiring bug; `textures_wired: 1` counting only base-color is a report-line quirk, not a bug). The real cause: `node.orbit_camera`'s `near` clip plane defaulted to a fixed `0.05` (`camera_orbit.rs`), never scaled to the framed object's size. The importer already scales `distance = 2.2 * radius` to the object's own bounding-sphere radius, so the object's front face sits at `distance - radius == 1.2 * radius` from the camera — BoomBox's radius (0.0172, a real-world-meters-scale asset) put its front face at 0.0206, inside the fixed 0.05 near plane, so every frame clipped the entire object to black.

**Fix:** `gltf_import.rs` now computes `near_clip = DEFAULT_NEAR.min((distance - radius).max(1e-4) * 0.5)` and wires it onto the synthesized camera's `near` param (`DEFAULT_NEAR` is now a `pub const` on `camera_orbit.rs`, single source of truth shared with its own `ParamDef` default). The `.min(DEFAULT_NEAR)` cap means every asset whose front face already clears the old fixed default gets the IDENTICAL near value as before — verified empirically across all 58 then-`expect_pass` golden-checked assets: only Avocado (radius 0.0404), Corset (0.0399), and PotOfCoals (0.0617) besides the two bugs' own assets had their near value change at all, and their re-renders differ from the committed goldens by mean_abs ~1.5e-5 (rounding noise, unmeasurable) — no regression. `BoomBox.glb` now renders correctly (converges frame 4, non-black fraction 0.1299) and is `expect_pass` with a golden (`goldens/boombox.png`).

**Distinct finding:** `VirtualCity.glb` carried the same `xfail:BUG-165` note ("same never-converges class") but does NOT share this root cause — after the fix it still never converges after 60 frames with `io_pending=false` (167 materials/147 textures, a genuine separate throughput issue). Its manifest note now says so explicitly; it needs its own diagnosis, not a re-run of this fix.

**Symptom:** `docs/GLB_CONFORMANCE_DESIGN.md`'s own audit (§1) already names the mechanism: material maps sample via the dedicated REPEAT `material_sampler` (binding 22, landed `85b5bb9d` same day as the wrap-smear fix) — deliberately, to fix the striped-helmet out-of-range-UV bug. But that fix is a single global sampler, not a per-texture one: any glTF asset whose texture explicitly declares `CLAMP_TO_EDGE` or `MIRRORED_REPEAT` (or a non-default min/mag filter) has that declaration silently ignored — every map is forced to REPEAT regardless. Khronos `TextureSettingsTest.glb` renders non-degenerate (`render-import` produces a populated grid, no crash, non-black fraction 0.18) but its whole design exercises exactly this axis (clamp-s/clamp-t/repeat-s/repeat-t/mirror-s/mirror-t cells), so its correctness could not be verified without a fragment-level ground truth this session didn't build. Classified `xfail:BUG-164` in the conformance manifest rather than `expect_pass`, pending a real fix or a verified-safe reading of the render.

**Root cause:** unknown/not fully investigated this session — the `material_sampler` binding is provably one GPU sampler object shared by every map (`render_scene.rs`, landed `85b5bb9d`); confirming the *consequence* (which specific TextureSettingsTest cells actually render wrong) needs either a per-cell pixel comparison or reading `render_scene.wgsl`'s resolve path end to end, neither done here.

**Fix shape:** likely a per-texture (or per-material, since a group owns one texture set) sampler keyed by the glTF's own wrap/filter fields, rather than the single shared `material_sampler` — mirrors the anisotropy field's shape (D7 in GLB_CONFORMANCE_DESIGN.md): read wrap/filter off `gltf_load`'s parsed sampler, thread it through `node.gltf_texture_source`/`node.pbr_material`, and build (or select from a small pool of) samplers keyed by the resolved `(wrap_u, wrap_v, min, mag)` tuple at draw time.

### BUG-167 (spec-gloss-pbrspecularglossiness-entirely-unhandled) — `KHR_materials_pbrSpecularGlossiness` (the legacy spec-gloss workflow) is not parsed at all — falls back to a default material with no diffuse/specular/glossiness mapped
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P3, D2) — `KHR_materials_pbrSpecularGlossiness` converts to metal-rough at import time; `SpecGlossVsMetalRough.glb` flipped to expect_pass. Known named gap: the specularGlossinessTexture's RGB specular tint stays Deferred (needs a shader change), so the asset's two halves render close but not pixel-alike — documented in the conformance manifest note, not silently claimed as full parity.

**Root cause:** not investigated — `rg "pbrSpecularGlossiness|SpecularGlossiness"` across `gltf_import.rs`/`gltf_load.rs` returns zero hits; the extension is simply never read. Not in `GLB_CONFORMANCE_DESIGN.md` D5's scoped extension list and not covered by any §7 deferred item (D5 names sheen/iridescence/anisotropy-the-extension/volume/Draco/KTX2/meshopt as the deferred set; spec-gloss isn't in that list — a genuinely new gap this session's audit surfaced).

**Fix shape:** parse `KHR_materials_pbrSpecularGlossiness`'s `diffuseFactor`/`diffuseTexture`/`specularFactor`/`glossinessFactor`/`specularGlossinessTexture` and either convert to the existing metallic-roughness port set at import time (the common approach — diffuse≈baseColor, invert glossiness→roughness) or add a dedicated spec-gloss shading path. Low priority: legacy extension, one asset in the whole Khronos suite.

### BUG-168 (ext-mesh-gpu-instancing-unhandled) — `EXT_mesh_gpu_instancing` nodes import as "no materials with geometry — nothing to import", not as N instanced copies
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P4, D6) — `EXT_mesh_gpu_instancing` expands to N wired copies at summary time (raw-JSON sniff, no typed crate support); `SimpleInstancing.glb` (125 instances) flipped to expect_pass. True GPU-instanced rendering stays Deferred.

**Symptom:** `render-import` on `SimpleInstancing.glb` fails with the same "no materials with geometry" error `gltf_import.rs` emits when a glb genuinely has zero material-bearing primitives — but this asset does have a mesh with a material; the geometry is expressed as per-instance attributes (`EXT_mesh_gpu_instancing`'s `attributes.TRANSLATION/ROTATION/SCALE`) on a node that owns no vertex data of its own, which the importer's material/geometry summary walk apparently doesn't recognize as geometry-bearing.

**Root cause:** not investigated. `rg "mesh_gpu_instancing|gpu_instancing"` returns zero hits in the importer — the extension is unparsed.

**Fix shape:** read `node.extensions.EXT_mesh_gpu_instancing.attributes`, and for each instance either emit N copies of the node's ports (if the graph node budget allows) or add per-object instancing support to the relevant primitive. Not scoped to any G-P phase; one asset in the suite.

### BUG-169 (metalroughspheresnotextures-renders-fully-black) — FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P1)
**Status:** FIXED — root cause was NOT a texture-less-material/lighting bug, despite the prior "ruled out camera framing" note (that ruling used `--orbit 3.0`, which changes orbit angle, not distance/near — it never actually tested the near-clip axis). This is the exact same mechanism as BUG-165: `node.orbit_camera`'s fixed `near = 0.05` clip plane exceeds this asset's front-face distance. `MetalRoughSpheresNoTextures.glb`'s bounding radius is 0.0056 (this texture-less variant is authored at a dramatically smaller scale than its textured sibling `MetalRoughSpheres.glb`, radius 6.99 — the two "same spheres" assets are not actually the same scale), giving a front face at `distance - radius = 0.0067`, deep inside the 0.05 near plane. Confirmed via `--trace`: `io_pending=false` from frame 0 (there's nothing to decode, as this entry originally noted) and the frame is black from frame 0 — a pure clip-plane bug, no decode or lighting involved.

**Fix:** shared with BUG-165 — see that entry for the `near_clip` formula, the no-regression proof across the other 58 `expect_pass` assets, and `gltf_import.rs`. `MetalRoughSpheresNoTextures.glb` now renders correctly (converges frame 4, non-black fraction 0.1488, visibly matches its textured sibling's metal/roughness gradient grid) and is `expect_pass` with a golden (`goldens/metal_rough_spheres_no_textures.png`).

**Lesson for future bug-hunts:** "ruled out camera framing" needs to specify WHICH camera parameter was varied — orbit angle and clip-plane distance are both "camera framing" in casual language but only one of them was actually tested here, and it was the wrong one.

### BUG-171 (boxvertexcolors-no-material-primitive-skipped-entirely) — a mesh primitive with vertex colors but no material index (spec-legal — implies the glTF default material) is skipped entirely, not imported with a default material
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN.md P3, D4) — synthetic default-material entry (sentinel `material_index = u32::MAX`) added for materialless geometry; `BoxVertexColors.glb` now imports (previously errored) and flipped to expect_pass. Known named gap: COLOR_0 (per-vertex color) itself is still never read anywhere in the mesh pipeline — the box renders flat gray, not with its vertex colors — escalated to its own entry, BUG-177.

**Root cause:** not investigated. `gltf_import.rs`'s geometry-summary walk (the "no materials with geometry" error path, `gltf_import.rs:385`) appears to require every counted primitive to carry an explicit `material` index; a primitive that omits it (relying on glTF's implicit default material, spec-legal) contributes no geometry to the summary, so an asset built entirely of such primitives imports as empty.

**Fix shape:** treat a materialless primitive as using glTF's default material (base fallback PBR values) rather than excluding it from the geometry summary — likely a small addition next to the existing `default_material_vertex_count` report-line path (`gltf_import.rs:416`), which already tracks materialless *sub*-geometry within an otherwise-material asset but apparently doesn't cover an asset that is *entirely* materialless primitives.

### BUG-172 (recursiveskeletons-no-default-scene-rejected) — a glb with no `scene` index (spec-legal — importer should fall back to all root nodes) is rejected outright
**Status:** FIXED 2026-07-16 (GLB_XFAIL_BURNDOWN_DESIGN P2, D5) — `resolve_import_nodes()` (`gltf_load.rs`) replaces all 3 `document.default_scene().ok_or_else(...)` sites: default scene if present, else the union of every `scenes[]` entry's nodes (de-duplicated by node index), else every parentless node in the document. `RecursiveSkeletons.glb` now imports and renders non-black (converges frame 4, fraction 0.0764). Originally found 2026-07-15 during GLB_CONFORMANCE_DESIGN G-P7 full-suite classification (`RecursiveSkeletons.glb`: `render-import` fails "glb has no default scene").

**Root cause:** not investigated — the importer requires `document.default_scene()` (or equivalent) to resolve, and errors rather than falling back to importing every root-level node when the glTF omits the top-level `scene` field (legal per spec: absence just means "no default scene is suggested," not "there is no content").

**Fix shape:** when no default scene is present, fall back to unioning all nodes referenced by any `scenes[]` entry (or, if `scenes` is also empty, all nodes with no parent) rather than erroring. Low priority: RecursiveSkeletons is a skinning stress-test asset (also out of scope per deferred item 7), and no-default-scene is a rare authoring choice.

### BUG-181 (import-ao-mix-flattens-alpha) — imported GLB layers composite fully opaque: the AO group's `node.mix` replaces the scene's alpha with the AO map's alpha=1, so the black void hides every layer below
**Status:** FIXED 2026-07-16 (same day) — option (a) shipped: `node.mix`'s non-Lerp blend modes now pass input `a`'s alpha through untouched (`mix.wgsl` + `mix_body.wgsl`, mode-conditional; Lerp keeps its full crossfade). Preset sweep confirmed nothing relied on the old alpha-lerp in a non-Lerp mode (43 instances checked; FilmGrain's Overlay had the same latent defect and is fixed by the same change; Lightning terminates in `set_alpha` so unaffected). Gate: new value-level alpha gpu_test + generated-vs-hand parity + full alpha-contract sweep + generator smoke, all green. Peter's visual confirm (plasma visible under the skull GLB void, contact AO intact) still owed.

**Symptom:** an imported GLB generator layer blacks out everything beneath it in the compositor, even though `render_scene` correctly clears its void to transparent `(0,0,0,0)` (`manifold-gpu` `encoder.rs` MSAA pass clear) and its lit shaders leave alpha untouched (alpha contract honored). The compositor's Normal blend is a correct premultiplied over (`compositor_blend_compute.wgsl` case 0) — with the scene's real alpha the layers below would show — but by the time the graph output reaches it, alpha is 1.0 across the whole frame.

**Root cause (confirmed by reading the full chain):** the import rig's spine is `render_scene → ao group → final` (`gltf_import.rs` ~1239–1349; the same AO group CinematicScene ships per CINEMATIC_POST D9, so that preset has the identical defect). Inside the group: `ssao_gtao` writes its AO map as `vec4(ao, ao, ao, 1.0)` (`ssao_gtao_body.wgsl:219` — legitimate for a data texture), the bilateral blurs preserve that alpha, then `node.mix` (Multiply, `amount = 1.0`) computes `out_a = mix(a.a, b.a, amount) = b.a = 1.0` (`mix.wgsl:96`) — the scene's alpha is replaced wholesale by the AO data texture's filler alpha. This is the alpha-contract violation class `alpha_contract.rs` guards against, but it arises from *wiring* (display texture × data texture through mix's lerp-alpha semantics), not from any single primitive, so the sweep can't see it.

**Fix shape:** AO modulation must preserve the display input's alpha. Root options: (a) make `node.mix`'s non-Lerp blend modes pass `a`'s alpha through (the shader already documents "blend modes are RGB-only" — lerping in `b`'s alpha during a Multiply was never meaningful), or (b) route AO through an RGB-only modulate path. Prefer (a) — it fixes the class (import rigs, CinematicScene, any user graph multiplying a data map onto a display chain); before changing semantics, sweep existing presets for anything relying on mix's alpha-lerp at amount=1, and re-run the fusion parity proofs for `mix_body.wgsl`. Instrument-level consequence while open: an imported GLB scene can't be layered over anything — it only works as the bottom layer of a stack.

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

### BUG-264 (param-step-action-ui-flow-stale-asserts) — `scripts/ui-flows/param-step-action.json` step 6/last assert an "A"/"S" button `under_text: "Amount"`; finds 0 on MAIN (pre-existing, fails identically before the param-drawer unification)
**Status:** OPEN (found 2026-07-19, param-drawer-unification lane). Repro: `cargo xtask ui-snap inspector --script scripts/ui-flows/param-step-action.json` — steps 0–5 pass, step 6 `Count(1)` gets 0 on main AND on the lane.
**Severity:** LOW — acceptance-flow rot, not an app bug: the "A" audio button exists (the flow's own earlier steps arm and use the drawer), the spatial/text query no longer matches anything.
**Symptom:** the param-step-action acceptance flow always fails at step 6.
**Root cause:** unknown — NOT the 2026-07-11 "Amount"→"Sensitivity" slider relabel (retried with `under_text: "Sensitivity"`, still 0). `under_text` semantics in the query engine vs. where the "A" button actually sits (param row, not drawer row) needs someone to read the query matcher, not guess labels. Suspects: stale assumption from the pre-drawer-redesign row layout.
**Fix shape:** read the `under_text` matcher in the ui-snapshot query engine, find what the "A" button is actually "under" in the current layout, update the two asserts — or replace them with the drawer-row-aware query the newer flows use.
