# Physics direction — rigid bodies, liquids and interacting materials

**Status:** APPROVED direction · 2026-09-28 · Codex, agreed with Peter. High-level discussion record and evaluation guide, not an executable design contract or authorization to begin a GPU port.
<!-- index: Shared physics with Box3D, CPU/GPU liquid backends and XPBD candidates; bounded coupling and cinematic-pipeline evaluation. -->

MANIFOLD should make physical scenes composable and playable: a scanned sculpture
slumps into liquid, cloth responds to an audio-driven force, or grains deform a
soft object. Peter's reference is “Vellum ... as the gold standard,” integrated
into MANIFOLD rather than requiring Houdini and its licence at runtime.

The agreed direction is to retain Box3D for rigid bodies, preserve FLIP Fluids'
detailed liquid capabilities and cinematic output while evaluating a GPU FLIP/APIC
backend, and evaluate a PBD/XPBD framework for deformable and mixed materials.
These share scene authoring and interaction contracts through `manifold-physics`.
This offers broad coverage of physical scene materials, not all physics. No new
engine, dependency or implementation phase is authorized by this document.

## 1. Existing work and related contracts

Initial snapshot verified 2026-09-27, GPU/history references checked 2026-09-28;
active worktree code is not a claim about shipped main. Extend existing infrastructure.

| Piece | Evidence | Meaning for this direction |
|---|---|---|
| Box3D adapter on main | `crates/manifold-physics/src/lib.rs` (`PhysicsWorld`); [Box3D design](BOX3D_PHYSICS_DESIGN.md) | Preserve the existing rigid-body foundation. |
| Active FLIP integration | Slot-9, inspected at `b7d942d19`: `crates/manifold-fluids/src/lib.rs` (`FluidWorld`, `SurfaceOptions`, `WhitewaterOptions`); `native/PROVENANCE.md` | A CPU liquid engine and explicit native modifications exist in ongoing work. Full coupled-scene acceptance is not established by this document. |
| Shared interaction data | Same workstream: `crates/manifold-physics/src/interaction.rs` (`VectorField`, `TickStamp`), `src/lib.rs` (`BodyDynamics`, `BodyImpulse`) | Reuse existing fields, timing and physical data where suitable; do not invent a parallel physics control system. |
| Prior XPBD direction | [Realtime Simulations](SIMULATIONS_DESIGN.md), D2–D3 | Already describes cloth, ropes and grains as constraint families. Selecting an external library must reconcile its graph decomposition and ownership requirements. |
| Prior water experiment | `wave/live-water` checkpoint `8f3cdd23f`, `WORKTREE_HANDOFF.md`; [Water Simulation Design](WATER_SIMULATION_DESIGN.md), [Water Implementation Plan](WATER_IMPLEMENTATION_PLAN.md) | Started as GPU MLS-MPM; the preserved branch also contains a GPU MAC-grid APIC replacement. Numerical and visual acceptance remain unresolved. Reusable evidence and code, not a verified real-time backend; main's older documents do not describe the complete branch history. |
| Existing GPU infrastructure | [GPU architecture](MANIFOLD_GPU_ARCHITECTURE.md); `crates/manifold-gpu/src/lib.rs`, `metal/shader_compiler.rs` | Native Metal, a Vulkan backend path and shared WGSL/SPIR-V translation already exist. Use these; native cross-vendor fluid validation remains required. |

The active workstream's `docs/FLUID_ENGINE_INTEGRATION_PLAN.md` owns its detailed
fluid and coupling decisions. Re-read its current version before future design;
do not treat a temporary slot path as a permanent integration dependency.

## 2. Preferred division of responsibilities

| Family | Intended role | Boundaries |
|---|---|---|
| **Box3D** | Rigid objects, contacts, joints and real-time mechanical scenes | Whole-body translation/rotation, not deformation. “Real-time” remains scene- and hardware-dependent. |
| **Liquid backends** | Detailed free-surface liquids, viscosity, splashes and cinematic liquid output | GPU FLIP in the show; CPU FLIP Fluids remains a proof reference only. Neither real-time performance nor matching trajectories across backends is promised. |
| **PBD/XPBD candidate** | Cloth, ropes, jelly-like solids, grains and supported mixed-material interactions | Library, supported material models and performance remain to be evaluated. PBD support does not mean every feature uses XPBD. |

FLIP supplies much more than a pressure solve: particle/grid transfers, liquid
boundaries, sources, obstacles, viscosity, surface reconstruction and secondary
particles. Preserve those capabilities and use the existing engine as a reference;
reuse implementations where practical. Preserving the cinematic result does not
require retaining every CPU algorithm. A GPU backend extends this liquid family,
not a fourth material family or a parallel scene-control system.

APIC describes particle/grid motion transfer; MAC describes a staggered grid
layout; MLS-MPM describes a simulation formulation. CPU/GPU describes execution
hardware. These are different choices, not interchangeable names for full engines.

XPBD's appeal is shared constraint machinery. Different constraints express
stretch, bending, volume preservation and contact, allowing several materials to
participate in one solve. It is not inherently slower, more accurate or less
accurate than every alternative; compare implementations at matched quality.

This division is a preferred direction, not a mandate that every scene runs all
three engines. An XPBD-owned mixed scene may use that framework's own rigid-body
support where appropriate. One body must have one authoritative simulation owner.

## 3. What the shared API does—and does not do

`manifold-physics` can provide common physical data and interaction boundaries;
an API does not by itself make independent solvers physically consistent.

Box3D solves rigid contacts while FLIP solves pressure and viscosity. Their
coupling needs consistent time, geometry, body mass/inertia and equal-and-opposite
reactions. A deformable solver adds a different boundary: its surface changes
shape, not just position and orientation.

Within a shared XPBD framework, materials use common solving machinery. Across
engines, each supported interaction needs an explicit coupling method and tests.
Do not assume that adding XPBD gives automatic cloth–FLIP or XPBD–Box3D coupling.
Do not simulate the same object in two engines and blend conflicting results.

Peter wants an “open” coupling interface that makes future backend work easy.
Derive it from the existing CPU liquid–Box3D path and a bounded GPU liquid–Box3D
proof, rather than designing a universal framework before either consumer needs it.
The shared contract should describe supported capabilities and exchange geometry,
poses/velocities, mass/inertia, forces/impulses and accepted tick/epoch identities.
Keep backend buffer layouts and solver-specific numerical operators behind adapters.
Unsupported interactions must be explicit; do not substitute approximate buoyancy
or drag while advertising equivalent pressure/viscosity coupling.

Preserve one authoritative owner per body, coherent substep exchange, once-only
reaction application and publication of matching liquid/rigid states. The current
pressure and viscosity coupling changes the numerical solve itself: a generic GPU
pressure port does not automatically preserve it. A deformable boundary will need
its own supported geometry-update and reaction contract. Shared infrastructure
should make that work easier, not imply it has already been solved.

Preserve content-thread ownership of authored state, commands and undo, existing
parameter/modulation infrastructure, and beat-based composition. Native simulation
steps use physical time. Solver outputs feed ordinary scene rendering; a physics
backend should not introduce its own material system or renderer.

## 4. Candidates and research

- [PositionBasedDynamics](https://github.com/InteractiveComputerGraphics/PositionBasedDynamics):
  C++/MIT library with PBD/XPBD constraints, deformables, rods, rigid bodies and
  position-based fluids. First evaluation candidate, not a proven Vellum replacement.
  Its [collision video](https://www.youtube.com/watch?v=x_Iq2yM4FcA) and
  [stiff-rod video](https://www.youtube.com/watch?v=EFH9xt4omls) use the library.
- [Gaia](https://github.com/AnkaChan/Gaia): Apache-2.0 C++ framework offering XPBD
  and Vertex Block Descent. Compare deformation and contact capabilities; its
  documented Windows testing does not establish macOS suitability.
- [Offset Geometric Contact](https://graphics.cs.utah.edu/research/projects/ogc/):
  contact research relevant to thin deformable surfaces. It is a collision/contact
  component, not a liquid solver or complete multiphysics engine. Published RTX
  timings are not MANIFOLD/Metal performance measurements.
- [SPlisHSPlasH](https://github.com/InteractiveComputerGraphics/SPlisHSPlasH):
  MIT particle-fluid framework with existing PositionBasedDynamics rigid coupling;
  useful reference, not a decision to replace FLIP.

Houdini's [Vellum material interactions](https://www.sidefx.com/docs/houdini/vellum/fluidsoftbodies.html)
are the capability reference. Its [Vellum/FLIP comparison](https://www.sidefx.com/docs/houdini/vellum/vellumvsflip.html)
also illustrates why specialised liquid and shared-material solvers can coexist.

Core project licences do not clear all dependencies, assets or bundled components.
Bead **BUG-jp7k** tracks removal of the non-commercial Mixbox material found in the
FLIP integration before distribution; disabling the feature is not its acceptance
criterion. Audit any candidate's actual imported files before adoption.

## 5. Evaluation before implementation commitment

First land the current CPU integration and complete the useful scene → simulate →
Bake → cached playback → export workflow. Comprehensive future cache generalisation,
new solvers and research experiments must not become prerequisites for that delivery.

The first GPU milestone, when separately scheduled, is one ordinary Manifold scene
with a GPU liquid, a meshed surface and a coupled Box3D body, compared with CPU FLIP.
Use a small dense grid and the existing `manifold-gpu` infrastructure. Peter agreed
that Slang is unnecessary here; do not introduce a new shader toolchain or unrelated
GPU abstraction. Preserve portable buffer/kernel contracts and verify actual Vulkan
hardware before claiming that backend supported; cross-platform expansion need not
block the initial Metal feasibility result.

Prove the existing discrete physics and early two-way coupling before changing
timestepping or grid resolution policy. Audit the old APIC experiment for reusable
kernels and fixtures, retaining its unresolved failures as evidence. Reuse the
existing capture runner, stage timings and proofs rather than building a second
benchmark framework. Pin the CPU source/settings and comparison scenes; the CPU
implementation is a reference, not mathematical ground truth.

Test GPU particle output through surface generation and rendering early. The
existing CPU mesher can be reused through an adapter, but its cost and CPU/GPU
synchronization remain. Whitewater needs velocity and boundary/surface data, not
just particle positions. Keep state GPU-resident where practical and exchange
compact rigid-body data; measure any required synchronization. Preserve a proper
surface path for cinematic output; screen-space previews are optional.

The milestone passes only with declared numerical tolerances, an observed visual
comparison, memory measurements and improved end-to-end time at matched settings.
Measure wall time per simulated second and display throughput separately, including
transfers, pressure/viscosity, collisions/coupling, meshing, whitewater where enabled,
rendering and export overhead. Fix scene/resolution/tolerances during comparisons;
never trade them away silently for a speedup. Encoded video FPS, isolated kernel
timings and paper speedups are not real-time acceptance. No fixed acceleration
factor or 30/60 FPS guarantee follows from the existing demo measurements.

XPBD evaluation is independent of GPU-liquid development: neither must finish
before the other. They share the physics API and product workflow, with concrete
cross-engine interactions evaluated when needed.

For XPBD evaluation, compare a small set of composition-relevant scenes:
cloth draped over moving objects, a deformable scanned shape, rope under tension,
and grains interacting with a soft body. Include a mixed liquid scene only where
the candidate actually supports that interaction; do not infer it from feature lists.

Measure simulation, collision, surface generation, upload and rendering separately
on target Apple hardware. Record material error, penetration, energy behaviour and
long-run stability alongside timing. Include resets, changing controls, bake/replay
and export in the eventual application acceptance criteria. An attractive video
or successful compile is not those measurements.

The strongest alternative is to put a complete mixed scene in one framework. It
reduces cross-engine integration but may give up specialised rigid/liquid features.
Compare that with keeping specialised engines; do not assume either wins in advance.

## 6. Coverage limits and future directions

The three families cover many intended scenes, not smoke/fire combustion,
electromagnetism, fracture, or every plastic material model. Sand, clay and snow
also need appropriate friction/yielding models; the label XPBD alone supplies none
of those guarantees. MPM and other methods remain possible future candidates when
a concrete scene exposes a gap.

FLIP is a numerical technique; **FLIP Fluids is the particular liquid engine**
being integrated. The name does not imply a smoke/fire backend. Gas simulation
needs additional fields and behaviour, such as temperature and buoyancy, with
combustion for physically modelled fire. Existing smoke-style visual fluid effects
do not establish that complete capability.

**Fracture** means creating cracks and separating an object into pieces. Box3D
can simulate the resulting rigid chunks, but fracture geometry and break criteria
are additional systems. Pre-fractured pieces connected by breakable constraints
are a simpler possible approach than generating new cracks during simulation;
neither is selected here.

Research guides measured improvements after a trustworthy GPU baseline; it is not
a preapproved sequence of implementation phases:

- [Leapfrog Flow Maps](https://yuchen-sun-cg.github.io/projects/lfm/) offers a GPU
  multigrid-preconditioned pressure reference. Its vortical-flow demonstrations do
  not establish free-surface liquid or coupled-body performance here. Preserve our
  discrete equations and checked convergence when evaluating a different solver.
- [Spatiotemporal FLIP](https://vci.rwth-aachen.de/publication/05101/) motivates
  larger timesteps. It changes particle deposition and pressure-projection weights
  together; it is not just a transfer-kernel swap. Revalidate moving boundaries,
  viscosity and rigid coupling. Display FPS remains independent of physical steps,
  while authored event timing must survive any scheduling change.
- Sparse block allocation at unchanged resolution can precede adaptive resolution.
  Retain the liquid interior needed by pressure plus motion/interpolation padding.
  [Cirrus](https://wang-mengdi.github.io/proj/25-cirrus/) is a reference for GPU
  adaptivity; refinement/coarsening changes numerical behaviour and needs separate
  conservation, boundary and coupling checks.

Water–air simulation, specialised deep-water grids, neural assistance and a
standalone engine are later candidates only when a concrete workload warrants them.
Learned pressure estimates could accelerate a solve while residual checks retain
numerical control; visual detail synthesis does not establish physical accuracy.
None should delay a useful accelerated single-phase liquid workflow.

Photoscan melting is a motivating example, not a promised feature: it needs a
usable initial volume, controlled release/deformation, and appearance transfer as
the surface topology changes. High viscosity alone is not a temperature-driven
phase-change model.

The immediate outcome is a recorded direction: preserve Box3D and FLIP's liquid
capabilities, evaluate a compatible GPU liquid backend and XPBD for missing material
families, and prioritise useful, repeatable musical scenes. Implementation work
requires its own bounded scope; this document does not authorize an open-ended
engine rewrite or promise that every solver interacts with everything.
