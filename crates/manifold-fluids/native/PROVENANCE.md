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

`bridge.cpp`, `bridge.h`, `coupling_probe.*`, `coupling_operator_probe.*` and
`coupling_boundary_probe.*` and `coupling_viscosity_probe.*` are
MANIFOLD-owned code. The bridge compiles with
`WITH_MIXBOX=0`, so no external Mixbox runtime or download is required.
The native mutex serializes all engine operations because upstream thread and
mesh-source counters are mutable process-global state.

The upstream MIT license text is preserved in `LICENSE_MIT.md`; each vendored
source file also retains its original license header.

Local changes:

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
- `flip_engine/viscositysolver.{h,cpp}` optionally measures prescribed-boundary
  viscous impulses from the existing normal/shear strain stencil before applying
  the accepted fluid solution. Density converts the native kinematic-viscosity
  terms into momentum; the solver and its ordinary velocity path are retained.
  The MANIFOLD-owned `flip_engine/viscousboundaryreaction.h` uses prepared buffers
  and invalidates rejected captures. Fresh `ViscosityVolumeGrid` dimensions are
  initialized to zero, fixing an observed out-of-bounds access when a new solver
  reused stack storage from a previous solver of the same dimensions.
- `flip_engine/gridutils.h` exposes an optional owner-thread observer after
  each existing extrapolation layer, preserving the native stencil and scalar
  implementation for both mapped and ordinary callers.

The bounded native probes establish pressure-stage algebra, force/torque,
energy, closed-pocket constraints and the boundary map's interpolation/transpose
against native mesh velocities. Prescribed viscous boundaries are checked for
linear/angular momentum balance, dissipation, rigid-motion invariance, density
scaling and failure atomicity. `FluidSimulation` does not yet enable production
two-way coupling. Enabling the boundary map, viscous feedback and Box3D timing
remain integration requirements in
[`FLUID_ENGINE_INTEGRATION_PLAN.md`](../../../docs/FLUID_ENGINE_INTEGRATION_PLAN.md).
