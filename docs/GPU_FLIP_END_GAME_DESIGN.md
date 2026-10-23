# GPU FLIP end game — the fastest real-time water on consumer hardware

<!-- index: Direction for the GPU FLIP end-game programme (umbrella epic BUG-l2h3): where the water stands today, the decisions that shape the stack (own sparse tiles, no mesh in the live path, APIC, memory follows the water), the sub-epic programme, the free-performance class, and the expected outcomes per machine. The full design spec and implementation plan are the next session's job. -->

**Status:** DIRECTION captured 2026-10-21 · Fable + Peter · not designed to STANDARD, not built; the full design spec and implementation plan graduate from here. Epic: BUG-l2h3 (GPU FLIP live instrument), rewritten as the umbrella; sub-epics under it.
**Prerequisites:** none for the measurement; everything else waits on the fresh 64/128 split (D10).
**Companion docs:** [GPU_FLIP_STRUCTURAL_OPTIONS.md](GPU_FLIP_STRUCTURAL_OPTIONS.md) (the four structural levers, priced), [GPU_FLIP_SPARSE_BLOCKS_DESIGN.md](GPU_FLIP_SPARSE_BLOCKS_DESIGN.md) (sparse tiles as built), [GPU_FLIP_PRESSURE_SOLVE.md](GPU_FLIP_PRESSURE_SOLVE.md) (solver, deferred rows), [GPU_FLIP_NARROW_BAND_DESIGN.md](GPU_FLIP_NARROW_BAND_DESIGN.md), [GPU_FLIP_DISPLAY_HISTORY_DESIGN.md](GPU_FLIP_DISPLAY_HISTORY_DESIGN.md), [GPU_WHITEWATER_DESIGN.md](GPU_WHITEWATER_DESIGN.md), [OCEAN_SURFACE_DESIGN.md](OCEAN_SURFACE_DESIGN.md) section 9 (Ocean to FLIP coupling), [GPU_MPM_SOLVER_DESIGN.md](GPU_MPM_SOLVER_DESIGN.md) (the shared-grid consumer), [VULKAN_BACKEND_DESIGN.md](VULKAN_BACKEND_DESIGN.md) (D9), [RAYTRACING_DESIGN.md](RAYTRACING_DESIGN.md) (the tracer D2 extends).

## The claim

The goal is fast, low-memory, real-time GPU FLIP water: the fastest and most efficient GPU FLIP simulation there is, on consumer hardware, live on stage. The live path keeps no mesh. Export to other apps is out of scope. Bakes are low priority. Benchmarks come last, if at all.

The governing insight: the simulation is small. At resolution 128 the solver's own state is about 100 MB. Everything that made water expensive on this machine sits beside the solver: a surface mesh held as five copies, a ray-tracing structure rebuilt from it every frame, a display ring that copies the mesh per slot, and dense passes over a box that is mostly air. Remove those and the only cost left is wet cells plus particles, which is the cost a state-of-the-art sparse solver pays and no more.

## 1. Where we stand

Measured 2026-10-03 on main e6888c4b6, waterFunv3, 1080p, M4 Max, whitewater on, plain-frame GPU p50 (notes on BUG-g75v (water campaign landing queue), item 5, the optimisation lane and 64/128 measurement). Five speed landings since, none re-measured; the solver lanes start from two fresh runs (D10).

| | Today | Target |
|---|---|---|
| Resolution 64 | 64 ms a frame, about 15 fps | 16.7 ms |
| Resolution 128 | 461 ms a frame | 33 ms, then 16.7 |
| Forces and hits behind the beat | 75 to 110 ms | under one frame |
| Export at 128 Surface Detail 1, and at 192 | dies on GPU memory mid-run | 512 overnight |
| Surface mesh memory at 128 | about 6 GB (five copies at 80 B a vertex slot, 2x headroom) | 0 in the live path |
| Unaccounted GPU memory at 128 | 6 to 8 GB (suspect: the mesh's ray-tracing structure) | re-checked after D2 removes that structure |
| Solver state at 128 | about 100 MB | unchanged |

Against the published methods: sparse paged grids (Setaluri 2014) partial, work-sparse but memory-dense; narrow band (Ferstl 2016) built, off, refuses sheeting; coarse projection (Lentine 2010) built as the Solve Level card, broken above level 0; MGPCG (McAdams 2010) built, rows stored per level; APIC (Jiang 2015) nothing on FLIP, built on MPM; anisotropic surface (Yu and Turk 2013) partial; whitewater (Ihmsen 2012 as the FLIP Fluids engine ports it) built; surface tension and viscosity nothing; traced refraction nothing on main; caustics and underwater view nothing. Wu et al. 2018 (sparse GPU FLIP on GVDB) is the same family on a CUDA-only structure; it is the comparison if a benchmark is ever run (D8).

## 2. Decisions

- **D1. Own sparse tiles, not OpenVDB, NanoVDB or GVDB.** Tiles must appear and vanish on the GPU every tick with no CPU round trip. NanoVDB is read-only on the GPU; GVDB is CUDA-only and unmaintained; SPGrid-style flat tiles beat the VDB tree for stencil passes and are what every live GPU solver rolls. Rejected: linking OpenVDB, because it is CPU-side C++ with TBB, Boost and Blosc and nothing in it runs through `manifold-gpu`, so Vulkan would not follow. VDB file import and export is out of scope. Peter: "I'm not interested in exporting out to other apps if takes GB of memory."
- **D2. No mesh in the live path. The water surface is drawn directly from the level set.** Peter: "Why have the mesh at all if we can do this?" The solver already keeps a signed-distance grid every step. One full-screen pass walks each pixel's ray through that grid (sphere tracing: read the distance, jump that far, stop at zero), writes depth, normal and position, and the existing lighting, shadows, fog, transmissive layer and material system read those exactly as they read a mesh hit. Refraction and absorption are the same walk continued inside the water. The ray tracer gets a custom intersection on the water's tiles doing the same walk, so the no-partial-ray-tracing rule holds by construction. Marching cubes, the smoothing passes, the normals pass, the welded mesh and the mesh's ray-tracing structure leave the live path; marching cubes stays as an offline export tool only. Consequences, stated honestly: this is a render architecture change, not an optimisation; water shading moves out of the mesh material into the march; shadows cast by the surface, edge anti-aliasing and motion vectors need their own handling; the engine-default look was tuned on the smoothed mesh and needs a look pass against the FLIP Fluids reference exports (a grid-side blur replaces the mesh smoothing); cost scales with pixels, so 4K live needs a half-resolution search with a full-resolution refine (one or two exact steps per pixel from the coarse hit; the picture stays full-res). Rejected: fixing the five copies and keeping the mesh, because it leaves a per-frame rebuild, a ray-tracing structure and a display ring that all scale with the mesh; the five-copies fix, BUG-kipwz (mesh held as five full-size copies), stays only as the stopgap that unblocks exports before D2 lands.
  - **Priority 1, and it starts without the measurement.** It is justified by memory and look alone and touches no solver code, so it runs beside the solver lanes.
  - **What exists and what is new.** The field exists: the surface chain builds blobs (`node.shape_particle_blobs`), a volume (`node.particle_volume`) and a re-distanced lattice (`node.redistance_lattice`) before marching cubes. The renderer half is new: the scene renderer and the ray tracer draw and hit triangles only, so the march pass and the tracer's custom hit are the build.
  - **Fit with the scene document.** The march runs inside the sealed Scene node and writes colour, depth, normals and the item-id buffer, so a click on the water selects it (SCENE_DOCUMENT_DESIGN.md section 1 (Rulings), rulings 10 and 18). It also writes exact motion vectors from the sim's velocity. The Water emitter swaps its surface output in one lane and keeps the Surface row ids (SCENE_DOCUMENT_DESIGN.md section 7.18 (Fit with the water end game)). Water binds a Material item like any Object, so the water can be re-materialled; the Water card has no Material binding today.
  - **Modifiers on the water field.** Scene Loop, Scene Mirror and copies apply by moving or reflecting each sample point before the field is read, cheaper than on a mesh, so water joins them. Smooth deformers (waves, twists, bends) walk the field through the inverse of the deform with shorter steps. Shatter, Surface Peel and Ordered Recon cut or reveal triangles and do not apply to water.
- **D3. APIC transfers on FLIP.** One affine matrix per particle (36 B at f32, 18 B at f16). Less noise at the same particle count, Houdini's default, and the bridge to one shared grid with MPM, which already transfers this way. Particle memory roughly doubles; particles are not the budget (D4). Priority 2. APIC is the default with no option row: the scene document makes solver internals automatic. Peter: "APIC sounds like the proper way to do this anyway."
- **D4. Memory follows the water, with a budget per resolution.** BUG-7wna (sparse tile storage) then BUG-zdiuw (GPU FLIP infinite domain, the world-keyed tile table) make lattice memory proportional to wet tiles and the domain unbounded. Budgets: 128 live under 2 GB total; 256 under 1 GB of solver state; 512 export inside this machine's 36 GB. Every landing in the programme reports its memory against the budget. With the domain unbounded, the Water box becomes an optional Bounds setting, off by default. Off, water goes anywhere in the scene. On, the box is the region water may live in and water leaving it is removed (a tank, or a region following the camera along a river); moving it live keeps the water inside. Water ends only by falling below the lowest thing in the scene, or when GPU memory is full, where sources stop adding water and the card says why, never a crash. BUG-zdiuw (infinite domain) is priority 2, straight after BUG-7wna (sparse tile storage).
- **D5. 16-bit is storage, never arithmetic.** Position is a 16-bit integer offset inside the particle's tile (about one eight-thousandth of a cell). Velocity in f16 is gated by the long-run settled-pool agreement gate, BUG-0cbzb (the default has no long-run agreement gate); if a resting pool jitters, velocity stays f32 and the particle is 24 B. The pressure solve stays f32. Grid fields may store f16 where a pass only reads them. Rationale: the 2D texture sims quantised because the whole state lived in f16 across steps; here particles are the state and the grid is rebuilt every step.
- **D6. Surface tension and viscosity stay deferred.** Trigger: macro shots (a drop beading, pinching, bouncing). The engine has both solvers to port. Peter, 2026-10-03: low priority now.
- **D7. Bakes parked.** Peter: "bakes are actually low prio given the disk sizes." Export renders straight to video in the same pass, so no disk cache is needed for the programme's goals. The bake workflow stays in BUG-vglg (shared liquid physics) item 18, the fluid bake and cache workflow, for a later product discussion.
- **D8. Benchmarks last, if at all.** Peter: "benchmarks come at the very end if at all." No bead. If one is ever run, Wu et al. 2018's scenes are the comparison.
- **D9. Vulkan is the dependency for any non-Apple number.** Nothing in the programme is Metal-only; every GPU path goes through `manifold-gpu`.
- **D10. Two short runs before the solver lanes, nothing before D2.** One frame-time run at 64 and one at 128 on main, with the per-node split and GPU busy percent, minutes each through `scripts/gpu_queue.py`. They set the order of the solver work and rewrite section 5. No separate memory hunt: the unaccounted memory is re-checked once D2 has removed the mesh's ray-tracing structure.
- **D12. Liquid and Gas are separate kinds on one grid.** They behave differently, and both solve on the shared sparse grid. Turning one into the other (water boiling to steam, steam condensing, a liquid fuel igniting) is a conversion rule in the solver that moves mass between them at the same place, never a change of kind; Houdini runs FLIP and Pyro the same way. Adding the Gas kind is a ruling change in SCENE_DOCUMENT_DESIGN.md section 1 (Rulings), ruling 4: one kind, one compiler module, one card-table entry. Owned by BUG-rpf5b (fire and smoke on the shared sparse grid).
- **D13. The look stack, in order of look per millisecond.** Spray and bubbles as sphere impostors, with the same spheres in the ray tracer. Exact motion vectors from the sim, so motion blur and upscaling stay clean on fast water. Foam as a field on the surface, carried by the flow and gathering where water converges. Light through thin water from the distance the march travels inside it, so crests glow and depth goes dark. Wet surfaces: colliders darken where water has been and dry slowly. Detail normals: projected from world space on three axes (the field has no texture coordinates), moved along the sim's own velocity with two crossfading copies to hide stretching, strength from the flow, masked by foam, sourced from the FFT ocean's spectrum so FLIP water and ocean match where they meet; for close-ups the same detail displaces the surface inside the march. Then traced refraction and reflection, caustics, and mist from spray as thin fog.
- **D14. Real units everywhere, so one setup works from a drop to an ocean.** Metres, seconds and kilograms with real constants; no dial tuned in scene units. Cell size in metres, with time steps following automatically. Whitewater triggers on real speed and curvature, with drop size in millimetres. Absorption and scattering per metre. Ripple detail in metres, the layer chosen by its size on screen. March step sizes and tolerances relative to the cell, positions relative to their tile. Surface tension is the missing piece for drops (D6). The camera lens is already physical (CAMERA_AND_LENS_DESIGN.md); its Max Blur pixel cap is checked on macro shots, and camera autofocus is general scene work, not this programme.
- **D11. Tall cells are Peter's call, and only if the sparse, coarse and narrow-band stack leaves 128 short.** They are the one lever that changes what the audience sees under the surface.

## 3. The programme

One umbrella (BUG-l2h3 rewritten) with sub-epics in attack order. Each sub-epic names its end state on stage.

| # | Sub-epic | End state on stage |
|---|---|---|
| 0 | Measurement and tooling | Every speed and memory claim is a fresh number at 64 and 128; GPU busy percent is reported; the water's memory is fully accounted. |
| 1 | Simulation: correctness, step cost, structure and scale, coupling, reference completeness (APIC) | The sim at 128 costs what 64 costs today, never jitters, holds volume like the engine, floating bodies settle, and FLIP transfers like MPM. |
| 2 | Memory | Memory follows the water. Budgets per D4 met and reported per landing. |
| 3 | Live timing and display | Water reacts within a frame of the beat at 24, 30 or 60 fps; the display ring holds grids, not meshes. |
| 4 | Surface: level-set build and direct rendering | One high-quality level set built once from the particles, drawn directly (D2) in raster and ray tracer; the mesh exists only on export. |
| 5 | Rendering: water material, traced water, caustics, whitewater draw | The same water under raster and ray tracer; refraction and caustics that read as water; spray as spheres; particle velocity in the motion blur; foam as a field on the surface. |
| 6 | Whitewater simulation | Spray, foam and bubbles always refilling, matched to the engine, feeling wind and solids. |
| 7 | Instrument and authoring | Driven bodies, audio-shaped waves, wave tanks that do not resonate, the ocean feeding the tank, action areas inside a height-field sea. |

After the programme, on the same grid: BUG-rpf5b (fire and smoke on the shared sparse grid, and solids that ignite and melt) is its own epic. A gas solver with no particles on the same tiles and pressure solver, drawn by the same per-pixel walk accumulating density and blackbody emission, coupled one-way to the water, and MPM phase change so solids heat, melt into the shared grid and burn. It consumes sub-epics 0, 2, 3 and 5 and the APIC bead.

Out of the programme: bakes (D7), VDB (D1), benchmarks (D8). BUG-vglg (shared liquid physics: authoring, takes, coupling plumbing) stays separate and lends its physics beads. MPM consumes sub-epics 0, 2, 3, 4 and 5 unchanged; sub-epic 1's pressure solve, coarse projection and narrow band are FLIP-only.

## 4. Free performance — the class to keep hunting

Each of these removes work rather than speeding it up. The design session looks for more of the same.

- No mesh in the live path (D2): memory to zero, the render side stops scaling with resolution.
- The GPU never idles: overlap the next tick with this frame's render; two beads already show CPU-bound frames with zero GPU wait.
- Dispatch to live counts, never capacity: the GPU sizes its own work with no CPU readback.
- One level set for solver and render: build the good one once, feed both.
- Display history as sparse grids, not meshes: falls out of D2.
- Whitewater grid passes over active tiles only: falls out of sparse tiles.
- Fewer passes: fuse smoother levels, warm-start the pressure solve from last tick's pressure, tie the solve tolerance to what is visible.
- Sim rate decoupled from display rate: landed; keep it.

## 5. Expected outcomes

Paper estimates, give or take 2x, Dam Break with whitewater at 1080p, full stack including D2, Vulkan assumed perfect. Rewritten after D10.

| | M4 Max (36 GB) | RTX 5080 (16 GB) |
|---|---|---|
| 60 fps live | 128 with the sim at 30 steps a second, edge of budget | 128 at full sim rate; 192 with the sim at 30 |
| 30 fps live | 128 solid, 192 at the edge | 256 |
| Overnight export | 512 (about 6 GB; 2 to 5 s a frame; a minute of footage in a few hours) | 512, time-bound |
| Particles live at 60 fps | 10 to 15 million | 25 to 30 million |
| Solver memory at 256 | under 1 GB | under 1 GB |

- The 128 sim tick is 410 ms today. The structural stack (sparse, coarse projection, narrow band) is about 9x on paper; fusion, warm start and 16-byte particles stretch it toward 15 to 20x. That number decides 128 at 60 on the Mac.
- Each doubling of resolution costs 5 to 8x; a hardware doubling buys about half a resolution step; the software stack is the 10x lever.
- Memory stops being a limit on either machine at 512; 1024 (50 to 60 GB) is out on 36 GB.
- Pixels: the march costs per pixel. 1080p a few ms; 4K live 4x that without the half-res search; exports do not care.
- With the unbounded domain, resolution means cells per metre. A thin waterfall at 256 costs less than a full tank at 128; a macro shot is a tiny box at millimetre cells.

## 6. Open — Peter's calls

1. Headline target. Assumed here: 128 at 60 fps display with the sim at 30 steps a second on the M4 Max, and 512 overnight export.
2. Tall cells (D11).
3. Caustics and an underwater camera: assumed in, priority 3, after traced water.

## 7. Next session

Write the design spec to DESIGN_DOC_STANDARD for direct level-set rendering (D2) first: the march pass in the scene renderer, the tracer's custom hit, the item-id and motion outputs, modifiers on the field, the Water Material binding, and the look pass against the FLIP Fluids reference exports. Then the memory track (D4, D5) and APIC on faces (D3). Then the implementation plan: Opus lanes for solver, render and anything that can hard-lock the Mac; Sonnet lanes for decided work (tooling, measurement runs, doc sweeps, mechanical moves); one adversarial review per landing. The two runs (D10) gate the solver lanes only.
