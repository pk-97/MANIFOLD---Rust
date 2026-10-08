# Ocean Surface — a spectral open ocean out to the horizon, and a cliff cove that splashes

**Status:** SHIPPED · 2026-10-07 · D5 amended, D7 and D12 superseded by the look gate, see each · owed: BUG-2jka4 (Camera Sky seam at the HDRI wrap), BUG-3gwe9 (ripple detail normals and variance roughness), BUG-ra759 (Box3D hull assert on a cliff fragment) · Claude Opus 5.5 (lead)
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

The ocean is the industry's standard spectral ocean (Tessendorf 2001): a wind-driven
wave spectrum, advanced in time by the deep-water dispersion law, turned into height
and sideways displacement by an inverse 2D FFT, laid on a grid that covers exactly what
the camera sees. Three cascades of different tile sizes, each owning one band of
wavelengths, give swell, chop and ripples without visible tiling. Crest foam comes from
where the summed surface folds (the Jacobian of the sideways displacement). GPU FLIP
stays the near-field splash solver; tonight the two only share a scene. Coupling them
is section 9.

Peter's directives, load-bearing:
- "Don't retrigger with hacks, this sounds like useful infra and tooling … do this properly. Focused, no hacks … do not over node."
- "Metallic glass is quite dated, you can likely so much much better than that."
- Reuse the SWASH-era 3D FFT and its encode cache rather than rewriting it. The 2D FFT stays in manifold-gpu behind its backend boundary.

Companion docs: `DECOMPOSING_GENERATORS.md` section 2.5 (primitive audit) and its atom
rules; `ADDING_PRIMITIVES.md` (codegen path, proofs); `MANIFOLD_GPU_ARCHITECTURE.md`
(backend boundary); `LIQUID_SOLVER_SEAM_DESIGN.md` (the fluid roles the cove uses).

## 1. Audit — what exists (verified 2026-10-06)

Survey run: `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g "*.rs"`,
plus `rg -il 'ocean|tessendorf|jonswap|phillips'` over code, presets and docs (zero hits:
no ocean exists). Nearest reference preset read end to end: `MetallicGlass.json`
(grid → push → make_triangles → render). Peter called that look dated; it is a wiring
reference only.

| Piece | Where | State | Verdict |
|---|---|---|---|
| Multi-dimensional FFT (`GpuFft::new_nd`, real-output inverse, tensor-data cache) | `git show c21f9f2ce:crates/manifold-gpu/src/metal/fft.rs` (branch `feat/fft-encode-cache`) | proven on SWASH 64³ against a direct f64 DFT; removed from main by `75037de36` when the water solve stopped using it | **exists** — restore whole |
| 1D FFT on main | `crates/manifold-gpu/src/metal/fft.rs` | r2c/c2c 1D only, no callers | replaced by the restore |
| Grid of points in XZ | `generate_grid_mesh.rs` (`node.grid_mesh`, Source codegen) | fixed world-size grid, UV 0..1 | not usable: a fixed grid can't reach the horizon at constant screen density (D3) |
| Camera into a buffer atom | `flatten_to_camera_plane.rs:84-103` (`derived_uniforms` + `recompute` from `ctx.camera`) | shipped | **pattern exists** for the grid and displace atoms |
| Buffer Source fusion | `freeze/region.rs:1587` (`arr_in < 1` → `Boundary`) | a buffer Source never joins a region | the projected grid runs as its own kernel, like `node.grid_mesh` (D3) |
| Coincident mesh + gathered side arrays | `clamp_liquid_to_solids.rs` (`input_access: [Coincident, BufferGather, BufferGather]`) | shipped | **pattern exists** for ocean_displace |
| Complex element type | `grid_uv_field.rs` (`Array([f32; 2])`, codegen `Element { x, y }`) | shipped | **exists** — the spectrum's element |
| Grid → triangle list with normals | `triangulate_grid.rs` (`node.make_triangles`, BufferGather, finite-difference normals) | shipped | **exists** — gives the normal of the displaced surface (D5) |
| PBR water, IBL, alpha mask | `pbr_material.rs`, `render_scene.wgsl` (`resolve_albedo` = base × map × vertex colour; Mask mode discards under `alpha_cutoff`) | shipped | **exists** |
| HDRI sky | `hdri_source.rs` (path via preset string binding) | shipped | **exists** |
| glTF mesh + textures | `gltf_mesh_source.rs`, `gltf_texture_source.rs` | shipped | **exists** for the cliff scan |
| Sine wave field | `wave_field_3d.rs` | one travelling sine | not spectral; no reuse |
| Spatial mask | `mesh_spatial_mask.rs` | Band/Sphere/Half Space weights, normalised by scene radius | no box, writes weights not alpha; not a fit (D7) |
| Vertex colour writer for meshes | — | none (`rg 'type_id: "node\.[a-z_]*' | rg -i 'colou?r|paint|tint'`) | genuinely new need, folded into D6 |
| Per-face open/closed liquid box | `gpu_flip_domain.rs:408-413` (`closed_neg_x` … `closed_pos_z`) | shipped | **exists** |
| Animated inflow push | `fluid_role_source.rs:369` publishes historical controls; `preset_runtime/physics_sampling.rs:37,278,509` replays velocity ancestry (LFO allowed) at each tick's sample time; `liquid/bodies.rs:499` keeps velocity out of rebuild identity, `:636` consumes tick-start controls | shipped | **one wire away**: an LFO into `velocity_x`. Needs a proving test at the preset-runtime seam (P5), no new mechanism |
| Cinematic chain | `free_camera`, `camera_lens`, `atmosphere`, `coc_from_depth`, `bokeh_gather`, `motion_blur` | shipped, used on `feat/seawall-gpuflip` | **exists** |

Extend, don't redesign. Genuinely new: three buffer atoms (`node.ocean_spectrum`,
`node.projected_grid`, `node.ocean_displace`), one small mask atom (`node.cut_out_box`),
and one library-call node (`node.inverse_fft_2d`).

## 2. Decisions

**D1 — Spectrum: JONSWAP with Donelan-Banner spreading, deep water.** Equations are
pinned in section 3.1. Spreading follows Horvath 2015 ("Empirical directional wave
spectra for computer graphics"; reference implementation EncinoWaves `Spectra.h`,
`DirectionalSpreading.h`). Dispersion is deep water, ω = √(g|k|).
Rejected: Phillips spectrum (Tessendorf's original), because it has no peak shape and
reads as noise. Rejected: Mitsuyasu cos²ˢ spreading, because its normalisation needs
Γ-functions in WGSL for no visible gain. Rejected: a depth parameter, because shallow
water needs the TMA correction too, and nothing tonight is shallow.

**D2 — One spectrum atom per cascade produces the spectrum at time t.** `node.ocean_spectrum`
evaluates, for every wavevector of its band, h(k,t) = h₀(k) e^{−iωt} + conj(h₀(−k)) e^{+iωt}
and one of the six derived fields of section 3.1. With the inverse's +ik·x this
travels toward +k, so waves run downwind. h₀ is drawn independently at every signed
lattice index from a deterministic hash; h₀(−k) is the same function evaluated at the
negated index. Recomputing h₀ every frame costs about 100 flops per element, about
20 µs a frame for three cascades.
Rejected: a separate h₀ atom plus a time-evolution atom. Both would run every frame, and
fusion would merge them back into one kernel, so the split would only add a node.

**D3 — The surface is a projected grid (Johanson 2004).** `node.projected_grid` is a
Source atom. Each vertex is a fixed screen-space grid point, cast as a camera ray onto
the water level and clamped at Max Distance. Its spacing is constant on screen,
roughly 3 px at the default 640 × 360, from the camera's feet to the horizon. It runs as
its own kernel because buffer Sources don't fuse today (section 1). That costs one
18 MB write and read, about 0.05 ms.
Rejected: a camera-centred polar disc. At a constant angular count its screen spacing is
about 20 px for 512 segments; about 6 px needs 3M vertices. Rejected: clipmaps or
quadtree LOD (Crest, Unreal). They are right for a free-roaming game camera but need a
CPU patch-selection pass; our camera rigs are slow.
Consequence, stated honestly: while the camera pans, vertices slide over the waves.
P4 checks that with frozen ocean time.

**D4 — Three banded cascades, every one sampled at the rest position.** The tile sizes
are 1000 m, 167 m and 27 m, each ratio about 6 and none an integer, so the repeats
never line up. Cascade c holds |k| ∈ [k_lo,c, k_hi,c) with k_hi,c = k_lo,c+1 = 6 · 2π / L_c+1;
the first cascade starts at 0 and the last ends at Nyquist. That way no wavelength is
counted twice (the standard banding of production FFT oceans). Every cascade samples
at the vertex's rest position, which the grid writes to `uv` in metres.

**D5 — Normals come from `node.make_triangles`, not from slope fields.** Finite
differences over the displaced projected grid give the normal of the choppy surface,
including its sideways compression, at about 3 px. That drops the two slope fields,
leaving six fields per cascade.
Rejected: analytic normals from slope fields, because they need two more FFT fields per
cascade and alias wherever waves are finer than the grid. Consequence, stated honestly:
no ripples finer than the grid, and the far field is smoother than the real sea, because
unresolved slope variance doesn't reach roughness. P6's trigger revives a detail normal
map and variance roughness.
**Amended (2026-10-07, look gate): heights and sideways shifts are sampled Catmull-Rom.**
Bilinear sampling made the surface flat within each cascade sample, so wherever a grid
cell is smaller than a sample (a low camera, the ripple cascade's 10.5 cm samples within a
few metres) the finite-difference normals showed square facets. Catmull-Rom passes
through every sample and keeps the slope continuous, so the facets go and the far field
keeps the finite difference's averaging. The three fold fields only drive foam and stay
bilinear. Slope-field normals were tried again in a side-by-side and rejected for the
reason above: smooth near the camera, but they sparkle at the horizon without footprint
filtering, which is the variance-roughness work this decision already defers
(BUG-3gwe9 (ripple detail normals and variance roughness)).

**D6 — Foam is the summed surface's fold, painted into vertex colour.**
`node.projected_grid` writes its Colour param, the water's own albedo, into vertex
colour. `node.ocean_displace` sums every cascade's horizontal deformation
(Dxx, Dzz, Dxz, each × λ × fade), evaluates the Jacobian once, and mixes the vertex
colour toward white by smoothstep(Foam Threshold, Foam Threshold − Foam Width, J). The
material's base colour stays white, so albedo = vertex colour. The fade's own gradient
term (displacement ⊗ ∇fade, about 1% at a 100 m fade) is neglected.
Rejected: a separate foam painter. Vertex colour is the only per-vertex channel the
renderer reads, so a separate painter would need a side channel hidden in another vertex
slot. Consequence: foam is glossy, because roughness is per material.

**D7 — The FLIP tank is cut out of the ocean by `node.cut_out_box`.** A Pointwise mesh
atom multiplies vertex alpha by 0 inside a world box, with a small feather, and the ocean
material uses alpha mode Mask. It is generic: it hides any mesh inside a box.
Rejected: a Box shape in `mesh_spatial_mask` plus a new weights-to-alpha atom, because that
is two nodes, and that mask's positions are normalised by scene radius. Rejected for
tonight: a per-fragment world-space clip in the renderer. That is renderer work, and the
vertex mask's error is bounded. Consequence, stated honestly: the cut edge is accurate to
one grid cell, about 3 px, and that edge moves with the camera. The trigger for the
fragment clip is the seam visibly crawling in a still pair.
**Superseded for Ocean Cliff (2026-10-07, look gate).** With the cut, any height
difference between the tank and the ocean shows along the cut's straight edges: the
stills read as a raised slab of water with rectangular sides. Ocean Cliff instead rests
the cove 0.35 m under the ocean's mean surface and cuts nothing. The ocean covers the
tank; FLIP water shows only where a surge rises out of the sea, and the edge is where the
two surfaces cross. `node.cut_out_box` stays as a generic atom with no preset using it.

**D8 — The inverse FFT is one library-call node.** `node.inverse_fft_2d` takes
`Array([f32; 2])` half spectra of shape [B, N, N/2+1] and returns `Array(f32)` real fields
[B, N, N] through `GpuFft::new_nd(HermiteanToReal, [B, N, N], [1, 2])`. That is one MPSGraph
call per frame, and the plan is rebuilt only when N or B changes. It declares
`boundary_reason: IoBridge`, the FFI-bridge class (`freeze/classify.rs:388`): a
backend-library call, not a per-element kernel. The inverse's 1/N² scale is undone in
the spectrum atom, so the field is Σ h(k) e^{ik·x}.
Rejected: our own compute FFT (`feat/own-fft` prototype), because MPSGraph is proven here
and reuse comes first.

**D9 — One displace atom samples every cascade.** `node.ocean_displace` reads the mesh
coincidently and gathers the three cascades' fields, with wrap, at rest_xz / L_c:
Catmull-Rom for Dy, Dx, Dz and bilinear for the fold fields (D5 amendment). It writes position = in.position + Σ_c fade_c · (λ·Dx_c, Dy_c, λ·Dz_c),
and the foam of D6. fade_c = 1 − smoothstep(Fade Start_c, Fade End_c, |rest_xz − camera_xz|),
so short waves vanish before the grid stops resolving them.
Rejected: one displace node per cascade. The fold test needs the summed deformation, so
chained nodes would have to hand partial sums down in a spare vertex slot. That hidden
state is the sign that the cut is too fine, per DECOMPOSING_GENERATORS.md section 1.2 (Engine internals are stage nodes).

**D10 — Cards are preset bindings, never extra nodes.** Wind Speed, Wind Direction,
Choppiness (λ), Wave Size (amplitude multiplier, applied once, in the spectrum), Swell
Speed (time multiplier) and Foam (threshold) are preset parameter bindings, fanned out
to the atoms.

**D11 — Assets: Poly Haven CC0, one cliff scan, one ocean HDRI.** They live under the
gitignored `crates/manifold-renderer/tests/fixtures/` (gltf/, hdri/), referenced by string
binding defaults. Each file's name, URL and size is logged in section 8. Shipped presets
have no relative-path resolution for bundled assets. That is a `decision` bead for
Peter (a bundled asset library), not a blocker tonight.

**D12 — Surges are an LFO on the inflow's velocity.** The cove's inflow is a
`fluid_role_source` (role Inflow) on the sea-facing face, with `velocity_x` wired from an
LFO. History replay samples it at every tick's start, so the push is the same at any
frame rate and never resets the liquid. Rejected: Reset re-triggers or keyframed restarts
(Peter: "no hacks").
**Superseded (2026-10-07, look gate): surges are an LFO on a wave-maker paddle.** An
inflow is a source only (FLIP Fluids semantics): it emits to stay full and sets the
velocity of the water it holds, so it pushes water in but never draws any back. Every
variant tried (whole face, surface band with undertow, spillway outflows) gained water
each cycle and mounded the cove; the open sides then needed level-holding bands that
showed as steep walls. Ocean Cliff now uses the standard wave-flume generator: a collider
wall across the sea end, its transform's `pos_z` driven by an LFO (stroke ±0.7 m, 2.8 s,
between the cove's first two slosh periods of about 3.8 and 1.9 s). It moves water
without adding any, the domain replays its pose at each tick's start, and the tank is
closed on every face but the top. The side walls sit inside rock: the scan's
neighbouring pieces, turned to face into the cove, make it a gully, render, and collide.

## 3. Design body

### 3.1 Field layout and math (committed)

The spectrum for one cascade is `Array([f32; 2])` of length 6·N·H, H = N/2 + 1. Element
idx → field f = idx / (N·H), then row m = (idx mod N·H) / H, column n = idx mod H.
Signed indices: ix = n (0..N/2), iz = m < N/2 ? m : m − N. The wavevector is
k = (2π/L)(ix, iz), so column is x and row is z. The real field output is [6, N, N],
row-major, with row j at z = j·L/N and column i at x = i·L/N.

With h = h(k,t), |k| > 0, every value is multiplied by N² (D8):

| f | field | spectrum |
|---|---|---|
| 0 | Dy (height) | h |
| 1 | Dx | i·kx/|k| · h |
| 2 | Dz | i·kz/|k| · h |
| 3 | Dxx = ∂Dx/∂x | −kx²/|k| · h |
| 4 | Dzz = ∂Dz/∂z | −kz²/|k| · h |
| 5 | Dxz = ∂Dx/∂z | −kx·kz/|k| · h |

These signs make crests sharpen: the single wave h = A cos kx gives Dx = −A sin kx, so
dx′/dx = 1 − λAk at the crest. The Jacobian of the summed surface is
J = (1 + ΣλDxx)(1 + ΣλDzz) − (ΣλDxz)²; J < 0 is a fold. k = 0, the Nyquist row (iz = −N/2)
and the Nyquist column (ix = N/2) are zero in every field, because a Nyquist mode's
derivative has no Hermitian partner.

Amplitude, so that the field's variance is Σ S_k Δk²:
h₀(k) = ½ (ξ₁ + iξ₂) · √(S_k) · Δk, with Δk = 2π/L and S_k = S(ω) · D(ω, θ) · (dω/dk) / |k|,
dω/dk = g / (2ω). ξ₁, ξ₂ are independent standard normals by Box-Muller from a PCG hash of
(seed, ix, iz), with ix, iz the signed full-lattice indices. Outside [k_lo, k_hi), h₀ = 0.

JONSWAP, with U = max(Wind Speed, 0.1) m/s, F = 1000 · Fetch (km → m), g = 9.81:
α = 0.076 (U² / (F g))^0.22, ω_p = 22 (g² / (U F))^(1/3), γ = 3.3,
σ = 0.07 for ω ≤ ω_p else 0.09, r = exp(−(ω − ω_p)² / (2 σ² ω_p²)),
S(ω) = α g² ω⁻⁵ exp(−1.25 (ω_p / ω)⁴) γ^r.

Donelan-Banner, with θ = wrap_to_(−π, π](atan2(kz, kx) − Wind Direction) and ρ = ω / ω_p:
β = 2.61 ρ^1.3 for ρ < 0.95; 2.28 ρ^−1.3 for 0.95 ≤ ρ < 1.6; 10^(−0.4 + 0.8393 exp(−0.567 ln ρ²))
above. D = β / (2 tanh(βπ)) · sech²(βθ). For β < 1e-4 it takes the uniform limit
1 / (2π).

### 3.2 Atoms (committed signatures)

```
node.ocean_spectrum   Source, Array([f32; 2]) out "spectrum"   [codegen, Source, frame_time_inputs ["time"]]
  inputs (port-shadowed ScalarF32): wind_speed, wind_direction (deg), wave_size, swell_speed, time
  params: size Int (N, power of two 16..1024, default 256), tile_size (m), band_low,
          band_high (rad/m; band_high 0 = Nyquist), fetch (km, default 300), seed Int
  capacity: 6·N·(N/2+1)

node.inverse_fft_2d   Array([f32; 2]) "spectrum" → Array(f32) "field"   [IoBridge, MPSGraph]
  params: size Int (N), batch Int (B, default 6)
  capacity: B·N·N. Refuses a spectrum or field buffer too small for B×N×N with ctx.error.

node.projected_grid   Source, Array(MeshVertex) out "vertices"   [codegen, Source; derived camera uniforms]
  inputs: camera Camera required; params: columns Int (640), rows Int (360),
          level (m), max_distance (m, 20000), margin (0.25), color_r/g/b (water albedo)
  vertex: position on the plane, normal +Y, uv = rest xz (m), uv1 0, tangent 0, color (rgb, 1)

node.ocean_displace   Pointwise, Array(MeshVertex) "mesh" → "out"
                      [codegen, input_access [Coincident, BufferGather ×3]; derived camera uniforms]
  inputs: mesh, field_0..field_2 required (Array(f32)), camera Camera required,
          choppiness, foam_threshold (port-shadowed)
  params: per cascade c: size_c Int, tile_size_c, fade_start_c, fade_end_c; foam_width

node.cut_out_box      Pointwise, Array(MeshVertex) "mesh" → "out"   [codegen]
  params (port-shadowed): center_x/y/z, size_x/y/z, feather
```

The whole surface: projected_grid → ocean_displace (fed by three spectrum →
inverse_fft_2d pairs) → cut_out_box → make_triangles → render_scene object. The displace
and cut atoms fuse into one buffer kernel. make_triangles gathers, so it starts another.

### 3.3 Projected grid geometry

The camera ray for screen point (sx, sy) is r = fwd + sx·tan(fov_x/2)·right + sy·tan(fov_y/2)·up.
Columns span sx ∈ [−(1 + margin), 1 + margin]. Rows span sy from −(1 + margin) up to
y_top. y_top is the horizon at range R = Max Distance: the largest sy, over the two side
edges, where r.y / |r.xz| = −height / R (height = camera y − level). That covers roll,
which tilts the horizon. It is found by bisection in Rust, once per frame, and passed as
a derived uniform. If the screen's top edge still meets the water inside R (looking
down), y_top = 1 + margin. If its bottom edge doesn't (looking up, or the camera below
the level), the grid collapses onto the R circle and draws nothing visible. Any ray that
misses the plane or lands past R is clamped to R along its horizontal direction. Margin
0.25 covers crests rising from below the frame and chop pulling vertices in from the
sides. P4's stills at maximum Choppiness check the frame edges.

### 3.4 Look (Ocean preset)

Opaque PBR: base colour white, roughness about 0.06, ior 1.333, no transmission. The
HDRI drives IBL and the background, with a sun light matched to the HDRI's sun. Haze
comes from `node.atmosphere`. The camera chain is free_camera → camera_lens →
coc/bokeh → motion_blur → filmic, as on the Sea Wall cinematic pass.

## 4. Invariants & enforcement

1. **The inverse is the direct sum.** Enforcement: `gpu_tests::ocean_field_matches_direct_sum`
   runs the spectrum atom and then the GPU inverse at N = 16, against a CPU f64
   Σ h(k) e^{ik·x} for all six fields, max error 1e-4 of the field's peak. It also checks
   that column 0's ±kz pairs are conjugate.
2. **The variance is the spectrum's.** Enforcement: CPU test `field_variance_matches_jonswap`
   checks that Σ|h(k,0)|² averaged over 16 seeds is within 5% of Σ S_k Δk² over the band.
   A direct-sum test alone would pass with wrong amplitudes.
3. **Waves run downwind and crests sharpen.** Enforcement: CPU test `single_wave_runs_downwind_and_sharpens`.
   One wavevector along +x gives Dx = −A sin kx and J = 1 − λAk at the crest, and its phase
   moves toward +x as t grows.
4. **Bands never double-count.** Enforcement: CPU test `cascade_bands_partition_k` checks
   that every |k| on a fine lattice falls in exactly one of the preset's three bands.
5. **Each atom is on codegen, with value and fused proofs.** Enforcement:
   `gpu_tests::{ocean_spectrum, projected_grid, ocean_displace, cut_out_box}_matches_cpu`,
   plus a fused-vs-unfused proof for displace → cut. Machine check: `graph-tool fusion
   Ocean.json` shows displace and cut in one region.
6. **The grid reaches the horizon under roll and pitch.** Enforcement: CPU test
   `projected_grid_horizon_cases`. Level, rolled 20°, looking down and looking up: the top
   row's two side-edge vertices land within 1% of R, or the whole screen is water, or the
   grid collapses, as section 3.3 says.
7. **The FFT stays behind manifold-gpu.** Enforcement: `rg 'MPSGraph|objc2_metal_performance'
   crates/manifold-renderer` returns zero hits.
8. **The paddle is replayed per tick from its wire, never by Reset.** Enforcement:
   `preset_runtime::physics_sampling::tests::ocean_cliff_paddle_is_replayed_per_tick`
   loads OceanCliff.json and checks that the paddle's LFO, transform and collider role and
   the domain are in the per-tick passes, and the renderer, rock and sea are not. That a
   collider's row is its authored pose at each tick's start at 20, 24, 30 and 60 fps is
   `liquid::bodies::tests::liquid_body_rows_match_at_every_frame_rate`.
9. **Live at 60 fps at 1080p.** Enforcement: a measured number in the landing commit,
   from the release-build capture's GPU frame time (P4). It is a gate at landing, not a
   standing test.

## 5. Phasing

The lead runs every phase tonight, in this order, in slot `feat/ocean-cliff`.

**P1 — FFT restore + `node.inverse_fft_2d`.** Entry: `git show c21f9f2ce:crates/manifold-gpu/src/metal/fft.rs` exists.
Read-back: D8, invariant 7. Deliverables: restored `fft.rs` (with its tests),
`inverse_fft_2d.rs`. Gate: the restored FFT tests pass on the GPU queue, clippy is
clean, and invariant 7's `rg` returns zero hits.

**P2 — `node.ocean_spectrum`.** Read-back: D1, D2, D4, section 3.1. Deliverables: atom
and body WGSL, a CPU reference in the test module, invariants 1–4. Gate: those tests,
plus the GPU value proof.

**P3 — `node.projected_grid`, `node.ocean_displace`, `node.cut_out_box`.** Read-back: D3,
D5–D7, D9, section 3.3. Deliverables: the three atoms, invariants 5–6, the fused proof.
Gate: tests green, plus the `graph-tool fusion` output.

**P4 — Ocean preset.** Deliverables: `Ocean.json` (cards per D10), NODE_CATALOG
regenerated. Gate: `check-presets` and `graph-tool validate` pass; release GPU time at
1080p is under 16.7 ms (reported). Acceptance demo (L2): fluid_capture stills from
three angles and at maximum Choppiness, plus a pan with frozen ocean time to check
sliding, all looked at.

**P5 — Assets + Ocean Cliff preset.** Deliverables: downloads logged in section 8;
`OceanCliff.json`: ocean, cliff scan, HDRI sky, and GPU FLIP tank in the cove. As built
(D7, D12 superseded): the tank is closed but for its top, its side walls sit inside the
turned rock of the gully, and a paddle at the sea end makes the surges. Invariant 8's
test. Gate: the test, check-presets, validate. Acceptance demo (L2): a 15 s 1080p30 MP4
at true Speed, plus stills.

**P6 — Look pass.** Fix at the root whatever stands between the stills and a cinematic
shot. Known going in:
(a) Airborne liquid flickers as shards. The anisotropic kernel engages at 8 neighbours
(`shape_particle_blobs` `min_neighbours`) where Yu & Turk 2013 use N_ε = 25, and Sea Wall
runs Stretch 6, above their k_r = 4.
(b) Whitewater spray doesn't motion-blur; suspect: instanced particles write no velocity.
(c) Sea Wall's sky is black: environment select 0.
Triggers: far field reads as smooth plastic → detail normal map and variance roughness
(D5); seam crawls between frames → fragment clip (D7). Every fix gets a test at its seam.

**P7 — Renders, review, landing.** Hero shots at FLIP Resolution 128, three angles of 30 s
or more each, 1080p30, true Speed, phone copies under 30 MB. Astra adversarial review of
the final diff, then `landing_gate.py`, then merge.

## 6. Decided — do not reopen

1. JONSWAP, Donelan-Banner spreading, deep water; equations as section 3.1.
2. One spectrum atom per cascade; h₀ recomputed every frame; e^{−iωt} on h₀(k).
3. Projected grid, its own kernel; not a disc or clipmaps.
4. Three banded cascades of 1000, 167 and 27 m, sampled at rest xz held in `uv`.
5. Normals from make_triangles; six fields per cascade.
6. Foam from the summed Jacobian, painted into vertex colour; the grid owns the water colour.
7. One displace atom for exactly three cascades, all three inputs required.
8. ~~Tank cut out with `node.cut_out_box`~~ — reopened by the look gate: the cove rests under the ocean (D7).
9. MPSGraph inverse through restored `GpuFft::new_nd`, IoBridge; no MPS outside manifold-gpu.
10. ~~Inflow surges are an LFO on velocity~~ — reopened by the look gate: an LFO on a paddle's pose, replayed per tick (D12).

## 7. Deferred

- **Buffer Source fusion** (`region.rs:1587`): revive when a Source's own write shows in a
  profile. The projected grid would fuse with its displace.
- **Foam persistence** (foam that lingers and decays): revive when stills show crest foam
  vanishing too abruptly. Needs cross-frame state.
- **Detail normal map and variance roughness**: revive on P6's trigger.
- **Fragment-accurate tank clip**: revive on P6's trigger.
- **Footprint-driven fade** (fade by projected cell size instead of distance): revive when
  a rig's height changes enough that distance fades alias.
- **Camera-follow LOD for flying cameras**: revive when a rig flies faster than the grid
  can hide its sliding.
- **Vulkan `GpuFft`** (VkFFT behind the same API): owed with the Vulkan backend
  (`VULKAN_BACKEND_DESIGN.md`).
- **A generic image-space FFT node** (convolution bloom): revive on the first effect that
  needs it.
- **Shallow water** (TMA spectrum, depth dispersion): revive when a scene needs waves
  that feel the seabed.

## 8. Assets

Both Poly Haven, CC0, downloaded 2026-10-07 into the main checkout's gitignored
fixtures (the preset string bindings default to these paths).

| File | URL | Size |
|---|---|---|
| `tests/fixtures/hdri/umhlanga_sunrise_4k.exr` (Greg Zaal) | https://dl.polyhaven.org/file/ph-assets/HDRIs/exr/4k/umhlanga_sunrise_4k.exr | 19.6 MB |
| `tests/fixtures/gltf/coastal_cliff_02/coastal_cliff_02_2k.gltf` (Rob Tuytel; 41 × 8.7 × 10 m, 1.77M triangles) | https://dl.polyhaven.org/file/ph-assets/Models/gltf/2k/coastal_cliff_02/coastal_cliff_02_2k.gltf | 3 KB |
| `…/coastal_cliff_02.bin` | https://dl.polyhaven.org/file/ph-assets/Models/gltf/8k/coastal_cliff_02/coastal_cliff_02.bin | 26.8 MB |
| `…/textures/coastal_cliff_02_{diff,nor_gl,arm}_2k.jpg` | https://dl.polyhaven.org/file/ph-assets/Models/jpg/2k/coastal_cliff_02/ | 2.7 + 4.1 + 2.3 MB |

## 9. Ocean → FLIP coupling (out of scope tonight)

The standard way is one-way forcing at the tank's open faces: sample the ocean's height
and orbital velocity (from the same spectrum, ∂/∂t of the displacement) along the sea
face, and drive the inflow's level and velocity from them per tick. The FLIP water then
matches the incoming swell at the seam. Blending the two surfaces near the seam is the
other half. The seam infrastructure already exists: inflow roles are replayed per tick
(D12). What's missing is a sample of the ocean at points, fed into the role source.
Revive when a shot shows the seam.
