# Third-party notices

MANIFOLD includes code derived from the projects below. Each derived file carries a header line naming its source file and pointing here.

## FLIP Fluids

Source: FLIP Fluids by Ryan L. Guy & Dennis Fassbaender, MIT license. Vendored unchanged at `crates/manifold-fluids/native/flip_engine/` (license copy: `crates/manifold-fluids/native/LICENSE_MIT.md`).

Modules ported from it (all under `crates/manifold-renderer/src/node_graph/primitives/`; each `.rs` has a matching `shaders/<name>_body.wgsl` where one exists):

- `crossing_distance`, `lattice_curvature` — from `particlelevelset.cpp`
- `emission_count`, `energy_potential`, `jitter_particles`, `liquid_cells`, `spawn_whitewater`, `whitewater_type` — from `diffuseparticlesimulation.cpp`
- `wavecrest_potential` — from `diffuseparticlesimulation.cpp` and `interpolation.cpp`
- `turbulence_field` — from `turbulencefield.cpp`
- `turbulence_emission_count`, `inside_turbulence_potential`, `dust_potential`, `whitewater_emitter_velocity`, `whitewater_emitter_cpu.rs` (CPU reference) — turbulence, inside and dust emission, the spray speed factor and the generation coin from `diffuseparticlesimulation.cpp`
- `whitewater_influence`, `whitewater_obstacle_source` — the obstacle influence grid from `influencegrid.cpp`, `fluidsimulation.cpp` and `diffuseparticlesimulation.cpp`
- `extend_lattice` — from `gridutils.h`
- `sample_faces_at_particles`, `shaders/liquid_faces.wgsl` — from `macvelocityfield.cpp`
- `whitewater_cpu.rs`, `whitewater_particle_cpu.rs` (CPU references) — from the files above
- `gpu_flip_step` (`shaders/gpu_flip_step.wgsl`; its CPU references in `gpu_flip_step_tests.rs`) — particles to faces from `velocityadvector.cpp`, the PIC/FLIP blend per step from `fluidsimulation.cpp` (`_ratioPICFLIP`), the extension layer count from `fluidsimulation.cpp` (`_extrapolateFluidVelocities`, ceil(sqrt(3) * CFL) + 3 with the configured CFL 5), the particle distance and its extension into solids from `particlelevelset.cpp`, the solid collision and removal of particles from `fluidsimulation.cpp`, the solid distance's gradient from `meshlevelset.cpp` and `interpolation.cpp`, the solid open fractions from `levelsetutils.cpp` and `meshlevelset.cpp`, the solids' face velocity and the constraint from `fluidsimulation.cpp`, divergence, the pressure subtraction and the sealed pockets' solid velocity from `pressuresolver.cpp`, projected-velocity extrapolation before solid constraint, step-end inflow emission, the 250-marker cell cap and stable survivor compaction, the inflow constrained velocity and outflow removal from `fluidsimulation.cpp` (`_addNewFluidCells`, `_constrainMarkerParticleVelocities`, `_getInflowConstrainedVelocityComponents`, `_updateFluidObjects`); `liquid_fill`'s pool slots and `liquid/bodies.rs`' region rows serve them
- `gpu_flip_pressure` (`shaders/gpu_flip_pressure.wgsl`) — the ghost-fluid free-surface rows from `pressuresolver.cpp`; the conjugate gradient's stop (the infinity-norm residual test, its tolerance and acceptable tolerance, the zero right-hand-side early out) from `pcgsolver.h` and `pressuresolver.cpp`
- `gpu_flip_bodies` (`shaders/gpu_flip_bodies.wgsl`; its CPU references in `gpu_flip_body_tests.rs`) — the bodies' rows in the pressure solve and their captured impulse from `rigidpressurecoupling.h`, the pressure entries and the velocity change on the solid faces from `rigidboundaryvelocity.cpp`, the order of solve, impulse, velocity change and constraint from `rigidfluidcoupling.cpp`; the dynamic bodies' predicted velocity in `gpu_flip_step.wgsl` from `rigidfluidcoupling.cpp`
- `liquid_fill` (`shaders/liquid_fill_body.wgsl`) — the half-cell seeding lattice, and seeding only where the solid distance is positive, from `fluidsimulation.cpp`
- `shaders/matter_frame.wgsl` — the marker radius from rest volume, from `fluidsimulation.cpp` (`_initializeParticleRadii`)
- `shaders/marching_cubes_common.wgsl` — the corner order, edge order and triangle table from `polygonizer3d.cpp` (Paul Bourke's tables)
- `scripts/mgpcg_reference.py` (the f64 oracle for the pressure solve) — the segment and square inside-fractions from `levelsetutils.cpp`, the operator and stop from `pressuresolver.cpp` and `pcgsolver.h`

Constants taken from the engine (each file's header says which):

- `particle_volume`, `shape_particle_blobs`, `lattice_bricks`, `clamp_liquid_to_solids` (and their shaders) — the marker radius, inclusive field support, distance band and border/solid rules from `fluidsimulation.cpp`, `particlemesher.cpp` and `scalarfield.cpp`.
- `relax_surface_mesh` (and its shader) — the neighbour-mean smoothing from `trianglemesh.cpp` (`smooth`)
- `node_graph/whitewater.rs` — the particle id limit from `diffuseparticlesimulation.h` (`_diffuseParticleIDLimit`)
- `gpu_flip_preset.rs` — the PIC/FLIP ratio from `fluidsimulation.h` (`_ratioPICFLIP`) and the Dam Break scene values
- `matter_face_component` — the extrapolation layer count from `fluidsimulation.cpp` (`_extrapolateFluidVelocities`)

The C++ bridge in `crates/manifold-fluids/native/` (`bridge.*`, `coupling_*probe.*`) is MANIFOLD's own; it includes the vendored headers and copies no engine code.

Checked against the engine, no engine code in them (each file's header says so):

- `gpu_flip_pressure_tests.rs` — the pressure solve's stop, against `pressuresolver.cpp` and `pcgsolver.h`
- `gpu_flip_body_tests.rs` — the bodies' reaction, against `rigidfluidcoupling.cpp`
- `whitewater_field_tests.rs` — a field's rule, against `particlelevelset.cpp`
- `liquid_surface_tests.rs` — the marching-cubes tables, against `polygonizer3d.cpp`
- `gpu_flip_preset.rs`, `gpu_flip_race_tests.rs`, `gpu_flip_render_smoke_tests.rs`, `fluid/race_probe.rs` — the engine's Dam Break scene and its race numbers
- `liquid/conformance.rs`, `tests/gpu_proofs/liquid_conformance.rs` — the engine's coupled tank (its gravity tests)
- `manifold-fluids/src/whitewater_oracle.rs` — runs the engine's whitewater emitter and curvature as test oracles

`crates/manifold-renderer/src/live_sim_clock_reference.rs` ports the CFL duration rule from `fluidsimulation.cpp::_calculateNextTimeStep` (including epsilon, optional surface-tension/color restrictions and equal frame partition). It is a standalone CPU reference, not runtime integration.

`crates/manifold-physics/src/stepping.rs` ports `_calculateNextTimeStep`, the internal final-substep remainder rule in `nextUpdateTimeStep`, and `_getMarkerParticleSpeedLimit`, including MANIFOLD's minimum speed-limit protection. The GPU clock (`crates/manifold-renderer/src/node_graph/primitives/gpu_flip_clock.rs` and `shaders/gpu_flip_clock.wgsl`) ports the same CFL scheduling rule, marker/source maximum-speed calculation, the complete `_getMarkerParticleSpeedLimit` policy over the accepted frame interval, and `rigidfluidcoupling.cpp::pointSpeed` endpoint bound. These ports retain the FLIP Fluids MIT attribution to Ryan L. Guy and Dennis Fassbaender. The runtime GPU FLIP step uses this clock and removes extreme markers before survivor compaction and inflow emission.

The GPU structure (the step's passes, the multigrid preconditioner) is MANIFOLD's own; the ported parts are the rules above.

### License

```
Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## blub

Source: blub by Andreas Reich (github.com/Wumpf/blub), MIT license.

Ported from it: the density projection in `gpu_flip_step` (`shaders/gpu_flip_step.wgsl` entry `density_source`, its CPU-side wiring in `gpu_flip_step.rs`) from `density_projection_gather_error.comp`: the tent-kernel cell density, the 0.5625 solid face weight (extended here to edge, corner and per-site body weights), the rest clamp beside air and the source clamp, as blub builds Kugelstadt et al. 2019.

### License

```
MIT License

Copyright (c) 2020 Andreas Reich

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
