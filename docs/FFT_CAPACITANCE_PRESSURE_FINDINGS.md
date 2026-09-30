# FFT Capacitance Pressure Findings

<!-- index: Research record for a free-surface liquid pressure solve built from FFTs: capacitance unknowns on a one-cell air collar, whole-box FFT/DCT solves, and a surface-FFT |k| helper that keeps the pass count flat as the grid grows. Measured 2D/3D/GPU results, the rejected routes with their numbers (Dodd-Ferrante air split, naive masked FFT helper, warm start, edge band), the math found on the way (split ringing, waterbed law), literature status, and what is owed before engine work. -->

**Status: RESEARCH · 2026-09-30 · Claude + Peter, reviewed by Astra (Codex). Promising, not proven in the engine. Owed: per-piece surface helper, wall cosine transform, Rust MPSGraph benchmark against the MPM path. Tracker: BUG-wsim (FFT pressure split research).**

## The result

A free-surface liquid (air removed, pressure zero at the surface) can be made exactly incompressible with a small number of FFT passes that does not grow with grid resolution on wave-like surfaces. Measured: 9 to 10 passes from 32³ to 256³, solved to the exact grid equation. Every expensive step is a whole-box FFT, which maps onto Apple's GPU FFT (`crates/manifold-gpu/src/metal/fft.rs` already wraps MPSGraph FFT, 1D only today).

It is not faster than MPM yet in any measured sense. Open costs: splashes that break into many pieces raise the count (30 passes average at 128³), and the per-pass GPU cost is about 1.3 ms at 128³ and 4.7 ms at 256³ for the FFT alone.

## The method

Pressure equation: the masked 7-point Laplacian in water cells, with air neighbours read as zero (first-order free surface).

- **Collar unknowns.** Unknown sources λ live only on the one-cell layer of air cells touching the water, plus one constant c. With G the whole-box Poisson pseudo-inverse (FFT in wrapping directions, DCT for walls):

  p = G(f − Jᵀλ − mean) + c, with J p = 0 on the collar and sum(Jᵀλ) = sum(f).

  In water cells this is exactly the masked equation. It is the capacitance matrix method (Buzbee, Dorr, George, Golub 1971).
- **Why the naive FFT helper fails.** Using the box FFT as a helper on the masked problem lets pressure extend into air. For a planar surface the error per tangential wave k is λ(k) ≈ 1/(2|k|h), so long surface waves get eigenvalues that grow as 1/h and the count grows with the grid (Astra's derivation).
- **The surface helper.** The collar system behaves like 1/(2|k|) per surface wave, so it is preconditioned by multiplying each tangential surface Fourier mode by sqrt(−Δ_surface + q0²) ≈ |k|: sum collar sources per (x, z) column, 2D FFT, scale, inverse, spread back, plus a local term (2/h) on the within-column differences so the helper stays invertible. The column-only version was singular and falsely converged at 11% error.
- **Solver loop.** GMRES on the collar system; each pass is one box FFT solve plus one small surface FFT. A fixed pass count needs no convergence readback on the GPU.

## Measured results

Pass counts to a relative tolerance of 1e-6 (2D) or 1e-5/1e-6 (3D); "exact" means checked against the true masked-equation residual.

| Test | Grid | Passes | Notes |
|---|---|---|---|
| 2D flat layer, old helper vs surface FFT | 32 / 64 / 128 / 256 | 11,14,16,18 vs 8,8,8,7 | residual ≤ 5e-6 |
| 2D 4-cell floating sheet, surface FFT | 32 / 64 / 128 / 256 | 9, 11, 16, 22 | close paired surfaces grow |
| 2D sloshing tank over time | 64 / 128 | 9.2 / 11.3 avg | old helper 32 / 41 to only 1e-3; surfaces match exact to 0.2% of wave height |
| 3D standing wave | 32 / 64 / 96 | 10.1 / 10.4 / 10.2 | flat |
| 3D wave + two floating drops | 32 / 64 / 96 | 17.5 / 22.0 / 25.1 | column-summed helper lumps pieces |
| 3D particle pool + drop (PIC/FLIP), CPU | 40 | 15.2 avg | drop falls at true gravity, plunges, rebounds |
| 3D collapsing column + thrown drop, CPU | 40 | 20.0 avg | asymmetric |
| 3D GPU (MLX float32), wave | 32 → 256 | 9 at every size | float32 residual 3e-5 to 6e-4 |
| 3D GPU full particle step, splash | 128 (≈6M particles) | 29.9 avg (20–43) | 254 ms per step from Python |

GPU cost per pass (MLX, M-series, float32): 64³ 0.77 ms, 128³ 2.15 ms, 256³ 13.3 ms including scatter and sync. Pure FFT round trip: 64³ 0.42 ms, 128³ 1.30 ms, 256³ 4.69 ms; the y-mirror used in place of a DCT doubles the 256³ cost. Projection for the engine with a real DCT and GPU-side Krylov: about 1.5 ms per pass at 128³, so about 7 ms per step at 1e-3 tolerance (about 5 passes) once the per-piece helper holds the count flat. Estimate, not measured.

## Rejected routes, with the numbers that killed them

- **Dodd–Ferrante constant-coefficient split with air as a real phase (one FFT per step).** Algebra exact except the extrapolated pressure p̃ = 2pⁿ − pⁿ⁻¹. At show-speed steps (32 per wave swing) the motion error is 102–144% at 100:1 and 1000:1; under 5% needs about 400 steps per swing at 100:1. Usable only near a 5:1 density ratio (13% at 32 steps, 3% at 64), which makes it a liquid-in-liquid tool (oil on water, lava lamp, ink layers), not water in air.
- **Column-sum hydrostatic pressure as the guess.** Exact only for flat surfaces; on a sloped wave it injects sideways pressure and blew up 10–3000× at coarse steps.
- **Air as a real fluid with an FFT-preconditioned CG.** Resolution-independent but ~√r: 8 / 25 / 85 passes to 1% at 10:1 / 100:1 / 1000:1.
- **Masked free surface with the box FFT as the only helper.** 23 / 38 / 63 passes at 32 / 64 / 128 (about N^0.7).
- **Warm start from last frame.** Useless (35 → 38 at 32²): cells flip between air and water each frame exactly where the hard part is. A subcell (ghost-fluid) surface would make frames resemble each other; not tried.
- **Exact local solve on a 2-cell surface band.** Only 15–20%: the error is long-range along the surface, not local.
- **Grid-transported water fraction for the demos.** The drop fell at a third of gravity and smeared away before landing; particles fixed it.

## Math found on the way

- **The split's error rings.** In the heavy phase μ = 1 − 1/r and the guess error follows e⁺ = μ(2e − e⁻), roots μ ± i√(μ − μ²): a lightly damped oscillator with period ≈ 2π√r steps (≈199 at 1000:1). Smooth motion at that rhythm gives pressure error ≈ √r × pressure. The 2D model showed a velocity-error bump near that period (2.9 at 128 steps per swing at 1000:1). Exact critical damping of the heavy mode is β = (1 − r^−½)/(1 + r^−½) for p̃ = pⁿ + β(pⁿ − pⁿ⁻¹); it removes the ringing but makes the guess first-order.
- **Waterbed law.** For the fixed linear guess filter, the mean of ln|H| over all rhythms equals ln μ ≈ 0 for every β (Jensen's formula; Bode's sensitivity integral). Tuning only moves error between rhythms. Scope, per Astra: holds for stable fixed linear filters; adaptive predictors tuned to the rhythms actually present are not bound by it.
- **Delta form.** Solving for δ = pⁿ⁺¹ − p̃ removes the big-minus-big cancellation that costs float32 about r × machine precision at 1000:1; it does not remove all roundoff.
- **Fourier facts used.** In a uniform wrap-around box the pressure solve is a per-wave division and viscosity is exact per wave; anything non-uniform (walls, a density jump) couples the waves, which is the root of every obstacle above. Solids as drag masks (Brinkman penalization) converge to true walls as the drag stiffness grows.

## Literature status

A search found no publication combining a one-cell air collar, a free-surface liquid, an |k| surface-mode helper and a particle liquid with a real-time GPU FFT target (about 75% confidence; niche or paywalled work may exist). Every ingredient is published:

- Capacitance matrix: Buzbee, Dorr, George, Golub 1971; Proskurowski and Widlund 1976; O'Leary and Widlund 1979.
- Square-root interface preconditioner: Arioli and Loghin 2008 ("Matrix square-root preconditioners for the Steklov–Poincaré operator").
- Box FFT plus a boundary Krylov solve: Ying and Henriquez 2007 (kernel-free boundary integral); Leathers and Guy (arXiv 2203.12126).
- Graphics Schur-complement / FFT liquid solver: Liu, Mitchell, Aanjaneya, Sifakis, ACM TOG 2016 (CPU).
- Baselines: McAdams, Sifakis, Teran 2010 (multigrid); Chentanez and Müller 2011 (real-time GPU water).

No paper found that kills the idea; thin sheets, splash crowns and droplet clouds are untested anywhere.

## Owed before any engine work

1. **Per-piece surface helper.** Treat each connected piece of water as its own surface, so drops and splash fragments stop raising the count. Close paired surfaces (thin sheets) need explicit coupling.
2. **Side walls.** A cosine transform in x and z; the prototypes wrap around.
3. **Rust benchmark on the engine FFT.** Extend the MPSGraph plan to 3D, add the cosine-transform wrap, run the Krylov loop GPU-side, and time a pressure solve at 128³ and 256³ against the existing MPM path on the same scene. This is the only way to make "faster" a real claim.
4. **Integration shape.** It would replace MPM's spring pressure for water inside the GPU MPM pipeline (`docs/GPU_MPM_SOLVER_DESIGN.md`), keeping particles, transfers, the repeat region, meshing and coupling. MPM stores velocity at grid corners and this solve is face-based; the earlier `mac_*` face-based prototype recorded in that doc is the starting point. The FFT would be new primitives: section 2.5 (primitive audit) of `docs/DECOMPOSING_GENERATORS.md` first, and it goes on the freeze exemption list because it is not a per-element atom.

## Where the evidence lives

- Viewer (2D and 3D runs, pass-count charts, the 128³ GPU splash): https://claude.ai/artifact/1nvDRAGrQTTVYbm8KTEA6H
- Research scripts (Python; numpy/scipy, MLX for GPU) are published with that viewer under `scripts/`: `split2d.py` (air split model), `capacitance.py` (2D collar solve), `freesurf_sim.py` (2D tank), `cap3d.py` (3D solve), `sim3d_flip.py` / `sim3d_mix.py` (3D particle demos, CPU), `gpu_cap3d.py` / `gpu_breakdown.py` (GPU timings), `sim3d_mlx.py` (full GPU particle step).
- Numbers and the running log: BUG-wsim (FFT pressure split research) notes.
