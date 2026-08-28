# Layered Bokeh DoF — near/far field split inside `node.bokeh_gather`

**Status:** IN PROGRESS — P1 landed (signed CoC, no look change); P2 far-field dilation + P3 near field pending · 2026-08-28 · k3 (lead)
**Prerequisites:** the mip-gather + soft-ramp + coverage-fill chain (landed
2026-08-28: merges 4d6ee0b46, 40e4cbd00, bc4c20a6d)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs) and section 6 (acceptance demos) before starting any phase.

The single-pass gather shipped in CINEMATIC_POST D5 (occlusion-aware disc gather) has been incrementally
fixed (mip prefilter for speckle, soft occlusion ramp for tap-pattern
spray, coverage fill for halo dilution) and each fix held — but Peter's
music-video repro still shows a hard rim where a defocused bright region
meets an in-focus one. The rim is structural, not a tuning miss: a gather
redistributes light only among pixels whose own CoC is nonzero, so
in-focus pixels never receive light scattered from defocused neighbors.
Peter's verdict after walking the mechanism: "SOTA is what Manifold aims
for always." This doc is the layered scatter-as-gather design that removes
the rim by construction: separate the frame into near and far defocus
fields, blur each with correct energy bookkeeping, and composite in depth
order — the UE5-diaphragm-DoF / "A Life of a Bokeh" (SIGGRAPH 2018) class.

Governing constraint (load-bearing, verified): embedded graphs in saved
projects resolve `node.bokeh_gather` by type_id at load
(`crates/manifold-renderer/src/preset_runtime/core.rs:849` —
`fx.graph.unwrap_or(canonical_def)`). The split MUST live inside the atom
as internal passes, same as the mip chain did. Any design that rewires the
preset graph does not reach existing projects and is dead on arrival.

Companion docs: `docs/CINEMATIC_POST_DESIGN.md` (the D5 gather design this amends)
· `docs/ADDING_PRIMITIVES.md` (the `BarrieredReduction` exemption class the
atom already holds) · `docs/FREEZE_COMPILER_MAP.md` (fusion rules — this
atom stays a fusion boundary).

## 1. Audit — what exists (verified 2026-08-28, main @ bc4c20a6d)

| Piece | Where | State |
|---|---|---|
| `node.bokeh_gather` gather atom | `crates/manifold-renderer/src/node_graph/primitives/bokeh_gather.rs:178` (`run`) | Shipped. Internal mip chain (`mip_level_count`, :98), box-downsample prefilter, fractional-LOD tap sampling, soft 2px inclusion ramp, coverage-filled normalization |
| Gather body (codegen source) | `.../primitives/shaders/bokeh_gather_body.wgsl` (`body`) | Shipped; emitted standalone-only via `standalone_for_boundary_spec` (atom is `boundary_reason: BarrieredReduction`, bokeh_gather.rs:143) |
| Parity oracle | `.../primitives/shaders/bokeh_gather.wgsl` | Hand mirror, kept in lockstep |
| CPU reference + I1/I2/I3 | bokeh_gather.rs:504 (`build_mip_chain`), :547 (`bokeh_gather_texel`), :609 (`gpu_tests`) | I1 parity, I2 zero-CoC bit-clean passthrough, I3 anti-firefly numeric gate |
| `node.coc_from_depth` | `.../shaders/coc_from_depth_body.wgsl` | Shipped; thin-lens CoC, **unsigned** — `abs(D_mm - S_mm)` (:10). Output convention: R=G=B=\|coc\|/max_radius, A=1 |
| `node.coc_dilate` | `.../shaders/coc_dilate_body.wgsl` (`body`, :23) | Shipped; fixed 3×3 max (spreads CoC by **1px** — the BUG-137 (DoF depth-discontinuity seam) softener, never a halo-spreader) |
| CoC texture consumers | `rg 'coc_from_depth\|coc_dilate' crates/manifold-renderer/assets` | Only CinematicScene.json (and project-local forks embedding the same chain). Only `bokeh_gather.width` consumes the dilated field |
| Fusion status | `graph-tool fusion CinematicScene.json` | bokeh_gather unfused (`boundary:barriered_reduction`); partitions unchanged by this design |

Extend, don't redesign: the mip chain, the LOD sampling, the soft ramp,
and the coverage fill all survive unchanged — this design adds passes
around them.

## 2. Decisions

- **D1 — Signed CoC, packed backward-compatibly.** `coc_from_depth` drops
  the `abs()`: R keeps the MAGNITUDE (\|coc\|/max_radius, every existing
  reader unaffected), G gains the sign flag (1.0 = nearer than focus, 0.0 =
  far-or-in-focus), B stays a copy of R. Rationale: the layer split needs
  to know which side of the focal plane each pixel is on; the texture is
  internal to the coc→dilate→bokeh chain and only bokeh_gather consumes
  it, so the convention is free to extend. Rejected: a second coc texture
  output — changes the port shape, which ripples into every embedded graph.
  Rejected: sign in alpha — alpha is currently a documented 1.0 and several
  debugging paths treat the texture as opaque RGB.
- **D2 — The split is internal to `node.bokeh_gather`.** The atom grows
  internal passes (see section 3). It keeps `boundary_reason:
  BarrieredReduction` and the `standalone_for_boundary_spec` codegen path.
  Rejected: separate near/far atoms wired in the preset — the
  embedded-graph constraint (see above) makes it unreachable for saved
  projects. This is the plausible-wrong architecture: **you will want to
  fix this in the preset JSON — no, the atom must own it.**
- **D3 — Far field: full-radius CoC dilation + the existing gather.**
  Today's `coc_dilate` (1px max) is what confines halos to the silhouette.
  The far field needs the CoC spread outward by up to `max_radius` so
  in-focus background pixels near a defocused object start gathering and
  the halo feathers out. Implemented as a separable max (H then V, one
  extra internal pass pair), reading the magnitude channel of FAR-and-
  in-focus pixels only (near-field CoC must NOT leak into the far field —
  that is what would smear near bokeh across the focal plane in both
  directions). The square-vs-disc footprint error of separable max is
  accepted (mildly longer diagonal halos; the gather's own disc-overlap
  falloff dominates the visible falloff).
- **D4 — Near field: extract, dilate wider, gather, composite over.**
  Near-field pixels (sign flag = 1) are extracted with their own CoC,
  dilated by `max_radius` (near bokeh legitimately spills OVER the focal
  plane and over far content — that is the look real lenses give
  out-of-focus foreground), gathered with the same mip + ramp machinery,
  and composited over the far result using accumulated weight as coverage:
  `out = mix(far_result, near_gather.rgb, clamp(near_w_acc / 32, 0, 1))`.
  This is the pass that lets a defocused bright background's halo land on
  top of an in-focus foreground edge — Peter's rim, removed by
  construction.
- **D5 — Full res, current tap count.** 32 taps, full-resolution passes,
  one shared mip chain of `in`. Rejected: half-res near/far fields (the
  UE5 optimization) — the stated aim is look-first; halving reintroduces
  edge quantization around silhouettes. If the perf gate (section 5, P3) fails,
  half-res is the documented fallback, not a silent choice.
- **D6 — No new params.** The split needs no performer-facing controls;
  `max_radius` and `enabled` suffice. Rejected: a "near blur strength"
  param — unrequested surface; can be added later without breaking the
  design.

## 3. Design body

Internal pass sequence inside `BokehGather::run`
(bokeh_gather.rs:178), all textures cached per-instance like the existing
mip chain (resize-only rebuild, zero per-frame allocation; `enabled=false`
still aliases `in→out` before any pass runs):

1. **Mip chain** (unchanged): box-downsample `in` → shared prefiltered
   chain.
2. **Far dilation**: separable max of `width`'s R where G == 0 (far side +
   in-focus), radius `max_radius` → `far_coc` texture. The H pass writes a
   temp; the V pass reads it. Precedent for separable internal passes:
   `gaussian_blur_variable_width`'s H/V structure (as passes inside one
   atom, not as graph nodes).
3. **Near extraction + dilation**: threshold `width` (G == 1 → R, else 0)
   into `near_coc_raw`, then the same separable max → `near_coc`.
4. **Far gather**: the existing body, extended to take a per-pass CoC
   texture binding (here `far_coc`) — same taps, LOD, ramp, coverage fill.
   In-focus pixels inside the dilated far field now gather far taps and
   feather. The far gather's coverage fill term (center × sharpness gate)
   is unchanged.
5. **Near gather**: same kernel against `near_coc`, but the output is
   color + coverage (alpha = clamp(w_acc/32, 0, 1)) and the normalization
   is plain `acc/32` (no center fill — the near field is additive light
   over the composite, its own pixels' unscattered color is already in the
   far result).
6. **Composite**: `out = mix(far, near.rgb, near.a)` — D4's formula.

Kernel mechanics: passes 2–3 and 6 are small helper kernels
(`include_str!`, same pattern as the existing
`bokeh_mip_downsample.wgsl`); passes 4–5 are ONE codegen-generated body
(`standalone_for_boundary_spec`) parameterized by a `field` uniform
(far/near selects normalization+coverage behavior) so the CPU reference
and parity oracle stay single-source.

⚠ VERIFY-AT-IMPL: whether one generated body can take a second sampled
texture (the per-pass CoC field) under the current two-Gather-input ABI —
`bokeh_gather` already has exactly two Gather inputs (`in`, `width`), and
the far/near passes swap which texture backs `width`. If the body's
`fetch_width` binding can be bound per-dispatch (it can — run() owns the
bindings), no ABI change is needed. Check:
`crates/manifold-renderer/src/node_graph/freeze/codegen/standalone.rs`
(fetch emission).

CPU reference (`bokeh_gather.rs` `cpu_reference` module) grows matching
`dilate_max_1d`, `near_extract`, and composite functions; every gpu_test
fixture runs through the full internal pipeline, not the gather alone.

### Consequences, stated honestly

- Dispatch count per bokeh_gather instance goes from ~7 (mip chain +
  gather) to ~12–13 (chain + 2 dilations ×2 + 2 gathers + composite).
  At 1080p on the M4 Max this is comfortably sub-millisecond-to-~1ms; the
  DoF stage roughly doubles. Fine at 60fps; measure in P3's gate.
- Near-field dilation of 24px means defocused foreground spills up to 24px
  over sharp content. That is physically the look, but it softens hard
  graphic edges that cross the focal plane — performers driving graphic
  (non-scene) content through this atom will see it. Only CinematicScene-
  family generators wire this atom today.
- Two more cached full-res RGBA16F textures per instance (`far_coc`,
  `near_coc` + one dilation temp) — ~25MB at 1080p. Acceptable.

## 4. Invariants & enforcement

- The atom stays fusion-`Boundary` with a declared reason; runtime kernel
  still codegen-generated, never `include_str!`. Enforcement:
  `bokeh_gather::tests::boundary_atom_still_generates_standalone_kernel`
  (exists) + `every_boundary_atom_declares_its_reason` (classify.rs
  meta-test, exists).
- `enabled=false` remains zero-GPU-work (no dilation/gather passes run).
  Enforcement: `skip_passthrough_*` tests (exist) + a new gpu_test
  asserting the disabled path writes `out` without touching the cached
  internal textures.
- No per-frame allocation: internal textures are instance-cached,
  rebuilt on resize only. Enforcement: code shape (the mip-chain pattern
  already in `run`), plus the existing hot-path review at landing.
- CPU-reference parity for every new pass. Enforcement: I1 extended (P2,
  P3 gates).

## 5. Phasing

### P1 — Signed CoC end-to-end (no look change)

- **Entry state:** main contains the coverage-fill merge (bc4c20a6d).
  Prove: `git merge-base --is-ancestor bc4c20a6d origin/main`; re-read the
  audit table anchors.
- **Read-back:** coc_from_depth_body.wgsl (whole, 60 lines),
  coc_dilate_body.wgsl (whole), bokeh_gather_body.wgsl step 1–5 header,
  bokeh_gather.rs:504–610 (CPU reference + tests). Restate D1–D3 and the
  embedded-graph constraint before editing.
- **Deliverables:** `coc_from_depth` emits magnitude in R, sign flag in G
  (body + oracle + its cpu reference + tests updated); `coc_dilate` max
  propagates R and G independently (G max = "any near pixel in the
  neighborhood"); `bokeh_gather` reads R exactly as today (ignores G).
  No visual change anywhere.
- **Gate:** `scripts/gpu_proofs_gate.py` PASS (bokeh I1/I2/I3 + coc atoms'
  existing tests updated); `cargo nextest run -p manifold-renderer` green;
  clippy `-D warnings`; `graph-tool validate CinematicScene.json --kind
  generator` OK. Negative: `rg 'abs\(D_mm'
  crates/manifold-renderer/src/node_graph/primitives/shaders/coc_from_depth_body.wgsl`
  → zero hits.
- **Demo:** none — L1 (no user-visible change; the absence of change IS
  the gate, proven by I1/I2/I3 staying green).

### P2 — Far-field dilation: the rim fix

- **Entry state:** P1 landed. Prove: `rg -n 'sign' .../coc_from_depth_body.wgsl`
  shows the G channel write.
- **Read-back:** this doc D2–D3 + section 3; bokeh_gather.rs `run` (mip
  chain caching pattern — the new textures follow it); the fusion report
  (`cargo run -p manifold-renderer --bin graph-tool -- fusion
  crates/manifold-renderer/assets/reference-presets/CinematicScene.json` —
  bokeh stays a boundary).
- **Deliverables:** separable max-dilation helper kernel
  (`shaders/bokeh_coc_dilate_wide.wgsl`, include_str!, H+V passes,
  radius from the `max_radius` uniform); `far_coc` cached texture; far
  gather runs against `far_coc`; CPU reference `dilate_max_1d`. New gpu
  test **I4 (rim gate)**: bright rectangle (20.0) with coc 0.5 surrounded
  by in-focus black (coc 0) — assert the output BEYOND the silhouette is
  smooth and monotonically nonincreasing with distance (no cliff:
  per-step falloff bounded, nonzero glow at 0.5×max_radius out), and
  energy is conserved within tolerance. I1/I2/I3 updated and green.
- **Gate:** gpu_proofs_gate PASS; nextest; clippy; graph-tool
  validate/fusion. Negative: `rg 'textureSampleLevel\(tex_in, samp, tap_uv, 0\.0\)'
  .../bokeh_gather_body.wgsl` → zero hits (LOD sampling intact).
- **Demo:** L2 — one frame exported through the journey-proof harness
  (`run_headless_export`, the lane/bokeh-mip-gather temp-test pattern —
  load Peter's 'Right Where I Need You' project with
  `load_project_with(..., install_embedded_presets)`, export a live
  f_stop-10 window, extract PNG). Peter looks: the rim around defocused
  regions against the void must be gone. The temp test is deleted before
  landing, as before.

### P3 — Near field + composite: the layered look

- **Entry state:** P2 landed; I4 green.
- **Read-back:** this doc D4–D5 + section 3 passes 3, 5, 6.
- **Deliverables:** near extraction + dilation, near gather (coverage in
  alpha), composite pass; CPU reference extended; new gpu test **I5
  (occlusion sandwich)**: sharp dark bar (coc 0) crossing a defocused
  bright field (coc 0.6) — assert (a) the bar's interior keeps its own
  color (no dark fringe, no background bleed — max deviation from bar
  color bounded), and (b) the bright field's halo visibly overlaps the
  bar's edge (nonzero brightness 2px inside the bar boundary — the layered
  behavior the whole design exists for), and (c) the far side of the bar
  feathers smoothly.
- **Gate:** gpu_proofs_gate PASS; nextest; clippy; graph-tool. Perf:
  report the bokeh_gather dispatch count and a timing line from the
  export log (journey harness export of the same window as P2, compare
  wall time within 2.5× of the P2 run — exceed → escalate with numbers,
  the D5 half-res fallback is the named lever).
- **Demo:** L2 — same export, Peter looks: far halo feathers AND overlaps
  the sharp petals (his INFRA screenshot is exhibit A; the hard line where
  the model crosses focus must be gone).

## 6. Decided — do not reopen

1. The split is internal to `node.bokeh_gather`; no preset rewiring (D2).
2. CoC sign rides in the G channel; R stays magnitude (D1).
3. Full-res, 32 taps, shared mip chain; half-res is the named fallback if
   the P3 perf gate fails, never a silent choice (D5).
4. No new params (D6).
5. The gather keeps the mip prefilter, soft ramp, and coverage fill
   exactly as shipped; this design wraps passes around them.
6. The atom stays a fusion boundary with the codegen-generated kernel.

## 7. Deferred

- **Bokeh blade shaping (polygonal aperture)** — the disc stays circular.
  Revive if Peter asks for anamorphic/bladed highlights; the layered
  structure doesn't preclude it (tap pattern becomes aperture-shaped).
- **Half-res near/far fields** — the UE5 optimization. Revive only via the
  P3 perf gate failure path (D5).
- **Cat's-eye / vignette-weighted bokeh** (edge discs becoming ellipses) —
  needs per-pixel aperture distortion; revive on an explicit look request.
- **Applying the layered split to the `DepthOfField` effect preset** (the
  non-3D-scene DoF) — separate preset, separate decision; revive if the
  effect preset shows the same rim in use.
