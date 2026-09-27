# FLIP Fluids native provenance

The vendored engine is FLIP Fluids 1.8.8 at upstream revision
`70a0e954018fe39e1f9c3631264989569752bb7a`.

Except for the local changes documented below, these files are copied byte-for-byte from the pinned upstream
[engine source](https://github.com/rlguy/Blender-FLIP-Fluids/tree/70a0e954018fe39e1f9c3631264989569752bb7a/src/engine):

- all top-level engine `.cpp` and `.h` files selected by the upstream `SOURCES_FLUID_ENGINE_LIBRARY` list
- `pcgsolver/*.h` headers used by the pressure solver
- `mixbox/mixbox.h`
- `mixbox/mixbox_stub.cpp`
- `versionutils.cpp.in` (instantiated by `build.rs` into `OUT_DIR`)

`bridge.cpp`, `bridge.h`, `coupling_probe.*`, `coupling_operator_probe.*`,
`coupling_boundary_probe.*`, `coupling_viscosity_probe.*` and
`coupling_viscosity_operator_probe.*` are
MANIFOLD-owned code. The bridge compiles with
`WITH_MIXBOX=0`, so no external Mixbox runtime or download is required.
The native mutex serializes all engine operations because upstream thread and
mesh-source counters are mutable process-global state.

The upstream MIT license text is preserved in `LICENSE_MIT.md`; each vendored
source file also retains its original license header.

Local changes:

- `flip_engine/fluidsimulation.{h,cpp}` exposes begin/offer/advance/finish
  operations around the existing native frame loop. Owner-driven frames may
  consume smaller substeps, preserve the exact interval and reject budget
  exhaustion instead of forcing a step beyond the stability bound. Abandoned
  or failed sessions require a rebuilt world; outstanding native threads join
  before destruction. The bridge rejects snapshots from those sessions.
- Native output generation and output-only attribute refresh now run at the
  final substep. A translating-liquid probe reproduced a 0.0901 m difference
  between the first-substep mesh centre and completed particle centre; final
  output reduces it to 0.0190 m, within the 0.045 m reconstruction tolerance.
  This establishes liquid output timing, not moving-body contact alignment.
- `flip_engine/levelsetsolver.cpp` initializes the
  `LevelSetSolver::reinitializeUpwind` ping-pong scratch grid with
  `Array3d<float> tempSDF(inputSDF)`, preserving untouched stencil neighbors.
- `flip_engine/pressuresolver.{h,cpp}` exposes the transpose of the solid
  boundary-velocity coefficients as face impulses and accepts optional rigid
  pressure coupling. Coupled solves preserve prescribed boundary velocities,
  start pressure from zero and publish reactions only after success.
- The new MANIFOLD-owned `flip_engine/rigidpressurecoupling.h` supplies the
  mass/inertia contribution to the existing pressure operator, including its
  exact diagonal for preconditioning. `flip_engine/pcgsolver/pcgsolver.h` adds
  an optional matrix-product callback; ordinary callers keep the original
  solver path. No replacement iterative solver is introduced.
- The new MANIFOLD-owned `flip_engine/rigidboundaryvelocity.{h,cpp}` retains
  sparse body-velocity derivatives through native mesh sampling, weighted
  unions, normalization and extrapolation. It supplies the pressure transpose
  and the corresponding boundary-velocity update using prepared storage.
- `flip_engine/meshobject.{h,cpp}` optionally binds an obstacle to that map.
  `flip_engine/meshlevelset.{h,cpp}` records the actual sampled surface point,
  propagates capture identity through unions and rejects stale cached captures.
  Bound obstacles use instantaneous rigid velocity and reject inversion or
  nonphysical velocity scaling; ordinary obstacles retain their existing path.
  Static mesh uploads accept a const reference and reuse translation storage
  so owner-driven rigid pose uploads do not allocate new translation vectors.
- `flip_engine/viscositysolver.{h,cpp}` optionally measures prescribed-boundary
  viscous impulses from the existing normal/shear strain stencil before applying
  the accepted fluid solution. Density converts the native kinematic-viscosity
  terms into momentum; the solver and its ordinary velocity path are retained.
  The MANIFOLD-owned `flip_engine/viscousboundaryreaction.h` uses prepared buffers
  and invalidates rejected captures. Fresh `ViscosityVolumeGrid` dimensions are
  initialized to zero, fixing an observed out-of-bounds access when a new solver
  reused stack storage from a previous solver of the same dimensions.
- The new MANIFOLD-owned `flip_engine/rigidviscositycoupling.h` adds normalized
  body velocity changes to the existing viscosity PCG. Sparse native strain
  terms provide the fluid/body cross terms, body mass/inertia response and exact
  body preconditioner diagonal. The fluid matrix and iterative solver remain
  upstream code. The optional viscosity path consumes the accepted boundary
  derivative, including an optional constraint/friction scale, and exposes both
  solved body changes and measured impulses. Failed coupled solves invalidate
  both outputs and never use the ordinary solver's loose iteration-limit fallback.
- `flip_engine/gridutils.h` exposes an optional owner-thread observer after
  each existing extrapolation layer, preserving the native stencil and scalar
  implementation for both mapped and ordinary callers.
- The MANIFOLD-owned `flip_engine/rigidfluidcoupling.{h,cpp}` connects these
  boundary, viscosity and pressure stages inside `FluidSimulation`. Coupled
  worlds require owner-driven substeps, fresh body inputs before each CFL offer,
  constant physical density and successful stages before exposing reactions.
  Viscosity uses the qualified 1e-9 tolerance; pressure scales surface tension
  with density to preserve the existing kinematic control. Coupled boundaries
  rebuild their derivative each substep and contribute instantaneous rigid
  speed to CFL even when legacy obstacle adaptivity is disabled. The native
  bridge prepares bindings and retained buffers once and validates whole body
  batches before updating geometry. It symmetrizes only f32-sized rounding
  differences in exported inertia (within eight float epsilon times the tensor
  scale), then applies the existing double-precision PSD check.

The bounded native probes establish pressure-stage algebra, force/torque,
energy, closed-pocket constraints and the boundary map's interpolation/transpose
against native mesh velocities. Prescribed viscous boundaries are checked for
linear/angular momentum balance, dissipation, rigid-motion invariance, density
scaling and failure atomicity. A separate frozen-geometry energy test rejects
delayed viscous feedback for light bodies/high viscosity. The joint alternative
passes an independent two-body physical-mass oracle and 33 frozen-geometry
native cases covering translation/rotation, light/heavy bodies and constrained
boundary derivatives. Eight cases use planar free surfaces through or just
above a moving-velocity boundary. Separate production tests now run the combined
native frame loop with real Box3D hulls at density ratios 0.1/1/10 and kinematic
viscosity 0/1 m²/s. Across three frames, immediate Box3D velocity changes match
the FLIP-solved changes within 1.77e-7 (limit 5e-5). Inputs update each substep;
Box3D advances its pose after accepting the reaction. Empty/disabled recipients,
prescribed density scaling, invalid inputs, stale reactions and abandonment
are covered, along with two-body exchange order, preparation/retry and CFL
bounds for prescribed proxies crossing the domain with every vertex outside.
These short exchanges do not establish sustained energy bounds,
floating equilibrium or final moving-contact alignment. App worker ownership,
authored density and production memory/performance qualification remain required in
[`FLUID_ENGINE_INTEGRATION_PLAN.md`](../../../docs/FLUID_ENGINE_INTEGRATION_PLAN.md).
