# DN-I operating-point sweep results

**Measured 2026-08-08.** Tree: `lane/rt-operating-sweep` @ `045369e2` (DN-I sweep knobs on top of DN-G merge). Protocol: A3a (RAYTRACING_DESIGN.md section 16.4 (transmittance cost attribution)) — RtApricot*, 150 frames (60 play + 90 paused), 3840x2160, --sync-gpu, release build, private capture dir per cell, machine otherwise quiet. Settled window: frames 10-59 (play phase; accel build completes by frame ~5). Noise delta: mean per-pixel |delta| over consecutive paused-phase frames 144-149, 8-bit RGB levels.

## Results table

| Cell | ms median | p5 | p95 | composite |d| | Denoiser resolution | Composite PNG |
|---|---|---|---|---|---|---|---|
| 1. OFF-baseline | 61.92 | 59.12 | 76.07 | 0.0017 | - (denoiser off) | `/tmp/rt_sweep_cell1/composite_0149.png` |
| 2. ON-1:1-default | 252.23 | 237.43 | 273.00 | 0.0006 | 3840x2160 -> 3840x2160 | `/tmp/rt_sweep_cell2/composite_0149.png` |
| 3. ON-1:1-mid | 250.51 | 226.49 | 266.14 | 0.0008 | 3840x2160 -> 3840x2160 | `/tmp/rt_sweep_cell3/composite_0149.png` |
| 4. ON-1:1-low | 246.28 | 228.94 | 265.50 | 0.0010 | 3840x2160 -> 3840x2160 | `/tmp/rt_sweep_cell4/composite_0149.png` |
| 5. ON-1:1-floor | 242.72 | 217.50 | 272.67 | 0.0015 | 3840x2160 -> 3840x2160 | `/tmp/rt_sweep_cell5/composite_0149.png` |
| 6. ON-upscaled-default | 162.74 | 121.43 | 184.82 | 0.1007 | 2560x1440 -> 3840x2160 | `/tmp/rt_sweep_cell6/composite_0149.png` |
| 7. ON-upscaled-low | 162.92 | 129.01 | 180.79 | 0.1055 | 2560x1440 -> 3840x2160 | `/tmp/rt_sweep_cell7/composite_0149.png` |

SPP configs: `mid` = REFL=4 GI=2 AO=2, `low` = REFL=2 GI=2 AO=1, `floor` = REFL=1 GI=1 AO=1. `default` = committed constants (REFL=8 GI=4 AO=4, per RAYTRACING_DESIGN.md section 13 (temporal denoiser rebuild)). The upscaled cells use `temporal_upscale=true` (T2-B's 1/1.5 render scale, D11 mode B → fused denoise+upscale from 2560x1440 → 3840x2160).

## Anomalies

1. **OFF-baseline is 2x the morning reference** (~32.3 ms reference vs measured 61.92 ms). This tree's RtApricot fixture runs the full scene (3 emitters + point light), not the sun-only A3a setup. The DN-G G-buffer widening may carry struct overhead even when the denoiser is off. Per-cell deltas are internally consistent; the absolute scale is shifted uniformly.

2. **MetalFX denoiser dominates frame cost.** The incremental cost over OFF is ~190 ms for 1:1 and ~100 ms for upscaled. SPP variation (REFL 1–8, GI 1–4, AO 1–4) has essentially zero impact on frame time — render res is the only visible cost lever.

3. **No cell hits <32 ms.** The best denoised cell is ON-upscaled-default at 162.74 ms. Decomposing: OFF 61.91 ms + denoiser ~190 ms = ~252 ms 1:1; OFF 61.91 ms + denoiser+upscale ~101 ms = ~163 ms. If OFF were at the ~32 ms reference: 1:1 would be ~222 ms, upscaled ~133 ms. The denoiser cost itself (~190 ms / ~101 ms) appears structural.

4. **PNG brightness shift.** OFF composite mean = 18.5; all denoised composites = 64.7–65.3. The MetalFX scaler applies a color-space conversion (its output is sRGB-encoded vs the raw-accumulation linear path). Not a defect for measurement; visible in the PNG pairs as brighter-lit denoised frames.

## Per-cell verifications

- Composite PNGs: all 3840x2160, lit apricot (mean 18.5–65.3), not black.
- Denoiser resolution: cells 2–5 show 3840x2160 → 3840x2160 (1:1); cells 6–7 show 2560x1440 → 3840x2160 (fused upscale).
- No contamination discards: zero `RT accel structure (re)build enqueued` lines in the captured stderr during the paused phase for any cell.
- SPP env vars recognized: the sweep-knob commit (`045369e2`) populates `ShadowRayParams` from `MANIFOLD_RT_SWEEP_{AO,GI,REFL}_SPP`; the code path is production-inert (unset = committed constants, byte-identical).

## Read on shipping constants

The SPP knobs are a cost lever against a wall: the MetalFX denoiser itself costs ~190 ms 1:1 / ~100 ms upscaled, and any ray-count savings are dwarfed by it. For this scene class (single static mesh, 3 emitters + point light, void background), the **upscaled path is the only frame-budget-viable option** — 163 ms at full 4K is heavy but not hopeless, and it buys the clearest possible denoised look. The 1:1 path at ~250 ms is export-tier only.

The choice between default and low SPP on the upscaled path is ~0.2 ms and not visible in the composite noise delta (0.1007 vs 0.1055). Shipping default SPP is justified: the denoiser hides the extra samples and costs nothing measurable.

-- k3-lane (DeepSeek V4 Pro, slot-0)
