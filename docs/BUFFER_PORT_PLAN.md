# Buffer / Array Port Plan

**Status:**
- **Phase A** — shipped 2026-05-19. Five particle primitives + topology integration test.
- **Phase B** — shipped 2026-05-20. Five mesh primitives (GenerateGridMesh, GenerateInstanceTransforms, Rotate4D, Render3DMesh, RenderInstanced3DMesh). First render-pass primitives in node_graph.
- **Phase C** — partial 2026-05-20. GenerateParametricCurve + RenderLines shipped. **AudioInput deferred** — needs an audio sample channel through `EffectNodeContext` that doesn't exist yet; building it now would be a synth stub, not a real input.
- **Phase D** — parked. The four 3D-volume primitives need `MetalBackend` Texture3D resource backing (today only Texture2D allocates real GPU resources; Texture3D falls back to mock semantics). Implementing primitives now would compile but be runtime no-ops.

Full pixel-exact FluidSim parity needs additional blur + gradient atoms and the 7-pattern seed shader port — both follow-up sessions on top of this foundation.

**Why this exists:** Today's particle / mesh / line generators (FluidSim, BlackHole, MetallicGlass, Tesseract, Lissajous, etc.) are opaque atomic primitives because their internal state lives in `MTLBuffer`s that have no externally-visible wire type. To decompose them into a creative surface — the §0 "primitive library is the product" promise — graph wires need to carry not just textures and scalars but also **arrays of structured items** (particles, vertices, line points, audio samples).

Companion docs: [PRIMITIVE_LIBRARY_DESIGN.md §12.3](PRIMITIVE_LIBRARY_DESIGN.md) (Array port promoted to V1) and §12.8 (Black Hole / FluidSim worked-example decompositions).

---

## What's being added

A fourth port-type family, `Array<T>`, parallel to today's `Texture2D | Texture3D | Scalar(...)`. The wire carries a flat list of structured items — particles, vertices, line points, audio samples — accessed by index, not by spatial coordinate. Backed by `MTLBuffer` storage at the GPU level.

Four families of primitives ride on the new port type, decomposing roughly 15 of MANIFOLD's 20 shipping generators into rewireable graphs:

**Particles** — the most familiar pattern. Today FluidSim, BlackHole, ParticleText, StarField, and ComputeStrangeAttractor each hide an internal particle system. After decomposition, a particle is *a thing in space with momentum, colour, life* flowing on a wire. Producers spawn particles (uniform grid, ring, text-glyph mask, video-frame brightness). Movers integrate them through force fields. Splatters stamp them into image accumulators. ArrayFeedback closes the per-frame loop. The creative unlock: FluidSim's particles stop being FluidSim's — they can be advected by audio energy instead of fluid velocity, splatted onto a different background, or routed into something that's neither FluidSim nor any other shipping effect.

**Meshes** — 3D geometry as triangles and vertices. Drives MetallicGlass's million-vertex grid, the four-dimensional wireframe family (Tesseract, Duocylinder, WireframeZoo), and the instanced generators (NestedCubes, DigitalPlants). Producers generate the mesh (grid, instance set, 4D shape vertices). Transformers move it (3D and 4D rotation). Renderers draw it with camera and lighting. The creative unlock: swap any stage. Run MetallicGlass's grid through Tesseract's 4D rotation, or vice versa.

**Lines** — sequences of 2D points drawn as bright vector strokes. Lissajous (parametric curves) and OscilloscopeXY (audio waveforms). Producers compute the points; the renderer thickens and draws them. Unlocks routing audio into anything line-shaped, or driving non-line generators with line data.

**3D volumes** — 3D grids of density. FluidSim3D and MriVolume. Deferred (see Phase D below) — only two generators use it, the user value of decomposing them is lower, and the Texture3D primitive infrastructure is more greenfield.

---

## Design decisions, resolved

### Active-count slider, not fixed-at-build

Earlier draft of this plan had "particle count is fixed at chain build time" as a tradeoff. That was the wrong frame. The right model:

- A producing primitive declares a **maximum capacity** at chain build time (e.g. 8M particles for 2D systems, 1M for 3D, 2M for mesh vertices — picked per family, scaled to memory budget).
- The user-facing slider controls **active count** dynamically via a uniform. Each dispatch runs over `[0, active_count)`; the rest are skipped.
- No allocation churn when the slider moves. Performance scales with active count, memory cost is the max.
- The only thing that triggers a chain rebuild is exceeding the pre-allocated max. Pick reasonable maxes and that essentially never happens.

This is how FluidSim works internally today. The mistake was thinking of the buffer size as the user-facing knob; the active-count uniform is.

### Array layout: hybrid — generic storage, typed at the macro

`PortType::Array { item_size, item_align }` at the storage level — wire validation is just a size/align match. The `primitive!` macro provides syntactic sugar:

```rust
primitive! {
    name: SeedParticles,
    inputs: {},
    outputs: {
        particles: Array<Particle> capacity 1_048_576,
    },
    ...
}
```

`Array<Particle>` expands to `Array { item_size: 64, item_align: 16 }` plus a `capacity` annotation. The struct layout itself lives in `compute_common.rs` (same `#[repr(C)]` types that exist today — `Particle` is already canonical at 64 bytes). The graph editor can still surface "Particle Array (64 bytes/item)" in tooltips if useful.

Why hybrid: typed gives readability at primitive definition time; generic gives flexibility (new item layouts don't require enum-recompile gymnastics) and matches the "shaders own the byte interpretation" instinct of the rest of the codebase.

### Wires are transient by default; persistence via ArrayFeedback

Same model as texture wires. `Array<T>` flowing from primitive A to primitive B exists for one frame and is reusable by the buffer pool next frame. Persistence (last frame's particles → this frame's input) is explicit:

```
SeedParticles ─┐
               ↓
         ArrayFeedback ── prev_particles → IntegrateParticles ─→ next_particles → ScatterParticles
                              │                                       │
                              └───────────────────────────────────────┘
                              (loop closes via ArrayFeedback state)
```

`ArrayFeedback` holds the persistent buffer in `StateStore`, keyed by `(owner_key, node_id)` — same pattern as today's texture `Feedback`.

### CPU producers go through host-visible MTLBuffers

`AudioInput` is the canonical example: host writes audio samples to a `create_buffer_shared` buffer, downstream GPU primitive reads it. Same infra as today, just exposed on a wire.

---

## Phasing

**Phase A — foundational (shipped)**

The Array port + the particle family. Worked out in detail at [PRIMITIVE_LIBRARY_DESIGN.md §12.8](PRIMITIVE_LIBRARY_DESIGN.md). Delivered:

1. ✅ `PortType::Array(ArrayType)` variant + `primitive!` macro support for `Array(T)` syntax (paren syntax — `<>` doesn't parse cleanly in macros).
2. ✅ Wire format: validated through the existing `EffectGraphDef` schema; wires reference ports by name, port-type matching uses `PortType` derived Eq on the new variant.
3. ✅ Runtime: `MetalBackend::pre_bind_array` + `array_buffer` accessor + `GpuEncoder::copy_buffer_to_buffer`. No buffer pool yet — each chain build allocates fresh; pool comes when allocation cost shows up in profiles.
4. ✅ `ExecutionPlan` lifetime planning works unchanged — `ResourceId` is port-type-agnostic.
5. ✅ Primitive context accessors: `ctx.inputs.array("port")` and `ctx.outputs.array("port")` return `Option<&GpuBuffer>`. Active-count plumbing is per-primitive uniform data, not a runtime concept.
6. ✅ Five particle primitives shipped:
   - `node.array_feedback` — one-frame delay holding Array(Particle) in StateStore
   - `node.seed_particles` — uniform-random spawn into Array(Particle), `active_count` slider + `max_capacity` ceiling
   - `node.integrate_particles` — bilinear-sample velocity field, Euler step, toroidal wrap
   - `node.scatter_particles` — clear + atomic-add splat into Array(u32) accumulator
   - `node.resolve_accumulator` — Array(u32) → Texture2D, divide by fixed-point scale
7. ✅ Integration test validates the topology builds + connects: `SeedParticles → ArrayFeedback → IntegrateParticles → ScatterParticles → ResolveAccumulator` wires cleanly through the new port-type system, validation rejects type mismatches between Array(Particle) and Array(u32), and Integrate's required `velocity` Texture2D input is enforced. Full pixel-exact FluidSim parity needs additional blur + gradient atoms — separate follow-up.

**Phase B — mesh family (shipped 2026-05-20)**

Five primitives shipped:
- `node.generate_grid_mesh` — NxM grid of `MeshVertex` in XZ plane
- `node.generate_instance_transforms` — N InstanceTransforms in Grid / Ring / Spiral / Random layouts
- `node.rotate_4d` — 3-plane (XY, ZW, XW) rotation on `Array<Vec4Vertex>`
- `node.render_3d_mesh` — depth-tested triangle-list renderer (first render-pass primitive in node_graph)
- `node.render_instanced_3d_mesh` — instanced triangle-list renderer with per-instance Euler rotation

Plus the canonical `Array<T>` item layouts in `generators::mesh_common`: `MeshVertex` (32 bytes), `Vec4Vertex` (16), `InstanceTransform` (32), `LinePoint` (8). Unlocks MetallicGlass, the 4D wireframe trio (Tesseract / Duocylinder / WireframeZoo), and the instanced pair (NestedCubes / DigitalPlants) once the legacy generators get rewired through these primitives.

Triangle-list topology means the producer is responsible for emitting triangle-order vertices. `GenerateGridMesh` currently emits positions-only — a future `Triangulate` adapter primitive converts grid topology to triangle list.

**Phase C — line family (partial, shipped 2026-05-20)**

Two of three shipped:
- `node.generate_parametric_curve` — Lissajous / Hypocycloid / Rose / Circle into `Array<LinePoint>`
- `node.render_lines` — capsule-line renderer with 4x MSAA + additive blending, `closed_loop` toggle

`AudioInput` is deferred. The plan calls for "host writes audio samples to a `create_buffer_shared` buffer", but `EffectNodeContext` has no audio sample channel today and adding one is a runtime/executor change, not a primitive change. Land the audio path through executor first; then `AudioInput` is a one-day primitive.

**Phase D — 3D volume family (parked)**

`Sample3D`, `SliceVolume`, `Volume3DSplat`, `Volume3DAdvect`. Per [PRIMITIVE_LIBRARY_DESIGN.md §12.4](PRIMITIVE_LIBRARY_DESIGN.md), these were V2-deferred. The case for promoting is still weaker — only FluidSim3D and MriVolume need them, both can stay atomic-with-internal-state, and the prerequisite infrastructure isn't there: `MetalBackend` only allocates real Texture2D resources today; Texture3D falls back to mock semantics. Build the Texture3D backend first, then primitives.

---

## Open questions still to land

- **Particle struct evolution.** Today `Particle` is `#[repr(C)]` with `pos: vec3 + life + age + _pad1 + color`. If a new primitive wants particles with a `velocity` field, do we extend the canonical struct (breaks layout for existing splatters) or define `Particle2` alongside? Lean: define small variant structs per use case, the generic Array layout doesn't care.
- **Max capacity values.** Pick per family. Starting points: 8M for 2D particles (256MB at 32 bytes/item — too much; revisit), 1M for 3D particles, 2M for mesh vertices, 1M for line points, 65K for audio samples. The 2D particle number specifically needs research — FluidSim today uses how many? `MAX_PARTICLES` lookup.
- **Mesh rendering pipeline.** `Render3DMesh` is a vertex+fragment pass, not compute. The graph runtime today dispatches compute primarily. Adding vertex+fragment primitives needs render-pass plumbing. May be straightforward (the existing `line_pipeline.rs` and `mesh_pipeline.rs` already handle this for legacy generators) — verify when Phase B starts.
- **4D rotation as a primitive vs scalar params on Render3DMesh.** Tesseract's 4D rotation is six rotation angles (xy, xz, xw, yz, yw, zw). Is `Rotate4D` its own primitive that transforms `Array<Vec4Vertex>`, or are those scalar params on a 4D-aware renderer? Lean: separate primitive, lets the user swap rotation behaviour.

---

## What this doesn't do

- Doesn't migrate generators to JSON. That's a separate pass (the JSON migration scoped earlier in the same session). Once Phase A lands, the JSON migration can use Buffer ports for the buffer-using generators rather than wrapping them as atomic-with-internal-state.
- Doesn't add subgraph iteration (§12.4 of PRIMITIVE_LIBRARY_DESIGN). That stays V2+.
- Doesn't fuse particle dispatches (§12.5 fusion compiler). Still parked behind measured perf pressure.

---

## References

- [PRIMITIVE_LIBRARY_DESIGN.md §12.3](PRIMITIVE_LIBRARY_DESIGN.md) — Array port promoted to V1
- [PRIMITIVE_LIBRARY_DESIGN.md §12.8](PRIMITIVE_LIBRARY_DESIGN.md) — Black Hole + FluidSim worked-example decompositions
- [NODE_GRAPH_SYSTEM.md](NODE_GRAPH_SYSTEM.md) — overall graph architecture
- [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) — primitive macro authoring guide
- `crates/manifold-renderer/src/node_graph/ports.rs` — current PortType (Texture2D / Texture3D / Scalar)
- `crates/manifold-renderer/src/generators/compute_common.rs` — canonical Particle layout (64 bytes)
- `crates/manifold-renderer/src/generators/fluid_sim_core.rs` — internal particle pipeline that Phase A decomposes
