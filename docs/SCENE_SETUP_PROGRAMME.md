# Scene setup programme — gizmos on the output, one object model, physics on imports, swarms

<!-- index: Programme brief for 3D scene setup in the main window: interactive gizmos on the output preview, one Add menu and shape model for every scene object, physics on imported meshes, readable forces, force-only swarms, starter scenes. Routing, waves, landing and cleanup rules for the Opus lead. -->

**Status:** PROPOSED · 2026-10-20 · Fable with Peter. Opus leads execution unattended; every wave lands through this doc's rules.
**Document type:** Programme brief. Each workstream gets its phase brief from this doc; the designs it amends are named per workstream. No separate design doc unless a workstream's audit finds a decision this doc does not make.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) section 5 (Phase briefs) and section 6 (Seam briefs) per phase; [AGENT_ROUTING.md](AGENT_ROUTING.md) for seats, briefs, gates and the unattended-run rules.

Peter's direction, 2026-10-20: configure complex 3D scenes with photoscans in the main window, not the graph editor. Choose where water flows in, the domain size (not always square), see the resolution grid, place forces and see where they act, choose which objects have physics, make N physics copies of an object, run thousands of force-only instances as swarms. Standard Blender, Unity and Unreal transform gizmos on the output preview. Open a clean scene or a picker preset and go. Simple end-game architecture: no parallel systems, no fallbacks to replaced paths, no dead code, no over-testing.

## 1. Audit — what exists (verified 2026-10-20)

| Capability | State | Anchor |
|---|---|---|
| Interactive gizmos (move, rotate, scale; objects and fluid domains) | Built, hosted by the graph editor's 3D viewport only | `crates/manifold-nodes-scene/src/node_graph/viewport_gizmo.rs:56` (modes), `:181` (targets), `:257` (lines), `:364` (pick), `:413` (drag write); host `crates/manifold-app/src/window_input.rs:846` |
| Main output preview overlay | Display only: ground grid, light markers, domain boxes, matched to the exact frame on screen | `crates/manifold-app/src/physics_overlay.rs:1-6`, `crates/manifold-app/src/frame/present.rs:183-199` |
| Editor-side domain state | The editor viewport keeps its own `viewport_fluid_domains` applied over the scene | `crates/manifold-app/src/fluid_domain_edit.rs` (`apply_runtime_domains`), `window_input.rs:857-860` |
| Scene panel adds | Object, light, layer plane, fog, environment, fluid (GPU FLIP), force, modifier; duplicate is one object at a time | `SceneSetup*` actions in `crates/manifold-ui/src/panels/actions.rs`; panel `scene_setup_panel.rs` (2287 lines) |
| Force cards | Radial, Uniform, Vortex manifests. Strength is m/s², impulse size is m/s, both already in the manifest `unit` field; the UI shows neither. Trigger labelled "Kick" | `crates/manifold-nodes/assets/scene-modifier-presets/RadialForce.json:58-85` and the Uniform and Vortex siblings |
| Engine force fields | Uniform, Radial, Vortex, Sampled, Scaled, Sum | `crates/manifold-physics/src/interaction.rs:28,54,114,238,372,399` |
| Water solids from meshes | Signed distance lattice; open meshes rejected | `crates/manifold-physics/src/sdf.rs:66-71`, `crates/manifold-water-gpu-flip/src/primitives/liquid_solid_distance.rs:44` |
| Inflow | Solver supports it (`inflow_jitter`, `emit_flags` in `gpu_flip_step.rs`); no inflow object in the panel, placed by coordinates | `crates/manifold-water-gpu-flip/src/primitives/gpu_flip_step.rs:465,2276` |
| Lattice grid | Never drawn | — |
| 3D particles | 72 nodes: scatter, seed, noise, diffuse, swirl, container-repel forces, Euler step, `particles_to_copies` into instanced `render_scene` | `docs/NODE_CATALOG.md` Particles 3D; `crates/manifold-nodes-scene/src/node_graph/primitives/particles_to_copies.rs`; [REALTIME_3D_DESIGN.md](REALTIME_3D_DESIGN.md) section 10 (per-object instancing addendum) |
| Physics boundary | P1 landed; P2 (general template insertion), P3 (shared object cards), P6a (rigid add, enable, split, duplicate) open | [PHYSICS_ENGINE_BOUNDARY_DESIGN.md](PHYSICS_ENGINE_BOUNDARY_DESIGN.md) section 7 (Phasing) |

Unverified, first audit item: whether the fluid domain extent is per-axis or cube-only (`gpu_flip_domain.rs`). If cube-only, per-axis extent is in B1.

Open beads this programme absorbs: BUG-gm927 (water setup you can see and trust), BUG-uuso (force gizmo and one Add menu), BUG-4lfm (GPU-surface water object not recognised as water), BUG-9rai8 (force cards draw choices as sliders), BUG-o0us8 (viewport dolly jumps), BUG-mo4oa (scene force trigger skipped), BUG-403q (scene-liquid-forces flow). Close each with the landing that fixes it.

## 2. Decisions

- **D1 One gizmo core, two hosts.** `viewport_gizmo.rs` is the only pick, drag and line implementation. The main output preview becomes its primary host; the graph editor viewport stays a host for free navigation. No second gizmo implementation, no copied helpers.
- **D2 One source of chrome.** The display-only overlay's own line drawing (grid, lights, domains) is replaced by the gizmo core's line generator. The editor-side `viewport_fluid_domains` state is deleted; every host reads scene objects from the content thread's reported overlay frame.
- **D3 Shape is a declaration, not code.** Every scene object declares its extent in its manifest (`box`, `sphere`, `plane`, `point_radius`, `axis`), mapping handle names to its params. The gizmo, the out-of-domain check and the panel card all derive from that one declaration. No per-kind Rust for handles.
- **D4 One Add menu.** Object, light, fluid, inflow, drain, collider, force, swarm, all through the boundary design's general template insertion (P2). The bespoke Add Fluid path is deleted when P2 lands; no fallback.
- **D5 Gizmos edit params.** Every drag is an `EditingService` param edit: undoable, bindable, the same value as the knob. Wired params show locked.
- **D6 Show safety.** Preview gizmos live behind the existing Show gizmos toggle; session mode forces it off. The output render is byte-identical with gizmos on (REALTIME_3D D9 gate, re-asserted on the main preview).
- **D7 Forces share one vocabulary.** Water, rigid bodies and swarms sample the same scene force list through the engine's field types. No particle-only force vocabulary for swarms.
- **D8 Open meshes are solids.** Closed mesh: signed distance as today. Open mesh: unsigned distance with a user thickness (the shell approach Houdini and Blender use). The rejection in `sdf.rs` goes away.
- **D9 Labels, not IDs.** "Kick" becomes "Impulse", "Kick Size" becomes "Impulse Size". Param IDs `fire` and `impulse_strength` stay: `fire` is the engine-wide trigger id saved bindings persist.

Rejected: a new "setup mode" app state (the preview is the surface); a swarm solver of its own (atoms exist, the object and the force seam are missing); per-force Rust handle code; keeping card drag for forces beside the gizmo.

## 3. Workstreams

Each lists the user-visible outcome, the phases, the seat and the files. "Done" per phase is the outcome rendered to a headless PNG the lane reads, plus the scoped tests the brief names.

### A. Gizmos on the main output preview

Outcome: grab any scene object, domain, inflow, drain, collider or force on the output preview and move, rotate or scale it with standard handles. Optional preview-only navigation that never touches the output.

- **A1 Host the core on the preview.** Second input site in `window_input.rs` calling the same pick and drag over the preview rect with the overlay frame's camera. Overlay frame carries the selected object's transform targets beside lights and domains. Delete the overlay's own line drawing (D2). Seat: Opus lane, high. Files: `window_input.rs`, `physics_overlay.rs`, `ui_frame.rs` (`MainGizmos`), `frame/present.rs`, `viewport_gizmo.rs` read-only until B1 lands.
- **A2 Preview-only navigation.** Orbit, pan, dolly on the preview through the existing viewport camera session, output untouched (D9 gate). Fixes the viewport dolly bead in passing. Seat: Opus lane, high.
- **A3 Lattice grid.** Draw the domain's lattice at its real resolution, same line generator. Seat: Sonnet, medium, after B1.

### B. One object model, one Add menu

Outcome: every scene thing is added from one menu, carries one card, one gizmo and one out-of-domain check, from one manifest declaration.

- **B1 Shape declaration.** Manifest `shape` block per scene object kind (D3); `GizmoTargetKind` derives targets from it; `Force`, `Inflow`, `Drain`, `Collider` join `Object` and `FluidDomain`. Per-axis domain extent if the audit finds cube-only. Delete force card drag. Seat: Opus lane, high. Files: scene-modifier manifests, `viewport_gizmo.rs`, `scene_vm.rs`, and PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One scene-object model) amended in place.
- **B2 Add menu and cards.** Boundary P2 and P3 executed: general template insertion, shared cards, the Add Fluid special case deleted. Inflow and drain become placed objects. Seat: Sonnet, medium, under the P2 and P3 briefs in the boundary design.
- **B3 Setup check.** The water-setup epic's part 1: one check reading every shape against the domain each edit, surfaced on the card and the gizmo. Seat: Sonnet, medium.

### C. Physics on imports

Outcome: a photoscan is a water solid or a rigid collider with one switch; N copies of an object with physics on.

- **C1 Open-mesh solids.** D8 in `sdf.rs` and `liquid_solid_distance.rs`; thickness param on the collider card. Seat: Opus lane, high. GPU proof once at the end.
- **C2 Physics switch and copies.** Boundary P6a (rigid add, enable, duplicate) plus a copy count on duplicate. Seat: Opus lane, high for P6a; Sonnet, medium for the count.

### D. Forces you can read

Outcome: a force row says what it does in units; the industry set is complete.

- **D1 Labels and units.** D9 renames; unit shown on the row from the manifest `unit` field; gravity reference line on the force card. Seat: Sonnet, medium. Files: the three force manifests, the param row renderer, the node catalog regenerated.
- **D2 Industry set.** Drag, wind with turbulence, attractor, explosion, falloff curve choice, impulse decay. Manifests over existing engine fields where they fit; drag needs a velocity-dependent field kind in `interaction.rs`. Seat: Opus lane, high for the engine field; Sonnet, medium for manifests. Fixes the force-card slider bead.

### E. Swarms

Outcome: a Swarm object from the Add menu runs thousands of instances on the GPU, pushed by the same forces as water and bodies, bindable from the perform surface.

- **E1 Field-driven swarm.** Scene object over existing particle atoms and `particles_to_copies`; one atom samples the scene force list on the particle buffer through the GPU force evaluation water already uses (audit where it lives; share it, never copy it). Seat: Opus lane, high. GPU proof once.
- **E2 Flocking.** Separation, alignment, cohesion with a GPU spatial hash reusing the FLIP grid sort. Parked unless Peter approves it under the calls below.

### F. Starter scenes

Outcome: picker presets that open working: water in a photoscan, swarm, rigid debris.

- **F1** Three presets, authored with `graph-tool validate`. Seat: Sonnet, medium. Last wave.

## 4. Waves and landing

| Wave | Lanes in parallel | Landing |
|---|---|---|
| 0 | Lead audit through read-only agents: domain extent, GPU force evaluation location, `viewport_fluid_domains` uses, demo files `viewport_p5c_demo.rs` and `viewport_p6_demo.rs` (keep as headless fixtures or delete). Amend REALTIME_3D D7a and the boundary design's scene-object model in place. | docs direct to main |
| 1 | A1 ‖ B1 (A1 hosts existing kinds; B1 extends kinds; disjoint files) | one batched landing, `--skip-gpu` |
| 2 | B2 ‖ D1 ‖ C1 | one batched landing; GPU proof for C1 only |
| 3 | A2 ‖ B3 ‖ C2 ‖ D2 | one batched landing |
| 4 | E1 ‖ A3 | one batched landing; GPU proof for E1 |
| 5 | F1 ‖ E2 if approved | one batched landing |

Rules: lanes commit once and stop; the lead reviews with Opus agents and lands the wave as one batch with `scripts/land_branch.py`. One `--cheap` gate per lane, reused at landing. Non-GPU waves pass `--skip-gpu`. Reds proven on main get a bead and a named red, never an in-line fix. The lead never writes code itself: reads, builds, tests, probes and edits go to agents; the lead briefs, reviews, decides and lands.

## 5. Testing

- Scoped nextest on touched modules only; no family or full-suite runs in lanes.
- One UI flow per new interaction: preview gizmo drag, Add menu insertion, physics switch. No flows for chrome appearance.
- Visual changes verified by headless PNG the lane reads (`ui-headless-png-verification`); no new goldens for chrome.
- GPU proofs only on C1 and E1, one run each through `scripts/gpu_queue.py`.
- No new test helpers where `testkit` has one; the brief names the helper.

## 6. Cleanup owed by each landing

Every lane deletes what it replaces in the same commit. Named now so no wave ends with two paths:

- A1: the overlay's bespoke grid, light and domain line drawing.
- B1: force card drag in `scene_setup_panel/forces.rs`; `viewport_fluid_domains` and `apply_runtime_domains` once the overlay frame is the one source.
- B2: the Add Fluid special-case insertion and its family literals.
- C1: the open-mesh rejection and any caller that worked around it.
- D1: every "Kick" string in manifests and UI copy.
- Wave 0: demo files that are not fixtures.

Forbidden in every lane: a second gizmo core; an invalidation flag (frame planner inputs only, [UI_INVALIDATION_DESIGN.md](UI_INVALIDATION_DESIGN.md)); new `Arc<Mutex>` or `Arc<RwLock>`; bespoke rows or sliders ([WIDGET_TREE_DESIGN.md](WIDGET_TREE_DESIGN.md) section 5b (Agent contract and enforcement)); direct model writes from the UI; a kept "legacy" branch of any replaced path; `#[allow(dead_code)]`.

## 7. Peter's calls and the unattended defaults

The run is unattended. Each call carries the default the lead applies when Peter has not answered; the default is recorded in `.claude/orchestration/decisions.md` before the wave that needs it.

1. A2 preview navigation in wave 1 or wave 3. Default: wave 3.
2. Instance ceiling for swarms on stage. Default: 100k, so E1 keeps particle arrays GPU-resident from the start.
3. E2 flocking in this programme or parked. Default: parked; E1 ships field-driven.
4. Handles when an effect follows the scene layer (warp, mirror, feedback): fade or hide. Default: fade.

## 8. Report

The run ends with `docs/evidence/scene_setup_<date>/REPORT.md` in the style of the overnight reports: before and after renders for every visible change, every bead closed, every default applied under the calls above, everything parked and why. Everything lands on main, so no launch command is owed; anything left on a branch names its ref and the exact binary path to run.

## 9. Decided — do not reopen

1. Gizmos are param edits through `EditingService` (REALTIME_3D D8).
2. The output render is byte-identical with gizmos on (REALTIME_3D D9).
3. `fire` and `impulse_strength` IDs stay; labels change.
4. No setup mode; the preview is the surface.
5. Shape comes from the manifest, never per-kind Rust.
