# Ray Tracing — hybrid RT lighting for hero scenes

**Status:** IN PROGRESS — Tier 1+2, the motion class, and reflections R1/R2 LANDED on main (records: section 9.6 (phases)). Peter's L2 look PASSED 2026-07-24; RT is usable under model motion. OPEN: R3 textured roughness next (owns traced-detail wash + motion speckle); then Tier 3 item 8 multi-bounce GI (Peter's priority call 2026-07-29 — needs its own phase brief before execution); items 6/9 and P5 export (D13) / P6 frame interp stay show-need-triggered. D-61 sweep verdict PASSED 2026-07-29 after the D-63 clamp + v2 tone-mapped clip; faint extreme-transition residual accepted as BUG-im9s (residual streak, P3). R2 constants (incl. RT_REFL_CLAMP_GAMMA) still untuned — Peter's look, on demand. Perf profiling DEFERRED by Peter until the pipeline is complete. · 2026-07-29 · Fable
**Prerequisites:** none for P0. P1+ gated on P0 numbers and on RENDERING_INFRA_V2 section 2 (G-buffer/motion vectors) for temporal pieces.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

This doc graduates **RENDERING_INFRA_V2_DESIGN.md section 9 (Hybrid ray tracing)** into a full design, extended by the 2026-07-21 discussion. Governing insight: MANIFOLD's scene class — **static photoscanned hero objects, few lights, emissive sources, void backgrounds** — is close to the best case for hardware RT (tiny acceleration structures, most rays exit to void), while the current raster stack (GTAO + per-light shadow maps + env/sun) is the expensive general-case machinery, struggling at ~45fps/4K on the hero scenes. RT collapses that stack into one mechanism whose cost scales with rays×resolution, not lights×polys — and rays×resolution is exactly what MetalFX (already integrated) buys back. Peter's target, verbatim: **"I want things to look better than Unreal Engine maxed out"** — achievable *because* of narrow scope; the design must not creep toward a general-purpose renderer.

Companions: `REALTIME_3D_DESIGN.md` (the scene system RT extends), `MANIFOLD_GPU_ARCHITECTURE.md`, `MATERIAL_SYSTEM_DESIGN.md` (PBR model RT consumes), `CINEMATIC_POST_DESIGN.md` (post chain stays downstream, unchanged), `VULKAN_BACKEND_DESIGN.md` (parity seam).

## 1. Audit — what exists (verified 2026-07-21)

| Piece | Where | State |
|---|---|---|
| MetalFX Spatial upscaler | `crates/manifold-gpu/src/metal/metalfx.rs` | SHIPPED — ML spatial upscale, Lanczos fallback. *Temporal* variant (motion-vector-fed, the denoiser-adjacent one) NOT integrated. |
| Soft shadows (PCSS penumbra) | REALTIME_3D_DESIGN.md section status "SHIPPED @ `feat/pcss-penumbra` 2026-07-12" | The raster baseline RT shadows must beat. |
| GTAO | `crates/manifold-renderer/src/node_graph/primitives/ssao_gtao.rs` | The raster AO RT AO replaces. |
| PBR material model | `crates/manifold-core` material types per MATERIAL_SYSTEM_DESIGN.md (M1–M6 SHIPPED); glTF/Khronos import via `crates/manifold-renderer/src/node_graph/gltf_import.rs` | Metallic-roughness + emissive already deserialized and typed — RT consumes this as-is. **No new material system in v1** (Peter agrees; node-based material editor is a future direction — graphs *drive* material params first, *define* materials later). |
| Hybrid RT direction | RENDERING_INFRA_V2_DESIGN.md section 9 (Hybrid ray tracing (post-release) — GRADUATED 2026-07-21 → `RAYTRACING_DESIGN.md`) | Direction + backend seam decided in principle; this doc is its graduation. Its rejections stand (no real-time path tracing; emissive-as-real-GI in raster rejected). |
| HDR pipeline + tonemapping | CINEMATIC_POST_DESIGN.md (SHIPPED) | Peter: HDR path "already sorted". RT plugs into existing linear-HDR → grade chain. |
| Hardware | M4 Max 36GB — Metal ray queries + `MTLAccelerationStructure` (hardware RT since M3). Frame interpolation requires Metal 4 / macOS Tahoe (min-OS decision, section 5 D8). |

Extend, don't redesign. Instruction to executor: RT is an **extension of the REALTIME_3D scene pass** that outputs into the node graph like the current scene render does — not a set of graph pseudo-atoms, not a parallel renderer.

## 2. Decisions (D-numbered; from the 2026-07-21 discussion)

- **D1 — Hybrid, not path tracing.** Rasterize primary visibility at native output res (existing scene pass); trace *lighting terms* — shadow rays, AO rays, emissive/GI rays — at reduced rate, upscale the lighting, apply to full-res surfaces. Rejected: full path tracing real-time (RENDERING_INFRA_V2 rejection stands; export tier only, D7). Rejected: full-frame 1080p trace + MetalFX everything as the default — kept as P0 measurement mode C, may win on budget but softens primary edges.
- **D2 — P0 measures three resolution modes before anything is committed:** (A) native-4K raster + native-4K hard shadow rays; (B) native-4K raster + half-res soft-shadow/AO/GI rays upscaled (expected winner); (C) 1440p full-frame trace → MetalFX 4K. Baseline to beat: hero scene at 45fps/4K on current stack.
- **D3 — Temporal accumulation is trigger-aware.** Scene cuts (clip triggers) reset denoiser/upscaler history explicitly — the engine *knows* the cut, a structural advantage over pixel-guessing engines. Strobes are NOT cuts: accumulate demodulated irradiance (lighting separated from albedo) so light-intensity flips keep history. This is the design's answer to MANIFOLD's fast-cut/strobe content, which is exactly where UE-style TAA smears.
- **D4 — Emissive geometry lights the scene.** Emissive hero objects cast real light/shadow/god-rays — the headline stage win, and it subsumes RENDERING_INFRA_V2 section 3.1's derived-light idea on the RT path.
- **D5 — Volumetrics get RT occlusion.** God rays / fog march with shadow-ray visibility instead of shadow-map lookups; emissive-colored volumetric glow. DOF/motion blur stay post-process (rejected as traced: ray-hungry; revisit trigger = measured spare budget). Bloom/CA/grain: untouched post, better HDR inputs.
- **D6 — Frame interpolation is a per-output option, default OFF for beat-reactive outputs.** ~33ms added latency at 30fps input (~16ms at 60). Fine for passive projection walls; off where the performer plays against the screen. Requires Metal 4/Tahoe.
- **D7 — Export path reuses the pipeline at offline quality.** Same code, ~10× rays, no denoiser compromise. Deliberate section, not an accident.
- **D8 — Min-OS.** Ray queries: no OS bump needed. Frame interpolation: Tahoe. Product floor decision is Peter's, deferred until D6's feature is built.
- **D9 — Backend seam (inherited, RENDERING_INFRA_V2 section 9):** RT + upscaling behind per-backend traits in `manifold-gpu`; Metal RT + MetalFX now, Vulkan ray queries + FSR/DLSS when Vulkan lands. No Apple types leak. Cross-platform rule holds on paper in v1, in code when Vulkan builds.
- **D10 — Material scope is the shipped Khronos PBR model, frozen for v1.** Peter's scans are delit (calibration-cube captured, relight well) — asset ceiling confirmed OK. Plasticy look = audit roughness maps per hero asset, not renderer work.
- **D11 — Mode B committed (Peter, 2026-07-22).** Native-res raster + half-res soft-shadow/AO/GI rays, depth-aware upsample of the lighting buffers (trivial pass — ray *count* is the cost lever; native-res rays = 4× and blows budget). 120-frame 4K run WAIVED by Peter: interim numbers + visual read decided it. Modes A/C dead.
- **D12 — Single overnight wave, Fable→Opus→Sonnet (Peter, 2026-07-22).** Fable writes briefs (kernel signatures + required proofs) and reviews — writes no code; Opus dispatches; Sonnet lanes execute, porting the P0 prototype kernels rather than inventing. Spine: RENDERING_INFRA_V2 section 2 (G-buffer/motion vectors; proves BUG-136) → P1 → P2+P3; P4 needs only infra section 2, runs parallel. Staged lanes on one branch (everything touches the scene pass); only independent pieces fan out. Wave dispatches against post-Wave-3 layout. Denoiser look + final visual sign-off = Peter's morning gate.
- **D13 — P5 export path cut from this wave (Peter, 2026-07-22).** D7's design stands; build later, own trigger. P6 frame interpolation stays Tahoe-deferred (D6/D8); hand-rolled interpolation rejected outright.
- **D14 — Stored G-buffer is per-scene, tied to the RT toggle.** RENDERING_INFRA_V2 section 2's open decision (always-store vs opt-in vs tier-gated) is answered narrowly for this wave: a scene with RT enabled stores depth + motion vectors to real textures; a non-RT scene keeps today's memoryless path and pays zero bandwidth. Widening to always-store (DoF/motion-blur/SSR for raster scenes) stays RENDERING_INFRA_V2's measured decision, untouched. Amendable by Peter without reopening this doc's phases. **As built (W0, `f76253f5`):** `EffectNode::force_consumed_outputs` default trait hook + one fold-in at `ExecutionPlan::compile`'s `consumed_outputs`; `render_scene` gained `rt_enabled: Bool` (default false, serialization lands in P1) — reuses GBUFFER_DESIGN's shipped lazy `depth`/`velocity` outputs, no new textures or formats. BUG-136 outcome: velocity math PROVED correct under a real orbit; live-app suspects remain open.
- **D17 — Acceleration-structure builds are async-ordered, never synchronous mid-frame (ruled mid-wave 2026-07-22 night, Fable; BUG-308 root cause).** The bug class: a private command buffer with commit+wait mid-frame races the shared encoder's uncommitted mesh writes (accel built from stale vertex data, then cached forever by the dirty-key) AND stalls the frame. Banned outright on the RT path. Correct form: accel build/refit command buffers enqueue on the same queue AFTER the frame's pending shared-encoder work commits — Metal commit-order guarantees the data dependency with no wait; a completion handler flips an atomic accel-ready flag; until ready, an RT-enabled scene renders its existing raster shadow path (explicit, logged, ~7-frame transition at P0's 110–167ms build cost) and the mask path activates when ready. **Stage consequence:** toggling RT mid-set is a brief soft lighting transition with zero frame hitch — the inline alternative (threading the build through the frame encoder) was REJECTED because it turns first-enable into a 110ms+ frame. **Seam note:** any future GPU work that builds long-lived resources from GPU-written buffers (P2 denoiser history alloc, P3 emissive tables, mesh refit for sims) follows the same enqueue-after-commit + ready-flag pattern. **As built:** the lane satisfied D17 via defer-to-next-frame (build enqueued only once the accel key recurs unchanged, by which point the prior frame's mesh-gen has committed) — blessed; caveat: a deforming mesh whose vertices change WITHOUT a key change would get a one-frame-stale accel — attached to the already-flagged refit line item (section 3), must be revisited by any sim/deform RT phase. Tracer pipeline construction prewarms in the node's first-evaluate window (BUG-310).
- **D16 — P1 integration: RT shadows ride the existing opaque depth prepass; forward stays forward (ruled mid-wave 2026-07-22 night, Fable; P1's escalation).** No deferred combine pass is built. `render_scene` already renders an opaque depth prepass (`opaque_depth_snapshot`) before its lighting pass — that is the mode-B slot. When `rt_enabled`: half-res shadow-ray dispatch after the prepass (origins from prepass depth + inverse view-proj; bias normals via screen-space reconstruction from depth — no normal G-buffer target in P1; P2 adds one only if bias artifacts or AO demand it), depth-aware upsample to native, and the forward lighting shader samples the mask as the light visibility factor in place of the shadow-map sample (one uniform-gated bool, not a pipeline permutation). Shadow maps stop rendering for RT scenes. **Seam note:** P2's soft shadows + AO join the SAME half-res dispatch and SAME upsample — this is the extension point, not a new pass.
- **D15 — D3's cut-reset signal is NODE-LOCAL for v1 (ruled mid-wave 2026-07-22 night, Fable; P4's escalation).** No cut/trigger signal reaches node-graph evaluation today (audit: `FrameTime`/`EffectNodeContext` carry none; ContentPipeline has no clip-changed concept; the audio `"clip_trigger"` param is an unrelated envelope gate — repurposing it is FORBIDDEN). v1 shape: ONE shared runtime helper in `manifold-renderer`'s node_graph runtime (plain per-node state, no new shared state) that resets a node's temporal history when (a) its `owner_key` changes vs stored — covers live clip retriggers — or (b) frame time is discontinuous (>1.5× frame period, either direction) — covers seeks, loops, stutter retriggers, arrangement jumps. Strobes trip neither (same clip, continuous time), so D3's strobe rule holds by construction; demodulated accumulation (P2) handles in-clip light flips. P4 builds the helper; P2 MUST wire its accumulator to the SAME helper (P2's negative-`rg` no-second-reset-path gate enforces it). **Integration seam note for future work:** anything downstream needing "a cut happened" (frame interpolation P6, future temporal effects) wires to this helper, not a new detector — until the deferred engine-side signal (section 7) replaces it, at which point the helper becomes the single place to rewire.

## 3. Expected wins (the stage translation)

Maps Peter's three named artifacts to mechanisms: **hatchy shadows** (shadow-map acne/PCF patterns) → killed outright by shadow rays. **Flickers** (cascade transitions, GTAO shimmer) → killed where they're approximation instability (any remaining flicker is an engine bug to hunt separately — do not credit RT blindly). **Plasticy** → half-fixed by real occlusion/bounce; other half is roughness-map quality (D10). Plus: contact-hardening shadows, emissive bounce, RT god rays. Sims: volume-rendered fluids/smoke ray-march without BVH cost; deforming *meshes* (cloth) pay per-frame BVH refit — the line item P0 must measure (RENDERING_INFRA_V2 section 9 already flags `push_along_normals`).

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
- **Exit:** numbers pasted into this doc's section 6 (added then), winning mode chosen with Peter, P1+ briefed.

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

**W0 — stored G-buffer, per-scene (D14; executes RENDERING_INFRA_V2 section 2 narrowly).**
- *Entry:* main post-Wave-3; `rg -n "memoryless" crates/manifold-renderer/src/node_graph/primitives/render_scene.rs` (re-verify the depth-is-tile-memory claim before touching it).
- *Read-back:* RENDERING_INFRA_V2 section 2 whole; REALTIME_3D section 10 (why memoryless was chosen); `render_scene.rs` + `render_scene.wgsl`; BUG-136 backlog entry.
- *Deliverables:* RT-enabled scenes write depth + per-pixel motion vectors to real textures (camera-derived analytic vectors: previous-frame view-proj reprojection; graph-deformed geometry vectors DEFERRED — camera motion dominates Peter's scenes); non-RT scenes byte-identical to today.
- *Gate:* value test — motion vectors for a known camera delta vs CPU reprojection, exact math; BUG-136 oracle — two-frame orbit render, scripted readback of the motion-vector texture: mean |mv| > 0.5px AND per-pixel direction dot-product against the CPU-predicted field > 0.9 (proves or reroots the bug — record outcome in the backlog); negative `rg`: no stored-G-buffer write on the non-RT path; `MANIFOLD_RENDER_TRACE=1` run, no frame >20ms.
- *Demo (Peter only):* motion-vector false-color PNG next to the beauty frame — L2.

**P1 — hard shadow rays in the real scene pass (mode-B layout).**
- *Entry:* W0 landed on the wave branch; `tools/rt_prototype/` builds and runs (`cargo run --manifest-path tools/rt_prototype/Cargo.toml -- --help`).
- *Read-back:* D1/D9/D11/D14; `MANIFOLD_GPU_ARCHITECTURE.md`; prototype `accel.rs` + `rt_trace.metal`; `metalfx.rs` (the trait-seam precedent to copy).
- *Deliverables:* `manifold-gpu` RT trait (accel-structure build/refit + shadow-ray dispatch; Metal impl only, trait shaped so Vulkan ray queries fit — D9); accel structure built at scene load for RT-enabled scenes, kept resident (toggling RT live never builds mid-frame); half-res shadow-ray pass + depth-aware upsample + combine term replacing the shadow-map contribution when RT is on; scene-level `rt_enabled` through the existing scene def + EditingService path (serialized — round-trip gate applies).
- *Gate:* value test — shadow term for a 2-triangle occluder fixture vs CPU-computed expected (occluded texel = shadowed, unoccluded = lit, exact); scripted region probe on the apricot scan — mean luminance of a named occluded region drops ≥30% with RT shadows on vs shadows off, and a named lit region changes <5%; round-trip — save/reload an RT-enabled project, scripted probe still passes; negative `rg`: `objc2|MTL` zero hits outside `manifold-gpu`; no new id/addressing scheme in the diff (section 4); gpu-proofs run.
- *Performer gesture:* toggle RT on a playing scene mid-set — no hitch, no rebuild stall (frame-time trace across the toggle, no frame >20ms).
- *Demo (Peter only):* raster-vs-RT side-by-side PNG pair — L2 (flow-driver toggle flow if reachable — then L3).

**P2 — soft shadows + AO + temporal accumulation with D3 resets.**
- *Entry:* P1 landed on wave branch.
- *Read-back:* D3 verbatim; prototype `trace.rs` (AO/GI gather); `ssao_gtao.rs` (the term being replaced); CORE_ENGINE_MAP trigger plumbing (where clip triggers surface to the renderer).
- *Deliverables:* soft-shadow (area-light cone) + AO rays in the half-res pass; temporal accumulation buffer with explicit reset on clip-trigger cut; demodulated irradiance accumulation (strobe ≠ cut); GTAO term replaced (not paralleled) when RT on.
- *Gate:* cut-reset proof — the section 4 invariant's machine check, fully scripted: cut from scene X to scene Y, per-pixel diff of cut+1 frame vs a cold-start render of Y — mean abs diff < stated epsilon (no ghost of X); strobe proof — light intensity flip, cut+1-style diff vs cold-start *exceeds* epsilon (history retained, numerically shown); negative `rg`: GTAO dispatch absent from the RT-on path; gpu-proofs run. Denoiser *parameter* choices land as named constants with ranges — tuning is Peter's morning gate, not the lane's.
- *Demo (Peter only):* three PNGs — steady / cut+1 / strobe+1 — L2.

**P3 — emissive GI + RT volumetrics.**
- *Entry:* P2 landed on wave branch.
- *Read-back:* D4/D5; section 5.1 self-emission gap note; prototype GI gather; VOLUMETRIC_LIGHT_DESIGN.md P1 findings (fog state of play, BUG-118 (render-scene-fog-washes-out-instead-of-depth-gra…) context — DEFERRED, do not touch).
- *Deliverables:* emissive gather incl. sun-bounce term (the section 5.1 gap: P0 had env+emissive only) + self-emission in combine (emissives glow themselves); volumetric march sampling shadow-ray visibility instead of shadow maps when RT on (D5); emissive-colored volumetric glow.
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

## 8. v2 roadmap — from landed skeleton to stage-ready (captured 2026-07-23, Peter's first real look)

Peter's first in-app session with RT v1 (apricot scene) found three integration bugs — all
fixed same-day — and established that the remaining gaps are structural, not tuning. This
section is the durable brief for the next RT session.

**Fixed 2026-07-23 (context for the roadmap, not open work):**
- RT toggles invisible everywhere — `card_visible_for` had no `node.render_scene` arm.
- RT ambient floor unremovable — now rides the scene Ambient knob; knob 0 = true black.
- Sun counted twice — the irradiance kernel carried its own sun*n·l*vis copy on top of the
  raster light loop's; irradiance is now ambient*ao + gi only. Post-fix probe:
  occluded region drop 45.5%, lit region change 2.7% (`rt_p1_region_probe`, 18/18 rt proofs green).

**Tier 1 — one amendment, shared infrastructure, do first (unblocks everything):**
1. **Motion-reprojected, validity-tested accumulation** (BUG-311, HIGH). `accumulate_irradiance`
   blends same-texel history — lighting ghosts behind ANY movement. Reproject through motion
   vectors (`prev_view_proj` + per-object `prev_model` already exist for MetalFX), reject on
   depth/normal mismatch, fall back to current at disocclusions. SVGF-style.
2. **Real surface normals in the kernel.** Rays currently use depth-buffer finite-difference
   normals — camera-facing, wrong at silhouettes/thin geometry (i.e. at every petal). Thread a
   per-object vertex-normal buffer through `RtObjectGeometry` (the same G-buffer plumbing the
   reprojection validity test needs). Also upgrades the GI bounce from its flat-cosine stand-in.
3. **Variance-guided denoiser** (BUG-312, blocked on 1). Replace the depth-only bilateral
   upsample with an SVGF-class spatial+temporal filter; only after that, re-judge the ray
   budgets (the committed constants in render_scene.rs are placeholders for accumulated input).

**Tier 2 — correctness + cost, after Tier 1:**
4. **Alpha-aware rays.** Intersectors `force_opacity(opaque)` — cutout foliage shadows wrong.
5. **Live MetalFX wiring.** The P4 seam exists (scaler, jitter, toggle); the reduced-res-render →
   upscale path into scene output is still unwired. Same motion vectors as Tier-1 item 1.

**Tier 3 — the SOTA features, pick by show need (all depend on Tier 1's stack):**
6. **Many-light sampling (ReSTIR).** RT shadows are sun-only today; every other light is still a
   shadow map. For MANIFOLD's emissive/strobe-heavy scene class this is the highest-value one.
7. **RT reflections** (specular rays) — the most visible missing RT feature.
8. **Multi-bounce GI** (path-traced or probe-cached) — v1 is one bounce, flat approximation.
9. **RT translucency / volumetric interaction** — light through petals, rays through haze;
   furthest out, closest to the Latent Space aesthetic.

Verification instrument for all of it: `rt_p1_region_probe` (numeric region-luminance A/B through
the real node path) + Peter's in-app look (L2). Motion artifacts need a MOVING oracle — a
scripted-orbit capture diffing consecutive frames, to be built with Tier 1; a green still-frame
probe cannot see BUG-311's class.

### 8.1 Tier 1 wave (dispatched 2026-07-23)

**D18 — Tier 1 executes as one staged wave, Opus dispatcher + Sonnet lanes (D12 pattern; Peter 2026-07-23).** Peter's same-day report: **static shots with NO motion also read worse than raster** — still quality (BUG-312 speckle + depth-derived-normal shading errors) is a first-class gate alongside BUG-311's motion class, not a footnote. Wave shape:

- **T1-A — oracles first** (bisection-instrument-first): scripted-orbit consecutive-frame ghost metric + static-frame luminance-variance speckle metric, both through the real node path (`rt_p1_region_probe` precedent, exit-code gates). Pre-fix baselines must TRIP both metrics before any fix lands — an oracle that can't see the bug gates nothing.
- **T1-B — real vertex normals** through `RtObjectGeometry` into the kernel (shading bias, AO/GI cosine); screen-space depth-normal reconstruction deleted from the RT path (no parallel old path).
- **T1-C — motion-reprojected, validity-tested accumulation** (BUG-311): reproject history via existing `prev_view_proj` + per-object `prev_model`; reject on depth/normal mismatch; current-frame fallback at disocclusions; wired to the D15 reset helper (negative `rg`: no second reset path).
- **T1-D — variance-guided spatial filter (SVGF-class)** replacing the depth-only bilateral upsample, **+ blue-noise ray-direction sequences; ray-budget constants re-judged only after this lands** (BUG-312's ordering). Constants stay named-with-ranges; Peter's look pass closes the wave (L2 — the only image gate).

Staging: T1-A ∥ T1-B, then T1-C → T1-D, all on `wave/rt-t1`. Pre-allocated BUG range: 315–318. Out of scope (escalate, don't build): MetalFX live wiring, alpha-aware rays, any Tier-3 feature, new `Arc<Mutex>`. Lane briefs: `.claude/orchestration/rt-t1-queue.md` (gitignored process state).

- **D19/D20 — motion-ghost oracle rulings (Fable, mid-wave).** The T1-A ORBIT oracle was confounded (BUG-316, né 315 — id collision with main's stale-roughness bug: tracked point on the shadow boundary measures real parallax); rewritten to accumulated-vs-cold-start at same pose (D19), still non-discriminating even at ~10°/frame stimulus (D20). Terminator: one-shot instrumentation inside `accumulate_irradiance` proved the reprojection ACTIVE (95–98% of texels reproject to a shifted history texel, 97%+ pass validity) — BUG-311 accepted FIXED on that evidence; both motion oracles kept `#[ignore]`d with full investigation recorded in their doc comments. **Standing lesson: numeric pose/frame-diff metrics cannot isolate ghosting from legitimate accumulation lag at these alphas — motion-quality judgment on this surface is Peter's L2 look until someone designs a genuinely discriminating instrument (no third redesign inside a wave).**
- **T1-D honest residual:** STILL oracle improved 1.076e-4 → 8.6e-5 (threshold 7e-5) but the residual is proven scene structure (box-blur + 16× samples both no-ops), not speckle — kept `#[ignore]`d, threshold untouched. Ray budgets unchanged pending Peter's look.
- Lane-surfaced gotchas for future RT test authors: orbit tests must step `dt` with `time` or `TemporalResetDetector` hard-resets every frame; async accel builds need per-frame commit (batching warmup frames into one encoder breaks the RT-D4 state machine).

### 8.2 Tier 2 wave (dispatched 2026-07-23)

**D21 — Tier 2 executes as a second staged wave, same D12/D18 pattern (Peter 2026-07-23), on `wave/rt-t2`:**

- **T2-A — alpha-aware rays (section 8 item 4).** Cutout materials stop shadowing as solid slabs. Mechanism: materials flagged alpha-mask get non-opaque intersection — the kernel's intersection query iterates candidate triangles, samples base-color alpha at the candidate's interpolated UV, and continues through texels below the material's alpha cutoff; opaque materials keep the `force_opacity(opaque)` fast path untouched (cost discipline — alpha-test only where flagged). Plumbing precedent: T1-B's bindless per-object table (normals) extends to UV + base-color-texture + cutoff per object. Applies to shadow, AO, and GI rays in the same pass — one mechanism, not three.
- **T2-B — live MetalFX temporal wiring (section 8 item 5).** P4's seam (scaler, `TemporalResetDetector`, jitter, per-scene toggle) finally drives the real path: RT-enabled scene with temporal quality mode renders reduced-res and upscales into the scene output. Reuses W0 motion vectors and the D15/RT-D2 reset helper — negative `rg`: no second reset or jitter path. **Stage consequence:** this is the fps lever — same look, rays at a fraction of native res; the ray-budget re-judge (post-Tier-1 open item) happens at the upscaled config, not before.

Staging: T2-A → T2-B sequential (both touch `render_scene.rs`). Pre-allocated BUG range: 317–318 (315 lost to collision, 316 spent). Out of scope: Tier 3 features, ray-budget changes, deforming-mesh refit (stays attached to the section 3/D17 sim line item), new `Arc<Mutex>`. Lane briefs: `.claude/orchestration/rt-t2-queue.md`.

**D22 — reduced-res render path for temporal upscale (Fable, mid-wave 2026-07-23; T2-B's park).** The T2-B lane correctly stopped: P4 landed only the seam (scaler type, jitter, toggle) — no reduced-res render path exists to wire it into. Ruled seams, in the existing design's spirit (P4 committed the per-scene mode; D2 mode C supplies the measured config):

1. **Path shape:** quality mode = temporal-upscaled → `render_scene` draws color + depth + velocity into internal scratch targets at render res = output res × `RT_TEMPORAL_RENDER_SCALE` (named constant, **1/1.5 linear** — P0 mode C's measured 1440p→4K config; Peter-amendable, not a lane knob). MetalFX temporal consumes color + depth + motion (+ P4's jitter) → native-res color = the scene's graph output. Native mode keeps today's direct draw path, byte-identical (machine-diff gated). Scratch targets follow `render_scene.rs`'s existing target-allocation pattern — zero new systems.
2. **`depth`/`velocity` graph outputs stay at render res when upscaled mode is on** — MetalFX doesn't upscale them and building a bespoke upscaler for them is FORBIDDEN. Documented limitation; revival trigger = a downstream consumer needing native-res depth from an upscaled RT scene.
3. **Not mode C's resurrection:** modes A/C stay dead as *defaults* (D11); this is P4's committed per-scene opt-in trade (fps lever for heavy scenes). The RT half-res ray pass now keys off render res (rays at ~1/3 native in upscaled mode) — that compounding is the point, and the ray-budget re-judge (post-wave) happens at this config.

**Executed 2026-07-23 (same night).** T2-A `62244989` — one shared `walk_with_alpha_test` intersection-query walk (raytrace.rs), 5 call sites/ray classes, per-texel alpha via interpolated UV + base-color sample, opaque fast path untouched; 2 exact CPU-oracle asserts; API gotcha for future kernels: Metal's `intersection_query` commits via `commit_triangle_intersection()`, not `accept_intersection()`. T2-B `fa7a6d7f` — D22 as ruled: `RT_TEMPORAL_RENDER_SCALE` 2/3, scratch color target + MetalFX temporal to native as graph color output, depth/velocity outputs at render res (documented limitation, D22.2), reset on the sole detector; 6 new gpu-proofs. Native-mode byte-identity gate closed at landing by Fable with a real machine diff (graph-tool render of RasterCompare at pre/post commits: renderer proven deterministic, outputs `cmp`-identical) after the lane correctly declined to claim it from a code-diff argument.
### 8.3 T2-C — per-object motion reprojection (2026-07-23, post-Tier-2)

**Closes T1-C's own recorded gap.** section 8.1's T1-C spec called for reprojection "via existing `prev_view_proj` **+ per-object `prev_model`**"; the lane shipped the camera term only (no per-pixel object id existed to index a `prev_model` with) and recorded the limitation: an animated object's pixels fail the validity test and fall back to current-frame-only. Peter's in-app look after BUG-320 (accel refit) isolated exactly that residual: dragging/rotating a mesh shimmers at the raw ray budget for the whole gesture, then "snaps" to the converged look ~1s after motion stops. With RT's committed budgets (4 AO / 2 GI spp at half res) the accumulation IS the image quality, so losing history for the duration of a gesture is losing the look for the duration of a gesture — the performance case, not an edge case.

**Mechanism (one term added to the existing reprojection, no new pass):** `trace_shadow_rays` already casts a primary ray for T1-B's vertex normal — its `get_committed_instance_id()` rides out in the free `.w` of the normal texture (`-1` = no object: void, or a frame that cast no primary ray), passed through `upsample_shadow` (nearest tap's id, never blended) and `atrous_filter` (center id, untouched). `accumulate_irradiance` gains a `constant float4x4* obj_motion` buffer (`prev_model * inverse(model)`, both straight off the draw uniforms MetalFX velocity already maintains) and carries the reconstructed world position back through that object's own delta BEFORE the existing `prev_view_proj` reprojection. An out-of-range id (stale texture content across a topology change) or `-1` reprojects camera-only — the pre-T2-C path, still correct, never an out-of-bounds read.

**Gate:** `object_motion_reprojection_retains_history_where_camera_only_rejects` (`rt_p2_soft_ao_temporal.rs`) — an object moving 0.2 in NDC z between frames, run twice on identical fixtures: WITH the motion table, mean red = 0.8496 against CPU-computed `1 - alpha` = 0.85 (history retained); WITHOUT it (`obj_count = 0` — literally the pre-T2-C behavior), 0.0 (history discarded by the depth reject). The control leg is what makes this a proof of the OBJECT term specifically rather than of accumulation in general — the same discriminating-oracle discipline D19/D20 cost a whole wave to learn.

**Still Peter's L2 look:** per D19/D20's standing lesson, no numeric metric on this surface separates ghosting from legitimate accumulation lag. The proof above is value-level (does history survive a move — yes/no), not a quality verdict. Whether a fast rotation now holds its converged look is the eye test.

**Outcome of that eye test (2026-07-23, same day): initially FAILED — see `BUG-322`, now fixed and confirmed.** Peter's rotation still flickers, with shadows changing shape *and* location for the duration of the gesture and snapping back on stop. T2-C's mechanism is correct and proven, but it is not what he was seeing; nor was BUG-320's. **Standing lesson, stronger than D19/D20's:** two consecutive mechanism-level fixes were declared done on value-level GPU proofs of *reasoned* causes, with no in-app observation anywhere in the loop — and both missed. On this surface, a proof that the mechanism now behaves as designed is not evidence that the mechanism was the cause. The next attempt starts with an instrumented in-app drag (per-frame `rebuild_epoch`/`topo_key`/`rt_accel_built`), not with a code reading.

**BUG-322 close-out (2026-07-23) — the diagnosis, and the method lesson that cost three attempts.** The helmet shimmer was NOT an acceleration-structure problem. `accumulate_irradiance` compared `stored_normal` (world space, written last frame, in the object's PREVIOUS orientation) against `cur_normal` (this frame's orientation) with no correction, so a ROTATING object failed the depth/normal validity test every frame and discarded all temporal history — leaving the raw 4 AO / 2 GI half-res budget on screen. T2-C had carried the reprojected POSITION through `obj_motion` and never did the same for the normal. Fixed by carrying the current normal back through that matrix's rotation block and comparing in one orientation.

Three things future RT work should take from how this was found:
1. **The split case is the diagnosis.** Peter's "flowers look correct, the helmet has the problem" eliminated every cause common to both objects in one sentence — no stale-accel theory survives one of two co-moving objects rendering correctly. Ask what does NOT show the symptom before reading any code.
2. **Match the oracle's stimulus to the user's gesture.** The synthetic object-motion probe built for this bug TRANSLATED its occluder and passed honestly while the defect sat in ROTATION — translation leaves normals untouched, so the oracle was structurally blind. It is now `rt_object_motion_shadow.rs` and still useful (it proved accel refit correct), but it could never have found this.
3. **A green value-level proof of a reasoned mechanism is not evidence that the mechanism was the cause.** BUG-320 and BUG-321 were both real defects, both proven fixed at value level, and neither moved the symptom. Two "fixed" reports were wrong before an in-app observation entered the loop. On this surface, close a motion-quality bug on Peter's look, never on a passing gate.

## 9. RT Reflections — traced specular for the PBR base lobe (Tier 3 item 7; APPROVED 2026-07-24 · **R1 LANDED 2026-07-25** — probe: mirror on 2.15 vs off 0.82 (delta 1.33); empty-scene equality 0.305 ≈ 0.308; worst frame 9.82 ms; native byte-diff identical. R2 LANDED 2026-07-26; R3 not started)

Folded in from `RT_REFLECTIONS_DESIGN.md` (draft deleted on fold, per its own header). Reviewed by
K3 (lead) 2026-07-24: every section 1 code anchor re-verified against main (`render_scene.wgsl:1518`
substitution site, binding 43 free, `GiMaterial` 32 B at `raytrace.rs:1478`, kernel helpers at the
named lines). **Review rulings (Q1–Q5 from the draft's section 0):**

- **Q1 — vertex normals in Base traced reflections (R1), shading-normal prepass is the RD3 escalation, not a planned path.**
  The draft's cheap settling test was RUN before approval: DamagedHelmet (the
  canonical heavily-normal-mapped asset — a harder case than Peter's scans), metallic 1.0 /
  roughness 0.1 / sharp point light, headless render with the normal map wired vs unwired, numeric
  region diff. Result: highlight **shape and position identical**; normal-map contribution is sparse
  sparkle — ~1% of specular-region pixels shift by >20/255, whole-object mean diff 0.7/255.
  Vertex-normal reflections stand; RD3's trigger (Peter's look reports the mismatch at Base traced reflections' (R1) demo)
  remains the escalation. Test caveat recorded for future probe authors: the headless readback
  double-tonemaps (graph ACES + readback Reinhard, `headless_readback.rs:58`), pinning PNGs at
  127 — BUG-327 (headless-readback-double-tonemap).
- **Q2 — the roughness cutoff is a BRDF-domain split, approved.** Above the cutoff the prefiltered
  env IS the correct approximation; named constant, continuous band, visible in code. Not a silent
  fallback.
- **Q3 — `rt_reflections` default ON for RT-enabled scenes (Peter, 2026-07-24).**
- **Q4 — Textured roughness (R3, per-texel metallic-roughness) is IN this design.** D10 pins "plasticy" on roughness
  maps; factor-only reflections would be wrong on exactly Peter's assets.
- **Q5 — reflections before ReSTIR (Peter, 2026-07-24).** Recorded dissent (draft author + this
  doc's section 8 "highest-value" note favor many-light first) stands as dissent; build order is a
  show-need call and Peter made it.
- **Blocking line cleared:** Peter's L2 look PASSED 2026-07-24; the ray-budget re-judge is deferred
  by Peter until the full RT pipeline is built, so section 6 budgets are starting constants to be judged
  after, not gates before.

Pre-allocated BUG range: **BUG-323 – BUG-326** (execution), BUG-327 spent on the readback
double-tonemap found by the settling test.

### 9.1 What exists (audit verified 2026-07-23, re-verified by reviewer 2026-07-24)

| Piece | Where | State |
|---|---|---|
| Split-sum specular IBL, base lobe | `render_scene.wgsl:1506-1518` | The single substitution site. Only `fs_pbr` has it. |
| Anisotropic / clearcoat / sheen / transmission lobes | `render_scene.wgsl:1537/1550/1568/1647` | Out of v1 scope (RD5); anisotropic branch OVERWRITES `specular_ibl` — Base traced reflections (R1) must substitute inside it too. |
| Prefiltered env mip chain + BRDF LUT + irradiance map | `render_scene.rs:645/647`, `run_ibl_convolution` :1985 | Node-owned at the RT dispatch site — one wire away for ray misses. |
| RT trace kernel (shadow+AO+GI, ONE dispatch) | `raytrace.rs:735` `trace_shadow_rays`; trait `ShadowRayTracer` :1815 | D16's seam: new ray classes join this dispatch. Primary ray already casts for the T1-B normal — reflection origin+normal already computed. |
| Hit shading for a secondary ray | `raytrace.rs:940-975` (GI gather) | A reflection ray's hit shading is these lines; RD4 reuses them. |
| Per-object bindless table | `RtNormalSource` (`crates/manifold-gpu/src/metal/raytrace.rs`, 80 B) | Extended three times already (T1-B, T2-A, Textured roughness R3) — the precedent for material fields. |
| Per-object material table | `GiMaterial` (`raytrace.rs:1478`, 32 B) | Built at `render_scene.rs:3976`; `pbr_metallic_roughness` (`render_scene.rs:332`) is in the same uniforms struct, unread. |
| Bindless texture slots | `MAX_RT_MATERIAL_TEXTURES = 64` (`crates/manifold-gpu/src/metal/raytrace.rs`) | Raster-parity reflections widened it into a general material-texture cap; Textured roughness (R3) consumes it via `RtNormalSource::mr_tex_index`, no re-widen. |
| Half-res trace → upsample → à-trous → accumulate chain | `render_scene.rs:4057/4071/4120/4175` | Reflection radiance rides the same chain. |
| Temporal reset | one shared `TemporalResetDetector` (`render_scene.rs:839`) | A second reset path is forbidden (D15/RT-D2). |
| Motion reprojection incl. per-object | `accumulate_irradiance` (`raytrace.rs:1206`) + `obj_motion` (section 8.3) | Reflections add one term (virtual hit point, RD6), not a mechanism. |
| Numeric region-probe harness | `tests/gpu_proofs/rt_p1_region_probe.rs` | The gate precedent every phase copies. |
| Screen-space reflections | — | Do not exist (negative `rg` verified). Nothing to migrate. |

**Binding constraints:** hot path (ray budget is the cost argument); persistence (one serialized
scene param — round-trip gate applies); performance surface (`rt_reflections` is a card param from
Base traced reflections (R1), not later). Thread residency and time model untouched — entirely inside `render_scene`'s
evaluate.

### 9.2 Decisions

- **RD1 — the reflection term SUBSTITUTES for `prefiltered`, never adds to `specular_ibl`.** Traced
  incident radiance along `R` is the same physical quantity `prefiltered` approximates; swap it in
  before the `(F0 * env_brdf.x + env_brdf.y)` weighting, leaving energy conservation and the
  roughness LUT untouched. Rejected: adding on top — literally the `818a06b0` sun double-count bug
  one lobe over. Machine check: I-R1.
- **RD2 — reflection rays join `trace_shadow_rays`; there is no reflection pass.** D16's seam note
  governs. ~15 lines inside the existing thread. Rejected: a separate dispatch (duplicates origin
  reconstruction + accel binding, invites a second upsample and history — three new systems where
  the zero-new-systems test allows zero).
- **RD3 — v1 traces along `reflect(-V, n_vertex)`** — the interpolated vertex normal the kernel
  already fetches — NOT the normal-mapped shading normal. Settled empirically (Q1 ruling above).
  Named trigger for the Stable reflections (R2) escalation (shading-normal prepass target): Peter's look reports the
  reflection sitting on a different surface than the highlight.
- **RD4 — hit returns the GI gather's shading; miss returns prefiltered env at the ray's roughness
  mip.** The miss branch makes RD1 safe: no reflective occluders ⇒ render identical to raster. No
  recursive specular (one bounce, D1); a chrome ball in a mirror reads matte.
- **RD5 — v1 substitutes the BASE lobe of `fs_pbr` only.** Other lobes and non-PBR paths untouched.
  Consequence accepted: clearcoat-heavy assets show a traced base reflection under an env-only coat.
  The anisotropic branch (`:1537`) OVERWRITES `specular_ibl` — Base traced reflections (R1) must substitute inside it too; the
  single easiest thing to get wrong.
- **RD6 — specular gets its OWN history, reprojected through the virtual hit point, in the SAME
  `accumulate_irradiance` kernel.** Trace writes hit distance in `out_refl.a`; accumulate reprojects
  the virtual image `world_pos − hit_dist * V` (D-62, amended 2026-07-26: the tangent-plane mirror
  of the hit point — `world_pos + hit_dist * R` is the REAL hit point and is correct only against
  a scene-color history, which MANIFOLD does not have; with the reflection texture's own history it
  samples the hit surface's own reflection channel and lands off-screen in practice), lerping toward
  plain surface reprojection as roughness rises (`RT_REFL_VIRTUAL_REPROJ_ROUGHNESS_BLEND`, named
  constant with range). Rejected: reusing diffuse
  history (BUG-311's ghost on a new surface); no accumulation (at 1 spp the accumulation IS the
  image quality, BUG-312).
- **RD7 — above `RT_REFLECTION_MAX_ROUGHNESS` (0.6 starting constant, named with range) the pixel
  uses the prefiltered env sample, blended over a band, no ray cast.** Approved as BRDF-domain split
  (Q2).
- **RD8 — 1 reflection ray per pixel at the existing trace resolution** (half-res of render res; ~1/3
  native under T2-B temporal mode). No separate resolution knob; a mirror from a 1/3-res signal is
  the design's most likely disappointment — measured answer from Base traced reflections' (R1) `trace_ms` delta + Peter's
  look, reflection-specific resolution Deferred (section 9.5).
- **RD9 — one new scene param `rt_reflections: Bool`, serialized alongside `rt_enabled`, inert when
  `rt_enabled` false.** Shaped exactly like `rt_enabled`'s path (P1 precedent). Default ON (Q3).
- **RD10 — the Metal RT trait grows no new method.** `dispatch_shadow_rays` gains an `out_refl`
  texture argument; `ShadowRayParams` gains the reflection fields; `upsample_shadow`, `atrous_pass`,
  `accumulate_irradiance` each gain the reflection texture set. Keeps D9's Vulkan seam a
  ray-query-translation matter.

### 9.3 Architecture

Two struct extensions and one texture, following the T1-B/T2-A extend-the-existing-table precedent:

```rust
// crates/manifold-gpu/src/metal/raytrace.rs — GiMaterial grows 32 → 48 bytes.
// Field order and packing MUST match the MSL mirror exactly (P0 section 5.1's packed_float3 lesson).
#[repr(C)]
pub struct GiMaterial {
    pub albedo:    [f32; 3], _pad0: f32,
    pub emissive:  [f32; 3], _pad1: f32,
    /// RT-R1: x = metallic, y = roughness — read straight off
    /// `d.uniforms.pbr_metallic_roughness` (render_scene.rs:332), the SAME
    /// resolved factors `fs_pbr` shades with. z/w reserved.
    pub metallic_roughness: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<GiMaterial>() == 48);
```

```rust
// ShadowRayParams gains, before the mat4 block (keep the offset asserts green):
    pub refl_spp:            u32,   // 1 in v1 (RD8); 0 disables the reflection branch
    pub refl_max_roughness:  f32,   // RT_REFLECTION_MAX_ROUGHNESS (RD7)
    pub refl_rough_band:     f32,   // blend band width
    pub _pad_refl:           u32,
```

New output texture `out_refl` (`Rgba16Float`, trace resolution): `.rgb` = incident radiance along
`R`, `.a` = hit distance (clamped sentinel on miss ⇒ RD6 degenerates to surface reprojection).
Rides every stage the irradiance texture rides — half-res target, upsample, à-trous, accumulate
ping-pong pair — allocated and reset by the same `ensure_rt_irradiance` lifecycle
(`render_scene.rs:1699`), including its RESET-not-resized rule.

**Kernel flow, inside the existing thread of `trace_shadow_rays`** (`raytrace.rs:735`), after the
shadow/AO/GI blocks, reusing their `origin`, `n`, `bias_eps`, `obj_id`:

1. Fetch `metallic`/`roughness` from `gi_materials[obj_id]`. If `roughness > refl_max_roughness +
   band`, write the env sample and return — no ray (RD7).
2. `V = normalize(p.camera_pos - wp)`; `R = reflect(-V, n)`; for `roughness > 0`, perturb `R` by a
   GGX-importance-sampled half-vector using the SAME `blue_noise_sample` sequence
   (`raytrace.rs:694`) — a new sampling function, not a new sampling system.
3. One `walk_with_alpha_test` query (`raytrace.rs:597`) — alpha-aware for free (T2-A).
4. Hit → GI gather's shading lines (RD4). Miss → `prefiltered_specular` sampled with the same
   equirect mapping `render_scene.wgsl:1506-1510` uses, at mip `roughness * PREFILTER_MAX_MIP`.
5. Write `float4(radiance, hit_dist)`.

**The WGSL substitution** (`render_scene.wgsl`, `fs_pbr` only):

```wgsl
// binding 43, full-res, always bound (ABI-stub discipline — a 1x1 dummy when RT
// reflections are off this frame), exactly like rt_irradiance_mask at :352.
@group(0) @binding(43) var rt_reflection: texture_2d<f32>;

// at :1510, replacing the single `prefiltered` fetch:
var prefiltered = textureSampleLevel(prefiltered_specular, envmap_sampler, r_uv,
                                     roughness * PREFILTER_MAX_MIP).rgb;
if u.scene_params.w > 0.5 && u.rt_flags.x > 0.5 {          // rt_enabled && rt_reflections
    prefiltered = textureLoad(rt_reflection, vec2<i32>(in.clip_pos.xy), 0).rgb;
}
```

Everything downstream — `specular_ibl`, the anisotropic overwrite at `:1542` (must consume the
substituted value), `ibl`, `base_rgb` — unchanged. **That two-line diff is the entire raster-side
change; a larger diff to this file means the phase has gone wrong.**

**Stage translation.** A glowing hero object shows up IN the surfaces around it — wet-floor,
dark-mirror, black-acrylic under club lighting — which env-map IBL structurally cannot give (the
env map does not contain your content). Correct under strobes by construction (recomputed per
frame; demodulated-history discipline applies unchanged). Cost: rays, on a budget Peter re-judges
after the pipeline completes. Failure look: reflection noise crawling on shiny surfaces during
fast camera moves — RD6 is the mechanism against it.

### 9.4 Invariants & enforcement

- **I-R1 — exactly one environment-specular contribution per lobe per pixel.** Machine check:
  `rt_r1_reflection.rs::reflection_of_empty_scene_equals_env_only` — empty scene (reflective
  surface, no occluders, uniform envmap): reflections-ON region mean equals reflections-OFF within
  stated epsilon. Fails loudly if the term is added rather than substituted (the `818a06b0` class).
- **I-R2 — one temporal-reset path for the whole RT node.** Negative `rg` for a second
  `TemporalResetDetector` construction in `render_scene.rs` (zero beyond the existing one).
- **I-R3 — the reflection texture is consumed in exactly one place.** `rg -c "rt_reflection"
  shaders/render_scene.wgsl` — declaration plus exactly one `textureLoad`.
- **I-R4 — no reflection work on the non-RT path.** Negative `rg` for reflection dispatch outside
  the `rt_ready` block, plus the native-mode machine-diff gate (T2-B precedent: real
  `graph-tool render` byte comparison at pre/post commits).
- **I-R5 — no Apple types above `manifold-gpu`.** Standing negative `rg` (`objc2|MTL`, zero hits
  outside `manifold-gpu`) at every phase gate.

### 9.5 Alternatives (priced; the favourite kill-passed and settled)

**Shape B — separate reflection pass on a raster-written material G-buffer** (shading normal + MR
after maps): the only shape getting normal-mapped/per-texel reflections for free, but puts material
resolution in two places (translation-layer smell), contradicts D16's one-dispatch seam extended
four times without breaking, and cannot shrink back. Chosen: **Shape A**, and the asymmetry that
decided it — A can grow B's normal handling later at one prepass target (RD3 trigger); B cannot
shrink. **Q1's settling test (run, above) removed the dominant uncertainty**: normal-map breakup on
a real asset is sparkle, not shape.

**Rejected outright:** SSR as a first step (no SSR exists; would create a second reflection path
needing blending — parallel-old-path by name). Reflections as a graph node/atom (section 6.6, decided).

### 9.6 Phases

Four phases, each one session, each committable. **Phases are NAMED (Peter 2026-07-25): the
historical R-numbers survive only attached to names, because kernel comments cite them — never
bare.** **Base traced reflections (R1) is the vertical slice** — model param →
serialized → dispatch → kernel → WGSL → pixels, exercised end to end before anything is refined.
Execution order: Base traced reflections (R1) → Raster-parity reflections → Stable reflections
(R2) → Textured roughness (R3). Raster-parity runs BEFORE Stable: denoising a signal that shades
wrong is wasted work, and the black-car defect is what makes reflections unusable on real assets.

#### Base traced reflections (R1) — traced base-lobe reflection, factors only, no accumulation — LANDED 2026-07-25

- *Entry:* Tier 2 on main (`git merge-base --is-ancestor` on the T2-B tip); re-verify
  `raytrace.rs:1478` (`GiMaterial` still 32 B) and `render_scene.wgsl:1518` (`specular_ibl` still
  assembled there) — a moved anchor is an escalation, not a guess.
- *Read-back:* this doc section 9.1–section 9.3 whole; D16 + section 8.3's three method lessons; `raytrace.rs:735-990`
  (whole trace kernel); `render_scene.wgsl:1494-1620`.
- *Deliverables:* `GiMaterial` +`metallic_roughness`, populated at `render_scene.rs:3976`;
  `ShadowRayParams` reflection fields; `out_refl` through dispatch + upsample + à-trous (accumulate
  carries untouched); kernel reflection block (RD4/RD7); WGSL substitution **including inside the
  anisotropic branch**; `rt_reflections` scene param end-to-end with serialization; I-R1 and I-R3
  checks by name.
- *Gate:* (a) **mirror probe** — new `tests/gpu_proofs/rt_r1_reflection.rs` on the
  `rt_p1_region_probe.rs` computed-pixel harness: metallic/roughness-0 ground plane, one emissive
  quad at known world coords, envmap unwired; CPU computes the emitter's mirror image and projects
  it (`Camera::orbit_perspective` + `project_to_pixel`); 15×15 region mean must exceed a stated
  threshold. **Control leg, mandatory:** identical fixture with `rt_reflections` false must read
  below a stated floor. (b) I-R1's empty-scene equality test. (c) round-trip: save → reload →
  probe still passes. (d) `MANIFOLD_RENDER_TRACE=1`, no frame > 20 ms, **report the measured
  `trace_ms` delta reflections on vs off** — a number in the phase report. (e) negative `rg`:
  I-R2, I-R3, I-R4, I-R5. (f) `cargo test -p manifold-renderer --features gpu-proofs` (GPU path
  touched — `cargo test`, never nextest).
- *Performer gesture:* toggle `rt_reflections` on a playing scene mid-set — no frame > 20 ms
  across the toggle (pipeline already resident; nothing rebuilds).
- *Forbidden moves:* adding to `specular_ibl` instead of substituting (RD1 — the `818a06b0` trap);
  a second dispatch or reflection-specific upsampler; touching clearcoat/sheen/transmission lobes;
  a second `TemporalResetDetector`; widening `MAX_RT_ALPHA_TEXTURES` (Raster-parity reflections'); "temporarily"
  hard-coding roughness; claiming native-mode-unchanged from a code-diff argument instead of a
  machine diff.
- *Demo (Peter only):* reflections-on vs off PNG pair on a mirror-plane scene and on a real hero
  scan — **L2. Peter's look also answers RD3's trigger question**, which is why the hero-scan frame
  is not optional.
- *Test scope:* `-p manifold-renderer -p manifold-gpu` + the gpu-proofs run. Clippy `-p` both.

#### Raster-parity reflections — environment + textured material shading at the hit point

- *Entry:* Base traced reflections (R1) landed; Peter's black-car look report recorded (AMG GT3
  GLB, 2026-07-25, in-app + headless `render-import` PNGs: with `rt_reflections` on, a metallic
  textured hero asset renders black and textureless — RD4's hit shading returns
  `emissive + flat per-object albedo × sun-bounce`, no environment term, no maps, so the
  substitution (RD1) replaces the prefiltered env that WAS the car's look with a near-black
  signal). Diagnosis confirmed in code + renders same day.
- *Read-back:* RD4/RD7; the kernel reflection block (`raytrace.rs:1040-1116`); the fs_pbr
  substitution (`render_scene.wgsl:1513-1541`); `gi_materials` population (`render_scene.rs:3976`);
  T2-A's bindless-texture extension (commit `62244989`, whole — the field pattern this phase
  generalizes).
- *Deliverables:* environment shading at the reflection hit point (the hit surface's own IBL
  contribution — the same physical quantity the raster would add at that surface; I-R1 preserved:
  exactly one environment-specular contribution per lobe per pixel, the hit point's env is the
  VIRTUAL surface's contribution, not a second one at the primary pixel); textured base-color
  albedo at hits via the bindless table (`alpha_tex_index`'s field pattern generalized to material
  textures — `MAX_RT_ALPHA_TEXTURES` widened HERE into a general material-texture cap with a
  stated new value and un-suppression trigger; Textured roughness (R3) CONSUMES this cap for MR
  maps, it does not re-widen); factors stay the fallback when no map is bound; the shading-normal
  prepass target is absorbed HERE if Peter's look fires RD3's mismatch trigger on a real asset,
  otherwise it stays deferred (section 9.7).
- *Gate:* (a) computed-pixel value test on the region-probe harness: textured metallic plane +
  known env — CPU computes the expected reflected radiance INCLUDING the hit-point env term;
  region mean within a stated tolerance; **control leg, mandatory:** the pre-parity shading
  (no env at hit) must read below a stated floor. (b) I-R1's empty-scene equality test still
  passes. (c) held-out input: the AMG GT3 GLB
  (`tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb`) rendered headlessly via
  `render-import`, reflections on vs off PNG pair — **the parity verdict is Peter's look**
  (D19/D20 standing lesson), never an agent's. (d) `cargo test -p manifold-renderer
  --features gpu-proofs` (`cargo test`, never nextest).
- *Performer gesture:* load any textured GLB, toggle `rt_reflections` mid-set — the model keeps
  its paint and textures; reflections add to the model's look, never replace it.
- *Forbidden moves:* a second reflection dispatch (RD2 stands); re-tuning ray budgets (Peter's
  profiling deferral stands); touching clearcoat/sheen/aniso/transmission lobes (RD5 stands);
  claiming parity from a code-diff argument instead of Peter's look on the PNG pair; widening the
  texture cap without stating the new limit's trigger.

#### Stable reflections (R2) — specular temporal accumulation + roughness-aware filtering · **LANDED 2026-07-26** — gate: scene control-leg blend/cut + kernel-level proof, both green; D-61 (gate shape), D-62 (two root causes fixed en route)

**D-63 follow-on (2026-07-29, BUG-dx6w — specular history variance clip):** Peter's D-61 sweep
verdict came back FAILED — camera sweeps leave reflection trails decaying at the blend rate
(~1/RT_REFL_ACCUM_ALPHA frames), because the specular path has no depth test (a virtual image's
depth never matches the surface) and the normal test alone lets stale history through. Root fix,
not a retune: `clamp_refl_history` variance-clips reprojected specular history to mean ±
RT_REFL_CLAMP_GAMMA·stddev of the current frame's 3x3 `hi_refl` neighborhood before the blend —
stale content dies in 1–2 frames; at noisy texels the box widens so amortization survives exactly
where it is needed. Gate consequence: the step-leg pass value moved from ≈1.1 (slow blend — now
the MUST-FAIL signature of a dead clamp) to measured ≈1.67; new kernel-level value proof
`rt_r2_clamp.rs` on the RT-T1-B debug-dispatch pattern. RT_REFL_CLAMP_GAMMA (0.5–3.0) joins the
untuned set — tuning stays Peter's look. v2 same day, BUG-axe9 (tone-mapped variance clip):
Peter's residual verdict (fast-sweep + bright-to-black streaks) traced to linear-HDR moments —
one hot texel inflates sigma; the clamp now maps through `c/(1+luma)`, clamps, inverts.

- *Entry:* Raster-parity reflections landed; Base traced reflections' (R1) `trace_ms` delta and
  Peter's L2 verdict recorded in the phase report.
- *Read-back:* RD6; `accumulate_irradiance` (`raytrace.rs:1206`) and `atrous_filter` (:1107) whole;
  section 8.3 (per-object motion) and D19/D20 (why numeric motion oracles failed).
- *Deliverables:* specular history ping-pong set alongside the irradiance set, wired to the SAME
  reset detector; virtual-hit-point reprojection with the roughness blend (RD6) inside the existing
  accumulate kernel; à-trous edge-stopping weights that narrow with roughness; all constants named
  with ranges, untuned — tuning is Peter's look.
- *Gate:* **control-leg value test** (section 8.3 shape) — camera moves, reflected geometry does not:
  WITH virtual-hit reprojection the accumulated value matches the CPU-computed `1 - alpha` blend;
  WITHOUT it (pre-accumulation behaviour) the history is rejected and the value collapses. Two legs, one file.
  Plus the P2 cut-reset numeric oracle on the specular history; plus I-R2's negative `rg`; plus
  gpu-proofs.
- *Performer gesture:* fast camera sweep across a mirror mid-clip — the gate captures the frame
  sequence; **quality verdict is Peter's look** (D19/D20 standing lesson). A lane proposing a third
  oracle redesign stops and escalates.
- *Escalation (RD3's trigger):* the normal-map mismatch reported ⇒ shading-normal prepass target
  built HERE — a re-brief by the lead, not a lane improvising (changes the prepass's
  render-target shape).
- *Forbidden moves:* reusing diffuse history for specular; a second reset path; a third motion
  oracle; re-tuning ray budgets.

#### Textured roughness (R3) — per-texel metallic-roughness in the kernel

- *Entry:* Stable reflections (R2) landed.
- *Read-back:* T2-A's commit `62244989` (bindless-texture extension precedent, whole);
  `RtNormalSource` + `ensure_normal_sources` (`crates/manifold-gpu/src/metal/raytrace.rs`); D10.
- *Deliverables:* `RtNormalSource` grows an MR-texture index (`mr_tex_index`, same field pattern as
  `alpha_tex_index`/`base_color_tex_index`), riding the general material-texture cap Raster-parity
  reflections already widened (stated limit + trigger live there — this phase does NOT re-widen);
  the reflection lobe's primary-hit block in `trace_shadow_rays` samples metallic/roughness per
  texel at the primary hit's interpolated UV (`fetch_interpolated_uv`, already exists), factors
  (`GiMaterial::metallic_roughness.y`) when no map bound.
  **LANDED — BUG-7y7d (RT R3: textured roughness in the kernel):** the primary-hit primitive
  id/barycentric coord are hoisted to kernel scope in the SAME block that already hoists `obj_id`,
  so no second primary-visibility trace is needed.
- *Gate:* value test — plane with two-region roughness map (0.0/1.0) + one emissive quad: sharp
  region shows the emitter's mirror image above threshold, rough region does not, both against
  CPU-computed expectations (`tests/gpu_proofs/rt_r3_textured_roughness.rs`); held-out input: a real
  imported glTF with an MR map the builder did not develop against
  (`tests/gpu_proofs/rt_r3_heldout_gltf.rs`, `DamagedHelmet.glb`). Plus gpu-proofs.
- *Forbidden moves:* growing the cap without stating the new limit's trigger; sampling MR maps for
  secondary (GI/AO) rays in the same phase.

**Phasing-completeness check:** every section 9 commitment appears exactly once — toggle (Base traced
reflections, R1), traced base lobe (R1), roughness cutoff (R1), raster-parity hit shading
(Raster-parity reflections), accumulation/denoising (Stable reflections, R2), per-texel roughness
(Textured roughness, R3); the four
other lobes, reflection-specific resolution, multi-bounce, shading-normal prepass — section 9.7 with
triggers (the shading-normal prepass is conditionally absorbed by Raster-parity reflections on
RD3's trigger).

### 9.7 Deferred (with revival triggers)

- **Shading-normal prepass target** — trigger: RD3's mismatch in Base traced reflections' (R1)
  demo or Peter's look; when the trigger fires it is absorbed by Raster-parity reflections (section 9.6).
- **Clearcoat / sheen / anisotropic / transmission traced lobes** — trigger: a show asset dominated
  by a coat reflection, plus spare measured ray budget.
- **Reflection-specific trace resolution** — trigger: Base traced reflections' (R1) `trace_ms`
  delta shows headroom, or
  Peter reports mirror reflections reading soft at 1/3-res reconstruction.
- **Multi-bounce / recursive specular** — trigger: none before Tier 3 item 8.
- **Reflections on non-PBR fragment paths** — trigger: a scene needing a reflective cel/phong
  material (those shaders have no Fresnel term to weight against).
- **ReSTIR many-light before or after this** — Peter ruled reflections first (2026-07-24);
  recorded so it is not silently re-decided by build order.

## 10. RT output & transition contract — derivation over side effects (2026-07-26, K3 + Peter)

Provenance: Peter's `RtNoiseTesting.manifold` repro — RT reflections fade to raster a few
seconds after pause; after save+reload RT never engages (toggles display ON, buttons inert);
re-importing the GLB into the same project restores RT. One mechanism, two triggers: the
"raster look" is what the frame shows whenever the RT pass produces no output. These rules
are the agreed contract the fixes are reviewed against.

- **The graph is the source of truth; the RT scene is a derived cache.** RT scene =
  f(graph content, asset payloads), keyed by content/asset version stamps — same family as
  the freeze compiler and the effect-chain state caches. Built by derivation, never as a
  command side effect. Import, project load, duplicate, paste, undo then converge for free.
  Bug class this kills: accel registration fired only by the GLB import path, so a loaded
  project has RT-on params and no RT machinery (the reload bug).
- **Lifecycle derives from content change, never transport.** Build, rebuild, and history
  invalidation trigger on graph structure / params / assets / camera changes only.
  Transport's only role is whether time-varying params evaluate. Pause is a non-event:
  dispatch keeps running, history holds, and a paused static scene CONVERGES. Bug class this
  kills: trace dispatch gated on transport/time-advance → accumulator starves and decays to
  raster (the pause fade).
- **Absent RT output is a bug state, never a rendering mode.** No silent raster fallback —
  fallback is what made the reload bug invisible for a whole session. The ONE sanctioned
  raster-presenting window is D17's bounded, logged accel-build transition; anything else
  that starves the RT pass is a bug to fix, not a state to render gracefully.
- **Never crossfade between lighting models.** Raster↔RT blends over time are banned — the
  in-between frames match neither world and read as broken. Legitimate transitions:
  commanded toggle-off (instant, one frame — the user asked), D17's bounded build window,
  and invalidation (next rule). The only blend inside RT is temporal accumulation
  (history ↔ new sample) — noise averaging within ONE lighting model, not engine-switching.
- **On invalidation: seed, don't clear.** First frame after history invalidation shows raw
  1spp and converges in place; clearing history to zero (the pause→resume black beat) is
  banned. Converging noise is honest and stage-forgivable; a morph through a world that
  never existed is not.
- **Speckle under motion is not a contract violation.** At 24 FPS with history rejected
  under motion the image sits near raw 1spp — the accumulator honestly reporting its limits.
  Improving it is Textured roughness (R3)-era tuning (reprojection quality, motion-aware
  blend weights, sample budget), not a transition fix.

**Conviction test (Peter's repro project):** reload → RT engages without re-import;
pause → converges clean, no fade; resume → no black beat; rotating → unchanged (speckle
expected, tuning scope). **Bug A (reload-inert RT) LANDED 2026-07-26:**
root was NOT bindings/params (three probe harnesses + two live logged sessions exonerated
the whole chain) — the accel topo key hashed vertex-buffer identity, never content;
async glb parse lands after the initial build, the BUG-326 one-shot rerun lost the race
at release timing, and the blind BVH traced forever (all rays miss → env look ≈ raster,
toggles "dead"). Fix: two-key split — identity topo key (first build + topology changes,
one-frame recur defer) + content-settle key (topo + per-draw `vertices_generation`;
rebuilds only once generation settles, so never-settling producers keep D17 behavior);
`rt_accel_rerun_armed` heuristic DELETED as redundant; `accel_key` generation-free so
refits stay transform-only. gpu-proofs 1847/1847; Peter's release-build conviction test
PASSED (RT engages after load, toggles live). OPEN from the same repro: converged-static
RT reads identical to RT-off (traced shell reflections wash out between raw trace and
accumulated output — R2 filter path, R3 scope); motion speckle/lag at 24 FPS (R3 tuning).
