# Matter sand and soil

<!-- index: Sand and soil on the Matter (MLS-MPM) solver: a Sand scene item, a per-particle material stage with a 3×3 SVD, Drucker-Prager sand with volume correction, soil as sand with cohesion that weakens as it shears, wall friction, an opaque grain look. Matter's water path is deleted. Later: the World's shared grid and two-way coupling with GPU FLIP water through its solid faces, kept open for a unified solver. -->

**Status:** steps 1–4, the MVP, are built. Steps 5–6 wait for WATER_SHARED_GRID_DESIGN.md phase 4.

**Peter's calls (29 Oct):** Matter is never water; water is GPU FLIP only. Sand meets water with separate velocities on one grid, built simple but kept open for a later unified solver. Tests serve the end state: two maths checks, the rest is stills looked at by eye.

## What the performer gets

A **Sand** item in the Add menu. It fills a box, a sphere or any Object's shape (a hill model filled with soil), rests on colliders and the Bounds floor, pushes Box3D bodies and is pushed back (boulders ride a slide), and takes the scene's forces and impulses (a quake on the drop). Card dials:

| Dial | Default | What it does |
|---|---|---|
| Friction Angle | 35° | How steep a pile holds. Sand flows below it and stops at it. |
| Cohesion | 0 kPa | Soil. About half a metre of vertical cliff per kPa. Dropping it live brings a held slope down. |
| Density | 1600 kg/m³ | Bulk dry sand. Sets how bodies sink and float on it. |
| Stiffness | 1 | Scales the elastic stiffness. Higher costs more steps per frame; the card says when it is limited. |
| Cell Size, Bounds, Gravity, Speed, Reset | as Matter today | |

## 1. Sand item

Sand is its own item kind, not a Water setting. A Water item is GPU FLIP only.

- **Kind:** `ItemKind::Sand` (`manifold-foundation/src/scene_surface.rs`, `ALL` becomes 12) with `SandFields { draw, material, bounds }` in `scene_doc.rs`. Sand holds Water Source items (Fill only) and a Material (`tree.rs` `can_hold`); a Water Source sits in a Water or a Sand. Every `match` on the kind gains its arm; Swarm's sites are the list (icons, native text, clipboard, group command, projection, setup checks, transform, compile order).
- **Card:** `SAND_ROWS` in a new `scene_schema/sand.rs`, taking Water's Matter rows (Stiffness, points per cell, Seed) and its shared rows (Cell Size, Bounds and faces, Gravity, Speed, Reset), plus Friction Angle, Cohesion and Density. FLIP Ratio goes: sand runs pure APIC, so G2P's FLIP blend and the grid node's saved pre-force velocity are deleted, and the clamped-node count moves into the stats words.
- **Water loses:** `WaterSolver`, `WaterFields.solver`, the `SOLVERS` choice, every `.when(mpm)` row and the switch-to-MPM refusal (`scene_schema/water.rs`). `WaterTemplate` is keyed by item kind and draw instead of solver; the two Matter templates in `scene_family.rs` register for Sand.
- **Old projects:** every saved Water writes `"solver"` (Peter's waterTestV2, waterTestV3 and waterBoxTests do), and `WaterFields` denies unknown keys, so a `manifold-io` migration drops the key. No project saves MPM water, so nothing converts to Sand.
- **Add menu:** a Sand entry. `app/src/scene_edit.rs` gives a new Sand its own box Fill and its own opaque sand Material, as it does for Water.
- **Compile:** `scene_compile/water.rs` splices Sand the way it splices Water (same adopt, transform, cell size, bounds, world controls and source wiring), from the Matter family.
- **Fill shapes:** a Sand Fill is a box, a sphere or an Object's shape. `liquid_fill_in_shape` (BVH or distance lattice, today in the FLIP crate's `liquid_fill_body.wgsl`) moves to a shared include in `manifold-water-liquid`, and `matter_fill_body.wgsl` keeps only sites inside the shape. `node.matter_domain` takes the shape and its region the way the FLIP domain does (`take_initial_fill`, `fill_region`) and loses its "MPM fills a box" refusal. Inflow and Drain stay refused for Sand ("Sand fills only"); pouring sand is later work.

## 2. Material stage

Every particle carries its elastic deformation F (3×3, starts as identity) and the stress it produces. The point record grows from 80 to 160 bytes and stays one record, so the substep region still carries one state array and the sort still permutes indices, never records (D9 of the old design: storage is id order).

```
struct MatterPoint {            // 160 B
  position: vec3, id: u32,
  velocity: vec3, volume_ratio: f32,   // J = det F, for the frame and stats
  affine_x: vec4,   // C row 0; w = v_c, log volume gained while pulled apart
  affine_y: vec4,   // C row 1; w = V0, rest volume
  affine_z: vec4,   // C row 2; w = material index (0 = sand), kept by every pass
  f_x, f_y, f_z: vec4,   // F rows; f_x.w = q, accumulated plastic strain; y.w, z.w spare
  stress_diag: vec4,     // Kirchhoff τ xx, yy, zz
  stress_off: vec4,      // τ xy, xz, yz
}
```

One new binding-free include, `shaders/matter_material.wgsl`, holds all material maths. It sees one particle's F, its velocity gradient and the material constants, and returns the new F, plastic state and stress. It never reads the grid, a binding or a record. This is the seam the unified solver needs: the maths moves unchanged, only the transfer is rewritten.

- **SVD** (McAdams et al. 2011, "Computing the SVD of 3×3 matrices with minimal branching"): F = U Σ Vᵀ with U and V proper rotations. Six Jacobi sweeps with approximate Givens rotations (the paper's four leave up to 0.7% error on random F near identity), then a Givens QR. Fixed cost, no branches that diverge. This is what sand's friction rule runs on: it only makes sense on Σ, the three stretches.
- **Order inside `node.grid_to_matter`**, after it forms C (whose rows are the velocity gradient): F ← (I + dt·C)·F, SVD, return map (section 3), rebuild F = U Σ' Vᵀ, J = det Σ', then τ = U·diag(2μ·ε + λ·tr ε)·Uᵀ with ε = log Σ'. Write F, τ, v_c and q. One SVD per particle per substep.
- **`node.matter_to_grid`** reads τ from the record: affine rows become m·C − (dt·V0·4/dx²)·τ, the full matrix instead of today's scalar pressure. It no longer needs λ or Cohesion.
- **`node.matter_stats`** computes elastic energy V0·(μ‖ε‖² + ½λ(tr ε)²) with its own SVD, once a tick.
- **`node.matter_fill`** writes F = I, τ = 0, v_c = 0, q = 0, material 0.
- **`matter_remove_nonfinite`** also checks F and τ.

The constants reach the passes as scalars from `node.matter_domain` (μ, λ, α, cohesion, softening, density), as λ, cohesion and density do today.

## 3. Sand and soil

**Elasticity:** Hencky strain, St. Venant–Kirchhoff (Klár et al. 2016, "Drucker-Prager Elastoplasticity for Sand Animation"). Starting constants E = 3.537·10⁵ Pa, ν = 0.3 (the values Tampubolon et al. 2017 used), so μ = 136 kPa and λ = 204 kPa. These are starting points, tuned on stills: sand's look comes from plasticity, and stiffness only has to be high enough not to bounce.

**Return map** on ε = log|Σ| (each σ clamped to at least 10⁻⁴), with β the cohesion as a tensile log-strain and α = √(2/3)·2 sin φ / (3 − sin φ) from the Friction Angle φ (α = 0.386 at 35°):

1. ε_c = ε + v_c/3 and tr = Σε_c (the volume correction, Tampubolon et al. 2017: volume gained while grains were apart counts as stretch, so it is paid back before pressure builds).
2. **Pulled apart** (tr ≥ β): Σ' = exp(β/3)·I, v_c ← tr − β, δq = ‖ε_c − β/3‖.
3. Otherwise v_c ← 0, ε̂ = ε_c − (tr/3)·1, δγ = ‖ε̂‖ + α·(3λ + 2μ)/(2μ)·(tr − β).
4. **Holding** (δγ ≤ 0): Σ' = exp(ε_c).
5. **Sliding** (δγ > 0): Σ' = exp(ε_c − δγ·ε̂/‖ε̂‖), δq = δγ.
6. q ← q + δq.

With β = 0 this is Klár's cone and taichi_elements' `sand_projection` (engine/mpm_solver.py), except that taichi drops the volume correction in the holding case; here it is kept in F, so no volume is lost. Written in `shaders/matter_material.wgsl`. β > 0 shifts the cone's tip into tension: the material carries some pull and gains shear strength α·(3λ + 2μ)·β, which is Drucker-Prager with cohesion.

**Soil:** β = β₀·exp(−q / q_s). β₀ comes from the Cohesion dial in kPa, divided by α·(3λ + 2μ) so the dial means the same at any Stiffness or Friction Angle. Intact soil holds; where it shears, q grows, cohesion fades and it breaks into clumps and loose sand. q_s starts at 0.05 and is tuned on stills. Friction hardening (Klár's h₀–h₃) is left out: one Friction Angle dial reads better on stage, and q is already stored if it is wanted later.

**Walls and colliders:** sand needs grip or it skates. `matter_wall_stop` gains Coulomb friction: when a wall stops a node's inward speed v_n, the tangential speed shrinks by μ_b·|v_n|, never past zero, with μ_b = tan φ (the sand's own friction; one source). Colliders already pass each body's friction to `liquid_collider_project` and the mesh path in `matter_tree.wgsl`.

**Steps per frame:** the wave speed becomes √((λ + 2μ)/ρ), 17.3 m/s for the defaults (water's was about 34). Today's water-only `water_lambda` (stiffness scaled by the box size, taichi_elements' rule) goes. On paper, from the acoustic CFL with a 4 m fall: 21 steps per 1/60 s at 6.25 cm cells, 11 at 12.5 cm, 6 at 25 cm. The Stiffness fit to `MAX_SUBSTEPS` is wired (it is stubbed today, BUG-5hq2l) so the card shows when Stiffness is limited.

**Scale:** a 20 × 20 m slope 2 m deep is about 26 million particles at 6.25 cm cells and about 400 thousand at 25 cm. Big slides use coarse cells; on the shared grid that becomes soil on its own level (step 5).

## 4. Look

Sand draws through the normal opaque scene path, not the water renderer. The water renderer is glass only: `fs_water` has no diffuse body, refracts the scene behind, and a field casts no shadows.

- **Mesh:** the Sand object draws `tile_surface_mesh`, the marching-cubes mesh already built every frame on the display tiles (FLIP uses it only for first-hit depth), as its `vertices`, with no `field` wires. This is not the old water mesh chain the decision log retired; it is today's tile mesh, and drawing it through the plain material path was measured on 28 Oct (Dam Break 36.0 ms against the field march's 38.5). On this path sand casts and receives shadows, gets AO and RT reflections, and binds any Material. The mesh's UV is world xz, so a sand texture tiles across the ground.
- **Material:** a new Sand gets an opaque PBR Material (transmission 0, roughness about 0.9, sand albedo), tuned on stills. Ripples, caustics, mist and foam are not built for Sand.
- **Motion blur:** the mesh's triangles change every frame, so the previous-position history (`render_scene/prev_positions.rs`, same triangles only) cannot serve it. `tile_surface_mesh` also writes each vertex's previous position (its position minus the surface velocity times the frame time, from the surface group's `velocity` output) into a second buffer, and `node.scene_object` takes it as an optional `prev_positions` input that the velocity pass reads in place of the history. Any generated mesh can use the same port later.
- **Smoothing:** the surface group's smoothing rounds ridges; Sand starts with less than water and is tuned on stills.
- **Grains (the default draw):** `node.matter_grains` turns each point into four octahedron grains, each at a hashed offset, turn and size keyed on the point's id, drawn as instances with the sand Material. The grains rest on the floor, so there is no mesh lip there. Render cost is vertex work (non-indexed, several passes): 5.9 ms on the 4 m test pile, against 20 ms for eight icosahedra per point. Surface-only grains would allow more per point.
- Later: grain texture on steep faces (triplanar), sparkle, dust.

## Deleted with this work

Matter's water: the λJ(J − 1) pressure and its Cohesion, `water_lambda`, `COHESIVE_J_MAX`, `j_max`, `WATER_DENSITY` in Matter, FLIP Ratio, and the Water Solver choice. Also:

- `matter/look.rs` (the A1–A6 water look metrics), `tests/gpu_proofs/matter_look.rs`, and the look-metrics mode in `examples/fluid_capture.rs`.
- `matter/reference.rs` (the f64 water oracle) and the oracle comparisons in `tests/gpu_proofs/matter_transfer.rs`. Any check there that holds for every material stays.
- The Matter row of `LIQUID_SOLVERS` (`testkit/liquid_conformance_fixtures.rs`): Matter is not a liquid.
- The fixture presets `WaterDamBreakMatter`, `WaterFloatingBoxMatter` and `WaterStillPoolMatter`. One `SandColumn` preset replaces them wherever a test or a primitive's `examples:` list needs a Matter graph (about 40 references: `engine_scenes.rs`, `testkit/assets.rs`, catalog tests, `encode_replay`, `gpu_flip_export_demo.rs`, `retired_cpu_flip.rs`). `matter_scene.rs` and `matter_bodies.rs` test the solver, not water, and move to it.

## 5. More materials

A Sand item is made of a `Matter` (`SandFields.matter`): Sand, Snow, Clay, Mud, Goo or Jelly, each an Add menu entry under Matter with its own starting look (`scene_schema::matter_look`) and dials. The compiler sets the domain's `material` enum; `MatterMaterial::of` (matter.rs) turns the kind and its dials into the constants `node.grid_to_matter` takes, its rule among them (`material_rule`). The rules live in `matter_material.wgsl`:

| Rule | Materials | Model |
|---|---|---|
| Granular | Sand, soil | Drucker-Prager with cohesion (section 3) |
| Snow | Snow | Stomakhin et al. 2013: stretches clamped to [1 − 2.5e-2, 1 + 7.5e-3], the plastic part packs or loosens the point (log Jp in `f_x.w`), hardening exp(ξ(1 − Jp)) held to [0.1, 5]; fixed-corotated stress |
| Viscoplastic | Clay, mud, goo | Von Mises with a Perzyna overstress on Hencky strain: yield strain τ_y / (√2 μ), relax time η / μ, so steady shear carries τ_y + η·γ̇ (Bingham) |
| Elastic | Jelly | Hencky elasticity, no plastic flow |

Dials: Stiffness for all; Friction Angle and Cohesion for sand; Hardening for snow; Firmness (shear yield stress, Pa) and Thickness (plastic viscosity, Pa·s) for clay, mud and goo; a Density for each, starting at its material's (snow 400, clay 1800, mud 1700, goo 1400, jelly 1050 kg/m³). Snow's substeps budget for five times its stiffness (fully packed), so it costs about two and a half times sand's steps. Clay runs at sand's Young's modulus rather than its real one, since only the yield shows.

Every Matter item in a scene runs on one solver (step 7 below), so they collide.

## Build order

One batch per step, in step order, each built, checked and committed before the next. Step 1 first so every still after it is a real scene. Step 1 is mostly mechanical match arms across separate files (lane workers); steps 2 and 3 are the lead's or one sonnet worker given the formulas above; step 4 touches the render crate's velocity pass.

## Checks

- **One GPU maths proof** for `matter_material.wgsl`, on a few thousand random F including nearly flat and inverted ones: the SVD rebuilds F, U and V are rotations, Σ is sorted; the return map lands on or inside the cone, the pulled-apart case lands on the tip, and the plastic step never raises the elastic energy.
- **Stills by eye** (`--profile stills`): a sand column slumps into a cone and stops near the Friction Angle; a soil block holds a vertical face, then fails into clumps when Cohesion drops; a box dropped on sand sinks a little and stops; sand poured on a sloped collider runs off and piles.
- One `--release` frame-time number for a Sand scene at the end.

## Later: steps 5 and 6

**5. Shared grid.** Matter's arrays move onto the World's one tile table and budget (WATER_SHARED_GRID_DESIGN.md D1, D2). Matter's corner nodes and FLIP's cells share tile keys already (both use `world_tiles`, TILE = 8). Two Matter rules: the table grows between Matter's own substeps without moving any water tile, since sand outruns a one-tile ring within a frame; and at the budget, new sand is refused, never existing sand deleted (each particle carries F and q). Soil may sit on a coarser level than water.

**6. Sand meets water.** Built, both ways, for the first Sand and the first Water.

- Water in sand. The first Sand's particles each frame reach every Water's step (`sand`, `sand_count`). The step spreads each grain's rest volume and momentum onto its cell corners (`sand_corners`, five words a corner with its grains' solid volume) and draws the water there toward the sand's mean velocity at up to `SAND_WATER_DRAG` (40) per second at a full cell of sand (`sand_drag`, after the forces): sand is a porous moving solid to the water, which soaks into a still pile over seconds and seeps through at about g / 40 = 0.25 m/s. Sand as a hard solid in the open fractions was tried and dropped: water caught in moving sand could not be pushed out (thousands of particles a tick), speeds reached 80 m/s and the pressure solve took ten times the iterations.
- Water on sand. Once a tick, `node.matter_water_push` (in the Matter's tick body, between `node.matter_state` and Points to Grid) reads the first Water's step as its last tick left it, through state-capture inputs: the water's velocity (MAC trilinear on the pooled faces) and the share under water (the step's distance) at each grain. Each point under water loses `water_lift` of its gravity (Archimedes: sand and snow by their grains' volume, quartz and ice, since the pore water is the water's own; clay, mud, goo and jelly by their whole volume), then moves toward the water's velocity at `SAND_WATER_DRAG` × water's density over the sand's, so the push is equal and opposite; exact over the tick.
- Damp sand sticks. The state keeps each point's wetness (F row 1's w: soaked to its share under water, drying over the wet look's 15 s) and its share under water now (F row 2's w); the granular return map adds 1 kPa of cohesion times the wetness above the water, which flow never softens (the water's bridges form again).
- Wet look. Each Water's wet map (`water_wetness`) takes the same grains, spreads their rest volume onto its display cells (`sand_share`) and marks the sand's surface where the water touches it (`cs_mark_sand`); the wet pass darkens it and it dries like any wet surface. Sand with grain keeps its roughness when wet: water drains between the grains and leaves no film.
- Porous in the solve: a porous material's grains close the water's open fractions by their solid share and move the closed part with the sand (`sand_porous`, ONE_WORLD_DESIGN.md "Porous sand in the pressure solve"). Not built: water inside sand taking only the pores' volume, continuous materials in the solve.

References: Tampubolon et al. 2017 ("Multi-species simulation of porous sand and water mixtures") and Gao et al. 2018 ("Animating fluid sediment mixture in particle-laden flows").

## 7. One Matter solver per scene

Peter, 30 Oct: every Matter item shares one solver and one grid, so snow, sand, jelly and mud collide instead of passing through each other, and the per-item grid work (tables, static-mesh caches, grid passes over the same tiles) is done once.

- **Owner and members.** The first Matter item in tree order splices the whole template. Every later one (a member) splices only its material and draw (`MATTER_SOLVER` in `scene_compile/water.rs` lists the shared nodes): wires the solver fed come from the owner's nodes, and nothing of the member feeds the solver. Grid-wide rows (Cell Size, Bounds and faces, Domain Size, Fill Height, Particles per Cell, Seed) bind on the owner only; a member's card skips them because it has no domain. A scene runs one solver for each group of `MAX_MATTER_SLOTS` (8) Sands.
- **Material slot.** Each item is a slot, 0 for the owner, in tree order. A point carries its slot in `affine_z.w`, written by the fill.
- **`node.matter_material`.** One per item, holding the material rows the card binds, with a `slot` param. It writes its 16-float row (μ, λ, α, β₀, q_s, rule, yield strain, relax time, ξ, density, lift, drag rate, wall friction) into a copy of the table it is handed. Its scalars fold down the chain in slot order: the fastest unhardened wave (`wave`), snow's (`snow_wave`), the largest damping diffusivity η/ρ (`diffusivity`), the heaviest density, and the owner's wall friction. The last node feeds the domain, the state, P2G, G2P and the stats.
- **The step follows the stiffness present.** The domain's substeps come from the folded wave, snow's times √h for the packing it last read (`node.matter_stats` word 18, the largest snow hardening, through the state's `hardened` output on a state-capture input), and the viscous limit, capped at `MAX_SUBSTEPS`. Live it reads h with 25% to spare; offline it budgets the full 5 so exports repeat. The domain publishes `wave_reach` and `speed_estimate`.
- **Caps per point.** `grid_to_matter` builds each point's material from its row and caps it to the step (`matter_capped`): stiffness times at most `(wave_reach / dt − speed_estimate)² ρ / (λ + 2μ)` (snow's hardening held under it, other materials' μ and λ scaled down only past it) and Kelvin-Voigt damping at most `ρ dx² / (12 dt)`. A late read softens a hard-packed point for a frame, never blows it up. `limited_by_substeps` reports the share of stiffness the stiffest material keeps.
- **Per-point values.** P2G's mass, the stats' mass and energy, the water's lift and drag all read the point's row.
- **Fills.** Slot k's sources are the domain's role ports from k·`roles_per_slot`; each slot takes one Initial Fill. Without Bounds the grid starts from the box round every fill. The domain writes a fill table (`fills`: each slot's box cells above the pool, its shaped fill's region row, its first point) and `fill_points`; `node.matter_fill` seeds the pool (slot 0) then each slot's box in order.
- **Draws.** Points never move in the array, so each item's points are one run. The domain publishes each slot's run (`first_k`, `points_k`); each item's `node.matter_frame` publishes only its run (`first`, `count`), so its surface and grains build from its own material. With several items and a Water, Smoke or Fire present, a `coupling_frame` publishes every point for the water, wet map, water on sand and the gases.
- **Cost.** One grid halves the grid-side work (static solid, grid update) and ends the static-solid spikes. The points all step at the stiffest material's rate: with hard-packed snow in the mix (h near 5) everything steps at packed snow's rate. Stress scene (sand, snow, jelly, mud, water, gas, colliders, bodies): Matter work 69 → 91 ms a frame with snow, 55 → 45 ms with the snow swapped for clay.
- **Not built.** Wall friction per point (the owner's is used at walls), Particles per Cell per item, a member's Bounds, and regional time steps (snow on its own faster step inside the shared grid).

## Kept open for the unified solver

- Material maths never reads the grid (section 2), so it moves onto FLIP's face grid unchanged; only the particle-to-grid transfer is rewritten.
- Every particle carries a material index (`affine_z.w`), so a mixed domain needs no record change.
- Material particles are never deleted for crowding or depth, only by Open faces, the kill height or a drain, because they carry F and q. FLIP's own deletion passes must key on water, never on every particle.
- The published frame stays the 32-byte `FluidParticle`; F and τ stay in Matter's record.
