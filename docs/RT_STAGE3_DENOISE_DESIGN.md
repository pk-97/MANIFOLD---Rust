# RT Stage-3 Denoise — post-accumulation à-trous filter + pre-blur firefly clamp

**Status:** APPROVED design, not built · 2026-08-27 · k3 (lead) — overnight autonomous session per Peter's written brief (BUG-eytk (spatial à-trous denoiser) + BUG-mkgh (pre-blur firefly clamp)); closes the BUG-312 (RT ray noise speckle) lineage
**Prerequisites:** none (RT_QUALITY_SETTINGS P1–P3 landed; this design extends its grid by one row)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

The RT stack today has three of four denoising stages: (1) a firefly cap inside the trace kernel anchored to emissive mean power, (2) a pre-accumulation à-trous spatial filter on the raw per-frame signal (T1-D), (3) temporal accumulation with reprojection. **What does not exist is a spatial filter on the accumulated signal** — the "stage 3" hole. The permanent noise floor (RAYTRACING_DESIGN.md section 17 (ML denoising): the running mean caps at ~40 frames, so every channel carries ~2.5% of per-frame Monte Carlo noise forever) and the slow-convergence windows (motion, disocclusion, the gesture fade after any light change) go straight into the composite, and then DoF/bloom smear the surviving outliers into bright bokeh blobs (BUG-mkgh (pre-blur firefly clamp)). This design adds the missing post-accumulation filter and the output clamp. Both are pipeline-level: no project migration, new params carry defaults.

This work supersedes the BUG-312 (RT ray noise speckle) depth-only-bilateral upgrade note (RAYTRACING_DESIGN.md section 8 (v2 roadmap) Tier-1 item 3 / section 17 (ML denoising)): T1-D landed the *pre-accumulation* half of that note; this design is the post-accumulation half, and BUG-eytk (spatial à-trous denoiser) closing closes the BUG-312 lineage. The MetalFX denoiser (section 17 (ML denoising), hard-off per BUG-woji (MTL4 denoiser SIGABRT)) is out of scope; when it is live, these stages stand down (D6).

Companion docs: `docs/RAYTRACING_DESIGN.md` (the engine; section 13 (temporal denoiser rebuild) the accumulator, section 17 (ML denoising) the ML direction this feeds), `docs/RT_QUALITY_SETTINGS_DESIGN.md` (the quality grid this extends), `docs/MANIFOLD_GPU_ARCHITECTURE.md` (uniform/texture discipline).

## 1. Audit — what exists (verified 2026-08-27, worktree slot-1 at main tip 23a1cbfed)

| Piece | Where | State |
|---|---|---|
| Pre-accumulation à-trous (T1-D) | `crates/manifold-gpu/src/metal/raytrace.rs:2404` (`atrous_filter`); dispatched `render_scene.rs:5989-6041`, 2 dilated passes (steps 2,4) after `upsample_shadow` | filters sv/sv2/svt/irr/refl on the RAW frame, before accumulation — **not** the missing stage |
| Temporal accumulation | `raytrace.rs` `accumulate_irradiance`, dispatched `render_scene.rs:6171-6213`; ping-pong `rt_irr_history` (rgba16, `render_scene.rs:2547-2551`) | writes just-accumulated irr (.rgb) + history count (.a, section 13 (temporal denoiser rebuild)) + moments (m1/m2 luma, ao) |
| Variance guidance | `rt_moments_history` pair (**Rgba32Float**, render_scene.rs:2629-2631); read by `atrous_filter` as `mo.g − mo.r²` (raytrace.rs:2456-2458); `.w` = history count, `.b` = accumulated AO | exists, pre-accumulation scale |
| Composite consumes history | `render_scene.rs:6433` — `rt_irr_tex = rt_irr_history[rt_history_ping]` bound into the PBR forward pass | **the one rebinding seam the post-filter hangs on** |
| Trace-kernel firefly cap | `render_scene.rs:5609-5636` threads `emissive_table_mean_power`; cap lives in the trace kernel (RS-B) | per-sample cap only; outliers still reach the accumulated buffer (DN-M: RtEmissiveStrength irr_full p99.9 = 126.8 8-bit levels) |
| Blur-family effects | `assets/effect-presets/Bloom.json` (threshold→downsample→gaussian→mix), `DepthOfField.json` (CoC→variable-width gaussian→masked mix) | run downstream of `node.render_scene` output, on the HDR layer texture |
| Output redirect seam | `render_scene.rs:6311-6321` — `target` is `rt_temporal_color_scratch` when temporal-upscale/1:1-denoise is active, else `native_color` | precedent for redirecting the final color through a scratch before the tail |
| Quality grid | `crates/manifold-foundation/src/settings.rs` (`RtQualityTier`, `RtRayResolution`, `RtQualityColumn`, `RtQualitySettings`); resolved `RtQuality` in renderer; panel row machinery per RT_QUALITY_SETTINGS P3 | extend by one row (D4) |
| Reset machinery | `TemporalResetDetector` + lighting/geo gesture keys (`render_scene.rs:6072-6104`) | unchanged — the post-filter is derived state, holds no history, needs no reset path (D5) |
| `denoise_active` / DN-L near-raw | `render_scene.rs:6120-6123` (`with_denoise_near_raw`) | when MetalFX owns denoising, this design's stages bypass (D6) |

Section 2.5 audit findings (DECOMPOSING_GENERATORS.md section 2.5 (Precondition: audit by analogy)): **no new graph primitives.** Both stages are internal passes of the `node.render_scene` RT pipeline, same class as `atrous_filter` — MSL kernels in `manifold-gpu`'s raytrace.rs dispatched from `render_scene.rs`. The primitive survey (`rg 'purpose: "' .../primitives/`) shows no median/clamp/denoise atom, and none is wanted: a graph atom would only reach Bloom/DoF through preset edits, which never reach flattened graphs in saved projects (the no-migration constraint kills that shape — see D2's rejected alternative). Freeze-codegen classification, stated not elided: both kernels are multi-tap cross-pixel gathers — NOT barrier-free per-element, so they fail the ADDING_PRIMITIVES.md scope test on their face. As pipeline stages of a rasterizer-class primitive they sit outside the mandate entirely (precedent: `atrous_filter` itself). If a future graph atom wants à-trous, that atom would be BLOCKED on a tracked codegen gap (no offset-texel `InputAccess` kind) — named here, not pursued.

## 2. Decisions

- **D1 — The post-accumulation filter runs on the demodulated irradiance only.** Input: the just-written `rt_irr_history[write]` slot + `moments_write` + current-frame depth/normal. Output: a new persistent `rt_irr_filtered` texture the composite binds instead of the raw history slot. History and moments are never written by the filter — accumulation learns from the unfiltered signal (I2). Scope is irradiance only: sv has SV-ACCUM + hold machinery, refl has its own variance gain + Karis clamp (BUG-dx6w (specular history neighborhood clamp)/BUG-axe9 (tone-mapped variance clip)). Rejected: filtering all channels, because it doubles bandwidth for channels that already have dedicated machinery. Rejected: filtering the composited beauty, because demodulation is exactly what makes filtering safe — texture detail lives outside the irradiance term.
- **D2 — The firefly clamp runs on the node's final color, gated on RT-active, before the upscale/denoise tail.** One compute pass: 3×3 neighborhood luma median over non-void texels; `threshold = GAIN × max(median, FLOOR)`; if luma exceeds threshold, scale rgb down. It rides the `target` redirect precedent (render_scene.rs:6311-6321): when RT is active and the param is on, the forward resolve lands in a scratch and the clamp kernel writes scratch→target. Rejected: clamp atoms inside the Bloom/DepthOfField preset JSONs, because saved projects carry flattened graphs and never see preset edits — the fix would only reach newly-authored projects, and the no-migration constraint forbids that. Rejected: clamping the irradiance term only, because the bokeh-blob outliers include direct emissive surfaces, which never pass through the irradiance texture.
- **D3 — Both stages are new MSL kernels in `manifold-gpu`'s raytrace.rs, dispatched from `render_scene.rs`.** Same file, same dispatch style, same params-buffer discipline as `atrous_filter`/`AtrousParams` (raytrace.rs:4507-4545). No WGSL, no graph atoms, no new crate edges.
- **D4 — Quality wiring is one new row on the RT Quality grid: Spatial Denoise.** `RtQualityColumn` gains `#[serde(default)] pub spatial_denoise: RtSpatialDenoise` with `Off | Low | Medium | High` → (strength, iterations): Off = pass skipped; Low = (0.6, 2); Medium = (0.85, 3); High = (1.0, 4). Defaults: realtime Medium, export High. **Consequences, stated honestly:** this deliberately breaks RT_QUALITY_SETTINGS I1's byte-identical-defaults convention for the new field — old projects deserialize to Medium and their RT output changes (less boiling, slightly softer lighting transients). That is the point of the feature (Peter filed it P1; "defaults that keep old projects pixel-behavior sane" — sane, not identical). The firefly clamp likewise defaults ON via a `node.render_scene` bool param (`rt_firefly_clamp`, default true, cardable like the other rt_* toggles). Peter can turn either off per project/per scene.
- **D5 — The post-filter holds no history and adds no reset path.** `rt_irr_filtered` is derived per frame from post-accumulation state. On frames where accumulation didn't run, the filter idles and the composite binds the raw history slot (today's behavior). On reset/resize, accumulation snaps and the filter runs on the snapped frame — the temporal moments cold-start at zero variance there (raytrace.rs:2818), so the additive spatial-spread term (D7) is what carries cut/cue/gesture frames: a raw frame has high spatial spread, which widens sigma directly. Maximum smoothing when the image is rawest, no dependence on the moments having re-inflated.
- **D6 — When `denoise_active` (MetalFX owns denoising), both stages stand down.** DN-L already drops our history to near-raw for the network; pre-smoothing its input is the double-filter confound Peter rejected in section 17.7 (Metal 4 scaler migration + input conditioning). The MetalFX path is hard-off (BUG-woji (MTL4 denoiser SIGABRT)) so this gate is untestable tonight beyond a code-shape check; DN-N's re-look re-judges.
- **D7 — Variance estimate = temporal output variance + spatial spread.** Temporal term: `var_out = max(m2 − m1², 0) / n_eff` where m1/m2 ride `moments` (.r/.g) and **`n_eff` is the history count from `moments.w`** (verified 2026-08-27: ED2 moved the count off `history.a` — that channel is the accumulated AO end-to-end, raytrace.rs:2846-2849/3218-3224; the count is `moments_write.w`, clamped to min(n, 1/alpha_floor) = 50 at raytrace.rs:3089). The temporal term alone stands down exactly when the filter is needed (moments cold-start at zero variance on reset, and take frames to re-inflate), so the sigma adds the reflection channel's precedent — a 3×3 spatial luma spread at the pass's own step (`ATROUS_REFL_VARIANCE_GAIN`, raytrace.rs:2504-2520): `σ = max(SCALE × √var_out, FLOOR) + SPATIAL_GAIN × spatial_sd`. Early-out pass-through only when BOTH terms are quiet: `√var_out < EARLY_OUT && spatial_sd < EARLY_OUT`. Overestimate note (recorded, safe direction): at the alpha floor the true EWMA output variance is ≈ raw/99 but n_eff caps at 50, so var_out overestimates ~2× and σ runs ~1.4× wide in steady state — slight over-filtering, never under.

## 3. Design body

### 3.1 `atrous_post` kernel (raytrace.rs, new)

```
struct AtrousPostParams { uint2 size; uint step; float strength; }   // tuning constants are MSL `const float`, same discipline as atrous_filter
kernel void atrous_post(
    params, depth_tex,          // current frame, same guide as atrous_filter
    normal_tex,                 // rt_normal_full (current frame, .xyz normal)
    moments_read,               // just-written moments: r=m1, g=m2, b=ao
    src_irr,                    // read: history slot (pass 1) or ping-pong scratch
    dst_irr,                    // write: rt_irr_filtered or its scratch
    tid)
```

Per texel: void (`depth ≥ 1−1e-6`) → **bit-exact (0,0,0,1) passthrough** — the blend-queue void fallback pattern-matches exactly that value (render_scene.wgsl:1688-1704), so void texels must pass through untouched, never filtered-into. Translucency note: translucent texels aren't in the depth buffer, so they're void here → passthrough; glass/transmission unaffected. Otherwise `n_eff = max(moments_read.w, 1)` (the history count — NOT `src_irr.a`, which is the accumulated AO), `var = max(m2−m1²,0) / n_eff` from moments .r/.g, and `σ = max(scale×√var, floor) + spatial_gain × spatial_sd(src_irr, step)` where the spatial term mirrors `atrous_filter`'s reflection-channel block (raytrace.rs:2505-2521) at this pass's dilation. If `√var < early_out && spatial_sd < early_out`, write src unchanged and return. Taps: the same 8-neighbor 3×3 pattern as `atrous_filter` (diagonals included — T1-D's measured call), dilated by `step` ∈ {1, 2, 4, 8} per pass. Weights: `w_depth = exp(−|Δd|/3e-3)`, `w_normal = pow(max(dot,0), 16)` (both constants shared with `atrous_filter` by name), `w_luma = exp(−|Δluma|/σ)`. Output: `mix(src.rgb, filtered, strength)`; **`.a` = src's `.a` unchanged — it is the accumulated AO and `rt_or_flat_ambient` (render_scene.wgsl:430-437) reads it; the spatial filter never reintroduces per-frame AO noise, same discipline as `atrous_filter`'s center_ao passthrough.**

Moments precision, verified at design time (2026-08-27): `rt_moments_history` is **Rgba32Float** (render_scene.rs:2629-2631) — no f16 cancellation in the variance estimate; the only f16 in the path is the history rgb itself.

Pass structure: N = iterations from the quality tier (2..4). Ping-pong `rt_irr_filtered` ↔ `rt_irr_filtered_b`; pass 1 always reads the history slot. The ping-pong picks destinations so the final result always lands in `rt_irr_filtered` — the composite's single binding point.

Constants (committed initial values; ranges are the tuning envelope, gate-measured):
`POST_LUMA_SIGMA_SCALE` 4.0 (range 2–8), `POST_LUMA_SIGMA_FLOOR` 0.02 (range 0.01–0.05), `POST_SPATIAL_GAIN` 2.0 (range 1–4, anchored to `ATROUS_REFL_VARIANCE_GAIN` = 2.0), `POST_EARLY_OUT` 0.004 ≈ 1 8-bit level (range 0.002–0.01).

### 3.2 `firefly_clamp` kernel (raytrace.rs, new)

Per texel over the resolved scene color: 3×3 luma median (small selection sort over the 3..9-element non-void subset, not a fixed network) over **non-void** neighbors (void = depth ≥ 1−1e-6, read from `self.depth_texture` — the always-ensured internal depth resolve (render_scene.rs:6349), NEVER the graph's lazy `depth` output, which is `None` when unwired); center void → passthrough (an EXR sun disc in the void background is legit content, never clamped). Fewer than 3 non-void texels in the neighborhood → passthrough (silhouette glints can't establish a median). Otherwise `threshold = GAIN × max(median, FLOOR)`; `if luma > threshold { rgb *= threshold / luma }`; alpha passthrough.

Constants: `FIREFLY_MEDIAN_GAIN` 8.0 (range 4–16; anchored to `RT_REFL_FIREFLY_GAIN` = 8), `FIREFLY_ABS_FLOOR` = `max(4.0, emissive_table_mean_power)` — the scene's emissive mean power is already threaded CPU-side (render_scene.rs:5613-5618) and is the scale that answers "firefly or small legit emitter": GAIN 8 × bare FLOOR 1.0 would hard-ceiling isolated content at 8 luma, but ED-B (RAYTRACING_DESIGN.md section 14 (traced environment diffuse)) measured the sun-disc fixture at gain 32, so the isolated-pixel ceiling must be ≥32 — with the floor at `max(4, mean_power)` the worst case is 8 × 4 = 32, and a bright-emissive scene raises its own ceiling. **Consequences, stated honestly:** the <3-non-void-neighbor passthrough lets silhouette fireflies (petal edges — this rig's content class) escape the clamp; the post-accumulation filter still damps them. Deliberately not relaxed tonight — relaxing it is what clamps legit glints.

### 3.3 Dispatch wiring (render_scene.rs)

- `ensure_rt_irradiance` allocates `rt_irr_filtered` + `rt_irr_filtered_b` (rgba16, same usage flags and lifecycle as `rt_irr_history`, render_scene.rs:2547-2551 shape).
- After `accumulate_irradiance` (render_scene.rs:6213), when the tier ≠ Off, RT accumulated this frame, and not `denoise_active`: dispatch N `atrous_post` passes. Reuse the `rt_atrous_params_buffer` precedent (render_scene.rs:2676-2680) for the params struct.
- Composite seam: `rt_irr_tex` (render_scene.rs:6433) binds `rt_irr_filtered` when the filter ran this frame, else the raw history slot. One `let`, no downstream change.
- The clamp: inside the output redirect (render_scene.rs:6311-6321), when RT rendered this frame, `rt_firefly_clamp` param true, and not `denoise_active`: resolve into a **dedicated `rt_firefly_scratch`** (never `rt_temporal_color_scratch` — under temporal upscale `target` IS that scratch, so the clamp's source and destination would alias) and dispatch `firefly_clamp` scratch→target. The scratch is ensured alongside `rt_temporal_color_scratch`.

### 3.4 Settings model (foundation), command (editing), panel (ui)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RtSpatialDenoise { Off, Low, #[default] Medium, High }
impl RtSpatialDenoise {
    pub fn strength(self) -> f32 { /* 0.0 / 0.6 / 0.85 / 1.0 */ }
    pub fn iterations(self) -> u32 { /* 0 / 2 / 3 / 4 */ }
    pub fn label(self) -> &'static str { /* "Off" / "Low" / "Medium" / "High" */ }
}
```

`RtQualityColumn` gains `#[serde(default)] pub spatial_denoise: RtSpatialDenoise`; `RtQualityColumn::export_default` sets High. `RtQuality` (renderer) gains `denoise_strength: f32, denoise_iterations: u32` resolved per frame through the existing `set_rt_quality` path. `ChangeRtQualityCommand` already replaces the whole settings struct — no new command; the panel adds a "Spatial Denoise" row with a 4-option dropdown, same manifest machinery as the P3 rows (no bespoke widgets, WIDGET_TREE_DESIGN.md section 5b (Agent contract & enforcement)).

### 3.5 Perf budget

≤2 ms at 4K (3840×2160) for the post-filter **in steady state** — met by early-out: converged texels cost two reads and a write (~25 B/texel), a fully-converged frame ≈ 0.2 GB, sub-millisecond. The gesture transient (no early-out, 3 passes, ~100 B/texel ≈ 2.5 GB) can reach ~5 ms — accepted because that is exactly the frame where the filter earns its cost and the trace dispatch already dwarfs it; the transient is measured and recorded at P5, and the pre-named lever if it lands hot is Medium dropping to 2 iterations at steps {1,2}. The clamp is one pass, ~80 B/texel, <1 ms at 4K. Measurement fixtures: the apricot RT fixture at 4K rays 100% and RtEmissiveStrength (the Liveschool fixture is pre-3D — no RT node, the filter never dispatches; it serves as the non-RT zero-cost proof, not the cost measurement).

## 4. Invariants & enforcement

- **I1 — Off means byte-identical.** With `spatial_denoise = Off` and `rt_firefly_clamp = false` the RT path is pixel-identical to pre-change. Enforcement: gpu-proofs value test rendering a fixed fixture both ways + the existing gpu-proofs suite as regression net.
- **I2 — The filter never teaches the accumulator.** `atrous_post` writes only `rt_irr_filtered*`; a gpu-proofs test runs N frames with the filter On and Off and asserts the history/moments textures are identical. Negative `rg`: `atrous_post` never takes `rt_irr_history` as a write binding.
- **I3 — No per-frame allocation.** All textures ensured in `ensure_rt_irradiance`; params ride a persistent buffer. Enforcement: code shape + `MANIFOLD_RENDER_TRACE=1` run at P4 gate (no frame >20 ms attributable).
- **I4 — One reset path.** Negative `rg`: zero new `TemporalResetDetector` constructions.
- **I5 — `denoise_active` bypass.** Both stages no-op under it. Enforcement: gpu-proofs code-path test (denoise-active frame produces byte-identical composite to Off).
- **I6 — Moments precision verified, variance arithmetic stated.** `rt_moments_history` is Rgba32Float (render_scene.rs:2629-2631, verified 2026-08-27) — the f16-cancellation worry is closed, not deferred. The known arithmetic skew is the alpha-floor overestimate (D7, safe direction). Enforcement: none — verified at design time; the texture's format line names why fp32 is load-bearing.
- **I7 — Clamp never touches the void background or sub-3-neighbor silhouettes.** Enforcement: gpu-proofs value test — a sun-disc-bright void texel and an isolated 1-px glint both pass through unclamped (CPU-computed expected).

## 5. Phasing

### P1 — Firefly clamp (BUG-mkgh (pre-blur firefly clamp)); branch `lane/rt-firefly-clamp`, lands independently (lane: pro)

- **Entry state:** recon anchors re-verified: output redirect at render_scene.rs:6311-6321; `rt_temporal_color_scratch` ensure block; how `rt_shadows` etc. appear as cardable node params (⚠ VERIFY-AT-IMPL: `rg -n "rt_shadows" crates/manifold-renderer/src` and follow the param declaration).
- **Read-back:** D2, D3, section 3.2 (`firefly_clamp` kernel), I1/I3/I7; the `atrous_filter` MSL + `AtrousParams` CPU mirror + dispatch precedent (raytrace.rs:2404, 4507-4545, render_scene.rs:6016-6040). Restate the forbidden moves.
- **Deliverables:** `firefly_clamp` MSL kernel + CPU-mirrored params struct + `tracer.firefly_clamp` method; `rt_firefly_clamp` bool param on `node.render_scene` (default true); scratch redirect + dispatch; gpu-proofs value test (median math vs CPU-computed expected, plus I7's two passthrough cases); unit-level sorting-network median test.
- **Gate:** `cargo clippy -p manifold-gpu -p manifold-renderer -- -D warnings` clean; `cargo nextest run -p manifold-renderer` green; `scripts/gpu_proofs_gate.py` green (run by lead at review — device contention).
- **Demo:** headless render of `tests/fixtures/rt/RtEmissiveStrength.manifold` with DoF in the chain, clamp on vs off → scripted pixel-diff (outlier texel count above threshold must drop; stated threshold), PNG pair artifact for Peter (L2).
- **Performer gesture:** an emissive strobe cue against a dark scene with DoF wide open — the bokeh discs stay the emitters' honest color, no white hot-spots.
- **Forbidden moves:** clamping when RT didn't render this frame; touching the trace-kernel cap; putting the clamp in a preset JSON; a second scratch allocation per frame.
- **Test scope:** manifold-gpu + manifold-renderer + gpu-proofs.

### P2 — Settings row (lane: v25, mechanical; may run parallel with P1/P3)

- **Entry state:** RT_QUALITY_SETTINGS P1/P2/P3 on main; re-verify `rg -n "RtSpatialDenoise|spatial_denoise" crates/` = zero hits; read `settings.rs` (foundation), `RtQuality` resolution, the panel section built in P3.
- **Read-back:** D4, section 3.4 (Settings model); RT_QUALITY_SETTINGS_DESIGN.md section 3 (Data model) + its P3 brief. Restate: serde defaults ARE the migration; no bespoke dropdowns.
- **Deliverables:** `RtSpatialDenoise` in `manifold-foundation/src/settings.rs`; `RtQualityColumn.spatial_denoise`; export_default High; `RtQuality.denoise_strength/denoise_iterations` resolution; panel row with dropdown; serde round-trip test (old JSON missing the field → Medium realtime / High export; saved non-default reloads intact).
- **Gate:** `cargo nextest run -p manifold-foundation -p manifold-core -p manifold-editing -p manifold-ui` green; clippy on those crates clean.
- **Demo:** `cargo xtask ui-snap` PNG of the RT Quality section with the new row (L2).
- **Forbidden moves:** touching the renderer dispatch; changing any existing default; a third column.
- **Test scope:** the four crates named.

### P3 — `atrous_post` kernel (lane: pro)

- **Entry state:** P1 merged or on its own branch (disjoint files except raytrace.rs — rebase if P1 landed); anchors re-verified: `accumulate_irradiance` write set (render_scene.rs:6171-6213), moments layout (⚠ VERIFY-AT-IMPL: read the accumulate kernel's moments_write writes in raytrace.rs and state which channel holds m1/m2/ao — do not trust this doc's claim, verify), `rt_irr_history.a` count semantics (⚠ VERIFY-AT-IMPL: confirm post-DN-L the count channel still carries n on the non-denoise path).
- **Read-back:** D1, D5, D7, section 3.1 (`atrous_post` kernel), I2/I6; `atrous_filter` whole (raytrace.rs:2404-2584). Restate the forbidden moves.
- **Deliverables:** `atrous_post` MSL kernel + `AtrousPostParams` CPU mirror + `tracer.atrous_post_pass`; gpu-proofs value test: synthetic noisy irradiance + known moments + known depth/normal → filtered output vs CPU-computed expected (the full weight math mirrored); early-out test (converged texel passes through bit-exact); void passthrough test.
- **Gate:** clippy `-p manifold-gpu` clean; `scripts/gpu_proofs_gate.py` green (lead runs at review).
- **Demo:** none — L1 (P4's render is the vertical path).
- **Forbidden moves:** writing history or moments; a luma stop that ignores variance; `create_compute_pipeline` anywhere in manifold-renderer for this (the kernel lives in manifold-gpu's MSL, like its siblings).
- **Test scope:** manifold-gpu + gpu-proofs.

### P4 — Wiring + settings consumption (lane: pro)

- **Entry state:** P2 + P3 on the branch. Re-verify: `rg -n "denoise_iterations" crates/manifold-renderer` returns the P2 resolution; the 6433 binding unchanged.
- **Read-back:** D1, D4, D5, D6, section 3.3 (Dispatch wiring), I1-I5. Restate the forbidden moves.
- **Deliverables:** `rt_irr_filtered`/`_b` allocation in `ensure_rt_irradiance`; dispatch block after accumulate (gated: tier ≠ Off, accumulated-this-frame, !denoise_active); composite rebinding; I2's history-honesty gpu-proof; I5's bypass proof; I1's Off-identity proof.
- **Gate:** `scripts/gpu_proofs_gate.py` green (lead runs); `MANIFOLD_RENDER_TRACE=1` run — no frame >20 ms attributable; clippy `-p manifold-renderer` clean.
- **Acceptance demo:** headless render, `tests/fixtures/rt/RtEmissiveStrength.manifold` paused static, denoise Medium vs Off. The rt-capture `irr_full` slot taps the PRE-accumulation `rt_irr_full` (render_scene.rs:6259-6261) — the post-filter never moves it; **add an `irr_accum` capture slot mirroring the `refl_history_write` capture (render_scene.rs:6253-6256)** — the capture block sits right after accumulate, exactly where the new dispatch lands. Report `composite` + `irr_accum` frame-to-frame |delta| (mean, p99.9) both ways. Expectation: composite and irr_accum drop; `irr_full` UNCHANGED (it is the input signal — a drop there would mean the filter reached backwards). PNG pair for Peter (L2).
- **Performer gesture:** grab a light's intensity and sweep it continuously for 5 s mid-scene — boiling through the gesture fade visibly reduced, no new ghost trail (the filter is spatial, it cannot trail).
- **Forbidden moves:** a new reset path; filtering when accumulation idled; strength applied inside the weight math (it is a final blend); touching the pre-accumulation `atrous_filter` constants.
- **Test scope:** manifold-renderer + gpu-proofs.

### P5 — Measure, gate, land (lead, not a lane)

`scripts/landing_gate.py`; `scripts/gpu_proofs_gate.py`; `scripts/rt_noise_gate.py` — expect composite + the new `irr_accum` channel measurably down; `irr_full` (the pre-accumulation input) must NOT move. **The noise gate is a floor, not a target (Peter's directive 2026-08-27): do not tune to it.** Tuning and re-baseline are driven by the motion-regime PNG pairs, all lead-rendered and lead-eyed before any `--record`: (a) slow camera dolly over glossy surfaces — reflections glued, no boiling, no ghost trails (the DamagedHelmet orbit fixture, rt-capture `--animate`, is the starting oracle); (b) post-light-cue recovery — clean within ~10 frames, not the current ~2.5 s tail; (c) defocused bright emitter — no bokeh blobs AND sun glints/speculars not dulled (bright-emissive scene check before settling the clamp threshold); (d) thin geometry (railings/cables) — texture detail survives, no halos, no smeared contact shadows (halos = edge-stopping too weak = fix the weights before landing, not after). If a fixture for (c) or (d) doesn't exist under `tests/fixtures/rt/`, the lead authors one via `project_tool` in P5. Only `--record` after those PNGs look right. Perf: post-filter + clamp ms at 4K rays 100% on the apricot RT fixture and RtEmissiveStrength (steady state + gesture transient; ≤2 ms steady-state budget), Liveschool as the non-RT zero-cost proof — all written into section 9 (Landing notes), alongside the flip-off path Peter gets if the look is wrong (one dropdown row, one node bool). Close BUG-eytk (spatial à-trous denoiser) + BUG-mkgh (pre-blur firefly clamp); RAYTRACING_DESIGN.md status header + section 17 (ML denoising) pointer updated; supersession sweep (`rg` this design's name + BUG-312 (RT ray noise speckle) across docs/ and memory); merge per `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).

## 6. Decided — do not reopen

1. Post-accumulation filter on demodulated irradiance only; beauty filtering rejected (D1).
2. Clamp at the node output when RT is active, not in presets, not irradiance-side (D2).
3. Both stages are manifold-gpu MSL pipeline kernels, not graph atoms (D3).
4. Quality wiring = one grid row, defaults Medium realtime / High export — old projects change pixels, deliberately (D4).
5. The filter holds no history, adds no reset path (D5).
6. `denoise_active` bypasses both stages (D6).
7. Variance = temporal output variance (m2−m1²)/n_eff; early-out below ~1 8-bit level (D7).

## 7. Deferred (with revival triggers)

- **Filtering the reflection/sv channels post-accumulation** — trigger: Peter's look still names specular boil on a still scene after this lands (refl already has variance-gain + Karis clamp; measure first).
- **fp32 or log-space moments** — CLOSED at design time: moments are already Rgba32Float (render_scene.rs:2629-2631). Not deferred; verified.
- **Silhouette fireflies escaping the clamp** — the <3-non-void-neighbor passthrough (section 3.2 (`firefly_clamp` kernel)) is deliberate; trigger for revisiting: petal-edge fireflies still visible after the post-filter lands, in which case study a wider (5×5) or anisotropic neighborhood rather than lowering the neighbor minimum.
- **Post-filter as a general graph atom** — BLOCKED-class (no offset-texel codegen access kind); trigger: a non-RT consumer wants à-trous. Track in beads if requested.
- **Clamp threshold exposure as a user param** — trigger: Peter reaches for it on stage; constants are tuned against fixtures first.

## 8. Knob table (Peter's morning pass — dial-turning, not spelunking)

Every tunable this design adds, plus the two accumulation constants Peter may reach for in the same session. "Treats" names the visual symptom each direction of turn addresses.

| Knob | Where | Value | Safe range | Turn up treats | Turn down treats |
|---|---|---|---|---|---|
| `POST_LUMA_SIGMA_SCALE` | raytrace.rs `atrous_post` const | 4.0 | 2–8 | residual boil on noisy texels (wider luma tolerance → more averaging) | over-blur / soft GI gradients (narrower → less averaging) |
| `POST_LUMA_SIGMA_FLOOR` | same | 0.02 | 0.01–0.05 | boil on converged-but-noisy texels | flat-region detail loss |
| `POST_SPATIAL_GAIN` | same | 2.0 | 1–4 | post-cue/gesture raw-frame harshness (the cold-start term) | halo risk at true lighting edges through gestures |
| `POST_EARLY_OUT` | same | 0.004 | 0.002–0.01 | filter cost (more texels skip) | unfiltered permanent-floor shimmer |
| Tier → (strength, iterations) | `RtSpatialDenoise` (settings.rs) | Low (0.6,2) / Med (0.85,3) / High (1.0,4) | per-row edit | — | — |
| `FIREFLY_MEDIAN_GAIN` | raytrace.rs `firefly_clamp` const | 8.0 | 4–16 | bokeh blobs / hot bokeh discs | dulled glints (if up fails: floor is binding instead) |
| `FIREFLY_ABS_FLOOR` | same — `max(4.0, emissive_mean_power)` | 4.0 base | 2–8 base | isolated bright panels/small emitters surviving | dark-scene outliers surviving |
| `IRRADIANCE_ACCUM_ALPHA` | render_scene.rs (existing) | 0.02 | 0.01–0.05 | post-cue tail length (higher = shorter tail, more boil) | residual boil on stills |
| `RT_REFL_ACCUM_ALPHA_MIN` | render_scene.rs (existing) | 0.025 | 0.01–0.05 | reflection trail through motion | reflection boil |

## 9. Landing notes (filled at P5)

_To be completed with measured before/after: the four motion-regime PNG pairs, noise-gate channel deltas (composite + `irr_accum` down, `irr_full` unmoved), post-filter + clamp ms at 4K (steady + gesture transient), and the flip-off path._
