# Scene modifiers — validation and resource contract

<!-- index: Shared numeric, migration, UI, GPU and performance gates for scene modifier milestones; planned test names and fixture budgets. -->

**Status:** APPROVED · 2026-09-12 · Codex lead · unified acceptance tests pending. Shipped photoscan evidence is recorded below; it does not qualify the future stack.
**Prerequisites:** Each tested phase's implementation. This document itself requires only reference/diff checks.
**Execution contract:** [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8, with current AGENTS.md bounded-check and reporting rules taking precedence over older broad-sweep instructions.

## 1. Audit

Existing precedents: `crates/manifold-renderer/tests/scene_modifier_inv_gate.rs`, `scene_loop_wrap_parity.rs`, `scene_loop_roundtrip.rs`; per-primitive codegen `gpu_tests`; `scripts/gpu_proofs_gate.py:172` supports `--manifest-path` and `--filter`; existing `cargo xtask ui-snap gltfscene` and semantic flows. Test names introduced by this programme are planned deliverables, never evidence of a passed check today.

**September 12 baseline:** the [photoscan landing report](landings/2026-09-11-photoscan-modifiers.md) records all ten landing checks green, nine focused Metal proofs, five recipe tests, four editing tests, the 34-step control flow and observed mushroom raster phase strips. Reuse `tests/photoscan_modifier_plans.rs`, editing `tests/scene_mesh_modifier_roundtrip.rs`, primitive `photoscan_modifier` tests and `scripts/ui-flows/scene-photoscan-modifiers.json`. The old UI fixture had no playing clip, so its black viewport only qualified controls. Peter subsequently praised all three looks and reported that LFOs appeared to work. That is user-observed visual/LFO evidence, not a measured audio, save/reopen or timing result. BUG-e3p6.5 retains those remaining checks; BUG-e3p6.4 owns dynamic RT.

## 2. Decisions

**D1:** Structural tests compare parsed IDs, paths, bindings and values. Numeric GPU tests compare geometry to independently calculated reference values. Render tests compare independently routed baseline/modified geometry, depth and IDs. No screenshot-only oracle for mathematical correctness.

**D2:** Every introduced invariant gets its check in the same phase. Save/reload tests perform a second gesture after reload. Performance controls must change the effective result without graph rebuild or cold pipeline creation.

**D3:** Fused and unfused paths share authored definitions but are independently executed. Do not compare two aliases of the same output resource. Always include a positive nonzero case so two inert paths cannot pass parity.

**D4:** Budgets below are proposed admission/test budgets, not measured claims about this Mac. Record hardware, resolution, raster/RT mode, warmup, triangle/instance counts, GPU bytes and CPU/GPU timing with every measurement. No "millions of anything" marketing promise follows from instancing alone.

**D5:** Separate technical authorship from stock curation. A file-only preset can prove the architecture without becoming a new stock modifier. V9 applies when promoting a new behaviour family. Waveform/seed/mask changes alone are variations; GPU code reuse is encouraged. Preserve existing Peel/Vortex cards during migration regardless of their shared operation.

## 3. Fixture matrix

| Fixture | Construction | What it proves |
|---|---|---|
| Single primitive | Cube at nonzero translation, rotation and nonuniform object scale | World/local distinction and pivot handling |
| Multi-object | Three different meshes, duplicate display names, nested groups | Stable identity, target selection, reorder and independent motion |
| Dense copies | Low-poly tetrahedron at 1,024 then 16,384 instances | Array math and capacity without expensive source mesh |
| Loop corridor | Stock Loop, pattern length >1, camera crosses a window boundary | Semantic copy IDs, wrap and shared phase |
| Scan | Existing licensed test scan plus a held-out GLB not used during development | Materials, imported topology, bounds and preserved source appearance |
| Shipped photoscan stack | Immutable v2 snapshots of Elastic, Peel and Vortex alone/together, with non-default controls and mappings | Migration appearance, current/reference order, exact bypass and post-reload modulation |
| Adversarial mesh attachment | Nested multi-material scan, punctuation in IDs, renamed handles, edited per-target body, unsupported transform route | Atomic diagnostics; no guessed coordinate frame, alias collision or partial migration |
| Animated mesh | Matched glTF animation fixture | Modifier order relative to skin/morph and reference time |
| Faces | Two adjacent triangles with UV seam and a degenerate triangle | Rigid face motion, cracks, degeneracy and reconstruction |
| Splats, later | Synthetic anisotropic splats plus held-out supported scan | Extent/orientation, masks, sort, mesh depth and return pose |

Generated fixtures live under `crates/manifold-renderer/tests/fixtures/scene-modifiers/` and must use legal distributable assets. Held-out asset is selected by the lead after worker development; record its hash/counts. No fixture is fetched or rendered for this documentation task.

## 4. Shared gates

| ID | Required check | Numeric acceptance |
|---|---|---|
| V1: persistence | schema + commands + migration | Parsed graph/bindings equal after roundtrip; undo/redo restores all addressed state; idempotent migration |
| V2: identity | duplicate/reorder/selection test | No changed surviving IDs or mappings; exact target set; reject missing explicit target |
| V3: math | `scene_modifier_math` GPU group | Finite positions; absolute error ≤1e-5 × max(1, reference extent) for position, ≤1e-5 for weights; exact identity bypass |
| V4: time | `scene_modifier_time` group | Same beat/seed gives same data after seek; periodic preset phase 0 and 1 equal; nonperiodic presets never advertised as looping |
| V5: compiler | `scene_modifier_fusion` group | Standalone/fused geometry within V3; bypass exact; positive nonzero result; binding fan-out exercised |
| V6: render | `scene_modifier_render` group | Same transformed geometry feeds raster/depth/shadow/RT; against baseline maximum geometric hit-position error ≤1e-4 × extent, excluding explicitly flagged silhouette ambiguity pixels |
| V7: controls | semantic UI flow + runtime counters | Amount/phase gestures change output; zero graph rebuilds and PipelineCompile events during prepared parameter gestures; survive reload |
| V8: resources | trace + allocation counters | Zero steady-state CPU allocations in new hot code; no capacity growth/pipeline creation during gestures; declared memory cap respected |
| V9: creative distinction | Candidate brief + bounded matched-scene gesture comparison | Recognisable surface action and gesture distinct from nearest existing family; pattern-only differences stay preset variations; record human judgement separately |

For V6 use deterministic direct lighting and fixed samples/seed, not noisy beauty-frame equality. Report excluded silhouette pixels and cap them at 1% of evaluated pixels; exceeding that is a failed/inconclusive proof, not permission to mask more. Geometry/depth/shadow positivity checks prevent an empty render passing. RT support cannot be labelled verified if only raster checks ran. Motion vectors require comparing current/previous projected positions for moving geometry; reset histories on seek/discontinuity through existing lifecycle.

**M1 renderer scope:** raster is required; enumerate and qualify the depth/shadow/motion-vector paths actually supported. Continuous dynamic mesh RT is excluded pending BUG-e3p6.4; verify the generic incompatibility diagnostic and preservation of saved render settings rather than implementing BLAS work in this milestone. A disabled toggle does not trigger capability reclassification/pipeline compilation during a gesture. Use a conservative topology-based admission rule while this mode is unsupported. Future RT work must pass V6 on the transformed geometry; raster evidence cannot waive it.

**Failure-class regressions:** verify actual registry ports (including patch `in`, not group `current`), scalar shadow bindings, duplicate group interfaces, stable IDs containing separators, and control handles without reserved separators. Test the full candidate before mutating graph or metadata. Multi-material frames share one radius and correct signed offsets; dimensional cell size is not multiplied by radius twice. Normals use the actual deformation Jacobian, tangents preserve handedness, UVs stay attached, disabled output preserves exact bytes, and a two-stage shear fusion proof executes both fused and standalone paths. Reuse the shipped atom oracles; the new tests target expansion/migration rather than restating shader code.

**V7 production journey:** create a playing scan clip; assign a Phase LFO, a driver and a deterministic audio input to independent controls/instances, save, close/reopen through production project IO, then change mappings and observe effective geometry. Verify numeric mapping state, independent IDs, undo/removal and zero structural rebuilds during live modulation. Report any unavailable audio input explicitly. Preparation-only controls reject those mappings before mutation. A controller-value assertion with a frozen/empty viewport is insufficient.

**V9 procedure:** the brief states action, signature gesture, neutral/return endpoint, nearest family, visible contrast and cost. Use the same photoscan/camera/material/lighting and comparable displacement bounds; one bounded gesture sequence per candidate, at most one correction/verification for a named failure. Assembly needs staged arrival, slicing coherent sections, and echoes multiple phase poses. Compare behaviour, not a beauty score. Existing three modifiers retain their positive user review; their migration requires parity, not a fresh novelty contest. Record unreviewed candidates as experimental.

Existing Loop exact seam tolerance is preserved where stricter than V3. New mathematical trigonometric or normal calculations use stated tolerance; do not silently weaken an old bit-exact contract. Reference reconstruction endpoint takes an exact source-data branch to avoid normal/UV drift.

## 5. Admission and timing budgets

M1: maximum 16 modifier entries and 256 selected object bindings per scene. Dense fixture: 16,384 tetrahedron instances, 1,024×1,024 output. M3 initial mesh fixture ceiling: 250,000 triangles. M4 recorded-transform history: 64 MiB per modifier, maximum 256 MiB across a scene, with integer overflow checks and explicit admission errors. These limits are not guarantees all admitted combinations hit frame rate.

For M1 mesh tests, also use the existing mushroom plus a held-out static scan within the 250,000-triangle qualification ceiling; do not decimate silently. Additional expansion limits: 65,536 generated nodes and 262,144 generated wires per owner graph, and 256 MiB of additional prepared modifier GPU buffers per scene. Use checked arithmetic for stage×target expansion and actual prepared layouts/liveness for buffer accounting, including retained reference and intermediate buffers. Count shared resources once; report baseline source/renderer memory separately. These are proposed admission ceilings, not throughput promises. Exceeding them rejects the candidate with requested/allowed counts before installation; existing ordinary graphs still load. Changing a ceiling requires measured evidence and an explicit contract edit.

Run one bounded prepared sequence of 240 frames per relevant fixture, no optional soak. Report median/p95 CPU and GPU frame duration and modifier delta against the same base scene. Proposed acceptance: total measured CPU content-frame time ≤20 ms for every steady-state frame, p95 total GPU frame ≤16.67 ms on the designated 60 Hz setup, and p95 added GPU cost ≤2 ms for the dense wave fixture. Photoscan results report each modifier and the three-stage stack against the same scan baseline, within the total-frame and memory ceilings; do not infer their cost from the wave. Preparation frames are separate; no hiding first live cold touches as preparation. If the baseline exceeds budget, stop and record an inconclusive base/environment result. Do not lower quality/count invisibly to obtain green.

These budgets deliberately make throughput falsifiable. Failure returns to the lead to reduce published supported scope or change the implementation; changing a budget requires an explicit contract edit with measurement evidence. RT and large photoscans are measured separately; the tetrahedron result does not establish their throughput.

Use `MANIFOLD_RENDER_TRACE=1` for new content-thread work. Respect current GPU diagnostics and resource retirement; no global waits added merely to make tests pass. Asset preparation compiles/allocates ahead of performance. New runtime fields remain skipped from serialization.

## 6. Commands and phase deliverables

Run from a leased slot; set `MODIFIER_WORKTREE` to its absolute path. Examples are executable after the named test/flow has been delivered:

```sh
cargo test --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-core -p manifold-io scene_modifier_v3
cargo test --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-editing scene_modifier
python3 "$MODIFIER_WORKTREE/scripts/gpu_proofs_gate.py" --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" --filter scene_modifier_photoscan_migration
cargo clippy --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-renderer --tests -- -D warnings
cargo run --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" --quiet -p manifold-app --bin manifold --features ui-snapshot -- ui-snap gltfscene --script "$MODIFIER_WORKTREE/scripts/ui-flows/scene-modifier-preset.json"
```

The explicit demo command expands the existing xtask alias while placing Cargo options before its `--`; do not append `--manifest-path` after `cargo xtask`, where the alias passes it to the app. The phase must document the exact successful command and output file. Runtime demo command for Peter is `cargo run --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-app --release`; verify current binary selection/arguments before handing it over. Never give a main-checkout launch when the changes only exist in a slot.

CPU filters run on cargo test/nextest according to the current repo contract. GPU work always runs on cargo test through the proof gate; zero matching tests fails. Each phase runs its relevant subset once, then required landing checks. Two failed attempts stop speculative correction and return evidence to the lead. One named reproduction and one verification per visual/runtime fix; do not expand into a broad exploration.

## 7. Invariants and release reporting

V1–V8 are owned by F0/F1–F8 and by the later phase introducing each data domain. V9 is required for new stock-family promotion and reviewed at F7/F8 without forcing a conformance variation into the stock catalog. New geometry cannot inherit verification from another target type. Report L1 numeric checks, L2 observed artifacts, L3 scripted interactions and Peter's playtest evidence separately. Performance results are measured tables, not green compiles.

No extra committed status ledger is required: use existing design status headers, phase gates, git history and beads. Document discovered unfinished work or verification gaps in beads with the failing fixture/check, actual result and next bounded action. Existing required landing tools remain the authority.

## 8. Phasing

F0 records baseline/reopen/resource evidence; F1 delivers roundtrip/mutability fixtures; F2/F3 identity/expansion/frame/admission tests; F4 all-five-kind migration parity; F5 playing-scene flow/counters; F6 catalog/deletion checks; F7 independent file authorship; F8 remaining budget and supported-render proofs. Mesh, echo and splat contracts add their own oracles when implemented. This is shared acceptance, not an independent broad sweep.

## 9. Decided — do not reopen

Numeric nontrivial oracles; held-out import; reload then modulate; explicit resource budgets; no GPU nextest; no broad optional sweeps; no runtime claims from static inspection.

## 10. Deferred

Long-show soak, hardware matrix and broad nightly health remain `scripts/trunk_health.py`/explicit qualification scope. They are not silently appended to every modifier phase.
