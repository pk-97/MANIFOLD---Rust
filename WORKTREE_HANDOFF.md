# Particle frame blending — P3

Branch `feat/particle-frame-blend`, base `31f69772772eb026270f0c140c45c5acfcf4d542`. No push or landing. BUG-upao remains open for integration and the later solver-rate control.

Commit is blocked by the sandbox: explicit slot `git -C ... commit` failed to create `.git/worktrees/slot-9/index.lock` with `Operation not permitted`. HEAD remains the base; new files were staged successfully, and tracked edits remain in the worktree. Run the exact pathspec commit below from an environment with Git metadata write access.

## Section 2.5 audit

Before adding atoms, surveyed all primitive purposes with `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g '*.rs'`. None of the three requested atoms existed. `array_math` has a CPU-only Mix; `blend_copies` blends another record type; `keep_in_box` handles analytic boundaries in a different space. Reused `FluidParticle`, generated standalone pipelines, the existing lattice convention, Liquid Surface and `particles_to_copies` instead of adding another particle record or presentation system.

## Identity blocker (protected solver scope)

GPU FLIP does **not** keep stable slots. `sort_particles_into_cells.wgsl::stabilise` copies every record word into cell order (the optional `order` output is additional, not the only permutation). `gpu_flip_step.wgsl::faces_to_particles` reads `sorted[idx]` and writes `particles_out[idx]`; later substeps and ticks consume that order. Using slot + 1 at publication would match different particles.

Initial `liquid_fill_body.wgsl` already assigns `id = idx + 1`, and the sort and advection preserve it. Two other breaks remain: `gpu_flip_step.wgsl::emit_write` assigns `slot + 1`, which can collide/reuse ids after deaths and compaction; `liquid_frame.wgsl` overwrites every published id with zero. Removing that overwrite alone is insufficient because the frame remains cell-sorted, while D11 requires strictly increasing ids.

Required integration changes, owned by the solver merge:

1. In `liquid_state.rs`, retain a GPU identity counter and identity epoch beside persistent particle state. Seed next-id above the fill's maximum id; reset with the domain epoch. Carry them across every tick and substep; never derive the counter from the current live count or compacted slot.
2. In `gpu_flip_step.rs` and its `gpu_flip_step.wgsl::emit_write`, bind that counter/state. Reserve an emission's deterministic id range using the existing emission scan: new id = next_id + accepted rank. Advance next_id once by accepted births, preserving existing record ids through sort, advection and removal. Before u32 exhaustion, renumber live records 1..n and bump identity epoch; prevent matching frames across that boundary. Keep the epoch wire within the exact f32 integer range (the seam's existing constraint).
3. In `liquid_frame.rs` / `shaders/liquid_frame.wgsl`, stop clearing live ids, and publish a compact **id-sorted copy**, not the solver's cell-sorted working buffer. Reuse the sort/scan machinery or an owned radix sort keyed by u32 id; do not CPU-read GPU particles or change solver order merely for presentation. Publish the live count and each frame's retained identity epoch (A's and B's independently). On epoch change collapse/reset the pair as the ring contract requires. Keep radius-zero unused slots outside the sorted live prefix.
4. Add proofs with reordered particles, deaths followed by emission, duplicate-id prevention, multi-substep ticks and epoch rollover. Verify published nonzero ids strictly increase and matching A/B ids identify the same trajectory. Only then can GPU FLIP exercise the Hermite branch. Until then this branch's preset invokes D11's explicit id-zero move-from-B path, including its radius growth; it is not verified smooth GPU FLIP water.

## Extent audit integration (protected `liquid/extent.rs`)

The new atoms also need entries in `LIQUID_EXTENT_RULES`, which this task was forbidden to edit. Add `node.interpolate_particle_frames`, `node.push_out_of_solid`, and `node.mix_arrays`:

- Interpolate: output covers B's full capacity (32 bytes per record); wired A covers its declared count, bounded by capacity; B covers count_b, with -1 meaning full capacity. Optional unwired A is legal. No summed A+B capacity.
- Push-out: output covers particle capacity; solid covers `nodes_x * nodes_y * nodes_z * 4`; require finite positive sizes and node counts >=2. Counts are rounded exactly as the shader does. Bounds are centre/size scalar wires.
- Mix: a and b are equal-length f32 arrays; output covers that length. Reject unequal input capacities explicitly. The fused capacity expression follows a.

Do not exempt these atoms from the audit or make a generic permissive rule. Extend the preset extent tests after registration. `graph-tool validate` establishes graph load/compile, not this separate extent audit.

## Optional gather fusion gap

BUG-adcx tracks the whitewater move-from-B chain: `freeze/region.rs::build_region` refuses an unwired gathered input with `required/gather input unwired`; `freeze/codegen/fused_buffer.rs` accepts only `External` for `BufferGather`. Thus an interpolation atom with A wired can fuse with push-out, but the designed A-unwired whitewater path currently runs generated standalone. It needs an explicit absent gathered-array read contract and a fused-vs-unfused proof, not a fabricated A frame or a boundary exemption. No compiler files were changed here.

## Presentation wiring

`WaterDamBreakGpuFlip.json` feeds interpolated then solid-clamped liquid particles to Liquid Surface; solid A/B blend feeds both push-out and the surface. Foam/bubble/spray presentation gets the same blend/span before `particles_to_copies`, with gravity only on spray's move-from-B path. Whitewater **simulation** remains inside the tick region; moving its inputs to the post-region frame would create a cycle and mix display time with solver time.

`WaterDamBreakParticles.json` shares that simulation/presentation graph and replaces the liquid surface with unit-radius icosahedron copies scaled to particle radius (the existing small-sphere mesh convention used by whitewater). Solver-rate and clock files are unchanged. CPU mesh obstacle/coupled-body presentation from the broader P3 text is outside this GPU FLIP blending request.

## Verification

Passed:

- `env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo check -p manifold-renderer`.
- Seven new CPU tests: interpolation 1, push-out 1, mix 3, cross-atom/preset 2. They validate generated WGSL, fusion capacities/partitioning and shared display-clock wiring.
- `env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings`.
- `env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --no-run -p manifold-renderer --features gpu-proofs` (all renderer proof/test targets compiled; no GPU execution).
- Catalog freshness: initially failed as expected, regenerated both catalog files, then `node_graph::catalog_gen::tests::regenerates_in_sync` passed (1 additional CPU test). Docs index regenerated with no change. `git diff --check` passed.
- `graph-tool fusion` on both touched presets. GPU FLIP: 2 regions, estimated 87 dispatches; flattened nodes 82 interpolate and 84 push-out share region 1. Particle view: 1 region, estimated 70 dispatches; nodes 502 interpolate, 504 push-out and 509 copies share region 0. Mix is isolated before gathered solid consumers; the mix→mix fusion proof covers its eligibility. Whitewater interpolation/copies remain standalone for the optional gather gap above. Reports: `/tmp/p3-gpuflip-fusion.json`, `/tmp/p3-particles-fusion.json`.

`graph-tool validate` and generator `check-presets` are **not CPU-only**: both construct `GpuDevice::new_queued` and require the GPU lock and Metal. They were not run under the CPU-only instruction. No Metal tests, app launch, capture or GPU performance measurement ran here. No redundant source files identified; no files deleted. Protected solver/clock/extent files are unchanged.

## Lead GPU commands

Run from this worktree after the solver identity and extent integration. Each command uses the shared GPU queue; no full renderer test sweep is needed:

```sh
scripts/gpu_queue.py --label 'P3 interpolate values and motion' -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::interpolate_particle_frames::gpu_tests:: -- --nocapture
scripts/gpu_queue.py --label 'P3 solid projection values' -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::push_out_of_solid::gpu_tests:: -- --nocapture
scripts/gpu_queue.py --label 'P3 array mix values and fusion' -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::mix_arrays::gpu_tests:: -- --nocapture
scripts/gpu_queue.py --label 'P3 fused particle display values' -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::particle_frame_blend_tests::gpu_tests:: -- --nocapture
scripts/gpu_queue.py --label 'P3 GPU FLIP graph validation' -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo run --profile test -p manifold-renderer --features gpu-proofs --bin graph-tool -- validate crates/manifold-renderer/assets/generator-presets/WaterDamBreakGpuFlip.json --kind generator
scripts/gpu_queue.py --label 'P3 particle graph validation' -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo run --profile test -p manifold-renderer --features gpu-proofs --bin graph-tool -- validate crates/manifold-renderer/assets/generator-presets/WaterDamBreakParticles.json --kind generator
scripts/gpu_queue.py --label 'P3 preset catalog validation' -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo run --profile test -p manifold-renderer --features gpu-proofs --bin check-presets
```

The four proof filters select exactly seven GPU tests:

- `fluid_interpolate_particle_frames_matches_cpu_f64_reference`
- `fluid_interpolated_motion_is_even`
- `fluid_push_out_penetration_bounded`
- `fluid_push_out_zero_gradient_is_finite_and_preserved`
- `mix_arrays_matches_cpu_formula_and_clamps_amount`
- `mix_arrays_mix_arrays_fused_matches_unfused`
- `fluid_particle_blend_fused_matches_unfused`

The interpolation/push/copies proof compares the fused result with actual standalone dispatches and independently computed positions/radii. Interpolation covers ids, births, epochs, endpoint/clamped time, zero span, unwired A and count tails. Push-out covers an affine plane, a 16³ sphere penetration bound, and undefined gradients. Device results remain unverified until those commands run.

## Pending commit

```sh
git -C '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-9' commit \
  -m 'Particle frame blending: interpolate GPU water particles between ticks' \
  -m 'Co-Authored-By: Codex gpt-6-astra <noreply@openai.com>' -- \
  WORKTREE_HANDOFF.md \
  crates/manifold-renderer/assets/generator-presets/WaterDamBreakGpuFlip.json \
  crates/manifold-renderer/assets/generator-presets/WaterDamBreakParticles.json \
  crates/manifold-renderer/src/node_graph/primitives/mod.rs \
  crates/manifold-renderer/src/node_graph/primitives/interpolate_particle_frames.rs \
  crates/manifold-renderer/src/node_graph/primitives/push_out_of_solid.rs \
  crates/manifold-renderer/src/node_graph/primitives/mix_arrays.rs \
  crates/manifold-renderer/src/node_graph/primitives/particle_frame_blend_tests.rs \
  crates/manifold-renderer/src/node_graph/primitives/shaders/interpolate_particle_frames_body.wgsl \
  crates/manifold-renderer/src/node_graph/primitives/shaders/push_out_of_solid_body.wgsl \
  crates/manifold-renderer/src/node_graph/primitives/shaders/mix_arrays_body.wgsl \
  docs/GPU_FLUID_SURFACE_DESIGN.md docs/NODE_CATALOG.md docs/node_catalog.json
```
