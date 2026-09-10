# Scene modifiers — validation and resource contract

<!-- index: Shared numeric, migration, UI, GPU and performance gates for scene modifier milestones; planned test names and fixture budgets. -->

**Status:** PROPOSED · 2026-09-10 · Codex lead · test additions not implemented.
**Prerequisites:** Each tested phase's implementation. This document itself requires only reference/diff checks.
**Execution contract:** [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8, with current AGENTS.md bounded-check and reporting rules taking precedence over older broad-sweep instructions.

## 1. Audit

Existing precedents: `crates/manifold-renderer/tests/scene_modifier_inv_gate.rs`, `scene_loop_wrap_parity.rs`, `scene_loop_roundtrip.rs`; per-primitive codegen `gpu_tests`; `scripts/gpu_proofs_gate.py:172` supports `--manifest-path` and `--filter`; existing `cargo xtask ui-snap gltfscene` and semantic flows. Test names introduced by this programme are planned deliverables, never evidence of a passed check today.

## 2. Decisions

**D1:** Structural tests compare parsed IDs, paths, bindings and values. Numeric GPU tests compare geometry to independently calculated reference values. Render tests compare independently routed baseline/modified geometry, depth and IDs. No screenshot-only oracle for mathematical correctness.

**D2:** Every introduced invariant gets its check in the same phase. Save/reload tests perform a second gesture after reload. Performance controls must change the effective result without graph rebuild or cold pipeline creation.

**D3:** Fused and unfused paths share authored definitions but are independently executed. Do not compare two aliases of the same output resource. Always include a positive nonzero case so two inert paths cannot pass parity.

**D4:** Budgets below are proposed admission/test budgets, not measured claims about this Mac. Record hardware, resolution, raster/RT mode, warmup, triangle/instance counts, GPU bytes and CPU/GPU timing with every measurement. No "millions of anything" marketing promise follows from instancing alone.

## 3. Fixture matrix

| Fixture | Construction | What it proves |
|---|---|---|
| Single primitive | Cube at nonzero translation, rotation and nonuniform object scale | World/local distinction and pivot handling |
| Multi-object | Three different meshes, duplicate display names, nested groups | Stable identity, target selection, reorder and independent motion |
| Dense copies | Low-poly tetrahedron at 1,024 then 16,384 instances | Array math and capacity without expensive source mesh |
| Loop corridor | Stock Loop, pattern length >1, camera crosses a window boundary | Semantic copy IDs, wrap and shared phase |
| Scan | Existing licensed test scan plus a held-out GLB not used during development | Materials, imported topology, bounds and preserved source appearance |
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

For V6 use deterministic direct lighting and fixed samples/seed, not noisy beauty-frame equality. Report excluded silhouette pixels and cap them at 1% of evaluated pixels; exceeding that is a failed/inconclusive proof, not permission to mask more. Geometry/depth/shadow positivity checks prevent an empty render passing. RT support cannot be labelled verified if only raster checks ran. Motion vectors require comparing current/previous projected positions for moving geometry; reset histories on seek/discontinuity through existing lifecycle.

Existing Loop exact seam tolerance is preserved where stricter than V3. New mathematical trigonometric or normal calculations use stated tolerance; do not silently weaken an old bit-exact contract. Reference reconstruction endpoint takes an exact source-data branch to avoid normal/UV drift.

## 5. Admission and timing budgets

M1: maximum 16 modifier entries and 256 selected object bindings per scene. Dense fixture: 16,384 tetrahedron instances, 1,024×1,024 output. M3 initial mesh fixture ceiling: 250,000 triangles. M4 recorded-transform history: 64 MiB per modifier, maximum 256 MiB across a scene, with integer overflow checks and explicit admission errors. These limits are not guarantees all admitted combinations hit frame rate.

Run one bounded prepared sequence of 240 frames per relevant fixture, no optional soak. Report median/p95 CPU and GPU frame duration and modifier delta against the same base scene. Proposed acceptance: total measured CPU content-frame time ≤20 ms for every steady-state frame (existing design-standard gate), p95 total GPU frame ≤16.67 ms on the designated 60 Hz reference setup, and p95 added GPU cost ≤2 ms for the dense wave fixture. Preparation frames are reported separately; no hiding first live cold touches as preparation. If the baseline already exceeds budget, stop and record an inconclusive environment/base result. Do not lower samples/resolution/count invisibly to obtain green.

These budgets deliberately make throughput falsifiable. Failure returns to the lead to reduce published supported scope or change the implementation; changing a budget requires an explicit contract edit with measurement evidence. RT and large photoscans are measured separately; the tetrahedron result does not establish their throughput.

Use `MANIFOLD_RENDER_TRACE=1` for new content-thread work. Respect current GPU diagnostics and resource retirement; no global waits added merely to make tests pass. Asset preparation compiles/allocates ahead of performance. New runtime fields remain skipped from serialization.

## 6. Commands and phase deliverables

Run from a leased slot; set `MODIFIER_WORKTREE` to its absolute path. Examples are executable after the named test/flow has been delivered:

```sh
cargo test --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-core -p manifold-io scene_modifier_v3
cargo test --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-editing scene_modifier
python3 "$MODIFIER_WORKTREE/scripts/gpu_proofs_gate.py" --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" --filter scene_modifier_wave
cargo clippy --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-renderer --tests -- -D warnings
cargo run --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" --quiet -p manifold-app --features ui-snapshot,perf-soak -- ui-snap gltfscene --script "$MODIFIER_WORKTREE/scripts/ui-flows/scene-modifier-preset.json"
```

The explicit demo command expands the existing xtask alias while placing Cargo options before its `--`; do not append `--manifest-path` after `cargo xtask`, where the alias passes it to the app. The phase must document the exact successful command and output file. Runtime demo command for Peter is `cargo run --manifest-path "$MODIFIER_WORKTREE/Cargo.toml" -p manifold-app --release`; verify current binary selection/arguments before handing it over. Never give a main-checkout launch when the changes only exist in a slot.

CPU filters run on cargo test/nextest according to the current repo contract. GPU work always runs on cargo test through the proof gate; zero matching tests fails. Each phase runs its relevant subset once, then required landing checks. Two failed attempts stop speculative correction and return evidence to the lead. One named reproduction and one verification per visual/runtime fix; do not expand into a broad exploration.

## 7. Invariants and release reporting

V1–V8 are owned by F1–F8 as assigned in the foundation plan and by the later phase that first introduces the relevant data domain. New geometry cannot inherit a "verified" label from an unrelated earlier target type. Report L1 numeric checks, L2 captured/observed artifacts, L3 scripted interactions and Peter's L4 experience separately. Performance results are a measured table, not a green compile.

No extra committed status ledger is required: use existing design status headers, phase gates, git history and beads. Document discovered unfinished work or verification gaps in beads with the failing fixture/check, actual result and next bounded action. Existing required landing tools remain the authority.

## 8. Phasing

Validation implementation follows the phase introducing each invariant. F1 delivers roundtrip fixtures; F2/F3 identity/expansion tests; F4 legacy parity; F5 flow/counters; F7 numeric wave/geometry probes; F8 budget and cross-render proofs. Mesh, echo and splat contracts add their own oracles to this harness. This file specifies shared acceptance, not an independent broad test-sweep phase.

## 9. Decided — do not reopen

Numeric nontrivial oracles; held-out import; reload then modulate; explicit resource budgets; no GPU nextest; no broad optional sweeps; no runtime claims from static inspection.

## 10. Deferred

Long-show soak, hardware matrix and broad nightly health remain `scripts/trunk_health.py`/explicit qualification scope. They are not silently appended to every modifier phase.
