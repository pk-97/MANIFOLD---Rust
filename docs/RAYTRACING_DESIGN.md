# Ray Tracing — hybrid RT lighting for hero scenes

**Status:** IN PROGRESS — sections through 16 landed; stage-3 spatial denoise + firefly clamp landed (RT_STAGE3_DENOISE_DESIGN.md; closes the BUG-312 (RT ray noise speckle) lineage) but measured inert under motion (BUG-27bs (RT spatial denoise inert under continuous motion)) → stage-4 motion denoise APPROVED not built (RT_STAGE4_MOTION_DENOISE_DESIGN.md); MetalFX classic is the operating point (Metal 4 hard-off, BUG-woji (MTL4FX denoiser crash)). OWED: Peter's looks (17.7, DN-I, TL-C, ED-A hero), multi-bounce, R2 constants, fast-camera denoiser, noise-gate re-baseline, P5 export, P6 interp. · K3 + Peter
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
   **LINEAGE CLOSED 2026-08-28:** T1-D landed the pre-accumulation half;
   RT_STAGE3_DENOISE_DESIGN.md (BUG-eytk (spatial à-trous denoiser)) landed the
   post-accumulation half.

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
  **The trigger FIRED as BUG-wytp (rt-reflections-are-normal-map-blind) and shipped 2026-07-31
  as IN-KERNEL normal-map sampling instead** (analytic per-triangle TBN at the primary hit,
  `normal_tex_index` on `RtNormalSource`): the prepass is a depth-only pipeline with no
  fragment stage, so the prepass target was the bigger build. The prepass target stays deferred —
  revival: secondary-hit normal fidelity or AO bias artifacts demand it.
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

**Addendum — the gesture rule: soft-and-current under fast lighting moves
(2026-08-07, K3 + Peter).** Peter's symptom on main: shadows and RT lighting
visibly trail fast gestures (whipping the sun, scrubbing light/env brightness);
static convergence is excellent and stays exactly as clean. Approved direction:
during a gesture the RT channels answer within ~2 frames and read spatially
soft. Speckle-during-motion is vetoed; frame generation was considered and
rejected (it interpolates already-lagged frames and adds latency). Zero new
rays, zero new textures, no frame-time regression — detection-and-response
only, on the accumulation path.

- **The CPU lighting key covers every lighting input, not the convenient
  subset.** Rule: any param whose change alters the traced textures (or the RT
  shading substitutions) and has no other reset path belongs in
  `lighting_key`. The audit this addendum shipped with is in the landing
  commit; the gap that motivated it was an env-brightness scrub that moves no
  hashed input.
- **A gesture is two consecutive frames of lighting-key change**, detected on
  the CPU from the key stream, with a one-frame hangover so a throttled
  mid-scrub update (a rebake that lands every Nth frame) doesn't break it.
  Carried to the accumulator in spare `reset`-word bits — no new fields, no
  new textures, no new detectors (the I-TL5 (one temporal-reset path)
  discipline stands).
- **Geometry cues move shadows; strobes don't.** The key splits in two. The
  full key (every lighting input) drives the irradiance/reflection response. A
  geometry sub-key (caster position/direction/cone/kind + the designated
  sun-tint slot) drives the shadow-tint response. A pure
  intensity/color/env-brightness change flips only the full key, and the
  sv/sv2/svt channels hold their convergence through it BY DESIGN: visibility
  and transmission tint are geometry terms, so snapping them on a strobe just
  re-noises an unchanged signal. This sharpens the sv gate's standing
  "deliberately NOT OR'd with `lighting_changed`" discipline, and TL8 is
  amended accordingly: svt no longer blindly rides the irradiance alpha — it
  snaps on geometry cues, and on the per-texel gate when the CPU key is silent
  (occluder motion, the CPU's blind spot), and HOLDS when the CPU reports a
  non-geometry change. One reset path still; only the per-channel blend
  decision sharpened.
- **While a geometry gesture is active, the shadow channels take the cue
  directly** — sv/sv2 snap to n=2 for the gesture's duration, folded into the
  existing trip condition so the BUG-tr5o (sv straddling-moments snap-hold)
  machinery and the BUG-boil (moments self-sustain on snap) floored-moments
  update run unchanged. Between gestures nothing changes: a single nudge still
  goes through the sigma gate plus snap-hold, and a static scene converges
  byte-identically.
- **The irradiance channel holds n=2 for the gesture's duration** instead of
  re-converging between the frames of a continuous scrub. The frame the
  gesture ends, normal convergence resumes — no crossfade; seed-don't-clear
  untouched.
- **Softness is the à-trous's job at 2-frame history.** If a gesture reads
  speckly rather than soft, the à-trous luma sigma widens while the gesture
  flag is up (a gesture-scaled sigma, the `cam_motion` band precedent) —
  contingent on the look, gated by the sub-threshold scrub oracle and Peter's
  eyes.

## 11. Multi-bounce GI — path-extended gather (Tier 3 item 8; APPROVED 2026-07-30)

Graduates `RT_TIER3_SCOPING.md` section 3 (T3-8 — multi-bounce GI); that section is this
design's intake and its findings stand except where the audit below corrects them. Build
order is Peter's priority call 2026-07-29 (status header). Execution vehicle: lane
sessions under lead review — the workflow-program vehicle that ran the P3 shakedown
(docs/archive/WORKFLOW_RUNTIME_DESIGN.md section 5 (Phasing)) was retired 2026-07-31.

**Stage translation.** Colour bleeding between surfaces: a red wall tints the white floor
next to it; a glowing hero object fills a concave shell with its own colour instead of
stopping at the first surface. Honest expectation (scoping section 3's warning, kept on
purpose): at 2 GI spp the second bounce is dim and low-frequency — the fixture proves it
above threshold; on a hero scene it may read as "slightly richer", not as a feature.

### 11.1 Audit — what exists (verified 2026-07-30)

| Piece | Where | State |
|---|---|---|
| One-bounce GI gather | `raytrace.rs:1150-1216` — `gi_spp` loop, cosine hemisphere, hit → emissive + sun-bounce, miss → nothing | The block MB-B extends |
| Sun-bounce caster loop, copy 1 | `raytrace.rs:1183-1211` (GI gather) | Duplicated — MB-A extracts |
| Sun-bounce caster loop, copy 2 | `raytrace.rs:1319-1335` (reflection hit shading, `sun_bounce_term`) | Same shape, different seed offsets |
| Env/ambient term | `ambient_color * ao` (`raytrace.rs:1366`), fed by the scene Ambient knob (`render_scene.rs:4142-4158`) | Env deliberately excluded from the gather (comment `raytrace.rs:1143-1146`) — MB3 keeps that at every depth |
| Value-test precedent | `tests/gpu_proofs/rt_p3_emissive_gi.rs` (PresetRuntime harness, region probes, `RT_WARMUP_FRAMES`) | MB-B's test copies this harness |
| Determinism/byte-diff precedent | T2-B native-mode gate (section 8.2): `graph_tool render` at pre/post commits, `cmp`-identical | MB-A's identity gate reuses it |

**Audit correction to the scoping pass:** Finding 2's "one shared `gather_radiance(ray,
depth)` the term blocks call" no longer maps onto the shipped kernel. Reflections landed
AFTER that audit with intentionally different hit shading (raster-parity: textured albedo,
hit-point env, F0 specular — section 9.6 Raster-parity reflections) while the GI gather is
demodulated and env-excluded by design. The shareable unit is the sun-bounce caster loop
only; a full shared gather would force one of the two shadings to lie. The refactor is
scoped accordingly (MB2).

### 11.2 Decisions

- **MB1 — path extension inside the existing GI block; probe/irradiance cache rejected.**
  Scoping section 3's read, confirmed: probes need invalidation on every gesture, and
  MANIFOLD's hero objects are animated — the BUG-322 (rotating-object history discard)
  class of problem, bought voluntarily. Rejected: any spatial cache structure.
- **MB2 — refactor precondition, scoped by the audit.** Extract ONE sun-bounce helper
  (hit position/normal/albedo + seed in, summed caster term out) used by both the GI
  gather and the reflection hit block; restructure the gather into a bounce loop carrying
  a `throughput` colour. `RT_GI_MAX_BOUNCES = 1` in the refactor phase — output
  byte-identical, machine-diffed (I-MB1). No MSL recursion: a loop with throughput is the
  only shape (GPU function recursion is banned by the target anyway).
- **MB3 — env-miss contributes nothing at ANY depth (Peter 2026-07-30: Ambient knob
  untouched).** This resolves scoping section 3's blocking question structurally: the
  gather never adds env at any bounce, so multi-bounce adds only emissive and sun paths —
  nothing the flat ambient term is faking. No double count by construction; the knob
  stays a pure performer control (knob 0 = true black holds). Machine check: I-MB2.
  **SUPERSEDED by section 14 (Traced environment diffuse), 2026-07-31:** the furnace
  oracle showed knob 0 was never black with a sky wired, and substitution (ED2) gives
  the no-double-count property without excluding env.
- **MB4 — `RT_GI_MAX_BOUNCES` is a named constant, default 2, range 1–3; NOT a scene
  param (Peter 2026-07-30).** It joins the committed-budget constants awaiting Peter's
  deferred re-judge. No Russian roulette at fixed depth 2. The per-extension energy fold
  is `RT_GI_THROUGHPUT_FOLD` (default 0.318 ≈ 1/π, range 0.1–0.5), a named constant like
  `SUN_BOUNCE_INTENSITY_SCALE`; the sun-bounce term at every depth keeps
  `SUN_BOUNCE_INTENSITY_SCALE`, multiplied by the path throughput.
  (Both constants re-derived and re-valued by section 14 (Traced environment diffuse)
  ED4 — the fold is deleted, the sun scale becomes 1/π.)
- **MB5 — demodulation stays first-vertex-only.** The gather returns radiance incident at
  the primary hit (no primary-surface albedo multiply — D3 discipline unchanged);
  throughput from bounce 2 onward carries the INTERMEDIATE surfaces' albedo — that
  carried albedo IS the colour bleed.
- **MB6 — reflection hit shading stays depth-1.** Section 9.7's recursive-specular
  deferral stands; the reflection block's only change is calling MB2's helper.

**Consequences, stated honestly:** worst-case GI ray cost doubles (one extension ray plus
one sun ray per caster per sample); the `trace_ms` delta is a reported number in MB-B's
phase report, and the budget re-judge stays deferred per Peter's standing call. And the
effect may be hard to point at on stage at these budgets — the value test sees what the
eye may not.

### 11.3 Invariants & enforcement

- **I-MB1 — the refactor changes no pixel.** `graph_tool render` of an RT-enabled compare
  graph at the pre- and post-MB-A commits, `cmp`-identical (T2-B precedent — never a
  code-diff argument).
- **I-MB2 — env is never gathered, at any depth.** A scene with geometry, ambient only —
  zero suns, zero emissive — renders `cmp`-identical at bounces=2 vs bounces=1 (the
  cross-commit pair in MB-B's gate), and the in-repo pin holds its region at the analytic
  `ambient*ao` value. **RETIRED by section 14 (Traced environment diffuse)** — env IS
  gathered from ED-A on; the ambient-only pin survives as I-ED1's value gate.
- **I-MB3 — the sun-bounce caster loop has one home.** `rg` — the
  `SUN_BOUNCE_INTENSITY_SCALE` multiply appears inside exactly one function.

### 11.4 Phases (one workflow-program step each, committable)

#### MB-A — bounce-loop refactor, behavior-identical

- *Entry:* main; re-verify `raytrace.rs:1150-1216` (gather block) and `:1319-1335`
  (reflection sun loop) — a moved anchor is an escalation, not a guess.
- *Deliverables:* the sun-bounce helper (both call sites converted, seeds passed in so
  existing sequences are preserved exactly); the gather as a bounce loop with throughput,
  `RT_GI_MAX_BOUNCES = 1`; `RT_GI_THROUGHPUT_FOLD` declared (unused at 1 bounce is fine —
  it ships with its consumer in MB-B if clippy objects).
- *Gate:* (a) clippy `-p manifold-gpu -p manifold-renderer`; (b) the full `rt_` gpu-proofs
  subset (`cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs rt_
  --no-fail-fast` — `cargo test`, never nextest); (c) I-MB1's byte diff; (d) I-MB3's `rg`.
- *Forbidden moves:* changing any sampled sequence (seed offsets are load-bearing for
  I-MB1); touching the reflection block's shading beyond the helper call; touching
  ambient plumbing; MSL recursion.

#### MB-B — second bounce

- *Entry:* MB-A committed, gates green.
- *Deliverables:* `RT_GI_MAX_BOUNCES = 2`; extension path — at a hit, after shading,
  `throughput *= hit_albedo * RT_GI_THROUGHPUT_FOLD`, new cosine-hemisphere ray off the
  hit's interpolated normal, shade again (emissive + sun-bounce × throughput), stop at
  the depth cap or miss; new value test `tests/gpu_proofs/rt_t38_multibounce.rs` on the
  `rt_p3_emissive_gi.rs` harness.
- *Gate:* (a) clippy as MB-A; (b) the bounce discrimination, ruled at compile time
  (2026-07-30, Fable): `RT_GI_MAX_BOUNCES` is an MSL constant with no runtime knob — a
  ShadowRayParams field only for testability was priced and rejected (the R1 slot-map
  incident class, bought for nothing a commit-level A/B already gives). The 1-vs-2 legs
  run ACROSS the two program commits: MB-A's gate renders the bleed fixture
  (`tools/rt_prototype/compare/RtBleed.json` — emitter the floor region cannot see,
  coloured wall both can) and the ambient-only fixture into the run dir; MB-B's gate
  re-renders both and `scripts/rt_region_probe.py` asserts — **bleed leg:** floor region
  mean at MB-B exceeds the MB-A capture by a stated threshold in the wall's colour
  channel; **control leg:** the MB-A capture reads below a stated floor (no leakage at 1
  bounce); **I-MB2 leg:** the ambient-only pair is `cmp`-identical (env never gathered at
  depth 2). The in-repo value test `tests/gpu_proofs/rt_t38_multibounce.rs` (PresetRuntime
  harness, `rt_p3_emissive_gi.rs` precedent) then PINS the shipped bounces=2 behaviour as
  a regression floor: bleed region above threshold, ambient-only region at its analytic
  `ambient*ao` value within epsilon — the causal 1-vs-2 proof is the program's, the pin
  is the repo's;
  (c) the full `rt_` subset; (d) `trace_ms` delta bounces 2-vs-1, a number in the phase
  report.
- *Demo (Peter only):* PNG pair bounces 1 vs 2 on an emissive hero scene — **L2; the
  stage verdict is Peter's look**, per the D19/D20 standing lesson.
- *Forbidden moves:* a probe cache; a scene param for bounces; Russian roulette;
  re-tuning `gi_spp`/`ao_spp`/any existing constant; sampling MR or base-colour textures
  for the bounce hit (flat `GiMaterial` albedo, same as bounce 1); touching the Ambient
  knob path.

**Phasing-completeness check:** every section 11.2 commitment lands in a phase — MB2
(MB-A), MB1/MB3/MB4/MB5 (MB-B), MB6 (both phases' forbidden lists). Deferred with
triggers: bounce count > 2 (trigger: Peter's look wants more after the budget re-judge);
recursive specular (section 9.7, unchanged).

#### Phase records — multi-bounce (LANDED 2026-07-30, merge `ca4206c1`)

- **MB-A:** landed. One-shot execute could not produce the refactor (six parked attempts
  across two runs — the emitted helper broke MSL's declare-before-use ordering, invisible
  to a blind diff); a lane fixed forward on the committed attempt. I-MB1 byte identity and
  I-MB3 single-home held through the program's gate. Boundary lesson recorded as
  docs/archive/WORKFLOW_RUNTIME_DESIGN.md section 5 (Phasing — P3 outcome).
- **MB-B:** landed. The constant flip succeeded one-shot, first attempt. The bleed probe
  then parked on byte-identical captures — root cause one layer down: the RT pass was
  gated on `!casters.is_empty()` (predates GI), so zero-light emissive scenes had NO
  raytraced GI at all. Gate lifted + lightless-GI gpu-proof landed same day (finding of
  this phase, not of the scoping audit). Honest evidence on the fixed engine, cross-commit
  A/B: bleed delta 0.019 over the 0.008 threshold, control leg 0.0, ambient pair
  `cmp`-identical (I-MB2); pin test `rt_t38_multibounce.rs` discriminates 1-vs-2
  (verified failing at bounces=1). OWED: `trace_ms` 2-vs-1 was below GPU scheduling noise
  at fixture scale — measure on a heavier scene when Peter re-judges budgets. Demo pair:
  the run's bleed captures (1 bounce: glow pools under the emitter; 2: the room fills and
  the wall's red crosses the floor).

## 12. Screen-space AO handoff — `ao_mask` (BUG-tgbd + BUG-ay0e; APPROVED 2026-07-30)

Intake: BUG-tgbd (import SSAO group RT-oblivious — double AO with RT on) and BUG-ay0e
(post-process AO darkens baked-look surfaces). One root cause: the importer's AO group
(`gltf_import/scene.rs`, the "Ambient Occlusion" group) multiplies screen-space AO onto
`render_scene`'s final color with no signal about RT state or material kind. The scene
pass is the only stage that knows both, so it publishes the signal; the group consumes it.

**Stage translation.** RT scenes stop double-darkening corners (GTAO no longer stomps the
traced AO and multi-bounce bleed); a baked-look photoscan stays flat next to lit
neighbours instead of growing a fake contact gradient.

### 12.1 Audit — what exists (verified 2026-07-30)

| Piece | Where | State |
|---|---|---|
| Lazy G-buffer outputs, MSAA target + resolve pattern | `render_scene.rs:315-340` (`depth` R32Float, `velocity` Rg16Float — rendered only when wired, GBUFFER_DESIGN.md section 2 (Decisions) D1) | `ao_mask` is a third instance of the same mechanism; no spare channel exists (depth 1-ch, velocity 2-ch, color alpha is scene alpha) |
| The RT gate expression | `render_scene.rs:3588` — `rt_enabled && rt_ready` (latched built flag) | Reused verbatim; bool, not a ramp (Peter 2026-07-30: nobody flips RT mid-show) |
| Material kind at draw time | `baked_look` in `pbr_material.rs` — BUG-pt6g (unlit imports default to lit) made it the opt-back-in flag → emitted `Material` unlit kind | The per-pixel zero source |
| Importer AO group | `gltf_import/scene.rs:628-683` — ssao_gtao → bilateral H/V → `node.mix` Multiply | The consumer to rewire |
| Per-pixel gated blend | `node.masked_mix` — a/b weighted by mask red channel; `mask` is a required input | The atom that applies the mask; no new node needed |
| Loader migration choke point | `graph_loader.rs` (`migrate_def_type_ids`, `migrate_gltf_anim_v2`) | Third migration goes here; pattern is established and idempotent |

### 12.2 Decisions

- **AM1 — `ao_mask` is a fourth lazy `render_scene` output.** R8Unorm, canvas-sized,
  MSAA target + resolve on the `velocity` pattern, rendered only when wired. Meaning:
  per-pixel weight of screen-space AO still owed. Lit raster pixel → 1; unlit-kind
  material (`baked_look`) → 0; clear value 1 (background AO is already identity there —
  old behaviour preserved). Rejected: riding an existing channel (none is spare), a new
  boolean bypass param on the group (fixes the double-AO half only, leaves the
  baked-look half manual).
- **AM2 — RT zeroes the whole mask.** When `rt_enabled && rt_ready` (the line-3588
  expression), the mask is written 0 everywhere. Bool gate; no float ramp — RT toggling
  is a load-time act, not a performance gesture (Peter 2026-07-30).
- **AM3 — the name stays `ao_mask`.** Concrete over speculative: a shared
  "screen-space-terms-owed" signal waits for a second consumer to exist; a port rename
  later is one more loader migration on an established pattern.
- **AM4 — the group applies the mask with `node.masked_mix`, downstream of the existing
  multiply.** New required group input `ao_mask`; `masked_mix(a = color, b = ssao_mix
  out, mask)` becomes the group output. The existing ssao_gtao/bilateral/mix chain is
  untouched. Unwired means impossible in migrated/new graphs (the input is wired at
  assembly and at migration); a group the migration does not recognize keeps `node.mix`
  and therefore exact old behaviour — nothing on disk changes silently.
- **AM5 — migration on load, structure-gated.** At the `graph_loader.rs` choke point:
  match the importer's exact AO group shape (node type-ids + wire set); on match, add the
  `ao_mask` interface input, insert `masked_mix`, rewire the group output, and wire
  `render_scene.ao_mask` → group at the outer level. No match (hand-edited group) → leave
  it alone. Idempotent: a migrated group no longer matches the pre-migration shape.
- **AM6 — the zero list is `baked_look` only.** Emissive-lit surfaces keep AO (they are
  lit by default since the BUG-pt6g ruling above and want contact shading like anything
  else). The list lives in one place — the mask-write site in the scene shader — with
  this section cited.

### 12.3 Invariants & enforcement

- **I-AM1 — migration is idempotent and structure-gated.** Loader test: migrate twice ==
  once; a deliberately perturbed group is untouched. (No real-project fixture leg: the
  Liveschool fixture was retired 2026-07-30 — pre-3D, so it cannot exercise this
  migration. PROJECT_IO_MAP.md section 9 (Honest edges) E9 carries the resulting gap.)
- **I-AM2 — RT on+ready makes the AO group an identity on color.** Gpu proof: RT scene,
  region probe of group input vs output — equal within epsilon.
- **I-AM3 — baked-look pixels pass the AO group unchanged while lit neighbours darken.**
  Value proof on a two-quad fixture (one `baked_look`, one lit, shared corner).
- **I-AM4 — RT off, all-lit scene matches old output within epsilon.** Region probe, not
  `cmp`: `masked_mix` at mask==1 is a lerp, not a select, so byte identity is not owed —
  state the epsilon in the test.

### 12.4 Phases

#### AM-A — `render_scene` grows the output

- *Deliverables:* `ao_mask` in `RENDER_SCENE_OUTPUTS`; R8Unorm MSAA target + resolve
  (velocity pattern); shader writes 1 / 0-for-unlit-kind / 0-everywhere under the AM2
  gate; clear 1. Gpu proof for the three mask states (lit=1, baked_look=0, RT⇒all 0).
- *Gate:* clippy `-p manifold-renderer`; `scripts/gpu_proofs_gate.py`.
- *Forbidden:* touching the AO group or loader; any change to color/depth/velocity
  output behaviour when `ao_mask` is unwired (lazy rule — unwired must stay byte-inert).

#### AM-B — consumer + migration

- *Deliverables:* importer assembles the AM4 group shape and outer wire; AM5 loader
  migration; I-AM1..I-AM4 tests; `graph-tool validate` + `graph-tool fusion` pre-flight
  on the assembled import graph.
- *Gate:* clippy; scoped nextest (`manifold-renderer` loader/import filters); gpu proofs
  (I-AM2/I-AM3).

#### Phase records — AO handoff (LANDED 2026-07-30)

- **AM-A:** landed. `ao_mask` ships as a fourth lazy output. Two defects surfaced only
  under `--tests --features gpu-proofs`, which the first commit had not run: a broken
  `pipeline_for` call site and a stale generated node catalog. Fixed at the class — the
  prewarm test now asserts every `(kind, velocity, ao_mask, blend)` variant is warm, so a
  future aux output added without a prewarm entry fails there rather than costing a
  first-draw compile stall on stage. Lesson: `cargo check` + `clippy` do not compile
  feature-gated test modules; the gpu-proofs gate is the real oracle for primitive work.
- **AM-B:** landed. Values, not green checks — signal level: lit 1.0000, baked-look
  0.0000, background 1.0000, RT-live 0.0000, unwired-vs-wired colour delta 0.000000.
  Group level (I-AM2/I-AM3): under RT the group is bit-exact identity on colour
  (0.000000); RT off, a baked-look surface passes at 0.000000 while its lit neighbour on
  the same contact corner darkens 6.92% — both legs required, either alone passes
  trivially if AO is dead. Fusion neutral: 27 estimated dispatches before and after
  (`masked_mix` joins the existing region); `ssao_gtao`/`bilat_h` stay unfused
  (BUG-141 (import AO/DoF chain fails to fuse), untouched).
- **Owed:** I-AM4's cross-commit epsilon A/B (pre-change main vs post, all-lit RT-off)
  was not run as a separate gate — the unwired-vs-wired 0.000000 delta and the full
  gpu-proofs suite cover the same ground in-repo. Peter's look is the stage oracle.

## 13. Temporal denoiser rebuild — measured, not tuned by eye (2026-07-30, Opus + Peter)

Peter's report: RT lighting boiled constantly and read low-res, on a completely static
scene. Four wrong answers were eliminated by measurement before the real one appeared, so the
method matters as much as the result.

**Instrument.** `manifold rt-capture --paused` already existed; its paused phase now captures
a run of CONSECUTIVE frames, because "what differs between frame N and N+1 when nothing
moves" is the whole question and sparse captures cannot answer it. Metric: per-pixel
|delta| between consecutive frames, per channel, mean and 99.9th percentile in 8-bit levels.

**Eliminated, with numbers.** Shadow mask (0.03 levels — stable; Peter's independent
cure test with the sun cone at 0 agreed). Reset detector (0/120 resets, two runs). "The
accumulator is broken" — it damped its input 3.5x, which is what its window buys. Attribution
by per-pixel correlation against the composite named the culprit: reflections +0.66, diffuse
+0.12, shadows +0.05.

**Root cause.** Both accumulators used a FIXED blend weight, which has a permanent noise
floor: output variance settles at `alpha/(2-alpha)` of the raw single-frame variance and stays
there. No amount of standing still removes it. Underneath that, the input was one GGX
reflection sample per pixel per frame at half res, heavy-tailed.

**Built.** Per-texel running mean at `1/n` with the old alpha as a floor (counts ride
`history.a` for irradiance and `normal_history.w` for specular — spare channels, no new
textures). `REFL_SAMPLES_PER_PIXEL` 1 -> 8 with a median-anchored firefly clamp.
Variance-guided widening of the a-trous luma stop, so the filter stops standing down exactly
where the noise is.

| composite frame-to-frame change | mean | p99.9 | gen ms |
|---|---|---|---|
| before | 0.806 | 69 | 25.2 |
| after | 0.070 | 6 | 25.9 |

**Cue responsiveness — the cost, and its fix.** Long windows lag a real lighting change, which
Peter hit immediately on stage ("can't do hard lighting transitions any more"). Two threshold
attempts both failed for the same reason: any fixed fraction is wrong for some scene's mix of
terms. An absolute floor of 0.01 sat three orders above a real scene's demodulated irradiance
and disabled the gate entirely; a 15% relative gate caught env changes (the dominant term) but
not sun changes (a small slice of the same buffer). **The engine knows when a light param
changed and now says so** — a hash of caster direction/colour/cone/kind plus scene ambient,
compared frame to frame, riding spare bits of `AccumulateParams::reset`. Per-texel gates stay
for what the CPU cannot see (an emissive object animated inside the graph). This is section 10
(RT output & transition contract)'s "lifecycle derives from content change" applied to
history.

Accepted, named: a continuously automated light sweep runs at short-history quality while it
moves and converges when it settles. Section 10's own stance.

**Constants** (all in `render_scene.rs` / `raytrace.rs`, ranges in their doc comments):
`IRRADIANCE_ACCUM_ALPHA` 0.02 floor, `RT_REFL_ACCUM_ALPHA_MIN` 0.025,
`REFL_SAMPLES_PER_PIXEL` 8, `RT_REFL_FIREFLY_GAIN` 8, `ATROUS_REFL_VARIANCE_GAIN` 2,
change gates at 4 sigma / 15% / 1e-4.

**The gate that keeps it.** `scripts/rt_noise_gate.py` — same instrument, same metric, as an
exit code. Median of clean runs per channel against ceilings in
`scripts/rt_noise_baseline.json`; re-baseline with `--record` and commit the JSON. A channel
that goes silent FAILS as inert rather than passing, because a dead channel is perfectly
stable — the one failure mode a delta-only metric cannot see. An async accel rebuild in or
within 60 frames before the measured window discards the run instead of failing it. Nightly on
main via `trunk_health.py`; not a landing item, because it costs an app build plus three
300-frame renders.

Two measurement traps this cost us, both worth knowing before trusting any capture number.
**Build profile:** a debug build is several times slower, and the RT path has an async accel
build racing the first trace dispatch (D17) — captures that looked like "RT is intermittently
dead, 7 runs in 10" came from a debug binary, while release measured 5/5 alive with 1-4%
run-to-run spread. The gate builds release for that reason, not just for speed.
**Shared output path:** the capture harness wrote to a fixed `/tmp/rt_capture` and cleared it
on entry, so two concurrent captures silently destroyed each other's frames — which is most of
what the "all-zero channels" report actually was (BUG-mw0x, reframed). The directory is now
overridable via `MANIFOLD_RT_CAPTURE_DIR` and the gate always uses a private one. Measure one
at a time, in release, or measure nothing.

**Owed:** Peter's look under fast camera motion — longer specular history reopens D-61's
sweep-trail risk in principle, with the variance clamp and per-texel count reset as the
guards. 40 frames is the first constant to pull back if it trails. Everything measured here is
one project, one camera, one material class.

**D-64 (2026-08-01, camera motion — gates and specular history learn to move, k3 lead):**
Peter's look arrived and it was worse than the trail risk: rotating the camera boiled the
whole image. Measured on the DamagedHelmet orbit fixture (rt-capture `--animate`):
the accumulated reflection was NOISIER than the raw trace (hf 3.9 vs 2.7), and the
per-texel history length oscillated 16→1.3 instead of converging. Two root causes, both
in `accumulate_irradiance`. (1) The change gates compare cur against hist and cannot
tell a real lighting change from motion-induced texel change (disocclusion churn, GI
gradient resampling, view-dependent reflection shift), so they snapped every frame and
the snap→rebuild→retrip cycle boiled the image. (2) The specular reprojection validated
taps by normal only — a parallel-but-different surface's stale reflection blended in
under rotation. Fix: a per-frame `cam_motion` scalar (view-direction turn + weighted
translation, computed in `render_scene.rs`, 0 on a held camera so the static path is
byte-identical) widens every data-driven gate band (`1 + 60·cam_motion`) — as
shipped this reached only the refl gate; the addendum below corrects the record
and motion-scales the sv gate); the CPU
lighting key is NOT scaled, so cues still land mid-gesture. Specular taps whose
reprojection is a plain surface reprojection (roughness ≥ RD6's blend, bt→1) now take
the same depth test the diffuse channel passes; and specular history carries a
motion-scaled alpha floor (`min(cam_motion·5, 0.9)`) because specular content is
view-dependent — no reprojection fixes that, so motion leans on the current frame and
stillness re-converges. Measured after: center history length converges 68+ at
0.02 rad/frame (was ~10 with speckle), composite clean at 0.08 rad/frame (was salt
everywhere). The static noise gate is untouched (cam_motion = 0 there).
Residual: glossy virtual-point reprojection (bt<1) still has no parallax validation —
bounded by `clamp_refl_history`; a prev-view-direction virtual point is the upgrade if
it shows. Same session's MetalFX audit (temporal_upscale): the motion vectors carried
the camera jitter baked into BOTH clip positions, one previous-frame jitter of phantom
motion every frame — MetalFX expects jitter-free vectors (its jitterOffset compensates
the current frame itself). `velocity_jitter` (cur/prev NDC offsets, all-zero with
upscale off) now subtracts the delta in the velocity fragment; the extreme-drag
screen-door grid largely resolves, static and slow-drag captures clean. Residual:
wholesale MetalFX rejection at ~300 px/frame still falls back to raw half-res —
TAA-intrinsic; a soft-upsample fallback is Apple's side of the fence.

**D-64 addendum (2026-08-01, sv gate learns to move too, k3 lead):** recorded
under BUG-tr5o (RT motion leg for rt_noise_gate). D-64's "widens every
data-driven gate band" overclaimed — `motion_band` reached only the refl gate;
the irr and sv gates shipped unscaled. The sv hole was live: under a 0.02
rad/frame orbit the sv change gate re-fired every frame on converged
penumbra-edge texels (subpixel true edge shift > the 0.05 data floor while
sigma was still small from the last snap), pinning the mask at alpha 0.5 for
the whole move. Measured via a new `sv_hold` rt-capture channel (the snap-hold
counter texture, the direct observable): orbit mean hold 0.006–0.014, never
decaying, vs static 0.000006–0.00001 (1000x). Whole-texture sd and per-texel
time-series both failed as oracles first (scene-gradient domination; 75+
px/frame edge sweep drowns noise separation at any capture spacing). Fix:
`motion_band` now scales the WHOLE sv band including the floor — unlike the
refl gate's 1e-4 numerical floor, sv's 0.05 is a data floor and it is what
binds. Static path byte-identical (band = 1 at cam_motion = 0). After: orbit
hold 0.0005–0.003 (5-10x cut; the residual is genuine edge-sweep change,
which SHOULD snap), mask still tracks the sweep. The irr gate stays unscaled
by measurement — D-64's composite was clean; its moments self-widen under
motion.

## 14. Traced environment diffuse — env joins the GI gather (BUG-yq1d (traced AO never darkens env diffuse); SUPERSEDES MB3; LANDED 2026-07-31 — ED-A (kernel/substitution/constants/clamp + PBR-only consumers ED3a + void-texel fallback, Peter's metal-fixture sign-off), ED-B (sun-disc firefly fixture, gain 32 by measurement, furnace is the noise gate's correctness leg — closes BUG-ipad (noise gate certifies frozen noise); stability ceilings re-baselined), ED-C (white-enclosure convergence: reference irradiance 0.954 vs MC 0.934, ED4's constants certified — closes BUG-qt32 (GI energy constants look unphysical)))

The furnace oracle (lane/rt-furnace-oracle) measured what MB3's "env is never gathered"
actually costs: with a uniform sky wired, the term that lights the scene is the raster
irradiance map (`diffuse_ibl`, `render_scene.wgsl`), and no traced ray ever touches it.
Traced occlusion multiplied only the flat Ambient knob (`irradiance = ambient_color*ao +
gi`), which defaults to ~zero — so a floor-wall corner under a full sky read 1.0005x the
open-ground value. Occlusion was absent in any env-lit scene, exactly where it matters.

**Stage translation.** Corners, contact shadows, and concavities darken under sky
lighting — the difference between objects floating on a backdrop and objects sitting in
a space. This is most of what "turn RT on" was supposed to buy for env-lit hero scenes.

### 14.1 Why MB3 falls

MB3 (2026-07-30) kept env out of the gather so it could never double-count against the
flat ambient term, preserving "knob 0 = true black". The furnace evidence shows the
premise was already false whenever a sky is wired: `diffuse_ibl` lights the scene
regardless of the knob, so knob 0 was never black in env scenes — the knob is a fill
control, not a master. Meanwhile the double-count MB3 feared is avoided the same way
reflections already avoid it: substitution, never addition (RD1/I-R3 discipline).

The scale question that makes substitution clean: `ibl_irradiance.wgsl` bakes the
irradiance map as the plain average of cosine-weighted env radiance samples (the cos
and 1/pi cancel — documented in its header). The GI gather is the same estimator with
the same normalization, so "miss returns env radiance in the ray direction" produces
exactly the quantity the irradiance map holds, on the same scale. Uniform sky L: both
return L.

### 14.2 Decisions

- **ED1 — env joins the GI gather at every depth.** A gather ray that misses returns
  the equirect env radiance in its own direction (mip 0 — unbiased; the prefiltered
  chain's base level IS the source env, and the kernel already binds it for
  reflections, so no new binding). Extension rays (bounce >= 1) do the same through
  `throughput` — this is what makes an enclosure with an opening converge instead of
  going artificially dark. Cost: one texture fetch per miss, no new rays.
- **ED2 — substitution, never addition.** The kernel's irradiance texture becomes
  `.rgb = env+GI gather` (no `ambient_color*ao` folded in), `.a = ao` (free end-to-end
  today — every stage writes `float4(x, 0)` and reads `.rgb`; the three write sites
  learn to carry alpha through the same accumulation weights). In `render_scene.wgsl`:
  `diffuse_ibl`'s irradiance-map fetch is REPLACED by the traced `.rgb` when RT is on
  (same physical quantity — the 818a06b0 double-count trap, same guard as RD1); the
  baked material occlusion texture still multiplies (artist micro-occlusion, a
  different quantity, IMPORT_FIDELITY_DESIGN.md D3 unchanged). `rt_or_flat_ambient`
  recomposes today's value consumer-side: `albedo * scene_params.y * ambient_tint *
  AMBIENT_IRRADIANCE_SCALE * mask.a` — the 0.15 ceiling constant mirrors into WGSL
  (named, cross-mirror comment discipline per `RT_REFL_PREFILTER_MAX_MIP`). Each term
  is consumed in exactly one place; the Ambient knob never gets `kd_ibl` scaling.
- **ED3 — demodulation unchanged** (D3/MB5): no primary-surface albedo in-kernel.
- **ED3a — RT lighting consumers are PBR-only (Peter, 2026-07-31).** The ED2
  substitution lives in `fs_pbr`'s `diffuse_ibl`; phong/cel draws in an RT
  scene get the `rt_or_flat_ambient` recompose and NO traced env/GI (their
  old GI coupling was accidental — they rode the shared ambient slot), with
  a one-time `log::warn` per scene instance so the degradation is loud.
  Phong is absent from every shipped preset (survey 2026-07-31: only the
  three RT compare fixtures used it — migrated to `pbr_material` +
  uniform-black `bake_environment` in ED-A); supporting a second lit
  consumer would double every RT shading decision for a material nothing
  ships. Unlit is exempt by design (no lighting). Blend-queue fragments at
  void texels keep the raster irradiance-map fetch (the `rt_refl.a < 0`
  fallback discipline — the fragment class from BUG-88m (rt-reflection-substitution-domain-wider-than-trace-domain)
  — keyed on the kernel's void signature rgb 0/ao 1 in WGSL).
- **ED4 — the two GI constants are settled by the codebase's own conventions, then
  certified by fixture** (closes BUG-qt32 (GI energy constants look unphysical)).
  `RT_GI_THROUGHPUT_FOLD` is DELETED: the cosine-weighted estimator's throughput
  multiplier is the hit albedo alone (pi cancels — the convention
  `ibl_irradiance.wgsl` documents; the extra 1/pi made bounce 2 ~3.1x dark).
  `SUN_BOUNCE_INTENSITY_SCALE` becomes 1/pi: the raster light loop's diffuse is
  `kd * albedo / PI * l_col * n_dot_l` (`render_scene.wgsl` light loop), so sun
  colour is an irradiance and the second-vertex bounce carries the same 1/pi; 0.08
  was ~4x dark. Both derivations are stated here so the fixture (ED-C) is checking
  physics, not curve-fit.
- **ED5 — firefly control on the env sample.** An HDRI sun disk at mip 0 through 2
  spp is the sparkle regime the reflection path needed its clamp for. Per-sample cap:
  `RT_GI_ENV_FIREFLY_GAIN` (named constant, committed range per the RT_REFL_FIREFLY_GAIN
  precedent) times the roughest-mip env fetch at the surface normal
  (`refl_env_sample(n, 1.0)` — a typical-value anchor already bound, no new texture).
  At gi_spp < 3 a median is inert; the env anchor is not.
- **ED6 — the substitution gate mirrors the reflection fallback.** Traced diffuse
  substitutes only when RT is on AND the GI gather ran (`gi_spp > 0`); otherwise the
  irradiance map stands, same discipline as `rt_refl.a < 0` keeping the raster
  prefiltered fetch. No new scene param (MB4 discipline).
- **ED7 — the furnace oracle becomes a two-path oracle, and joins the noise gate**
  (closes BUG-ipad (noise gate certifies frozen noise) when wired). Open-sky
  brightness must now be returned by the TRACED path (RT on) AND match the raster
  path (RT off) within tolerance — before this change the brightness leg passed
  entirely through the raster irradiance map and certified nothing about RT. The
  corner leg (ratio < 1 by a committed margin) goes live for the first time.
  `scripts/rt_noise_gate.py` green requires the correctness oracle alongside the
  stability ceilings — stability alone can no longer certify (the frozen-seed trap).

### 14.3 Invariants & enforcement

- **I-ED1 — no-env RT scenes keep today's ambient/AO values.** Ambient-only fixture
  (no env, knob swept), probed values equal within 1e-6 — an epsilon gate, not
  `cmp`: multiplication order changes consumer-side.
- **I-ED2 — the env term has exactly one consumer.** `rg`: `irradiance_map` is
  sampled once in `render_scene.wgsl`, at the substitution site; the RT irradiance
  texture is read in exactly two places (the substitution, the ambient recompose).
- **I-ED3 — substitution, never addition.** With RT on, irradiance map and traced
  diffuse never both contribute (RD1 pattern; machine-checked by I-ED2 plus the
  RT-on/off furnace cross-check).
- **I-ED4 — furnace, both legs.** Open sky: RT-on brightness 1.0 +/- tolerance, and
  |RT-on - RT-off| within tolerance. Corner: shaded/open ratio below a committed
  ceiling, with the white-occluder anti-vacuity discipline from the furnace branch
  (albedo-1 everything, or the gate reads paint).
- **I-ED5 — white-enclosure convergence** (BUG-qt32's empirical close): a white
  multi-bounce enclosure under uniform sky returns the field radiance within
  tolerance; any deficit is lost energy and fails the gate.

### 14.4 Phases (one lane brief each, lead review per phase)

#### ED-A — kernel + substitution

- *Deliverables:* env-on-miss at all gather depths; ao to `.a` through accumulation
  (the three write sites); ED2's two consumer changes; ED4's constants; ED5's clamp.
- *Gate:* clippy `-p manifold-gpu -p manifold-renderer`; `scripts/gpu_proofs_gate.py`
  including the furnace oracle's I-ED1/I-ED4 legs; the RT suite otherwise green.
- *Named deviation, gated by eye, not number:* GI/emissive/sun-bounce energy is now
  weighted by `kd_ibl` and baked occlusion like every other diffuse term — metals
  lose the unphysical full-strength diffuse fill they get today. Peter reviews one
  metal fixture render.

#### ED-B — HDRI firefly fixture + noise-gate pairing

- *Deliverables:* real-HDRI scene fixture (sun disk, bright/dim extremes), cap tuned
  in its committed range; furnace oracle wired into `scripts/rt_noise_gate.py` as the
  correctness leg (BUG-ipad closes).
- *Gate:* the paired gate green on the fixture; frozen-seed attack re-run and caught.

#### ED-C — enclosure convergence

- *Deliverables:* I-ED5's white-enclosure fixture with a brute-force converged
  reference vs the shipping path; BUG-qt32 closes on the measured numbers, whatever
  they say about ED4's constants.
- *Gate:* enclosure within tolerance at committed spp/history.

**Consequences, stated honestly.** Worst-case GI cost is unchanged in ray count; the
env fetch is one texture read per miss. The accumulate/denoise chain carries alpha
where it carried zero — same bandwidth class, no new texture. And the metal-fill
deviation above is a real look change on existing RT scenes: darker, correct, and
Peter signs it off on a fixture before it lands.

## 15. Many-light direct lighting — caster cap + emissive RIS (Tier 3 item 6; APPROVED 2026-08-02, K3 lead on k3-restir-design's draft; **RS-A LANDED 2026-08-03 on main — MAX_RT_CASTERS 8, second sv texture through trace→upsample→atrous→SV-ACCUM→binding 44, 6-caster value test, gpu-proofs 143/143, trace_ms 4-vs-8 delta −0.06ms; RS-B (emissive light table: per-triangle power-ranked build + alias table + refit, 4096 cap, CPU-oracle value tests) LANDED 2026-08-03 on main; RS-C (kernel sampling + substitution) LANDED 2026-08-07 on main — I-RS3 gate green (delta/analytic 0.876), full gpu-proofs green; BUG-ny4v (RS-C I-RS3 gate hangs the GPU) fixed at the root: degenerate self-sample rays inverted the visibility interval (guard: non-empty interval + rt_finite), and the sampler emits double-sided to match the gather/raster paths)**)

### 15.0 What changed since the scoping note

Section 8 item 6 (2026-07-23) says "RT shadows are sun-only today; every
other light is still a shadow map." That is stale. The multi-caster fix
shipped after it: all `MAX_RT_CASTERS` (= 4) casters — sun AND point — are
traced per pixel with soft shadows (`render_scene.rs:4533-4557` fills one
`RtCasterParams` per caster, kind 0 sun cone / kind 1 point with
`light_size`; kernel loop `crates/manifold-gpu/src/metal/raytrace.rs:1307-1346`),
and shadow maps never render at all in an RT scene
(`render_scene.rs:3933` and `:4079` both gate on `!(rt_enabled && rt_ready)`).

The item's real content, restated against today's code:

1. **Casters beyond 4 are unshadowed.** `MAX_RT_CASTERS = 4` is a fixed
   kernel slot count (`raytrace.rs` (metal) `:542` MSL, `:2791` Rust);
   `shadow_factor` returns lit for slot −1 (`render_scene.wgsl:636`). A
   show scene with 6 shadow-casting strobes shadows 4 of them.
2. **Emissive geometry has no direct-light estimator.** Emissive surfaces
   light the scene only when a cosine-hemisphere GI gather ray happens to
   hit them (`raytrace.rs` (metal) `:1459-1469`, `hit_emissive` at bounce
   1). A small bright emitter — an LED strip, a strobe bar, a glowing edge
   on a hero scan — is hit by almost no gather rays, so its direct light
   is statistically absent at 4 GI spp. This is the actual many-light
   problem in MANIFOLD's scene class, and it is the one worth building.

### 15.1 Audit — what exists (verified 2026-08-02)

| Piece | Where | State |
|---|---|---|
| Light types | `crates/manifold-renderer/src/node_graph/light.rs:43-52` (`LightMode`) | **Sun and Point only. No spot light exists** (negative `rg` for `Spot` in `light.rs`). Both modes carry `cast_shadows`, `shadow_softness` (PCF tiers + `Contact { light_size }` PCSS), `shadow_resolution`. |
| Light count cap | `render_scene.rs:146` (`LIGHT_SLIDER_MAX = 64`), module doc `:14-16` | Uncapped structurally — runtime-sized `@binding(8)` storage buffer; slider soft bound 64, `setBytes` hard ceiling 127. |
| Real light census | Peter's `~/Downloads/*.manifold`, counted `node.light` occurrences per project 2026-08-02 | Typical 1–10 light nodes; max 20 (`SceneLadders`). **A few dozen is the observed ceiling; the median show scene is single digits.** |
| Shadow-map path | `render_scene.rs:159` (`MAX_SHADOW_CASTING_LIGHTS = 4`), `:3089-3130` | First 4 `cast_shadows` lights in slot order, one depth-only prepass each; lights past the cap illuminate unshadowed (slot −1 → `shadow_factor` 1.0, `render_scene.wgsl:636`). |
| RT shadow path | `render_scene.rs:4526-4557`; kernel `raytrace.rs` (metal) `:1304-1346` | All ≤4 casters traced, sun cone (`SOFT_SHADOW_CONE_RADIANS`, `render_scene.rs:191`) and point `light_size` softness; visibility rides `out_sv` RGBA, one channel per slot, consumed by `shadow_factor` at `render_scene.wgsl:647-650`. |
| sv accumulation | `raytrace.rs` (metal) `:2118-2193` | SV-ACCUM: ping-pong history + per-channel moments + snap-hold; geometry-driven gates, deliberately NOT snapped by the lighting key (visibility is geometry; strobes must not snap it — `:2125`). |
| GI gather (emissive path) | `raytrace.rs` (metal) `:1428-1486` | 4 spp cosine hemisphere, 2 bounces; bounce-1 `hit_emissive` = factor × emissive-map sample (BUG-1gqt (kernels ignored emissive texture)). Demodulated into `out_irr.rgb`, accumulated, substituted for `diffuse_ibl` in `fs_pbr` (ED2). |
| Emissive volumetric proxy | `render_scene.rs:4942-4968` | Every emissive object is already an unshadowed Point-mode pseudo-light in the shaft march — the per-object proxy precedent. |
| Lighting-key snap | `render_scene.rs:4823-4864` | CPU hash of caster direction/colour/cone/kind + ambient snaps history on light-param change (section 13). Does NOT cover in-graph animated emissive — per-texel gates carry that today, same as for the GI gather. |
| Trace dispatch | `render_scene.rs:4566-4567`, D11/D22 | One dispatch, half of render res; render res = 2/3 native under temporal upscale → trace ≈ 1280×720 ≈ 0.92M texels at 4K. |
| Ray budget today | kernel + `render_scene.rs:191-244` | Per trace texel: ≤4 shadow + 1 primary + 4 AO + 4 GI × (2 bounces + ≤4 sun-bounce/hit) + 8 refl ≈ 45 rays worst case. |
| Frame cost | brief + section 13 table | Full-RT frame ~26ms at half-res rays; 24fps ceiling = 41.6ms. |
| Reset machinery | D15, `render_scene.rs:839` | One `TemporalResetDetector`; a second is forbidden (I-R2 pattern). |
| Region-probe harness | `tests/gpu_proofs/rt_p1_region_probe.rs`, `rt_p3_emissive_gi.rs` | The gate precedent every phase below copies. |

**Extend, don't redesign.** The emissive-sampling term joins the existing
`trace_shadow_rays` dispatch and the existing `out_irr` accumulation chain.
The caster-cap raise extends existing slot tables. No new pass, no new
temporal state.

### 15.2 Decisions

- **RS1 — two mechanisms for two different gaps, not one ReSTIR.**
  Analytic lights and emissive geometry are different problems at
  MANIFOLD's scales and get different answers (RS2, RS3). Rejected: one
  sampled estimator over a unified light table for everything — it makes
  the ≤8-caster case stochastic (worse than deterministic) to serve a
  table shape only the emissive case needs.
- **RS2 — analytic lights: raise the RT caster cap 4 → 8, deterministic
  rays, no sampling.** At the observed census (typical <10 light nodes,
  max 20) every shadow-casting light a real show uses fits in 8 slots, and
  deterministic per-caster rays are strictly better than sampling at these
  counts — zero variance, zero MIS, zero clamp machinery. Cost: 0–4 extra
  shadow rays per trace texel, only on scenes that actually wire >4
  casters. `MAX_SHADOW_CASTING_LIGHTS` (the raster shadow-map path) stays
  4 — non-RT scenes are untouched; the raster/RT coverage divergence
  widens from "RT shadows better" to "RT shadows more", same direction as
  every RT term so far. Un-suppression trigger for >8: a real show project
  with >8 shadow-casting lights — at that point re-read RS5's rejection,
  because that is the light count where sampling starts winning.
- **RS3 — emissive geometry: RIS (resampled importance sampling, one
  candidate, no reservoir reuse) over an emissive-triangle light table,
  one visibility ray per trace texel, demodulated into `out_irr.rgb`.**
  This is the estimator at the core of ReSTIR DI with the spatial/temporal
  reservoir machinery deliberately left off — see RS5 for why that is not
  cowardice but sizing. Unbiased: candidate triangle drawn from an alias
  table ∝ static geometric power, a point sampled area-uniform on it, one
  shadow ray to the point, weighted by the full solid-angle factor. Handles
  any emissive-triangle count at flat per-pixel cost, which is the only
  sense in which MANIFOLD is a many-light scene class.
- **RS4 — the term rides `out_irr`; there is no new temporal state of any
  kind.** The sampled direct-emissive irradiance is the same physical
  quantity the GI gather already writes (demodulated incident irradiance
  at the primary surface, no albedo — D3/MB5 discipline). It is added
  in-kernel to `out_irr.rgb` and inherits upsample, à-trous, accumulation,
  the D15 reset detector, and the section-13 gates unchanged. Rejected:
  reservoir textures with their own ping-pong history — a second temporal
  state machine whose failure modes are exactly the classes three waves
  paid to tame: BUG-311 (motion ghosting), BUG-312 (speckle), and
  BUG-322 (rotation shimmer) — built to amortize a signal the existing
  accumulator already amortizes (it converges 8-spp reflections and 4-spp
  AO today).
- **RS5 — full ReSTIR DI (temporal + spatial reservoir reuse) is rejected
  for this scene class, with a named revival trigger.** ReSTIR's value
  scales with light count and with rays saved per shading point.
  MANIFOLD's analytic counts are single digits; its emissive case is
  served at 1 ray/pixel by RS3; and the heavy temporal+spatial filtering
  ReSTIR would amortize into already exists as the accumulator + à-trous
  chain. What reservoirs would buy — cleaner 1-spp input before filtering —
  is the thing this codebase has repeatedly chosen to buy with accumulation
  instead (section 13's 1/n running mean). Revival trigger: after RS-C and
  Peter's deferred ray-budget re-judge, converged stills STILL show
  emissive-sampling noise the accumulator cannot hide — measured, not
  eyeballed, on the rt-capture instrument (section 13). At that point
  reservoirs are a delta on RS3's machinery (the table, the estimator, the
  clamp all survive), not a redesign.
- **RS6 — static sampling weights, current-frame evaluation: strobes are
  free.** The alias table weights are geometric (triangle area × build-time
  emissive power rank) and built once with the accel; the estimator reads
  the CURRENT frame's emissive colour from `gi_materials` / the emissive
  map at evaluation. RIS stays unbiased for any candidate distribution
  with nonzero probability, so an intensity strobe needs no table rebuild
  and invalidates nothing — the strobe case reduces to "the term's value
  changed this frame", which is precisely what the section-13 per-texel
  gates and the accumulator's snap machinery already govern for the GI
  gather's emissive term. The lighting key is NOT extended to emissive
  (it can't see in-graph animation today; per-texel gates carry it — same
  coverage as the status quo, no regression).
- **RS7 — substitution, never addition: the sampler owns DIRECT emissive
  light; the GI gather keeps indirect.** The gather's bounce-1
  `hit_emissive` is deleted when the sampler runs; bounce ≥ 2 keeps its
  `hit_emissive` (that IS indirect light), and env/sun-bounce are
  untouched. The `818a06b0` double-count trap one term over — same guard
  family as RD1/I-ED3. Machine check: I-RS3.
- **RS8 — firefly control by table-mean anchor, ED5's pattern one level
  down.** A near or hot triangle gives one sample a huge weight; at 1
  candidate there is no median to anchor on (RT_REFL's clamp needs ≥3
  spp). Cap each sample's luminance at `RT_EMISSIVE_FIREFLY_GAIN` × the
  light table's mean power — a CPU-computed scalar riding
  `ShadowRayParams`, the same "typical-value anchor, no new texture" move
  as ED5's roughest-mip env anchor. Named constant, committed range 8–32
  per the `RT_GI_ENV_FIREFLY_GAIN` precedent; tuned by measurement on the
  RS-C fixture, not by eye.
- **RS9 — resolution: the term inherits the irradiance channel's
  resolution, whatever the quality wave decides.** It is written into
  `out_irr` at trace size, so if the wave's A2 measurement moves a ray
  term native-res, this term follows `out_irr`'s fate automatically. No
  independent resolution decision exists here.
- **RS10 — the Metal RT trait grows no new method (RD10 precedent).**
  `dispatch_shadow_rays` gains the light-table buffer argument;
  `ShadowRayParams` gains the emissive-sampling fields. Vulkan parity
  (D9): the table and alias are plain `GpuBuffer`s; sampling is in-kernel
  ray queries — a translation matter, nothing Apple-shaped crosses
  `manifold-gpu`.

### 15.3 Architecture

**Light table.** One entry per emissive triangle of every RT-registered
object with non-black emissive (factor or map): `{ v0, v1, v2 }` world
positions (refit alongside the TLAS — transforms only, same discipline as
`refit_accel`) + the alias table (probability + alias index per entry),
both plain `GpuBuffer`s. Built CPU-side on the content thread at
accel-build time inside `render_scene`'s existing RT registration —
D17's async discipline applies unchanged (table lands with the accel's
ready flag; until then the GI gather owns emissive as today, no partial
state). Capped at `RT_EMISSIVE_TABLE_MAX = 4096` entries, top-power
truncation — **Consequences, stated honestly:** beyond 4096 emissive
triangles the dimmest tail is never sampled (a bias, not a crash);
a photoscan with an emissive map over most of its surface blows the cap
by itself, which is why truncation is by power rank, not slot order.
Trigger to raise or go hierarchical: a hero asset whose emissive tail
visibly vanishes — Peter's look on the RS-C demo pair.

Weights are static-geometric (RS6): per-entry power = area × build-time
emissive luma (factor × emissive-map mean; the map mean is computed once
at registration from the already-uploaded texture). Per-frame intensity
changes (a strobe, an LFO on emission) never touch the table.

**Kernel flow**, inside `trace_shadow_rays` after the GI block, reusing
`sec_origin`, `shading_n`, `bias_eps`, `walk_with_alpha_test`:

1. `u = rand2(tid, frame, 700u)` → alias draw → triangle `t`; second draw
   → area-uniform point `q` on `t` (standard sqrt-remap).
2. `l = q - sec_origin`; shadow ray toward `q`, `max_distance = |l| -
   bias_eps`, `RT_MASK_SHADOW_CASTER` — one query, any-hit.
3. Contribution = `emissive_at_hit(...)` at the triangle's own UV (the
   current frame's value — RS6) × `max(dot(shading_n, l̂),0)` ×
   `max(dot(n_t, -l̂),0)` × area / (`|l|²` × pdf), pdf from the alias
   entry; firefly-capped per RS8; added to the `gi` accumulator BEFORE
   the `/ gi_spp` divide site owns its own normalization (the term is a
   single weighted sample, not averaged with the gather).
4. The GI block's bounce-1 `hit_emissive` line is gated off when the
   sampler ran (RS7); bounce ≥ 2 unchanged.

**No WGSL change.** The term arrives inside `rt_irradiance_mask.rgb`,
already substituted for `diffuse_ibl` (ED2) and already modulated by
albedo/`kd_ibl` consumer-side. The entire raster-side diff is zero lines;
a larger diff to `render_scene.wgsl` in RS-C means the phase has gone
wrong (the RS2 sv-mask widening excepted — that phase owns binding 44).

**Stage translation.** A glowing strip or strobe bar on a hero object
lights its surroundings directly and correctly — crisp enough to read as
a source, not only as the soft wash the GI gather statistically finds.
Under a strobe the light lands the same frame the emission flips (RS6),
with the accumulator's snap machinery deciding convergence speed exactly
as it does for every other RT term. Cost: one ray per trace texel plus
the cap raise's 0–4 — about +2% worst-case rays on today's budget.

### 15.4 Invariants & enforcement

- **I-RS1 — one temporal-reset path.** Negative `rg` for a second
  `TemporalResetDetector` construction in `render_scene.rs` (I-R2
  pattern). RS4 makes this structural: no new history exists to reset.
- **I-RS2 — the emissive table is derived state, never a side effect.**
  Built from the same `objects` slice and registration path as the accel
  (section 10 contract: the RT scene is a derived cache). Negative `rg`:
  no table construction outside the RT registration block.
- **I-RS3 — substitution, never addition.** Cross-commit A/B (MB-B
  precedent): the emissive fixture's converged direct-light region at
  RS-C equals the CPU-computed analytic value within stated tolerance,
  AND the pre-RS-C gather-only capture of the same fixture reads BELOW
  a stated floor on the small-emitter region (the gather statistically
  misses it) — both legs required; "adds on top" shows as the RS-C leg
  reading ~2× analytic and fails loudly.
- **I-RS4 — no Apple types above `manifold-gpu`.** Standing negative `rg`
  (`objc2|MTL` zero hits outside `manifold-gpu`) at every phase gate.
- **I-RS5 — no emissive sampling on the non-RT path.** The sampler block
  lives inside the existing `rt_ready` dispatch; the native-mode
  byte-identity discipline (T2-B precedent — real `graph-tool render`
  machine diff, never a code-diff argument) is re-run at RS-C's gate.
- **I-RS6 — sv mask channel count matches `MAX_RT_CASTERS`.** After RS2:
  `rg` — the shadow-mask textures' channel total (two `Rgba16Float`) and
  the Rust/MSL `MAX_RT_CASTERS` constants agree at 8; `shadow_factor`'s
  slot indexing covers 0..7 (the `clamp(..., 0, 3)` at
  `render_scene.wgsl:649` is gone).

### 15.5 Alternatives (priced)

Trace-res ≈ 0.92M texels at 4K/temporal-upscale. Ray costs per texel.

- **Trace every analytic light deterministically, uncapped.** Rays =
  casters/texel — fine at 4, linearly bad at 20, useless for emissive
  triangles (they aren't casters). Survives as RS2, capped at 8.
- **Light-list importance sampling for analytic lights too.** 1 ray/texel
  regardless of count, but converts 4–8 deterministic visibilities into
  stochastic ones needing accumulation to converge — strictly worse at
  observed counts. Becomes interesting past ~16 casters; RS2's trigger
  names the re-read.
- **Per-OBJECT emissive proxies instead of per-triangle** (the
  `shaft_lights` shape — one point/disc light per emissive object): table
  build collapses to trivial, but an extended emitter (a wall of panels, a
  long strip) lights from its centroid — no shape, no contact gradient,
  and the bright end of a strip can't read brighter than the dim end.
  The per-triangle table is the mechanism that makes emission shape true;
  the proxy is the named fallback only if RS-B's table build proves
  intractable in review, not a phase option.
- **Full ReSTIR DI.** RS5. Priced: reservoir ping-pong textures + a
  spatial reuse pass + bias correction ≈ the sv/irr accumulation surface
  area again, to reduce variance the existing accumulator already reduces;
  the spatial pass re-opens the edge-stop problem à-trous already owns.
- **Clustered / light-tree sampling.** For thousands of lights.
  MANIFOLD's observed ceiling is 20 analytic + ≤4096 table entries; a
  tree over 4096 entries buys nothing over an alias table. Revival
  trigger: a scene class that makes RS3's cap the binding constraint.

### 15.6 Phases

One lane brief each, committable, every gate numeric. Pre-allocated BUG
range: assign at wave dispatch.

#### RS-A — caster cap 4 → 8

- *Entry:* main with section 14 landed; re-verify `raytrace.rs` (metal)
  `:542`/`:2791` (`MAX_RT_CASTERS` still 4 both mirrors), `render_scene.
  wgsl:649` (the `clamp(..., 0, 3)` slot index), `render_scene.rs:4533`
  (rt_casters fill). A moved anchor is an escalation, not a guess.
- *Read-back:* RS1/RS2; the multi-caster fix's own diff shape
  (`rt_multi_caster_shadow.rs` — the test this phase extends); the MSL/
  Rust mirror-sync discipline comments at `raytrace.rs` (metal) `:538-542`.
- *Deliverables:* `MAX_RT_CASTERS` 8 in both mirrors with the manual-sync
  comment updated; `ShadowRayParams` caster array growth (offset asserts
  stay green); second sv texture (slots 4–7) through trace → upsample →
  atrous → SV-ACCUM → binding 44; `shadow_factor` two-texture slot
  indexing; the 6-caster value test — each caster independently occluded
  reads its own channel's visibility, CPU-computed expected, on the
  `rt_p1_region_probe` harness.
- *Gate:* clippy `-p manifold-gpu -p manifold-renderer`; the new value
  test; `scripts/gpu_proofs_gate.py`; negative `rg`: `clamp(i32(slot_f + 0.5), 0, 3)`
  zero hits; `MANIFOLD_RENDER_TRACE=1`, no frame >20ms on an 8-caster
  fixture, `trace_ms` delta 4-vs-8 casters reported as a number.
- *Performer gesture:* wire 6 shadow-casting point lights into a playing
  RT scene — every light's strobe throws its own shadow, no frame >20ms.
- *Forbidden moves:* touching `MAX_SHADOW_CASTING_LIGHTS` (raster path
  stays 4); a third sv texture "for headroom"; sampling instead of
  deterministic rays; claiming channel-count agreement without I-RS6's rg.
- *Demo:* none — L1 (no new visible surface beyond more-correct shadows;
  Peter sees it in RS-C's demo pair).
- *Test scope:* `-p manifold-renderer -p manifold-gpu` + gpu-proofs.

#### RS-B — emissive light table + alias build

- *Entry:* RS-A landed; re-verify the RT registration block
  (`render_scene.rs:4526` area) and `RtObjectGeometry`'s field set.
- *Read-back:* RS3/RS6/RS8; D17; the `build_normal_sources` /
  `ensure_normal_sources` pattern (the per-object bindless-table build
  this phase's table build mirrors); `GiMaterial` population
  (`render_scene.rs:4605`).
- *Deliverables:* per-triangle emissive table + alias table, built
  CPU-side at accel registration, capped 4096 by power rank; emissive-map
  mean computed once per registered texture; world-position refit on
  `refit_accel`; table-mean-power scalar into `ShadowRayParams`; value
  test — table contents (areas, powers, alias probabilities, truncation
  order) vs CPU-computed expected on a synthetic multi-object fixture,
  exact math; held-out input — a real GLB with an emissive map the lane
  did not develop against, table entry count and total power vs a
  CPU-computed census of the same asset.
- *Gate:* clippy `-p manifold-gpu -p manifold-renderer`; both value
  tests; `scripts/gpu_proofs_gate.py`; I-RS2's negative `rg`.
- *Forbidden moves:* building the table mid-frame (D17); a new
  `Arc<Mutex>`; reading the emissive map per frame (the mean is a
  registration-time computation); per-object proxies "to simplify".
- *Demo:* none — L1 (no pixels yet; RS-C is the vertical slice).
- *Test scope:* `-p manifold-renderer -p manifold-gpu` + gpu-proofs.

#### RS-C — kernel sampling + substitution

- *Entry:* RS-B landed; held-out census numbers recorded (in the
  2026-08-02 automated session: recorded by the lead; Peter's look closes
  with the wave's other owed looks).
- *Read-back:* RS3/RS4/RS6/RS7/RS8; the GI block (`raytrace.rs` (metal)
  `:1428-1486`) whole; `emissive_at_hit` (`:825-838`); ED-B's firefly
  tuning record (`:1408-1427` — the measurement style RS8's gain copy).
- *Deliverables:* the section-15.3 kernel block (alias draw, point
  sample, one visibility ray, solid-angle weight, RS8 clamp); RS7's
  bounce-1 `hit_emissive` gate; new value test
  `tests/gpu_proofs/rt_emissive_direct.rs` on the `rt_p3_emissive_gi.rs`
  harness: a small bright emissive quad above a floor — CPU computes the
  analytic solid-angle irradiance at a named floor region; converged
  (accumulated) region mean within stated tolerance; **control leg,
  mandatory:** the pre-RS-C gather-only behaviour on the identical
  fixture reads below a stated floor (cross-commit A/B, MB-B precedent),
  and bounce-2 indirect emissive (a wall only reachable via one bounce)
  is unchanged within epsilon (proves the RS7 gate didn't amputate
  indirect).
- *Gate:* (a) the value test with both legs; (b) I-RS3's substitution
  pair; (c) I-RS5's native-mode machine diff; (d) negative `rg`: I-RS1,
  I-RS4; (e) `scripts/gpu_proofs_gate.py`; (f) `MANIFOLD_RENDER_TRACE=1`
  no frame >20ms, **`trace_ms` delta sampler on vs off reported as a
  number** — the budget evidence for Peter's deferred ray-budget
  re-judge; (g) strobe leg: emissive intensity flipped 0 ↔ full across
  two frames through the real node path, region mean at flip+1 ≥ stated
  fraction of converged (RS6 — no table rebuild, snap machinery lands
  the cue).
- *Performer gesture:* an LFO strobing an emissive hero object's emission
  on a playing RT scene — the room's lighting strobes with it, same
  frame, no boil beyond the accumulator's governed convergence.
- *Forbidden moves:* reservoir textures or any new temporal buffer; a new
  scene param (ED6/MB4 discipline); a WGSL diff (section 15.3 — zero
  lines is the spec); averaging the sampled term into the `gi_spp`
  divisor; touching the env/sun-bounce terms; a second
  `TemporalResetDetector`; claiming the substitution from a code reading
  instead of the two-leg gate.
- *Demo (Peter only):* PNG pair on an emissive-strip hero scene —
  gather-only vs sampled, converged stills — **L2; the stage verdict is
  Peter's look** (D19/D20 standing lesson).
- *Test scope:* `-p manifold-renderer -p manifold-gpu` + gpu-proofs
  (`cargo test`, never nextest).

**Phasing-completeness check:** every section-15.2 commitment lands in a
phase — RS2 (RS-A), RS3/RS6/RS8 + table (RS-B), RS3/RS4/RS7/RS8 kernel
(RS-C); RS1/RS5/RS9/RS10 are structural rulings enforced by I-RS1/I-RS2/
I-RS5 and the forbidden-move lists. Deferred with triggers below.

### 15.7 Deferred (with revival triggers)

- **Reservoir reuse (full ReSTIR DI).** RS5's measured trigger:
  converged stills still show emissive-sampling noise after RS-C and the
  ray-budget re-judge, on the rt-capture instrument.
- **More than 8 analytic casters.** Trigger: a real show project wiring
  >8 shadow-casting lights; re-read the light-list-sampling rejection
  then, not before.
- **Hierarchical / power-rank-free table (light tree over emissive
  triangles; emissive-map texel-level entries).** Trigger: a hero asset
  whose emissive map concentrates in a small texel fraction (the
  map-mean weight mis-ranks it) or whose emissive tail visibly vanishes
  under the 4096 cap — Peter's look on the RS-C demo pair.
- **Specular response to emissive lights** (direct-light specular from
  the sampled term). The reflection path already picks emissive up at
  hits; a diffuse-only direct term is the v1 scope call. Trigger: Peter's
  look reports emissive sources reading flat on glossy floors.
- **Clustered analytic-light sampling.** Trigger: a scene class past
  `LIGHT_SLIDER_MAX` — the slider itself moves first.

## 16. RT translucency — light through thin surfaces (Tier 3 item 9; APPROVED 2026-08-02, K3 lead on k3-translucency-design's draft; **TL-A LANDED 2026-08-03 on main (wave/rt-quality) — wrap term + uniform + cardable param + KHR import; I-TL1 byte-identity, gpu-proofs 140/140**; **TL-B LANDED 2026-08-07 (wave/rt-translucency-b) — transmitting walk + BLAS opacity + luma sv; 5 value proofs + full gpu-proofs green, trace_ms delta ≈ 0 (−0.23 ms) on the 4K apricot**; **TL-C LANDED 2026-08-07 (wave/rt-translucency-c) — colored sun tint: out_svt through the chain + fs_pbr substitution for the designated sun slot; 4 value proofs + 2 cut-reset oracle legs + full gpu-proofs green + noise gate green; 4K apricot 44.2 ms median, of which TL-C's texture ≈ +1.1 ms and the BUG-p14x (RS-A sv2 slot-map omission) repair ≈ +3.2 ms against a 39.8 ms same-day control. Feature A complete; B stays deferred per section 16.6**)

Tier 3 item 9 names two features. **(A) thin-surface transmission** — sunlight
through a flower petal: the petal glows when backlit, and the light that passes
through it lands, tinted, on whatever is behind. **(B) volumetric
participation** — the god-ray march seeing traced occlusion at every march step,
so beams carve themselves out of haze between the camera and the surface.

**Recommendation: build A now (three one-session phases), keep B
deferred with a revival trigger.** The audit shows A is a forward-shading term
plus an extension to a walk that already exists, on data the kernel already
fetches — cheap in rays and in machinery. B is a per-march-step ray problem: its
cheapest honest variant costs ~+37% of the entire trace class, and its full form
is absurd (section 16.6). B also gets a free dividend from A: the transmitted shadow
mask A produces already flows into the march's sun visibility, so haze under a
petal canopy dapples at zero extra cost.

**Stage translation.** Peter's hero assets are photoscanned flowers. Today a
petal between the sun and the camera renders dark on its back side (the shading
normal is flipped toward the camera, `render_scene.wgsl:1372-1374`, so N·L = 0
and only IBL/ambient fill remains), and its shadow is a hard black silhouette.
With A: petals glow their own color when backlit — pink through pink, green
through green — stacked petals dim each other softly instead of binary, and the
floor under a backlit flower carries a colored pool instead of a void. That is
"light through petals" as a stage look, and the factor is one cardable scalar,
so the glow can breathe with the music like every other material param.

### 16.1 Audit — what exists (verified 2026-08-02)

| Piece | Where | State |
|---|---|---|
| Two-sided shading normal | `render_scene.wgsl:1372-1374` (`fs_pbr`: `if dot(N, V) < 0.0 { N = -N; }`) | Present — and it is exactly why backlit petals go dark: after the flip, a petal facing the camera away from the sun has N·L = 0. |
| Glass transmission (KHR_materials_transmission + volume) | `render_scene.wgsl:1032-1153` (`sample_transmission`/`transmission_diffuse`), composition `:1776-1786` | SHIPPED (GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2/E6): screen-space refraction + Beer–Lambert, REPLACES the diffuse response. Wrong physics for petals — this is the specular/refraction lobe for vases and windows, not diffuse transmission. |
| Diffuse transmission (KHR_materials_diffuse_transmission) | docs/GLTF_MATERIAL_EXTENSIONS_DESIGN.md section 5 (Deferred); not parsed (`rg diffuse_transmission crates/` → zero code hits; BUG-213 (unparsed-extension reporting) tracks even the missing report line) | The Khronos lobe petals actually want. Its deferral trigger fires here: this design IS the follow-up phase that doc named. D10 (frozen Khronos PBR set) is not violated — this is a shipped-family Khronos extension, not a new material system. |
| Per-caster RT shadow visibility | `raytrace.rs:1304-1346` (sv loop), consumed by `render_scene.wgsl:635-665` (`shadow_factor` — RT branch `:647-650` reads `rt_shadow_mask`, one channel per caster slot) | The term TL-B extends. Binary today: blocked or not. |
| Alpha-aware walk | `raytrace.rs:871-898` (`walk_with_alpha_test`), 5 call sites | T2-A's mechanism. A ray at a cutout texel already has three relevant outcomes available: below-cutoff → pass through; accepted → block. TL adds the third state: accepted on a translucent object → attenuate and continue. **Cost-critical observation:** for alpha-mask foliage the hardware early-out is already lost (`encode_blas_build`'s `setOpaque(!alpha_mask)`, comment `raytrace.rs:861`) — shadow rays through petal clusters ALREADY iterate candidate lists. Transmission is an increment on a walk that exists, not a new walk. |
| Per-object material table | `GiMaterial` (`raytrace.rs:602-606`, 48 B, size-asserted; populated `render_scene.rs:3976`) | Grows one vec4 (translucency factor) — the T1-B/T2-A/R1 extend-the-table precedent, fourth extension. |
| Sun-bounce caster loop | `sun_bounce_at_hit` (`raytrace.rs:989-1019`, shadow-class ray at `:1014`) | Used by the GI gather and reflection hit shading. Its shadow rays are shadow-class — they transmit under the same rule (TL4). |
| March sun visibility under RT | `shaft_march.wgsl:85` (binding 10 `rt_shadow_mask`), `:189-194` (one lookup per pixel, surface-depth visibility, documented approximation) | D5's landed half of "RT volumetrics". Reads `out_sv.r` — TL-B's transmitted mask flows into it unchanged (the free dividend). |
| Volumetric march | `shaft_march.wgsl` whole; steps 16/24/32 (`u.misc.x`), per-light per-step `shadow_vis` taps | Feature B's substrate. All visibility today is texture taps — zero rays. |
| Accumulation/denoise chain | `render_scene.rs:4057/4071/4120/4175`; sv is temporally accumulated (D-64 addendum, "sv gate") | TL-C's one new texture rides this chain stage-for-stage (the `out_refl` precedent, section 9.3). |
| Importer material params | `gltf_import/materials.rs:339-350` (`transmission`, `volume_thickness`, attenuation params already mapped); texture table `:193-203` | The `diffuse_transmission` factor mapping goes here. |
| Uniform free slots | `render_scene.wgsl:120-211`; `render_scene.rs:3705/3708` | **None free at material scope.** The E1 block was sized for five families; both reserved `w` slots are spent as texture-present flags (E6). One new vec4, E1/D2-style with size asserts (TL7). |
| Photoscanned flower assets carry transmission data? | Importer survey above + asset class knowledge | **No.** AlphaMode MASK + baseColor, no extensions. The factor's source for Peter's scenes is a material card param he dials (TL3) — same as every other look decision on a scan. |

### 16.2 Decisions (TL-numbered)

- **TL1 — the thin-surface model is wrap-diffuse around the backward normal,
  albedo-tinted, one constant factor.** Committed term, per light, in
  `fs_pbr`'s light loop (the `direct_sheen` parallel-accumulator precedent,
  `render_scene.wgsl:1498/1577`):

  ```
  // N is already flipped toward V (:1372); the transmitted term fires exactly
  // where the front term can't — light arriving at the BACK of the surface.
  // wrap (named constant RT_TRANSMISSION_WRAP, default 0.5, range 0–1) softens
  // the terminator so petals glow at wide angles, not just dead-on.
  back_l = saturate((dot(-N, L) + wrap) / (1.0 + wrap));
  direct_translucent += factor * albedo.rgb / PI * l_col.rgb * back_l * l_dir.w * vis;
  ```

  `vis` is the SAME per-light `shadow_factor` every other term uses — RT sv
  mask when RT is on, shadow map otherwise. **Why this model:** it is the
  standard two-sided foliage approximation, and the alternatives fail on the
  asset class. Rejected: full KHR diffuse-transmission BTDF with thickness +
  Beer–Lambert attenuation — Peter's scans carry no thickness maps and no
  volume data; a constant factor IS the constant-thickness case, and buying
  the full BTDF buys parameters nothing can fill. Rejected: reuse of the E2
  glass path (`transmission_diffuse`) — refraction of the background is the
  wrong physics for an opaque-backed petal, and it replaces rather than adds.
  Thickness attenuation exists as the factor itself (thin petal ~0.3–0.7,
  fleshy leaf lower — Peter's dial).
- **TL2 — the forward term runs in `fs_pbr`, zero rays, both paths.** It is an
  analytic function of data the fragment already has (N, L, albedo, vis).
  ED3a discipline (section 14.2): PBR-only; cel/phong get nothing. Default
  factor 0.0 → the accumulator adds exactly zero → byte-identical output, the
  house zero-default contract, machine-checked (I-TL1). On the raster path
  (RT off) the petal glow still works (the petal is thinner than the shadow
  bias, so its own back face reads lit); what RT-off does NOT get is the
  transmitted pool on the floor (shadow maps carry no transmission) — honest
  cost, stated: the floor-pool half is RT-only.
- **TL3 — the parameter is one new `pbr_material` scalar `translucency`
  (default 0), port-shadowed and cardable; the importer maps
  KHR_materials_diffuse_transmission's factor into it; nothing auto-defaults.**
  Three sources, in precedence: the extension's `diffuseTransmissionFactor`
  when an asset carries it (parse added — BUG-213's family gets its factor
  leg); Peter's dial on the card for scans that carry nothing; 0 otherwise.
  Rejected: defaulting Mask-mode materials to a nonzero translucency — changes
  every existing cutout scene's pixels silently, the exact class the
  byte-identical discipline exists to kill. Rejected: a scene-level global —
  a stone vase and a petal in the same scene want different values, and the
  material card is already where Peter sets roughness per asset. The
  extension's color factor/texture is a named fidelity gap (section 16.8):
  tint = albedo, which is physically right for foliage (a petal transmits its
  own color) and wrong for the three Khronos conformance assets — they become
  the held-out demo, not the gate.
- **TL4 — visibility rays transmit; geometry rays block.** A new walk variant
  `walk_with_transmission` (alongside `walk_with_alpha_test`, sharing its
  candidate loop): below-cutoff texel → pass (unchanged); accepted hit on an
  object whose `GiMaterial.translucency > 0` → `tint *= translucency *
  albedo_at_hit`, continue; accepted hit otherwise → block (unchanged).
  Bounded: `RT_TRANSMISSION_MAX_HITS = 8` (named constant, range 4–16) and an
  early-out when `luma(tint) < 1/256`. Called from exactly two sites: the sv
  caster loop (`raytrace.rs:1341`) and `sun_bounce_at_hit` (`:1014`). AO, GI,
  reflection, and primary-visibility rays keep the binary walk — a leaf still
  occludes bounce light and mirrors; transmitting those would be paying
  walk-extension cost on every ray class for an effect order-of-magnitude
  below the direct-light one. Machine check: I-TL4 pins the call-site count.
  The albedo sample at an accepted translucent hit reuses the alpha walk's
  base-color texture (same texture, same UV — cache-hot for Mask foliage,
  which already samples it for alpha).
- **TL5 — grey luminance in `out_sv`, color in ONE new texture for the first
  sun caster.** Channel arithmetic: `out_sv`'s four channels are the four
  caster slots; per-caster rgb tint needs 12 channels the texture does not
  have, and 4-caster colored tint is three textures through every
  accumulation stage — rejected as disproportionate. Instead: (a) every sv
  channel carries `luma(tint)` — point casters and the march
  (`shaft_march.wgsl:193` reads `.r`) work unchanged, grey transmission
  everywhere; (b) ONE new trace-res `Rgba16Float` texture `out_svt` carries
  the rgb tint of the first kind==0 caster (CPU picks the slot, passes it in
  `ShadowRayParams`, mirrors it to the raster in the reserved `rt_flags.z` as
  slot+1, 0 = none); `fs_pbr` substitutes `vis_rgb = textureLoad(out_svt)`
  for that light only. A second sun falls back to its luma channel — named
  honest cost, revival trigger in section 16.8. Hero-scene shape (one sun) is
  exact; the general case degrades gracefully. `out_svt` rides the existing
  upsample → à-trous → accumulate chain and the SAME
  `TemporalResetDetector` (I-R2's negative `rg` discipline extends).
- **TL6 — translucent objects leave the hardware opaque fast path.**
  `encode_blas_build`'s `setOpaque(!alpha_mask)` becomes
  `setOpaque(!(alpha_mask || translucency > 0))`, keyed through the same
  dirty-key path the alpha flag already rides, under D17's async-build
  discipline (no mid-frame builds; the bounded raster-presenting transition
  covers a live factor flip from 0 — same gesture as toggling RT itself).
  Without this the walk never sees solid-but-thin leaves as candidates.
- **TL7 — one new material uniform vec4 `diffuse_transmission_params`** (x =
  factor; yzw reserved, first consumer = the future color-texture flag).
  Smallest possible E1/D2-style growth, `RenderSceneUniforms` size asserts
  updated, WGSL mirror documented. No per-phase growth — this is the only one.
- **TL8 — nothing new accumulates; the sv signal gets smoother, not noisier.**
  TL-A is analytic over the already-accumulated sv mask. TL-B/C replace binary
  cutout-edge visibility with partial tints — strictly lower variance at leaf
  boundaries (the binary in/out flicker at canopy edges becomes a smooth
  gradient), so the existing sv change-gate and history discipline (D-64 and
  its addendum) apply unchanged, bands untouched. `out_svt` accumulates with
  the same weights and the same reset path as the irradiance texture
  (**blend decision AMENDED by section 10 (RT output & transition contract)'s
  gesture-rule addendum, 2026-08-07:** svt snaps on geometry cues and holds
  through strobes; the reset path stays shared). **No
  tuning constants join the untuned set beyond TL1's wrap and TL4's hit cap**;
  Peter's look is the quality gate, per the standing D19/D20 lesson.
- **TL9 — D9 (Vulkan seam) holds by construction.** The Metal RT trait grows
  no new method: `ShadowRayParams` gains fields and `dispatch_shadow_rays`
  gains the `out_svt` texture argument — exactly RD10's shape (section 9.2).
  The candidate-continuation walk is `rayQueryProceed`-shaped; Vulkan ray
  queries express the same loop. No Apple types above `manifold-gpu`
  (standing I-R5 negative `rg` at every gate).

### 16.3 Architecture

Three pieces, each extending an existing mechanism:

**Forward term (TL-A).** `render_scene.wgsl`, `fs_pbr` only: a
`direct_translucent` accumulator in the light loop (TL1's three lines) and one
addition at the composition site (`base_rgb = base_rgb + direct_translucent`
before the glass block at `:1776` — the glass `transmission_factor` and the new
`translucency` are independent lobes; a material with both gets both, matching
the Khronos layering where diffuse-transmission and specular-transmission
coexist). Population: `pbr_material` param → uniform (TL7's slot) at the
existing material-uniform fold; importer mapping in
`gltf_import/materials.rs` next to `:339`.

**Transmitting walk (TL-B).** `raytrace.rs`: `GiMaterial` 48 → 64 B (factor +
pad; MSL/Rust mirror discipline, size asserts both sides — P0's packed_float3
lesson); `walk_with_transmission` next to `walk_with_alpha_test`; two call
sites switched (TL4); sv write carries `luma(tint)` instead of binary.
Population of the factor at `render_scene.rs:3976` from the same material
uniform the raster reads — one source of truth, no second param path.

**Colored sun tint (TL-C).** `out_svt` allocated and lifecycle-managed by the
same `ensure_rt_irradiance` pattern as `out_refl` (section 9.3); written only
in the sv loop for the designated sun slot; upsample/à-trous/accumulate gain
the texture set exactly as they gained `out_refl`; `fs_pbr` substitutes per
TL5 (one `textureLoad` behind `rt_flags.z > 0.5 && slot match`, dummy 1×1
bound otherwise — ABI-stub discipline).

### 16.4 Cost model — rays/pixel/frame against 41.6 ms

Current full-RT frame: ~26 ms (section 13's number). Budget: 41.6 ms. Headroom:
~15 ms, minus what show content (layers/effects/encode) actually eats — the
section 7 frame-budget-sharing caveat applies; these numbers are RT-solo.

Trace resolution today: half-res of render res — 2.07M trace px native,
0.92M under T2-B temporal. Rays per trace px today: ~20–25 (1 primary + spp ×
casters + 4 AO + 2×2 GI + 8 refl + GI sun-bounce casts).

| Feature | New rays | Other cost | Estimate |
|---|---|---|---|
| TL-A forward term | **0** | ~4 ALU × lights in `fs_pbr`; one uniform vec4 | unmeasurable; gate is the byte-identity + a standard trace run |
| TL-B transmitting walk | **0** | shadow-class walks extend through ≤8 translucent hits; one albedo sample per accepted translucent hit (cache-hot for Mask foliage, which already samples for alpha); solid-translucent objects lose the BLAS opaque fast path (T2-A's foliage already did) | worst case = dense canopy against the sun, every shadow/sun-bounce ray walking to the cap; **MEASURED 2026-08-07: delta −0.23 ms (noise) at 3840×2160 on the apricot, translucency 0.5 asset-baked on all 4 materials; both legs ~40.6 ms median — above the 32 ms line on the pre-TL-B baseline already (RS-A 8-caster + RS-C sampler era, not section 13's 26 ms world) — VOID: that "baseline" was TL-B code at factor-0, see the correction below** |
| TL-C colored tint | **0** | +1 `Rgba16Float` trace-res texture through upsample/à-trous/accumulate (+~25% chain bandwidth: 4 textures → 5); one `textureLoad` per sun light in `fs_pbr` | **MEASURED 2026-08-07: +1.1 ms at 3840×2160 on the apricot (43.1 → 44.2 ms median, paused 120-frame legs); the same landing's sv2 slot-map repair (BUG-p14x (RS-A sv2 slot-map omission)) accounts for the larger +3.2 ms step from the 39.8 ms control — RS-A's intended chain work finally running** |
| B (deferred) | sun-only every-4th-step: +8 rays/march-px ≈ **+37% of the whole trace class**; all-lights every step: 128 rays/px ≈ 6× the trace class — rejected outright | plus a new march-kernel binding and validation path | see section 16.6 |

**CORRECTED 2026-08-08 by the BUG-2tb7 (frame drift) bisect** (RtApricot,
translucency off, A3a protocol, sun-only snaps): the RS-C-era tip measures
27.8 ms, NOT ~40.6 — the 08-07 "pre-TL-B baseline" legs were the TL-B code
at factor-0, misread as the baseline. True attribution: TL-B +11.4 ms,
TL-C +4.3 ms (their sum IS the whole 27.5→43.9 drift; RS-A/RS-B/RS-C flat).
TL-B's +11.4 is paid at translucency 0 — codegen/occupancy overhead
(GiMaterial 48→64 B, a device `GiMaterial*` threaded through the hottest
loop, a per-candidate translucency deref), NOT extra traversal; the
transmitting walk behaves bit-identically on binary scenes. The TL-C
+4.3 vs the landing's +1.1 is a protocol difference (A3a sun-only snaps vs
paused legs) — owed reconciliation at DN-I. **Done (e719a158):** two PSO variants via `MTLFunctionConstantValues`
(`HAS_TRANSLUCENCY` index 100), binary dispatch selected at runtime from
`rt_has_translucency`. Binary RtApricot 4K sun-only: 39.4 -> 32.34 ms
median; translucent leg 30.95 ms. Gate 6/6.

The recommendation's shape: A+B+C together add zero rays and one texture; the
only real risk line is the walk extension, and it is capped, measured, and
foliage-local. If TL-B's measured delta lands the fixture frame above 32 ms
(leaving ~10 ms for show content), TL-C pauses and Peter re-judges — that
number is in TL-B's gate.

### 16.5 Invariants & enforcement

- **I-TL1 — factor 0 is byte-identical.** `graph-tool render` of an RT compare
  fixture at pre/post TL-A commits, `cmp`-identical (T2-B/I-MB1 precedent —
  never a code-diff argument).
- **I-TL2 — `diffuse_transmission_params` has exactly one consumer.** `rg` —
  one read site in `render_scene.wgsl` (the forward term) plus its population.
- **I-TL3 — `out_svt` has exactly one consumer.** `rg -c` — declaration plus
  exactly one `textureLoad` in `render_scene.wgsl` (I-R3's shape).
- **I-TL4 — geometry rays never transmit.** `rg` — `walk_with_transmission`
  called from exactly two sites (sv loop, `sun_bounce_at_hit`);
  `walk_with_alpha_test` keeps its remaining call sites (AO, GI, reflection,
  primary) untouched.
- **I-TL5 — one temporal-reset path.** Negative `rg` — zero additional
  `TemporalResetDetector` constructions (I-R2 extended).
- **I-TL6 — BLAS opacity tracks translucency.** Unit test on the opacity
  decision (`alpha_mask || translucency > 0` → non-opaque), the
  `wants_shafts_gate` precedent.
- **I-TL7 — no Apple types above `manifold-gpu`.** Standing negative `rg`.

### 16.6 Feature B — volumetric participation: scope, price, revival trigger

**What exists.** The march (`shaft_march.wgsl`) is half-res, 16–32 steps,
per-light per-step shadow-map taps, and — under RT — ONE sun-visibility lookup
per pixel from the surface sv mask (`:189-194`), documented as an
approximation: the mask holds the SURFACE's visibility, so beams cannot form
from occluders between the camera and the surface. Canopy god-rays — shafts
through leaf gaps, hanging in the haze in front of the flower — are precisely
the missing case.

**Scope of the real thing:** per-step traced visibility in the march. Cheapest
honest variant: sun only, one ray every 4th step (8 rays per march px, half
res) ≈ +16.6M rays/frame against the current ~45M — +37% of the trace class,
for one light's beams. All lights, every step: 128 rays/march-px — six times
the entire trace budget, rejected outright, not deferred.

**Verdict: defer.** The trigger that revives it: (a) the post-pipeline
ray-budget re-judge (Peter's standing deferral) shows the temporal-upscaled
config (rays at 1/3 native) with headroom, AND (b) a staged look wants canopy
god-rays specifically — the alpha-cutout volumetric-shadows item in
VOLUMETRIC_LIGHT_DESIGN.md section 6 (Deferred) is the same want and joins
this trigger. When revived, the shape is the sun-only quarter-step variant
above plus section 15 (many-light) if it has landed, since many-light beam
carving is the same per-step visibility problem.

**The free dividend, landed with A:** TL-B's transmitted sv mask flows into
the march's existing `rt_sun_vis` read with zero new work — haze under a
backlit canopy carries the dappled, tinted brightness of the transmitted mask
at the surface depth. Not the full look; a real improvement; costs nothing.

### 16.7 Phases (one lane brief each, sequential — all touch the scene pass)

**No PNG oracles for agents** (the wave rule): every gate below is a computed
number or exit code; PNGs are Peter's morning look only.

#### TL-A — forward thin-transmission term + param + importer parse

- *Entry:* re-verify `render_scene.wgsl:1372` (N-flip), `:1498` (sheen
  accumulator), `:1776` (composition site), `render_scene.rs:3705/3708` (both
  reserved `w` slots still spent — a freed slot changes TL7). A moved anchor
  is an escalation, not a guess.
- *Read-back:* this section whole; GLTF_MATERIAL_EXTENSIONS_DESIGN.md section
  5 (the deferred item this revives); the `direct_sheen` accumulator and its
  byte-identical discipline; `gltf_import/materials.rs:322-350`.
- *Deliverables:* TL7's uniform vec4 (+ size asserts, WGSL mirror comment);
  `pbr_material` `translucency` param (default 0, port-shadowed, cardable);
  TL1's accumulator + composition; importer parse of
  `KHR_materials_diffuse_transmission` factor → param (color/texture gap
  logged per BUG-213's pattern); I-TL1/I-TL2 checks by name.
- *Gate:* (a) value test on the `rt_p1_region_probe` computed-pixel harness —
  quad, sun dead behind, factor 0.5, wrap 0.5: region mean within epsilon of
  the CPU-computed wrap term; **control legs, mandatory:** factor 0 reads the
  pre-term value; sun in FRONT reads no transmitted contribution. (b) I-TL1's
  byte diff. (c) round-trip: save → reload → probe passes. (d) importer unit
  test: a diffuse-transmission fixture parses factor → uniform. (e) clippy
  `-p manifold-renderer`; `cargo test -p manifold-renderer --features
  gpu-proofs` (`cargo test`, never nextest — the shader is touched).
- *Performer gesture:* `translucency` on a card fader swept live — gate
  drives the param, asserts monotonic region-luminance response.
- *Forbidden moves:* defaulting Mask materials nonzero (TL3); touching the
  glass transmission block's math; adding the color/texture fidelity
  (section 16.8); a second consumer of the uniform slot.
- *Demo (Peter only):* backlit-flower PNG pair on a real scan, factor 0 vs
  0.5 — **L2. The "does a petal glow like a petal" verdict is Peter's look.**
  Held-out render: `DiffuseTransmissionPlant.glb` (the conformance asset this
  partially un-defers) in the landing report.

#### TL-B — transmitting shadow walk (grey in sv, rgb inside) — **LANDED 2026-08-07 (wave/rt-translucency-b)**

Landing notes: the walk's terminal committed-type check is load-bearing (an opaque-BLAS object auto-commits in hardware with no candidate delivered — the first revision dropped it and read opaque-blocked rays as fully lit; the factor-0 control leg caught it). The 0→0.5 live flip is verification debt (the fixture's serialized store predates TL-A and never materialized the param — BUG-079 (preset-template-unresolved placeholder reconcile)); measurement used asset-baked translucency (KHR extension on a glb copy), and the D17 transition was exercised via the RT-toggle proxy (bounded, +5 ms over two frames at 4K, no hang). One-off GPU hang sighting in the gesture run logged as BUG-09ut (generators-CB hang sighting).

- *Entry:* TL-A landed; re-verify `raytrace.rs:871-898` (walk), `:1304-1346`
  (sv loop), `:989-1019` (sun-bounce), `GiMaterial` still 48 B.
- *Read-back:* TL4/TL5/TL6; T2-A's commit `62244989` (the walk and its
  candidate-loop gotchas — `commit_triangle_intersection`, not
  `accept_intersection`); the BUG-309 (night-wave trace bug) and
  BUG-8p1h (lights-out leak) bias discipline (a transmitting walk crosses
  more surfaces at more angles; the bias rules are load-bearing).
- *Deliverables:* `GiMaterial` 64 B + population; `walk_with_transmission`
  with the cap and early-out; two call sites switched; sv writes `luma(tint)`;
  TL6's BLAS opacity change + key; I-TL4/I-TL5/I-TL6 checks by name.
- *Gate:* (a) value tests on the region-probe harness: single translucent
  occluder (factor 0.5, known albedo) between sun and floor — floor region
  mean within epsilon of CPU-computed `luma(tint)` × lit value; **control
  legs:** factor 0 → full shadow; opaque occluder → unchanged; (b) stack test
  — two stacked 0.5 petals → 0.25 transmitted, exact; (c) cutout regression —
  below-cutoff texels still pass through an alpha-mask translucent object
  (T2-A's proofs stay green); (d) **the number:** `trace_ms` delta TL-B on vs
  off on the apricot scan, in the phase report, against the 32 ms line in
  section 16.4; `MANIFOLD_RENDER_TRACE=1`, no frame >20 ms; (e) gpu-proofs.
- *Performer gesture:* flip a hero flower's `translucency` 0 → 0.5 mid-set —
  D17's bounded transition, no frame >20 ms across it.
- *Forbidden moves:* transmitting AO/GI/reflection/primary rays (TL4);
  widening `MAX_RT_MATERIAL_TEXTURES`; a second reset path; retuning any
  existing constant; claiming the cost from code reading instead of the
  measured delta.
- *Demo (Peter only):* canopy-shadow PNG pair (hard black vs soft grey
  dapple) on a real scan — L2.

#### TL-C — colored sun tint through the chain — **LANDED 2026-08-07 (wave/rt-translucency-c)**

Landing notes: the lane-found slot-map repair is the wave's biggest catch — the four RT compute pipelines' slot maps were never extended for RS-A's sv2 bindings, and `GpuEncoder` silently skips any binding missing from the map, so caster slots 4-7 were inert under RT since RS-A (probe-proven: entries out → the TL-C value proofs fail 3/4 while rt_6caster_shadow stays green via the shadow-map fallback, i.e. it never exercised the RT sv2 chain). Logged as BUG-p14x (RS-A sv2 slot-map omission), fixed by the entries this wave added alongside svt's. Frame-time at 3840×2160 on the apricot (paused, 120-frame medians, same-day controls): pre-TL-C 39.8 ms → sv2-repair-only 43.1 ms → full TL-C 44.2 ms — TL-C's own out_svt cost ≈ +1.1 ms; the other +3.2 ms is RS-A's sv2 chain work finally running. Section 16.4's "small" holds for TL-C itself. The pink-pool demo pair is Peter's L2 look, pending.

- *Entry:* TL-B landed, its `trace_ms` delta inside the section 16.4 line (or
  Peter's explicit go above it).
- *Read-back:* TL5; section 9.3's `out_refl` plumbing (the template this
  copies stage-for-stage); section 10's transition contract (seed-don't-clear
  applies to the new history).
- *Deliverables:* `out_svt` + dispatch/param/flag plumbing (TL5/TL9); chain
  stages extended; `fs_pbr` substitution for the designated sun slot; I-TL3
  by name.
- *Gate:* (a) value test — red petal (albedo 1,0.1,0.1, factor 0.6): floor
  pool's rgb ratio matches CPU-computed tint within epsilon; **control
  legs:** point caster keeps luma discipline (its rgb contribution equals
  white × its sv channel); no-sun scene binds the dummy and reads white;
  (b) cut-reset numeric oracle on the tint history (P2's oracle shape);
  (c) I-TL3, I-TL5 negative `rg`s; (d) frame-time gate re-run; gpu-proofs.
- *Performer gesture:* sun color driven by a beat envelope through a flower —
  the floor pool pulses tinted; gate drives the color and asserts the pool
  region tracks.
- *Forbidden moves:* a general per-caster tint texture set (TL5's rejection);
  consuming `out_svt` anywhere but the one site; touching point-caster
  shading.
- *Demo (Peter only):* the pink-pool-under-flower PNG pair — L2, and the
  wave's close-out look.

**Phasing-completeness check:** TL1–TL3 (TL-A), TL4/TL6 (TL-B), TL5 (TL-C),
TL7 (TL-A), TL8 (all phases' gates), TL9 (TL-B/TL-C deliverables). Deferred
with triggers: section 16.8. B: section 16.6, not in a phase.

### 16.8 Deferred (with revival triggers)

- **KHR diffuse-transmission color factor/texture fidelity** — trigger: Peter
  wants the three Khronos conformance assets green, or an asset whose
  transmission color is not its albedo shows up. Shape: yzw of TL7's vec4 +
  the E6 bitmask pattern.
- **Second-sun and point-caster colored tint** — trigger: a staged look with
  two suns, or colored canopy shadows from a point light, that luma visibly
  fails. Shape: one more chain texture per added caster, priced then.
- **Transmission in AO/GI/reflection rays** — trigger: Peter's look reports
  bounce light or reflections reading too blocked through foliage. Priced at
  walk-extension cost on every ray class — the trigger bar is high.
- **Per-step RT visibility in the volumetric march (feature B)** — section
  16.6: priced, trigger named there (budget re-judge headroom + a canopy
  god-ray look; absorbs the VOLUMETRIC_LIGHT_DESIGN.md deferred item named
  there).
- **Thickness-varying transmission (thickness maps)** — trigger: an asset
  class with real thickness data (scanning workflow change). The factor is
  the constant-thickness case until then.

## 17. ML denoising — MetalFX Temporal Denoised Scaler (APPROVED + BUILT 2026-08-08, Peter + K3; direction: "rays down + ML denoiser is the spine"; Tahoe floor accepted same day. DN-A…DN-I LANDED same day; DN6 answered by the sweep: fused upscale + default spp is the operating point — under ML denoise, MC spp stops being the stability bottleneck (edge reconstruction is; the flicker is the pre-existing T2-B class, not a denoiser regression — control leg proved it). 1:1 denoise is frozen but export-tier at 65 ms (BUG-iadf (1:1 cost anomaly): +30 ms unexplained). OWED: Peter's look (upscaled PNG pair + 148↔149 frame-flip), noise-gate re-baseline when the mode flips default, DN-J ceremony close-out)

Peter's report: RT still shimmers on a STILL scene — chrome-like metals on
the DamagedHelmet against an EXR sun, penumbra crawl on photoscan shadows —
plus the afterglow (section 17.1) and the frame at ~42 ms solo. The static
noise gate passes (apricot composite 0.07 mean) because the apricot's
material/lighting class makes the per-frame samples tame; the shimmer is
the accumulator's PERMANENT NOISE FLOOR made visible by wild samples. The
floor is structural: the running mean caps history (~40 frames) for
responsiveness, so every channel carries ~2.5% of per-frame Monte Carlo
noise forever. Raising the cap when quiet was considered and REJECTED by
Peter: the snap from a long-converged clean image to raw noisy motion is a
jarring cliff. The root problem is that our raw single frame is noisy —
so the fix class that changes the trade itself is a denoiser that makes
one frame presentable. That is also the cost answer: fewer rays + ML
denoise beats more rays + filter, which is the architecture Apple built
the API for ("cast fewer rays"), and it dissolves the
convergence-vs-responsiveness management (gates, alpha floors, motion
bands) instead of adding another layer of it.

### 17.1 Afterglow root cause (separate fix, this wave)

Reading the accumulate kernel found the afterglow mechanism without a
probe: on `cpu_lighting_changed` the irradiance/AO and reflection paths
snap n to 2 — 50% stale lighting on frame 0, 1/(k+2) decay, then a ~2
%/frame exponential tail once the alpha floor binds. ~2 s of visible
afterglow on every cued change, matching Peter's report exactly.
Snap-to-2's stated guard ("one wild sample cannot define the pixel")
applies only to the noisy per-texel gate verdict; a CPU-vouched change is
the same epistemic class as a cut, and cuts reset fully. Fix: CPU-vouched
lighting change → full snap (n=1) on irr/AO/refl; texel-fired and gesture
paths keep n=2; sv/svt hold by design, unchanged. This stays load-bearing
post-denoiser as the pre-Tahoe path and as the denoiser's input
conditioner.

### 17.2 API findings (verified 2026-08-08 on this rig, runtime probe — NOT doc-page claims)

macOS 26.6; `objc_copyClassList` after dlopen of MetalFX. The Metal 4
denoiser is **`MTLFXTemporalDenoisedScaler`** — fused temporal denoise +
upscale, exactly "denoising integrated into the upscaling process".
Bindings already exist in our locked `objc2-metal-fx` 0.3.2 — objc2
weak-links and resolves by `AnyClass::get`, so **no Xcode 26 toolchain is
needed** (installed Xcode is 16.4; headers irrelevant to us).

- **Reset is first-class**: the effect exposes a per-frame `reset`
  property AND caller-owned history (`initWithDevice:descriptor:history:`)
  AND a per-pixel `reactiveMaskTexture`. Our cut/cue/gesture signals (the
  engine-knows advantage) drive the denoiser directly — the advantage
  survives the swap; no pixel-guessing.
- **Inputs (ReLAX-class ray reconstruction)**: color, depth (reversed
  flag), motion (+ dilated option, jitterOffset, motionVectorScale),
  normal, roughness, diffuse albedo, specular albedo, specular
  hit-distance, exposure/preExposure (+autoExposure toggle), reactive
  mask, denoise-strength mask, transparency overlay, debug texture.
- **Dynamic input resolution**: `inputContentMinScale/MaxScale` +
  per-frame `inputContentWidth/Height` — 1:1 (native denoise, no upscale)
  to be verified at integration; the scaler family allows 1.0.
- `MPSSVGFDenoiser` also exists at runtime (older SVGF in MPS) —
  comparison point only, not the plan; our accumulator already fills that
  role pre-Tahoe.
- Availability gate pattern: `AnyClass::get(c"MTLFXTemporalDenoisedScalerDescriptor")`,
  same as `supports_spatial_scaling` in `metalfx.rs`.

### 17.3 Decisions (DN-numbered)

- **DN1 — Tahoe floor for the RT path (Peter, 2026-08-08).** The denoised
  path targets macOS 26+ only; pre-Tahoe keeps the current accumulator
  path AS-IS — a fallback, no longer the tuning target. This answers D8
  (min-OS) for RT (frame interpolation's Tahoe gate is the same decision,
  confirmed once).
- **DN2 — fused denoise+upscale on the composited RT output.** Feed the
  beauty (RT-applied scene color) at render res, denoise+upscale to
  output res in one effect — REPLACES T2-B's plain temporal scaler on RT
  scenes when enabled. If 1:1 is allowed, native-denoise-no-upscale is a
  quality mode. Per-term denoising of lighting buffers is the fallback
  study if the fused path disappoints (deferred, section 17.6).
- **DN3 — one reset path, extended not duplicated.** The shared
  `TemporalResetDetector` + lighting key + gesture flags (the section 13
  (temporal denoiser rebuild) / section 10 (RT output & transition
  contract) addendum signals) drive the scaler's `reset` property and the
  reactive mask. Strobes still do NOT reset (D3) — demodulation is the
  denoiser's problem now and it is trained for it.
- **DN4 — new G-buffer outputs on RT scenes (D14 widened).** Normals,
  roughness, diffuse+specular albedo become stored outputs when the
  denoiser is on (the denoiser's albedo inputs are how it preserves
  texture detail through denoise); specular hit-distance is a new texture
  written by the reflection pass (the value already exists — the
  virtual-image reprojection computes it). Non-denoiser RT scenes pay
  nothing (D14's opt-in shape holds).
- **DN5 — trait behind the existing upscaler seam, Vulkan-parity shaped.**
  Interface = { color, depth, motion, normal, roughness, albedo×2,
  specular hit-distance, reset, reactive mask } — no MetalFX-specific
  knobs. Vulkan parity when the backend builds: NRD/ReLAX consumes the
  same input set (ray queries + these G-buffers all port). The
  architecture (few rays + ML denoise, engine-driven resets) is decided
  once, here, and holds for both backends.
- **DN6 — the operating point is a sweep, not a guess (Peter: "we can
  feed it a pretty huge amount of ray data").** Phase DN-I runs the
  spp × denoiser matrix per scene class — frame ms, noise-gate deltas,
  PNG pairs — over refl 1–8 / GI 1–4 / AO 1–4 spp. Shipping constants
  come from the matrix + Peter's look, not from this doc.
- **DN7 — afterglow fix (section 17.1) ships regardless** — it fixes the
  pre-Tahoe path Peter runs today and conditions the denoiser's input.

### 17.4 Invariants & enforcement

- **I-DN1 — pre-Tahoe path byte-identical.** Availability gate off →
  zero diff in the accumulation/scaling path; `graph-tool render` cmp on
  an RT fixture (I-TL1 precedent).
- **I-DN2 — one reset path.** Negative `rg`: zero additional
  `TemporalResetDetector` constructions; the scaler's `reset` is driven
  from the existing signals only (I-TL5/I-R2 extended).
- **I-DN3 — no Apple types above `manifold-gpu`.** Standing negative `rg`.
- **I-DN4 — no stale history across a cut.** Scripted pixel-diff proof
  (P2's oracle shape): cut+1 frame vs cold-start render of the target —
  mean abs diff < stated epsilon, with the denoiser ENABLED.
- **I-DN5 — the noise gate re-baselines, never loosens.** Ceilings
  re-recorded (`--record`) against the denoised path at landing; a silent
  channel still FAILS as inert.

### 17.5 Phases (dispatched items named; spine briefs written per-phase as usual)

- **DN-A — instruments (dispatched 2026-08-08).** BUG-drcb (capture garbage)
  and BUG-665r (wedge culprit unlogged). Everything after is
  measurement-driven; instruments first.
- **DN-B — afterglow snap (dispatched 2026-08-08).** Section 17.1 (afterglow
  root cause), one commit, byte-identity when the flag is clear; lead runs
  gpu-proofs at review (device contention).
- **DN-C — stale-accel OOB (dispatched 2026-08-08).** BUG-rmmv (stale accel)
  — root-cause fix in the deferred-build/trace generation seam.
- **DN-D — drift bisect (dispatched 2026-08-08).** BUG-2tb7 (frame drift)
  — per-landing ms attribution 2026-08-02→07; feeds DN-I's matrix.
- **DN-E — G-buffer widening (DN4).** Normals/roughness/albedo×2 outputs
  on RT scenes + specular hit-distance texture from the reflection pass.
  Gate: value tests per output against CPU-computed expected; byte-
  identity with the denoiser off.
- **DN-F — denoiser trait + Metal impl (DN5/DN2).** `manifold-gpu`
  upscaler seam extended; availability gate; scaler constructed with
  caller-owned history. Gate: denoiser runs end-to-end on a synthetic
  noisy buffer and reduces error vs a CPU reference (value test, stated
  threshold); I-DN1/I-DN3 negative checks.
- **DN-G — feed wiring + reset path (DN3).** All DN4 textures bound;
  cut/cue/gesture → `reset` + reactive mask. Gate: I-DN4 cut proof WITH
  the denoiser on; strobe non-reset proof (P2's oracle, re-run on the new
  path); motion-vector honesty check (D-64's jitter lesson applies —
  verify what the denoiser expects for jittered vectors before wiring).
- **DN-H — integration on hero scenes.** Apricot + DamagedHelmet +
  puresky HDRI fixture; frame-ms table vs pre-denoiser path.
- **DN-I — operating-point sweep (lead runs, Peter's look gate).** DN6's
  matrix; output = shipping constants + quality mode defaults.
- **DN-J — landing.** Gate, full gpu-proofs, noise-gate re-baseline
  (I-DN5), supersession sweep, Peter's live look.

### 17.6 Deferred (with revival triggers)

- **Per-term denoising of lighting buffers** — trigger: the fused beauty
  path measurably blurs lighting detail the per-term path would keep.
- **MPSSVGFDenoiser comparison** — trigger: the ML path shows artifacts
  on our content class (strobes, void backgrounds) that a classical SVGF
  wouldn't; also the reference if Apple deprecates.
- **NRD/ReLAX Vulkan integration** — trigger: the Vulkan backend builds
  (DN5's input set is its integration contract).
- **Frame interpolation pairing** — D6 (per-output frame interpolation)
  unchanged; the Tahoe floor (DN1) removes its OS blocker, not its
  show-need trigger.

### 17.7 Metal 4 scaler migration + input conditioning (2026-08-09, Peter + K3)

Peter's live look at the fused path (DN2) as-fed: REJECTED — smeared,
detail loss. Two confounds mean the verdict isn't final: (1) the
denoiser's input is pre-smoothed by our own temporal accumulator
(double temporal filtering — the network is trained on raw noisy
frames); (2) the reactive mask is unwired (`render_scene.rs` passes
`None`), so emissives and movers trail. Peter's direction: straight
upgrade to the Metal 4 (`MTL4FX*`) scaler generation, no quality
probe — "they will be better than previous."

- **DN-K — Metal 4 scaler migration. LANDED 2026-08-09 on main.**
  `MTL4FXTemporalDenoisedScaler` (and `MTL4FXTemporalScaler` for the
  plain path) are the preferred implementations behind the DN5 seam,
  availability-gated with silent `MTLFX*` fallback. Typed objc2-metal
  0.3.2 MTL4 API (additive features; the classic command timeline is
  untouched). Per-device `MTL4Bridge`: one MTL4 queue + 3-allocator
  ring + shared event; GPU-side sync only, no CPU stalls; ring
  saturation skips the scaler for one frame (stale output) rather than
  resetting a live allocator. Value proof
  `m4_denoise_reduces_error_vs_clean_ramp` drives the bridge on
  hardware. **CORRECTION (DN-O, 2026-08-11): that proof silently
  skipped — the availability probe was broken, see DN-O. The "no
  explicit residency sets" claim here was wrong: Metal 4 requires
  them.**
- **DN-L — input conditioning. LANDED 2026-08-10 on main** (lead-built,
  lane/rt-mtl4-upscaler). When `rt_denoise_feed` engages
  (`denoise_active`), `ACCUM_FLAG_DENOISE_NEAR_RAW` (reset bit 5) drops
  every accumulator history cap to near-raw — alpha floor 0.25, n ≤ 4,
  specular floor included; flag-off frames byte-identical. Reactive
  mask wired: new `reactive_mask` output (R16Float, fifth denoise MRT
  feed) = emissive (luma > 1e-3) OR object-moved (`prev_model !=
  model`, camera motion excluded by construction); replaces the `None`
  at the denoiser encode. Gate: gpu-proof
  `denoise_near_raw_caps_history_at_four_frames` (control >12 vs
  capped ≤ 4.5, converged value unchanged), full gpu-proofs PASS,
  noise gate green. The emissive-strobe pixel proof folds into DN-N's
  re-look on the RtEmissiveStrength fixture.
  Also landed same branch: the MTL4 *temporal* upscaler is now wired
  into `MetalFxTemporalUpscaler` (DN-K's gpu-side class was built but
  unwired — `temporal_upscale` used classic MTLFX until this). Same
  DN5 seam, same ring-saturation skip semantics as the denoiser.
- **DN-M — emissive noise fixture. LANDED 2026-08-09 on main.**
  Peter saved `RtEmissiveStrength.manifold` (import preset over the
  Khronos `EmissiveStrengthTest.glb`, six stepped-emissive cubes);
  `scripts/rt_noise_gate.py` baselines are now per-fixture (schema 2).
  First measurement: still-scene emissive flicker is firefly-class —
  irr_full mean |delta| 3.083, p99.9 126.8 (8-bit levels) vs the
  apricot fixture's frozen flats.
- **DN-O — MTL4 activation fixes. LANDED 2026-08-11** (lead +
  probe lanes, lane/mtl4-probe-fix). The MTL4 path had never executed
  on-rig: the availability probe looked up `MTL4FX*` protocol names as
  classes (always nil → silent classic fallback); every "MTL4" run
  before this, including DN-K/DN-L's, was classic MTLFX. Fixed probe:
  `instancesRespondToSelector` for the MTL4 creation selector on the
  public descriptor. Three further Metal 4 requirements surfaced, each
  oracle-proven on Tahoe 26.6.1: (1) **MTL4 denoiser creation crashes
  Apple-side** (uncatchable SIGABRT inside MetalFX's graph compile;
  compiler path fine for temporal, legacy denoiser fine, combination
  aborts) — hard-off with `MANIFOLD_MTL4FX_DENOISER=1` re-test hatch,
  BUG-woji (MTL4 probe bug); classic MTLFX remains the denoise path,
  so DN-N's re-look still judges classic denoise quality. (2) **Barrier
  stages**: the MTL4FX effect requires `outputTextureBarrierStages`
  (validation assert; silent black without it); no public setter
  exists — KVC writes the ivar; color defaults to Dispatch.
  (3) **Explicit residency**: classic-created textures are invisible
  to MTL4-committed work without a residency set on the MTL4 queue —
  bridge-owned set, add-if-missing, commit-on-change, prune only when
  >64 allocations and no frames in flight. Gate: new gpu-proof
  `m4_temporal_scaler_encodes_one_frame` (create + encode + nonzero
  readback) green; the MTL4 temporal upscaler wired in DN-L now
  actually runs.
- **DN-N — re-look gate (Peter).** Fused path, conditioned, Metal 4
  networks. PASS → default-flip + effect-card button + noise-gate
  re-baseline land (default-on denoise implies temporal_upscale —
  the engage logic must never select 1:1 by default). FAIL → ML
  beauty denoise is dead for live; section 18 = lighting-only
  architecture (native 4K scene, RT lighting traced + denoised at
  reduced res, per section 17.6 per-term trigger, now fired).
