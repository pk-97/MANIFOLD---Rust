# Rendering Infra v2 — the cinematic baseline

**Status:** DIRECTION captured 2026-07-11 · Fable + Peter discussion · not designed to STANDARD, not built. Full per-piece designs graduate from this doc after the scene-ladder intake (Scenes 1–3, in flight) and PERF_BUDGET_GATE numbers; Peter directed capture-now so the discussion survives ("Better to have them laid out with all of the ideas etc now than forget this entire discussion").
**Graduated 2026-07-12 (Fable, Peter's picks):** §2 → [GBUFFER_DESIGN.md](GBUFFER_DESIGN.md) · §6 → [CAMERA_AND_LENS_DESIGN.md](CAMERA_AND_LENS_DESIGN.md) + [CINEMATIC_POST_DESIGN.md](CINEMATIC_POST_DESIGN.md) (DoF/SSAO/motion blur) · §8 → REALTIME_3D §11 (PCSS, P9). **§5 export quality tiers DROPPED by Peter 2026-07-12** ("Ignore the explicit per-scene export quality please" — quality knobs are ordinary explicit scene config; no tier mechanism). §3 materials v2 DEPRIORITIZED by Peter 2026-07-12 (his assets are game models/photoscans; black-background hero scenes) — stays captured here, not designed. **§4 volumetrics → [VOLUMETRIC_LIGHT_DESIGN.md](VOLUMETRIC_LIGHT_DESIGN.md) (graduated 2026-07-13, Fable + Peter — god rays for the Interim EP; froxels stay deferred there).** §7 HDR, §9 RT, §10 animated glTF remain here awaiting their turn.
**Companion docs:** [REALTIME_3D_DESIGN.md](REALTIME_3D_DESIGN.md) (the base scene pass this extends; its §10 documents the memoryless-depth constraint), [MATERIAL_SYSTEM_DESIGN.md](MATERIAL_SYSTEM_DESIGN.md) (v1 contract this extends), [PERF_BUDGET_GATE_DESIGN.md](PERF_BUDGET_GATE_DESIGN.md) (the measuring instrument), [VULKAN_BACKEND_DESIGN.md](VULKAN_BACKEND_DESIGN.md) (the non-Apple hardware path).

## The claim

Peter, 2026-07-11: MANIFOLD scenes should "look SOTA … genuinely stunning, cinematic
grade," and this cluster is "our highest prio in terms of next set of features …
it sets the baseline for the rendering infra that everything else will build on top
of with simulations etc." Direction confirmed verbatim: "Love all of this and yes
it's the correct direction."

Cinematic-grade has three pillars, and MANIFOLD is currently strong in exactly one:

1. **Light transport** — what light does in the scene (materials, emissive,
   transparency, shadows, GI approximations). v1 shipped the base: multi-light,
   shadow maps, distance fog, unlit/phong/pbr/cel materials.
2. **Image pipeline** — what the camera does to the image (DoF, motion blur, tone
   map, bloom, grade). Our 2D graph system already excels here; the missing inputs
   are per-pixel depth and motion vectors.
3. **Content** — what's in the scene (scans now; splats and animated assets later).

The keystone that unlocks pillars 1 and 2 simultaneously is one storage decision
(§2). Hardware note, Peter verbatim: "The Macbook is not the ceiling for hardware,
it's just what I have right now" — everything here designs for per-backend seams,
not for the current chip.

## 1. Sequencing (settled 2026-07-11)

- **Scene ladder first** (running, other agent): Scenes 1–3 are the intake evidence —
  what a lit scene here actually lacks is the requirements list for §3. First
  finding already in: BUG-118 fog-washout, fog pulled from Scene 1 (`0d65db3d`).
- **PERF_BUDGET_GATE builds in PARALLEL** (approved, Sonnet-executable). Every
  decision below is a milliseconds question; the instrument precedes the measured
  decisions, not the capture of direction.
- **This doc reviewed/amended as scenes land**; per-piece STANDARD designs authored
  after intake (Opus block). HDR-out (§7) and physical camera (§6) are separable and
  can graduate first if design capacity is idle.

## 2. Stored G-buffer — the keystone decision

Scene depth today is memoryless MSAA tile memory — deliberately never written to RAM
(zero bandwidth; REALTIME_3D §10 records why and rejected a depth-compositor on this
ground). Storing depth (+ motion vectors later) to real textures unlocks, in one
decision: depth of field, motion blur, SSR/screen-space refraction, SSAO, MetalFX
temporal upscaling, RT denoising, and depth-aware compositing.

**Consequences, stated honestly:** a full-res depth write+read every scene frame —
a few GB/s at 4K60, affordable on Apple silicon but a permanent perf line item; and
motion vectors for graph-deformed geometry require previous-frame vertex positions
(deform runs twice or caches output — real cost, unmeasured). This is THE measured
decision the perf gate exists for. Decide: always-store vs per-scene opt-in vs
quality-tier-gated (§5).

## 3. Materials v2

Peter verbatim: "emissives and transpent and transmissions is something we would
100% want." Payoff order argued in-session:

1. **Emissive channel + auto-derived light.** No GI in a rasterizer — an emissive
   surface cannot light neighbors (settled physics, not a MANIFOLD gap; engines fake
   it). The move Peter explicitly endorsed ("I like the automatic fake lights
   idea"): the scene pass derives a scene light from an emissive object (centroid,
   emissive color, intensity from the same param) and injects it into the existing
   ring-buffered light array, shadow-casting included. One authored thing — "this
   material emits" — and the glow, the ground pool, and the shadows follow one
   fader. Open (design-time): centroid of a scattered instance group; behavior at
   the light-budget/caster cap; whether derived lights count against
   `MAX_SHADOW_CASTING_LIGHTS = 4`.
2. **Graph-generated normal maps + parallax.** Per-pixel normal perturbation, with
   the map being a *live graph output* (height-field noise → height-to-normal atom):
   lava crust that cracks in time, rain ripples on puddle normals, glinting facets.
   Parallax mapping on top for "fake depth" in cracks/facets. Pure per-element math —
   codegen-path by construction.
3. **Alpha cutout** (alpha-test, order-independent, cheap) — scan foliage/petals
   likely need it immediately; Scene 1 will confirm.
4. **Ambient/environment term + matcap MaterialKind.** The missing "looks real"
   ingredient — without an ambient term shadows fall to black and metals have
   nothing to mirror. Matcap (baked sphere-capture sampled by normal) is the
   industry cheat that buys the whole diamond/chrome/glass/gem family without
   transmission rendering; `matcap_two_tone` exists as a 2.5D atom but matcap is
   NOT currently an in-scene MaterialKind.
5. **Sorted blended transparency.** The rasterizer classic: back-to-front draw
   order, depth-write off. Medium cost (sorting infra in the scene pass).
6. **Transmission/refraction LAST** — light through petals, refraction through
   diamond. Needs a scene-color grab (screen-space), which collides with the same
   memoryless constraint as §2 — it is gated on the G-buffer decision.

Persistence constraint (binding): the material def serializes — every addition
load-migrates old projects; canonical fixture is the test. Audit items owed before
the full design (oracles named, run then): does the v1 material def already carry
an emissive channel (read `material.rs`); can the material path take a perturbed
normal input today (read the four material atoms + `render_scene.wgsl` lighting).

## 4. Volumetric light shafts

Fog today is distance-only. God rays — sun breaking through the apricot canopy —
are the largest cinematic-per-millisecond gap in the catalog. Froxel or
shadow-map-marched volumetrics: a real design with a real frame cost; quality-tier
candidate (§5). Scene 1 (fog + sun + canopy) is its intake evidence — note its
first fog finding is already a bug (BUG-118 washout), so §4's design starts from
"why did distance fog wash out a lit scan" before adding shafts.

## 5. Live vs export quality tiers

**Possibly the highest-leverage single feature for the ~Aug release.** The export
path is not 60fps-bound — a frame may take 200ms and nobody knows. One scene, two
setting tiers: live (cheap, frame-safe) and export (full-res RT, high-res shadows,
fat volumetrics, max MSAA) — cinema-grade release video NOW, on current hardware,
while the live rig runs the same scene cheap. Design questions: where tiers live
(per-scene? per-export-job?), what they cover, and that A/B'ing tiers must not
change *composition* (same graph, same params — only quality knobs move).

## 6. Physical camera model

Focal length, aperture, shutter — one param set that ties DoF (needs §2 depth),
motion blur (needs §2 motion vectors), and exposure together so they behave like
one lens. On stage: performable cinematography — rack focus on a fader. Separable
design; the camera atoms (`free_camera`/`look_at_camera`) are its precedent.

## 7. HDR end-to-end

We render HDR internally and tone-map to SDR at the door. The rig's towers are HDR
panels (GT2000HDR) and YouTube ingests HDR10 — so: EDR output live on macOS, HDR10
in export. Likely the most *visible* single upgrade for both stage and release
video. Separable design (output plumbing + export encode, minimal scene-pass
contact). Constraint from the memory corpus: the "HDR intermediates half-res
minimum" rule governs effect intermediates, not the output swapchain.

## 8. Soft shadows

Shadow maps are hard-edged today. PCF/PCSS filtered penumbra: cheap, per-element,
half of what separates "rendered" from "real-time" to the eye. Small enough to ride
along with any scene-pass phase; needs no new decision.

## 9. Hybrid ray tracing (post-release) — GRADUATED 2026-07-21 → `RAYTRACING_DESIGN.md`

**This section graduated to [RAYTRACING_DESIGN.md](RAYTRACING_DESIGN.md) (PROPOSED, P0 prototype gate).** Text below kept as the original direction record; the design doc supersedes it where they differ.

Realistic scope is *hybrid* — one or two RT terms, never path tracing: RT shadows
or RT AO first (smallest surface, most realism per ray), RT reflections (the
diamond), short-range emissive bounce eventually subsuming §3.1's derived light.
Recipe: quarter-res, ~1–2 rays/px, temporal accumulation, denoise, MetalFX upscale.
Apple silicon has hardware RT from M3; MetalFX is the DLSS-analog lever.

MANIFOLD-specific costs (all measured questions → perf gate): BVH refit every frame
for procedurally deforming meshes (`push_along_normals` moves vertices per-frame);
motion vectors + stored depth (gated on §2); denoiser tuning. P8 instances map
directly onto TLAS instancing — ten thousand blossoms are cheap TLAS entries.

**Backend seam (decided in principle):** RT and upscaling go behind per-backend
traits — Metal RT + MetalFX now, Vulkan RT + DLSS/FSR when the Vulkan backend
lands. No API leaks Apple types (the manifold-gpu discipline already forbids it).

## 10. Animated glTF import

Named content gap, unscheduled: scans are static; glTF carries skeletal/keyframed
animation we don't ingest. A scanned dancer or animated creature is a content class
no material work substitutes for. Trigger: the first scene that wants one.

## Rejected in-discussion (recorded for the 2am reinventor)

- **Emissive-as-real-GI in the rasterizer** — every-surface-asks-every-surface is
  the rendering equation; no per-element shortcut exists. The derived light (§3.1)
  IS the industry technique, made automatic.
- **Full path tracing real-time** — offline/export-tier only, if ever.
- **Depth-aware two-pass compositor** — already rejected by name in REALTIME_3D
  §10; the G-buffer (§2) does not reopen it (storing depth ≠ compositing passes;
  composited passes still can't exchange shadows/fog).
- **Per-instance material variation** — P8 deferral stands (instances share the
  group's material); "one diamond flower among ten thousand lava ones" is two
  groups. Trigger lives in REALTIME_3D §10 Deferred.

## Consciously parked behind this cluster (must not die)

Codegen conversion sweep (standing rule, non-blocking) · UI_WIDGET_UNIFICATION ·
**AUDIO_ANALYSIS_ACCURACY — BUG-069 licensing gates commercialization; it stays
next in line, not behind everything.**

## Owed

- Scene-ladder findings folded in (requirements for §3/§4; confirm cutout need,
  scan-vs-scene-lighting reality; BUG-118 root cause feeds §4).
- Perf-gate numbers for §2 (bandwidth, deform-twice cost) → the G-buffer decision.
- The two §3 audit items (emissive channel; normal input path) — run before the
  materials-v2 STANDARD design.
- Per-piece STANDARD designs, Opus block, after intake: materials v2 · G-buffer ·
  volumetrics · quality tiers · physical camera · HDR out · hybrid RT.
