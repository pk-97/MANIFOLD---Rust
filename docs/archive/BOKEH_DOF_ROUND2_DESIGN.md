# Bokeh DoF Round 2 — tent-shaped halo fields + half-res dilation

**Status:** SHIPPED 2026-08-29 — tent far / sqrt-near fields + half-res dilation landed; Peter runs the live look-check himself (L4 direct, his explicit call) · k3 (lead)
**Prerequisites:** BOKEH_LAYERED_DOF_DESIGN shipped (P1–P3 landed 2026-08-28).
**Execution contract:** docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs).

Peter's L4 verdict on the shipped layered DoF: it works — the rim is gone —
but it is very performance heavy, blocky/smeary, and shows areas where the
halo cuts off or looks disconnected. Root cause (mechanism analysis, agreed
as the direction before building): the P2/P3 dilation is a FLAT separable
max — it stamps the full neighborhood CoC everywhere in a `max_radius`
window, so the field steps to zero at the window edge (the cutoff), its
square footprint bands the gather radius (the blockiness), and four
full-res 49-tap passes cost more texture reads than both gathers combined
(the perf). Smear is the box-filter color mip, deferred here.

## Decisions

- **D1 — Tent (distance-decayed) dilation.** The dilation kernel propagates
  `max(sample.r - |k| * decay)` instead of `max(sample.r)`, with
  `decay = px_per_tap / max_radius` (normalized CoC units). A source pixel
  with coc_frac `c` now influences the field out to exactly `c * max_radius`
  px, falling linearly to zero — no outer cliff, no square banding, and the
  reach becomes proportional to the source's own defocus. Same pass
  structure (H then V, separable), same tap count, same G-mask semantics
  (far: R where G==0; near: extracted, G cleared). Rejected: blur-the-field
  afterwards (extra pass, softens the near/far separation); rejected:
  jump-flooding distance transform (more passes, no benefit over linear
  decay for a single-kernel halo).
- **D2 — Dilation chain at half res.** `near_coc_raw`, `far_coc`,
  `near_coc`, `dilation_temp` become half-res RGBA16F (w/2, h/2). The tent
  field is smooth by construction, so bilinear upsample-on-read is lossless
  for this use: the gather bodies already read the field via
  `textureSampleLevel` with a linear sampler — binding a half-res texture
  needs NO body change. Extract + dilation passes drop to quarter cost.
  Gathers stay full res in this round.
- **D3 — No new params, no ABI change, atom stays the owner.** Same
  constraint as the parent design: everything internal to
  `node.bokeh_gather`; `enabled=false` still aliases before any pass.
- **D4 — Half-res gathers and the empty-near skip are DEFERRED**, not
  chosen silently. Triggers: Peter reports the DoF stage is still too heavy
  after D1+D2 (half-res near/far gathers), or a profile shows the near
  pipeline dominating in near-empty shots (per-frame skip via reduction).
  Smear fix (Kawase/dual-filter downsample replacing box in the color mip
  chain) is deferred with trigger: still reads smeary after D1.

- **D5 — Far field linear, near field sqrt (added at execution, Peter
  2026-08-29).** The linear tent applied to the NEAR field halves the
  foreground spill: a pixel at distance `d` from a defocused source gets
  CoC `c(1 - d/cR)`, whose gather disc reaches back to the source only when
  `d ≤ cR/2` — the layered halo-overlap look (BOKEH_LAYERED_DOF P3,
  Peter-approved) collapsed, and the lane's first response was to weaken
  I5's overlap assertion to match (rejected in lead review; the test's
  intent is restored, not the numbers). The near field therefore uses a
  sqrt fade — same reach, same kernel with a `shape` uniform — holding
  ~70% strength at mid-reach so the disc still reaches back. I5's fixture
  also drops its 8px bright-to-bar gap (the P3 brief says the bar CROSSES
  the field; the gap was lane-invented) and widens the interior margin to
  12px to sit past the sqrt spill. Rejected: near field flat (keeps the
  hard outer edge Peter reported).

## Design body

Changes, all inside `BokehGather::run` + its helpers + cpu_reference:

1. `bokeh_coc_dilate_wide.wgsl`: uniform gains `decay: f32`; the tap
   contribution becomes `sample.r - f32(abs(k)) * decay` (still masked by
   the G==0 far test). run() passes `decay = 2.0 / max_radius` (half-res
   taps step 2 full-res px).
2. Half-res field textures in the instance cache (resize-only rebuild
   unchanged); extract kernel writes half-res `near_coc_raw` (reads the
   full-res signed CoC with its existing sampler — no kernel change beyond
   dst size); dilation H/V run at half res; gathers bind the half-res
   fields to `tex_width` unchanged.
3. CPU reference mirrors exactly: half-res `dilate_max_1d` with decay,
   bilinear sample of the half-res field at gather time.
4. Test recalibration, honestly: I4's rim gate keeps its purpose (halo
   must extend past the silhouette with smooth monotonic falloff, energy
   conserved) but the "nonzero glow at 0.5×max_radius" assertion moves to
   inside the tent reach (e.g. nonzero at 0.25×max_radius for the 0.5-CoC
   fixture, whose reach is 0.5×max_radius) plus a new assertion that the
   falloff REACHES ~zero at the reach with no step (the cutoff regression
   gate). I5 unchanged in intent; thresholds re-derived if the tent shifts
   them. I1/I2/I3 through the full pipeline as before.

Perf expectation: 4 of 13 passes at quarter cost; dilation was the largest
texture-read consumer (~196 reads/px vs ~128 for both gathers).

## Phase

Single phase P1 (one lane, one commit):
- **Deliverables:** the four design-body items; gates: clippy
  `-D warnings` (renderer), `cargo nextest run -p manifold-renderer`,
  `scripts/gpu_proofs_gate.py` PASS, graph-tool validate/fusion OK, bokeh
  stays `boundary:barriered_reduction`. Report dilation-pass resolution +
  test wall time vs the pre-change tree.
- **Demo:** none by Peter's explicit call — he runs the live check himself
  on his project after landing and reports back (L4 direct; the L2 harness
  round is skipped at his instruction, recorded here so the gap is a
  decision, not a debt).

## Decided — do not reopen

1. Tent decay is linear in normalized CoC units with decay = px_per_tap /
   max_radius for the far field; the near field uses the sqrt fade (D5).
2. Fields half-res, gathers full-res this round (D2).
3. Everything stays internal to the atom; no params (D3).

## Deferred

- Half-res near/far gathers (D4 trigger).
- Empty-near-pipeline skip (D4 trigger).
- Kawase/dual-filter color mip for smear (D4 trigger).
