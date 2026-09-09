# Realtime Simulations — XPBD Cloth/Ropes, Dedicated Water, Baked Playback

**Status:** APPROVED design, not built · 2026-07-03 · Fable; water split supersedes liquid decisions below, 2026-09-09 · Astra.
**Prerequisites: MATERIAL_SYSTEM M1–M5 (sims render through materials); REALTIME_3D P1
(`node.render_scene`) for scene composition — cloth can smoke-test through
`node.render_mesh` before it. Vocab-audit apply first (post-rename ids used
throughout).**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) and section 8 (Execution protocol (how a phase is run)) before starting
any phase.**

Peter's directives (2026-07-03): realtime simulations "like Nuke and Houdini";
realistic water explicitly wanted; approved the full shape: three lanes ordered
bridge → live solver → volumes. **Amended by Peter, 2026-09-09:** dedicated
MLS-MPM water proceeds independently of XPBD cloth/ropes and baked import. The old
cloth-first/PBF-first water prerequisite is retired. Water's detailed specification
and proof checkpoints live in [WATER_SIMULATION_DESIGN.md](WATER_SIMULATION_DESIGN.md)
and [WATER_IMPLEMENTATION_PLAN.md](WATER_IMPLEMENTATION_PLAN.md).
Competitive frame: **Notch** (realtime stage sims), not offline Houdini — Houdini is
the authoring world lane 1 borrows from.

Companions: `REALTIME_3D_DESIGN.md` (the scene sims render into; its section 8 (Deferred (with triggers)) pins lane 1),
`MATERIAL_SYSTEM_DESIGN.md` (shading), `CAPABILITY_ROADMAP.md` section 4.1 (vertex-cache
origin).

---

## 1. Audit — what exists (verified 2026-07-03)

| Piece | Where | State |
|---|---|---|
| Particle atom suites (2D+3D): spawn/forces/turbulence/move/draw | `primitives/` (post-vocab: `node.spawn_particles`, `node.move_particles_3d`, `node.turbulence_3d`, `node.draw_particles_3d`, `node.keep_in_box_3d`, …) | **The precedent** — sims are already decomposed atom pipelines here |
| Stateful ping-pong | `node.array_feedback` / `node.feedback` | Zero-copy state between frames; the sim-state carrier |
| Fluid sims | FluidSim2D / FluidSim3D / OilyFluid presets | Velocity-field (smoke-style) sims. **No free-surface liquid anywhere** — water is genuinely new |
| Mesh arrays | `Array(MeshVertex)`, `node.make_triangles`, `node.grid_mesh` | Cloth's topology source and its render output type |
| State keying rules | state store + chain caches, (node, island) | Two-cache rule applies to solver state |

`⚠ VERIFY-AT-IMPL`: re-verify all anchors; material + 3D-scene phases land between
this doc and execution.

## 2. Decisions

- **D1 — Three capability lanes; live water now independent (2026-09-09).**
  **Lane 1 — baked playback:** Houdini/Blender-quality sims (incl. photoreal FLIP
  water) baked to per-frame vertex caches, streamed as mesh sequences, **beat-retimed**
  (`beat_ramp` scrubs the playhead; loop a bar; freeze on a trigger). Designed:
  `docs/IMPORT_DESIGN.md` P3 (MDD/PC2 streaming + `node.mesh_sequence`). **Lane 2 — live XPBD cloth/ropes (this doc) and dedicated MLS-MPM water (WATER_SIMULATION_DESIGN).** **Lane 3 — volume rendering**
  (raymarched smoke/pyro + baked VDB) — deferred section 8; baked VDB may cover most stage
  needs first.
- **D2 — XPBD families as constraint recipes; water is separate. No monoliths.** XPBD
  (position-based dynamics — the same family Houdini Vellum, Unreal and Unity cloth
  are built on). Cloth, grains, ropes, soft bodies = different constraint
  sets over the same particle arrays and the same solver atom. A fused `cloth_sim`
  node is the named forbidden move — this extends the particle-suite decomposition,
  same doctrine as everything else.
- **D3 — Cloth → grains/ropes within XPBD; MLS-MPM water independently.**
  Supersedes the 2026-07-16 PBF-first fallback ladder. Do not build PBF liquid
  constraints under this document. Water uses a dedicated grid/particle pipeline,
  bounded graph substeps and screen-space surface; its numerical and visual proofs
  are in WATER_SIMULATION_DESIGN. Shared scheduling is reusable infrastructure, not
  a reason to force different physical models into one solver. Published demo particle
  counts are not MANIFOLD performance measurements.
- **D4 — Fixed-substep time.** XPBD needs stable dt: the solver runs fixed substeps
  (default 1/240 s) accumulated from the content clock; iteration count and substep
  are params. Consequences: deterministic re-runs at fixed export FPS (same
  guarantee as existing sims), and a **time-scale param that beat-ramps cleanly**
  (slow-motion cloth on the breakdown).
- **D5 — Collision v1 = analytic shapes; scene-mesh SDF = v2.** A collider-list atom
  (planes, spheres, boxes — port-shadowed transforms, so a collider can dance).
  Baking `render_scene` objects to SDF volumes is real infra — deferred with its
  trigger (section 8).
- **D6 — Liquid surface moved to WATER_SIMULATION_DESIGN section 7.** That
  contract owns camera/depth conventions, surface reconstruction, material reuse,
  scene pass order and the explicit MVP transparency limits.
- **D7 — Sim outputs are ordinary wires.** Cloth emits `Array(MeshVertex)` →
  feeds a `render_scene` object input (or `node.render_mesh`) and gets materials,
  lights and shadows through the renderer. Water's typed particle/surface wires are
  specified in WATER_SIMULATION_DESIGN. Grains/ropes → instanced copies. No sim has
  a private renderer.
- **D8 — Beat-native is the differentiator (the layer Notch doesn't have).** Wind,
  gravity, stiffness, viscosity, time-scale: all port-shadowed → audio-reactive
  cloth, a fluid that thickens on the drop. Reseed/reset quantized to the bar via
  the existing trigger machinery. No new modulation mechanism.

## 3. Atom sketch (names reconciled at section 2.5 audit)

| Atom | One purpose |
|---|---|
| `node.cloth_from_grid` | Grid mesh → particle array + distance/bend constraint arrays (pin map param) |
| `node.rope_from_points` | Point chain → rope constraints |
| `node.solve_constraints` | The XPBD step: substeps × iterations over (particles, constraints, colliders) |
| `node.collide_shapes` | Analytic collider list (plane/sphere/box) consumed by the solver |
| `node.cloth_to_mesh` | Solved particles + topology → `Array(MeshVertex)` (normals recomputed) |

Solver state (positions, prev-positions, velocities) rides `array_feedback`-style
persistent buffers, keyed per the two-cache rules.

## 4. What it buys on stage

- A silk sheet the size of the stage, blowing in audio-reactive wind, lit and
  shadowed by the scene — torn down on the drop (pin release on trigger).
- Water pouring and pooling live, thickening with the bass; the photoreal ocean
  crash is a lane-1 bake that breaks exactly on the downbeat.
- Slow-motion everything: time-scale on a fader.

## 5. Phasing

Forbidden, all phases: fused per-effect sim nodes (D2) · private renderers (D7) ·
variable-dt integration (D4) · new modulation machinery (D8) · per-frame allocation
in solver loops (pre-allocated constraint/particle buffers only).

- **P1 — Solver core + cloth.** `solve_constraints`, `cloth_from_grid`,
  `collide_shapes`, `cloth_to_mesh`; a bundled Cloth preset rendered through
  `render_mesh` (or `render_scene` if landed). Read-back: this doc; particle-suite
  atoms end-to-end; `array_feedback` state rules. Gate: gpu_tests — a pinned cloth
  under gravity settles to known sag (value-level vertex positions, fixed seed);
  collision keeps particles outside a sphere; determinism — two identical runs,
  identical buffers. Full workspace sweep (new stateful runtime pattern = infra).
- **P2 — Liquids: SUPERSEDED, not implemented.** Execution moved to
  WATER_IMPLEMENTATION_PLAN S1–S8. PBF work under this phase is retired.
- **P3 — Grains + ropes.** Constraint recipes only — solver untouched. Gate: recipe
  unit tests + demo presets.
- **P4 — Beat wiring + polish.** Bar-quantized reseed examples, time-scale ramp
  preset, perf HUD line (particles × iterations), caps documented.

## 6. Performance (stated honestly)

XPBD cost = particles × constraints × iterations × substeps. The earlier cloth
~64k–256k and liquid ~100k–500k counts were planning estimates, not measured support.
Water's explicit first-scene timing/memory targets and pass/fail checkpoint are now
in WATER_SIMULATION_DESIGN section 8. Measure cloth separately when its lane starts.

## 7. Decided — do not reopen

1. Three capability lanes; live water has no baked-import or cloth prerequisite.
   Baked playback lives in the import design; performance is the live frame.
2. XPBD for cloth/ropes/grains; dedicated MLS-MPM water; no fused sim monoliths.
3. Cloth first within XPBD; water follows WATER_IMPLEMENTATION_PLAN independently.
4. Fixed-substep integration; deterministic export; beat-rampable time-scale.
5. Colliders v1 analytic; scene-SDF v2.
6. Sim outputs are ordinary wires into the existing render stack; no private
   renderers.
7. Beat-native params via port-shadowing and existing triggers; no new modulation
   silo.

## 8. Deferred (with triggers)

- **Lane 3 — volume rendering** (raymarched pyro + baked VDB playback): when smoke
  looks matter beyond what FluidSim3D + compositing deliver; VDB playback rides the
  lane-1 streaming infra.
- **Scene-mesh SDF colliders**: when analytic shapes visibly fail (cloth on a
  complex set piece).
- **Tearing / plastic deformation**: constraint-breaking thresholds — after cloth
  ships and gets performed with.
- **Two-way coupling** (fluid pushes cloth): research-tier; revisit on demand.
- **GPU rigid bodies**: ~~only if a look demands stacking/contact; XPBD shape-matching
  is the cheap version if so.~~ **SUPERSEDED 2026-07-07** — rigid bodies are now
  `docs/BOX3D_PHYSICS_DESIGN.md` (box3d FFI, Peter's call). Two-way coupling with
  this doc's XPBD lane remains deferred here; collider-atom vocabulary reconciles at
  this lane's section 2.5 audit.
