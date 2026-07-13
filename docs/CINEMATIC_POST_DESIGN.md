# Cinematic Post — DoF, SSAO, motion blur as graph atoms

**Status:** IN PROGRESS — P0–P4 SHIPPED · Sonnet 5 · **P0 (D7/I6, both layers, `docs/landings/2026-07-12-cinematic-post-batch-a.md`) — derived uniforms are first-class on the texture codegen path AND in fused regions. P1+P2 (`docs/landings/2026-07-12-cinematic-post-batch-b.md`) — `node.coc_from_depth` + DoF slice, `node.ssao_from_depth` + SSAO arm. P3 (`docs/landings/2026-07-12-cinematic-post-batch-c.md`) — `node.motion_blur` tail. AMENDED 2026-07-13 (Fable, with Peter): quality verdict on the shipped stack (*"look terrible and need a lot of work... very blocky and has a hard cut off on the blur"*) → P4 escalated from optional to the DoF root fix (see the `dof-polish-wave-prompt` lane; bugs BUG-136/137/138 own the diagnosis), P5 GTAO decisions committed (D9), P6 AO denoise added (D8), PNG look-pass rule added to open phases (§4). **P4 SHIPPED 2026-07-13 (Sonnet 5, `dof-polish` worktree/branch `feat/dof-polish`)** — BUG-137's `node.coc_dilate` (standalone neighborhood-max atom) landed first, then `node.bokeh_gather` (D5's 32-tap occlusion-aware disc gather) replaced the two `variable_blur` H/V nodes, still consuming `coc_dilate`'s dilated CoC. `CinematicScene` now runs the full DoF(dilated+bokeh)+SSAO+motion-blur chain. Orchestrator before/after PNG look-pass (see BUG-137) showed the silhouette-bleed halo visibly gone; Peter's own confirmation on a richer depth-discontinuity scene is still owed. BUG-138 (blockiness) FIXED 2026-07-13 in `node.variable_blur` itself (scales sub-tap density with CoC radius above an 8px `step_size` threshold, byte-identical below it) — the atom is no longer in `CinematicScene`'s chain but remains user-wireable elsewhere. BUG-136 (motion blur no visible effect) — the dof-polish lane ran both committed runtime probes (shutter_angle at uniform-pack, a velocity texel during a headless orbit) against the shipped `CinematicScene` graph: both check out clean every frame, and a `shutter=0` vs `shutter=181.05` headless render diff shows a real shader-level visual delta. This exonerates the graph wiring, shader math, matrix bookkeeping, derived-uniform packing, and velocity buffer end to end — the bug does not reproduce headlessly. **ESCALATED, not fixed:** the remaining suspects (UI slider-drag propagation cadence into the content-thread graph; whether the render loop ticks continuously outside active playback) live entirely in the live app's interactive layer, which this lane's headless workers cannot observe — needs either a live repro session with Peter or a design decision on which layer to instrument. P5/P6 open.**
**Prerequisites:** P0 (this doc, D7) before P1–P4; CAMERA_AND_LENS P1+P2 and GBUFFER P1 before this P1/P2; GBUFFER P2 before this P3.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.
**Companions:** [CAMERA_AND_LENS_DESIGN.md](CAMERA_AND_LENS_DESIGN.md) (`LensParams` on the Camera wire; the CPU oracle) · [GBUFFER_DESIGN.md](GBUFFER_DESIGN.md) (the depth/velocity inputs; the shared linearize helper) · [RENDERING_INFRA_V2_DESIGN.md](RENDERING_INFRA_V2_DESIGN.md) (direction: pillar 2, "our 2D graph system already excels here; the missing inputs are per-pixel depth and motion vectors") · [docs/ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) (authoring contract; the codegen-path rule every atom here satisfies)

Peter's picks, 2026-07-12: *"physical camera with DoF first slice, motion
blur, and SSAO"*, for scenes that are *"pure black backgrounds with the
models I have as the main focus"* — and the instrument frame: *"consider the
'cinematic' effects, true depth of field stuff, etc so I can render visuals
that look high grade."* On stage and in export this buys: **rack focus on a
fader** (focus_distance is a port-shadowed scalar — bind it to a macro knob
or LFO), **depth isolation on a knob** (f_stop), **drop-smear on a button**
(shutter_angle spiked by a beat envelope), and **contact weight** on
self-occluding hero meshes (SSAO) that makes a statue read as mass instead
of a texture.

The governing find of the audit: MANIFOLD nearly has DoF already.
`node.variable_blur` (separable gaussian, per-pixel kernel width sampled
from a texture's R channel) shipped for other reasons — true DoF is **one
new pointwise atom** (`coc_from_depth`) wired into it. The rest of the
cluster follows the same shape: small single-dispatch atoms consuming the
G-buffer, composed in preset JSON, every one on the codegen path.

Execution note (Peter, 2026-07-12, governs all phases): every gate is
numeric, and no phase produces a PNG artifact (*"No need to produce the
PNGs if they're not going to look at them"* — overrides the standard's
L2-demo minimum for this cluster). The house pattern here is the **CPU-reference parity gate**: each
atom's algorithm is implemented twice from the same committed spec — once
as the WGSL body, once as a plain-Rust reference in the test module — and
the gate uploads a *synthetic* input texture, runs both, and asserts
pixel-for-pixel agreement within tolerance. No scene render, no PNG
judgment, no flake surface. (Precedent: the hand-vs-generated kernel parity
suites, e.g. `project_3d.rs::gpu_tests`; this extends the same idea from
"two kernels agree" to "kernel agrees with committed math".)

## 1. Audit — what exists (verified 2026-07-12, tip `9e537b16`)

| Piece | Where | State |
|---|---|---|
| `node.variable_blur` — separable gaussian, per-pixel width from `width` Texture2D R channel, H/V param; codegen-path (`MultiInputCoincident`, `input_access: [Gather, Gather]`) | `primitives/gaussian_blur_variable_width.rs:45-97` | THE DoF gather. ⚠ VERIFY-AT-IMPL (P1): read its body shader for the R-channel unit (sigma-in-px vs half-width-px) — `coc_from_depth` emits exactly that unit; transcribe, don't guess |
| `node.gaussian_blur` (fixed-width separable, `input_access: [Gather]`) | `primitives/separable_gaussian.rs:126-128` | Precedent: Gather atoms are fusable; copy its shader-body shape |
| Depth input, raw [0,1] R32Float; `shared/depth.wgsl::linearize_depth`; near/far via Camera wire | GBUFFER D2/D4 (⚠ VERIFY-AT-IMPL: exists once GBUFFER P1 lands — check `rg linearize_depth crates/manifold-renderer/src/node_graph/shaders/shared/`) | The depth contract |
| Velocity input, NDC-delta Rg16Float, rigid+camera motion | GBUFFER D5 (P2) | The motion-blur input |
| `LensParams` (focus_distance, f_stop, shutter_angle, exposure_ev) on the Camera wire | CAMERA_AND_LENS D4 | The lens facts every atom reads |
| `Camera::project_to_pixel` CPU oracle (`view_z`, `depth`) | CAMERA_AND_LENS D2 | Reference for every depth↔world computation in the gates |
| Tone map atoms (`node.tone_map`, reinhard) | `primitives/tone_map.rs`, `reinhard_tone_map.rs` | Untouched — exposure lives in render_scene (CAMERA D5); no new tonemap work here |
| Effect/generator preset JSON + checker + card surface | `assets/` presets, `check_presets` (remember: checker ≠ runtime — load smoke required) | The composition deliverable's plumbing |
| DoF/SSAO/motion-blur/bokeh atoms | nowhere | Genuinely new: 4 atoms, all single-dispatch, all codegen-path |

§2.5 audit statement (mandatory): `coc_from_depth` = genuinely new
(pointwise math, no existing atom computes CoC); DoF gather = **exists**
(`variable_blur` — one wire away); `ssao_from_depth`, `motion_blur`,
`bokeh_gather` = genuinely new (no neighborhood atom samples depth-derived
AO, velocity-directed blur, or CoC-weighted discs); composition = preset
JSON, zero new infra. No fused monoliths: each atom is one dispatch; the
*effect* "cinematic DoF" is a graph, per the no-monolith rule.

Binding constraints: hot path — each atom is one full-res dispatch; the
lazy rule upstream (GBUFFER D1) plus "unwired = not in the graph" means
cost is opt-in per scene; performance surface — focus/f_stop/shutter are
port-shadowed and preset-carded (that IS the feature); persistence —
presets serialize as ordinary graph JSON; time model / threads — untouched.

## 2. Decisions

**D1 — DoF v1 = `coc_from_depth` → `variable_blur` ×2, shipped as a
preset subgraph, not a fused kernel.** The committed CoC math (thin lens;
world units are meters for lens physics, `WORLD_TO_MM = 1000.0`;
`SENSOR_H_MM = 24.0` fixed, CAMERA D4):

```text
f_mm    = SENSOR_H_MM / (2 · tan(fov_y / 2))          // from the Camera wire's fov
A_mm    = f_mm / f_stop                                // aperture diameter
D_mm    = linearize_depth(raw_depth, near, far) · WORLD_TO_MM
S_mm    = focus_distance · WORLD_TO_MM
coc_mm  = A_mm · f_mm · |D_mm − S_mm| / (D_mm · max(S_mm − f_mm, 1.0))
coc_px  = clamp(coc_mm / SENSOR_H_MM · viewport_h, 0.0, max_radius)
out.r   = coc_px  (in variable_blur's width unit — the VERIFY-AT-IMPL above)
```

`f_stop = INFINITY` (pinhole default) → `A = 0` → CoC 0 everywhere →
variable_blur is an exact pass-through: **an unlensed camera produces a
bit-clean image through the DoF subgraph** (invariant I2). Inputs:
`depth: Texture2D required`, `camera: Camera required`; params
`focus_distance`/`f_stop` port-shadowed, overriding the wire's lens when
wired (port-shadows-lens, same precedence rule as port-shadows-param);
`max_radius` param (default 24 px). `fusion_kind: Pointwise`,
`input_access: [CoincidentTexel]` — codegen-path by construction.
Honest costs, stated: gaussian CoC is not photographic bokeh — no aperture
shapes, and out-of-focus background bleeds across in-focus silhouettes
(no occlusion-aware gather). That is exactly the P4 upgrade
(`bokeh_gather`), designed now, built when the v1 look demands it.
Rejected: a monolithic `node.depth_of_field` kernel (bundles CoC + gather —
the no-monolith rule names this exact shape); premultiplied CoC packing
into the color alpha (collides with the alpha contract).

**D2 — Deterministic sampling everywhere; no temporal accumulation in
v1.** SSAO and bokeh use fixed sample tables generated by a committed
formula (golden-angle spiral: `r_i = sqrt((i+0.5)/N)`, `θ_i = i · 2.399963`,
N fixed per atom below) and a committed per-pixel rotation hash
(`hash = fract(sin(dot(px, vec2(12.9898, 78.233))) · 43758.5453) · 2π` —
the classic; committed so CPU and GPU agree exactly). No frame-index
jitter, no TAA dependence: output is a pure function of inputs, which is
what makes CPU-reference parity possible and keeps export deterministic
(two exports bit-identical — the journey-proofs property). Consequence,
stated honestly: banding/noise patterns are static rather than
temporally-smoothed; N is sized accordingly (16 SSAO taps, 32 bokeh taps).
Rejected: blue-noise texture sampling (an asset dependency and a lookup
where a formula does; revisit only if static patterns visibly band on
Peter's scenes).

**D3 — SSAO v1 reconstructs normals from depth; committed algorithm.**
`node.ssao_from_depth`: inputs `depth` + `camera`; params `radius` (world,
default 0.5), `intensity` (default 1.0), `bias` (default 0.025), N = 16.
Algorithm (committed; the WGSL and the CPU reference implement THIS, no
variation): reconstruct view-space position per texel via
`linearize_depth` + inverse-projection xy; normal = `normalize(cross(dFdx-
style neighbor deltas))` using explicit ±1-texel reads (Gather access, not
derivative intrinsics — compute has no fragment derivatives and neighbor
reads are what the CPU reference can replicate exactly); hemisphere-sample
N spiral points scaled by `radius`, rotated per-pixel by the D2 hash;
occlusion += range-checked depth comparison with `bias`; `out.r = 1 −
intensity · occlusion / N`. Output is an AO map the graph author wires into
their comp (multiply node) — the atom does NOT modify the color image.
Rejected: normal-map-quality SSAO via a true normal G-buffer attachment
(GBUFFER D6 reserves the ABI; trigger lives there); SSAO inside
render_scene's shader (couples scene pass to post; kills composability —
AO-on-wireframes, AO-graded-before-DoF etc. all want it as a wire).

**D4 — Motion blur = velocity-directed gather, shutter from the lens.**
`node.motion_blur`: inputs `in` (color), `velocity` (GBUFFER Rg16Float NDC
deltas), `camera` (reads `lens.shutter_angle`); params `shutter_angle`
port-shadowed override, `samples` fixed 8 (const), `max_blur_px` (default
32, safety clamp). Per pixel: `smear_px = velocity_ndc · 0.5 · viewport ·
(shutter_angle / 360)`, clamped; accumulate 8 taps along ±smear/2; equal
weights. `shutter_angle = 0` (pinhole default) → exact pass-through
(invariant I2 twin). `input_access: [Gather, CoincidentTexel]`. Honest
cost: gather blur ghosts slightly at leading edges vs scatter methods —
industry-standard artifact, accepted v1; and it inherits GBUFFER D5's
rigid-only honesty (deforming surfaces blur by their rigid motion only).

**D5 — `node.bokeh_gather` is the designed P4 upgrade, replacing the two
variable_blur nodes inside the preset only.** Single-pass disc gather:
32 spiral taps (D2) scaled by the *center* pixel's CoC, each tap weighted
by `step(distance_to_center_px, coc_at_tap)` (a sample only contributes if
its own CoC reaches the center — the standard scatter-as-gather occlusion
approximation) with luminance-preserving normalization. Circular aperture
v1 (no blade count). Same inputs as variable_blur (`in`, `width`) so the
preset swap is literally re-wiring two nodes to one. Rejected: multi-pass
scatter DoF (draw-per-splat; a render-pass architecture, not an atom) and
hexagonal separable bokeh (three-pass elegance, real complexity, zero ask —
"someday" justification, deleted per the no-someday-parts rule).

**D6 — The performable surface ships as ONE bundled generator preset,
`CinematicScene`.** Camera atom → `camera_lens` → `render_scene`
(depth + velocity wired) → `ssao_from_depth` (+ multiply) →
`coc_from_depth` → `variable_blur` H/V → `motion_blur` → out. Cards:
`focus_distance`, `f_stop`, `shutter_angle`, `exposure_ev`,
`ssao_intensity`, `ssao_radius` — the six-knob lens, bindable like any
card param (param_values is the performance surface). Grown incrementally:
P1 ships it with the DoF chain, P2 adds the SSAO arm, P3 the motion-blur
tail — each phase's preset edit passes the checker AND a load-smoke test
(checker ≠ runtime). Rejected: N separate demo presets (nobody performs a
demo; one instrument, grown).

**D7 — Derived uniforms become first-class in the freeze compiler, on
both paths; no camera-atom boundary carve-out (P0; amendment 2026-07-12).**
Discovered at P1 implementation start: D1 commits `coc_from_depth` to read
`fov_y`/`near`/`far`/viewport CPU-side from the Camera wire into its
kernel's uniform every frame — and the mechanism for exactly that,
`DERIVED_UNIFORMS` (`n: [...]` in `primitive!`), exists **only for
Array-output buffer atoms**. Verified anchors (tip `71a3503e`):
`standalone_for_spec` routes derived uniforms solely through
`generate_standalone_buffer` (`freeze/codegen.rs:203-217`; the texture
path `generate_standalone_ext` has no derived parameter); in fused
regions, derived values are sourced by a NAME WHITELIST
(`dt_scaled`/`frame_count`/`time*` wired from `system.generator_input`,
`freeze/install.rs:1322-1334`) where an unknown name bails the whole
region and a vec3 bails unconditionally; and a Camera wire into any
texture atom classifies it a Boundary outright (`freeze/region.rs:
1047-1053`). Without P0, every atom in this doc (D1/D3/D4 all take
`camera`) is unimplementable on the codegen path — violating I4 and the
repo-wide all-nodes-fusable rule.

The committed fix, two layers, both mandatory (Peter, 2026-07-12: *"I
don't want any stopgaps"*):

- **Standalone:** the texture codegen path accepts `DERIVED_UNIFORMS`
  exactly as the buffer path does — fields appended to the merged params
  uniform (same `param_word_count` layout rules, vec3 included), values
  recomputed per frame by the atom's `run()` from its CPU-struct inputs
  (Camera) + frame context. CPU-struct input ports (`Camera`, later
  `Light`/`Material`) are legal on texture atoms and emit no GPU binding —
  consumed CPU-side only. Generated-vs-hand parity proof required (the
  existing I4 meta-test covers any `wgsl_body` atom automatically).
- **Fusion:** the install-time name whitelist and the vec3 bail are
  **deleted**, replaced by per-member recompute: the fused
  `node.wgsl_compute` node recomputes each member's derived-uniform
  values at uniform-pack time via the member's registered recompute
  (registry lookup by the member's type-id string — data-driven, no
  closures serialized into the def), fed by (a) frame context and (b) the
  region's CPU-struct externals, which install routes to the fused node.
  The time-family (`dt_scaled` etc.) migrates onto the same mechanism —
  one sourcing path, zero per-name install code, so any future derived
  uniform works without touching the compiler again. The classify cut at
  `region.rs:1047` gains the matching exemption: a CPU-struct wire into
  an atom that consumes it entirely via derived uniforms no longer cuts;
  a wire into any other non-texture non-param port still does. How the
  Camera external physically reaches the fused node (a declared
  non-introspected input port vs. a freeze-marker side channel) is
  implementation latitude — the fixed contract is I6.

Honest scope statement, so nobody oversells the win: in the D6 chain
itself, `variable_blur`/`motion_blur`/`bokeh_gather` GATHER their
upstream inputs, and a gather can never fuse with its producer (the
intermediate must be materialized) — those cuts are physics of fusion,
not this gap. What P0 buys is (1) the four atoms existing at all on the
codegen path, and (2) pointwise camera consumers (`coc_from_depth`, and
future depth-fog/exposure-style atoms) fusing with adjacent pointwise
work instead of punching a hole per camera read. Rejected alternatives:
boundary carve-out for camera atoms (a stopgap by name — Peter rejected
it directly); scalar output ports (fov/near/far) on every camera
producer + control wires (authoring-surface creep on N camera nodes, and
it leaves the whitelist alive); camera scalars as fake user params packed
by the preset (breaks the wire-carries-the-camera model and desyncs the
lens from render_scene).

**D8 — AO ships denoised: `node.bilateral_blur`, a depth-guided separable
blur pair between the AO atom and its mix (amendment 2026-07-13).** The
observed defect that forces this: the shipped chain wires `ssao → ssao_mix`
raw (`CinematicScene.json` wire `12.out → 13.b`, verified 2026-07-13) — 16
hash-rotated samples per pixel with NO smoothing pass is per-pixel noise by
construction, and every production AO implementation (SSAO or GTAO) follows
the sampler with an edge-aware blur. We shipped the sampler without the blur.
§2.5 audit (2026-07-13, 214 primitives surveyed): no edge-aware/bilateral
blur exists — nearest relatives are `separable_gaussian.rs` (the axis-pair
pattern + fixed 9-tap kernel to copy) and `gaussian_blur_variable_width.rs`
(the two-texture-input Gather ABI to copy) — genuinely new, and
general-purpose by design (any texture + depth guide, not AO-only).

Committed atom: inputs `in: Texture2D` (Gather), `depth: Texture2D` raw
[0,1] (GatherTexel), `camera: Camera` (derived uniforms `near`/`far` for
`linearize_depth` — the D7 mechanism, precedent `coc_from_depth`); params
`axis` (Enum H/V), `depth_sigma` (world units, default 0.1). Fixed 9 taps at
1-px spacing; `weight_j = K9_j · exp(−(Δz_j / depth_sigma)²)` where `K9_j`
are the σ≈2 gaussian constants (same values as `VBW_K9_*`) and `Δz_j` is the
linearized-depth difference from center; renormalize by the weight sum;
alpha = center pass-through. `fusion_kind: MultiInputCoincident`,
`input_access: [Gather, GatherTexel]` — codegen-path by construction. Preset:
insert `bilat_h`(15) + `bilat_v`(16) into the `12.out → 13.b` wire, `8.depth`
→ both `depth`, `5.out` → both `camera`. No new cards (denoise is quality
plumbing, not a performer knob). Rejected: joint-bilateral inside the AO atom
(a second dispatch hiding in one kernel — the no-monolith rule); a non-
separable 5×5 single pass (2× the taps of the pair for marginal quality on a
smooth AO field).

**D9 — GTAO replaces SSAO: `node.ssao_gtao`, decisions committed (amendment
2026-07-13; closes P5's two open lines).** Peter 2026-07-13: worth it if it
beats SSAO's look *"without hitting the performance harder than the current
SSAO"* — the committed budget below is deliberately the same sample class as
D3's (16 occlusion taps + 4 normal taps).

(a) **Deterministic single-frame sampling, committed:** 2 slices per pixel,
slice angles `φ_i = ssao_hash_angle(px) · 0.5 + i · (π/2)`, `i ∈ {0,1}` (the
D2 hash, halved into [0,π) so two slices spread the semicircle); per slice,
per side (±tangent), 4 steps at screen-space radii `r_j = radius_px · (j+1)/4`
(`radius_px` = the world `radius` projected at center depth — transcribe
`ssao_from_depth`'s existing projection, don't re-derive). Total: 2·2·4 = 16
depth taps + the same ±1-texel normal reconstruction as D3. No temporal
accumulation (D2 stands; this is the "sized for ONE deterministic frame"
answer P5 demanded). Committed integral, per slice: view-space center `P`,
view vector `V = normalize(−P)`; per side, horizon cosine `hcos` = max over
in-range samples (range check: reject `|S_j − P| > radius`, the D3 halo
guard) of `dot(normalize(S_j − P), V)`; signed horizon angles `h1 =
−acos(hcos₋)`, `h2 = +acos(hcos₊)`; normal projected into the slice plane →
length `‖N_p‖` and signed angle `n` from `V` (sign by the slice tangent);
clamp `h1 = n + max(h1 − n, −π/2)`, `h2 = n + min(h2 − n, π/2)`; per-side
arc `a(h) = 0.25·(−cos(2h − n) + cos n + 2h·sin n)`; slice visibility =
`‖N_p‖ · (a(h1) + a(h2))`. Pixel visibility = mean of the 2 slices;
`out.r = clamp(1 − intensity·(1 − visibility), 0, 1)`, broadcast RGB, alpha 1
— same output contract as D3, so the mix wiring and cards are untouched.

(b) **REPLACES `node.ssao_from_depth` outright** — the primitive file is
deleted, not paralleled. Params carry: `radius` → `radius`, `intensity` →
`intensity`; `bias` dies (the range check subsumes it; recorded here so
nobody re-adds it). Load-migration maps the type id `node.ssao_from_depth` →
`node.ssao_gtao` (precedent: `WireframeDepthGraph` → `WireframeDepth` at
v1.7.0; ⚠ VERIFY-AT-IMPL: locate the migration table via
`rg 'WireframeDepthGraph' crates/` and add the mapping in the same shape).
`CinematicScene` node 12 swaps type in place (handle/id/wires unchanged).
Rejected: shipping both AO atoms behind a picker (double parity surface,
double maintenance, and a picker choice no performer can evaluate — the
curated-vocabulary rule); HBAO as an intermediate step (GTAO subsumes it,
already stated in P5's original stub). Honest costs: GTAO's thin-object
over-darkening (no thickness heuristic in v1 — deterministic budget) and
slightly more ALU per tap (acos + arc math; Apple-silicon-trivial, and the
tap count is unchanged, which is where the real cost lives).

**Consequences of the whole design, stated honestly:** four full-res
dispatches when the whole chain is wired (CoC + 2×blur + SSAO + blur ≈ 5)
— on top of GBUFFER's bandwidth line; the perf gate measures, the lazy/
opt-in rule contains until then. Every artifact named above (silhouette
bleed until P4, static noise patterns, gather ghosting) is a v1 look
boundary Peter accepts by approving this doc — none may be "fixed" by an
executor improvising a different algorithm (that's the plausible-wrong
move here: swapping in a fancier technique from training memory mid-phase;
the committed math is the contract, upgrades are new decisions).

## 3. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| I1 — Every atom matches its CPU reference on synthetic inputs, pixel-exact within 1e-4 | per-atom `gpu_tests` CPU-reference parity (P1: coc; P2: ssao; P3: motion_blur; P4: bokeh) — synthetic depth/velocity ramps uploaded, both implementations run, full-buffer compare |
| I2 — Pinhole/zero-shutter lens is a bit-clean pass-through through the whole chain | gpu_test: uniform-lens chain on a noise texture, in == out byte-compare (f_stop=∞ → CoC buffer all-zero asserted; shutter=0 → motion_blur identity asserted) |
| I3 — CoC math agrees with hand-computed values | unit test (CPU, no GPU): 5 (depth, focus, f_stop) triples → coc_px vs values computed by hand in the test source with the D1 formula |
| I4 — All four atoms are codegen-path | existing meta-test that every `primitive!` with `wgsl_body` proves generated-vs-hand parity; plus negative gate `rg 'create_compute_pipeline\(include_str' <the four new files>` = 0 hits |
| I5 — Presets stay loadable | `check_presets` green + load-smoke gpu_test instantiating `CinematicScene` and executing one frame (magenta-free readback: no structured errors) |
| I6 — Derived-uniform atoms are fused == unfused, byte-identical under the precision contract (FREEZE_COMPILER_MAP §7) | freeze proof gpu_test: a graph chaining a camera-derived pointwise atom with a pointwise neighbour, fused vs unfused full-buffer byte-compare; PLUS the entire existing freeze proof suite stays green (the time-family migration in D7 touches every fused buffer sim) |
| I7 — `bilateral_blur` on a uniform-depth plane equals the plain 9-tap gaussian; across a depth step it does not bleed | gpu_tests (P6): `bilateral_uniform_depth_matches_gaussian` (flat synthetic depth, byte-compare vs K9 reference) + `bilateral_depth_edge_no_bleed` (step-edge depth, cross-edge contribution < 1% asserted numerically) + I1-pattern CPU-reference parity |
| I8 — GTAO analytic sanity + migration round trip | gpu_tests (P5): `gtao_flat_plane_full_visibility` (unoccluded plane → out.r ≈ 1 within 1e-3) + `gtao_matches_cpu_reference` (I1 pattern, synthetic depth ramp) · unit test `ssao_from_depth_migrates_to_gtao` (a saved graph JSON with the old type id loads, node resolves to `node.ssao_gtao`, radius/intensity carried) · negative gate: `rg 'ssao_from_depth' crates/ assets/` → only migration-table + doc hits |

## 4. Phasing

Common to all phases — **Read-back:** this doc §2 (the phase's D-decision
whole), `docs/ADDING_PRIMITIVES.md`, `docs/DECOMPOSING_GENERATORS.md` §2.5,
the precedent atom named in the phase. **Forbidden moves, all phases:**
algorithm substitution (D2/D-math are the contract) · fused monolith
kernels · any gate that requires looking at an image · touching tone-map
atoms · frame-index/time inputs into any of these atoms (determinism is
load-bearing). **Test scope:** focused `-p manifold-renderer` + the new
gpu_tests; workspace sweep at landing.

**Demo rule for the open phases (P4/P5/P6), amended 2026-07-13:** the
original cluster rule was numeric-only by Peter's directive (*"No need to
produce the PNGs if they're not going to look at them"*); his verdict on the
P0–P3 result — *"look terrible and need a lot of work"* — ended that
premise: he IS going to look now. Gates stay numeric; additionally each open
phase ends with ONE headless `render-generator-preset` PNG of
`CinematicScene` (before/after pair for a swap phase) in the landing report,
and Peter's look-pass is the real exit. P0–P3 shipped under the old rule;
this governs everything still open.

- **P0 — derived uniforms first-class in the freeze compiler — SHIPPED
  2026-07-12** (D7; two sessions, standalone layer `42929678` then fusion
  layer `38d2f0f8`, both landed together in batch A `docs/landings/
  2026-07-12-cinematic-post-batch-a.md`). Deliverables landed:
  texture-path `DERIVED_UNIFORMS` in `generate_standalone_ext` +
  CPU-struct inputs binding nothing; `install.rs`'s name whitelist +
  vec3 bail deleted, replaced by `freeze/derived_uniform_registry.rs`
  (inventory-based, type_id-keyed per-member recompute); `region.rs`
  classify exemption (both texture and buffer paths — one shared
  predicate, wider than D7's literal texture-only line, judged as
  completing the one conceptual fix consistently); time-family migrated
  onto the same path. Gate: I6 (`camera_derived_pointwise_atom_fuses_
  and_matches_unfused`) + the full existing freeze proof suite (125
  passed) + the full `manifold-renderer --features gpu-proofs` sweep
  (1412 passed, 0 failed) + focused suite (1114 passed) + clippy — all
  independently re-run by the orchestrating session, not self-reported.
  `docs/FREEZE_COMPILER_MAP.md` §4/§5/§9 (+ a §7 cross-reference) updated
  to the new sourcing model in the same landing. Demo: none — compiler
  phase, the proofs are the demo.
- **P1 — `coc_from_depth` + DoF slice of `CinematicScene`** — SHIPPED
  2026-07-12 (`docs/landings/2026-07-12-cinematic-post-batch-b.md`; `focus_distance`/`f_stop`
  read entirely via derived_uniforms from the Camera's lens block, no port-shadowed
  overrides on the atom itself — cards bind to `camera_lens` directly). (one session).
  Entry: P0 landed (both layers) + CAMERA P2 + GBUFFER P1 landed (verify:
  `rg 'LensParams'` hits camera.rs; `rg 'linearize_depth'` hits shared
  header; `rg 'DERIVED_UNIFORMS' crates/manifold-renderer/src/node_graph/freeze/codegen.rs`
  shows the texture route). Deliverables: the
  atom (full descriptor/picker/aliases; `Pointwise`, `[CoincidentTexel]`),
  variable_blur width-unit note resolved (VERIFY-AT-IMPL) and recorded in
  the atom's composition_notes, I2+I3 tests, `CinematicScene` preset with
  DoF chain + `focus_distance`/`f_stop`/`exposure_ev` cards, I5 smoke.
  Gate: I1(coc)+I2+I3+I5 + focused suite + clippy. Demo: none — L1
  (cluster no-PNG rule). Performer gesture: `focus_distance` bound to a slow LFO — the
  rack-focus breathe; the gate exercises the binding path by driving the
  card param and asserting the CoC buffer changes accordingly.
- **P2 — `ssao_from_depth` + SSAO arm** — SHIPPED 2026-07-12
  (`docs/landings/2026-07-12-cinematic-post-batch-b.md`; `radius`/`intensity`/`bias`
  are ordinary atom params, not port-shadowed — D3 doesn't call for it and the
  preset cards bind directly). (one session). Entry: GBUFFER P1.
  Deliverables: atom per D3 (Gather), CPU reference, synthetic-ramp parity
  (I1), analytic sanity unit test (flat plane → occlusion 0 everywhere
  except bias tolerance), preset arm + `ssao_intensity`/`ssao_radius`
  cards, I5. Demo: none — L1 (cluster no-PNG rule).
  Performer gesture: `ssao_intensity` on a fader — contact weight swells.
- **P3 — `node.motion_blur`** — SHIPPED 2026-07-12
  (`docs/landings/2026-07-12-cinematic-post-batch-c.md`; `shutter_angle` read
  entirely via derived_uniforms from the Camera's lens block, no port-shadowed
  override on the atom itself — the card binds to `camera_lens` directly,
  matching P1/P2's precedent; `node.camera_lens` already had a working
  port-shadowed `shutter_angle` param reserved for this). (one session). Entry: GBUFFER P2 (velocity
  exists) + CAMERA P2 (shutter on the wire). Deliverables: atom per D4,
  CPU reference on a synthetic velocity ramp (I1), I2 zero-shutter
  identity, preset tail + `shutter_angle` card, I5. Demo: none — L1
  (cluster no-PNG rule). Performer gesture:
  `shutter_angle` spiked by a beat envelope — drop smear.
- **P4 — `node.bokeh_gather` swap** (one session; triggered when Peter
  wants bokeh over gaussian — this phase is pre-approved, not gated on a
  new decision). Deliverables: atom per D5, CPU reference (I1), preset
  re-wire (two variable_blur nodes → one bokeh_gather), I2 re-proof, I5.
  Demo: none — L1 (cluster no-PNG rule).
- **P6 — `node.bilateral_blur` AO denoise (one session; amendment
  2026-07-13; INDEPENDENT of P4/P5 — land it FIRST: it improves the
  already-shipped SSAO immediately, and P5's GTAO plugs into the same
  denoise unchanged).** Entry: `rg 'bilateral' crates/manifold-renderer/src/
  node_graph/primitives/` → 0 hits; D8 anchors re-verified. Read-back adds:
  D8 whole, `separable_gaussian.rs` (axis-pair + K9 precedent),
  `gaussian_blur_variable_width.rs` (two-input Gather ABI precedent).
  Deliverables: the atom per D8 (descriptor/picker/aliases; codegen-path
  `wgsl_body`), CPU reference, I7's three named tests, `CinematicScene`
  insert (nodes 15/16 per D8), I5 re-smoke. Gate: I7 + I5 + I4's negative
  `rg` on the new file + focused suite + scoped clippy. Acceptance demo
  (L2, amended rule): before/after PNG pair of `CinematicScene` — the AO
  noise visibly gone, Peter look-pass. Performer gesture: none (quality
  plumbing; the existing `ssao_intensity` fader is unchanged). Forbidden
  moves, this phase: making `depth_sigma` a card · widening to a general
  "denoiser framework" · touching the AO atom itself (that's P5).
- **P5 — `node.ssao_gtao` replaces `node.ssao_from_depth` (one session;
  decisions COMMITTED 2026-07-13 in D9 — this phase is now buildable;
  prefer landing after P6 so the look-pass judges GTAO denoised).**
  Entry: P6 landed (preferred; if scheduling forces P5-first, the demo
  comparison is raw-vs-raw and says so); D9 anchors re-verified; the
  migration-table VERIFY-AT-IMPL resolved and recorded in phase notes
  before code. Deliverables: the atom per D9(a) (Gather, derived uniforms
  fov_y/near/far — same as D3), CPU reference, `ssao_from_depth` deleted +
  load-migration per D9(b), `CinematicScene` node-12 type swap, I8's four
  named checks, I5 re-smoke. Gate: I8 + I5 + I4 negative gate on the new
  file + focused suite + scoped clippy. Acceptance demo (L2, amended
  rule): before/after PNG pair (SSAO+denoise vs GTAO+denoise), Peter
  look-pass. Performer gesture: `ssao_intensity` fader still swells
  contact weight — gate re-drives the card against the new atom.
  Forbidden moves, this phase: keeping both AO atoms alive (D9(b) is a
  seam brief: old symbol deleted, negative gate proves it) · adding a
  thickness heuristic or temporal reuse "for quality" (D2/D9 are the
  contract) · re-adding `bias`.

Phasing-completeness check: D7→P0, D1→P1, D3→P2, D4→P3, D5→P4, D6 grown
across P1–P4; D2 is inside P2/P4 deliverables; exposure card (CAMERA D5)
rides P1's preset. 2026-07-13 amendment: D8→P6, D9→P5 — every committed
line in D8/D9 lands in exactly one of those two briefs. No body-committed
affordance is unphased.

## 5. Decided — do not reopen

1. DoF v1 is CoC + variable_blur composition; bokeh is the P4 swap, inside
   the preset only (D1/D5).
2. Deterministic sampling, no temporal accumulation, no noise textures (D2).
3. SSAO from reconstructed normals; AO is a wire, not a scene-pass term (D3).
4. Shutter/focus/f-stop read from the Camera wire, port-shadow overridable.
5. Lens-neutral chain is bit-clean pass-through — enforced, not aspirational (I2).
6. One grown `CinematicScene` preset, not demo-per-feature (D6).
7. Derived uniforms are first-class on both codegen paths; the sourcing
   whitelist dies; NO boundary carve-out for camera atoms — Peter,
   2026-07-12: "I don't want any stopgaps" (D7/P0).
8. AO is always denoised in the preset: `bilateral_blur` H/V between the AO
   atom and its mix; denoise params are not cards (D8, 2026-07-13).
9. GTAO replaces SSAO — one AO atom, deleted-not-paralleled, load-migrated;
   16-tap deterministic single-frame budget, no thickness heuristic, no
   temporal (D9, 2026-07-13).
10. Open phases gate numeric AND ship a looked-at PNG — the 2026-07-12
    no-PNG directive is ended by Peter's 2026-07-13 verdict (§4 demo rule).

## 6. Deferred

- **Aperture blade shapes / anamorphic bokeh** — trigger: Peter asks after
  seeing P4's circular discs.
- **Temporal accumulation (TAA-style noise smoothing)** — trigger: static
  SSAO/bokeh patterns visibly band on a real scene AND MetalFX work starts
  (RENDERING_INFRA_V2 §9 owns that lane).
- **Depth-graded fog/color (depth-driven LUT)** — one `math` graph away
  once depth is a wire; needs no design — listed so nobody writes one.
- **Auto-focus (focus follows a target object)** — trigger: Peter asks;
  shape: a CPU atom reading a transform wire → focus_distance scalar out.
