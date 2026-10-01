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
