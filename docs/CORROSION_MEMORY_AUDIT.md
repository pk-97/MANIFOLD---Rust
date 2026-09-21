# Corrosion memory audit — one Azalea modifier stack

<!-- index: Bounded Azalea/Vortex Fragments/Ordered Recon/Math View memory audit; measured allocation classes, source-derived costs and unverified recovery candidates. -->

**Status:** Cut-remap reuse, CPU source-copy release, conservative temporary-array reuse and pool telemetry implemented and measured, 2026-09-21. Broader memory investigation remains open: BUG-dl16.

Reusing identical cut remaps before fusion reduced Corrosion's measured peak
Metal allocations by **1,926,807,552 bytes (1.794 GiB, 9.40%)**, with 0/191
intervals late by more than 1 ms in the same eight-second headless window.
This is a GPU allocation saving, not a measured process RAM reduction. A second
bounded batch below adds temporary-array reuse, with only a small additional
Metal saving in this scene, and reports process memory separately.

## Scope and evidence

Source: `bdcebebc3`. Project: Corrosion Music Video V9 - MathView,
SHA256 `e306503153eb32e7bb6e3995b8b1059be9787288e38eb4824417c99f3a35d0bc`.
Settings: 1080×1920, 24 fps, 164 BPM. Representative layer `94f645a8`,
clip `f1a62b3a` at beats 65–78. Authored order: **Vortex Fragments → Math
View → Ordered Recon**; layer effect: EdgeStretch. The other two Azalea
layers have the same modifier types, with independent parameters/state.

The initial audit below reanalysed the existing diagnostic trace and read the
source without a new GPU run. The subsequent implementation and its one
bounded verification are recorded separately below.

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

## Initial recovery estimate and smallest useful change

Before this change, `fragment_cuts::apply` created distinct current/reference
remaps unconditionally. `align_mesh` had an existing `(source address, map)`
reuse cache, but directly created remaps did not populate it. At the first stage, current and reference
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

## Implemented reuse and bounded verification

Direct fragment remaps and later alignment now use the same preparation-time
cache, keyed by exact source node/port, cut-map ID and mesh/scalar kind. The
runtime remapper still tracks mesh/map content revisions. No shader, array
capacity, residency lease, GPU retirement or temporal-state policy changed.
Focused structural tests pass for direct-to-alignment reuse, repeated-key
reuse, preparation idempotence and source/port/map/type isolation.

One release-build verification used the unchanged project hash above, retained
disk caches, start beat 56 and eight seconds at 24 fps. Source diff, binary hash,
command, complete logs and comparison are retained at
`/tmp/manifold-corrosion-remap-reuse-20260921/verification/`; the tested executable
is `/tmp/manifold-corrosion-remap-reuse-20260921/manifold`.

| Measurement | Earlier clean baseline | With remap reuse |
|---|---:|---:|
| Peak Metal allocated bytes | 20,497,104,896 | 18,570,297,344 |
| Warmup tracked residency bytes | 19,831,374,424 | 17,904,566,872 |
| Warmup tracked allocations | 1,290 | 1,263 |
| Loading | 19.784 s | 19.393 s |
| Intervals late by over 1 ms | 0 / 191 | 0 / 191 |
| Maximum interval | 41.670791 ms | 41.671709 ms |
| Maximum recorded GPU fence wait | 22.230167 ms | 0.002542 ms |
| Playback cold resource touches | 0 | 0 |

The cumulative allocation snapshots diverge by **642,269,184 bytes** after
each of the three Azalea warmups; other layer increments are unchanged. The
27 fewer tracked allocations and rounded byte saving match three avoided mesh
sets per instance: two first-stage sets and one second-stage set. This supports
the larger initial recovery candidate, beyond the first duplicate pair alone;
it is not a new resource-by-resource trace census.

Residency and peak allocations overlap and must not be added. The comparison
uses separate processes/builds and uncontrolled machine load; one run does not
establish faster loading or a repeatable GPU-time improvement. Headless timing
excludes display presentation and audio hardware, and this run is not a visual
before/after comparison. CPU heap, physical footprint, unused texture-pool
capacity and future scene combinations remain unmeasured.

## CPU-copy release and temporary-array reuse

`GltfMeshSource` now releases the CPU vertex vector immediately after its
successful shared-buffer upload. Existing `uploaded` state distinguishes a
ready staging buffer from an unloaded source; subsequent destination changes
still copy from staging. GPU tests cover retained output, replacement output
storage and changed source-fit parameters. The three Azalea instances account
for about **491,241,600 bytes of source vertex payload** in those vectors before
release. That is a source-derived retention estimate, not an OS footprint delta;
allocator capacity and transient loading copies are separate.

The existing pure array planner now shares exact-type, exact-capacity temporary
roots after their last reader, using the backend's existing same-slot alias
action. Current-step inputs cannot share storage with that step's outputs.
Held, feedback/persistent, prebound, atomic, explicit in-place, canvas-dependent
and carried/exported resources remain dedicated. Canvas-dependent families are
excluded to preserve staged-resize storage identity. No resources are freed
between dispatches or frames, and residency/retirement policy is unchanged.
This is deliberately conservative and does not share between layer runtimes.

A native GPU comparison gives identical final geometry with shared and dedicated
buffers across five animated/repeated frames; the reference uses separately
prebound buffers. The four-wave test uses four physical buffers instead of five,
while retaining the cached source and carried final geometry. Existing planner
proofs cover feedback, explicit aliases, borrowed Math View inputs, atomic
initialization and invalid capacities.

One fresh baseline used the previously verified remap-reuse executable; one
verification used this batch's release executable. Both used the unchanged
project, start beat 56, eight seconds, retained disk caches and `/usr/bin/time -l`.

| Measurement | Remap-reuse baseline | This batch |
|---|---:|---:|
| Peak Metal allocated bytes | 18,570,297,344 | 18,569,117,696 |
| Warmup tracked residency bytes | 17,904,566,872 | 17,903,387,224 |
| Warmup tracked allocations | 1,263 | 1,251 |
| Maximum resident set size, bytes | 4,174,299,136 | 3,865,427,968 |
| Peak memory footprint, bytes | 23,905,440,064 | 24,320,807,016 |
| Loading | 19.457 s | 19.256 s |
| Intervals late by over 1 ms | 0 / 191 | 0 / 191 |
| Maximum interval | 41.671500 ms | 41.671042 ms |
| Playback cold touches | 0 | 0 |
| End-of-capture free texture pool | Not instrumented | 0 textures / 0 payload bytes |

The additional Metal saving is **1,179,648 bytes (1.125 MiB)** and 12 allocations.
This confirms that ordinary temporary reuse alone recovers little from this
particular protected/exported stack. Peak RSS decreased by 308,871,168 bytes,
but peak footprint increased by 415,366,952 bytes: this pair does **not** establish
an overall physical-memory improvement. Process peaks include loading and have
different accounting from Metal; none of these values should be added. No
repeatable load-time improvement, displayed-frame parity or audio result is
claimed. The pool measurement covers free entries only at capture end, not
temporary high-water occupancy or backing held inside live runtimes.

Commands, project/binary hashes, source diff, full captures and summaries are in
`/tmp/manifold-memory-overnight-20260921/{baseline,after}/`. The retained tested
executable is `/tmp/manifold-memory-overnight-20260921/manifold`.

## Follow-up static accounting

Resolving the six layers through `GeneratorRenderer`'s inline-graph override
and project preset catalog gives **17 glTF texture sources and six HDRI
sources**. Layers 1–2 use embedded preset 25; layers 3–6 use inline graphs.
There are no clip string overrides. Five 4096² texture identities occur three
times each; one 1024² identity occurs twice. All HDRI bindings have empty paths.
These are authored source identities, not a new measurement of live GPU images.
The full saved project also contains unused preset graphs; counting those as
live allocations would overstate memory use. Census artifacts are under
`/tmp/manifold-memory-overnight-20260921/texture-census*`.

Repeated sources are a concrete follow-up candidate, but sharing their current
mutable output textures would be incorrect: layers retain independent path,
conversion and parameter bindings. A shared immutable image would need exact
content identity and separate ownership of writable conversion outputs. The
existing disk decode cache does not establish that GPU sharing contract.

Static review also found existing duration-related protections:
`unique_clip_chain_topologies` deduplicates clip preparation; additional
topologies are prepared through one scratch runtime, rather than retaining a
runtime for every clip. Generator state is retained per layer, and video
lookahead already uses bounded prewarm candidates. Evicting inactive generator
runtimes would require preserving feedback/simulation state and preparing GPU
resources without delaying playback. Those changes are not implied by the
array planner's ordinary within-frame lifetimes.

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
