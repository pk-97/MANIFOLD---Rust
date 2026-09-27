# Physics direction — rigid bodies, liquids and interacting materials

**Status:** PROPOSED · 2026-09-27 · Codex. High-level discussion record and evaluation guide, not an executable design contract.
<!-- index: Retain Box3D and FLIP, evaluate XPBD for deformable and mixed materials, and make physics a native musical instrument. -->

MANIFOLD should make physical scenes composable and playable: a scanned sculpture
slumps into liquid, cloth responds to an audio-driven force, or grains deform a
soft object. Peter's reference is “Vellum ... as the gold standard,” integrated
into MANIFOLD rather than requiring Houdini and its licence at runtime.

The preferred direction is to retain Box3D for rigid bodies, retain FLIP Fluids
for detailed liquids, and evaluate a PBD/XPBD framework for deformable and mixed
materials. This offers broad coverage of physical scene materials, not all
physics. No new engine, dependency, GPU port or implementation phase is approved
by this document.

## 1. Existing work and related contracts

Snapshot verified 2026-09-27; active worktree code is not a claim about shipped main.

| Piece | Evidence | Meaning for this direction |
|---|---|---|
| Box3D adapter on main | `crates/manifold-physics/src/lib.rs` (`PhysicsWorld`); [Box3D design](BOX3D_PHYSICS_DESIGN.md) | Preserve the existing rigid-body foundation. |
| Active FLIP integration | Slot-9, inspected at `b7d942d19`: `crates/manifold-fluids/src/lib.rs` (`FluidWorld`, `SurfaceOptions`, `WhitewaterOptions`); `native/PROVENANCE.md` | A CPU liquid engine and explicit native modifications exist in ongoing work. Full coupled-scene acceptance is not established by this document. |
| Shared interaction data | Same workstream: `crates/manifold-physics/src/interaction.rs` (`VectorField`, `TickStamp`), `src/lib.rs` (`BodyDynamics`, `BodyImpulse`) | Reuse existing fields, timing and physical data where suitable; do not invent a parallel physics control system. |
| Prior XPBD direction | [Realtime Simulations](SIMULATIONS_DESIGN.md), D2–D3 | Already describes cloth, ropes and grains as constraint families. Selecting an external library must reconcile its graph decomposition and ownership requirements. |
| Prior water direction | [Water Simulation Design](WATER_SIMULATION_DESIGN.md), [Water Implementation Plan](WATER_IMPLEMENTATION_PLAN.md) | Contains earlier MLS-MPM decisions. This discussion does not silently supersede those contracts. |

The active workstream's `docs/FLUID_ENGINE_INTEGRATION_PLAN.md` owns its detailed
fluid and coupling decisions. Re-read its current version before future design;
do not treat a temporary slot path as a permanent integration dependency.

## 2. Preferred division of responsibilities

| Family | Intended role | Boundaries |
|---|---|---|
| **Box3D** | Rigid objects, contacts, joints and real-time mechanical scenes | Whole-body translation/rotation, not deformation. “Real-time” remains scene- and hardware-dependent. |
| **FLIP Fluids** | Detailed free-surface liquids, viscosity, splashes and cinematic liquid output | CPU integration today. Coarse interactive previews and detailed bakes are useful targets, not guaranteed frame rates. |
| **PBD/XPBD candidate** | Cloth, ropes, jelly-like solids, grains and supported mixed-material interactions | Library, supported material models and performance remain to be evaluated. PBD support does not mean every feature uses XPBD. |

FLIP supplies much more than a pressure solve: particle/grid transfers, liquid
boundaries, sources, obstacles, viscosity, surface reconstruction and secondary
particles. Retain that machinery rather than replacing it solely because another
solver supports liquid too.

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

When this work is scheduled, compare a small set of composition-relevant scenes:
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

GPU ports and neural assistance are later research directions. Learned pressure
estimates/preconditioners could accelerate a solve while residual checks retain
numerical control; neural visual detail could enrich a coarse simulation. Neither
is an established performance result for MANIFOLD. A GPU implementation must fit
`manifold-gpu` and native Metal rather than introduce an unrelated GPU backend.

Photoscan melting is a motivating example, not a promised feature: it needs a
usable initial volume, controlled release/deformation, and appearance transfer as
the surface topology changes. High viscosity alone is not a temperature-driven
phase-change model.

The immediate outcome is a recorded direction: preserve Box3D and FLIP, evaluate
XPBD for the missing material families, and prioritise useful, repeatable musical
scenes over a claim that every solver interacts with everything.
