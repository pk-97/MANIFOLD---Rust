# Ray Tracing — hybrid RT lighting for hero scenes

**Status:** IN PROGRESS — P0 DONE (mode B locked by Peter 2026-07-22, D11; 120-frame 4K run WAIVED — interim numbers + visual gate sufficed). Next: single overnight wave post-Wave-3, infra §2 → P1 → P2+P3, P4 parallel (D12); P5 export + P6 frame-interp cut from wave (D13) · 2026-07-22 · Fable
**Prerequisites:** none for P0. P1+ gated on P0 numbers and on RENDERING_INFRA_V2 §2 (G-buffer/motion vectors) for temporal pieces.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

This doc graduates **RENDERING_INFRA_V2_DESIGN.md §9 (Hybrid ray tracing)** into a full design, extended by the 2026-07-21 discussion. Governing insight: MANIFOLD's scene class — **static photoscanned hero objects, few lights, emissive sources, void backgrounds** — is close to the best case for hardware RT (tiny acceleration structures, most rays exit to void), while the current raster stack (GTAO + per-light shadow maps + env/sun) is the expensive general-case machinery, struggling at ~45fps/4K on the hero scenes. RT collapses that stack into one mechanism whose cost scales with rays×resolution, not lights×polys — and rays×resolution is exactly what MetalFX (already integrated) buys back. Peter's target, verbatim: **"I want things to look better than Unreal Engine maxed out"** — achievable *because* of narrow scope; the design must not creep toward a general-purpose renderer.

Companions: `REALTIME_3D_DESIGN.md` (the scene system RT extends), `MANIFOLD_GPU_ARCHITECTURE.md`, `MATERIAL_SYSTEM_DESIGN.md` (PBR model RT consumes), `CINEMATIC_POST_DESIGN.md` (post chain stays downstream, unchanged), `VULKAN_BACKEND_DESIGN.md` (parity seam).

## 1. Audit — what exists (verified 2026-07-21)

| Piece | Where | State |
|---|---|---|
| MetalFX Spatial upscaler | `crates/manifold-gpu/src/metal/metalfx.rs` | SHIPPED — ML spatial upscale, Lanczos fallback. *Temporal* variant (motion-vector-fed, the denoiser-adjacent one) NOT integrated. |
| Soft shadows (PCSS penumbra) | REALTIME_3D_DESIGN.md §status "SHIPPED @ `feat/pcss-penumbra` 2026-07-12" | The raster baseline RT shadows must beat. |
| GTAO | `crates/manifold-renderer/src/node_graph/primitives/ssao_gtao.rs` | The raster AO RT AO replaces. |
| PBR material model | `crates/manifold-core` material types per MATERIAL_SYSTEM_DESIGN.md (M1–M6 SHIPPED); glTF/Khronos import via `crates/manifold-renderer/src/node_graph/gltf_import.rs` | Metallic-roughness + emissive already deserialized and typed — RT consumes this as-is. **No new material system in v1** (Peter agrees; node-based material editor is a future direction — graphs *drive* material params first, *define* materials later). |
| Hybrid RT direction | RENDERING_INFRA_V2_DESIGN.md §9 | Direction + backend seam decided in principle; this doc is its graduation. Its rejections stand (no real-time path tracing; emissive-as-real-GI in raster rejected). |
| HDR pipeline + tonemapping | CINEMATIC_POST_DESIGN.md (SHIPPED) | Peter: HDR path "already sorted". RT plugs into existing linear-HDR → grade chain. |
| Hardware | M4 Max 36GB — Metal ray queries + `MTLAccelerationStructure` (hardware RT since M3). Frame interpolation requires Metal 4 / macOS Tahoe (min-OS decision, §5 D8). |

Extend, don't redesign. Instruction to executor: RT is an **extension of the REALTIME_3D scene pass** that outputs into the node graph like the current scene render does — not a set of graph pseudo-atoms, not a parallel renderer.

## 2. Decisions (D-numbered; from the 2026-07-21 discussion)

- **D1 — Hybrid, not path tracing.** Rasterize primary visibility at native output res (existing scene pass); trace *lighting terms* — shadow rays, AO rays, emissive/GI rays — at reduced rate, upscale the lighting, apply to full-res surfaces. Rejected: full path tracing real-time (RENDERING_INFRA_V2 rejection stands; export tier only, D7). Rejected: full-frame 1080p trace + MetalFX everything as the default — kept as P0 measurement mode C, may win on budget but softens primary edges.
- **D2 — P0 measures three resolution modes before anything is committed:** (A) native-4K raster + native-4K hard shadow rays; (B) native-4K raster + half-res soft-shadow/AO/GI rays upscaled (expected winner); (C) 1440p full-frame trace → MetalFX 4K. Baseline to beat: hero scene at 45fps/4K on current stack.
- **D3 — Temporal accumulation is trigger-aware.** Scene cuts (clip triggers) reset denoiser/upscaler history explicitly — the engine *knows* the cut, a structural advantage over pixel-guessing engines. Strobes are NOT cuts: accumulate demodulated irradiance (lighting separated from albedo) so light-intensity flips keep history. This is the design's answer to MANIFOLD's fast-cut/strobe content, which is exactly where UE-style TAA smears.
- **D4 — Emissive geometry lights the scene.** Emissive hero objects cast real light/shadow/god-rays — the headline stage win, and it subsumes RENDERING_INFRA_V2 §3.1's derived-light idea on the RT path.
- **D5 — Volumetrics get RT occlusion.** God rays / fog march with shadow-ray visibility instead of shadow-map lookups; emissive-colored volumetric glow. DOF/motion blur stay post-process (rejected as traced: ray-hungry; revisit trigger = measured spare budget). Bloom/CA/grain: untouched post, better HDR inputs.
- **D6 — Frame interpolation is a per-output option, default OFF for beat-reactive outputs.** ~33ms added latency at 30fps input (~16ms at 60). Fine for passive projection walls; off where the performer plays against the screen. Requires Metal 4/Tahoe.
- **D7 — Export path reuses the pipeline at offline quality.** Same code, ~10× rays, no denoiser compromise. Deliberate section, not an accident.
- **D8 — Min-OS.** Ray queries: no OS bump needed. Frame interpolation: Tahoe. Product floor decision is Peter's, deferred until D6's feature is built.
- **D9 — Backend seam (inherited, RENDERING_INFRA_V2 §9):** RT + upscaling behind per-backend traits in `manifold-gpu`; Metal RT + MetalFX now, Vulkan ray queries + FSR/DLSS when Vulkan lands. No Apple types leak. Cross-platform rule holds on paper in v1, in code when Vulkan builds.
- **D10 — Material scope is the shipped Khronos PBR model, frozen for v1.** Peter's scans are delit (calibration-cube captured, relight well) — asset ceiling confirmed OK. Plasticy look = audit roughness maps per hero asset, not renderer work.
- **D11 — Mode B committed (Peter, 2026-07-22).** Native-res raster + half-res soft-shadow/AO/GI rays, depth-aware upsample of the lighting buffers (trivial pass — ray *count* is the cost lever; native-res rays = 4× and blows budget). 120-frame 4K run WAIVED by Peter: interim numbers + visual read decided it. Modes A/C dead.
- **D12 — Single overnight wave, Fable→Opus→Sonnet (Peter, 2026-07-22).** Fable writes briefs (kernel signatures + required proofs) and reviews — writes no code; Opus dispatches; Sonnet lanes execute, porting the P0 prototype kernels rather than inventing. Spine: RENDERING_INFRA_V2 §2 (G-buffer/motion vectors; proves BUG-136) → P1 → P2+P3; P4 needs only infra §2, runs parallel. Staged lanes on one branch (everything touches the scene pass); only independent pieces fan out. Wave dispatches against post-Wave-3 layout. Denoiser look + final visual sign-off = Peter's morning gate.
- **D13 — P5 export path cut from this wave (Peter, 2026-07-22).** D7's design stands; build later, own trigger. P6 frame interpolation stays Tahoe-deferred (D6/D8); hand-rolled interpolation rejected outright.
- **D14 — Stored G-buffer is per-scene, tied to the RT toggle.** RENDERING_INFRA_V2 §2's open decision (always-store vs opt-in vs tier-gated) is answered narrowly for this wave: a scene with RT enabled stores depth + motion vectors to real textures; a non-RT scene keeps today's memoryless path and pays zero bandwidth. Widening to always-store (DoF/motion-blur/SSR for raster scenes) stays RENDERING_INFRA_V2's measured decision, untouched. Amendable by Peter without reopening this doc's phases. **As built (W0, `f76253f5`):** `EffectNode::force_consumed_outputs` default trait hook + one fold-in at `ExecutionPlan::compile`'s `consumed_outputs`; `render_scene` gained `rt_enabled: Bool` (default false, serialization lands in P1) — reuses GBUFFER_DESIGN's shipped lazy `depth`/`velocity` outputs, no new textures or formats. BUG-136 outcome: velocity math PROVED correct under a real orbit; live-app suspects remain open.
- **D15 — D3's cut-reset signal is NODE-LOCAL for v1 (ruled mid-wave 2026-07-22 night, Fable; P4's escalation).** No cut/trigger signal reaches node-graph evaluation today (audit: `FrameTime`/`EffectNodeContext` carry none; ContentPipeline has no clip-changed concept; the audio `"clip_trigger"` param is an unrelated envelope gate — repurposing it is FORBIDDEN). v1 shape: ONE shared runtime helper in `manifold-renderer`'s node_graph runtime (plain per-node state, no new shared state) that resets a node's temporal history when (a) its `owner_key` changes vs stored — covers live clip retriggers — or (b) frame time is discontinuous (>1.5× frame period, either direction) — covers seeks, loops, stutter retriggers, arrangement jumps. Strobes trip neither (same clip, continuous time), so D3's strobe rule holds by construction; demodulated accumulation (P2) handles in-clip light flips. P4 builds the helper; P2 MUST wire its accumulator to the SAME helper (P2's negative-`rg` no-second-reset-path gate enforces it). **Integration seam note for future work:** anything downstream needing "a cut happened" (frame interpolation P6, future temporal effects) wires to this helper, not a new detector — until the deferred engine-side signal (§7) replaces it, at which point the helper becomes the single place to rewire.

## 3. Expected wins (the stage translation)

Maps Peter's three named artifacts to mechanisms: **hatchy shadows** (shadow-map acne/PCF patterns) → killed outright by shadow rays. **Flickers** (cascade transitions, GTAO shimmer) → killed where they're approximation instability (any remaining flicker is an engine bug to hunt separately — do not credit RT blindly). **Plasticy** → half-fixed by real occlusion/bounce; other half is roughness-map quality (D10). Plus: contact-hardening shadows, emissive bounce, RT god rays. Sims: volume-rendered fluids/smoke ray-march without BVH cost; deforming *meshes* (cloth) pay per-frame BVH refit — the line item P0 must measure (RENDERING_INFRA_V2 §9 already flags `push_along_normals`).

## 4. Invariants & enforcement

- **RT output enters the graph as a texture like the current scene pass** — no new addressing/dispatch systems (zero-new-systems test). Enforcement: review gate at P1 brief time; `rg` for new id schemes in the RT phases' diffs.
- **No Apple types above `manifold-gpu`.** Enforcement: existing crate discipline + review; negative `rg` for `objc2|MTL` outside `manifold-gpu` at each phase gate.
- **History reset on cut** — flow-driven test once UI automation can trigger a scene cut over an RT scene: assert no ghost frame (pixel diff at cut+1). ⚠ VERIFY-AT-IMPL at P-brief time.

## 5. Phasing

Only **P0 is briefed now**; P1+ briefs are written *after* P0, to STANDARD, because their content depends on which mode wins (DESIGN_AUTHORING: no oracle numbers, no committed design).

### P0 — standalone Metal prototype (measurement, not product code)

- **Entry:** any delit hero photoscan; M4 Max; current OS (no Tahoe needed).
- **Deliverable:** a standalone Metal binary (scratch tree or `tools/`, NOT wired into the app): loads one scan, sun + env + one emissive, shadow+AO rays, MetalFX spatial upscale, fps counter. Modes A/B/C from D2 switchable.
- **Gate (measured numbers, reported):** fps per mode at 4K output; BVH build time for the scan; refit time for a deforming mesh; visual side-by-side PNG per mode vs the current raster render of the same scene. No "works correctly" — numbers and images.
- **Forbidden moves:** integrating into `manifold-renderer`; building a denoiser (P0 may be noisy — accumulation experiments only if time is free); any material system work.
- **Exit:** numbers pasted into this doc's §6 (added then), winning mode chosen with Peter, P1+ briefed.

### 5.1 P0 results (2026-07-22 — the full 120-frame 4K run was WAIVED by Peter; these interim numbers + the visual gate decided mode B, D11)

Harness: `tools/rt_prototype/` (standalone crate, manifold-gpu path dep for device+MetalFX; raw-MSL ray-query kernel `shaders/rt_trace.metal`). Asset: `cc0__japanese_apricot_prunus_mume.glb`, 1.43M tris. `--sun-only` flag zeroes the env for single-source looks. Comparison preset vs the current raster stack (matched camera/sun/albedo/AO/ACES; structural deltas documented in its description): `tools/rt_prototype/compare/RasterCompare.json` via `graph-tool render`.

- BVH: build ~110–167ms one-time; **refit ~12–16ms/frame at this poly count** — the deforming-mesh line item is real; static heroes unaffected.
- 4K single-frame (unvalidated, 1-frame avg — indicative only, mode C's trace_ms reading is implausible): A ~20ms, B ~25ms, C ~10.5ms. `combine` costs ~8ms flat in every mode — optimization headroom before P1.
- Visual gate: side-by-sides rendered (raster max-quality vs A/B/C, full lighting + sun-only). Peter's read: RT clearly better with full lighting; sun-only near-parity is expected — P0's GI gathers env+emissive only, no sun-bounce term (that's P3).
- Kernel lesson (cost one GPU-hang debug): buffer-visible MSL structs MUST use `packed_float3` — bare `float3` is sizeof 16 and desyncs from `#[repr(C)] [f32;3]`. See `feedback_wgsl_vec3_alignment` memory (now covers both WGSL and MSL).
- P0 self-emission gap: emissive surfaces light others but don't glow themselves (combine has no self-emission term) — add before judging emissive hero scenes.

### 5.2 Wave briefs (D12 — single overnight wave; Fable reviews per stage, Sonnet executes, Opus dispatches)

Spine W0 → P1 → P2 + P3; P4 parallel after W0. Staged lanes on one wave branch — every stage touches the scene pass. Lanes port `tools/rt_prototype/` kernels (`shaders/rt_trace.metal`, `shaders/gbuffer.metal`, `src/accel.rs`, `src/trace.rs`), they do not invent. One gpu-proofs run per stage gate; the full workspace sweep once, at landing, in the warm main checkout. Every stage: clippy `-p` touched crates; forbidden everywhere — new `Arc<Mutex>`, Apple types above `manifold-gpu`, parallel old paths kept alive, scope-widening into raster code the brief doesn't name.

**No PNG oracles for agents (Peter, 2026-07-22).** No agent — lane, reviewer, or dispatcher — gates on *reading* an image; models are unreliable at it. Every agent-run gate is a computed number or exit code: value tests against CPU-computed expected, scripted pixel-diffs with stated thresholds, region-mean probes at named coordinates. PNGs are still rendered at every stage, but solely as artifacts for **Peter's morning review** — that review closes the wave (denoiser look + final side-by-sides) and is the only image-judged gate in it.

**W0 — stored G-buffer, per-scene (D14; executes RENDERING_INFRA_V2 §2 narrowly).**
- *Entry:* main post-Wave-3; `rg -n "memoryless" crates/manifold-renderer/src/node_graph/primitives/render_scene.rs` (re-verify the depth-is-tile-memory claim before touching it).
- *Read-back:* RENDERING_INFRA_V2 §2 whole; REALTIME_3D §10 (why memoryless was chosen); `render_scene.rs` + `render_scene.wgsl`; BUG-136 backlog entry.
- *Deliverables:* RT-enabled scenes write depth + per-pixel motion vectors to real textures (camera-derived analytic vectors: previous-frame view-proj reprojection; graph-deformed geometry vectors DEFERRED — camera motion dominates Peter's scenes); non-RT scenes byte-identical to today.
- *Gate:* value test — motion vectors for a known camera delta vs CPU reprojection, exact math; BUG-136 oracle — two-frame orbit render, scripted readback of the motion-vector texture: mean |mv| > 0.5px AND per-pixel direction dot-product against the CPU-predicted field > 0.9 (proves or reroots the bug — record outcome in the backlog); negative `rg`: no stored-G-buffer write on the non-RT path; `MANIFOLD_RENDER_TRACE=1` run, no frame >20ms.
- *Demo (Peter only):* motion-vector false-color PNG next to the beauty frame — L2.

**P1 — hard shadow rays in the real scene pass (mode-B layout).**
- *Entry:* W0 landed on the wave branch; `tools/rt_prototype/` builds and runs (`cargo run --manifest-path tools/rt_prototype/Cargo.toml -- --help`).
- *Read-back:* D1/D9/D11/D14; `MANIFOLD_GPU_ARCHITECTURE.md`; prototype `accel.rs` + `rt_trace.metal`; `metalfx.rs` (the trait-seam precedent to copy).
- *Deliverables:* `manifold-gpu` RT trait (accel-structure build/refit + shadow-ray dispatch; Metal impl only, trait shaped so Vulkan ray queries fit — D9); accel structure built at scene load for RT-enabled scenes, kept resident (toggling RT live never builds mid-frame); half-res shadow-ray pass + depth-aware upsample + combine term replacing the shadow-map contribution when RT is on; scene-level `rt_enabled` through the existing scene def + EditingService path (serialized — round-trip gate applies).
- *Gate:* value test — shadow term for a 2-triangle occluder fixture vs CPU-computed expected (occluded texel = shadowed, unoccluded = lit, exact); scripted region probe on the apricot scan — mean luminance of a named occluded region drops ≥30% with RT shadows on vs shadows off, and a named lit region changes <5%; round-trip — save/reload an RT-enabled project, scripted probe still passes; negative `rg`: `objc2|MTL` zero hits outside `manifold-gpu`; no new id/addressing scheme in the diff (§4); gpu-proofs run.
- *Performer gesture:* toggle RT on a playing scene mid-set — no hitch, no rebuild stall (frame-time trace across the toggle, no frame >20ms).
- *Demo (Peter only):* raster-vs-RT side-by-side PNG pair — L2 (flow-driver toggle flow if reachable — then L3).

**P2 — soft shadows + AO + temporal accumulation with D3 resets.**
- *Entry:* P1 landed on wave branch.
- *Read-back:* D3 verbatim; prototype `trace.rs` (AO/GI gather); `ssao_gtao.rs` (the term being replaced); CORE_ENGINE_MAP trigger plumbing (where clip triggers surface to the renderer).
- *Deliverables:* soft-shadow (area-light cone) + AO rays in the half-res pass; temporal accumulation buffer with explicit reset on clip-trigger cut; demodulated irradiance accumulation (strobe ≠ cut); GTAO term replaced (not paralleled) when RT on.
- *Gate:* cut-reset proof — the §4 invariant's machine check, fully scripted: cut from scene X to scene Y, per-pixel diff of cut+1 frame vs a cold-start render of Y — mean abs diff < stated epsilon (no ghost of X); strobe proof — light intensity flip, cut+1-style diff vs cold-start *exceeds* epsilon (history retained, numerically shown); negative `rg`: GTAO dispatch absent from the RT-on path; gpu-proofs run. Denoiser *parameter* choices land as named constants with ranges — tuning is Peter's morning gate, not the lane's.
- *Demo (Peter only):* three PNGs — steady / cut+1 / strobe+1 — L2.

**P3 — emissive GI + RT volumetrics.**
- *Entry:* P2 landed on wave branch.
- *Read-back:* D4/D5; §5.1 self-emission gap note; prototype GI gather; VOLUMETRIC_LIGHT_DESIGN.md P1 findings (fog state of play, BUG-118 context — DEFERRED, do not touch).
- *Deliverables:* emissive gather incl. sun-bounce term (the §5.1 gap: P0 had env+emissive only) + self-emission in combine (emissives glow themselves); volumetric march sampling shadow-ray visibility instead of shadow maps when RT on (D5); emissive-colored volumetric glow.
- *Gate:* value test on the combine term — CPU-computed expected for a 2-triangle emissive fixture, including the self-emission term; scripted probes — neighbor-region mean brightness delta (emissive on vs off) > stated threshold, emissive-surface region mean ≥ its material emissive value, volumetric shaft region brighter with emissive on than off; gpu-proofs run.
- *Demo (Peter only):* emissive + god-ray PNGs — L2.

**P4 — MetalFX temporal upscaling (parallel lane; needs W0 only, not P1).**
- *Entry:* W0 landed on wave branch.
- *Read-back:* `metalfx.rs` whole (spatial variant is the template); D9; W0's motion-vector formats.
- *Deliverables:* temporal-variant behind the same `manifold-gpu` upscaler seam as spatial; camera jitter sequence in the scene pass when temporal upscaling is on; history reset wired to the same D3 trigger signal as P2's accumulator (shared plumbing, built once — whoever lands second wires to the first's signal, dispatcher sequences this); per-scene quality mode: native vs temporal-upscaled.
- *Gate:* scripted — temporal scaler produces the exact target resolution; upscaled frame vs native render of the same frame, mean abs diff below a stated coarse epsilon (proves it upscales the scene, not garbage — quality judgment is Peter's, not an agent's); cut-reset proof same as P2's numeric oracle; negative `rg`: no second trigger-reset plumbing path; gpu-proofs run.
- *Demo (Peter only):* upscaled-vs-native PNG pair — L2 (softness/ghosting is Peter's morning call).

Cut from wave: P5 export (D13), P6 frame interp (D6/D8). Escalation lines (misfit = stop and park, dispatcher charter applies): RT trait shape that Vulkan ray queries can't satisfy; motion vectors for graph-deformed geometry (deferred, but a lane that finds it load-bearing stops); anything wanting a new `Arc<Mutex>`.

## 6. Decided — do not reopen

1. Hybrid RT, never real-time path tracing (D1; RENDERING_INFRA_V2 rejection).
2. Prototype-first; no P1+ briefs without P0 numbers (D2).
3. Cuts reset history via triggers; strobes accumulate demodulated (D3).
4. v1 material model = shipped Khronos PBR, no material system (D10).
5. Frame interpolation per-output, off for beat-reactive outputs (D6).
6. RT lives in the REALTIME_3D scene pass, outputs into the graph; post stays downstream and composable.
7. Per-backend RT/upscale traits in `manifold-gpu` (D9).

## 7. Deferred (with revival triggers)

- **Node-based material editor** — Peter wants it eventually ("would be pretty cool in the future"); ramp: graphs drive material params (audio→roughness) first, define materials later. Trigger: RT v1 shipped + a scene that needs a material the fixed model can't express.
- **Traced DOF/motion blur** — trigger: measured spare ray budget after P3.
- **Engine-side cut signal (`cut_generation` on FrameTime, bumped by ContentPipeline on active-clip identity change)** — the D3-faithful "engine knows the cut" form, deferred by D15 to keep content-thread changes out of the overnight wave. Trigger: the node-local heuristic misses a cut or spuriously resets on real show content (Peter's review, any session). When built, it replaces D15's detector inside the shared helper — one rewire point.
- **Automated per-output display calibration** (camera + test patterns → per-output LUT) — adjacent, not RT; belongs with multi-display/projection-mapping. Trigger: next design session on either.
- **Two concurrent RT scenes (crossfade)** — budget for 2× or design non-overlapping transitions; decide with P0 numbers in hand.
- **Frame-budget sharing measurement** — P0 measures RT solo; a real-project run (RT scene + layers + effects + UI + encode) is the phase-2 measurement before any "60fps" claim about shows.
- **Min-OS floor for the product** — Peter decides when D6's feature exists.
