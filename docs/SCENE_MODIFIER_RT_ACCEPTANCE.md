# Scene modifier RT — acceptance definitions

<!-- index: Implementation-ready numerical, catalog, export, lifetime and performance gates for automatic RT on modified meshes. -->

**Status:** APPROVED definitions · 2026-09-16 · Codex lead · tests below are implementation deliverables, not executed results.

Authority: [design and phase briefs](SCENE_MODIFIER_RT_DESIGN.md), [dated source inventory](SCENE_MODIFIER_RT_INVENTORY.md), existing [modifier validation contract](SCENE_MODIFIER_VALIDATION_PLAN.md). Work item: `BUG-e3p6.4`. No rendering, GPU capability or performance result is asserted by this document. The lead owns acceptance; K3 implements the named probes alongside their fixes.

## A0. Harness, evidence and execution

Extend `crates/manifold-renderer/tests/gpu_proofs/` and its existing shared native Metal device/readback harness. New file `rt_dynamic_geometry.rs` contains nested modules named `rt_dynamic_baseline`, `rt_dynamic_fusion`, `rt_dynamic_ordering`, `rt_dynamic_shading`, `rt_dynamic_current_frame`, `rt_dynamic_refit` and `rt_dynamic_catalog`. Put the named test in its owning module so phase filters select real tests. CPU tests use `mesh_change_` prefixes. App tests use `rt_dynamic_export_` and the existing `journey-proofs` harness. Register modules in the existing test root; zero selected tests is a failed gate.

Add a debug ray-query entry point beside `MetalShadowRayTracer::debug_fetch_interpolated_normal`, using the production candidate-hit helper, source tables, descriptors and AS. It encodes into the caller's encoder and returns readback buffers; the harness waits only after all geometry/update/query commands are submitted. Ray input is origin/direction/tmin/tmax; output includes hit flag, object/instance/primitive IDs, distance, barycentrics, interpolated normal/UV and coverage. Invalid IDs are explicit sentinels. Debug code must not create a second acceleration or material implementation.

Use a CPU Möller–Trumbore oracle only in tests. Read final GPU geometry after query completion to compute the oracle when testing real modifiers: never duplicate modifier math. Compare rays away from edges (minimum barycentric coordinate 0.05); separate edge/degenerate cases use explicit expected hit/miss rules. Seed is fixed and reported. For stochastic visibility use fixed seeds over many independent samples; do not compare single noisy beauty images.

Common thresholds: hit/miss and all IDs exact; finite distance/position error ≤ `max(1e-4 scene units, 1e-4 * abs(expected))`; barycentric/UV error ≤ `2e-4`; normalized normal dot ≥ `0.9999`; no NaN/Inf. Fixture extent is 2 scene units unless specified. Rebuild-vs-refit uses the **same current GPU output**, not separately evaluated modifiers. Rays at distances near a topology boundary are excluded from the general tolerance and tested explicitly.

Exact group ownership: `rt_dynamic_baseline::rt_dynamic_baseline_records_unsupported` is P0-only; `rt_dynamic_ordering` owns backend same-command-buffer and teardown/snapshot probes in A2 (P3); `rt_dynamic_fusion::rt_dynamic_fusion_roundtrip` runs the fused/unfused metadata and actual GPU mesh-output comparison in A1 (P2). `rt_dynamic_shading` owns A4 (P4a/P4b). `rt_dynamic_current_frame` owns pending/admission/history and production-renderer versions of A2/A3 (P5). `rt_dynamic_refit` owns final selective-action and fresh-build comparisons in A3 (P6). `rt_dynamic_catalog::rt_dynamic_catalog_all_stock_and_compositions` owns A6 (P7a). Add module `rt_dynamic_perf` with `rt_dynamic_bounded_perf` for A9 as an explicitly invoked release-build proof (P8); ordinary GPU mode excludes this measurement module. CPU mode runs A1, catalog mode A6, export mode A7, and GPU mode the correctness groups only. Each phase adds only its now-passing group; the final runner requires every final group.

P0 first introduces a baseline witness that passes by recording the existing unsupported result and proving the oracle distinguishes alternating states. Remove that baseline limitation assertion when P5 lands; the runner then requires the positive same-frame test. Never leave ignored tests, expected-failure acceptance tests, reduced ray counts or missing modules to obtain green output.

Deliver `scripts/rt_dynamic_acceptance.py` in P7b, following `gpu_proofs_gate.py`/journey runner conventions. CLI: `--manifest-path PATH --mode cpu|gpu|catalog|export|perf --report PATH`; execute exactly the selected group and exit nonzero on a failure, missing test, missing native RT support, missing required ffmpeg/ffprobe, or malformed report. An unavailable host yields a blocked report, never a passing qualification. This script dispatches existing cargo/gate entry points; it is not a second test framework. Perf mode accepts `--reference-project PATH --held-out-project PATH` and records both files' SHA-256 hashes.

Report JSON fields: `schemaVersion: 1`, `gitSha`, `dirtyPaths`, `hardware`, `osVersion`, `mode`, `fixtures[{name,hash}]`, `tests[{name,status,observed,required,artifactPaths}]`, `metrics`, `commands`, `exitCode`. GPU report records GPU family, sample/resolution settings, output precision and whether validation layers were enabled. Store artifacts under `/tmp/manifold-rt-dynamic/<git-sha>/`; tests may use a unique child directory. Checked-in fixtures are compact generated geometry/project definitions, not large GPU dumps. PNGs/videos are for Peter's review, never the only oracle.

## A1. CPU revision and preparation contract — P1/P2

| Test deliverable | Inputs/actions | Required result |
|---|---|---|
| `mesh_change_default_is_conservative` | Undeclared mesh writer, raw authored WGSL, externally prebound mesh | Every actual unknown write changes topology/positions/content. Untracked prebound data changes conservatively each evaluated frame. No RT opt-in needed. |
| `mesh_change_deformation_and_attributes` | Unary deformer, normal-only writer, static source; controls change and then settle | Deformer retains topology, changes positions/content; normal-only retains topology/positions, changes content; truthful unchanged output retains all. |
| `mesh_change_multi_input_and_cut` | Morph with two independently changing source lineages; cut remap with stable/changing map; same capacity throughout | Both source topology dependencies participate. Map content change revises remap topology; moving source with unchanged map does not. No pointer/count shortcut. |
| `mesh_change_alias_memo_hoist_feedback` | Input switch, memo hit/miss, hoisted node, in-place write, delayed capture; reuse physical slot for another logical resource | Correct selected source revision survives; no overwritten-input self-dependency; recycled slot cannot inherit old logical identity; feedback revision changes when its bytes become observable. |
| `mesh_change_pending_through_fusion_and_alias` | Source pending for two evaluations, then ready; through unary, alias, fused node, scene bundle | All dependent mesh consumers remain pending. Pending is independent of an unchanged generation. The first complete scene uses the ready revision. |
| `mesh_change_fused_rules_match_unfused` | Same canonical graph: unfused, fused, cache hit, reload, reordered/repeated modifier cards, segment splice | Same aspect-change sequence and update class; no lost metadata at def/view boundaries or card-prefix collisions. Fusion stays enabled. |
| `mesh_change_metadata_rejected_when_invalid` | Unknown output/input, wrong layout, missing generated-node sidecar, stale IDs after rewrite, duplicate stable IDs | Structured preparation error before publication; original active graph remains valid. No canonical fallback hides the error. |
| `mesh_change_custom_source_and_serde` | Custom triangle producer, authored fake metadata WGSL comment, project save/reload | Producer runs via conservative rebuild; comment cannot assert trusted refit; no new serialized fields. Existing project content round-trips. |

Compile-time migration gates also require trait/blanket forwarding and all prepared view constructors to include the new fields. An empty sidecar is valid for canonical nodes with declarations; it is invalid when a generated fused mesh node is expected to carry compiler provenance.

## A2. Same-frame command order and lifetime — P3/P5

`rt_dynamic_same_frame_gpu_write_then_hit`: GPU kernel alternates a triangle between x=-0.75 and x=+0.75 inside one persistent private vertex buffer for eight frames. Each frame encodes write → build/update → two analytical rays on **one caller command buffer**, without an intermediate commit/wait. Current-side ray hits, previous-side ray misses; all eight frames must pass. First frame must dispatch RT. Repeat for an indexed buffer whose connectivity changes in place at equal capacity. The planner must report rebuild for index/topology changes.

`rt_dynamic_unsubmitted_teardown_and_multiframe`: encode geometry, update and query; tear down graph/owner before committing; commit and verify the query. Repeat with three distinct input snapshots queued before one completion wait, changing transforms, gain/material and normal-source values in each. Each output must match its own snapshot. Replace resources while earlier submissions are in flight; hold only the required completion pins. Assert zero GPU faults/use-after-free and readiness attached to the correct resource set. Artificially delayed completion uses the existing test synchronization mechanism, not a production thread or sleep.

`rt_dynamic_pending_first_ready_frame`: pending mesh behind a modifier and fused path for two frames. No partial RT scene is published; existing candidate remains active. When ready, the first published frame hits the new mesh. Inject pending/unavailable into an active source and assert the specified structured error, not silent omission or old geometry.

`rt_dynamic_admission_is_atomic`: override existing modifier memory budget to one byte below the reported candidate peak, then exactly the required allowed peak. First rejects before allocations/publication; second prepares and runs. Include old/new resource overlap, worst-case emissive candidates and sort scratch. Inject allocation failure at each new resource kind; old resident remains valid and failed candidate resources retire. Negative counts/overflow/layout mismatch fail before GPU encode. No allocation-failure test is allowed to crash or hang the real GPU.

## A3. Update policy and refit — P5/P6

Use a scene with three objects: one static triangle, a 32×32-cell wave grid (2048 triangles), and an indexed cut fixture. Query 64 deterministic non-edge rays per moving object over eight distinct states. Keep a freshly built reference from the same output for comparison.

| Test | Exact action/count requirement after preparation |
|---|---|
| `rt_dynamic_selective_updates_and_idle` | Idle frames: zero BLAS/TLAS operations and zero emissive refreshes. Wave-only dirty frame in P6: one BLAS refit, zero builds, one TLAS refit. Static peer untouched. Transform-only frame: zero BLAS work, one TLAS refit. |
| `rt_dynamic_same_capacity_cut_rebuilds` | Connectivity/cut-map mutation with identical buffer identities and counts: exactly one affected BLAS build and one TLAS update; zero stale-triangle hits. Repeated identical cut map followed by position motion selects refit. |
| `rt_dynamic_refit_matches_rebuild` | Every ray matches current fresh-build reference and CPU oracle. Expand vertices beyond initial bounds, collapse to finite zero-area triangles, revive them, reverse phase, seek backward and restore. No miss tolerated for revived triangles. |
| `rt_dynamic_deform_then_instance` | Deformed mesh under translated, mirrored and nonuniformly scaled copies; dead slots; wired capacity-one instance buffer. Hit IDs and distances meet A0; unit/uniform-scale normals meet the analytical oracle. Nonuniform-scale normals must match the current fresh-build RT reference (the inherited normal-transform approximation is documented in design §5.2, not silently claimed fixed). BLAS motion updates TLAS bounds even when instance transforms are unchanged. |
| `rt_dynamic_membership_and_capacity` | Add/remove/reorder objects, replace mesh source/layout, grow instance capacity through preparation. One valid canonical table order; no stale material/normal/descriptor indices. Build TLAS on structural list/capacity changes. |
| `rt_dynamic_attributes_only` | GPU changes only normals/UVs, then weights/gain, then emission factor | No BLAS/TLAS operation while descriptor opacity class stays fixed; current shading fields change. Crossing descriptor class causes one affected BLAS rebuild. |

P5 intentionally reports builds where P6 reports refits; phase-specific expected counts must be explicit in tests, never relaxed to “either works.” After P6, final acceptance requires refits for Surface Waves. If Metal cannot refit a degenerate-to-valid representation, the lead receives that exact failed proof; rebuild remains correct, fast-path qualification stays blocked for that representation. Do not suppress the proof or claim all positions-only layouts refit safely.

## A4. Emissive geometry, appearance and attributes — P4a/P4b

`rt_dynamic_emissive_gpu_geometry`: private GPU mesh changes position/area/UV after CPU encode starts; generate 3, then 4097 candidate triangles with unequal known powers, plus equal-power ties. Read table/stats only after completion. Compare to a CPU reference built from final GPU bytes: selected IDs exactly match top-4096 order with the specified tie break; entry count exact; positions/UVs meet A0; total area/mean power and alias-derived probabilities within `max(1e-5, 2e-4 * abs(reference))`. Probabilities sum within `2e-4` of one; all alias indices in range and all probabilities in [0,1]. Repeat zero→positive emission, positive→zero, collapse→revival, indexed data and current material changes. Zero entries have finite zero stats and no invalid sample.

Repeat noninstanced object transforms and wired instances, including scale (2,1,0.5), mirroring and dead slots. Preserve the existing local/world ranking and sampling conventions; check descriptor IDs and sampled world area against transformed vertices. Direct-light and firefly kernels must consume current GPU stats on frame one. A deliberately stale CPU mirror must not affect results. Production mapped vertex/index reads are forbidden.

`rt_dynamic_coverage_and_attributes`: triangle weights [0,0.5,1], gains [0,0.5,1,2], at specified barycentrics. Compare coverage/brightness formulas to raster helper with absolute error ≤1e-6; test existing alpha mask just below/equal/above cutoff, the already-supported emissive texture transform and current UV/normal changes. Existing base-alpha-factor, nonemissive UV-transform and authored-tangent limitations remain explicit; do not use those known gaps as new passing parity claims. Coverage 0 always misses; 1 always accepts. For each fractional case send 65,536 fixed-seed samples: absolute acceptance-frequency error ≤0.01. All visibility/GI/reflection walkers invoke the common helper. Emissive sample radiance applies coverage once; accepted hit radiance applies brightness once. Nonfinite radiance fails.

Run existing static emissive, alpha-mask, instancing and normal tests selected by the diff-based gate. Equal-power truncation order is the only intended static emissive selection difference. Stable scenes retain existing visual/lighting behavior within the existing numerical gates, not a newly enlarged screenshot tolerance.

## A5. Temporal state — P5

`rt_dynamic_history_reset_and_resume`: inject a distinctive finite sentinel in each RT accumulation/moments/SVT, denoiser and temporal-upscale history via the test harness. Change positions/topology, then normals/UVs/material/appearance, while keeping camera/transforms fixed. Reset decision must be true for each affected consumer on that frame, output must equal its fresh-history numerical reference within the existing filter tolerance, and the sentinel must contribute zero weight. Freeze the modifier: subsequent unchanged frames retain history and sample count increases. Transform-only motion retains existing reprojection behavior. First valid frame is seeded from current samples, never black.

Do not pass this test by permanently disabling temporal systems. Record reset counts alongside AS counts; initial setup is excluded from steady-state counts. True deformation velocity is deferred explicitly; continuous deformation can be noisier under this v1 policy.

## A6. Complete stock catalog and future producers — P2/P7a

Discover recipe files from `assets/scene-modifier-presets/*.json`; at audit time there are **13 (9 available, 4 hidden)**. Assert that discovered names equal the acceptance fixture registry; an added recipe without a fixture fails with its name. Attach through `prepare_new_scene_modifier`/`prepare_scene_modifiers` to the existing tiny imported host and generated analytical host. Use real parameter bindings to enable RT and assert RT actually dispatched. Capture final geometry for the independent ray oracle; do not validate by “nonblack output.”

| Class | Recipes | Required coverage |
|---|---|---|
| Deformation | ElasticSculpture, SurfaceWaves | Idle, nonzero motion, controls at documented endpoints, pause, seek. Proven stable topology selects refit. |
| Cut/remap | MaskedPeel, OrderedRecon, OrderedReconHit, SurfacePeel, SurfacePeelHit, VortexFragments | Topology control mutation, unchanged cut with animation, same-capacity changed map, finite padded triangles. |
| Instances | SpatialEchoes, WavesEchoes, SceneLoop | Live/dead slots, one and multiple copies, changed transforms; WavesEchoes combines deformation and instancing. |
| Scene-only | RenderMode, SceneFog | No unnecessary AS work when geometry/instances unchanged; material/render behavior retained. |

Composition fixtures: Waves→cuts, cuts→Waves, Waves→echoes, two Waves cards with distinct IDs, cut→Waves→echoes. Test each fused and unfused, before/after card reorder, undo/redo, save/reload and graph replacement. Use the current allowed parameter range from recipe metadata; record chosen finite values in fixture JSON. Any recipe that cannot attach under current host rules must supply an appropriate supported host; skip is not accepted.

Custom future-proof fixture: load an unregistered custom WGSL mesh producer that changes topology at equal capacity, attach normal modifiers and render with RT. Correct rebuild must occur without adding its type/name to RT code. A source check forbids modifier recipe IDs or recipe-count branches in production RT maintenance.

## A7. Production export and failure behavior — P7b

Use real `ContentThread::run_export`, `ContentPipeline`, native encoder and the existing `journey-proofs`/ffprobe path. Fixture: 120 BPM, 2 beats, 12 fps, 320×180 SDR = exactly 12 frames. RT enabled through saved manifest bindings; wave plus one topology change at output frame 6. Place a diffuse receiver so the ray-hit witness affects visible output. Record pre-encode hit/AS/state-step counters per output frame through a test observer; never rerun graph evaluation for the observer.

* `rt_dynamic_export_first_frame_and_state_steps`: frames 0–11 each have current expected hit/mesh revision, frame 0 has RT dispatch, topology change first appears on frame 6. Exactly one simulation/render evaluation per exported frame after declared setup; first dt=0 and later dt=1/12. Stateful SceneLoop modifier is included in a second fixture. Warmup is separately counted and resets/re-seeks through the existing path.
* `rt_dynamic_export_repeat_and_sections`: repeat from the same saved project and seed; raw numerical witnesses identical. ffprobe frame count, dimensions and frame timestamps match; timestamps monotonic at 1/12 spacing within container timebase. Section export reinitializes at each declared section origin; no previous-section geometry/history leaks. Compressed pixel equality is not a ray oracle.
* `rt_dynamic_export_fault_before_encode`: inject structured prepare/encode error, changed GPU-fault count, ignored submission and completion timeout through existing test seams. Zero affected frames reach `ExportSession::encode_frame`; export reports failure and performs existing cleanup. Do not intentionally hang/fault hardware.
* `rt_dynamic_export_cancel_resize_hdr`: cancel before next frame, verify existing partial-file cleanup and successful subsequent export; resize to 640×360 between exports; run a 2-frame HDR fixture through `pq_encode_for_export` and its final completion fence. Assert correct dimensions/current frame and no stale resource pin.

Export uses shared acceleration policy; no content-settle loop, duplicate engine tick, export-only AS path or unsupported-mode raster substitution.

## A8. Performer flow and project compatibility — P7b

One registered existing UI flow, one reproduction and one verification after an evidenced fix: open tiny saved 3D scene; enable RT; add Surface Waves; animate phase; add cuts and echoes; reorder; undo/redo; save/reopen; export. Assert visible modifier cards and project state through existing UI/accessibility helpers; numerical GPU/export gates establish geometry correctness. Produce a short diagnostic recording/PNGs and the exact worktree launch command. Peter's visual/performance judgment is L4 and remains pending until he actually tests; do not label it passed from agent inspection.

Compatibility: existing project fixtures deserialize without migration changes; canonical recipe JSON contains no RT capability flags. No new renderer selector, RT setting per modifier, or per-recipe implementation is accepted. Existing AlphaMode::Blend RT boundary is retained and documented; it is not advertised as newly supported transparent transport.

## A9. Bounded performance, allocation and memory — P8

Acceptance budgets below are **targets, not measurements**. Use release builds on Peter's current native-RT Mac, plugged in, report model/GPU/OS. Disable validation layers only for timing, retain correctness run with the ordinary proof configuration. Render at 1280×720, the existing `RtQualityColumn::default()` realtime settings (`manifold-foundation/src/settings.rs:188`: shadows=1, AO=4, GI=4, reflections=8 samples, Half ray resolution, Medium spatial denoise); serialize/report those settings and the existing upscaler selection and hold them constant across comparisons.

Reference scene: two objects, one static and one exactly 65,536-triangle grid with Surface Waves, one directional light and a small emissive triangle; one view, no echoes/cuts. Held-out scene: an existing representative imported scan selected from Peter's projects, with its path/hash, triangle count and modifier chain recorded before timing. If none is available, mark the held-out gate blocked and report it; do not invent a proxy pass.

Prepare once, 16 warmup frames then 120 measured frames; one bounded run per fixture/configuration. Reference configurations: RT off; static RT; dynamic selective-refit RT; fresh-build RT reference using the same geometry (test harness only). One verification run is permitted after a concrete fix. Do not launch a general soak or explore arbitrary GPU settings.

Required reference-scene gates: full GPU frame p95 ≤16.67 ms; content-thread frame p95 ≤16.67 ms; dynamic AS maintenance GPU p95 ≤2.0 ms; static RT regression relative to pre-change baseline ≤5% or 0.2 ms, whichever is larger. Report p50/p95/max, actual misses and total update-plus-trace cost. Missing the target blocks the **live Surface Waves qualification**, not the correctness of the rebuild/export milestone; the lead gets the measured bottleneck and decides follow-up scope. Do not quietly reduce resolution/samples or relabel target hardware.

Held-out scene has no invented 60 fps guarantee: require correct current-frame results, no faults, admission within the existing memory limit and bounded prepared resource use; report its achieved p95/memory and whether it meets 16.67 ms. Cuts rebuild correctly; their frame time is measured and reported, never used to silently disable RT.

After warmup require **zero new Metal buffers, AS objects, pipelines or scratch growth** from this feature during unchanged-capacity animation; zero new Rust heap allocations from its revision/RT preparation/maintenance code. Count framework command encoders/completion callbacks separately. End-state resident resource count equals start-state count. Measure actual peak including replacement overlap and compare with admitted bytes (actual ≤ admitted estimate plus explicitly reported preexisting device usage); no unchecked growth. Pause animation: subsequent frames have zero BLAS/TLAS/emissive updates and zero deformation resets.

## A10. Commands and final acceptance ledger

Commands use an acquired absolute `RT_WORKTREE` path and run only the phase's selected gates. Cargo target/builder lock follows repository slot discipline. These are implementation-time commands; this documentation task does not run them.

```sh
cargo test --manifest-path "$RT_WORKTREE/Cargo.toml" -p manifold-renderer mesh_change_
python3 "$RT_WORKTREE/scripts/gpu_proofs_gate.py" --manifest-path "$RT_WORKTREE/Cargo.toml" --filter rt_dynamic_refit
cargo test --manifest-path "$RT_WORKTREE/Cargo.toml" -p manifold-app --features journey-proofs rt_dynamic_export_ -- --test-threads=1
python3 "$RT_WORKTREE/scripts/rt_dynamic_acceptance.py" --manifest-path "$RT_WORKTREE/Cargo.toml" --mode catalog --report /tmp/manifold-rt-catalog.json
```

For other GPU phases replace the filter with the exact module in A0; do not run the whole suite by default. Use `scripts/codex_checks.py` for focused crate tests/clippy and `scripts/gpu_proofs_gate.py` for changed GPU paths. GPU proofs use cargo test, never nextest. Final app landing goes through `scripts/land_branch.py` and its required landing gate. Passed checks are not repeated without changed code/new evidence.

Lead ledger at final review must contain one row per A1–A9: implementing commit, exact command, test count, artifact/report, pass/fail/blocked, and remaining limitation. All correctness/resource/export rows must pass to close `BUG-e3p6.4`; live performance and L4 results are reported separately and honestly. Failures after two concrete attempts return evidence to the lead. No silent skips, fallback, ignored tests or fabricated measured results.
