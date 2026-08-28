# RT Stage-4 Motion Denoise — history-aware filtering + temporal feedback

**Status:** APPROVED design, not built · 2026-08-28 · k3 (lead) + Peter
**Prerequisites:** RT_STAGE3_DENOISE_DESIGN.md (shipped 2026-08-28) — this design extends its filter, never replaces it.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

The stage-3 post-accumulation à-trous filter is proven on converged temporal history and inert under continuous motion — measured on Peter's own project (helmetGlitches.manifold, 240-frame rt-capture A/B, denoise High vs Off: composite diff ~2% of pixels at mean 0.03/255, frame-to-frame boil identical at 2.036 vs 2.040; bead BUG-27bs (RT spatial denoise inert under continuous motion)). The mechanism: under motion the accumulator's validation keeps rejecting history, per-texel history length stays short, moments cold-start at zero variance on every rejected texel, and the filter's variance-guided sigma stands down exactly where the noise is. The static-scene win does not transfer, and Peter's show content moves. Peter's directive, verbatim: **"A denoiser that only works on static images isn't all that useful"** — and on the trade this design makes: **"blur for stability sounds better than boiling I think."** The MetalFX path is rejected for this slot, Peter verbatim 2026-08-28: **"MetalFX isn't great and actually quite expensive for the results it gives."**

The fix is the proven real-time recipe (SVGF's two load-bearing ideas) applied to machinery we already have: (A) modulate the existing filter by per-texel history length — strong where history is thin, light where converged; (B) feed the filtered frame back as next frame's accumulation input, so a rejected pixel inherits a neighborhood-filtered estimate instead of one noisy sample. On B's safety rails, Peter 2026-08-28: **"we have all of the infra and signals for cutting and resetting temporal feedback I think"** — correct: the reset decision is one call site and feedback folds into it (D4).

This design does NOT touch the deadline-miss glitch (BUG-i7p1 (RT glitches when FPS target is missed)) — that is the temporal path versus dropped frames, a separate bug with its own bead.

Companion docs: `docs/RT_STAGE3_DENOISE_DESIGN.md` (the filter this extends; its knob table and invariants carry over), `docs/RAYTRACING_DESIGN.md` section 13 (temporal denoiser rebuild — the accumulator), section 17 (ML denoising — the rejected alternative).

## 1. Audit — what exists (verified 2026-08-28, main tip 5c8d55d4b)

| Piece | Where | State |
|---|---|---|
| Per-texel history length | `raytrace.rs:2966` ("Frames of history behind this texel, carried in `moments_write.w`"), written `:3322`, clamped to 50 at `:3089`; moments texture is **Rgba32Float** (`render_scene.rs:2629-2631`) | **exists, already bound into `atrous_post` as `moments_read`** — used today only via `var_out = (m2−m1²)/n_eff` and the damped spatial term `min(2/n_eff, 1)` |
| Moments cold-start | `raytrace.rs:2818` (comment), `:2916` (`moments_write.write(float4(cur_luma, cur_luma², cur.a, 1.0), tid)` on reset) | a rejected texel resets variance to ZERO and history to 1 — the stage-3 early-out (`√var < EARLY_OUT && spatial_sd < EARLY_OUT`) then passes it through unfiltered. **The measured mechanism of BUG-27bs (RT spatial denoise inert under continuous motion), stated as hypothesis; P1's gate confirms or refutes it by outcome** |
| Post-accumulation filter | `raytrace.rs` `atrous_post`; dispatched `render_scene.rs:6405-6464`; ping-pong `rt_irr_filtered`/`_b`; composite rebind `render_scene.rs:6722-6726` | stage-3, shipped; irradiance only (its D1) |
| Accumulator history read | `accumulate_irradiance` reads `rt_irr_history[read]` (ping-pong, `render_scene.rs:6171-6213`); per-object motion reprojection (RT-T2-C (per-object motion reprojection) `obj_motion`) already in `AccumulateParams` (`raytrace.rs:1212-1244`) | the seam feedback hangs on: **the read address is the whole change** |
| Reset machinery | ONE call site: `reset_decision` (`render_scene.rs:6221-6223`) = TemporalResetDetector (`:4346`) OR `rt_irr_needs_reset` OR `toggle_flipped`; CPU lighting key + geo key + gesture holds (`:6240-6263`) | feedback's ghost safety rides this — no new detector (I4) |
| `denoise_active` bypass | `render_scene.rs:6405`, stage-3 I5 | unchanged; stage-4 stages stand down under it too |
| Motion fixtures | `RtMotionHelmet` orbit fixture (stage-3 P5 oracle, rt-capture `--animate`); helmetGlitches.manifold (Peter's project, /tmp capture sets from 2026-08-28) | RtMotionHelmet = the gate fixture; helmetGlitches = the project-shaped L2 look. Committing helmetGlitches as a fixture is blocked by BUG-trx5 (project_tool cannot strip a .manifold to a committable fixture) — not required: RtMotionHelmet suffices for the gate |
| Reflection-channel machinery | variance gain + Karis neighborhood clamp (BUG-dx6w (specular history neighborhood clamp) / BUG-axe9 (tone-mapped variance clip) lineage, stage-3 D1); SV-ACCUM has its own moments per caster slot (`raytrace.rs:2819`) | ⚠ VERIFY-AT-IMPL at P2 entry: whether refl carries per-texel history length in an addressable channel — read the refl accumulate kernel's moments writes, do not trust this doc |
| Motion A/B evidence | /tmp/rt_capture_high + /tmp/rt_capture_off (240 frames × 10 channels, 2026-08-28); per-frame sd + boil scripts /tmp/glitch_discriminate.py, /tmp/boil_metric.py | the BUG-27bs numbers above; baseline for every phase gate |

Section 2.5 audit findings: **no new graph primitives.** All work is inside `manifold-gpu`'s raytrace.rs kernels and `render_scene.rs` dispatch, same class as stage-3 (its audit paragraph applies verbatim — multi-tap cross-pixel gathers, outside the freeze-codegen mandate, precedent `atrous_filter`).

## 2. Decisions

- **D1 — Filter scope for motion is irradiance + reflections, decided directly (Peter, 2026-08-28: no census phase — the phase gates are the measurement).** Stage-3 scoped the post-filter to demodulated irradiance and it measured inert under motion; the helmet's glossy shell makes reflections the other plausible carrier. Both get the treatment: irradiance in P1, reflections in P2, each gated on the helmet A/B — if a channel doesn't pull its weight there, it reverts at that phase, no harm. sv stays out (SV-ACCUM owns it; revival trigger in section 7). Rejected: a P0 per-channel boil census before building (Peter: probe loops are misleading and burn session time — the end-to-end gate numbers answer the same question with the fix already built). Rejected: extending to all channels including sv up front (stage-3 D1's bandwidth argument stands).
- **D2 — Step A: history length drives the filter directly, not only through variance.** `atrous_post` gains a short-history term: `σ` floor and `strength` scale with `n_short = clamp(SHORT_N / max(n_eff, 1), 0, 1)` (SHORT_N ≈ 8, committed initial): a texel with 1–3 frames of history filters at full strength with a raised sigma floor (it has no temporal evidence — neighborhood is all it has); a texel at 20+ frames behaves exactly as stage-3. The early-out gains a third condition: never early-out while `n_eff < SHORT_N`. **Consequences, stated honestly:** moving content's GI (and P2's named channels) get visibly softer — spatial blur substituting for missing temporal evidence. This is the trade Peter accepted verbatim above. Sharp-but-boiling is available by keeping Spatial Denoise at Off.
- **D3 — Step B: temporal feedback through the existing ping-pong, reset-gated.** Next frame's `accumulate_irradiance` history-read binds the previous frame's `rt_irr_filtered` instead of the raw history slot — a read-address change at one seam (`render_scene.rs:6171-6213`), zero new textures, zero new passes (the filter already runs). Stage-3's I2 ("the filter never teaches the accumulator") is REWRITTEN, deliberately, into I2′ below — not eroded. Rejected: feedback without the reset fold-in (the plausible-wrong shape — see section 4). Rejected: adaptive alpha instead of feedback (faster accumulation under motion trades boil for lag on lighting cues — the gesture fixtures punish exactly that; feedback keeps alpha and lets the filter carry the noise).
- **D4 — Feedback honors the ONE reset path, verbatim.** On any frame `reset_decision` is true, the accumulator's history read bypasses `rt_irr_filtered` (reads nothing — today's reset behavior) AND `rt_irr_filtered` is treated as invalid until the next filter dispatch. Cuts, seeks, lighting-key snaps, gesture holds, toggle flips: all already funnel through `render_scene.rs:6221`. No new reset path, no new detector (I4 carries). The strobe rule (RAYTRACING_DESIGN.md D3 (temporal accumulation is trigger-aware)) is unaffected: demodulated irradiance + lighting-key snaps own strobes; the feedback read never sees a strobe frame's stale content because the key fires first.
- **D5 — Moments keep tracking the raw signal.** `accumulate_irradiance` still writes moments from the unfiltered current frame (m1/m2 EWMA), even when its history read is the filtered frame. Rationale: the variance estimate must keep measuring *incoming* noise, or the filter's own guidance decays as feedback cleans the signal (a self-calming loop that would let real lighting changes hide inside a shrinking σ). **Consequences, stated honestly:** with feedback on, `moments` no longer describes the composite's actual noise floor — the rt-capture `moments` channel reads noisier than the picture looks. The noise gate's ceilings stay calibrated on `irr_accum`/`composite`, not `moments`.
- **D6 — Phases land A before B, and B is gated on A's evidence.** P1 (history-aware strength, no contract change) lands and is measured first. P3 (feedback) is briefed only with P1's helmet numbers in hand: if A alone pulls motion boil to the target (section 5 P1 gate), B still proceeds — convergence under motion is the design's actual goal, A is the warm-up that de-risks the contract change. Peter may stop the train after any phase's PNG pair; every landed phase is independently useful and Off-reversible.
- **D7 — Quality tiers unchanged; the new behavior rides the existing row.** Spatial Denoise Off = byte-identical old path (I1 carries). Low/Medium/High map to the same (strength, iterations) pairs; history-awareness and feedback are part of what the row means, not new rows. Rejected: a separate "Motion Denoise" toggle — one more row for Peter to manage on stage, and the Off escape already exists.

## 3. Design body

### 3.1 `atrous_post` kernel changes (P1)

Signature unchanged (params, depth, normal, moments, src, dst). New arithmetic per texel:

```
n_eff  = max(moments_read.w, 1)
n_short = clamp(SHORT_N / n_eff, 0, 1)          // 1 at n=1, ~0 at n≥SHORT_N
σ      = max(SCALE × √var_out, FLOOR × (1 + FLOOR_SHORT_GAIN × n_short))
            + SPATIAL_GAIN × spatial_sd × max(min(2/n_eff, 1), n_short)
strength_eff = mix(strength, 1.0, n_short)       // short history: full blend
early-out only when √var_out < EARLY_OUT && spatial_sd < EARLY_OUT && n_eff ≥ SHORT_N
```

Constants (committed initials; knob-table ranges): `SHORT_N` 8 (range 4–16), `FLOOR_SHORT_GAIN` 4.0 (range 2–8), the rest stage-3's. The damped-spatial `max(min(2/n_eff,1), n_short)` merge keeps one "spatial speaks while temporal is thin" term — `n_short` is simply a longer-legged version of the existing damper, not a second mechanism (zero-new-systems test).

### 3.2 Accumulator feedback seam (P3)

Old → new, written out (seam brief per standard section 6):

- Old: `accumulate_irradiance` reads `rt_irr_history[1 - rt_history_ping]` every frame.
- New: same call site reads `rt_irr_filtered` when (a) the filter ran last frame (`irr_filtered_valid` persisted one frame as `irr_filtered_feedback_valid`), (b) `reset_decision` is false this frame, (c) tier ≠ Off, (d) !denoise_active. Else today's raw history slot. The write side is untouched: accumulate always writes `rt_irr_history[rt_history_ping]`; the filter always reads that slot and writes `rt_irr_filtered`. The data-flow cycle is filtered→accumulate→history→filter — one frame of lag, no same-frame read-after-write.

`irr_filtered_valid` is currently frame-local (`render_scene.rs:4376`); P3 persists it and clears it on any reset frame and on any frame the filter didn't run (dimension change, RT-idle frame) — stale-filtered feedback is the forbidden state (I2′).

Validation interaction, stated: with history now pre-smoothed, the per-texel validation gates (cam-motion-widened bands, `raytrace.rs:1230-1243`) accept more often — that IS the convergence win. The CPU lighting key (`render_scene.rs:6240`) owns real lighting changes and does not depend on pixels, so a genuine cue still snaps through the filter. Gesture holds (two consecutive changes arm a hold) extend the same way: during a hold, the reset path is hot and feedback is repeatedly bypassed — ghosting bounded by the hold window, measured on the gesture fixture at P3 gate.

### 3.3 Reflection-channel mechanics (P2)

Reflections get the same treatment as irradiance: a `rt_refl_filtered`/`_b` pair ensured alongside the existing refl history, `atrous_post` reused (it is channel-agnostic: params/depth/normal/moments/src/dst), composite rebind at the refl consumption seam. If the refl accumulate kernel has no per-texel history length (P2's entry check), P2a adds it to the refl moments write (mirroring `moments_write.w` at `raytrace.rs:3322`) before any filtering, as its own committable step with a gpu-proofs value test.

### 3.4 Perf budget

Same budget discipline as stage-3 (≤2 ms at 4K steady state). P1: zero added cost (arithmetic on bound values; the wider early-out window costs exactly the passes it already runs on raw frames). P2: roughly one more filter's bandwidth per added channel — measured at P2 gate on the apricot RT fixture, rays 100%. P3: zero added passes — the read-address change is free; the only cost is `irr_filtered_valid` persisting. Feedback does not add a sync point: `rt_irr_filtered` is written by the same encoder timeline the accumulator already reads history on.

## 4. Invariants & enforcement

- **I1 — Off means byte-identical** (carried, stage-3). Enforcement: gpu-proofs value test, fixed fixture both ways.
- **I2′ — The filter teaches the accumulator ONLY through the designated feedback read, and never stale.** Exactly one read site (`accumulate_irradiance`'s history binding); `irr_filtered_feedback_valid` is cleared on every reset frame and every filter-idle frame. Enforcement: gpu-proofs value test — force a reset mid-run, assert the reset frame's accumulate reads raw (feed it a poisoned `rt_irr_filtered`; output must not contain the poison value); negative `rg`: `rt_irr_filtered` appears as a read binding in exactly one call site.
- **I3 — No per-frame allocation** (carried). Enforcement: code shape + `MANIFOLD_RENDER_TRACE=1` at each gate, no frame >20 ms attributable.
- **I4 — One reset path** (carried). Enforcement: negative `rg` — zero new `TemporalResetDetector` constructions; P3's diff touches `reset_decision`'s consumers only.
- **I5 — `denoise_active` bypass** (carried). Both steps no-op under it.
- **I6 — Feedback can never self-sustain.** The new-sample weight (alpha) has a hard floor independent of history state — a pixel always ingests fresh trace at ≥ today's alpha; feedback only pre-smooths what it blends into. Enforcement: code shape (alpha expression untouched by P3 — negative `rg`: the alpha constant's line is outside P3's diff) + gpu-proofs: a pixel whose filtered input is frozen must still converge to a changed input signal within 1/alpha frames.
- **I7 — Void passthrough bit-exact** (carried, stage-3: the blend-queue fallback pattern-matches (0,0,0,1)).

**The plausible-wrong architecture, forbidden by name:** *feedback without the reset fold-in* — wiring `rt_irr_filtered` into the accumulate read and leaving `reset_decision` to clear only the raw history. It looks complete, every static test passes, and it ghosts through every cut and lighting cue on stage. I2′'s poison test exists precisely because this shape passes everything else. Second forbidden shape: *global strength under a motion flag* — detecting "scene is moving" and cranking `POST_LUMA_SIGMA_SCALE`/iterations for the whole frame. Cheaper to write, blurs converged detail everywhere instead of only where history is thin; D2's per-texel `n_short` is the mechanism precisely because the motion is per-texel.

## 5. Phasing

### P0 — REMOVED (Peter, 2026-08-28: no census phase). Its one real dependency — whether the reflection channel carries per-texel history length — folds into P2's entry state as a read-the-kernel check, ten minutes, not a measurement session.

### P1 — History-aware strength (lane: pro)

- **Entry state:** this doc on main. Re-verify anchors: `atrous_post` sigma arithmetic and early-out (raytrace.rs, post-stage-3); `moments.w` clamp site (`raytrace.rs:3089`).
- **Read-back:** D2, D7, section 3.1, I1/I3/I7; stage-3 section 3.1 (`atrous_post` kernel) whole. Restate the forbidden moves.
- **Deliverables:** the section 3.1 arithmetic in `atrous_post`; constants `SHORT_N`, `FLOOR_SHORT_GAIN`; gpu-proofs value test (synthetic moments at n_eff ∈ {1, 3, 50}: low-n texel filters measurably stronger than stage-3, high-n texel bit-matches stage-3); early-out extension test (n_eff < SHORT_N never early-outs).
- **Gate:** `cargo clippy -p manifold-gpu -- -D warnings`; `scripts/gpu_proofs_gate.py` green (lead runs); helmet motion A/B (rt-capture, denoise Medium, build vs main): `irr_accum` + `composite` motion boil (mean |delta|) measurably down vs the 2026-08-28 baseline, stated threshold ≥15% on irr_accum; RtEmissiveStrength static A/B: boil no worse than stage-3 (the converged path must not regress).
- **Acceptance demo:** helmet motion PNG pair (main vs P1) at a flare frame + a quiet frame, region-mean probes at named coordinates; PNGs for Peter (L2).
- **Performer gesture:** rotate the helmet continuously through 90° while a light cue lands — GI stays glued, no boil bloom on the newly-visible faces.
- **Forbidden moves:** touching the accumulate kernel or any reset logic; a motion "flag" (per-frame global) instead of per-texel `n_short`; changing tier→(strength, iterations) pairs.
- **Test scope:** manifold-gpu + gpu-proofs.

### P2 — Reflection channel (lane: pro)

- **Entry state:** P1 on main. Read the refl accumulate kernel's moments writes (raytrace.rs) and state whether refl carries per-texel history length in an addressable channel — if not, P2a adds it to the refl moments write (mirroring `moments_write.w` at `raytrace.rs:3322`) as its own committable step with a gpu-proofs value test, before any filtering. Re-verify: refl history/moments textures and the refl composite consumption seam (`render_scene.rs:6734` area).
- **Read-back:** D1, section 3.3, I1/I3; stage-3 D1 (why refl was excluded) — restate what changed (motion, not stills, is the target now).
- **Deliverables:** `rt_refl_filtered`/`_b` pair; dispatch + composite rebind at the refl seam; P2a if needed; gpu-proofs value test mirroring P1's.
- **Gate:** clippy `-p manifold-gpu -p manifold-renderer`; gpu-proofs green (lead); helmet motion A/B: refl-history boil down ≥15%, composite down with it — **if refl filtering cannot show that, the phase reverts and section 9 records it** (the channel wasn't the carrier; P1 still stands); perf on apricot 4K rays 100%: added ms recorded, ≤2 ms over stage-3.
- **Acceptance demo:** PNG pair on the helmet at a specular-heavy frame (L2 for Peter); region-mean probe numbers as the agent gate.
- **Performer gesture:** slow camera dolly across the glossy shell — reflections glued, no sparkle boil, hoses/thin geometry not smeared.
- **Forbidden moves:** filtering sv (SV-ACCUM + hold machinery owns that channel — overlap is scope creep); touching stage-3's irradiance constants; beauty-pass filtering anywhere.
- **Test scope:** manifold-gpu + manifold-renderer + gpu-proofs.

### P3 — Temporal feedback (lane: pro; lead reviews every diff)

- **Entry state:** P1 + P2 on main with their helmet numbers in the doc. Re-verify: `reset_decision` call site and its three inputs (`render_scene.rs:6221-6223`); `irr_filtered_valid` lifetime (`:4376`, `:6462`).
- **Read-back:** D3, D4, D5, section 3.2 (the seam brief), I2′/I4/I6; the forbidden shape in section 4, restated aloud.
- **Deliverables:** `irr_filtered_feedback_valid` persistence + clearing rules; the feedback read-address change; reset-frame bypass; I2′ poison test; I6 convergence test; (refl feedback rides the same seam, added in the same diff).
- **Gate:** clippy `-p manifold-renderer`; gpu-proofs green (lead); helmet motion A/B: composite motion boil vs post-P2 build — the convergence number, target ≥2× reduction in composite mean |delta| under continuous rotation vs the 2026-08-28 baseline (2.036 mean on the quiet span); **gesture ghost gate**: RtEmissiveStrength continuous intensity ramp + one-shot ambient snap (apricot fixture) — frame-to-frame |delta| tail after the cue returns to baseline within stage-3's window +2 frames, no sustained offset (the ghost detector); strobe leg: 4 Hz intensity square, no frame shows the previous phase's content (pixel-diff at phase boundary vs raw-history control run).
- **Acceptance demo:** helmet 60-frame clip PNGs at three rotation phases + the gesture recovery curve, for Peter (L2).
- **Performer gesture:** strobe cue mid-rotation — the frame after each snap is clean, no smeared afterimage of the pre-snap lighting.
- **Forbidden moves:** ANY change to `reset_decision`'s inputs or a second detector; letting `irr_filtered_feedback_valid` survive a dimension change; touching the alpha expression; filtering the moments writes.
- **Test scope:** manifold-renderer + gpu-proofs.

### P4 — Measure, gate, land (lead, not a lane)

`scripts/landing_gate.py`; `scripts/gpu_proofs_gate.py`; `scripts/rt_noise_gate.py` — re-baseline ONLY after the motion PNG pairs pass Peter's eye (his directive carries: the gate is a floor, not a target). Perf: steady-state + gesture transient at 4K rays 100% on apricot + RtEmissiveStrength, Liveschool non-RT zero-cost proof. Close BUG-27bs (RT spatial denoise inert under continuous motion). RAYTRACING_DESIGN.md status header + section 13 (temporal denoiser rebuild) pointer updated; supersession sweep (`rg` this design's name + "stage-4" + BUG-27bs across docs/ and memory); lifecycle call per CLAUDE.md shipping rule. Merge per `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).

## 6. Decided — do not reopen

1. Motion filter scope is irradiance + reflections directly; no census phase — gates are the measurement (D1, Peter 2026-08-28).
2. History length (`moments.w`) drives filter strength per texel; no global motion flag (D2).
3. Feedback = read-address change at the one accumulate seam, reset-gated; I2 rewritten to I2′ deliberately (D3/D4).
4. Moments keep tracking the raw signal; noise-gate ceilings live on irr_accum/composite, not moments (D5).
5. A lands before B; B's contract change is evidence-gated but not optional (D6).
6. MetalFX rejected for this slot (Peter, 2026-08-28, quoted above).
7. The deadline-miss glitch is BUG-i7p1's (RT glitches when FPS target is missed), not this design's.

## 7. Deferred (with revival triggers)

- **sv (shadow visibility) channel post-filtering** — SV-ACCUM + per-caster moments + snap-hold already own that channel; trigger: the helmet A/B still shows shadow-visibility boil after P1/P2 land.
- **Adaptive spp under motion** — rejected as a phase here: spends GPU exactly when the budget misses (the BUG-i7p1 correlation); trigger: hardware headroom changes (export tier may revisit).
- **World-space / disocclusion-mask-guided filtering** (a true SVGF disocclusion pass instead of n_eff as the thin-evidence proxy) — trigger: P3's gesture ghost gate fails in a way n_eff tuning cannot fix.
- **Committing helmetGlitches as a repo fixture** — blocked on BUG-trx5 (project_tool cannot strip a .manifold to a committable fixture); trigger: that bead closes. RtMotionHelmet carries the gate until then.

## 8. Knob table (additions to stage-3's table)

| Knob | Where | Value | Safe range | Turn up treats | Turn down treats |
|---|---|---|---|---|---|
| `SHORT_N` | raytrace.rs `atrous_post` const | 8 | 4–16 | boil on fast-moving/disoccluded texels (more texels count as thin) | soft-everything (fewer texels count as thin) |
| `FLOOR_SHORT_GAIN` | same | 4.0 | 2–8 | boil on just-rotated-into-view faces | halo/softness on legitimately sharp moving edges |
| feedback (on/off) | tier ≠ Off, code path | on | per-build | motion boil via convergence | (turn off = revert to stage-3 behavior) |

## 9. Landing notes (filled at P4)

_<pending>_
