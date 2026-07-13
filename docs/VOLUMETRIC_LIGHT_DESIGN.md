# Volumetric Light — god rays and haze for render_scene

**Status:** APPROVED design, not built · 2026-07-13 · Fable 5
**Prerequisites:** none (REALTIME_3D P2 shadows + P3 fog and CAMERA_AND_LENS P2 are shipped and are the substrate; verified in §1)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter's directives, 2026-07-13, all quoted because each one decides something:
*"God rays would be amazing!"* · the look: *"a black void filled with haze with
beams of light shining through"*, with *"the god rays form off the stems or
leaves of the photoscanned plant meshes"* · *"keep the haze simple for now,
global fader"* · *"god rays shouldn't react to audio, lights will do that for
us"* · *"All lights should support volumetrics."* Context: this is EP-release
content work ([interim-ep]) — the deliverable is the look, judged by Peter's
eye, not only by parity numbers (his 2026-07-12 verdict on the numerically-green
cinematic stack: *"look terrible and need a lot of work"* — this doc's demo
rules are written from that lesson).

**The governing insight:** the beams are not a new lighting system — they are
the *existing* lights, shadow maps, and Atmosphere haze, integrated along the
view ray instead of only at surfaces. Everything the march needs already lives
inside `render_scene`: per-light shadow maps + comparison sampler, the
ring-buffered light table, camera fov/near/far, resolved scene depth, and an
`Atmosphere` wire with density + height falloff. Zero new graph atoms; zero
freeze-compiler work (`render_scene` is already a draw-call boundary node —
the render_* codegen exemption class). The whole feature is internal passes +
three new `Atmosphere` fields.

On stage / in export: haze density and shaft intensity are port-shadowed
params on `node.atmosphere` → cards → faders. The beams inherit every light
modulation for free — a beat-enveloped light color pulses its own shafts,
which is exactly the "lights will do that for us" contract. A black-fog void
(fog_color = black) gives pure darkness + carved light beams; a colored fog
gives classic atmosphere. BUG-118's "milk filter" complaint is this design's
intake evidence: constant-color exponential fog is light-blind, so it can only
wash; marched inscatter is light-driven, so it can only sculpt.

Companions: [RENDERING_INFRA_V2_DESIGN.md](RENDERING_INFRA_V2_DESIGN.md) §4
(the direction this graduates; §4's froxel alternative is rejected below) ·
[REALTIME_3D_DESIGN.md](REALTIME_3D_DESIGN.md) (shadow + fog substrate) ·
[CINEMATIC_POST_DESIGN.md](CINEMATIC_POST_DESIGN.md) (D2 deterministic-sampling
doctrine, reused verbatim; the sibling amendment landing with this doc adds AO
denoise + GTAO there).

## 1. Audit — what exists (verified 2026-07-13, tip `a11e93d6`)

| Piece | Where | State |
|---|---|---|
| `Atmosphere` CPU wire (fog_color, fog_density, height_falloff, ambient_tint); unwired default = off, byte-identical | `node_graph/atmosphere.rs:23-51` | Extend with 3 shaft fields (D1). The "unwired = zero cost" contract is documented in the module header and must survive |
| `node.atmosphere` producer, 8 port-shadowed scalar params | `primitives/atmosphere.rs:42-133` | Extend with 3 params (D1) |
| Analytic exp fog, applied to straight rgb pre-exposure: `1−exp(−density·dist·exp(−falloff·max(y,0)))` lerp toward fog_color, alpha untouched | `shaders/render_scene.wgsl:516-526` (`apply_fog`), exposure `exp2(exposure_ev)` after it at `:543` | KEEP as the surface-extinction term (D4). Order verified: fog pre-exposure, fog_color gets exposed with the scene — physically consistent |
| `Light` CPU wire: `mode` Sun/Point, `pos/aim/dir`, `color` (premultiplied intensity), `range` (Sun: ortho half-extent; Point: attenuation half-distance, `1/(1+d²/range²)`), `cast_shadows`, `shadow_softness`, `shadow_bias`, `shadow_resolution` | `node_graph/light.rs:118-147`; attenuation `light.rs:261` | The march consumes it as-is. Point shadow map is a single frustum along `aim` (`light.rs:48-51`), NOT a cube map |
| Shadow maps: `MAX_SHADOW_CASTING_LIGHTS = 4` slots, per-light resolution, lazily created | `render_scene.rs:116` (cap), `:294` (slots), `:612` (`ensure_shadow_map`) | The march samples the same maps. Lights beyond 4 casters, and `cast_shadows=false` lights, have no map — they contribute UNSHADOWED inscatter (D2, honest cost) |
| Shadow sampling: `sample_shadow` K=4 switch over `shadow_map_0..3`, `sampler_comparison` @binding(14) | `shaders/render_scene.wgsl:198-204`, `:148` | `textureSampleCompareLevel` is legal in compute — the march reuses the same comparison-sampler pattern in its own bind group |
| Per-light shadow view-proj matrices (caster table built per frame for the raster pass) | `render_scene.rs:1184-1310` region | ⚠ VERIFY-AT-IMPL (P2): read how the caster table stores each light's view-proj + slot index; the march pass needs the same matrices in its uniforms — transcribe the existing layout, don't rebuild it |
| Resolved scene color: `Rgba16Float`, MSAA memoryless, resolves at pass end | `render_scene.rs:473,535` (format), `:78,253` (resolve) | HDR headroom exists — beams may exceed 1.0. The march composites AFTER resolve |
| Resolved scene depth: raw [0,1] R32Float, lazy Sample0 resolve when `depth` output wired (GBUFFER D1/D2) | GBUFFER_DESIGN.md; `DepthMsaaPassDesc` seam in render_scene | Shafts-on forces the internal depth resolve even when the `depth` output is unwired (D3) — same lazy machinery, one more trigger |
| Camera fov/near/far + `linearize_depth` shared helper | `shaders/shared/depth.wgsl` (GBUFFER D4); camera uniform fields already in `RenderSceneUniforms` | The ray reconstruction inputs |
| Committed deterministic hash (per-pixel rotation/jitter) | CINEMATIC_POST D2: `fract(sin(dot(px, vec2(12.9898, 78.233))) · 43758.5453)` | Reused as the march-start jitter — same doctrine: no temporal accumulation, no noise textures, export-deterministic |
| BUG-118 fog washout (constant-color fog reads as milk at macro scales) | `docs/BUG_BACKLOG.md` BUG-118, OPEN | Absorbed: P1 characterizes numerically, this design supersedes the look problem (the backlog entry already names this doc's lane as the superseder) |
| Volumetric/god-ray/froxel code | nowhere (`rg -i 'froxel|volumetric|god.?ray' crates/` → renderer hits none, 2026-07-13) | Genuinely new: two internal compute kernels (march, upsample-composite) inside render_scene |

§2.5 audit statement: **zero new graph primitives.** The feature is internal
to `render_scene` (precedent: shadows P2, fog P3 — both scene-internal, both
exempt from the codegen path as part of the render_* draw boundary), plus
fields on an existing CPU wire type. The march and upsample kernels are
internal pipelines like `ensure_shadow_map`'s depth pipelines, not atoms — they
are not user-wireable, not in the picker, and not fusable by definition.

Binding constraints: **hot path** — two compute dispatches per frame when
enabled; half-res march bounds the cost; `shaft_intensity == 0` (default) skips
everything (the unwired-zero-cost contract extended). **Performance surface** —
`fog_density` and `shaft_intensity` are the two faders; both port-shadowed on
`node.atmosphere`, cardable, LFO-able. **Persistence** — presets carrying the
new params serialize as ordinary graph JSON; `Atmosphere` is not serialized
directly (rebuilt per frame from node params), so no load-migration.
**Threads / time model** — untouched.

## 2. Decisions

**D1 — The haze stays on the Atmosphere wire; three new fields, one fader.**
Peter: *"keep the haze simple for now, global fader."*

```rust
// node_graph/atmosphere.rs — appended fields, same Copy struct
pub struct Atmosphere {
    // ... existing four fields unchanged ...
    /// Light-shaft (volumetric inscatter) master gain. 0 = off (default):
    /// the march never runs, output byte-identical to today. THE fader.
    pub shaft_intensity: f32,
    /// Henyey–Greenstein anisotropy g ∈ [-0.9, 0.9]. 0.6 default —
    /// forward-scattering, sun-shaft look. Negative = backscatter halo.
    pub shaft_anisotropy: f32,
    /// March step count: 0 = Low (16), 1 = Med (24, default), 2 = High (32).
    pub shaft_quality: u32,
}
```

`node.atmosphere` gains `shaft_intensity` / `shaft_anisotropy` (port-shadowed
ScalarF32, defaults 0.0 / 0.6) and `shaft_quality` (Enum Low/Med/High, default
Med) — same param pattern as the existing eight (`primitives/atmosphere.rs:42`).
The march's density function is **the same fog field the analytic fog uses**:
`σ(x) = fog_density · exp(−height_falloff · max(x.y, 0))` — one haze, two
renderings; `fog_density` is the density fader for both. Rejected: a separate
`node.volumetrics` producer (two haze sources that can disagree = double-fog
authoring trap); per-light shaft params (contradicts "keep it simple"; a
light's own color/intensity already shapes its beam — Peter: "lights will do
that for us").

**D2 — Single-scattering march over the light table; every light contributes.**
Peter: *"All lights should support volumetrics."* After the MSAA pass resolves,
one compute dispatch at **half resolution** marches each pixel's view ray from
the camera to the scene depth (background pixels: to `far`). Committed math —
the WGSL and the CPU reference implement THIS, no variation:

```text
N        = 16 | 24 | 32                       (shaft_quality)
ray      = camera → world position of this pixel's resolved depth (far if sky)
seg      = ray_length / N
t0       = (hash(px) − 0.5) · seg             (committed D2 hash; deterministic)
T        = 1.0                                 (transmittance)
L        = vec3(0)                             (accumulated inscatter)
for i in 0..N:
    x        = ray_origin + ray_dir · (seg·(i+0.5) + t0)
    σ        = fog_density · exp(−height_falloff · max(x.y, 0))
    for each wired light l:
        vis  = shadow(l, x)                    // below
        att  = Sun: 1.0 · Point: 1/(1 + d²/range²), d = |x − l.pos|  (light.rs:261)
        ph   = HG(g, cosθ) = (1−g²) / (4π · (1 + g² − 2g·cosθ)^1.5)
               // θ between ray_dir and the direction light→x (Sun: l.dir)
        L   += T · σ · vis · att · ph · l.color.rgb · seg
    T       *= exp(−σ · seg)
out = L · shaft_intensity · exp2(exposure_ev)   // exposed like the scene, wgsl:543
```

`shadow(l, x)`: light has a shadow slot → project `x` by that light's stored
view-proj; inside the map → ONE `textureSampleCompareLevel` tap (no PCF kernel
in the march — the half-res upsample is the softener); outside the map's
frustum, or no slot (`cast_shadows=false`, or beyond the 4-caster cap) →
`vis = 1.0` (unshadowed glow). **Consequences, stated honestly:** a Point
light's map covers a single frustum along `aim` (light.rs:48-51) — its beams
are carved only inside that cone, and its light elsewhere glows unshadowed;
unshadowed lights never form rays (nothing to carve them). Physically: a light
that carves means shadow-casting geometry, so "rays form off the stems and
leaves" requires the plant to be lit by a shadow-casting light — which is the
CinematicScene default (`key_light.cast_shadows`). Worst case cost is
N·lights shadow taps per half-res pixel (32·4 = 128 at High); the P3 perf gate
measures it and the quality enum is the relief valve.

**D3 — Half-res march, depth-aware upsample, additive composite.** The march
writes a half-res `Rgba16Float` inscatter texture (lazily created via the
`ensure_shadow_map` pattern, `render_scene.rs:612`). A second dispatch
upsamples to full res and ADDS into the resolved color. Committed upsample: for
each full-res pixel, 4 bilinear taps of the half-res inscatter, each weighted
`b_i · exp(−(Δz_i / z_full)² · 400)` where `Δz_i = |linearized full-res depth −
linearized half-res depth at tap i|` (depth-similarity kills beam bleed across
silhouettes); renormalize by the weight sum (fallback to plain bilinear when
the sum < 1e-4). Shafts-on forces the internal Sample0 depth resolve plus a
half-res depth downsample (point-sample the resolved depth — committed; no
min/max depth puzzle in v1). Whether the composite is a read-write storage
write in the upsample dispatch or a separate additive blend is implementation
latitude; the contract is the committed weights and `color.rgb += result`,
**alpha untouched**. Default + trigger (no unlabeled fork): alpha stays
untouched because the compositor contract treats un-backed rgb as additive
light over transparency; the P2 demo renders the void over a checkerboard —
if beams are invisible over transparency there, STOP and escalate to Peter
with the recorded fallback (`a += (1−a)·clamp(max(rgb), 0, 1)`), do not
improvise. Rejected: full-res march (4× the cost for banding the jitter
already hides); marching in the lit fragment shaders (beams exist where no
geometry is — a surface pass cannot draw a shaft across the void, this is THE
plausible-wrong shortcut here, forbidden by name).

**D4 — The analytic fog stays; the march adds inscatter only.** `apply_fog`
(wgsl:516) remains the surface-extinction term: geometry fades toward
fog_color with distance exactly as today. The march contributes ONLY the added
light term — its transmittance `T` attenuates the *accumulated inscatter*,
never re-darkens geometry (no double-extinction). Authoring model, written for
the preset docs: **fog_color black + density up + shaft_intensity up = Peter's
black void with beams** (extinction darkens with distance, beams add light);
fog_color non-black = classic atmospheric fade plus beams. BUG-118 is absorbed:
its milk-filter symptom is the light-blind constant-color model, and the fix
is not a fog curve tweak but this design; P1 still runs the numeric
characterization the backlog entry asks for, so the fog term's behavior is
pinned by numbers before the shafts land on top. Rejected: replacing
`apply_fog` with marched extinction on surfaces too (correctness win ≈ 0, cost
= a full-res march or visible half-res extinction seams on geometry edges).

**D5 — Deterministic, no temporal accumulation** — CINEMATIC_POST D2 doctrine
verbatim: committed hash jitter, fixed step counts, output a pure function of
inputs, two exports bit-identical. Banding relief is jitter + half-res
upsample smoothing, not TAA. Rejected: blue-noise texture (asset dependency;
same rejection as CINEMATIC_POST D2), frame-index jitter (breaks export
determinism and the journey-proofs property).

**D6 — Demo rule for this design: numeric gates AND a looked-at PNG.** The
cinematic-post cluster ran numeric-only by Peter's 2026-07-12 directive (*"No
need to produce the PNGs if they're not going to look at them"*); his
2026-07-13 verdict on the result — *"look terrible and need a lot of work"* —
is the observed failure of that premise for look-critical work. This design's
phases each end with ONE headless render (`render-generator-preset`, minding
BUG-117's async caveat — the committed demo scene is fully procedural) that
lands in the landing report for Peter's eyes. Gates stay numeric; the PNG is
the acceptance demo (L2), and Peter's look-pass is the phase's real exit.

## 3. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| V1 — `shaft_intensity == 0` (default/unwired) is byte-identical to today's output, and allocates nothing | gpu_test `shafts_off_byte_identical` (render twice, intensity 0 vs pre-change golden buffer, byte-compare) + unit test `wants_shafts_gate` on the CPU decision fn (off → no `ensure_` call; assert slot stays `None`) |
| V2 — Deterministic: same graph, two runs, bit-identical | gpu_test `shafts_two_runs_bit_identical` (the journey-proofs property, scene-local) |
| V3 — March matches its CPU reference on synthetic inputs (small grid, one Sun, fabricated 8×8 shadow map, flat depth) within 1e-3 | gpu_test `shaft_march_matches_cpu_reference` — same committed-math-twice pattern as CINEMATIC_POST I1 |
| V4 — Alpha is untouched by the composite | gpu_test `shafts_leave_alpha_untouched` (readback alpha plane, compare against shafts-off run) |
| V5 — Upsample never bleeds across a depth silhouette by more than the committed weight allows | CPU unit test on the committed upsample weights: synthetic near/far step edge, assert far-side inscatter contribution at the near-side pixel < 1% |
| V6 — The existing analytic fog is numerically characterized before shafts land | P1 gate: `render-generator-preset` density sweep at camera distance 9 and 30, near/far luminance-attenuation ratios reported as numbers in the landing report (BUG-118's fix-shape command, executed) |

## 4. Phasing

Common read-back for all phases: this doc §2 whole ·
`render_scene.rs` module header + the anchors in §1 · CINEMATIC_POST D2 (the
hash + determinism doctrine). **Forbidden moves, all phases:** screen-space
radial-blur "god rays" post effect (only works with an on-screen sun; not
"all lights"; forbidden by name) · marching in lit fragment shaders (D3) ·
a separate volumetrics node or second haze parameter family (D1) · temporal
accumulation / frame-index inputs (D5) · algorithm substitution (the committed
math is the contract; upgrades are new decisions) · full-res march "because
it looked banded" (fix is jitter/quality, D3/D5) · touching `apply_fog`'s
math beyond what P1's characterization demands (D4). **Test scope:** focused
`-p manifold-renderer --lib` + the named gpu_tests (`--features gpu-proofs`,
plain `cargo test`, never nextest); workspace sweep at landing.

- **P1 — Atmosphere fields + plumbing + fog characterization** (one session).
  Entry: `rg 'shaft_intensity' crates/` → 0 hits; anchors §1 re-verified.
  Deliverables: the three `Atmosphere` fields + defaults + doc comments
  (atmosphere.rs), three `node.atmosphere` params, fields threaded into
  `RenderSceneUniforms` (zero-cost when off), V1 tests (`shafts_off_byte_
  identical`, `wants_shafts_gate`), V6 fog sweep executed and reported,
  BUG-118 backlog entry updated with the measured numbers + `Absorbed-by:
  VOLUMETRIC_LIGHT_DESIGN` pointer. Gate: V1 + V6 + focused suite + scoped
  clippy. Demo: none — L1 (plumbing; the fog sweep PNGs go to the landing
  report as V6 evidence anyway). Performer gesture: none yet (no visible
  surface).
- **P2 — Sun shafts, vertical slice** (one session). Entry: P1 landed;
  ⚠ VERIFY-AT-IMPL from §1 resolved: read the caster-table layout
  (`render_scene.rs:1184-1310`) and record the view-proj access in the phase
  notes before writing the kernel. Deliverables: half-res march kernel (Sun
  lights only), depth downsample, committed bilateral upsample + additive
  composite, internal depth-resolve trigger, lazy texture creation
  (`ensure_shadow_map` pattern), V2/V3/V4/V5 tests. Gate: V2–V5 + V1 re-run
  + focused suite + scoped clippy. **Acceptance demo (L2, D6):** committed
  procedural scene — black void (`fog_color` 0,0,0, density 0.15), one Sun
  through `node.grid_mesh` geometry, `shaft_intensity` 1.0, rendered at two
  intensities (0.3 / 1.5) plus once over a checkerboard background (the D3
  alpha trigger check); PNGs in the landing report, Peter look-pass is the
  exit. Performer gesture: `shaft_intensity` card on a fader — the whole
  atmosphere swells in and out; gate drives the param and asserts monotonic
  luminance response in the beam region.
- **P3 — Point lights, multi-light, quality + perf** (one session). Entry: P2
  landed. Deliverables: Point attenuation + frustum-clipped shadow sampling
  (outside-frustum → unshadowed, D2), multi-light accumulation, quality enum
  wired (16/24/32), CPU reference extended to a Point case (V3 re-proof), perf
  measurement. Gate: V1–V5 re-run + `MANIFOLD_RENDER_TRACE=1` run on a
  4-shadow-caster scene at High quality — no frame >20ms on the content
  thread (the standard's content-thread gate), measured numbers in the
  landing report + focused suite + scoped clippy. **Acceptance demo (L2,
  D6):** night-garden shot — void, two Point lights (one shadow-casting
  through geometry, one bare glow), PNGs at Med and High quality. Performer
  gesture: key light color bound to a beat envelope — the beams pulse with
  the music with ZERO new binding work (*"god rays shouldn't react to audio,
  lights will do that for us"*); gate drives `light.color` across two frames
  and asserts the beam region tracks it.

Phasing-completeness check: D1→P1, D2→P2 (Sun) + P3 (Point/multi), D3→P2,
D4→P1 (characterization) with the authoring model exercised in P2's demo,
D5→P2 (V2), D6→P2/P3 demos. Every body-committed affordance (two faders, the
quality enum, beams-follow-lights) lands in a named phase; nothing rides on
"later".

## 5. Decided — do not reopen

1. Haze lives on the Atmosphere wire; three new fields; `fog_density` +
   `shaft_intensity` are the two faders; no separate volumetrics node (D1).
2. Single scattering, marched per pixel at half res against the existing
   shadow maps; every wired light contributes; unshadowed lights glow, only
   shadow-casting lights carve (D2).
3. The analytic surface fog stays; the march adds inscatter only; black
   fog_color is the authoring model for the void look (D4).
4. Deterministic forever: committed hash jitter, fixed steps, no temporal
   accumulation, no noise assets (D5).
5. No screen-space radial god-ray fake — ever. It cannot serve off-screen or
   multiple lights and would betray the "all lights" contract (D2, forbidden
   move).
6. Look-critical phases gate numeric AND ship a PNG Peter actually looks at
   (D6 — the 2026-07-13 lesson).
7. No audio-reactive params on the shafts themselves; light modulation is the
   performance path (Peter, verbatim in the intro).

## 6. Deferred

- **Froxel volumetrics / many-light scaling** — trigger: stage scenes need
  >4 shadow-casting lights or per-light beam cost dominates the trace.
  (RENDERING_INFRA_V2 §4 names froxels; rejected for v1 as a clustered-volume
  infrastructure build with no current scene demanding it.)
- **Cube-map point shadows** (beams carved in all directions from a Point
  light) — trigger: Peter stages a lantern-in-fog look and the single-frustum
  carve reads wrong. Inherits REALTIME_3D's shadow architecture, not this doc.
- **Per-light shaft overrides (color tint, per-light intensity)** — trigger:
  Peter asks while authoring the EP scenes; shape: fields on `Light`, not on
  `Atmosphere`.
- **Volumetric shadows from alpha-cutout foliage** (leaf-shaped ray detail
  depends on the shadow pass respecting alpha cutout) — trigger: P2's demo
  through a photoscanned plant shows solid-quad shadows where leaf gaps
  should be. ⚠ VERIFY-AT-IMPL (P2, five minutes): check whether the shadow
  depth pass samples albedo alpha for cutout materials; record the answer in
  the phase notes either way.
- **Wispy haze — animated density noise (the committed follow-on).** Peter,
  2026-07-13: *"Proper realistic haze that wisps, billows, and drifts would
  be very nice, maybe as an upgrade once we get the basic god rays working."*
  Trigger: P1–P3 landed and Peter green-lights after the P3 look-pass —
  becomes P4 via a short amendment, not a new design. Committed shape so the
  amendment is small: the march's `σ(x)` gains a multiplicative fBM noise
  term (2–3 octaves, procedural, no texture asset) sampled at `x · noise_
  scale + wind · scene_time`; new `node.atmosphere` params `noise_amount`
  (0 = today's uniform haze, byte-identical — the V1 contract extends),
  `noise_scale`, `wind_x/z`, `drift_speed`. Time-DRIVEN animation is legal
  under D5 (scene time is an input; D5 bans cross-frame accumulation, not
  time), so exports stay deterministic. Honest cost: one fBM eval per march
  step; expect roughly +30–50% march cost — the quality enum is the relief
  valve, and the P3 perf trace is the baseline it gets measured against.
- **Height-fog ground plane variants / fog volumes** — trigger: a scene
  needs haze bounded to a region rather than scene-wide.
- **Export-tier step counts** (auto-High in export) — export tiers were
  DROPPED by Peter (RENDERING_INFRA_V2 status); the quality enum is
  hand-settable per scene, which covers the need manually.
