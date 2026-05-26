# Graph Compiler — Shader Fusion + Per-Pixel Loops

**Status:** Proposed initiative, not yet started. Standing decision (2026-05-26): Plasma decomposition is deferred until this work lands; Plasma becomes the test bed.

**Owner:** TBD. Estimated 2-4 weeks of focused work.

---

## 1. The idea in one paragraph

Today every primitive in the node graph compiles down to **one GPU dispatch over the whole texture** — its WGSL shader runs once per pixel, reads its input textures, writes its output texture, done. A chain of three pointwise atoms (`sin_term → compose Add → trig_texture`) is three full-canvas dispatches with two intermediate textures read and written. A per-pixel loop (run the same sub-graph 5 times to compute one pixel) is not expressible at all — you'd either unroll into 5× the nodes (one full dispatch per iteration, with ping-pong textures carrying intermediate state) or pack the loop into a single bespoke atom with the loop body hardcoded in WGSL.

The graph compiler changes the unit of work. Instead of "one primitive = one dispatch," primitives become **inlineable shader fragments**, and a compiler pass walks adjacent sub-graphs at build time and emits **one fused shader** that does the whole chain's math as local variables in registers, with no intermediate texture roundtrips. The same machinery, with a `for` loop wrapped around the inlined body, lets a `for_each_n` node hold a sub-graph as its loop body — true per-pixel iteration expressed as a graph, not as WGSL paste-in.

## 2. Why this matters

Two unlocks, each independently worth the investment:

### 2.1 Performance — multi-pass fusion

Every multi-atom effect chain today eats real bandwidth. A 75-node Plasma graph is 75 full-canvas dispatches at 60fps × 4K = a lot of redundant texture reads/writes for math that could live in registers. On Metal, dispatch launch overhead is non-trivial too. A fusion pass would compile that 75-node graph down to ~8 dispatches (one per "branch" of the fan-out where two unrelated sub-graphs meet at a mux/compose), with all the per-pixel arithmetic chained as local variables inside each fused shader.

The performance argument alone probably justifies the build. Atomized graphs become free — there's no longer a performance tax for decomposing a curated effect into 12 atoms instead of one bespoke shader. The "fuse for parity" anti-pattern (`docs/DECOMPOSING_GENERATORS.md` §1.1) disappears as a tradeoff, because composing and fusing become the same thing at compile time.

### 2.2 Expressiveness — per-pixel loops as graphs

Today there is no way to express "run this sub-graph N times to compute one pixel" without unrolling or burning a bespoke atom. That blocks a whole class of effects: escape-time fractals (Mandelbrot, Julia), raymarching (volumetric SDFs, terrain, fluid), iterated function systems, per-pixel ODE integration, any "iterate until converged" pattern. Plasma's Noise and Fractal variants are the immediate forcing case — they need ~50 atoms each to unroll, or they need bespoke loop atoms (`iterated_sin_fbm_2d`, `iterated_sin_warp_2d`) that are single-use and defeat the composability framing.

With a `for_each_n` wrapper node whose `body:` slot points at a sub-graph, the Noise loop body becomes 4-5 visible, editable, recomposable atoms wired into a wrapper. The math is drillable; future generators can build on the same vocabulary; the no-fused-monolith rule applies all the way down without exception.

### 2.3 Positioning vs. TouchDesigner

TouchDesigner's TOP model is one operator = one framebuffer pass. To fuse, the user writes a GLSL TOP and types the combined shader themselves. To loop per-pixel, same answer — write the loop in a GLSL TOP. The fusion+loop compiler is the architectural move that lets MANIFOLD's graph editor stay first-class as both **a visible composition surface** and **a performant runtime** without forcing users into the WGSL escape hatch for either reason. That's a real differentiator — TD users routinely fall back to GLSL TOPs for either performance or expressiveness; MANIFOLD users wouldn't have to.

## 3. Prior art

The pattern is mature, not research:

- **Unity Shader Graph** and **Unreal Material Editor** — node graphs that compile to one fragment shader at build time. Direct shape match. Read these as the user-facing reference.
- **Blender shader nodes** — same pattern in the open-source world. Source-readable.
- **Halide** (MIT, ~2012) — the canonical academic reference. Image-processing DSL that separates *what the math is* from *how it's scheduled* (fused, tiled, vectorized, parallelized). The formal vocabulary for this kind of compiler. The fusion pass MANIFOLD would build is a small Halide-shaped scheduler over WGSL atoms.
- **XLA / `torch.compile` / Triton** — ML world's version. Operator fusion for tensor kernels. Same machinery, different domain.
- **NVIDIA NSight** uses the term "kernel fusion" for this; you'll see it in GPU-profiling literature.

What's *not* in this list: TouchDesigner. TD does some CHOP-level optimization (scalar-stream math fusion), but to the best of current knowledge does not do general multi-pass TOP fusion. The GLSL TOP escape hatch is the workaround. (Worth verifying if/when this work starts — TD internals aren't fully public.)

## 4. Two unlocks, one engine

The fusion pass and the loop pass share most of their implementation. Build them together; the loop variant is a small wrapper on top of the inliner.

### 4.1 The shared engine: WGSL inliner

The core capability: given a sub-graph of atoms whose WGSL bodies are pointwise pure functions of their inputs, emit one shader whose body is the inlined chain. Requirements:

- **Every atom declares its WGSL body as a callable function**, not just a complete compute shader. The `primitive!` macro grows a `wgsl_body:` slot containing the per-pixel math as a function. The existing full-shader pattern (`@compute @workgroup_size(...) fn cs_main(...)`) stays for atoms that genuinely can't inline (texture writes, sampler dependencies, workgroup-shared memory, multi-tap reads).
- **Variable naming and binding rewriting.** Inlined bodies need locally-unique names; bindings (textures, samplers, uniforms) need to be hoisted to the outer shader's binding layout. A simple SSA-style renamer is enough.
- **Boundary detection.** The inliner walks the sub-graph from a "fusion root" (a node consuming the fused output) backward until it hits a barrier — texture write that's read elsewhere, multi-input/multi-output node that can't inline, format conversion, downsample, anything that breaks pointwise purity. Everything inside that boundary fuses; the barrier becomes a real dispatch boundary.
- **Per-atom inline gates.** Each atom declares whether it's inlineable (`inline: pointwise` vs `inline: never`). Conservative default: pointwise atoms inline, anything stateful or multi-tap doesn't. We expand the set over time as we work cases out.

### 4.2 The fusion pass

A graph-compile-time pass that walks the post-validation plan, identifies maximal pointwise sub-graphs, and replaces each with one fused compute pipeline. Invisible to the user; the graph editor still shows N nodes. Pure optimization.

### 4.3 The loop pass

A new primitive — `node.for_each_n` — that takes `n: scalar`, `body: graph_ref` (a sub-graph), and `state_init: <typed>`. At graph-compile time, the inliner pulls the body sub-graph and wraps it in a `for (var i = 0u; i < n; i++) { ... }` inside one shader. State threads through as a local variable in the loop body. Output is the final state after `n` iterations.

This is the construct that makes per-pixel loops graph-expressible. Plasma's Noise becomes `centered_uv → for_each_n(body=octave_subgraph, n=octave_count) → texture_sum_or_normalize`. Mandelbrot becomes `centered_uv → for_each_n(body=mandelbrot_step, n=64, state_init=vec2(0,0)) → escape_time_color`.

## 5. Plasma as test bed

Plasma is the natural first case because:

- **6 of 8 variants are already known to atomize cleanly into ~75 nodes** (see the deferred audit in `PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md`). That graph is the perfect fusion-pass benchmark — measure pre-fusion vs. post-fusion frame time, validate the 75 dispatches collapse to ~8.
- **Noise and Fractal are the forcing cases for the loop primitive.** They're the smallest interesting per-pixel loops in the inventory. Build `for_each_n`, express Noise as a 4-atom body + wrapper, validate parity against the legacy shader.
- **Plasma's parity test already exists** and the legacy primitive is still in the tree, so visual regression is easy to verify at every step.

Order of operations when this work starts:

1. Build the WGSL inliner + the `inline:` gate on the `primitive!` macro. Land it inert (no fusion pass yet) and verify all existing atoms still compile and run.
2. Build the fusion pass. Validate on a 3-atom chain (`sin_term → compose Add → trig_texture`) that produces the same output as the unfused version, bit-exact, with fewer dispatches.
3. Decompose Plasma's 6 atomizable variants into a graph. Verify fusion pass collapses them and parity holds.
4. Build `node.for_each_n`. Validate on a trivial 1-iteration case (should equal the unfused body) then a 3-iteration case.
5. Express Noise + Fractal as `for_each_n` graphs. Validate parity against the legacy shader.
6. Delete `node.plasma_pattern_2d` + its WGSL. Ship.

## 6. What this defers

`docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md` Tranche 3 (curated math kernels) had `plasma_pattern_2d` as one of four entries. The other three (`shape_2d`, `star_field_2d`, `generate_lissajous`) are already done or proceeding independently — they don't need loop primitives, only the fusion pass would benefit them (and that's purely a performance win, not a blocker). They can ship as atomized graphs now and pick up the fusion speedup later when the compiler lands.

Plasma is the only one that hits the loop wall hard enough to be worth deferring. Other deferred candidates worth flagging when this work starts: any future Mandelbrot / Julia / raymarcher / iterated-function-system generator, the `integrate_particles_attractor` family (per-particle RK2 substeps are conceptually a loop — though it's already atom-decomposed at a coarser granularity that avoids the issue), and any DNN post-processing chains where a sequence of pointwise ops currently eats multiple full-canvas passes.

## 7. Open questions for when this starts

- **Inlining vs. unrolling tradeoff.** At what loop count does the fused shader's register pressure outweigh the dispatch-launch savings? Probably 10-50 iterations is the sweet spot for `for_each_n`; beyond that the unrolled shader gets too register-heavy on Metal's tile budget. Worth benchmarking.
- **Stateful atoms inside `for_each_n`.** First version: state types are limited to scalar / vec2 / vec3 / vec4 locals. Texture reads from constant sources are fine; texture writes from inside the loop are not. Revisit if a real case demands more.
- **Fusion boundary heuristics.** Always fuse vs. cost-model fuse. Always-fuse is simpler and probably right for v1; cost-model can wait for evidence of pathological cases (massive register spills from over-aggressive fusion).
- **Debugging / introspection.** When the user opens a fused chain in the editor, do they see the 12-node sub-graph or the one fused dispatch? Probably both: the editor surfaces the graph, the runtime profiler surfaces the fused dispatches. The two views need to be wired up so a user clicking "why is this slow" lands on the right level.
- **WGSL hygiene.** Inlining means atoms can't rely on global naming conventions (workgroup IDs, builtin variables). The macro needs to enforce a calling convention — every inlineable body takes `(uv: vec2<f32>, inputs: ...) -> output` and uses no globals.
- **Does TouchDesigner actually have any of this?** Verify before committing to "MANIFOLD is the first node-graph VJ tool to ship a fusion compiler." Probably true, but worth checking the CHOP/TOP optimizer's actual extent before claiming it publicly.

## 8. Why now, vs. later

The forcing function is Plasma. Without the compiler, Plasma ships with two single-use loop atoms (`iterated_sin_fbm_2d`, `iterated_sin_warp_2d`) that are exactly the "per-shader primitive wrap" anti-pattern `docs/DECOMPOSING_GENERATORS.md` §5 warns against — they exist only to serve one variant each. With the compiler, they're 4-5 visible atoms in a graph. That's a much better shipping shape, and the architectural investment pays out across every future per-pixel iteration case (Mandelbrot, raymarching, …) plus the across-the-board performance win on every atomized effect.

The cost is real but bounded: 2-4 weeks of focused work, scoped, with Plasma as a concrete validation target so we know when it's done. The pile of future work it unlocks is much larger than its cost. The alternative — ship Plasma with the two anti-pattern atoms, defer the compiler indefinitely — leaves a known anti-pattern in the catalog and a known performance ceiling on every multi-atom effect.

The decision is to do the compiler first.
