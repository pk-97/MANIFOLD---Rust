# Test Suite Reduction — the cut list after the crate split

**Status:** PROPOSED · awaiting Peter's review of the cut list (section 10, decisions for Peter) · 2026-10-20 · Opus lane · no test deleted or changed yet.

<!-- index: Design pass for BUG-hkbdp.1 (test suite review after the crate split): inventory of every test binary by crate from the 2026-10-20 nightly, the cut list (measurements out, duplicate sweeps merged, setup shared), the before and after contract map, and the stated reduction. Water is out of scope. -->

BUG-hkbdp.1 (test suite review and reduction after the crate split) is a child of BUG-hkbdp (renderer crate split epic).
It asks for focused tests at the new crate boundaries, a stated cut in test count and GPU leg time, and no public
contract left without a test. Its own rule holds here: this is the design pass, and Peter reviews the cut list
before anything is deleted.

What it means for the show: the landing gate is the wait between a fix and the rig. A shorter gate gets a fix onto
main sooner; a lost contract test lets a broken instrument through. Every row below keeps its contract.

The honest headline. Outside water, little duplication is left: earlier sweeps and BUG-hkbdp.6.11 (water test cut)
already took the slop the bead was opened for. The count cut is small (15 tests). The real win is time: four
measurements that run as tests, one duplicate sweep, and three suites that pay cold setup again and again inside the
serial GPU test group, which sets the nightly test wall.

## 1. Sources and method (2026-10-20, read-only, nothing built or run)

| Source | What it gives |
|---|---|
| Nightly `.claude/orchestration/trunk-health/2026-10-20.log` on origin/main 7742ea27d (an ancestor of this lane's tip 783e0ca35) | Every default test with its nextest time; the `gpu_proofs_gate.py --all` leg per binary: test names, counts, wall time, the 10 slowest |
| `.claude/orchestration/landing-gate-timings.jsonl` | Landing leg durations since 2026-10-11 (after the water cut) |
| `scripts/gpu_test_times.json` (committed, 2026-10-06) and the learned cache in the git common dir (2026-10-17) | Per-test GPU seconds |
| `.config/nextest.toml`, `scripts/gate_policy.py`, `scripts/gpu_scope.py`, `scripts/cpu_scope.py` | How tests are selected and which run serially |
| `rg` over `crates/` | 8,149 literal `#[test]` declarations in 1,225 files (includes feature-gated perf, oracle and automation tests) |
| `docs/evidence/simplification_census_2026-10/REPORT.md` | The rules for tests that guard the wrong thing |

Nextest times come from a contended nightly (all cores, every test its own process). They rank tests reliably;
they are not isolated costs. Every saving below is an estimate on that basis. One-off parsing scripts lived in the
lane scratchpad; no repo tool counts tests per crate (`landing_metrics.py` reports only the slowest tests).

## 2. Inventory

### 2.1 Totals now

| Measure | Now |
|---|---|
| Default suite (nightly `cargo nextest run --workspace`) | 6,526 tests, 614.5 s wall, 5,488 s summed; 5,978 under 1 s, 460 at 1–10 s, 65 at 10–30 s, 23 at 30 s or more |
| GPU-only tests (exist only under `gpu-proofs`) | 1,373 |
| Nightly GPU leg (`gpu_proofs_gate.py --all`) | 2,090 s (35 min): about 470 s build, 1,618 s test wall over 5,114 executions |
| Of those executions, default-suite tests rerun serially | about 3,715 (the leg runs `cargo test -p <pkg> --features gpu-proofs`, every target) |
| Landing GPU leg since 2026-10-11 (33 runs) | median 159 s, p90 359 s, max 984 s; its build leg median 29 s |
| Serial nextest `gpu` group (`.config/nextest.toml`, one thread) | 132 tests, 600 s back to back: this group, not the parallel pool, sets the nightly wall |

### 2.2 By crate (nightly 2026-10-20)

Default = nextest tests and summed seconds. GPU-only = tests only the GPU leg compiles, and that leg's wall time
for the crate's binaries (CPU reruns included).

| Crate | Binaries | Default | Default s | GPU-only | GPU leg s | Contract groups (largest first) |
|---|---|---|---|---|---|---|
| manifold-ui | lib + 3 | 1,011 | 17 | 0 | — | graph canvas, param card, scene setup panel, interaction overlay, inspector, text edit |
| manifold-app | 3 bins + 12 tests | 815 | 1,612 | 129 | 271 | UI bridge projection and inspector, frame plan, perform mode, content thread; renderer contracts; liquid conformance (water) |
| manifold-core | lib + 6 | 610 | 43 | 0 | — | effects, project, layer, flatten, audio mod, clip, graph def; docs guards |
| manifold-node-engine | lib | 609 | 29 | 70 | 1 | scene, execution, freeze, validation, substeps, WGSL compute, expand, param binding |
| manifold-playback | lib + 11 | 568 | 20 | 0 | — | modulation, engine, automation, session, Ableton bridge, transport sync |
| manifold-nodes | lib + main + gpu_proofs | 502 | 1,152 | 415 | 1,076 | the bundled catalog: node graph catalog tests (65 modules), freeze, load, scene loop, scene modifiers; GPU render and RT proofs; water contracts |
| manifold-editing | lib + 4 | 490 | 44 | 0 | 1 | graph commands (225), automation, effects, groups, presets |
| manifold-nodes-scene | lib + main + gpu_proofs | 477 | 63 | 330 | 71 | scene primitives, glTF import and load, scene VM, decode cache, gizmo; render scene and RT proofs |
| manifold-nodes-image | lib | 236 | 90 | 148 | 3 | image primitives, text rasterizer; codegen value proofs |
| manifold-io | lib + 6 | 147 | 12 | 0 | — | migrations, collect, loader, project folder |
| manifold-physics | lib | 142 | 2 | 0 | — | input, clock, coupled motion, stepping |
| manifold-audio | lib | 110 | 547 | 0 | — | analysis, trained detectors (parity, measure), capture |
| manifold-fluids | lib + 1 | 105 | 1,735 | 0 | — | the pinned FLIP Fluids CPU reference: coupling, frame, mesh, whitewater |
| manifold-gpu | lib + 1 | 91 | 41 | 33 | 5 | trace planner, raytrace, frame fence, queue, FFT; replay and template proofs |
| water leaf crates (gpu-flip, liquid, rigid, gpu-mpm, surface, whitewater) and manifold-nodes-water | 7 libs | 474 | 26 | 218 | 181 | water, owned by BUG-hkbdp.6.11 |
| manifold-foundation, media, ui-paint, compositor, profiler, native, led, spectral, recording | 12 | 139 | 56 | 30 | 7 | settings, ids, units, device guard; export; UI paint proofs; presentation curves |

### 2.3 Where the time goes

- **Nightly default suite (614.5 s wall).** Two chains decide it. The serial `gpu` group runs 600 s back to back, and
  14 of its 132 tests take 454 s: the four viewport demos in the app (`viewport_p5c_demo`, `viewport_p6_demo`,
  234 s), the nine loop wrap parity tests (201 s) and `scene_loop_probe` (19 s). The other chain is one test:
  `manifold-fluids` `production_coupling_neutral_body_uses_queued_gravity_prediction` (484 s), part of 1,507 s of
  CPU coupling tests in that crate.
- **Nightly GPU leg, test wall 1,618 s, estimated split:** water about 800 s (BUG-hkbdp.6.11 territory, including
  `matter_look_volume_drift`, 211 s, already nightly-only); the glTF sample sweep 249 s (runs only on glTF paths);
  the two bundled-preset sweeps the bead keeps (1080p chain build and 256 px first frame) about 100 s; default-suite
  reruns about 150 s; everything else about 320 s. The cut list works on the last bucket.
- **Default-suite reruns inside the GPU leg** (about 3,715 tests, about 150 s) are seen and left: serial
  `cargo test` runs them fast, and they are the only run of those tests with the `gpu-proofs` features unified on.

### 2.4 Slow, cause unknown (not cut; listed so nobody guesses)

| Group | Nightly s | What is unknown |
|---|---|---|
| manifold-nodes-image `text_rasterizer::tests` (9) | 61 | 6–8 s each for small rasters; font system start per process is the suspect. Needs `cargo nextest run -p manifold-nodes-image text_rasterizer` in isolation |
| manifold-app `content_thread::tests` (8) | 70; 56 s each in loaded landings | real-time waits versus contention. Needs an isolated run of that module |
| manifold-app `ui_color_swatches` in renderer_contracts (6, serial group) | 44 | device start per test or render size. Needs an isolated run |
| manifold-audio `kick_fed_pair_ignores_push_size`, `parity_snare`, `parity_kick` | 102, 81, 38 | whole golden clips through the detectors; fixture length is the cost (decision 6) |

## 3. Placement: what each crate keeps

Rule: a crate keeps tests on its public contract; an internal gets a test only where the contract cannot reach
it (a WGSL kernel against its CPU reference, a solver step, a parser). By that rule the unit tests in every crate
stay: they sit in the owning crate, nearly all run under a second, and no test was found pinned to an internal the
split moved. The `testkit` features (`manifold_gpu::testkit` used from 190 files, `manifold_node_engine::testkit`
from 164) are shared device and harness support, not reach-ins.

| Crate | Stays | Moves or changes |
|---|---|---|
| manifold-nodes | catalog-wide tests that need every leaf crate linked: bundled preset sweeps, validation, fusion census, card bindings | measurements leave (section 5); the duplicate generator sweep merges |
| manifold-nodes-scene | scene primitives, glTF, scene VM, its gpu_proofs | gains the viewport gizmo and orbit demos if decision 4 says yes |
| manifold-app | app behaviour: UI bridge, frame plan, content thread, export, renderer contracts that need the app | the two viewport demo modules test only nodes-scene functions (decision 4) |
| manifold-nodes-image | primitives and codegen value proofs | two mirror tests of an engine type go (section 5) |
| manifold-audio | parity and measure tests | the cost print leaves for an example |
| every other crate | everything | nothing |

Deferred with a trigger: about 30 render-scene and RT proof modules in `crates/manifold-nodes/tests/gpu_proofs/`
test scene contracts that `crates/manifold-nodes-scene/tests/gpu_proofs/` also hosts. Moving them changes no count
and no time; it changes what a scene-only landing selects. Trigger: BUG-bkfv2 (split bulky UI-flow and GPU-proof gate scopes)
measures a scene landing that pulls the manifold-nodes GPU binary for them.

## 4. Decisions (the rules the cut list applies)

- **D1. A test asserts.** A test that only prints a number is a measurement. It moves to an `examples/` program
  in its crate (precedents: `crates/manifold-audio/examples/detector_fires.rs`,
  `crates/manifold-nodes/examples/physics_benchmark.rs`), and the design docs that cite it name the example.
  Examples are not run by nextest or the GPU leg.
- **D2. One sweep per question.** Two tests over the same corpus through the same call merge into one that keeps
  every assertion of both.
- **D3. Setup is paid once per test.** A test that opens a new GPU device for every frame, or a suite that loads
  the same 2,928-clip fixture once per test, shares it. Assertions, fixtures, sizes and frame counts stay. The
  precedent is the liquid clock proofs in the BUG-hkbdp.1 notes: one device per test, 574 s to 233 s, nothing dropped.
- **D4. Value-level GPU proofs stay.** The fused-against-unfused oracle proofs and library sweeps
  (`docs/FREEZE_COMPILER_MAP.md` section 10 (test surface and how to debug)) and every kernel-against-CPU-reference
  proof are untouched. `docs/MANIFOLD_GPU_ARCHITECTURE.md` sets no test-tier rule beyond those. No GPU proof
  was found that only checks host arithmetic. A text scan flagged 359 GPU-only tests with no readback call in their
  own body; the sample opened (the compositor presentation tests) runs its shader and reads back through a helper.
  The rest are unverified: P1 opens every flagged non-water test over 1 s (five, the two C3 tests among them) before it lands.
- **D5. Water is out of scope.** BUG-hkbdp.6.11 cut the water half with Peter's approval. Nothing here re-cuts it;
  the residue it did not census is decision 7.
- **D6. No new framework.** Existing nextest filters, `gpu_scope.py` rows, examples. No `#[ignore]` (hook-banned),
  no new feature flag (the census names tests behind features no gate runs as a pattern to remove).
- **D7. Every cut shows its net.** A shrink is proven by the kept test going red on code with the contract broken
  (the red-first rule), then green, with before and after times from one gate run.

## 5. The cut list

Kinds: EXAMPLE (a measurement leaves the suite for an example), DELETE, MERGE, SHRINK (same assertions, setup
shared), RECORDS (timing tables). Savings are nightly seconds unless marked.

| # | Kind | File : test | Why nothing is lost | Saves |
|---|---|---|---|---|
| C1 | EXAMPLE | `crates/manifold-audio/src/trained/tests.rs` : `detector_cost` | prints ms per second of audio, asserts nothing; DETECTOR_RUNTIME_DESIGN.md section 7 (invariants and enforcement) and ROLL_DETECTOR_DESIGN.md section 10 (invariants and enforcement) already say timing is not gated and record release numbers by hand | 115 s default, on every manifold-audio landing |
| C2 | EXAMPLE | `crates/manifold-nodes/tests/gpu_proofs/encode_replay.rs` : `encode_replay_probe` | prints CPU and GPU ms with replay on and off; its one `expect` (the region records) is covered by `encode_replay_parity` and `encode_replay_survives_changes`; ENCODE_REPLAY_DESIGN.md lists it as a report | 20 s GPU |
| C3 | EXAMPLE | `crates/manifold-nodes/tests/gpu_proofs/cinematic_scene_tail.rs` : `import_tail_frame_cost_reported_at_1080p`, `import_tail_frame_cost_reported_at_4k` | assert only that the fixture exists and the timing is finite; the contract (byte-clean passthrough at a neutral lens) stays in `import_tail_is_byte_clean_passthrough_at_neutral_lens` | 6 s GPU |
| C4 | DELETE | `crates/manifold-nodes/src/catalog_gen.rs` : `block_is_deterministic` | `regenerates_in_sync` compares a fresh generation with the committed `docs/node_catalog.json` and `docs/NODE_CATALOG.md`, written by an earlier process: determinism across processes, the stronger check | 22 s default |
| C5 | MERGE | same file : `block_carries_markers_and_every_registered_node` into `regenerates_in_sync` | the completeness loop moves into the in-sync test, so one generation serves both; the marker check is already the `inject` expect there | 13 s default |
| C6 | DELETE | `crates/manifold-nodes-image/src/node_graph/primitives/clip_trigger_cycle.rs` : `sequential_trigger_counts_pass_through_unchanged`, `would_be_repeat_advances_by_one` | they call the engine's `ClipTriggerCycle::step` through the field ("mirror the legacy ones"); the same two cases are tested on the type in `crates/manifold-node-engine/src/clip_trigger.rs`. The primitive's own contract, `clear_state_releases_the_cycle_idempotence_cache`, stays | under 1 s |
| C7 | MERGE | `crates/manifold-nodes/tests/gpu_proofs/smoke.rs` : `every_registered_generator_runs_without_panicking_or_nans` into `crates/manifold-nodes/src/bundled_generator_presets.rs` : `every_bundled_preset_executes_one_frame` | the same generator catalog, the same `PresetRuntime::render` of one default frame; smoke adds a finite-pixel readback, the sweep adds panic collection. The sweep gains the readback and keeps 256 px and its failure list | 22 s GPU |
| C8 | SHRINK | `crates/manifold-nodes/tests/gpu_proofs/node_error_status.rs` : `node_error_fails_the_frame` | proves a node error fails the frame by building and running all of WaterDamBreakGpuFlip for one frame (52–56 s, nearly all pipeline build). The smallest graph holding the domain node with Simulation Speed 5 asserts the same `FrameRenderStatus::Failed(NodeError)` | about 50 s GPU |
| C9 | SHRINK | `crates/manifold-nodes/tests/scene_loop_wrap_parity.rs` : all 9 tests | `render_frame` opens a new GPU device and registry for every frame: 23 frames, 201 s, in the serial group. One device per test keeps all nine purity and continuity assertions and the 64 px frames | about 100 s default wall, estimate; needs one run |
| C10 | SHRINK | `crates/manifold-app/src/viewport_p5c_demo.rs`, `crates/manifold-app/src/viewport_p6_demo.rs` : 4 tests | phase acceptance demos (REALTIME_3D P5c, P6) that assert rendered pixels change; 234 s in the serial group, and they write six PNGs to /tmp on every run. Keep the pixel asserts; share the device; write PNGs only when an artifact directory is set (the `MANIFOLD_FOG_ARTIFACT_DIR` precedent in the BUG-hkbdp.1 notes) | unknown, needs an isolated run of those modules |
| C11 | MERGE (decision 5) | `crates/manifold-app/tests/legacy_param_id_resolution.rs` : the seven `liveschool_*` tests into one | each loads the canonical Liveschool fixture again (about 10 s a load, 70 s); one load serves all seven checks, every failed check is collected and reported. Costs the per-check names in the test list | about 58 s default |
| C12 | RECORDS | `scripts/gpu_test_times.json` and the learned cache | 634 of the committed 3,420 rows name tests that did not run tonight (moved water tests still keyed `manifold-nodes-water/lib`, dead targets such as `manifold-nodes/led_preset_value_tests`). The learned cache holds 4,825 unqualified and 1,868 stale rows of 10,567. Prune both to live keys; this changes only hang-watchdog allowances | none; stops wrong allowances |

Checked and kept: the two bundled-preset sweeps (the bead's own note keeps both); the `card_binding_shadow_corpus`
pair (different questions: the shipped set, and the fused view mapping back); `serialized_param_value_round_trips_every_variant`
in core and in node-engine (serde shape versus the `ParamValue` conversion); the same-named `free_copy_is_exact` in
the app and nodes-scene (command versus math); the playback unit and integration pairs
`guard_latches_and_holds_through_tempo_changes` and `stall_skips_and_stays_in_time` (one tests the type, one the
engine through it); the compositor presentation GPU tests (they run the tonemap shader).

## 6. Contract map, before and after

| Public contract | Before | After |
|---|---|---|
| Detector cost under its ceiling (measured, never gated) | `detector_cost` (prints) | an example in manifold-audio; the two design rows name it |
| Encode replay cost report | `encode_replay_probe` (prints) | an example in manifold-nodes |
| Encode replay output matches replay off, survives changes, stays off under profiling | `encode_replay_parity`, `encode_replay_survives_changes`, `replay_off_under_profiling_and_dump` | unchanged |
| Cinematic import tail is a byte-clean passthrough at a neutral lens | `import_tail_is_byte_clean_passthrough_at_neutral_lens` and two cost reports | the passthrough test; cost moves to an example |
| Node catalog docs match the registry, deterministically and completely | 4 tests, 6 generations | `regenerates_in_sync` with the completeness loop, `palette_visible_nodes_have_complete_descriptors` |
| Clip trigger never repeats an index | engine: 3 tests on `ClipTriggerCycle`; image: 2 mirrors and `clear_state_releases_the_cycle_idempotence_cache` | engine: 3 tests; image: the clear-state test |
| Every bundled generator renders a finite first frame without panicking | the smoke sweep and `every_bundled_preset_executes_one_frame` | `every_bundled_preset_executes_one_frame` with the finite readback |
| Every bundled generator builds its chain at 1080p | `every_bundled_preset_chain_builds` | unchanged |
| A node error fails the frame | one frame of the full dam break preset | one frame of the smallest graph with the failing domain node; same assertion |
| Loop wrap purity and continuity (SCENE_LOOP INV-3, corridor D8) | 9 pixel tests, a device per frame; buffer-level gate in `crates/manifold-nodes-scene/src/node_graph/primitives/scene_array.rs` | the same 9 tests, one device per test; buffer gate unchanged |
| Viewport orbit drag, pick highlight, gizmo modes, gizmo drag render | 4 demos, a device per test, PNGs always | the same 4 asserts, shared device, PNGs on request; home per decision 4 |
| Canonical fixtures resolve every driver, envelope, Ableton and macro mapping to a stable param id | 7 Liveschool tests, 7 loads (plus 2 other tests) | per decision 5: 1 test, 1 load, 7 checks (the 2 others unchanged) |
| Fused output matches unfused; kernels match CPU references | the freeze oracle proofs, codegen and kernel proofs | unchanged |

## 7. The reduction, stated

| Measure | Now | After (C11 included / excluded) |
|---|---|---|
| Default tests | 6,526 | 6,515 / 6,521 |
| GPU-only tests | 1,373 | 1,369 |
| All tests | 7,899 | 7,884 / 7,890 |
| Default summed seconds, firm rows (C1, C4, C5, C11) | 5,488 | about 5,280 / 5,340 |
| Serial `gpu` group (sets the nightly wall) | 600 s | about 500 s from C9 alone; C10 unknown, needs one run |
| Nightly default wall | 614.5 s | bounded below by the 484 s fluids test until decision 7; the serial group stops being the longer chain once C9 and C10 land |
| Nightly GPU leg | 2,090 s | about 1,990 s (C2, C3, C7, C8: about 98 s, 5 percent) |
| Landing GPU leg | median 159 s | the always-run smoke set is unchanged; C7 and C8 save 22 s and about 50 s on landings that select them |

Unknown without a build: the exact saving of C9 and C10 (needs
`cargo nextest run -p manifold-nodes --test main -E 'test(/scene_loop_wrap_parity/)'` and the same for the two app
demo modules, before and after); whether C8's minimal graph reaches the same validation (needs the red-first run of
D7); the cause of the section 2.4 rows.

## 8. The plausible-wrong turns, by name

- **"Shrink the fixture to make it fast."** No. C8 changes the graph, not a resolution; C9 and C10 share setup and
  keep sizes and frame counts. A resolution or duration cut is a decision for Peter, never a lane's.
- **"Mark the slow ones `#[ignore]` or put them behind a feature."** No (D6). Ignores are hook-banned and the census
  names tests behind features no gate runs as a pattern to remove.
- **"Collapse the GPU sweeps into one."** No. The 1080p chain build and the 256 px first frame are kept by the bead's
  own note; only the third sweep with the same question merges (C7).
- **"Move the measurement into a nightly-only test."** No. A test that never fails is not a test anywhere; it is an
  example (D1).
- **"Cut the water rows too while we are here."** No (D5).

## 9. Phasing

- **P1, one lane: C1–C7, with C12.** Deletes, merges and examples; the two detector design rows and the
  ENCODE_REPLAY_DESIGN.md deliverable line name the examples; both timing tables pruned to keys that ran in the
  latest nightly, which relearns. Gate: scoped nextest for the touched modules; `gpu_proofs_gate.py` with filters
  `bundled_generator_presets::`, `encode_replay::`, `cinematic_scene_tail::`; `cargo build -p manifold-audio -p manifold-nodes --examples`;
  `rg` for each removed test name returns only the examples and docs.
- **P2, one lane: C8–C10, and C11 if approved.** Each shrink shown red-first (D7), then green, with before and
  after times from one gate run. Gate: the named tests, one GPU run each through `scripts/gpu_queue.py`.

## 10. Decisions for Peter

1. Approve the cut list C1–C10 and C12 as written (recommended).
2. C1–C3: measurements become examples (recommended), or are deleted outright.
3. C7: fold the finite-pixel check into the 256 px first-frame sweep and delete the smoke sweep (recommended).
4. C10: move the four viewport demos from the app to manifold-nodes-scene, where their subject lives (recommended), or shrink them in place.
5. C11: merge the seven Liveschool fixture checks into one load (saves about a minute, loses per-check names), or keep seven tests.
6. Trimming the long golden clips in `kick_fed_pair_ignores_push_size` and the parity tests to an excerpt is not proposed; say if you want it.
7. Water residue BUG-hkbdp.6.11 did not census: the CPU coupling tests in `crates/manifold-fluids/src/coupling/tests/gravity.rs` (three tests, 1,296 s, the longest single chain of the nightly) go to a water bead, not this cut.
