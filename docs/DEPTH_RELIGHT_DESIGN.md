# DEPTH_RELIGHT_DESIGN — 2.5D relighting as compiler infrastructure

<!-- index: The "3D Shading" toggle: depth as a compiler-propagated companion channel, a fixed relight stage (Lambert + specular + heightfield GTAO + shadows), heightfield-native shadow atom, fp32 precision for derivative consumers. Zero graph edits for existing presets. -->

**Status:** APPROVED 2026-07-17 (Peter, in-session, after the rendered probe sweep) — EXECUTING. P1/P2/P4 dispatched to Sonnet lanes same session; P3/P5/P6 follow.
**Author:** Fable, from the 2026-07-17 depth-relight probe session.
**Prior landings from the probe (already on main):** GTAO `slices`/`steps` quality params; GTAO `projection = Height Field` mode (ortho frame, raw-height read, fp32 in-kernel); `graph_tool render` verb; BUG-216/217 logged.

## 1. Goal and evidence

One toggle ("3D Shading") on any effect or generator makes its output read as lit, occluded, self-shadowed relief — automatically, with zero re-authoring of existing graphs, zero cost when off, and no graph reasoning required from users or agents.

Evidence this looks good and generalizes: the probe sweep (5 fronts — feedback vortex, noise terrain, voronoi, shipped Caustics, shipped Watercolor — same fixed relight tail appended). Final sheet: v6, contour-free, saturated, crisp. The look recipe, the failure modes (blur mismatch, multiply desaturation, white-spec hue shift, perspective-AO haze, fp16 contour banding), and their fixes are all encoded in the decisions below — do not re-derive them; each was found by rendering and each fix was verified by re-rendering.

Cost: the full tail is per-pixel atoms + a 64-tap GTAO + a 9-tap blur pair — same class as the shipped cinematic stack, fractions of a ms at 1080p on M-series.

## 2. Decisions

**D1 — Depth is a companion channel propagated by the compiler, not a wire users route.**
Every primitive declares a `depth_rule` in its `primitive!` metadata (exactly like `fusion_kind`):
- `Inherit` — pass the input's depth through untouched (all pure color ops: tint, contrast, tone map, …).
- `Warp` — sample depth with the same UV transform as color (mirror, kaleidoscope, transform, displace, feedback). Mechanical: the kernel's resample expression applied to the depth channel.
- `CombineNearest` — multi-texture-input nodes: per-pixel nearest-depth wins (z-buffer semantics). `node.mix` Lerp mode lerps depth by the same amount.
- `SourceHeight` — the node's output IS a height/scalar field (noise, voronoi, SDF shapes, gradients): depth = own output luminance.
- `Terminal` — no meaningful depth (IO, bridges, control-rate). Chains ending with only Terminal producers have no depth origin; the toggle then uses D4's fallback.
The compiler walks the graph once, synthesizes the depth path, and dead-codes it entirely when the toggle is off (compiled variant, not a runtime branch).

**D2 — The toggle is a per-preset-instance flag, not a graph edit.** A standard card param (`3D Shading`, toggle) on every effect/generator card. On = the freeze pipeline compiles the augmented variant (depth companion + relight stage appended before `final_output`). Off = the exact graph that ships today, byte-identical. No preset JSON changes anywhere; old projects load unchanged and gain the toggle.

**D3 — The relight stage is a fixed compiler-emitted template** (the probe's v6 tail, proven across all 5 fronts):
height → per-pixel dither (Random noise, scale≈canvas, amp 0.003) → 9-tap σ2 blur H+V → `surface_bumps` (normals) → `basic_light` (Lambert) and `shininess` (specular, **tinted by the source color** — white spec reads as hue shift) → `ssao_gtao` (projection=Height Field, relief 0.25, radius 0.02, 4×8) → 9-tap soften H+V → AO multiplies **the Lambert term, never the final color** (saturation survives) → source × shading + spec → exposure gain 1.4 (multiplies can only darken; restore the source's exposure).
Card params exposed by the toggle: Light X/Y, Relief, AO Intensity, Shadow Softness (P1's atom), Gain. Plus D4's height-source override.
Anti-goals, each a rendered failure from the probe: no tone map in the stage; no big blur before the normal derivative; no AO multiply over final color; no levels/smoothstep hack (heightfield GTAO reads flat = 1.0 exactly).

**D4 — Height origin: structural when the rules provide it, luminance fallback otherwise.** The D1 walk yields a depth channel when any producer is `SourceHeight`/`Warp`-reachable; a chain with none falls back to luminance-of-output as height — the *proven* default (the whole probe sweep ran on it), surfaced as a card enum (`Height From: Auto | Luminance | Inverted Luminance`), never a silent guess a user can't see.

**D5 — `node.heightfield_shadow` (the one new atom).** Screen-space raymarch of the height texture toward the light: for each pixel, march N steps along `-light_dir.xy` (uv units), occluded where marched height (plus slope of `light_dir.z`) exceeds the ray; soft shadow = penumbra from closest-miss distance (standard heightfield shadowing). Inputs: `height` Texture2D, optional light_x/y/z scalars (same convention as `basic_light`). Params: `steps` (default 24), `strength`, `softness`, `relief` (same meaning as GTAO's). Fusable (`GatherTexel`, wgsl_body, fp32 in-kernel), CPU reference + hand-oracle parity + analytic tests (flat field = unshadowed; single raised bump casts a shadow strictly on the away-from-light side). Multiplies into the Lambert term next to AO in D3.

**D6 — Precision is a compiler property.** Two parts:
(a) The freeze compiler allocates **fp32 (Rgba32Float) for materialized intermediates whose consumer differentiates or gathers** (`input_access` contains `GatherTexel`, plus `surface_bumps`/`node.gradient`-class derivative consumers — mark via a new `precision_critical` flag on the input declaration). Fused-region interiors already live in fp32 registers; this closes the boundary case. Evidence: the fp16 contour iso-lines, eliminated in-kernel by the GTAO heightfield mode; the general mechanism does the same for every future consumer.
(b) BUG-216's promised-but-missing copy fallback in `late_capture` (`execution.rs:1689`) ships as part of this design — feedback loops wired to boundary outputs must degrade to one copy, never to a frozen loop.

**D7 — BUG-217 (non-Lerp mix alpha) is satisfied inside the template:** the relight stage's own additive blends operate on the source's alpha contract; the template inserts nothing that widens alpha. Document the `set_alpha`-before-blend idiom in `node.mix`/`node.feedback` composition_notes (the cheap fix shape already logged).

## 3. Phases

| Phase | Content | Depends | Gate |
|---|---|---|---|
| **P1** | `node.heightfield_shadow` atom per D5 | — | clippy; unit + analytic tests; GPU parity (CPU ref, hand-vs-generated); `graph_tool render` of a voronoi+shadow probe, eyeballed |
| **P2** | `depth_rule` metadata: enum + `primitive!` slot + declaration on ALL primitives + meta-test that every registered primitive declares one (default = explicit, not implicit) | — | clippy; workspace sweep green (meta-test enforces coverage); classification table in the PR body for review |
| **P3** | Compiler: depth-companion synthesis pass + toggle → emit D3 template; off = identical plan (assert structural equality in a test) | P1, P2 | clippy; freeze proofs; golden: toggle-off graph plan == today's plan for every bundled preset |
| **P4** | D6(a) fp32 intermediate allocation + D6(b) BUG-216 copy fallback | — | clippy; GPU suite; BUG-216 repro (feedback→final_output) accumulates trails; update BUG-216 Status |
| **P5** | Card toggle + params surface (D2/D3/D4), EditingService command path | P3 | clippy; focused tests; ui-snap `editor` render eyeballed |
| **P6** | Acceptance: regenerate the 5-front sweep via `scripts/depth_relight_sweep.py` with the TOGGLE (not a hand tail); perf-soak on the canonical fixture; full workspace + GPU sweep | P3–P5 | sweep eyeballed vs v6 baseline; no perf regression; Peter sign-off |

P1/P2/P4 are independent — dispatched in parallel. P3 is the core and follows P1+P2. Lanes land per the merge-trunk protocol; batch landings per 2–3 phases.

## 4. Acceptance

The v6 sweep is the baseline: five fronts, 2D row untouched, 3D row via the *toggle* — continuous shading, no contours, no haze, saturation preserved, and the toggle off compiles to today's exact plans. Probe artifacts (tail generator + sweep harness): `scripts/depth_relight_sweep.py`.
