
# Graph authoring & runtime rules

## Decomposition bars
- A single GPU dispatch is NOT the decomposition bar — a one-dispatch shader encoding a whole named effect is still the monolith anti-pattern.
- Decomposition has a CEILING — canonical-answer plumbing belongs inside a higher-level node, not exposed as a hand-wireable atom.
- When a JSON graph balloons into dozens of identical math+value scaffolding nodes, create a primitive that internalizes the pattern. Distinct from bundling: scaffolding = repeated single-op plumbing (collapse it); bundling = distinct operations fused into one kernel (never).
- When a generator's compute pass glues multiple distinct topology shapes together, prefer granular decomposition into reusable atoms over one curated wrap primitive.
- Curated higher-level primitives beat raw-WGSL escape hatches when the math generalizes. wgsl_compute-as-curated-backing is legitimate for OPEN families (attractors, fluid integrators — 30+ variants, users invent new ones) where a new variant should be a JSON edit; closed families (Plasma's 8, the Platonic solids) ship as registered primitives with compiled enums.
- wgsl_compute authoring is a current user surface for Peter — never dismiss its UX/ergonomics as "core-dev only".
- Never suggest reverting an effect to pure Rust to dodge graph complexity — users must be able to see and modify graph-defined effects.
- Static option tables feeding mux inputs belong as inline `in_N` params on the mux, not a constellation of node.value constants.
- Orientation/scale problems with imported meshes are fixed with a transform NODE in the graph, never by modifying the mesh file.

## Editor framing
- The graph editor is an authoring surface, never used during live performance — frame complexity decisions around authorability, not stage-time editability.
- The graph editor (canvas + right panel + interactions) is ONE surface; behavior must not fork on Effect vs Generator target — when it does, that's the bug.

## Binding / param surface
- Card bindings overwrite node params at build time — poking an imported def's node params directly is silently reverted.
- `EffectInstance.param_values` + `user_param_bindings` are the live performance surface, not legacy — never propose removing them as cleanup.
- Effect and generator runtime binding/param paths stay in lockstep: fix one, check the other.

## Runtime invariants
- Wire-type identity is the named Channels signature, not ItemKind or raw size+align — consult `docs/CHANNEL_TYPE_SYSTEM.md` before extending the type system.
- Compile-time plan reachability and the runtime live-step pruner must seed from the same liveness-root set, or aliased/state-capture primitives get silently filtered from the plan.
- State-capture (cycle-break) is a per-port property on stateful primitives, not per-node — the planner checks `state_capture_input_ports().contains(&port)`, not just `breaks_dependency_cycle()`.
- When wrapping a CompositeHandle as a PostProcessEffect, walk the ExecutionPlan and pre-bind every Texture2D resource — lazy-alloc panics in without_device mode.
