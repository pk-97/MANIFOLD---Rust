# Ray Tracing — hybrid RT lighting for hero scenes

**Status:** RT v1 WAVE LANDED on main 2026-07-23 (overnight run, D12; wave tip merged `519d01ee`+C4 `bff0fa15`, full gate green: nextest 3879, clippy+deny clean, gpu-proofs 77/77). SHIPPED: W0 stored G-buffer (D14, BUG-136 velocity math proved correct), P1 hard shadow rays (D16 forward integration; BUG-308 accel race + BUG-309 bias epsilon fixed en route, D17; BUG-310 tracer prewarm fixed at landing), P2 soft shadows + AO + demodulated temporal accumulation with D3/RT-D2 node-local resets, P3 emissive GI + sun-bounce + RT volumetrics (D4/D5), P4 MetalFX temporal SEAM-ONLY (scaler + shared TemporalResetDetector + jitter + toggle landed; the live reduced-res-render→upscale path into scene output is NOT wired — follow-on). 2026-07-23 Peter's first look: three integration bugs found+fixed same day (toggle visibility `6e44894e`, ambient-knob wiring `7b3d8dd2`, sun double-count `818a06b0`); verdict = v1 is a landed skeleton, not stage-ready — ghosting (BUG-311) + noise (BUG-312) + depth-derived normals are structural gaps. Tier 1 BUILT + LANDED 2026-07-23 (§8.1 D18–D20, overnight wave: real vertex normals, reprojected validity-tested accumulation, variance-guided à-trous + blue-noise; BUG-311/312 FIXED, BUG-316 resolved-as-oracle-confound (id 315 ceded to main at merge)). OPEN: Peter's L2 look = final motion/still verdict; Tier 2 wave IN FLIGHT (§8.2 D21 — alpha-aware rays, live MetalFX temporal wiring; ray-budget re-judge moves to after T2-B's upscaled config); Tier 3 per §8; P5 export (D13); P6 frame interp (Tahoe, D6/D8). · 2026-07-23 · Fable
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
- **D17 — Acceleration-structure builds are async-ordered, never synchronous mid-frame (ruled mid-wave 2026-07-22 night, Fable; BUG-308 root cause).** The bug class: a private command buffer with commit+wait mid-frame races the shared encoder's uncommitted mesh writes (accel built from stale vertex data, then cached forever by the dirty-key) AND stalls the frame. Banned outright on the RT path. Correct form: accel build/refit command buffers enqueue on the same queue AFTER the frame's pending shared-encoder work commits — Metal commit-order guarantees the data dependency with no wait; a completion handler flips an atomic accel-ready flag; until ready, an RT-enabled scene renders its existing raster shadow path (explicit, logged, ~7-frame transition at P0's 110–167ms build cost) and the mask path activates when ready. **Stage consequence:** toggling RT mid-set is a brief soft lighting transition with zero frame hitch — the inline alternative (threading the build through the frame encoder) was REJECTED because it turns first-enable into a 110ms+ frame. **Seam note:** any future GPU work that builds long-lived resources from GPU-written buffers (P2 denoiser history alloc, P3 emissive tables, mesh refit for sims) follows the same enqueue-after-commit + ready-flag pattern. **As built:** the lane satisfied D17 via defer-to-next-frame (build enqueued only once the accel key recurs unchanged, by which point the prior frame's mesh-gen has committed) — blessed; caveat: a deforming mesh whose vertices change WITHOUT a key change would get a one-frame-stale accel — attached to the already-flagged refit line item (§3), must be revisited by any sim/deform RT phase. Tracer pipeline construction prewarms in the node's first-evaluate window (BUG-310).
- **D16 — P1 integration: RT shadows ride the existing opaque depth prepass; forward stays forward (ruled mid-wave 2026-07-22 night, Fable; P1's escalation).** No deferred combine pass is built. `render_scene` already renders an opaque depth prepass (`opaque_depth_snapshot`) before its lighting pass — that is the mode-B slot. When `rt_enabled`: half-res shadow-ray dispatch after the prepass (origins from prepass depth + inverse view-proj; bias normals via screen-space reconstruction from depth — no normal G-buffer target in P1; P2 adds one only if bias artifacts or AO demand it), depth-aware upsample to native, and the forward lighting shader samples the mask as the light visibility factor in place of the shadow-map sample (one uniform-gated bool, not a pipeline permutation). Shadow maps stop rendering for RT scenes. **Seam note:** P2's soft shadows + AO join the SAME half-res dispatch and SAME upsample — this is the extension point, not a new pass.
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

## 8. v2 roadmap — from landed skeleton to stage-ready (captured 2026-07-23, Peter's first real look)

Peter's first in-app session with RT v1 (apricot scene) found three integration bugs — all
fixed same-day — and established that the remaining gaps are structural, not tuning. This
section is the durable brief for the next RT session.

**Fixed 2026-07-23 (context for the roadmap, not open work):**
- RT toggles invisible everywhere — `card_visible_for` had no `node.render_scene` arm (`6e44894e`).
- RT ambient floor unremovable — now rides the scene Ambient knob; knob 0 = true black (`7b3d8dd2`).
- Sun counted twice — the irradiance kernel carried its own sun*n·l*vis copy on top of the
  raster light loop's; irradiance is now ambient*ao + gi only (`818a06b0`). Post-fix probe:
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

**Executed 2026-07-23 (same night).** Commits: T1-A `124dbbb5` (oracles, both tripping pre-fix) → T1-B `10359365` (bindless per-object vertex-normal table, barycentric interpolation, depth-derivative path deleted) → T1-C `f9bc2b30` (reprojection + depth/normal validity + ping-pong history; camera-motion-only — no per-pixel object id for `prev_model`, animated objects fail validity and fall back to current-frame, recorded limitation) → `dadcfb68` (oracle revision) → T1-D `06340e17` (moments in `Rg32Float` ping-pong, 3-pass à-trous incl. normal-weighted upsample, R2 blue-noise for AO/GI rays).

- **D19/D20 — motion-ghost oracle rulings (Fable, mid-wave).** The T1-A ORBIT oracle was confounded (BUG-316, né 315 — id collision with main's stale-roughness bug: tracked point on the shadow boundary measures real parallax); rewritten to accumulated-vs-cold-start at same pose (D19), still non-discriminating even at ~10°/frame stimulus (D20). Terminator: one-shot instrumentation inside `accumulate_irradiance` proved the reprojection ACTIVE (95–98% of texels reproject to a shifted history texel, 97%+ pass validity) — BUG-311 accepted FIXED on that evidence; both motion oracles kept `#[ignore]`d with full investigation recorded in their doc comments. **Standing lesson: numeric pose/frame-diff metrics cannot isolate ghosting from legitimate accumulation lag at these alphas — motion-quality judgment on this surface is Peter's L2 look until someone designs a genuinely discriminating instrument (no third redesign inside a wave).**
- **T1-D honest residual:** STILL oracle improved 1.076e-4 → 8.6e-5 (threshold 7e-5) but the residual is proven scene structure (box-blur + 16× samples both no-ops), not speckle — kept `#[ignore]`d, threshold untouched. Ray budgets unchanged pending Peter's look.
- Lane-surfaced gotchas for future RT test authors: orbit tests must step `dt` with `time` or `TemporalResetDetector` hard-resets every frame; async accel builds need per-frame commit (batching warmup frames into one encoder breaks the RT-D4 state machine).

### 8.2 Tier 2 wave (dispatched 2026-07-23)

**D21 — Tier 2 executes as a second staged wave, same D12/D18 pattern (Peter 2026-07-23), on `wave/rt-t2`:**

- **T2-A — alpha-aware rays (§8 item 4).** Cutout materials stop shadowing as solid slabs. Mechanism: materials flagged alpha-mask get non-opaque intersection — the kernel's intersection query iterates candidate triangles, samples base-color alpha at the candidate's interpolated UV, and continues through texels below the material's alpha cutoff; opaque materials keep the `force_opacity(opaque)` fast path untouched (cost discipline — alpha-test only where flagged). Plumbing precedent: T1-B's bindless per-object table (normals) extends to UV + base-color-texture + cutoff per object. Applies to shadow, AO, and GI rays in the same pass — one mechanism, not three.
- **T2-B — live MetalFX temporal wiring (§8 item 5).** P4's seam (scaler, `TemporalResetDetector`, jitter, per-scene toggle) finally drives the real path: RT-enabled scene with temporal quality mode renders reduced-res and upscales into the scene output. Reuses W0 motion vectors and the D15/RT-D2 reset helper — negative `rg`: no second reset or jitter path. **Stage consequence:** this is the fps lever — same look, rays at a fraction of native res; the ray-budget re-judge (post-Tier-1 open item) happens at the upscaled config, not before.

Staging: T2-A → T2-B sequential (both touch `render_scene.rs`). Pre-allocated BUG range: 317–318 (315 lost to collision, 316 spent). Out of scope: Tier 3 features, ray-budget changes, deforming-mesh refit (stays attached to the §3/D17 sim line item), new `Arc<Mutex>`. Lane briefs: `.claude/orchestration/rt-t2-queue.md`.

**D22 — reduced-res render path for temporal upscale (Fable, mid-wave 2026-07-23; T2-B's park).** The T2-B lane correctly stopped: P4 landed only the seam (scaler type, jitter, toggle) — no reduced-res render path exists to wire it into. Ruled seams, in the existing design's spirit (P4 committed the per-scene mode; D2 mode C supplies the measured config):

1. **Path shape:** quality mode = temporal-upscaled → `render_scene` draws color + depth + velocity into internal scratch targets at render res = output res × `RT_TEMPORAL_RENDER_SCALE` (named constant, **1/1.5 linear** — P0 mode C's measured 1440p→4K config; Peter-amendable, not a lane knob). MetalFX temporal consumes color + depth + motion (+ P4's jitter) → native-res color = the scene's graph output. Native mode keeps today's direct draw path, byte-identical (machine-diff gated). Scratch targets follow `render_scene.rs`'s existing target-allocation pattern — zero new systems.
2. **`depth`/`velocity` graph outputs stay at render res when upscaled mode is on** — MetalFX doesn't upscale them and building a bespoke upscaler for them is FORBIDDEN. Documented limitation; revival trigger = a downstream consumer needing native-res depth from an upscaled RT scene.
3. **Not mode C's resurrection:** modes A/C stay dead as *defaults* (D11); this is P4's committed per-scene opt-in trade (fps lever for heavy scenes). The RT half-res ray pass now keys off render res (rays at ~1/3 native in upscaled mode) — that compounding is the point, and the ray-budget re-judge (post-wave) happens at this config.
