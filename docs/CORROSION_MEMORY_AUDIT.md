# Corrosion memory audit — one Azalea modifier stack

<!-- index: Bounded Azalea/Vortex Fragments/Ordered Recon/Math View memory audit; measured allocation classes, source-derived costs and unverified recovery candidates. -->

**Status:** Cut-remap reuse, CPU source-copy release, conservative temporary-array reuse, immutable source/converted-image sharing, loading retirement checkpoints and pool telemetry implemented and measured, 2026-09-22. Broader memory investigation remains open: BUG-dl16.

The latest combined build measures **15,736,799,232 bytes** of peak Metal
allocations, **4,760,305,664 bytes (23.22%) below the original clean capture**.
Peak process footprint measures **21,841,956,296 bytes**, compared with
23,905,440,064 bytes at the start of the overnight follow-up. These are separate,
overlapping measures from bounded headless captures, not additive memory savings.
Loading measured 22.14 seconds; duration-independent RAM and arbitrary modifier
complexity are not established.

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

## Immutable source-image sharing

The next bounded change shares identical glTF **source uploads**, while retaining
each node's writable conversion/mipmap output. A private thread-local cache uses
decoded RGBA8 SHA256, source dimensions, colour format and a unique device
resource-scope ID. The existing decode worker computes the digest. A miss creates
a fresh texture and completes its synchronous CPU upload before publishing it;
an existing shared texture is never overwritten. Weak entries do not keep images
alive after their source owners disappear, and expired entries are pruned during
upload lookup. Existing GPU retirement and residency still govern final release.
No new mutex, image-quality change, serialization field or runtime eviction policy
was introduced.

Eight focused native proofs pass. New synthetic tests verify shared native
identity and equal independent outputs; changing one source leaves the other
able to regenerate its original pixels. Equal payloads with different dimensions,
colour formats or device wrappers remain separate. Dropping both owners expires
the weak entry, and the next upload prunes it. The existing mode-flip proof now
also verifies that independent conversions use the same immutable source.

One release verification reused the preceding batch's saved capture as baseline;
there was no additional baseline reproduction. Project, settings, beat 56 start,
eight-second window and retained disk-cache policy are unchanged.

| Measurement | Before source sharing | With source sharing |
|---|---:|---:|
| Peak Metal allocated bytes | 18,569,117,696 | 17,888,559,104 |
| Warmup tracked residency bytes | 17,903,387,224 | 17,222,828,632 |
| Warmup tracked allocations | 1,251 | 1,240 |
| Maximum resident set size, bytes | 3,865,427,968 | 2,980,888,576 |
| Peak memory footprint, bytes | 24,320,807,016 | 22,747,876,784 |
| Loading | 19.256 s | 19.905 s |
| Intervals late by over 1 ms | 0 / 191 | 0 / 191 |
| Maximum interval | 41.671042 ms | 41.672083 ms |
| Playback cold touches | 0 | 0 |
| End-of-capture free texture pool | 0 textures / 0 bytes | 0 textures / 0 bytes |

The additional **680,558,592 bytes** of Metal recovery and 11 fewer tracked
allocations match ten avoided 4096² source textures and one avoided 1024² source
texture at the earlier trace's rounded sizes. This supports the source-census
attribution; it is not a new per-owner allocation trace. This batch's process
footprint peak is 1,572,930,232 bytes lower than its immediate baseline, and
1,157,563,280 bytes lower than the first overnight baseline. Those single-run
process differences include transient loading and uncontrolled machine conditions;
they are not precise attribution to the cache alone. The loading samples do not
show an improvement. No displayed-playback or audio claim is made.

Evidence, hashes, source diff, comparison and the retained tested executable are
under `/tmp/manifold-memory-images-20260921/`. Full required landing transcripts
are kept outside the managed slot cache in that directory's `landing-logs/`.

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

Repeated source uploads are shared as described above. The follow-up below adds
an immutable-output contract for identical converted images. Each node retains
independent path, conversion and parameter bindings; the disk decode cache alone
does not establish GPU sharing or ownership.

Static review also found existing duration-related protections:
`unique_clip_chain_topologies` deduplicates clip preparation; additional
topologies are prepared through one scratch runtime, rather than retaining a
runtime for every clip. Generator state is retained per layer, and video
lookahead already uses bounded prewarm candidates. Evicting inactive generator
runtimes would require preserving feedback/simulation state and preparing GPU
resources without delaying playback. Those changes are not implied by the
array planner's ordinary within-frame lifetimes.

## Converted-image sharing follow-up (2026-09-22)

The bounded follow-up extends the existing glTF image cache and graph resource
planner. A producer can supply an immutable converted texture instead of writing
into a separate backend-owned target. The plan holds that output in a dedicated
slot. Its descriptor preserves the original dimensions, format and full mip chain;
the conversion shader is unchanged. Feedback back-edges and host-prebound targets
continue to use the existing writable path.

The weak cache key includes decoded source identity, output dimensions, format,
mip count, repack mode and device resource scope. A GPU event is signaled after
conversion and mip generation. Cache consumers adopt only a ready image, never
wait for an unsubmitted encoder, and never write into an already published image.
If simultaneous first conversions create duplicates, warmup gives them another
pass to adopt a completed canonical image. A completed local image can replace
an unfinished canonical entry, so abandoned encoders do not hold up warmup.

Changes to one node select or create a different image. Logical content versions
stay separate from physical storage identity. The backend cannot pool or swap
immutable outputs into writable storage; staged resize retains compatible images
and prepares incompatible bindings without modifying the live renderer. Existing
GPU retirement and residency ownership remain in force.

The first project capture exposed a loading integration gap: recorded peak Metal
allocations increased from 17,888,559,104 to 18,794,561,536 bytes, and warmup
residency increased by the same 906,002,432 bytes (six allocations). No late
intervals were measured. Static tracing found synchronous warmup commits did not
advance the content frame retirement event: replaced black/duplicate images could
remain retained until playback. A load-only checkpoint now signals that same
event after each layer and at final warmup, waits for completion, then drains the
existing retirement/residency owners. It requires no outstanding unsubmitted
encoder. Playback retains its ordinary nonblocking fence path. The six-allocation
delta matches five 4096² and one 1024² converted mip image at observed Metal sizes;
this agreement is supporting evidence, not a complete live-allocation census.

The effect-chain allocator also reserves provided intermediate slots without a
writable RenderTarget or host pin, preserving ordinary writable source/final
endpoints. This closes the second build path's ownership integration.

One verification of that concrete correction used the same project, retained disk
caches, beat 56 start and eight-second 24 fps headless window:

| Measure | Previous landed build | Converted sharing + loading checkpoint |
|---|---:|---:|
| Load | 19.905 s | 22.141 s |
| Recorded peak Metal allocations | 17,888,559,104 B | 15,736,799,232 B |
| Warmup residency accounting | 17,222,828,632 B / 1,240 allocations | 15,054,357,080 B / 1,207 allocations |
| Intervals late by more than 1 ms | 0 / 191 | 0 / 191 |
| Maximum interval | 41.672083 ms | 41.671875 ms |
| Cold preparations | 0 | 0 |
| Maximum process RSS | 2,980,888,576 B | 3,163,275,264 B |
| OS peak process footprint | 22,747,876,784 B | 21,841,956,296 B |
| End-of-capture free texture pool | 0 textures / 0 bytes | 0 textures / 0 bytes |

This batch reduces recorded peak Metal allocations by **2,151,759,872 bytes**.
The footprint decreases by 905,920,488 bytes while RSS increases by 182,386,688
bytes; these distinct process/GPU metrics overlap and must not be added. Loading
is 2.24 seconds slower in this pair. The single-run evidence establishes neither
a repeatable loading improvement nor isolated savings for each component of the
batch. Display presentation, audio hardware and long-show seek/loop/edit behavior
remain unmeasured. The failed capture is retained as `after/`; the corrected
verification is `after-completion/`, with `comparison-completion.json` alongside.

Four focused planner/backend proofs pass: dedicated storage without a duplicate
writable allocation, exclusion from pool reuse and feedback swaps, host-prebound
compatibility, fixed-image retention through staged resize, and feedback back-edge
exclusion. The 12-test glTF GPU group passes, including four new proofs for:

- byte equality with the existing writable conversion at every mip of a 4×4
  nonuniform image, before and after a repack-mode edit;
- no sharing of unsubmitted work, reversed submission order, and convergence
  after the original canonical encoder was delayed;
- distinct mode/dimension keys and actual opaque-black output pixels;
- two real executors sharing one image, repeated-frame retention, changing one
  layer without changing its peer's pixels, and weak-entry expiry.

The chain-builder ownership proof and existing fence-retirement/residency proof
also pass. Landing validation exposed an imported-model test that compared two
black images before async loading completed. Its existing 600-frame bound now
checks warmup readiness, yields briefly while loading, and rejects a zero baseline;
the original RT-dispatch and pixel-ratio assertions remain. Focused verification
passes. The new chain ownership test is feature-gated so normal CPU checks do not
require a Metal test device.

Validation and measurement artifacts are kept in
`/tmp/manifold-memory-converted-20260922/`. This change does not implement inactive
scene eviction, simulation checkpoints or a project-wide memory budget.

## Idle owners and scratch reuse (2026-09-22)

This follow-up addresses retention gaps identified by static review. It does
not change the project-level measurements above.

- `GeneratorRenderer::thumb_gens` kept a full generator runtime for a visible
  parked clip even after its still image had been copied into the clip atlas.
  The old length comparison also missed changes between equally sized visible
  sets. The content pipeline now prunes these owners after atlas work on every
  frame, including pressure, export and empty-visibility paths. Only visible,
  uncaptured thumbnails remain. Live layer generators are separate owners.
- Cold thumbnail output is withheld while runtime preparation or frame validity
  is pending. Each cold attempt consumes the existing one-per-frame budget;
  retries preserve the warmup frame counter. Atlas copy encoding precedes owner
  release; existing frame retirement retains GPU resources until completion.
- `LayerCompositor` now drops cached layer, group, LED-group and master runtimes
  when their authored effect list becomes empty, including during empty playback.
  Disabled and zero-amount effects retain their existing lifecycle. Pool slots
  and liveness stamps remain paired so later layer deletion can still prune them.
- `plan_array_allocations` remembered only one free physical root for each exact
  layout/byte-capacity key. A later free root with the same key was discarded
  from the reuse inventory. Buckets now retain distinct eligible roots and remove
  roots claimed by explicit aliases. The existing lifetime, state, host-pinning,
  atomic, canvas-resize and carried-resource exclusions remain unchanged.
  CPU and IO boundary array inputs/outputs are additionally kept dedicated.

Savings depend on the visible thumbnails and graph lifetimes. A thumbnail runtime
can contain much more than its 512×288 output texture. Neither that texture's
payload nor a planner fixture is a measured saving for the Corrosion project.
There is no new claim about loading speed, process RAM or playback latency here.
The focused planner fixture needs two 16-byte allocations for four logical arrays;
the previous single-root inventory would require three. This demonstrates the
missed reuse case, not its frequency or byte impact in the saved project. Tests
also cover thumbnail capture surviving owner release before command submission,
and removal of obsolete effect owners without evicting disabled effects.

The first complete GPU run exposed a missing lifetime boundary: Digital Plants
camera wrap equivalence failed with mean absolute channel difference 11.614
(required <1.0). The same isolated test passed when only the allocation planner
was restored to `origin/main`. Static review found that ordinary step completion
does not end a buffer lifetime across CPU evaluation and deferred GPU execution.
Mapped CPU writes can overwrite a GPU input before its encoded command runs.
The planner now uses the existing `NonGpu` / `IoBridge` classification to exclude
both sides of those boundaries from scratch reuse, while GPU-only lifetimes remain
eligible. The native retirement, shader and camera logic are unchanged.

The audit also found an older, distinct limitation in `ArrayMath::run`: CPU reads
assume CPU-produced inputs, and its output loop caps writes at 4096 elements.
Digital Plants connects GPU-produced arrays with a larger capacity. Commit
`f0a700400` introduced that CPU implementation for curve chains. Dedicated storage
addresses the reuse regression; it does not establish correct same-frame transfer
or full-capacity arithmetic across that older CPU/GPU boundary. This needs a
separate value-level producer/consumer proof and an execution-domain fix that also
preserves CPU curve consumers, rather than inserting per-node blocking GPU waits.

### Active/nearby scenes: audit result, not an implemented scheduler

The audited seams are `GeneratorRenderer::layer_generators`,
`LayerCompositor::trim_excess_buffers`, `PresetRuntime::clear_state`,
`MetalBackend::prepare_resize`, and `PlaybackEngine::compute_prewarm_candidates`.
Generator ownership is already per layer rather than per clip. Compositor scratch
buffers recreate on demand; array backing is prebound and cannot be treated as an
ordinary free texture slot. Node fields and `StateStore` both carry persistent
state. Existing idle effect-state clearing is a separate playback policy and
cannot serve as a state-preserving suspension operation.

Video lookahead is bounded, but does not prepare generators. The existing generator
and effect warmup methods render synthetic frames and may synchronously wait for
GPU completion. Reusing them during playback would advance state and could stall
the frame thread. Dropping an entire distant runtime would instead lose state.
Neither approach is acceptable for the requested preparation policy.

The next implementation boundary is a side-effect-free preparation/suspension API
for eligible transient backing, retaining simulation, feedback, immutable content
identity and output dependencies. It must use the existing resource plan and
retirement/residency owners, invalidate storage-dependent cached outputs after
rematerialization, and keep old resources usable until replacements are ready.
Only after a focused suspend/resume pixel-and-state proof should beat-domain
lookahead select nearby layers and enforce memory/work budgets. Seek, loop,
topology, resolution and content changes need explicit invalidation; preparation
must never activate clips or advance triggers. This remains unfinished work under
BUG-dl16, not a claim that project duration is now independent of memory.

## RT filter scratch: bounded follow-up (2026-09-22)

Static review at `15256c420` follows the same Azalea → Vortex Fragments →
Math View → Ordered Recon stack through its shared `node.render_scene` backend.
The currently saved project has SHA256
`87f1e89a4baeef5cb66da345423ae2d46c9882cb29dcc5a2a7c69dc10ee1f405`,
different from the earlier capture. Its representative layer enables RT, disables
temporal upscale and MetalFX denoise feed, and selects spatial denoise Off for
realtime / High for export.
`ensure_rt_irradiance` nevertheless allocated two full-render-resolution RGBA16
post-filter textures even when the post-filter did not run. Ray resolution is
quarter for realtime; it changes trace targets, not these full-resolution targets.
These are source/project findings, not a new live allocation capture.

The relevant lifetimes are:

| Storage | Required lifetime and coexistence |
|---|---|
| Imported geometry, cut maps/remaps and exported Math View inputs | Retain their existing cached/exported ownership; not reclaimed here |
| RT irradiance, reflection, visibility, depth, normal and moments histories | Independent read/write history pairs; survive frames and remain separate from scratch |
| Pre-accumulation full targets plus their `_b` scratch | Both sets coexist during two spatial passes; the last pass writes back to the full targets |
| Full normal, depth and newly written moments/history | Remain readable as post-filter guides/input; cannot be reused for its outputs |
| `rt_irr_full_b` and `rt_normal_full_b` | Last reads finish in GPU command order before accumulation/post-filter; no CPU mapped access or raw capture export |
| Post-filter `rt_irr_filtered` and `_b` | Derived current-frame output/intermediate, not history; final output survives through the composite |

The bounded change reuses the two pre-filter scratch handles for the post-filter
pair. Both have identical full-render dimensions, RGBA16 format and usage. It
keeps all histories, guides and diagnostic raw full targets separate. The next
frame overwrites scratch in the same GPU queue order, after the previous
composite. Existing command-buffer checkpoints preserve queue order and native
hazard tracking; no CPU completion assumption, mapped writes, cross-runtime
sharing, new pool, wait or scene-readiness policy is introduced. Resize recreates
the backing and aliases together. Disabled post-filtering requires no extra
backing, and enabling it uses already-prepared storage.

The smallest saving is **two physical textures**, with payload
`2 × render_width × render_height × 8` bytes: **33,177,600 bytes (31.64 MiB)**
for one 1080×1920 renderer. Three matching Azalea renderers give a source-derived
estimate of **99,532,800 bytes (94.92 MiB)**. This is neither a measured project
Metal peak nor a process RAM reduction; alignment, other owners, resize overlap
and actual render dimensions affect live accounting. No new project capture,
loading-speed, display/audio or performance result is claimed.

The focused native Metal proof passes: actual pre- and post-filter kernels
produce bit-identical finite pixels with reused and dedicated storage across
queued Off/odd/even frames. Raw irradiance and normal guides remain identical,
history/moments identities stay separate, and resize refreshes both aliases.
This is a synthetic lifetime/value proof, not displayed project verification.
Focused output, source diff and required landing logs are retained outside the
slot in `/tmp/manifold-memory-rt-scratch-20260922/`. The first sandboxed test
compiled but could not find a Metal device; the corrected proof passed with
native device access. The larger question remains which RT output/history families can
be absent for a given actual consumer set: current trace, upsample and accumulation
kernels bind those families together. Disabling a feature alone does not establish
that its textures can be removed. Resolve producer writes, consumer bindings and
history reset requirements before changing that contract. Predictive loading and
scene suspension are outside this follow-up's scope. BUG-dl16 remains open.

## RT scalar histories: allocation audit (2026-09-22)

Source inventory at `f6cd18425` counts 46 physical textures in
`ensure_rt_masks` and `ensure_rt_irradiance`, including the already-shared
post-filter pair only once. For the representative 1080×1920 output and
270×480 trace dimensions:

| Lifetime | Textures | Payload bytes |
|---|---:|---:|
| Persistent ping-pong histories | 28 | 481,075,200 |
| Current-frame trace, full-resolution outputs and filter scratch | 18 | 205,286,400 |
| Total for these two allocators | 46 | 686,361,600 |

These are calculated payloads, not measured Metal allocations, residency or
process RAM. They exclude other render resources, alignment and resize overlap.
History halves must coexist: reprojection reads previous-frame neighbours while
writing the current frame. The current-frame storage has the ordered lifetimes
described above; its size alone does not establish further reuse opportunities.

The strongest bounded waste is four full-resolution snap-hold textures:
`rt_sv_hold_history` and `rt_sv2_hold_history`. The accumulation shader reads only
`.x` and writes a scalar countdown with the other channels zero. Changing these
from RGBA16Float to R16Float retains the same half-float precision, both groups,
both history halves and all reset behaviour. The payload reduction is
`4 × width × height × (8 − 2)` = **49,766,400 bytes (47.46 MiB)** per
1080×1920 RT renderer. This allocator subtotal becomes 636,595,200 bytes;
three matching renderers would save 149,299,200 bytes (142.38 MiB) of payload.
There is no new project-scale memory or speed measurement.

Larger apparent opportunities require more evidence. Each complete visibility
group costs 166,924,800 bytes here, but removing the second group would conflict
with the intended eight-caster RT backend. Static review found that shared scene
selection currently caps shadow casters at four, despite the backend's eight-slot
contract. Existing “slot 5” tests enable shadows on light index 5 alone, which
compacts into caster slot 0; they do not prove five simultaneous shadow casters.
This is a separate correctness/proof gap recorded under BUG-dl16, not a reason to
delete the intended capacity. Likewise, conditional omission of reflection,
tint or other families needs a producer/consumer binding audit: the current
kernels bind and write them together. This change introduces no light limit,
readiness policy, scene unloading or new resource pool.

Static inventory and focused verification are retained in
`/tmp/manifold-memory-scalar-history-20260922/`. The native
`scalar_hold_r16_matches_rgba16_history` proof compares finite, bit-identical
lighting, reflection, visibility, tint and hold values against the previous
format through independent group crossings, hold decay and resets. The existing
resize proof also checks scalar formats, stable/replaced identities and a
render-pass sentinel clear. These are synthetic GPU proofs, not project renders.

## RT firefly resolve: bounded lifetime follow-up (2026-09-22)

Static review at `aae2686c4` confirms the representative Azalea enables RT
shadows, AO, GI and reflections. The trace, upsample, spatial filter and temporal
accumulator bind/write these families together. Omitting a disabled family still
requires explicit producer/consumer and re-enable/reset handling; it is not safe
to remove its allocations based on the surface-shading toggle alone. The earlier
four-caster selection defect above was corrected by `aae2686c4`; the remaining
user-facing raster/RT budget difference is tracked separately in BUG-9o5s.

A smaller demonstrated duplication is the firefly-clamp scene resolve. Its
RGBA16 texture and reflection-prefilter scratch have equal render dimensions but
nonoverlapping GPU uses:

1. Upsampling writes current reflection; spatial pass one writes `rt_refl_full_b`.
2. Spatial pass two reads that scratch and writes current reflection back.
3. Accumulation reads current reflection, preserving independent history pairs.
   The optional post-filter uses irradiance/normal scratch, not reflection scratch.
4. The scene pass resolves into `rt_firefly_scratch`; transparency and shafts
   complete there, then the firefly clamp reads it and writes a distinct target.

The change makes reflection scratch renderable and aliases the firefly resolve to
it. The next frame rewrites scratch after the preceding clamp read in queue order.
Both handles refresh on initial allocation and trace/render resize. Clamp bypass,
RT readiness, denoiser bypass and temporal-upscale output selection stay intact.
Raw capture outputs and all histories remain separate; there is no CPU mapped
access, new pool, wait, readiness policy or scene unloading.

The native proof
`rt_firefly_scratch_shared_backing_matches_dedicated_across_queued_frames`
passes with bit-identical finite results against independent backing. It queues
actual reflection filters, 4× MSAA resolves and firefly clamps across command
buffers without intervening CPU waits, including a clamp-bypass frame. It checks
the earlier reflection result after reuse, preserved raw/history pixels and
nonopaque alpha, nontrivial bright-pixel clamping, and alias refresh after both
trace-only and render-size changes. This is a synthetic GPU proof, not displayed
project verification.

One physical RGBA16 texture is removed: `width × height × 8` payload bytes,
**16,588,800 bytes (15.82 MiB)** at 1080×1920 per renderer. Three matching
renderers would remove 49,766,400 payload bytes. These are calculated payloads,
not a new project Metal peak, process RAM, loading or performance measurement.
Focused native verification and required landing results are recorded under
BUG-dl16 and `/tmp/manifold-memory-firefly-scratch-20260922/`.

## Source anchors

Paths below are under `crates/manifold-renderer/src/` unless prefixed with `crates/`:

- `generators/mesh_common.rs`: `MeshVertex` stride.
- `node_graph/primitives/mesh_cut_map.rs`: `map_capacity`, `scratch_bytes`.
- `node_graph/scene_modifier_expand/fragment_cuts.rs`: `apply`, `align_mesh`.
- `node_graph/resource_allocation.rs`: `plan_array_allocations`.
- `node_graph/metal_backend.rs`: `pre_bind_array`, `release`, `clear`.
- `node_graph/execution_plan.rs`: `free_after`, held/persistent resources.
- `node_graph/primitives/gltf_mesh_source.rs`: `cached_verts`, staging upload.
- `node_graph/primitives/gltf_texture_source.rs`: immutable source upload cache.
- `preset_runtime/math_view.rs`: shared resources, variants, presentation.
- `node_graph/scene_modifier_expand/buffer_budget.rs`: candidate admission.
- `crates/manifold-gpu/src/metal/texture_pool.rs`: cap, report, recycling.
- `crates/manifold-gpu/src/metal/device.rs`: resource-scope identity.
