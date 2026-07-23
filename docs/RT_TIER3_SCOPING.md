# RT Tier 3 — scoping pass for the remaining items

**Type: scoping pass, NOT a design contract** (DESIGN_DOC_STANDARD §1 — this is neither a design nor
a working guide; it is the intake that decides *which designs get written*). No phase here is
executable. Each item graduates to its own design — or into `RAYTRACING_DESIGN.md` §9+ — when a team
leader picks it up, and this doc's per-item section is that design's §0/§1 already done.
**Author:** Opus 4.8 · 2026-07-23 · audit anchors verified that day.
**Purpose:** `RAYTRACING_DESIGN.md` §8 Tier 3 is four one-line bullets. This pass turns them into
scoped work with the legwork spent: what already exists, what the item actually *is* once you look at
this codebase rather than the literature, what it costs, and what a reviewer must rule on. Reflections
(item 7) got the full treatment in `RT_REFLECTIONS_DESIGN.md`; the other three are here.

**Two audit findings reshape the tier before any sequencing argument.** They are the reason this doc
exists rather than four briefs.

**Finding 1 — "many lights" is the wrong frame for this renderer.** `RAYTRACING_DESIGN.md` §8 item 6
proposes ReSTIR, the standard answer to sampling hundreds of lights. This scene system does not have
hundreds of lights. It has: a `lights` storage buffer packed as **direction + premultiplied colour
only** (`render_scene.rs:2792-2794`; WGSL layout comment at `shaders/render_scene.wgsl:234-235`) —
positional data (`pos`, `range`) reaches only the volumetric march's separate `shaft_light_data`
packing (`render_scene.rs:2801-2806`), never the surface-shading loop; a hard cap of
`MAX_SHADOW_CASTING_LIGHTS = 4` (`render_scene.rs:129`); and RT visibility applied to **the sun
alone**, in both the surface path (`raytrace.rs:735`'s kernel takes a single `sun_dir`) and the
volumetric one (`shaders/shaft_march.wgsl:221` — `select(shadow_vis(...), rt_sun_vis, rt_enabled &&
is_sun)`). The real "many lights" in MANIFOLD are **emissive surfaces**, which are already sampled —
badly, at 2 spp — by the GI gather. So item 6 is two different pieces of work with a factor of ten
between them in cost, and the cheap one is most of the value. Split accordingly (T3-6a / T3-6b below).

**Finding 2 — the trace kernel is one function accumulating ray classes, and Tier 3 breaks that.**
`trace_shadow_rays` (`raytrace.rs:735-990`) now carries shadow + AO + GI + sun-bounce in one thread
body, each a hand-written block sharing `origin`/`n`/`bias_eps`. That has worked for four extensions
because every ray class was one bounce deep and returned a scalar or a demodulated colour. **T3-8
(multi-bounce) is the first item that needs recursion**, and adding it as a fifth block means a
hand-unrolled second bounce inside a kernel that already reads as a monolith. The refactor —
one shared `gather_radiance(ray, depth) -> radiance` the term blocks call — should be a **precondition
of T3-8 and of nothing before it** (recommended in §5). Refactoring earlier is speculative: there is
no second recursive consumer until multi-bounce exists.

---

## 1. T3-6a — RT shadows for every light (not just the sun)

**What it is.** The parent design's own sentence — "RT shadows are sun-only today; every other light
is still a shadow map" — is a complete, small, high-value item on its own, with no ReSTIR in it. The
kernel already casts cone-sampled shadow rays; it casts them once, toward `p.sun_dir`. Making that a
loop over the scene's ≤N lights is the same code with an index.

**Audit.** `ShadowRayParams` carries `sun_dir`/`sun_cone`/`sun_color` as scalars
(`raytrace.rs:1438-1470`) → becomes a small light table, shaped exactly like the `GiMaterial` /
`RtNormalSource` per-object tables the kernel already binds. Output `out_sv.r` is a single
sun-visibility scalar (`raytrace.rs:735`'s doc comment) → becomes per-light visibility. The raster
consumer is `shadow_factor()` (`shaders/render_scene.wgsl:592-601`), which already takes a `slot_f`
per light and currently ignores it entirely on the RT path — **the per-light plumbing exists on the
raster side and is being thrown away.** `shaft_march.wgsl:221` needs the same widening.

**The hard part, and it is real: channel count.** Four lights × visibility does not fit in `out_sv`'s
spare channels. Either the mask texture becomes an array/atlas, or visibility is packed (4 lights ×
8 bits in one channel — cheap, quantised, and shadows are exactly where banding shows), or the pass
runs per light. This is the one genuine decision and it should be made in the design, not the lane.
My lean: a texture array sized to `MAX_SHADOW_CASTING_LIGHTS`, because it costs nothing conceptually
and the existing upsample/à-trous/accumulate chain generalises over array slices without new systems.

**Decisions I can pre-make:** the light table mirrors the existing per-object table pattern; shadow
maps stop rendering for RT scenes entirely (they already do — the widening must not resurrect them
for non-sun lights, which is the obvious wrong turn: a hybrid where the sun is traced and lights 1-3
are mapped is a parallel old path kept alive); the volumetric march consumes the same array, so
`shaft_march.wgsl`'s `is_sun` special-case is *deleted*, not extended.

**Size:** one to two phases. **Stage value: high** — the whole point of RT shadows applied to the
lights a VJ actually animates, plus god-rays that occlude correctly for every light instead of one.
**Blocking question for a team leader:** the channel-layout decision above.

## 2. T3-6b — ReSTIR emissive area sampling

**What it is, once Finding 1 is applied:** not direct-lighting ReSTIR over punctual lights, but
reservoir-based sampling of **emissive geometry as area lights**. The GI gather currently finds
emissive surfaces by cosine-sampling the hemisphere at 2 spp and hoping to hit one
(`raytrace.rs:940-975`) — which is why a small bright emitter is noisy and a large dim one is fine.
ReSTIR replaces hope with importance: build a reservoir per pixel over emissive triangles, reuse it
temporally and spatially.

**Audit.** No light table over emissive geometry exists; `GiMaterial` (`raytrace.rs:1478`) has
per-object emissive but no per-triangle areas or a CDF, and nothing enumerates emissive triangles.
Reservoir buffers, temporal reuse and spatial reuse are all genuinely new — this is the one Tier 3
item with substantial new machinery rather than an extension. It does, however, inherit the reset
plumbing (D15/RT-D2) and the reprojection (T2-C) unchanged.

**Honest cost:** this is the largest item in the tier and the only one where I would expect a
multi-phase wave with its own design doc. Its value is concentrated on exactly one scene class —
several small bright emitters in a dark room — which happens to be the Latent Space aesthetic, so
the value is real but narrow.

**Recommendation: do not start this until T3-6a has shipped and Peter has looked at a scene with 3-4
traced lights.** 6a may absorb most of the perceived need; if it does, 6b's cost/benefit changes
completely. **Revival trigger, stated so it isn't decided by build order:** an emissive-heavy scene
still reads noisy after T3-6a and reflections, with the noise traced to emitter *sampling* rather
than to ray count.

## 3. T3-8 — multi-bounce GI

**What it is.** Today's GI is one bounce with a flat 1/π energy fold (`SUN_BOUNCE_INTENSITY_SCALE`,
`raytrace.rs:965-972`) — light leaves an emitter or the sun, hits one surface, arrives. Multi-bounce
is colour bleeding between surfaces: red wall tinting a white one, a glowing object filling a
concave shell.

**Audit.** The gather loop is a single `for s in 0..gi_spp` with one intersection query and one
sun-visibility ray — no path throughput, no recursion, no Russian roulette. Two shapes: **path
extension** (loop the bounce, carry throughput — small code, linear ray cost, noisy at these budgets)
or **a probe/irradiance cache** (bounded cost, spatial structure, a genuinely new system and a poor
fit for scenes whose geometry moves every frame, which after BUG-320/321/322 is now the normal case
for this instrument).

**My read: path extension, and only after Finding 2's refactor.** A probe cache is the classic
answer and the wrong one here specifically because MANIFOLD's hero objects are animated — probes
would need invalidation on every gesture, which is the same class of problem that cost three attempts
on BUG-322.

**Honest cost, and the reason I rank this last:** at 2 GI spp, a second bounce is *the least visible
ray in the tier*. Second-bounce energy is small, low-frequency, and mostly what the ambient term is
already faking. Adding it doubles GI ray cost for an effect Peter may not be able to point at.
**Blocking question:** whether the ambient knob's flat term should be *removed* when multi-bounce
lands — otherwise the design is paying rays to compute something a knob is already approximating,
and the two will double-count. That is the same trap as the sun (`818a06b0`) and reflections (RD1),
third time.

## 4. T3-9 — RT translucency, and why it splits

**T3-9a — transmission-aware shadow and GI rays (small, and the prettiest thing in the tier).**
Light through petals. Today every ray is binary: `walk_with_alpha_test` (`raytrace.rs:597`) either
passes a candidate or doesn't, per T2-A's alpha cutoff. A transmissive surface — and the material
data is fully present: `KHR_materials_transmission`'s factor plus volume thickness, attenuation
colour and distance, all deserialized and shaded in raster (`shaders/render_scene.wgsl:184-194`,
`:331-332`) — should instead *tint and attenuate* the ray rather than block it. The change is
localized: the shared walk accumulates a transmittance colour instead of returning a bool, and every
ray class inherits it at once, exactly as T2-A's alpha work did. Coloured shadows through a
translucent petal, an emissive shell glowing through its own skin.

**T3-9b — participating media / true subsurface (large, and partly already shipped).**
`VOLUMETRIC_LIGHT_DESIGN.md` is SHIPPED (P1-P3, 2026-07-13) and P3 already gave the march RT
occlusion for the sun (`shaft_march.wgsl:190-193`). What remains is rays that scatter *inside* a
medium rather than being occluded by geometry, plus real subsurface scattering. That is a research-
scale item and should not be scoped further until 9a exists.

**Recommendation: 9a is the second-best value in the tier after 6a**, it reuses T2-A's mechanism
almost exactly, and it is the one item whose result is unmistakably "Latent Space" rather than
"correct renderer". 9b stays a bullet.

## 5. Sequencing — the recommendation, and the constraint nobody owns yet

Dependency-wise, only two edges are hard: T3-8 wants Finding 2's refactor first, and T3-6b should
wait on T3-6a's outcome. Everything else is independent, which means the order is a value judgment,
not a technical one. Mine, cheapest-and-most-visible first:

**T3-6a (multi-light shadows) → T3-7 (reflections, drafted) → T3-9a (transmissive rays) → [Finding 2
refactor] → T3-8 (multi-bounce) → T3-6b (ReSTIR, only if 6a leaves the need).**

Two of those are the small items the one-line roadmap didn't distinguish from their expensive
siblings, which is the main thing this pass bought.

**The constraint nobody owns: the ray budget.** Every item above spends from one pool — currently 4
AO + 2 GI spp at half of a 2/3-scaled render (`render_scene.rs:165/192/203`), on a budget whose
re-judge at the upscaled config is *still the parent design's open item*. Reflections add ~1 primary
+ 1 shadow ray; multi-light shadows multiply the shadow term by light count; multi-bounce doubles GI.
**No item in this tier should be briefed until someone owns a per-term budget allocation**, or the
first two to land will silently spend the whole thing and the third will look like a performance
regression it didn't cause. That allocation is a team-leader deliverable, not a lane's, and the number
it needs comes from Peter's Tier 1+2 look.

## 6. Also open, and on the same board

A team leader planning RT work needs these visible next to the tier — all are parent-design decisions
already made, none are Tier 3, and each has an owner-less status today:

- **P5 export path** (D7/D13) — same pipeline at offline quality, ~10× rays, no denoiser compromise.
  Cut from the v1 wave with its design intact. Arguably higher product value than any Tier 3 item,
  since it is what puts RT frames in a released video rather than only on a stage.
- **P6 frame interpolation** (D6/D8) — Tahoe-gated, per-output, default off for beat-reactive outputs.
  Blocked on a min-OS product decision that is Peter's and deliberately deferred.
- **Deforming-mesh BVH refit** (§3 line item + D17's caveat) — P0 measured refit at **12-16 ms/frame**
  on a 1.43M-tri asset, and D17 records a known one-frame-stale-accel case for a mesh whose vertices
  change without a key change. Any sim/deform work that meets RT trips this. It is the only item on
  this page with a *known* correctness gap rather than a missing feature.

---

**Confidence and its limits.** Findings 1 and 2 are code-anchored and I am confident in them. The
sequencing in §5 is a judgment call about what Peter will value on stage, made without seeing Tier
1+2 running — a team leader who has seen it should overrule me freely. Nothing here is costed in
milliseconds: no item in this tier has a measured ray cost, and the pool they draw from has not been
re-judged since T2-B changed the render scale. Every code claim was verified by reading the file at
the anchor given, on 2026-07-23.
