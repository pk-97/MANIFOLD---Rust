# Third-party notices

MANIFOLD includes code derived from the projects below. Each derived file carries a header line naming its source file and pointing here.

## FLIP Fluids

Source: FLIP Fluids by Ryan L. Guy & Dennis Fassbaender, MIT license. Vendored unchanged at `crates/manifold-fluids/native/flip_engine/` (license copy: `crates/manifold-fluids/native/LICENSE_MIT.md`).

Modules ported from it (all under `crates/manifold-renderer/src/node_graph/primitives/`; each `.rs` has a matching `shaders/<name>_body.wgsl` where one exists):

- `gpu_flip_step` (`shaders/gpu_flip_step.wgsl`) — particles to faces from `velocityadvector.cpp`, the particle distance from `particlelevelset.cpp`, the solid open fractions from `levelsetutils.cpp` and `meshlevelset.cpp`, the solids' face velocity and the constraint from `fluidsimulation.cpp`, divergence and the pressure subtraction from `pressuresolver.cpp`
- `gpu_flip_pressure` (`shaders/gpu_flip_pressure.wgsl`) — the ghost-fluid free-surface rows from `pressuresolver.cpp`
- `particle_distance` — from `particlelevelset.cpp`
- `particles_to_faces` — from `velocityadvector.cpp`
- `pressure_residual`, `pressure_smooth`, `subtract_pressure` — from `pressuresolver.cpp` (the ghost-fluid free-surface rows)
- `face_divergence` (the solids' term in its shader) — from `pressuresolver.cpp`
- `body_pressure_product`, `pressure_face_impulse` — from `pressuresolver.cpp` and `rigidboundaryvelocity.cpp`
- `face_impulse_to_bodies` — from `rigidboundaryvelocity.cpp`
- `constrain_solid_faces` — from `fluidsimulation.cpp`
- `solid_face_velocity` — from `fluidsimulation.cpp` and `meshlevelset.cpp`
- `solid_faces` — from `levelsetutils.cpp` and `meshlevelset.cpp`

The GPU structure (atoms, the multigrid preconditioner, the fused step) is MANIFOLD's own; the ported parts are the rules above.

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
