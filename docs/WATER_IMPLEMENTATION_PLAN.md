# Live Water — Sol and Luna implementation briefs

<!-- index: Bounded implementation assignments and proof checkpoints for the MLS-MPM pool-and-cube prototype; Sol owns integration, Luna owns specified mechanical scopes. -->

**Status:** PROPOSED execution plan · 2026-09-09 · Astra. S1–S8 not implemented.
**Implementation epic:** `BUG-vglg`; update this work item as proof checkpoints land.
**Prerequisites:** [WATER_SIMULATION_DESIGN.md](WATER_SIMULATION_DESIGN.md), authoritative for the decisions below.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) sections 5–6 and 8. Current AGENTS.md execution limits override older broad-sweep instructions.

Sol High owns the workstream, diagnoses failures, reviews edits and lands. Luna Low
implements independently bounded assignments with the exact brief and relevant source,
not the full chat. Seat mapping (Astra review 2026-09-09): Sol is the k3 lead seat
in this repo's fleet; Luna lanes are K2.7, two concurrent maximum; Astra reviews
escalations only. Peter judges visual realism. Do not wait for Astra on
routine naming, private helpers, test implementation or a straightforward repair that
preserves the contract.

## 1. Audit and workstream setup

Design base `fd0dd5a96`, inspected 2026-09-09. Before implementation, record current
`origin/main`, verify the slot's base and re-find the symbols in design section 1.
Read `.codex/README.md` and `.claude/GIT_TREE_DISCIPLINE.md`; do not modify Claude
configuration. App changes use `scripts/agent-worktree.py acquire` and one leased slot
for the workstream. Two Luna lanes may share that slot only with disjoint file
ownership; Sol owns module declarations, registry joins and public seam changes.
Workers never commit/land or spawn workers. Do not prewarm a GPU build during S1's
CPU-only work.

Recommended concurrency:

| Wave | Sol | Luna A | Luna B |
|---|---|---|---|
| A | Review numerical formulation | S1 reference/proof | none |
| B, after S1 | Implement/review shared scheduling seam | S2 specified scheduler tests | S3 clock plumbing, after Sol pins shared types |
| C | Review shader values and resource ordering | S4 transfers/stress | S6 surface operations, using synthetic particle fixtures |
| D | S7 scene integration | S5 collision/events | none: avoid concurrent render_scene edits |
| E | Integrate, measure, prepare Peter's demo | S8 preset/roundtrip tests | only a separately diagnosed mechanical fix |

These are eight assignments, not eight simultaneous agents or a delivery-time
estimate. S2 and S7 are lead-owned architecture-sensitive work; Luna receives exact
mechanical portions after Sol's read-back, not “figure out the graph runtime”.

## 2. Committed seams and inventory

Existing signatures remain unchanged unless a row explicitly adds to them.
No new dependencies, public threads/channels or shared locks are authorised by this
plan. New source modules/types below are specified work, not permission to redesign
neighbouring systems.

| Seam | Old → new | Inventory/re-derivation |
|---|---|---|
| `EffectNode` and `Primitive` | Add default `substep_boundary() -> Option<SubstepBoundaryPorts>`; blanket forwarding | `effect_node.rs`, `primitive.rs`; `rg -n 'impl.*EffectNode|impl.*Primitive' crates/manifold-renderer/src/node_graph` identifies direct implementations, which retain None. |
| Execution plan | `steps` unchanged; add region metadata/accessor and contraction-aware ordering | `execution_plan.rs::compile`; `rg -n 'plan.steps\(\)|late_capture_steps|hoistable_steps|persistent_resources' crates/manifold-renderer/src` inventories consumers; each must honour repeated-region lifetime or remain frame-only. |
| Executor | Existing public `execute_frame_with_state` retained; add simulation-frame setter; extract ONE existing step evaluator for outer/repeated traversal | `execution.rs::execute_frame_inner`, `compute_live_steps`; `rg -n 'execute_frame_inner|fn execute_|late_capture' crates/manifold-renderer/src/node_graph/execution.rs`. No duplicate alternate executor. |
| Freeze | Preserve membership; never fuse across region border | `node_graph/freeze` classification/partition, lowering and generated step replacement. Re-derive `rg -n 'ExecutionPlan|ExecutionStep|compile\(' crates/manifold-renderer/src/node_graph/freeze`. |
| Host simulation clock | Add `simulation_epoch` getter/explicit increment to PlaybackEngine; add setters on GeneratorRenderer/PresetRuntime/Executor | `engine.rs::seek_to`, project replacement and Play external-alignment branch in `content_commands.rs`; generator call at `content_pipeline.rs`; export contexts at `content_export.rs`. |
| Headless/warmup host | Existing render calls unchanged; precede with explicit SimulationFrame | `PresetRuntime::render` callers in `src/bin/render_generator_preset.rs`, generator warmup and tests. Re-derive `rg -n 'PresetRuntime|\.render\(' crates/manifold-renderer/src/bin/render_generator_preset.rs crates/manifold-renderer/src/generator_renderer.rs`. |
| Scene input ports | Add the five optional water inputs from design section 7; existing object_N ports unchanged | `primitives/render_scene.rs` input declarations/evaluate/snapshot creation/draw/depth output; SceneVm continues tracing existing scene objects. |
| Surface filter | Add optional coverage and value-space enum; unwired/default retains old algorithm | `primitives/bilateral_blur.rs`, body shader and its existing reference tests. |
| GPU gate selection | Preserve existing selections, include water proof when render_scene water work lands | `scripts/landing_gate.py::GPU_PROOFS_SCOPE` currently maps render_scene to `rt_` only; it would miss water tests without an explicit additive selection. |

Inventories are intentionally symbol-based for S2 onward: S1 lands first and later
work may share trunk with other changes. Before the affected phase, Sol records the
fresh file:line call-site list in its task, classifies any new consumer, and stops only
for a conflicting seam. Do not blindly substitute guessed field offsets or stale
line numbers. Existing scripts supply landing mechanics; no new orchestration scripts.

### 2.1 Region result ports (part of the committed scheduler ABI)

Use the complete `SubstepBoundaryPorts` and `SubstepResultPorts` definitions in
design section 4; this section specifies their water bindings.
`WaterState` declares `collider_in -> collider_out` (Transform) and
`status_in -> status_out` (Channels<u32>, capacity 1). Its primary `in -> out`
remains the WaterParticle buffer. All captures are state-capture inputs and contribute
to region derivation; only final primary/result outputs escape. Result storage persists
when step_count=0; initial collider comes from required `collider_seed: Transform`,
status initially zero. This closes the final-visible-cube/diagnostic seam without
allowing arbitrary intermediate reads. Additional boundary results are type-checked,
not hard-coded WaterState names in the executor.

Only the clock boundary and event/collider primitives own per-owner CPU state.
Reset clears their region state together. `WaterColliderMotion` exports the accepted
transform each iteration into `collider_in`. Status-in is the final alias of the
sticky GPU word. Boundary run/late-capture uses existing GPU copy/slot operations;
do not read that status synchronously to decide how many iterations to schedule.

### 2.2 Stage port contract

`P=Channels<WaterParticle>`, `A=Channels<i32>`, `G=Channels<WaterGridCell>`,
`S=Channels<u32>` capacity 1, `T=Transform`, scalar controls are ScalarF32.
Names below are node type IDs without `node.`. Every solver stage gets a `step_dt`
dependency; outputs inherit particle capacity except explicit grid/status outputs.
No shader stage allocates buffers or fetches host data itself.

| Operation | Inputs → outputs | Dispatch and ownership |
|---|---|---|
| seed_water | build-time lattice/domain/capacity → `out:P` | Pure source, generated per-element code. Zero inactive slots; memoized until seed config changes. |
| water_state | `seed:P, in:P, collider_seed:T, collider_in:T, status_in:S`, controls → `out:P, collider_out:T, status_out:S`, clock scalars | CPU clock + persistent boundary; capture inputs break the dependency cycle. |
| water_emit | `in:P`, rate, step time/index → `out:P` | One elementwise spawn operation. Per-owner cursor/fraction resets with boundary. Births use deterministic ordinal/lattice, no GPU append/readback. |
| water_impulse | `in:P`, trigger count, centre/radius/vector, step index → `out:P` | Event latch plus one elementwise velocity change; never resets existing particles. |
| clear_grid | region `step_dt`, configured dimensions → `out:A` | One grid clear dispatch. `4*nx*ny*nz` capacity. |
| mpm_scatter_mass_momentum | `particles:P, accumulator:A, status:S` → `out:A, status_out:S` | Atomic output aliases accumulator and status wires; sequential graph ordering. |
| mpm_scatter_stress | `particles:P, accumulator:A, status:S` → `out:A, particles_out:P, status_out:S` | Reads completed grid mass; writes density to particle copy and adds stress momentum. Mass is never modified by this stage. |
| water_collider_motion | target `T`, step time/index/count → `transform:T, velocity:Vec3 scalar` | CPU interpolation from prior accepted target; no GPU allocation. |
| mpm_grid_velocity | `accumulator:A, collider:T, collider_velocity`, gravity → `out:G` | One grid resolve/force/boundary operation. |
| mpm_gather_advect | `particles:P, grid:G` → `out:P` | Gather kernel; generated codegen path, BufferGather. |
| water_collide_box | `in:P, collider:T, collider_velocity`, basin bounds → `out:P` | One elementwise projection/relative-velocity operation. |
| water_validate | `particles:P, status:S` → `out:S` | Global fault OR; status aliases input. |
| water_commit | `accepted:P, candidate:P, status:S` → `out:P` | One final copy/select dispatch after validation; accepted immutable until this stage completes. |

The entire row chain feeds `water_state.in`; final status and collider feed its
result captures. For the next iteration, accepted/candidate buffers swap roles only
after the commit dependency, and `out` exposes the accepted result. Copy-based first
implementation is acceptable; unsafe in-place mutation is not. Ordinary `array_feedback`
retains its old frame-late semantics.

`mpm_scatter_stress` has one invocation per particle but atomic grid output and
particle output: extend the existing generated scatter wrapper to emit its additional
coincident output if current codegen lacks that combination. This is a specified
compiler seam, not permission to hide all transfer stages in hand WGSL. S1/S4 include
the standalone mixed-output shader proof. Scatter uses existing atomic boundaries;
all barrier-free gather/pointwise operations must participate in freeze codegen.

## 3. Shared checks and execution budget

Commands below run from the leased worktree; set `WATER_WT` to the actual absolute
slot path. It is a task variable, never HOME or CODEX_HOME. New test names are
deliverables, not tests claimed to exist today. A filtered command that executes zero
matching tests is a failure; report the executed count.

```sh
cargo clippy --manifest-path "$WATER_WT/Cargo.toml" -p manifold-renderer --tests -- -D warnings
python3 "$WATER_WT/scripts/gpu_proofs_gate.py" --manifest-path "$WATER_WT/Cargo.toml" --filter water_
```

Use each phase's focused test first. Do not repeat passed checks without changed code
or new evidence. Required landing checks still run through `scripts/land_branch.py`,
which invokes `landing_gate.py`; never nextest for GPU proofs. Substep runtime changes
also require the existing feedback/freeze tests selected by the landing gate, not just
new water tests. No optional full-workspace sweep or perf soak. At two failed attempts
of a check, stop that lane and return evidence to Sol. An exact-command permit is only
for a necessary bounded check, never to reset the retry count.

Negative review, over the changed files only: no new Arc<Mutex>/Arc<RwLock>, native
Metal calls outside manifold-gpu, per-step heap allocation, runtime shader compilation,
unexplained ignored tests, velocity clamping or silent reset. These are diff checks;
do not demand zero matches across unrelated historical code.

## 4. Phase briefs

### S1 — Numerical recipe and atomic proof (Luna; Sol reviews)

**Entry/read-back:** design sections 2–5; `compute_common.rs`, `ports.rs` and
`freeze/codegen/standalone.rs`. Reconfirm 96-byte water layout and signed atomic
support. No app GPU work yet.

**Deliverables:** `node_graph/water.rs` records, coefficient helpers/constants and
tests; an f64 test-only reference in `node_graph/water/reference.rs` for weights,
P2G, stress and G2P. Keep production math specified in the design, not delegated to
the reference. Declare it under `#[cfg(test)]` in `water.rs` so the S1 lib test command
executes it; later GPU proof tests reuse that source via a test-only path module.
Sol owns the parent module registration. Pure Rust tests in `water.rs` establish mass/affine transfer, pressure
sign, guard-shell indexing, Q quantisation error and CFL arithmetic. Include the
density-dependent acoustic CFL `dt*(c(rho)+|v|)/h` with `c(rho)=c0*(rho/rho0)^3`,
and one fixture at 1.15*rho0 showing the default step above the 0.25 rest-density
guard. Include one
non-lattice particle fixture and negative velocities; use an analytically affine
velocity field so the reference cannot merely agree with itself.

**Gate:** `cargo test --manifest-path "$WATER_WT/Cargo.toml" -p manifold-renderer --lib water_`;
focused clippy. Expected all matching tests pass and measured quantisation error meets
design section 8. **Demo: none — L1.** No shader speed or realism claims.

**Stop:** if default encoding/formulation fails this proof, report the smallest failed
equation/fixture and measured error. Astra decides a correction; no lane parameter sweep.
**Forbidden:** importing a simulator dependency, writing a CPU shipping fallback,
substituting PBF, loosening tolerances. This is the first checkpoint, not a product build.

### S2 — Bounded graph substeps (Sol owns seam; Luna implements named tests)

**Entry/read-back:** S1 passed; design section 4 and plan section 2. Re-derive all
ExecutionPlan consumers and frame-late-capture paths. Read existing feedback and
persistent-slot tests end-to-end. Sol writes the actual inventory before dispatch.

**Deliverables:** `node_graph/substeps.rs`, trait/blanket forwarding, region compiler,
single extracted step evaluator, persistent region resources, WaterState boundary,
freeze membership preservation and mixed-output scatter codegen seam. Synthetic
test-only region uses a tiny array increment and an outside counter to prove order,
zero steps, 3 substeps vs 3 fixed frames, final output, result ports, reset, duplicate
frame, pause, skipped frame and overloaded clock. Test malformed nested/overlapping
regions, escaped intermediate wires and render nodes inside a region are rejected.

**Gate:** `cargo test --manifest-path "$WATER_WT/Cargo.toml" -p manifold-renderer --lib substeps_`;
`python3 "$WATER_WT/scripts/gpu_proofs_gate.py" --manifest-path "$WATER_WT/Cargo.toml" --filter substeps_`;
renderer clippy. Tests include `substeps_frozen_unfrozen_match` and
`substeps_no_recycle_between_iterations`. Existing feedback behaviour must remain
unchanged. **Demo: none — L1**, computed GPU array values only.

**Forbidden:** recursive whole-frame execution, rendering per substep, broad graph
serialization/group rewrite, memo-skipping changing state, aliasing candidate to
accepted, dynamic pipeline builds. If the generic region seam exceeds one session,
Sol may split declaration/compiler and executor/proof commits; no water shaders depend
on it until both pass. No architecture change is implied by that mechanical split.

### S3 — Host clock and lifecycle (Luna after S2 types are pinned)

**Entry/read-back:** design section 6, `generator_renderer.rs::render_all/stop_clip`,
`engine.rs::seek_to/stop/set_time`, warmup and export paths. This lane owns host clock
files only; Sol owns substeps.rs. Engine already provides `is_playing`; no UI model write.

**Deliverables:** engine simulation epoch; explicit frame setter plumbing through
content pipeline, generator renderer, preset runtime and headless renderer; lifecycle
tests named in design section 9. Epoch test covers user seek, export reseek, external
Play relocation and continuous sync correction. Stop preserves water. Source-media
loop is not a global seek. Gaps use per-instance last frame identity, not time deltas
accumulated while hidden. Fresh seed on generator replacement is explicit.

**Gate:** focused `water_` lib tests in `manifold-playback`, `manifold-renderer` and
`manifold-app` using the same `cargo test --manifest-path ... -p <crate> --lib water_`
form — except `manifold-app`, which is a bin-only crate: use
`cargo test --manifest-path ... -p manifold-app --bin manifold water_` there.
clippy `-p manifold-playback -p manifold-renderer -p manifold-app --tests`.
`water_lifecycle_seek_stop_export` must test both <1-beat and >1-beat seeks.
**Demo: none — L1** until S8 exercises the controls through the real app.

**Forbidden:** changing old effects' FrameTime.delta, resetting all legacy generators
on every clip edge, conflating effect-chain grace eviction with layer-generator state,
modifying sync arbitration or reconstructing past simulation during a live seek.

### S4 — GPU liquid steps (Luna; Sol owns joins)

**Entry/read-back:** S1 and S2 pass; design sections 3–5, plan stage table;
`ADDING_PRIMITIVES.md` codegen/install rules. Re-derive primitive vocabulary and
generated atomic wrapper before adding modules. S6 can run independently on fixture
particles; no shared shader/library file edits by both lanes.

**Deliverables:** seed, clear, P2G, stress, grid, G2P, validate and commit primitives;
WGSL bodies/full atomic kernels according to their actual fusion class; preallocated
status/readback ring; tests under `tests/gpu_proofs/water_*.rs`, joined by Sol in the
existing GPU proof harness. No cube/emitter production code owned here. Use static
basin boundaries for the first fluid proof. Tests: layout, analytic transfers, signed
momentum, forced overflow, candidate fault rejection and default static pool density.
Add `water_timestep_halving_stability`: the default pool and impact fixtures rerun
at dt/2 must agree with dt in density field, particle motion and settling outcome
within recorded tolerances. A mismatch is escalated numerical evidence for
Astra/Peter before S7, not a tuning task.

**Gate:** `gpu_proofs_gate.py --filter water_` with the manifest as above; renderer
clippy. Compare GPU output to f64/analytic expected values, not a duplicate WGSL oracle.
Default pool keeps all live particles and meets density bounds. Include mixed-output
stress and frozen/unfrozen end-to-end proof. **Demo: none — L1**; S7 supplies the image.

**Forbidden:** f16 physics, unbounded atomics retry without status/cap evidence,
unchecked conversion overflow, variable dt, artistic pressure/damping fixes hiding
instability, new GPU queue or synchronous live readback. One numerical failure returns
evidence; no fishing through constants to make a PNG attractive.

### S5 — Moving cube, emission and impulses (Luna)

**Entry/read-back:** S3/S4 passed, design section 6. Read Transform and existing trigger
latches. Sol supplies frozen type/port bindings and declared file ownership.

**Deliverables:** `water_collider_motion`, `water_collide_box`, `water_emit`,
`water_impulse`; authored target vs accepted collider transform wiring; bounded event
latches and deterministic birth cursor. Tests cover swept translation limited to
supported per-step displacement, relative velocity at contact, exact shared dimensions,
Full capacity, reset-dominates-trigger, no-substep pending event and no paused backlog.
Inactive particles stay inactive until born; no random replacement of pool particles.

**Gate:** focused `water_` lib tests plus GPU gate `--filter water_`; renderer clippy.
`water_cube_transform_and_collision_match` measures penetration <=0.1*h. Numerical
event test proves one trigger has the same total velocity effect at different
substep counts. **Demo: none — L1** until composed into S7/S8.

**Forbidden:** teleporting the collider, moving visible cube independently, cube
rotation/scale controls that physics ignores, physical force multiplied by dt twice,
scene rigid-body integration, new audio-event infrastructure.

### S6 — Surface reconstruction (Luna, independent of S4 after layout)

**Entry/read-back:** S1 layout, design section 7; Camera/depth helpers, BilateralBlur
and its tests. Synthetic particle slab/sphere fixtures avoid dependence on solver tuning.

**Deliverables:** particle depth/coverage raster, thickness raster, coverage-aware
linear-depth bilateral mode, normals-from-depth operation. All generated pure stages
ship fusion proofs. Camera and output dimension declarations are explicit; no texture
map can accidentally size the raster target. Tests cover empty pixels, a single sphere,
two separated depth layers, silhouette preservation and translated camera normals.

**Gate:** renderer `water_`/bilateral focused tests and GPU proof gate `--filter water_`;
renderer clippy. Slab thickness is within the declared sphere-splat approximation;
single sphere centre chord error <=2% at a fixture resolution of 256^2. Existing
bilateral defaults compare unchanged. **Demo: L2 artifact**, one contact sheet generated
by `water_surface_artifact` test, saved under `WATER_ARTIFACT_DIR`; Peter reviews with
S7. Automated checks remain numerical; no agent pass/fail based on image taste.

**Forbidden:** smoothing uncovered pixels into liquid, heightmap-normal substitution,
full scene lighting in the splat node, half-resolution optimisation before correctness.

### S7 — Scene integration and first visible water (Sol; mechanical Luna help only)

**Entry/read-back:** S4/S6 passed; design section 7 and current render_scene E2a
snapshot/Pass B/shadow/depth helpers. S5 is optional for the static integration proof,
required for the moving demonstration. Inventory all has_transmission branches,
snapshot allocations, pass ordering and public depth writes; narrow edit fence.

**Deliverables:** five water inputs, material/camera validation, snapshot condition,
depth-tested liquid shading and scene-depth update; existing lighting helper reuse;
unsupported-combination error tests; additive water GPU-gate selection. Static
fixture compares opaque foreground, partly submerged object and opaque object behind
water. No-water output must remain byte-identical. No water shader gets its own scene
camera/light state cache. Keep RenderScene final render ownership.

**Gate:** GPU gate `--filter water_` plus required existing render_scene/RT proofs at
landing, focused clippy. `water_scene_occlusion_and_depth` checks numeric clip depth
and region pixels; `water_scene_without_water_matches_existing` compares exact output.
`water_scene_rejects_unsupported_combinations` exercises EACH excluded setting.
**Demo: L2**, `water_scene_artifact` emits colour/depth/contact-sheet through the actual
graph and scene renderer into `WATER_ARTIFACT_DIR`, not a separate demo renderer.

**Stop checkpoint:** Peter sees the first integrated water. If the shape/motion is
unconvincing, Sol sends Astra this artifact plus scalar diagnostics; no extra beauty
passes or particle-count escalation to disguise the problem.

### S8 — Playable preset, round trip and measured prototype (Sol integrates; Luna packages)

**Entry/read-back:** S3–S7 passed; design sections 6–8; SceneStarter.json and current
parameter surface, graph-tool and UI-flow conventions. S8 owns preset/tests only;
any solver defect goes back as a bounded fix with evidence.

**Deliverables:** WaterPrototype.json with labelled Water/Surface/Scene groups,
existing cube/basin scene objects, parameter bindings and description of compatibility
limits. Defaults: emission off, impulse strength 0.5 m/s, cube stroke 0.25 m,
simulation speed 1, reset trigger 0. Cube drive uses existing beat/ease nodes; no
discrete position jump. Use the existing main preset picker; no separate Water window.
Save/reload + modulate test, adjacent-clip/gap/pause/reset flow, frame timing and
allocation report from production code. Headless tools receive the same clock contract
as the live path. Use existing stats/GPU timing facilities; do not measure GPU execution
by timing CPU submission alone.

**Checks (new commands are executable only after their named deliverables exist):**

```sh
cargo run --manifest-path "$WATER_WT/Cargo.toml" -p manifold-renderer --bin graph_tool -- validate "$WATER_WT/crates/manifold-renderer/assets/generator-presets/WaterPrototype.json" --kind generator
python3 "$WATER_WT/scripts/gpu_proofs_gate.py" --manifest-path "$WATER_WT/Cargo.toml" --filter water_
```

Confirm the existing graph_tool CLI spelling during read-back. The validation action
is the contract; do not introduce a second validator to preserve this command's spelling.
Extend the existing `render_generator_preset` tool with optional `--sequence-dir`
and `--schedule` flags. Sequence mode writes exactly `--frames` PNGs named
`frame_000000.png` onward; skip convergence polling, preserve existing final `--out`
behaviour, and use the existing fixed 60 Hz/120 BPM context. Schedule JSON is
`{"version":1,"events":[{"frame":60,"params":{"emissionRate":1000}}]}`:
ascending unique zero-based frames, exposed parameter keys and finite f32 values.
Validate the complete schedule before rendering; apply each event before its frame,
retain values until changed, and reject unknown keys/versions/out-of-range frames.
The preset's exact exposed keys are used in the committed schedule fixture.
Capture 600 frames: still 0–59, pour 60–119, stop pouring at 120, cube strokes
at 180/210/240/270 driven through eased targets, then settle through frame 599.
This mode calls the production PresetRuntime and clock setter, never a duplicate
simulation loop. Its blocking PNG readback is artifact generation, not a performance
measurement. Keep correctness tests enabled; no ignored correctness test substitute.

**Acceptance demo:** one ten-second 1080p/60 sequence with the design's still/pour/
four-strike/settle sequence, plus one short scripted lifecycle flow through the actual
app. The flow uses existing `scripts/ui-flows` infrastructure and asserts live parameter
values, not merely button existence. Measure the design's p95 GPU/memory targets in a
bounded production trace; report no-water baseline and incremental cost separately.
Target L3 for scripted controls, L4 for Peter's realism verdict. Provide the exact
worktree app launch command, built from its actual binary path, and a <=2-minute
click script. Do not claim L4 before Peter supplies the verdict.

**Performer gesture:** repeatedly strike the existing waves, pause, orbit the camera,
resume, then deliberately reset. Tests prove reset clears old momentum and reset's
parameter binding still works after project reload.

**Stop:** one reproduction and one verification for a named problem; two failed checks
return evidence. No broad render sweep. A missed performance target or an unobserved
visual verdict remains an open bead, never “prototype passed”.

## 5. Handoff and completion

Sol's first action is S1, not a full app build or simultaneous dispatch of eight lanes.
Each brief includes scope, established findings, reuse target, acceptance and exact
checks. Workers return edits, executed-test counts, errors and artifact paths; Sol
reviews the diff and owns commits/gates. Respect the slot build lock: two workers do
not run Cargo against the same target concurrently.

At a numerical/architectural escalation, send Astra only: decision that failed,
small reproducing fixture, measured result vs threshold, relevant files, and one
recommended correction. Avoid repeated full-history agent context. At the first scene
checkpoint, include motion artifact and timing; no routine Astra monitoring.

Implementation remains open until the requested prototype behaviour and its evidence
exist. Update design/phase status in the same verified landing. Track unsolved gaps in
beads and link the existing contracts; do not create a parallel daily status document.
Preserve unrelated work and commit exact paths only.

## 6. Decided and deferred

The architecture's section 11 governs settled choices. The architecture's section 12
is the single deferred-feature list. This plan does not add an independent backlog or
promise a second solver. Sol may split a mechanical assignment for context limits,
but must not omit its acceptance tests or turn an unspecified architecture choice into
a Luna task.
