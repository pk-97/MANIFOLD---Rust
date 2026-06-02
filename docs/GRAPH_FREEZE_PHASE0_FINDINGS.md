# Graph Freeze Compiler — Phase 0 Findings + Architecture Direction

**Status:** Phase 0 complete (2026-06-02). Branch `freeze-compiler` (worktree off `c5c4b850`). Method: a headless GPU bench (`cargo run --release -p manifold-renderer --bin freeze-profile`) + a 44-preset static fusion-headroom sweep. No app, no GUI. Companion: `docs/GRAPH_COMPILER.md` (shelved transcript), `docs/CHANNEL_TYPE_SYSTEM.md` §16, memory `project_graph_freeze_compiler_direction`.

The §1–§4 measurements are settled. The §5 architecture is a **proposed direction informed by Phase 0**, for the Phase 2 design checkpoint (Peter's call before implementation) — not committed.

**Goal (Peter, 2026-06-02): a state-of-the-art graph compiler optimised for performance, covering BOTH the texture-pixel and buffer/array domains as first-class peers.**

## 1. The bench (texture domain)

`src/bin/freeze_profile.rs` builds each stateless effect preset's graph, drives the graph runtime headlessly through `Executor::execute_frame_with_gpu`, and times GPU work per frame with `commit_and_wait_completed` (wall-time ≈ GPU time). Avg over 120 frames after 8 warmup.

| Preset | steps | 1080p ms | 4K ms | 4K ms/step |
|---|---|---|---|---|
| **ColorGrade** | 9 | 0.85 | **2.74** | 0.304 |
| Glitch | 16 | 1.13 | 4.54 | 0.284 |
| VoronoiPrism | 16 | 1.08 | 4.34 | 0.271 |
| Dither | 4 | 0.71 | 2.39 | 0.598 |
| QuadMirror | 7 | 0.47 | 2.20 | 0.314 |
| HdrBoost | 7 | 0.35 | 1.33 | 0.191 |
| ChromaticAberration | 6 | 0.37 | 1.33 | 0.222 |
| EdgeStretch | 5 | 0.35 | 1.25 | 0.249 |
| Kaleidoscope | 5 | 0.34 | 1.21 | 0.241 |
| Infrared | 14 | 0.31 | 1.00 | 0.071 |
| InvertColors | 3 | 0.16 | 0.48 | 0.160 |

(Stateful effects need `execute_frame_with_state`; omitted from the effect pass.)

### 1b. Generators (buffer/particle domain) — Phase 0-b

Driven through the production `Generator::render` path, avg over 60 frames:

| Generator | 1080p ms | 4K ms | scaling | domain |
|---|---|---|---|---|
| Plasma | 0.25 | 0.64 | pixel | single-dispatch baseline |
| StarField | 1.16 | 5.24 | **pixel** (4.5×) | texture (per-pixel cellular/voronoi) |
| OilyFluid | 3.65 | 15.06 | **pixel** (4.1×) | texture (grid fluid: feedback/blur/advect at canvas res) |
| DigitalPlants | 2.43 | 2.79 | **flat** (1.1×) | **buffer** (array_math + 160k instanced render) |
| FluidSimulation | 11.34 | 14.17 | **flat** (1.25×) | **buffer/particle** (heaviest preset measured) |

**Key diagnostic — resolution scaling separates the domains.** Cost that stays ~flat from 1080p→4K is *element/buffer-bound* (cost = per-particle / per-instance compute, not pixels); cost that scales ~linearly with pixel count is *texture-bound*. So **FluidSimulation** (11→14 ms, flat) and **DigitalPlants** (2.4→2.8, flat) are the real **buffer-domain** fusion targets — their cost is the per-particle / per-instance chains. **OilyFluid**, despite the name, is *texture*-bound (a grid fluid that scales 4× with resolution), so its win is texture-domain (the color-grade tail + per-pixel composites), not buffer. FluidSimulation at 11–14 ms is the single heaviest preset in the library and it's buffer-bound — concrete confirmation that buffer fusion is not optional. (Per-stage attribution within a generator needs finer instrumentation than this total-frame bench; deferred — the generator hides its plan behind `JsonGraphGenerator`.)

## 2. Finding 1 — per-element cost is a bandwidth tax (~0.3 ms/full-canvas pass at 4K)

`ms/step` clusters at **0.25–0.3 ms at 4K** for full-canvas passes regardless of the math (ColorGrade 0.304, Glitch 0.284, VoronoiPrism 0.271, QuadMirror 0.314…), scaling ~3.3× down at 1080p. That math-independence is the signature of a **bandwidth-bound round-trip**: each pass reads + writes a full 4K RGBA16F texture (~67 MB), dominating arithmetic. The 5× thesis confirmed — the cost is intermediate round-trips, not compute. **This applies identically to the buffer domain**: an intermediate `Array<Particle>` or `Array<f32>` written then re-read is the same VRAM round-trip. Fusion keeps intermediates in registers either way.

Outliers confirm the model: Infrared's 0.071 (tiny W×1 LUT writes, not full canvas); Dither's 0.598 (two full-res texture reads + pattern).

## 3. Finding 2 — ColorGrade is the first target (~3–6× on the effect)

2.74 ms at 4K across 7 fusable pointwise dispatches (gain → saturation → hue_saturation → contrast → colorize → mix → clamp). Fusing 7 → 1 removes ~6 round-trips ≈ ~2 ms → **~0.5–0.9 ms, a 3–6× speedup**. Pure pointwise, no gather, no buffer — the simplest end-to-end proof, so it stays the Phase 4 bake.

## 4. Finding 3 — TWO co-equal fusion domains

The compiler must fuse per-element pure-op chains in **both** domains. They are the same problem with a different element-space:

### 4a. Texture-pixel domain (effects)
Element = pixel. Fusable: **pointwise** (own-pixel), **multi-input coincident-UV** (`mix`/`compose`/`dither` read 2+ textures at the same pixel), **single dependent gather** (`remap`/UV-warp/`color_lut` — one sample at a computed coord, coord-math + blend inline around it), **bounded small fixed multi-tap** (chromatic's 3 taps).

The strict single-input criterion the sweep used undercounts: it finds long runs only in ColorGrade. The richer model above unlocks the **largest effect family** — Kaleidoscope, Mirror, QuadMirror, EdgeStretch, Transform, ChromaticAberration (all "0 headroom" strict, all ~1-kernel-fusable) — plus the `mix`-blocked ones (Glitch, HdrBoost).

### 4b. Buffer/array-element domain (generators, sims) — **co-equal, channel-system-powered**
Element = array sample (particle / vertex / curve point / FFT bin / detection). Fusable: per-element pure ops on `Array<Channels>` — `array_math` element-wise chains, per-particle force ops (`apply_force → euler_step → anti_clump → wrap`), per-vertex transforms (cos/sin/scale/offset). These round-trip the buffer through VRAM exactly like pixel chains and collapse into one element-indexed compute kernel.

This is where **most of the library's cost lives** — the sweep showed generators/sims are buffer-dominated (DigitalPlants ~9 `array_math`, Duocylinder 8, every particle sim's force chain). Texture-only fusion leaves all of it slow.

**The channel system is the enabler here, and this is what it was designed for (CHANNEL_TYPE_SYSTEM.md §16):** the compiler reads a wire's `_SPECS` to emit the intermediate WGSL struct mechanically (§16.3.1), drops untouched channels (dead-channel elim, §16.3.2), and erases `rename`/`reorder`/`select` plumbing to nothing (§16.3.4). The texture domain barely needs channels (a pixel is always `vec4`); the buffer domain is **load-bearing** on them. Peter's earlier "the channel system is key here" intuition and this "buffer fusion is equally important" steer are the same insight.

True boundaries (both domains): large/variable multi-tap (blur, convolution, Sobel, LIC; buffer neighbor/reduce), stateful feedback, resolution/length change (resample, downsample, compaction/realloc), domain crossings (§5), FFI/DNN.

## 5. Proposed architecture — one unified fusion compiler (draft, Phase 2 review)

A single region-growing fusion pass, **parametrized by domain**, not two separate compilers:

1. **Classify** each node by `(domain, element-space, purity)`. Domain ∈ {Texture2D, Array, Texture3D}. Element-space = the iteration extent (canvas resolution; array length; volume dims).
2. **Grow maximal regions**: a fusable region is a connected subgraph of per-element *pure* ops in the **same domain at the same element-space**, bounded by cross-element ops, state, resolution/length changes, and domain crossings.
3. **Domain-crossing bridges are the seams**: `scatter_particles` (Array→Texture), `sample_texture_at_particles` (Texture→Array), `resolve_accumulator`, `render_3d_mesh`. Each stays its own dispatch; the regions on either side fuse internally.
4. **Emit one fused kernel per region** — texture: one dispatch over the pixel grid; buffer: one dispatch over the element index; intermediates live in registers. Same `wgsl_body` inliner + the existing naga → spirv-opt backend for both; the only per-domain difference is the iteration-space wrapper and the intermediate type (`vec4` vs the channel struct from §4b).
5. **`wgsl_body` calling convention is domain-parametric**: pixel op = `fn(color: vec4<f32>, uv) -> vec4<f32>`; element op = `fn(elem: T, index) -> T` (or per-channel). Codegen wraps the body in the right iteration space.

This is Halide-shaped (separate the per-element algorithm from the schedule/fusion) applied **uniformly across texture, buffer, and volume domains** — which is the "state of the art" bar. The freeze/closed-world step (un-exposed params → constants → DCE → fuse → specialize) and the verification harness (oracle: render/run two ways, diff) apply to both domains unchanged.

## 6. Chain fusion + automatic compilation (no freeze button)

Two graph levels: the **per-card graph** (authored in the editor) and the **chain graph** (the layer's effect rack, which the runtime already splices into one `ChainGraph` and rebuilds on rack edits per `EFFECT_RUNTIME_UNIFICATION`). Chain fusion is an added pass in that existing rebuild — fuse across card boundaries = **link-time optimization over the rack**. For this to work, a specialized/baked card must stay an *optimized graph*, NOT an opaque kernel — sealing it would recreate native's black-box-per-effect limitation and kill cross-card fusion. (Per-card freeze = compile-a-unit-with-optimization; chain fusion = LTO across units. Frozen cards are LTO-enabled object files, not sealed machine code.)

**No explicit Freeze button.** The closed world is defined by the **expose-state**: exposed params (bound to MIDI/Ableton/LFO) stay live uniforms; unexposed params are de-facto constants (nothing drives them at runtime) → auto-baked → constant-fold + DCE. The expose choices the user already makes ARE the freeze contract; there is nothing extra to declare. Baking is **stability-gated** (idle / perform-mode entry), not per-keystroke, so tuning an unexposed param stays fluid — it stays live until it settles, then the specialized kernel compiles in the background and hot-swaps. Compile runs on a worker off the UI + content-render threads; the unfused chain runs immediately; MTLBinaryArchive caches per-config across sessions — so rack edits never stall, and only a brief novel-config window runs unfused. **UX surface = a status badge** (fused / baking… / baked ✓), not a button. An optional auto-warm on perform-mode entry guarantees zero mid-show compiles.

## 7. Why this is hard for TouchDesigner (the substrate is the moat)

This compiler is not a novel algorithm (fusion + LTO is textbook); it is the **payoff of MANIFOLD's existing architectural bets**, and those bets are the moat. To current knowledge TouchDesigner does not do general multi-pass TOP fusion — its answer to "too slow" is the hand-written GLSL TOP, i.e. it pushes fusion onto the user. (Verify before claiming publicly; TD internals aren't fully open — but the GLSL-TOP escape hatch is strong evidence.) Three structural reasons it's hard for a TD-style tool:

1. **Sealed operators vs a decomposed atom library.** A TD TOP is a compiled black-box shader with no extractable per-element body and no self-describing wire types. Fusion needs atoms authored as inlineable fragments + a type system that hands the compiler the intermediate layout — MANIFOLD has both (primitive library + channel type system §16). You can't fuse what you can't introspect and splice.
2. **Live-always vs an authoring/perform split.** TD's identity is no-build-step interactivity; there's nowhere to hide a compile pass. MANIFOLD's authoring-vs-perform split makes a background compile invisible.
3. **Openness vs a closed world.** TD allows anything to change anytime (Python/C++/live topology); the specialization half needs a closed world (a known fixed-vs-live set). MANIFOLD's fixed-rack-during-show + explicit exposed-param set provides it.

The substrate — typed atom library + channel system + authoring/perform split, built for composability / AI authoring / drill-in — is exactly the precondition for this compiler. TD made reasonable bets for a general-purpose tool that preclude it without a foundation rebuild. Not a claim of across-the-board superiority: TD is far broader and more mature; this is one capability that falls out of MANIFOLD's narrower, more opinionated foundation.

## 8. Status & next

- **Phase 0: complete.** Bandwidth-round-trip thesis confirmed (both domains); ColorGrade first target (~3–6×); the breadth lever (fusion-model richness) and the second co-equal domain (buffer/array, channel-powered) identified.
- **Phase 1 (verification harness)** + **Phase 2 (`wgsl_body` convention)** next. The convention must be domain-parametric from day one (§5.5) — that is the keystone checkpoint, Peter's call before implementation.
- Reproduce: `cargo run --release -p manifold-renderer --bin freeze-profile`.
