# Third-party notices

MANIFOLD includes code derived from the projects below. Each derived file carries a header line naming its source file and pointing here.

## FLIP Fluids

Source: FLIP Fluids by Ryan L. Guy & Dennis Fassbaender, MIT license. Vendored unchanged at `crates/manifold-fluids/native/flip_engine/` (license copy: `crates/manifold-fluids/native/LICENSE_MIT.md`).

Modules ported from it (all under `crates/manifold-renderer/src/node_graph/primitives/`; each `.rs` has a matching `shaders/<name>_body.wgsl` where one exists):

- `crossing_distance`, `lattice_curvature` — from `particlelevelset.cpp`
- `emission_count`, `energy_potential`, `jitter_particles`, `liquid_cells`, `spawn_whitewater`, `whitewater_type` — from `diffuseparticlesimulation.cpp`
- `wavecrest_potential` — from `diffuseparticlesimulation.cpp` and `interpolation.cpp`
- `extend_lattice` — from `gridutils.h`
- `sample_faces_at_particles`, `shaders/liquid_faces.wgsl` — from `macvelocityfield.cpp`
- `whitewater_cpu.rs`, `whitewater_particle_cpu.rs` (CPU references) — from the files above
- `gpu_flip_step` (`shaders/gpu_flip_step.wgsl`; its CPU references in `gpu_flip_step_tests.rs`) — particles to faces from `velocityadvector.cpp`, the PIC/FLIP blend per step from `fluidsimulation.cpp` (`_ratioPICFLIP`), the extension layer count from `fluidsimulation.cpp` (`_extrapolateFluidVelocities`, with the CFL guard's travel in cells for the engine's CFL number, the one deviation), the particle distance and its extension into solids from `particlelevelset.cpp`, the solid collision and removal of particles from `fluidsimulation.cpp`, the solid distance's gradient from `meshlevelset.cpp` and `interpolation.cpp`, the solid open fractions from `levelsetutils.cpp` and `meshlevelset.cpp`, the solids' face velocity and the constraint from `fluidsimulation.cpp`, divergence, the pressure subtraction and the sealed pockets' solid velocity from `pressuresolver.cpp`, inflow emission, the inflow constrained velocity and outflow removal from `fluidsimulation.cpp` (`_addNewFluidCells`, `_constrainMarkerParticleVelocities`, `_getInflowConstrainedVelocityComponents`, `_updateFluidObjects`); `liquid_fill`'s pool slots and `liquid/bodies.rs`' region rows serve them
- `gpu_flip_pressure` (`shaders/gpu_flip_pressure.wgsl`) — the ghost-fluid free-surface rows from `pressuresolver.cpp`; the conjugate gradient's stop (the infinity-norm residual test, its tolerance and acceptable tolerance, the zero right-hand-side early out) from `pcgsolver.h` and `pressuresolver.cpp`
- `gpu_flip_bodies` (`shaders/gpu_flip_bodies.wgsl`; its CPU references in `gpu_flip_body_tests.rs`) — the bodies' rows in the pressure solve and their captured impulse from `rigidpressurecoupling.h`, the pressure entries and the velocity change on the solid faces from `rigidboundaryvelocity.cpp`, the order of solve, impulse, velocity change and constraint from `rigidfluidcoupling.cpp`; the dynamic bodies' predicted velocity in `gpu_flip_step.wgsl` from `rigidfluidcoupling.cpp`
- `liquid_fill` — seeding only where the solid distance is positive, from `fluidsimulation.cpp`

`crates/manifold-renderer/src/live_sim_clock_reference.rs` ports the CFL duration rule from `fluidsimulation.cpp::_calculateNextTimeStep` (including epsilon, optional surface-tension/color restrictions and equal frame partition). It is a standalone CPU reference, not runtime integration.

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
