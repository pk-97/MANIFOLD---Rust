# Water optimisation lane — BUG-ddba and BUG-llkb

Slot: `/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0`.
Branch: `feat/water-optimisation-lane`; base HEAD `9d0ce1193`.
Prior BUG-a1xh/BUG-1nh4 work is committed as `c45462b04`; the Metal surface-shader fix is merged. This work is uncommitted. No git writes, pushes, app runs, file deletions or cleanup were performed. No redundant files identified; the dense shaders remain active proof oracles.

## BUG-ddba

The base already gates the scene-colour snapshot and mip generation on `draw.is_transmissive`. Retained that production gate and added a proof-only legacy schedule switch plus a CPU gate test. The GPU proof compares complete readback bytes for Blend panes in front of/behind transmissive glass and glass offscreen, at roughness 0 and 0.6. It has not established GPU bit equality yet.

Spray/bubble materials are unchanged. BUG-ddba reserves bubble appearance for Peter; `WATER_SIMULATION_DESIGN.md` describes embedded bubble meshes as localized optical density, and the preset uses that contract. The cited reference does not establish equivalent appearance from converting those draws to plain Blend. A material switch therefore has no parity basis under this request.

Files: `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs`, `primitives/mod.rs`; `crates/manifold-renderer/tests/gpu_proofs/render_scene_glass.rs`.

Checks completed before indexing: renderer cargo check, one CPU gate test, GPU proof no-run build with `gpu-proofs,fluid-perf-proofs,water-race-probes`, and renderer tests clippy with `gpu-proofs` all passed.

## BUG-llkb

Ported FLIP Fluids `polygonizer3d.cpp::_calculateVertexList` lower-endpoint U/V/W edge ownership into positive-axis edge counts and an inclusive prefix scan. Each crossed lattice edge has one compact vertex; emitted indices preserve the original triangle table and order. Boundary owner cells are clamped to the valid cell lattice. Interpolation, normals, UVs, colours and relaxation arithmetic are unchanged. Mesh Capacity and its growth/overflow policy are unchanged; index storage has the original triangle-list slot capacity. No resolution/quality cap or tuning was added.

`volume_surface_mesh.edge_scan` explicitly selects indexed output. Saved graphs without it retain triangle lists. Both relaxation passes address shared vertices using the same edge scan. `scene_object.indices` carries UInt32 topology to native indexed direct/indirect draws, including depth/volume passes; existing RT index-buffer support receives it. Topology/content keys include indices. Missing wired buffers report errors. The dam-break preset wires this chain. CPU extent admission includes the new count, scan and index storage.

GPU value proofs compare complete expanded triangle bytes in original and sorted order, f64 reference positions, winding, bounds, sharing, tails, overflow emptiness, live extents and two relaxation passes. Image proofs use production count/scan/mesh/relax/scene rendering on 8³ sphere and 16³ asymmetric fixtures, opaque and volume materials, require Complete frames, and compare with a hidden-object control. The paired perf proof compares sampled hashes and prints both timelines for the unchanged preset with indexed versus triangle-list topology. No successful GPU proof or performance measurement has run.

Files:
- `crates/manifold-gpu/src/metal/encoder.rs`: indexed draw variants and Metal encoding.
- `crates/manifold-renderer/src/node_graph/primitives/count_surface_edges.rs` and `shaders/{count_surface_edges_body,surface_edge_ownership,surface_edge_index}.wgsl`: edge identity/count/scan addressing and CPU/GPU proofs.
- `primitives/{volume_surface_mesh,relax_surface_mesh}.rs` and corresponding `shaders/*_body.wgsl`: indexed emission and relaxation.
- `node_graph/scene_object.rs`, `primitives/{scene_object,render_scene,live_draw_args}.rs`, `shaders/live_draw_args.wgsl`: topology transport, indexed rendering and indirect ABI.
- `node_graph/liquid/extent.rs`, `primitives/mod.rs`, `assets/generator-presets/WaterDamBreakGpuFlip.json`: admission, registration and preset wiring.
- `primitives/{liquid_surface_tests,liquid_bricks_tests,liquid_bricks_gpu_tests}.rs`; `tests/gpu_proofs/{main,liquid_indexed,gpu_flip_frame_perf}.rs`: value/image/perf proofs and retained dense-oracle ABI.
- `docs/GPU_FLUID_SURFACE_DESIGN.md`, generated node catalog, `scripts/gpu_scope.py`: contract/catalog/proof selection.
Paths abbreviated after `crates/manifold-renderer/src/` where appropriate.

Checks so far: both touched crates cargo check with all three proof features passed; edge ownership 4 CPU tests, mesh capacity/order 5, relaxation codegen 1 passed. Dense consumer WGSL validation and other three CPU brick checks passed. The preset extent check initially found the missing edge-count admission rule; after fixing it, its exact CPU test passes for 64/128 resolutions and 1/2/4 scales, frozen and unfrozen. Final builds, clippy, generated artifacts and CPU graph checks are now complete; see the continuation results below.

Validation incident: a too-broad `liquid_bricks::tests::` filter selected three nested GPU tests. They failed with `No Metal device found` before obtaining a device. This violated the no-GPU-test instruction; no GPU result is claimed. Subsequent CPU runs use exact filters. Do not rerun that broad module filter with `gpu-proofs`.

## Continuation results — 2026-10-03

Completed only the remaining validation and generation work. HEAD remains `9d0ce1193` on `feat/water-optimisation-lane`. No production source changes in this continuation, no GPU tests or app runs, no commits, pushes, git-index writes, file deletions, or cleanup. No redundant source files identified; the dense shaders remain active proof oracles. The slot remains assigned to the lead; no release or retirement was attempted.

Passed:
- Renderer GPU-proof no-run build with `gpu-proofs,fluid-perf-proofs,water-race-probes` (1m23s), using the exact corrected manifest path below.
- `manifold-gpu --features gpu-proofs --no-run` (6s), required because `metal/encoder.rs` is touched.
- Repository generator `gen_node_catalog` regenerated `docs/node_catalog.json` and `docs/NODE_CATALOG.md`: 365 registered nodes, including `count_surface_edges` and the indexed surface ports. The catalog freshness CPU test passes. The initial dev-profile generator build was intentionally interrupted when it began rebuilding dependencies; `--profile test` completed successfully.
- 72 CPU-only tests across the requested modules and catalog freshness, using the anchored nextest filter below. The proof features reuse the no-run artifacts; the filter excludes nested GPU modules and `render_scene::live_draw_args` GPU tests.
- Fused snapshot initially failed as stale (transcript `/tmp/manifold-water-fused-snapshot.log`). Regenerated through its test with `UPDATE_FUSION_GOLDEN=1`. The sole diff in `crates/manifold-renderer/tests/fixtures/fused_wgsl_snapshot.txt` is the WaterDamBreakGpuFlip node label changing from 82 to 84; WGSL body bytes are unchanged. The normal snapshot test then passed.
- Both new CPU graph checks in the GPU-proof integration binary passed: indexed liquid fixtures and indexed/unindexed perf variants. Together with the snapshot check, this makes 75 passing CPU checks, excluding the regeneration invocation.
- `cargo clippy -p manifold-gpu -p manifold-renderer --tests --features gpu-proofs -- -D warnings` (17s).
- `git diff --check`.

Reproducible completed commands (run from this slot; one Cargo command at a time):

```sh
export WATER_MANIFEST='/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0/Cargo.toml'
env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs,fluid-perf-proofs,water-race-probes --no-run
env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-gpu --features gpu-proofs --no-run
env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo run --manifest-path "$WATER_MANIFEST" -p manifold-renderer --profile test --features gpu-proofs,fluid-perf-proofs,water-race-probes --bin gen_node_catalog
env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo nextest run --manifest-path "$WATER_MANIFEST" -p manifold-renderer --lib --features gpu-proofs,fluid-perf-proofs,water-race-probes -E 'test(/^node_graph::primitives::(count_surface_edges|volume_surface_mesh|relax_surface_mesh|liquid_bricks|scene_object)::tests::[^:]+$/) | test(/^node_graph::primitives::render_scene::(tests|blend_snapshot_tests)::[^:]+$/) | test(/^node_graph::(liquid::extent|scene_object)::tests::[^:]+$/) | test(=node_graph::catalog_gen::tests::regenerates_in_sync)'
env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo nextest run --manifest-path "$WATER_MANIFEST" -p manifold-renderer --lib --test gpu_proofs --features gpu-proofs,fluid-perf-proofs,water-race-probes -E 'test(=node_graph::freeze::markers::tests::fused_wgsl_snapshot_unchanged) | test(=liquid_indexed::indexed_liquid_fixture_graphs_compile_on_cpu) | test(=gpu_flip_frame_perf::indexed_perf_variants_compile_on_cpu)'
env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo clippy --manifest-path "$WATER_MANIFEST" -p manifold-gpu -p manifold-renderer --tests --features gpu-proofs -- -D warnings
```

## GPU-capable lead commands

Run from slot-0. None of these commands was executed in this continuation. BUG-a1xh was previously reported passing in commit `c45462b04`; its exact command is retained below for the lead. BUG-1nh4 seam timing, BUG-ddba glass parity, and BUG-llkb value/image/performance evidence remain owed. The value filter selects exactly the three new indexed value proofs. The image filter includes its harmless CPU graph check. Perf commands require all three features.

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0'
export WATER_MANIFEST='/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0/Cargo.toml'

# BUG-a1xh
scripts/gpu_queue.py --label BUG-a1xh-empty-frame -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::fluid_surface::tests::fluid_empty_particle_frame_has_complete_zeroed_storage -- --exact --nocapture --test-threads=1

# BUG-1nh4 seam perf (water-race-probes)
scripts/gpu_queue.py --label BUG-1nh4-shipped -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs,fluid-perf-proofs,water-race-probes --test gpu_proofs gpu_flip_frame_perf::gpu_flip_frame_perf -- --exact --nocapture --test-threads=1
scripts/gpu_queue.py --label BUG-1nh4-whitewater-minimum -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs,fluid-perf-proofs,water-race-probes --test gpu_proofs gpu_flip_frame_perf::gpu_flip_frame_perf_whitewater_minimum -- --exact --nocapture --test-threads=1
scripts/gpu_queue.py --label BUG-1nh4-water-unwired -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs,fluid-perf-proofs,water-race-probes --test gpu_proofs gpu_flip_frame_perf::gpu_flip_frame_perf_water_unwired -- --exact --nocapture --test-threads=1

# BUG-ddba glass
scripts/gpu_queue.py --label BUG-ddba-snapshot -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs --test gpu_proofs render_scene_glass::blend_snapshot_elision_is_bit_exact -- --exact --nocapture --test-threads=1

# BUG-llkb value/image proofs
scripts/gpu_queue.py --label BUG-llkb-edge-values -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::count_surface_edges::gpu_tests::generated_count_values_match_the_cpu_fixture -- --exact --nocapture --test-threads=1
scripts/gpu_queue.py --label BUG-llkb-mesh-values -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs --lib fluid_indexed_ -- --nocapture --test-threads=1
scripts/gpu_queue.py --label BUG-llkb-indirect-args -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs --lib node_graph::primitives::render_scene::live_draw_args::tests::live_draw_args_are_whole_live_triangles_within_capacity -- --exact --nocapture --test-threads=1
scripts/gpu_queue.py --label BUG-llkb-image -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs --test gpu_proofs liquid_indexed:: -- --nocapture --test-threads=1

# BUG-llkb paired perf
scripts/gpu_queue.py --label BUG-llkb-paired-perf -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path "$WATER_MANIFEST" -p manifold-renderer --features gpu-proofs,fluid-perf-proofs,water-race-probes --test gpu_proofs gpu_flip_frame_perf::gpu_flip_frame_perf_indexed_parity -- --exact --nocapture --test-threads=1
```

BUG-ddba and BUG-llkb remain open pending GPU evidence. No landing or commit is authorized in this lane. Remaining work belongs to the GPU-capable lead: run the grouped proofs, assess results, then review and commit.
