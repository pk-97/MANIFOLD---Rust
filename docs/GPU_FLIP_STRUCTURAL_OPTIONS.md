# GPU FLIP structural options — what changes the solver's shape, and what each buys at 64 and 128

<!-- index: Options write-up for BUG-jyot: four structural changes to the GPU FLIP solver (tall-cell columns, narrow-band particles, sparse tiles, coarse-grid projection), each priced as work ratios from the Dam Break's cell counts, with build cost, look risk, what it touches, and a recommendation plus the cheapest proof. -->

**Status:** PROPOSED · 2026-10-02 · Fable 5.1 (design lane) · a decision write-up for Peter, not a build contract; the chosen option gets its own design doc per DESIGN_DOC_STANDARD.md section 2 (The skeleton). Bead: BUG-jyot (GPU FLIP at 128 for 60 fps).
**Prerequisites:** none to decide; to build, GPU_FLIP_PRESSURE_SOLVE.md's solids phase (BUG-4jfv (solids gate)) stays the step's live owner.

Peter, BUG-jyot: "Start only after 64^3 holds volume and runs at 60 fps." Standing rulings that bind every option here: no size caps, no quality caps, a capped solve is reported never an error, Steps per frame is 1, open faces are useful, everything through `manifold-gpu`.

Companions: GPU_FLIP_PRESSURE_SOLVE.md (the solver as built), GPU_WHITEWATER_DESIGN.md (the whitewater step on the same face grid), GPU_FLUID_SURFACE_DESIGN.md (the mesher), LIQUID_SOLVER_SEAM_DESIGN.md (sources, drains, forces, bodies).

## 0. Tonight's numbers and what they say

At 64 with whitewater the frame averages 70 ms; the solver plus whitewater is about 32 ms of it; the target is 16.7 ms. At 128 the frame is about 135 ms (BUG-jyot). So 38 ms of the 64 frame is outside the solver and whitewater (mesher, render, encode). No solver structure change reaches 16.7 ms at 64 on its own; the solver work has to land alongside the render-side work (feat/render-speed, BUG-l24y (GPU liquid surface kernels cost)). At 128 the solver dominates, which is where structure pays.

Where the step's time goes today (GPU_FLIP_PRESSURE_SOLVE.md section 6 (Measures), 64³ Dam Break, 1 step): the two pressure solves plus the density solve are about half the tick (smoothing alone 9.1 ms of a 19.8 ms frame); the other half is dense-lattice passes (`particle_distance`, `particles_to_faces`, three `extend_faces` runs of `band_layers` = 14 sweeps each at 64 one step, `density_source`) and the particle passes (`faces_to_particles`, the sort). Every one of those dense passes touches all 262,144 cells, and water is 16% of them.

## 1. Audit — what exists (verified 2026-10-02)

| Piece | Where | Shape |
|---|---|---|
| The step, one node, dense lattice | `crates/manifold-renderer/src/node_graph/primitives/gpu_flip_step.rs` (`encode`), `shaders/gpu_flip_step.wgsl` | passes in order: `particle_distance` → `particles_to_faces` → extend old → `face_gravity` → solids (`open_fractions`, `solid_face_velocity`, `phi_into_solids`) → `water_from_phi` → pockets → `divergence` → pressure solve → `subtract_pressure` → `constrain_solid_faces` ×2 → extend new → `density_source` → density solve → project → extend → `faces_to_particles` (move) |
| The pressure solver | `gpu_flip_pressure.rs` (`PressureSolver::prepare`, `solve`) | MGPCG on the full lattice; rows assembled once per level; `MAX_ITERATIONS` = 64; the stop and every dispatch after it indirect on the GPU |
| The free surface | `particle_distance`, `water_from_phi` | φ from particles at every cell; water = φ < 0; the engine's level set |
| Whitewater | `whitewater_step.rs` | reads the seam's faces and the surface level set; grid atoms over every whitewater cell (343,000 at 64), particle atoms over its pool |
| The mesher | `fluid_surface.rs`, `volume_surface_mesh.rs` (marching cubes), the Liquid Surface group | from particles: sort, blobs, volume, mesh; a particle-frame seam, not a grid seam |
| Sources, drains, forces, bodies | LIQUID_SOLVER_SEAM_DESIGN.md P8 (Forces and impulses for GPU liquids), `liquid_fill.rs`, `gpu_flip_bodies.rs` | coarse lattices read at face centres; bodies as open fractions and solid velocities on faces, mass-aware rows in the solve |
| The reference engine | `crates/manifold-fluids/native/flip_engine/` | see section 2 |

Extend, don't redesign: every option below keeps the step as one stage node, keeps `PressureSolver` as the one solver module, and keeps the particle-frame seam the mesher and whitewater read.

## 2. What the reference engine does and does not do, structurally

Read 2026-10-02 (`pressuresolver.h`, `blockarray3d.h`, `particlelevelset.cpp`, `fluidsimulation.h`):

- The pressure solve runs on **water cells only**: `_pressureCells` is a list, `_keymap` (`GridIndexKeyMap`) maps a cell to its row, and the PCG vectors are the length of that list. Its matrix is sparse over fluid cells, so an empty tank costs nothing. Ours is a full-lattice multigrid; water is masked, not compacted.
- The level set and attribute transfers run on **active blocks**: `BlockArray3d` with an `activeblocks` list, built from where particles are (`particlelevelset.cpp`). That is a sparse grid for the particle-side passes, not for the solve.
- There is **no narrow band** (every marker particle is a full particle), **no tall cell**, and **no adaptive resolution**. Its answer to thin sheets is sheet seeding (`particlesheeter.cpp`, `_updateSheetSeeding`): it adds particles into gaps. Its answer to speed is CFL-sized substeps and threads.
- Its band of exact level-set distance is 3 cells (`_liquidLevelSetExactBand`), the same number narrow-band FLIP uses.

So the engine is an oracle for two of the four options (sparse particle-side passes, water-only solve rows) and silent on the other two. Nothing in it can race a tall-cell or narrow-band build; those are proven against the dense GPU FLIP step itself, which is already proven against the engine.

## 3. The Dam Break's cell counts

The script is the appendix; its output is reproduced there. Scene from `gpu_flip_preset.rs` (`DAM_FILL_HEIGHT`, `DAM_COLUMN`): a 4 m tank, a 0.16 m pool, the column 1.18 × 1.92 × 3.5 m. Two moments: t = 0 (the column standing) and settled (the same water as one flat pool, 0.66 m deep). The splash in between sits between the two. Assumptions, stated: band = 3 cells each side of the surface (Ferstl 2016's default, the engine's exact band); the tall-cell 3D band = 4 regular cells per column; sparse tile = 8³ cells, active where water or its one-cell halo is; 8 particles a cell (the fill's rule).

| Quantity | 64 | 128 |
|---|---|---|
| Lattice cells | 262,144 | 2,097,152 |
| Water cells (both moments) | 42,965 (16%) | 343,723 (16%) |
| Particles, dense | 343,723 | 2,749,786 |
| Particles, narrow band, t = 0 / settled | 208,719 (0.61) / 98,304 (0.29) | 834,876 (0.30) / 393,216 (0.14) |
| Pressure unknowns, dense grid (today) | 262,144 | 2,097,152 |
| Pressure unknowns, water only | 42,965 (0.16) | 343,723 (0.16) |
| Pressure unknowns, tall cell | 20,480 (0.078) | 81,920 (0.039) |
| Sparse active cells, t = 0 / settled | 94,208 (0.36) / 65,536 (0.25) | 438,272 (0.21) / 393,216 (0.19) |

The particle count at 64 matches the shipped preset's 345,792 within the fill's rounding, so the geometry is right.

The ratios are work ratios for the class of pass they govern, not frame times. The three classes: **S** the pressure solves (about half the 64 tick), **G** dense-lattice passes, **P** particle passes. Today every class is 1.0.

## 4. Option A — tall cell (Chentanez and Müller 2011, "Real-time Eulerian water simulation using a restricted tall cell grid")

**What it is.** Every column of the lattice is one tall cell from the floor to a height, then a fixed number of regular cells above it. Pressure in the tall cell is linear in height (hydrostatic), so each column adds one unknown plus the band's. The band tracks the surface.

**What changes in our pass list.** The lattice becomes two arrays: a height field (per column: tall-cell top, as a cell index) and the band's regular cells, stored as a dense `n × n × band` slab with a per-column base. Every pass that walks cells walks the slab plus the height field: `particle_distance`, `particles_to_faces`, `extend_faces`, `face_gravity`, `divergence`, `subtract_pressure`, `density_source`, `faces_to_particles`. The pressure operator becomes the paper's: regular rows in the band, one row per column coupling the tall cell to its four neighbours' tall cells and to the band cell above it, with the hydrostatic closure. `PressureSolver` needs a second operator and a second coarsening (the paper uses multigrid over tall cells with its own restriction), so the module splits into dense and tall-cell variants behind one `prepare`/`solve`. Particles exist only in the band; the tall cell carries a column velocity. Bodies that reach into a tall cell force the band down to the floor at that column (the paper's rule), so the open fractions and solid velocities stay band-only.

**What it buys.** S: 0.078 at 64, 0.039 at 128, the biggest solve cut of the four. G: the dense passes shrink to the slab, about band/n per column plus the height field: 0.06 to 0.1 at 64 with a 4-cell band, less at 128. P: particles only in the band, about the narrow-band ratio (0.3 to 0.6 at 64, 0.14 to 0.3 at 128). The whole step at 128 lands near a 64 step's cost or under.

**Build cost.** 20 to 30 lane-days. Risky parts: the tall-cell operator and its multigrid (new math, a CPU reference needed first, as `scripts/mgpcg_reference.py` was); band tracking when the surface folds (a breaking wave puts air under water, which a height field cannot hold; the paper lets the band grow to the floor there, which costs its advantage exactly in the splash); bodies, which force full-depth columns around them; the mesher and whitewater, which read particles and a level set that now stop at the band's floor.

**Look risk.** This is the one that changes what the audience sees. Below the band the water is a column with one velocity: no eddies, no bubbles rising from the floor, no sub-surface churn; a body dropped in makes the column around it full-depth and the rest flat. Splash, sheets and the free surface look as today because the band is a full 3D FLIP. Settling is cleaner (hydrostatic by construction). Open faces are fine: a tall cell at an open wall drains as a column. The deep pool of the dam break's end state is where the audience would notice least; a tank of churned water with bodies is where they would notice most.

**Interaction.** Sources and drains that fill from the floor or drain through it need a tall-cell rule (the paper has inflow only through the band). Forces from the coarse lattices apply to the band and as a column mean to the tall cell. Bodies: full-depth columns, as above. Whitewater: bubbles that sink out of the band have nowhere to go; cap their depth or kill them at the band floor. The mesher reads the band's particles and needs the tall-cell top as a closed floor below them, a new input on the particle-frame seam.

**Composes with:** narrow band (A already is one in the band), coarse projection (not needed, A is smaller). Not with sparse tiles (the slab is already compact).

## 5. Option B — narrow-band FLIP (Ferstl et al. 2016, "Narrow band FLIP for liquid simulations")

**What it is.** Particles live only within a band of the free surface; the interior is grid velocity plus a level set. Each step: advect the level set in the interior, advect the particles in the band, build the velocity field from particles in the band and from the grid in the interior, project, then reseed where the band moved and delete particles that fell out of it.

**What changes in our pass list.** The lattice stays dense. New passes: level-set advection for φ in the interior (semi-Lagrangian on faces, one pass), a band mask from φ (|φ| < band·h), `particles_to_faces` masked to the band with the interior faces keeping the grid's advected velocity (a second semi-Lagrangian pass for the interior faces), and two particle passes: reseed (cells that entered the band and hold fewer than rest particles get the fill's eight sites, velocity from the grid) and delete (particles deeper than the band are compacted out, through the existing sort). The pressure operator is unchanged: the full-lattice multigrid solves every water cell as today. `density_source` runs only in the band (the interior's density is the grid's, exactly rest). The mesher reads band particles and the interior φ, so its blobs stop at the band floor and the volume field takes φ below it.

**What it buys.** S: 1.0, the solve is untouched. P: 0.61 at t = 0 and 0.29 settled at 64; 0.30 and 0.14 at 128. G: the φ build (`particle_distance`) and `particles_to_faces` are band-only, about 0.3 to 0.6; `extend_faces`, `divergence`, `subtract_pressure` unchanged. Net at 64: the tick's particle half drops to about 0.4, the solve half stays, so about 0.7 of today. At 128 the particle half is a bigger share and drops to 0.2, so about 0.6 of today.

**Build cost.** 8 to 12 lane-days. Risky parts: reseeding (Ferstl reseeds with velocities interpolated from the grid; too few particles at the band floor read as a density hole and the density projection pushes on it), the interior level-set advection (f32 drift of a surface that no particle pins; the paper re-derives φ from particles in the band every step, so the drift is interior-only), and the delete, which must keep the particle count under the 2²⁴ refusal and the seam's capacity.

**Look risk.** Small. Surface, splash, sheets and settling come from the same particles as today. What changes: nothing rises from the deep interior (a bubble sealed below the band is grid velocity only, and its surface is a level set that will smooth it away). Thin sheets are inside the band by definition and keep their particles.

**Interaction.** Sources that emit below the band need a level-set rule (an emitter deep in the pool writes φ and velocity, no particles); drains likewise. Forces apply to faces either way. Bodies pull the band to their surface (Ferstl keeps particles near solids too, which costs some of the ratio in a scene with many bodies). Whitewater: bubbles under the band advect in grid velocity only, which is what they do now. The mesher needs φ as an input below the band, a new array on the seam.

**Composes with:** sparse tiles (B's interior still runs dense passes, which C removes), coarse projection (C' takes the solve down, B the particles), tall cell (A subsumes B inside its band).

## 6. Option C — sparse tiles (Setaluri et al. 2014, "SPGrid: a sparse paged grid structure applied to adaptive smoke simulation"; the engine's `BlockArray3d`)

**What it is.** The lattice is divided into 8³ tiles; a tile is active when it holds water or its one-cell halo. Every pass runs over active tiles by indirect dispatch; the inactive tiles are never touched and never stored beyond a flag.

**What changes in our pass list.** One new pass per step: tile activation from the sort's bin counts (a tile is active if any bin in it or its halo holds a particle), producing an active-tile list and a count for `dispatch_compute_indirect`. Every dense pass keeps its body and changes its indexing: a thread maps (tile index in the list, cell in tile) to its lattice cell. The pressure operator is the same rows, assembled and swept over active tiles only; the multigrid's coarse levels are active where any child tile is, so each level carries its own list. The CG vectors' folds and `partial_count` run over active tiles. Buffers stay full-lattice (no paging; 2 M cells of rows at 128 is 64 MB, fine), so there is no allocation on the hot path; only the work is sparse.

**What it buys.** G and S: 0.25 to 0.36 at 64, 0.19 to 0.21 at 128. P: 1.0. The solve at 128 goes from about 45 ms (two solves plus density at 18.9 ms a solve, GPU_FLIP_PRESSURE_SOLVE.md section 6 (Measures)) to about 9 ms. The dense passes fall the same way. The tick at 128 lands near 0.35 of today's.

**Build cost.** 10 to 15 lane-days. Risky parts: the multigrid over active tiles (the coarse inverse and the restriction masks are lattice-shaped today; each level needs its list and a rule for the halo), the `extend_faces` sweeps, which must extend into inactive tiles up to `band_layers` cells (the halo has to be `band_layers` wide or the extend reactivates tiles, which costs 14 layers at 64 one step), and the whitewater grid, which runs its own dense atoms and would want the same list.

**Look risk.** None by construction: the same operator on the same cells, bit for bit where tiles are active. The proof is the existing fixtures against the dense step.

**Interaction.** Sources, drains, forces, bodies: each activates the tiles it touches (a body's open fractions need its tiles active before `open_fractions` runs). Whitewater: the grid atoms over 343,000 cells at 64 are the same kind of dense work and take the same list in a second pass of work. The mesher already runs over sorted particles, so it is sparse already.

**Composes with:** everything. C is indexing, not physics.

## 7. Option D — coarse-grid projection (Lentine, Zheng and Fedkiw 2010, "A novel algorithm for incompressible flow using only a coarse grid projection")

**What it is.** Particles, transfers and advection run at the fine lattice; the pressure solve runs at a coarser lattice (one level down, 2³ fine cells a coarse cell). The fine divergence is restricted to the coarse grid, the coarse pressure gradient is prolonged to fine faces, and the paper's correction keeps the fine velocity divergence-free at the fine scale by re-projecting within each coarse cell using the fine faces' own balance (its "velocity correction" step, one local pass).

**What changes in our pass list.** The solver module already has every piece: coarse water from all-eight-children water, restriction, prolongation, and the plain rows at coarse levels (`PressureSolver::prepare`, `level_lattices`). D moves the conjugate gradient from level 0 to level 1 and adds two passes: a fine-divergence restriction that keeps the surface's ghost θ (the paper restricts the fine Dirichlet condition by averaging; our rows carry it as the coarse ghost diagonal, which today is plain), and the local fine correction after `subtract_pressure`. `density_source` and its solve follow the same route. Everything else is unchanged.

**What it buys.** S: 0.125 at both sizes (one level down is 8× fewer unknowns and the V-cycle loses its most expensive level). G and P: 1.0. At 128 the solve drops from about 45 ms to about 6 ms; the tick to about 0.55 of today. At 64 it takes the solve half to about an eighth, the tick to about 0.55.

**Build cost.** 4 to 6 lane-days. Risky parts: the surface at the coarse level (a coarse cell is water only if all eight children are; the paper's partial-cell handling is what keeps thin sheets from losing their pressure, and ours would need the coarse ghost rows, which GPU_FLIP_PRESSURE_SOLVE.md section 2 (The equation) lists as plain today), and the local correction's conservation (it must be exact or the density projection fights it).

**Look risk.** Medium. Pressure resolution at 128 becomes 64's: sheets thinner than two fine cells carry 64's surface pressure, so splash break-up at 128 reads like 64's break-up with 128's particles; the paper shows the fine detail survives in advection but the audience would see 128's surface detail with 64's dynamics. Settling and open faces are unaffected. For the Dam Break at 128 this is the smallest visible change for the solve cut, but it is a quality cap in disguise if it is the only way 128 runs: the ruling "no quality caps" means D ships as a param (Solve Level, default fine) or not at all.

**Interaction.** Nothing new: sources, drains, forces, bodies all act on fine faces; bodies in the solve need their rows at the coarse level (open fractions average, as the coarse faces already do). Whitewater and the mesher read fine particles and faces as today.

**Composes with:** everything. With B it takes both halves of the tick down. With C the coarse level is sparse too.

## 8. Side by side

| | A tall cell | B narrow band | C sparse tiles | D coarse projection |
|---|---|---|---|---|
| Solve work (S) at 64 / 128 | 0.08 / 0.04 | 1.0 / 1.0 | 0.3 / 0.2 | 0.125 / 0.125 |
| Dense passes (G) at 64 / 128 | 0.1 / 0.05 | 0.5 / 0.3 | 0.3 / 0.2 | 1.0 / 1.0 |
| Particle passes (P) at 64 / 128 | 0.5 / 0.2 | 0.45 / 0.2 | 1.0 / 1.0 | 1.0 / 1.0 |
| Tick, rough, at 64 / 128 (solve ½, dense ¼, particles ¼ of today's tick at 64; solve ⅔, dense ⅙, particles ⅙ at 128) | 0.2 / 0.1 | 0.75 / 0.75 | 0.45 / 0.35 | 0.55 / 0.42 |
| Lane-days | 20–30 | 8–12 | 10–15 | 4–6 |
| Look change | deep water is a column | none at the surface | none | 128's detail, 64's dynamics |
| New seam inputs | tall-cell top | interior φ | none | none |
| Composes | with B, D | with C, D | with all | with all |

The tick split is an assumption from section 0 and GPU_FLIP_PRESSURE_SOLVE.md section 6 (Measures); the frame probe (`gpu_flip_speed_measure`) is the oracle for it and should be re-read per pass before any build.

What 128 at 16.7 ms needs: today's 128 frame is about 135 ms; the whole frame has to fall 8×. The solver side has to deliver nearly all of that, so the solver tick must fall to about 0.1 of today. Only A reaches 0.1 alone; B + C + D together, applied per class (solve ⅔ × 0.2 × 0.125, dense ⅙ × 0.2 × 0.3, particles ⅙ × 0.2), reach about 0.06 at 128 with no look change below the surface, and about 0.17 at 64. At 64 the stack takes the 32 ms to about 5 ms, and the frame to the render side's 38 ms, which is the next problem.

## 9. Recommendation

Build C first, then D as a param, then B; do not build A. C is pure indexing with a bit-for-bit proof against today's step, it cuts both the solve and the dense passes by 3 to 5× at the Dam Break's 16% fill, it composes with everything, and every later option runs faster on it. D is the cheapest solve cut and the existing multigrid already holds its pieces, but it is a quality lever, so it ships as Solve Level with the fine level as default and the audience never sees it unless Peter turns it. B takes the particle half down and is what the bead names; it goes third because its ratio is weakest where it matters least (the solve) and it adds a seam input the mesher has to learn. A is the only option that reaches 128 at 60 fps by itself, and the only one that changes what the audience sees under the surface and in every scene with bodies; it is the option to reopen if the stack of C, D and B lands and 128 still does not hold.

**The top uncertainty** is C's ratio in a real frame: the extend sweeps need a `band_layers`-wide halo (14 cells at 64 one step), and if the halo activates most tiles the ratio is nearer 0.7 than 0.3. **The cheapest proof** is a CPU count, no GPU: a scratch script that reads the water masks of the dumped Dam Break pressure problems (`crates/manifold-renderer/tests/fixtures/dambreak_pressure_problems.bin.zst`, the splash frames at 64 and 128), marks the 8³ tiles holding water, grows the active set by a 1-cell halo and by a 14-cell halo, and prints active-tile fractions for each. Two numbers per frame; under half a lane-day. If the 14-cell halo fraction is above 0.6, C's extend needs its own narrower halo rule (extend only inside active tiles and their one-tile ring, as the engine's `_extrapolateFluidVelocities` does on its active blocks) before C is worth building.

## 10. Deferred

- Adaptive step count per tick (BUG-jyot's comment): deferred 2026-10-02 by ruling; fixed 1 step per tick until Peter reopens it.
- Water-only compacted CG rows (what the engine does): subsumed by C; revive if C's multigrid over tiles proves harder than a compacted single-level solve, which would then trade the preconditioner for the compaction.

## Appendix — the cell-count script

```python
#!/usr/bin/env python3
"""Dam Break cell counts per structural option (docs/GPU_FLIP_STRUCTURAL_OPTIONS.md).

Scene: 4 m tank, pool 0.16 m, column x[-1.84,-0.66] y[0.16,2.08] z[-1.75,1.75]
(gpu_flip_preset.rs DAM_FILL_HEIGHT, DAM_COLUMN). Two moments: t=0 (column standing)
and settled (one flat pool of the same volume). Band = 3 cells (Ferstl 2016 default);
tall-cell 3D band = 4 cells; sparse tile = 8^3, active where water or its 1-cell halo.
"""
SIZE = 4.0
POOL = 0.16
COL = [(-1.84, -0.66), (0.16, 2.08), (-1.75, 1.75)]
BAND = 3
TALL_BAND = 4
TILE = 8

def counts(n):
    h = SIZE / n
    cells = n ** 3
    floor = n * n
    pool_rows = POOL / h
    col = [(hi - lo) / h for lo, hi in COL]
    col_cells = col[0] * col[1] * col[2]
    water0 = floor * pool_rows + col_cells
    settled_rows = water0 / floor
    settled_depth = settled_rows * h
    surf0 = floor + 2 * col[1] * col[2] + 2 * col[1] * col[0]
    surf1 = floor
    band0 = min(water0, surf0 * BAND)
    band1 = min(water0, surf1 * BAND)
    tall = floor * (1 + TALL_BAND)
    tiles_per_side = n / TILE
    pool_tiles = tiles_per_side ** 2 * -(-(pool_rows + 1) // TILE)
    col_tiles = 1
    for c in col:
        col_tiles *= -(-(c + 2) // TILE)
    tiles0 = (pool_tiles + col_tiles) * TILE ** 3
    tiles1 = tiles_per_side ** 2 * -(-(settled_rows + 1) // TILE) * TILE ** 3
    return dict(n=n, cells=cells, water0=water0, settled_m=settled_depth,
                parts=8 * water0, band_parts0=8 * band0, band_parts1=8 * band1,
                tall=tall, tiles0=tiles0, tiles1=tiles1)

for n in (64, 128):
    c = counts(n)
    print(f"n={n}  cells={c['cells']:,}  water={c['water0']:,.0f} ({c['water0']/c['cells']:.1%})  settled depth {c['settled_m']:.2f} m")
    print(f"  particles dense {c['parts']:,.0f}  narrow-band t0 {c['band_parts0']:,.0f} ({c['band_parts0']/c['parts']:.2f})  settled {c['band_parts1']:,.0f} ({c['band_parts1']/c['parts']:.2f})")
    print(f"  pressure unknowns: dense-grid {c['cells']:,}  water-only {c['water0']:,.0f} ({c['water0']/c['cells']:.2f})  tall-cell {c['tall']:,.0f} ({c['tall']/c['cells']:.3f})")
    print(f"  sparse active cells t0 {c['tiles0']:,.0f} ({c['tiles0']/c['cells']:.2f})  settled {c['tiles1']:,.0f} ({c['tiles1']/c['cells']:.2f})")
```

Output, 2026-10-02:

```
n=64  cells=262,144  water=42,965 (16.4%)  settled depth 0.66 m
  particles dense 343,723  narrow-band t0 208,719 (0.61)  settled 98,304 (0.29)
  pressure unknowns: dense-grid 262,144  water-only 42,965 (0.16)  tall-cell 20,480 (0.078)
  sparse active cells t0 94,208 (0.36)  settled 65,536 (0.25)
n=128  cells=2,097,152  water=343,723 (16.4%)  settled depth 0.66 m
  particles dense 2,749,786  narrow-band t0 834,876 (0.30)  settled 393,216 (0.14)
  pressure unknowns: dense-grid 2,097,152  water-only 343,723 (0.16)  tall-cell 81,920 (0.039)
  sparse active cells t0 438,272 (0.21)  settled 393,216 (0.19)
```
