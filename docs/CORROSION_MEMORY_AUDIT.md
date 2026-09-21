# Corrosion memory audit — one Azalea modifier stack

<!-- index: Bounded Azalea/Vortex Fragments/Ordered Recon/Math View memory audit; measured allocation classes, source-derived costs and unverified recovery candidates. -->

**Status:** Audit complete, 2026-09-21; no allocation change implemented or validated. Follow-up: BUG-dl16.

The smallest GPU-memory candidate is to reuse identical cut remaps before
fusion. One redundant first-stage mesh set costs **192.16 MiB per Azalea
instance**. This is an estimated saving, not a measured improvement. General
buffer lifetime reuse is a larger follow-up, not necessary for this first change.

## Scope and evidence

Source: `bdcebebc3`. Project: Corrosion Music Video V9 - MathView,
SHA256 `e306503153eb32e7bb6e3995b8b1059be9787288e38eb4824417c99f3a35d0bc`.
Settings: 1080×1920, 24 fps, 164 BPM. Representative layer `94f645a8`,
clip `f1a62b3a` at beats 65–78. Authored order: **Vortex Fragments → Math
View → Ordered Recon**; layer effect: EdgeStretch. The other two Azalea
layers have the same modifier types, with independent parameters/state.

No new playback, rendering, compilation or GPU sweep was run. This audit
reanalyses the existing diagnostic trace and reads the current source.

- Clean verification remains authoritative for its measurement: 19.784 s
  loading, peak Metal allocation 20,497,104,896 bytes, tracked residency
  19,831,374,424 bytes/1,290 allocations, 0/191 intervals late by over 1 ms.
  These are overlapping GPU measures, not additive process RAM figures.
- Existing **earlier diagnostic reproduction**, PID 14345: reconstructed
  resource-event peak **20,525,875,200 bytes** (19.117 GiB), comprising
  9,888,972,800 buffer bytes and 10,636,902,400 texture bytes. This is a
  different run and accounting surface; it does not replace the clean peak.
- Extraction resolves XML references and uses raw byte values, tracks live
  resource IDs through allocations/deallocations, and filters by process.
  Raw capture: `/tmp/manifold-corrosion-remaining-gpu-20260921/allocations.xml`.
  Reproducible extraction and JSON results:
  `/tmp/manifold-corrosion-memory-audit-20260921/{analyze.py,findings.json,trace-resources-at-30s.json}`.
- Resource labels are anonymous. Sizes/counts below are measured; attribution
  to graph roles is source/size inference, not a per-owner allocation dump.
  CPU heap, physical footprint, compression, and exact free-pool occupancy
  were not measured. No visual or performance gain is claimed.

## What the stack retains

The three source meshes declare 933,594, 896,157 and 728,799 records:
2,558,550 total. `MeshVertex` is 64 bytes. Each fragment stage adds 196,608
reserved records **per mesh**, independently of the current visible fragment
count. This increases each complete mesh set by 36 MiB per stage.

| Resource | Size for this three-mesh stack | Interpretation |
|---|---:|---|
| Original geometry | 163,747,200 B / 156.16 MiB | Required immutable reference; three source outputs |
| Geometry after Vortex cut | 201,495,936 B / 192.16 MiB per set | Source plus fixed fragment reserve |
| Geometry after Ordered Recon cut | 239,244,672 B / 228.16 MiB per set | Two accumulated reserves; final geometry must reach raster/RT consumers |
| First/second cut maps | 48.04 / 57.04 MiB | 16-byte provenance records; topology and remap dependencies |
| Cut count/prefix scratch | About 6.53 / 8.04 MiB | Formula from `scratch_bytes`; plus allocator rounding |
| Source CPU cache and staging | About 156.16 MiB each | CPU `cached_verts`, shared Metal staging, and output coexist; CPU cache is outside Metal totals |
| Full-resolution RGBA16 output | 15.82 MiB payload; 15.94 MiB observed allocation | Final output, effects and presentation need textures; not all are interchangeable |

The trace has **five buffers at each first-stage mesh size and five at each
second-stage size, repeated in three loading clusters**. Their rounded Metal
sizes total **6,611,435,520 bytes** across the three Azalea instances. The
corresponding unrounded source estimate is 2,203,703,040 bytes (2.052 GiB)
per instance for these ten mesh sets alone. This includes outputs and
intermediates; it is not all recoverable scratch.

Essential additional resources include material images/mips, environment
lighting, scene color/depth/velocity, RT acceleration structures and histories,
and Math View's sparse geometry, diagram/background and trail history. Math
View borrows parent scene buffers and depth; counting them again as copies is
wrong. Its trails and RT temporal state must survive according to their
existing contracts. Vortex/Recon geometry operations themselves are analytic;
their cached cut/remap results are recomputable but currently avoid repeated work.

Across the whole diagnostic run, 15 retained 4096² RGBA16 textures account for
2,684,436,480 bytes and 15 RGBA8-sRGB textures for 1,014,497,280 bytes. At peak,
342 full-resolution RGBA16 textures account for another 5,715,394,560 bytes.
These measured groups show that geometry is not the entire problem. They do
**not** establish which textures are essential, duplicated, or free in pools.

`MetalBackend::pre_bind_array` pins every array allocation; `release` ignores
pinned resources. Ordinary intermediate arrays therefore persist until runtime
rebuild/drop despite the execution plan having last-use information. Texture
slots retain their high-water backing, and a separate device pool caches up to
128 textures with frame-age protection. Neither count cap nor allocation
residency measures unused capacity. The existing trace cannot quantify free-pool
recovery; assign **no claimed saving** to that category yet.

## What another modifier costs

There is no universal per-card cost. For this embedded stack, adding the
second fragment stage introduces a larger map and multiple full mesh outputs.
The second-stage size classes represent **1,196,223,360 bytes (1.114 GiB)** of
mesh payload per instance, plus roughly 57.04 MiB map storage, 8.04 MiB cut
scratch, weight streams, uniforms and downstream RT changes. This is a
source-derived component estimate, **not a measured add/remove delta**: adding
a card can also alter fusion, liveness, Math View exports and rebuild overlap.
Non-fragment modifiers need not incur these reserves or copies.

Structural editing prepares candidates while live resources remain. Current
admission includes current device allocations plus fresh candidate storage;
the temporary edit peak can exceed the final steady-state increase. Turning
an enabled parameter off does not by itself remove the prepared capacity.

## Recovery estimate and smallest useful change

`fragment_cuts::apply` creates distinct current/reference remaps unconditionally.
`align_mesh` has an existing `(source address, map)` reuse cache, but the directly
created remaps do not populate it. At the first stage, current and reference
resolve to the same source, and later alignment can request that same pair again.

Reuse that existing remap cache for direct remaps, keyed by exact source/map
identity and compatible output type, before fusion. Start with the identical
first-stage current/reference pair: one avoided mesh set is **192.16 MiB per
instance**, or **576.48 MiB** if all three instances retain the same opportunity
after compilation. Broader duplicate alignment removal has a source-shaped
upper candidate of `2 × first-stage set + second-stage set` = **612.48 MiB per
instance**. Treat that larger figure as a hypothesis pending a compiled
allocation census; fusion and exported consumers can change the result.

A separate small CPU opportunity is releasing `cached_verts` after successful
staging upload, about **156.16 MiB per instance**. This requires explicit
loaded/count state because today's empty-vector check controls readiness.
It does not save Metal allocation bytes, and reduced physical footprint is
unverified. Do not combine it with GPU savings as though both were measured RAM.

First implementation should prove shared remap identity, unchanged capacities
and content revisions, differing-input isolation, and equal rendered geometry
with animated cuts/masks and Math View. Then measure one equivalent bounded
window's allocations and 24-fps intervals. Preserve residency leases, GPU
retirement and the three completed telemetry/scheduling/residency fixes.

General scratch reuse would additionally need to respect held/memoized outputs,
feedback, exported Math View dependencies, RT consumers, GPU completion and
cross-frame writes. Retain the existing planning/pool seams; do not reclaim
resources merely because the CPU has finished encoding their producer.

Project duration is not the direct multiplier for these geometry costs: these
are per-runtime resources. More distinct prepared content, layers and states
still increases retention. This audit does not establish duration-independent
RAM, safe eviction/lookahead budgets, or guaranteed frame-time improvement.

## Source anchors

All paths below are under `crates/manifold-renderer/src/`, except the last:

- `generators/mesh_common.rs`: `MeshVertex` stride.
- `node_graph/primitives/mesh_cut_map.rs`: `map_capacity`, `scratch_bytes`.
- `node_graph/scene_modifier_expand/fragment_cuts.rs`: `apply`, `align_mesh`.
- `node_graph/resource_allocation.rs`: `plan_array_allocations`.
- `node_graph/metal_backend.rs`: `pre_bind_array`, `release`, `clear`.
- `node_graph/execution_plan.rs`: `free_after`, held/persistent resources.
- `node_graph/primitives/gltf_mesh_source.rs`: `cached_verts`, staging upload.
- `preset_runtime/math_view.rs`: shared resources, variants, presentation.
- `node_graph/scene_modifier_expand/buffer_budget.rs`: candidate admission.
- `crates/manifold-gpu/src/metal/texture_pool.rs`: cap, report, recycling.
