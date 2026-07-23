# RT Reflections — traced specular for the PBR base lobe

**Status:** PROPOSED — **DRAFT, NOT APPROVED.** Authored by Opus 4.8, 2026-07-23, for review by two
judgment-tier models before anything is briefed. Everything here is pre-decided *so the reviewers only
have to rule on §0*; the rest is legwork they should not have to redo. On approval this doc's body
folds into `RAYTRACING_DESIGN.md` as §9 (Tier 3 item 7) and this file is deleted — named up front
because supersession under a different name is this repo's known killer (CLAUDE.md).
**Prerequisites:** RT Tier 1 + Tier 2 landed (they are, 2026-07-23). **Blocking:** Peter's L2 look on
Tier 1+2 and the ray-budget re-judge at the T2-B upscaled config (`RAYTRACING_DESIGN.md` §3 OPEN) —
no phase here is briefed until that closes, because every ray budget in §6 is priced against it.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The governing insight is that this is a **substitution, not an addition**. `render_scene.wgsl`'s
`fs_pbr` already computes a full split-sum IBL specular term — `prefiltered * (F0 * env_brdf.x +
env_brdf.y)` at `shaders/render_scene.wgsl:1518` — where `prefiltered` is one equirect sample of the
prefiltered environment along `R = reflect(-V, N)`. RT reflections replace **that one sample** with
traced incident radiance along the same `R`. The Fresnel/energy split, the roughness→mip weighting,
the clearcoat and sheen and anisotropic lobes, the transmission path: all untouched. Everything that
makes this look like a large feature — denoising, reprojection, ray budget, material plumbing —
already exists in the RT stack for the diffuse terms, and reflections extend it rather than found it.
That framing is what keeps the design small; a reviewer who finds it growing a second lighting
architecture should reject it on that basis alone.

Companions: `RAYTRACING_DESIGN.md` (parent design — this is its §8 Tier 3 item 7),
`MATERIAL_SYSTEM_DESIGN.md` + `IMPORT_FIDELITY_DESIGN.md` D2/F-P1 (the split-sum IBL being
substituted into), `MANIFOLD_GPU_ARCHITECTURE.md`, `VULKAN_BACKEND_DESIGN.md` (the D9 backend seam
this rides unchanged).

---

## 0. Review queue — what the two reviewers must actually rule on

Five items. Everything else in this doc is decided and anchored; if a reviewer finds themselves
deciding something not on this list, that is a defect in the doc, not in the list.

**Q1 (architectural, blocking R1) — reflection ray direction: interpolated vertex normal, or a
shading normal written by the raster prepass?** The RT kernel today has real interpolated vertex
normals (T1-B) but no normal-map detail; the raster `fs_pbr` shades with the normal-mapped surface
normal. Reflection direction is *far* more sensitive to normal detail than AO or GI are — on a
photoscan whose surface relief lives entirely in its normal map, a vertex-normal reflection is a
mirror where the raster highlight is broken-up. **My recommendation: vertex normals in R1, and treat
the shading-normal G-buffer as an R2 escalation with a named trigger, NOT a planned second path** —
see RD3 and §4's kill-pass for the full price of both. I hold this loosely; it is the one place I
think a stronger reviewer may legitimately overrule me, and the argument against me (design the end
state, don't ship a path you plan to replace — `feedback_dont_design_for_transitional_states`) is a
hard rule in this repo.

**Q2 (borders a hard rule) — the roughness cutoff.** RD7 stops tracing above
`RT_REFLECTION_MAX_ROUGHNESS` and uses the prefiltered env sample there instead. I argue that is a
BRDF-domain split (the env prefilter is a *good* approximation of a wide GGX lobe, and tracing 1 spp
into it is strictly noisier for no gain), not a silent fallback. But
`feedback_no_silent_fallbacks_or_interim_stopgaps` is a hard rule and this is exactly the shape it
forbids. Rule it explicitly so no lane has to.

**Q3 (product, Peter's) — is `rt_reflections` default-ON for RT-enabled scenes?** Recommendation: ON.
RT is days old, one test project exists, and a default-OFF visual feature is the
`feedback_no_conditionally_visible_ui` failure in another costume. Cost: existing RT scenes get more
expensive and look different on next load.

**Q4 (scope) — is R3 (per-texel metallic-roughness maps in the kernel) in this design or Deferred?**
It is what makes reflections correct on photoscans specifically, and D10 pins "plasticy" on roughness
*map* quality — factor-only reflections reflect uniformly across an asset whose roughness varies per
texel. It also forces `MAX_RT_ALPHA_TEXTURES` (currently 4, `raytrace.rs:1558`) to grow into a real
bindless material table. I have written it as a phase; a reviewer may prefer it Deferred with the
trigger already stated in §8.

**Q5 (sequencing, Peter's) — reflections before ReSTIR?** `RAYTRACING_DESIGN.md` §8 calls many-light
sampling "the highest-value one" for MANIFOLD's emissive/strobe-heavy scene class, and RT shadows are
still sun-only. I agree with that ordering and record my dissent from building reflections first —
then defer, because it is a show-need call, not a technical one.

Pre-allocated BUG range for execution: **BUG-323 – BUG-326** (highest in the backlog is BUG-322).

## 1. Audit — what exists (verified 2026-07-23)

| Piece | Where | State |
|---|---|---|
| Split-sum specular IBL, base lobe | `shaders/render_scene.wgsl:1506-1518` (`R = reflect(-V, N)` → equirect `r_uv` → `prefiltered` → `specular_ibl`) | **Exists — this is the single substitution site.** Only `fs_pbr` (:1282) has it; `fs_unlit`/`fs_phong`/`fs_cel` have no specular IBL at all. |
| Four more specular lobes | anisotropic bent-normal (:1537), clearcoat (:1550), sheen (:1568), transmission (:1647) | Exist. All resample `prefiltered_specular`. **Out of v1 scope** (RD5). |
| Prefiltered env mip chain + BRDF LUT + irradiance map | `render_scene.rs:645/647`, built by `run_ibl_convolution` (:1985), 512×256 base, `PREFILTER_MAX_MIP` | Exist as node-owned `GpuTexture`s at the RT dispatch site — **one wire away** from being bound into the trace kernel for ray misses. |
| RT trace kernel (shadow + AO + GI in ONE dispatch) | `raytrace.rs:735` `trace_shadow_rays`; trait `ShadowRayTracer` at :1815 | Exists. D16's seam note is explicit that new ray classes join this dispatch. Already casts a primary ray for the T1-B normal — **the reflection ray's origin and normal are already computed.** |
| Hit shading for a secondary ray | `raytrace.rs:940-975` — GI gather: `hit_emissive + hit_albedo * sun_color * hit_sun_vis * hit_ndotl * SUN_BOUNCE_INTENSITY_SCALE` | **Exists.** A reflection ray's hit shading is literally these lines; RD4 reuses them rather than writing a second shading path. |
| Per-object bindless table | `RtNormalSource` (`raytrace.rs:1533`, 72 B): vertex addr/stride, normal offset+matrix, uv offset, alpha mask/cutoff/tex index | Exists, and has been **extended twice already** (T1-B normals, T2-A UV + alpha texture) — the named precedent for adding material fields. |
| Per-object material table | `GiMaterial` (`raytrace.rs:1478`, 32 B): albedo + emissive only | Exists. Built at `render_scene.rs:3979` from `d.uniforms.base_color`/`.emission`; **`d.uniforms.pbr_metallic_roughness` (`render_scene.rs:332`) is in the same struct, unread** — metallic/roughness is one line away. |
| Bindless texture slots | `MAX_RT_ALPHA_TEXTURES = 4` (`raytrace.rs:1558`) | Exists, capped at 4, doc-comment names its own un-suppression trigger. R3 grows it. |
| Half-res trace → upsample → à-trous → accumulate chain | `render_scene.rs:4057 / 4071 / 4120 / 4175`; `ATROUS_ITERATIONS = 3` (:4095) | Exists, full-res output, ping-pong histories, variance-guided. Reflection radiance rides the same chain shape. |
| Temporal reset | one shared `TemporalResetDetector` (`render_scene.rs:839`), D15/RT-D2 | Exists; **a second reset path is forbidden by the parent design.** |
| Motion reprojection incl. per-object | `accumulate_irradiance` (`raytrace.rs:1206`) + `obj_motion` table (T2-C, §8.3) | Exists. Reflections need one *additional* term (virtual hit point, RD6), not a new mechanism. |
| Numeric region-probe harness | `tests/gpu_proofs/rt_p1_region_probe.rs` — computed (not eyeballed) probe pixels via `Camera::orbit_perspective` + `project_to_pixel`, 15×15 window means | Exists — the gate precedent every phase here copies. |
| RT ray budgets | `AO_SAMPLES_PER_PIXEL = 4` (:165), `GI_SAMPLES_PER_PIXEL = 2` (:192), half-res trace, `RT_TEMPORAL_RENDER_SCALE` 2/3 (:203) | Exist, and their re-judge is the parent design's open item — see the Blocking line in the header. |
| Screen-space reflections | *(negative claim — searched: `rg -in "SSR\|screen.space reflect"` across `crates/` and `docs/`)* | **Do not exist** anywhere in the renderer. Only a passing mention in `RENDERING_INFRA_V2_DESIGN.md:47` as a thing a stored G-buffer would enable. Nothing to migrate, nothing to parallel. |

Extend, don't redesign. Instruction to executor: **every deliverable in §6 is an extra field, an extra
output channel, or an extra ray inside a dispatch that already exists.** A new pass, a new
acceleration structure, a new upsampler, or a second history mechanism means you have left the
design.

**Binding constraints** (DESIGN_AUTHORING §1): *hot path* — yes, per-frame GPU work, and the ray
budget is the whole cost argument; *persistence* — yes, one new serialized scene param
(round-trip gate applies); *performance surface* — yes, `rt_reflections` is a live toggle and an fps
lever, so it is a card param from the first phase, not later. Thread residency and the time model are
untouched: this is entirely inside `render_scene`'s evaluate.

## 2. Decisions

- **RD1 — The reflection term SUBSTITUTES for `prefiltered`, never adds to `specular_ibl`.** The
  traced value is incident radiance along `R` — the same physical quantity `prefiltered` approximates
  — so it is swapped in *before* the `(F0 * env_brdf.x + env_brdf.y)` weighting at
  `render_scene.wgsl:1518`, leaving energy conservation, Fresnel and the roughness LUT exactly as
  they are. **Rejected: adding a traced reflection on top of the existing IBL, because that is
  literally the bug that blew out every sunlit surface on 2026-07-23** — the irradiance kernel
  carried its own sun term on top of the raster light loop's (`818a06b0`, `RAYTRACING_DESIGN.md` §8).
  The same trap, one lobe over. It will look "brighter and better" on the first render, which is why
  it needs a name and a machine check (I-R1).
- **RD2 — Reflection rays join `trace_shadow_rays`; there is no reflection pass.** D16's seam note
  ("P2's soft shadows + AO join the SAME half-res dispatch and SAME upsample — this is the extension
  point, not a new pass") governs here unchanged. The kernel already reconstructs world position,
  casts a primary ray, and has the interpolated normal and the camera position; the reflection ray is
  ~15 lines inside the existing thread. **Rejected: a separate reflection dispatch, because it
  duplicates ray-origin reconstruction and the accel binding for no benefit, and because a second
  dispatch invites a second upsample and a second history — three new systems where the
  zero-new-systems test (DESIGN_AUTHORING §3) allows zero.**
- **RD3 — v1 traces along `reflect(-V, n_vertex)` — the interpolated vertex normal the kernel already
  fetches — NOT the normal-mapped shading normal.** *Consequences, stated honestly:* on any asset
  whose surface detail lives in a normal map, the RT reflection will be smoother and better-behaved
  than the raster specular highlight on the same pixel, and the two will disagree. For a mirror, a
  polished metal prop, a wet floor, or a smooth emissive shell — the things a reflection is actually
  *for* on stage — this is invisible, because those materials have little or no normal-map relief.
  For a normal-mapped photoscan at low roughness it will read as "the reflection is on a different
  surface than the highlight". **Named trigger for the R2 escalation (a shading-normal target on the
  opaque depth prepass): Peter's look reports exactly that mismatch, or the R1 demo shows it.** This
  is Q1; a reviewer may reverse it into R1.
- **RD4 — A reflection ray returns `hit_emissive + hit_albedo * sun * hit_sun_vis * hit_ndotl` on
  hit, and the prefiltered environment sampled at the ray's roughness mip on miss.** The hit branch
  is the GI gather's existing code (`raytrace.rs:955-975`) called with a different direction — same
  one-extra-shadow-ray cost, same `SUN_BOUNCE_INTENSITY_SCALE` discipline, no new shading path. The
  miss branch is what makes RD1 safe: a ray that hits nothing returns *exactly what the raster path
  would have sampled*, so a scene with no reflective occluders renders identically with the feature
  on. *Consequences, stated honestly:* reflections show emissive and diffuse-lit geometry but carry no
  specular highlight of their own (no recursive specular) — a chrome ball reflected in a mirror reads
  matte. One bounce is the parent design's committed scope (D1); multi-bounce is §8's deferred item.
- **RD5 — v1 substitutes the BASE specular lobe of `fs_pbr` only.** Clearcoat, sheen, anisotropic and
  transmission keep their environment resamples; `fs_unlit`/`fs_phong`/`fs_cel` are untouched (they
  have no specular IBL to substitute). *Consequences:* a clearcoated asset shows a traced base
  reflection under an env-only coat reflection; at high `clearcoat` the coat dominates and the
  feature will look weak on exactly those materials. Stated so it is a trade Peter accepted, not a
  bug he finds. The anisotropic branch (:1537) *overwrites* `specular_ibl` wholesale — the executor
  must substitute inside that branch too or anisotropic materials silently lose the feature; this is
  the single easiest thing to get wrong in R1 and is called out in its brief.
- **RD6 — Specular gets its OWN history, reprojected through the virtual hit point, in the SAME
  `accumulate_irradiance` kernel.** Diffuse history is texel-stable under camera motion; a specular
  reflection is not — it moves with the reflected geometry, faster than the surface it sits on. The
  standard fix costs one channel: the trace writes hit distance in `out_refl.a`, and the accumulate
  step reprojects the *virtual* point `world_pos + hit_dist * R` rather than `world_pos`, lerping
  toward plain surface reprojection as roughness rises (`RT_REFL_VIRTUAL_REPROJ_ROUGHNESS_BLEND`,
  named constant with a range). **Rejected: reusing the diffuse irradiance history for the specular
  channel — it ghosts under every camera move, which is BUG-311's exact symptom on a new surface.**
  **Rejected: no accumulation at all** — at 1 spp the raw reflection is unusable, and BUG-312 already
  established that on this stack the accumulation *is* the image quality.
- **RD7 — Above `RT_REFLECTION_MAX_ROUGHNESS` (proposed 0.6, named constant with range) the pixel
  uses the prefiltered environment sample, blended in over a band, and no ray is cast.** Rationale:
  the split-sum prefilter is a genuinely good approximation of a wide GGX lobe; 1 spp into that lobe
  is strictly noisier for no added information, and the cutoff is what bounds both the ray budget and
  the denoiser's job. The band exists so there is no visible seam at the threshold. **This is Q2 —
  the reviewers must confirm it reads as a BRDF-domain split and not as the silent fallback
  `feedback_no_silent_fallbacks_or_interim_stopgaps` forbids.** My argument that it isn't: nothing is
  degraded or hidden — above the cutoff the *correct* approximation is the cheap one, the transition
  is continuous by construction, and the constant is visible in the doc and the code.
- **RD8 — 1 reflection ray per pixel at the existing trace resolution.** Half-res of render res, which
  under T2-B's temporal mode is ~1/3 native. No separate reflection resolution knob. *Consequences:*
  a mirror reflection is reconstructed from a 1/3-res signal — the sharpest possible content on the
  cheapest possible sampling, and the place this design is most likely to disappoint. The measured
  answer comes from R1's reported `trace_ms` delta and Peter's look; a reflection-specific resolution
  is Deferred (§8) with that as its trigger, rather than guessed at now.
- **RD9 — One new scene param, `rt_reflections: Bool`, serialized alongside `rt_enabled` and inert
  when `rt_enabled` is false.** Shaped exactly like `rt_enabled`'s existing path through the scene
  def + EditingService (P1's precedent). Default per Q3.
- **RD10 — The Metal RT trait grows no new method.** `dispatch_shadow_rays` gains an `out_refl`
  texture argument and `ShadowRayParams` gains the reflection fields; `upsample_shadow`,
  `atrous_pass` and `accumulate_irradiance` each gain the reflection texture set alongside the
  irradiance set they already carry. This keeps D9's Vulkan seam a matter of ray-query translation
  rather than a new capability, and it keeps the "one dispatch" claim honest in the trait, not just
  in prose.

## 3. Architecture

**Data model — committed shapes.** Two struct extensions and one texture, all following the
extend-the-existing-table precedent set twice by T1-B and T2-A:

```rust
// crates/manifold-gpu/src/metal/raytrace.rs — GiMaterial grows from 32 to 48 bytes.
// Field order and packing MUST match the MSL mirror exactly (P0 §5.1's packed_float3 lesson).
#[repr(C)]
pub struct GiMaterial {
    pub albedo:    [f32; 3], _pad0: f32,
    pub emissive:  [f32; 3], _pad1: f32,
    /// RT-R1: x = metallic, y = roughness — read straight off
    /// `d.uniforms.pbr_metallic_roughness` (render_scene.rs:332), the SAME
    /// resolved factors `fs_pbr` shades with. z/w reserved.
    pub metallic_roughness: [f32; 4],
}
const _: () = assert!(std::mem::size_of::<GiMaterial>() == 48);
```

```rust
// ShadowRayParams gains, before the mat4 block (keep the offset asserts green):
    pub refl_spp:            u32,   // 1 in v1 (RD8); 0 disables the whole reflection branch
    pub refl_max_roughness:  f32,   // RT_REFLECTION_MAX_ROUGHNESS (RD7)
    pub refl_rough_band:     f32,   // blend band width
    pub _pad_refl:           u32,
```

New output texture `out_refl` (`Rgba16Float`, trace resolution): `.rgb` = incident radiance along
`R`, `.a` = hit distance (a clamped sentinel on miss, so RD6's virtual reprojection degenerates to
surface reprojection there). It rides every stage the irradiance texture already rides — half-res
target, upsample, à-trous, accumulate ping-pong pair — allocated and reset by the same
`ensure_rt_irradiance` lifecycle (`render_scene.rs:774` and its neighbours), including the
"RESET, not resized-and-forgotten" rule that function's doc comment already states.

**Kernel flow, inside the existing thread of `trace_shadow_rays`** (`raytrace.rs:735`), after the
existing shadow/AO/GI blocks, reusing their `origin`, `n`, `bias_eps` and `obj_id`:

1. Fetch this surface's `metallic`/`roughness` from `gi_materials[obj_id]`. If `roughness >
   refl_max_roughness + band`, write the env sample and return — no ray (RD7).
2. `V = normalize(p.camera_pos - wp)`; `R = reflect(-V, n)`; for `roughness > 0`, perturb `R` by a
   GGX-importance-sampled half-vector using the SAME `blue_noise_sample` sequence T1-D introduced
   (`raytrace.rs:694`) — a new sampling function, not a new sampling *system*.
3. One `walk_with_alpha_test` intersection query (`raytrace.rs:597`) — alpha-aware for free, since
   T2-A made that the shared walk for every ray class.
4. Hit → the GI gather's shading lines (RD4). Miss → `prefiltered_specular` sampled with the same
   equirect mapping `render_scene.wgsl:1506-1510` uses, at mip `roughness * PREFILTER_MAX_MIP`, so
   the miss branch is numerically the raster path.
5. Write `float4(radiance, hit_dist)`.

**The WGSL substitution** (`render_scene.wgsl`, `fs_pbr` only):

```wgsl
// binding 43, full-res, always bound (ABI-stub discipline — a 1x1 dummy when RT
// reflections are off this frame), exactly like rt_irradiance_mask at :352.
@group(0) @binding(43) var rt_reflection: texture_2d<f32>;

// at :1510, replacing the single `prefiltered` fetch:
var prefiltered = textureSampleLevel(prefiltered_specular, envmap_sampler, r_uv,
                                     roughness * PREFILTER_MAX_MIP).rgb;
if u.scene_params.w > 0.5 && u.rt_flags.x > 0.5 {          // rt_enabled && rt_reflections
    prefiltered = textureLoad(rt_reflection, vec2<i32>(in.clip_pos.xy), 0).rgb;
}
```

Everything downstream — `specular_ibl`, the anisotropic overwrite at :1542 (which must consume the
same substituted value), `ibl`, `base_rgb` at :1611 — is unchanged. **That two-line diff is the
entire raster-side change, and if a phase's diff to this file is larger than this, the phase has
gone wrong.**

**Stage translation.** What this buys on stage: a glowing hero object shows up *in* the surfaces
around it — the wet-floor, dark-mirror, black-acrylic look that reads as production value under
club lighting, and which env-map IBL structurally cannot give you because the env map does not
contain your content. Under a strobe it behaves correctly by construction (the reflection is
recomputed per frame; the demodulated-history discipline that protects strobes for diffuse applies
unchanged). What it costs: rays, on a budget Peter has not yet re-judged — hence the blocking line
in the header. What it looks like when it breaks: reflection noise crawling on a shiny surface
during fast camera moves, which is BUG-312's symptom class one lobe over, and RD6 is the mechanism
that is supposed to prevent it.

## 4. Alternatives — priced, and the favourite kill-passed

**Shape B — a separate reflection pass driven by a raster-written material G-buffer.** The opaque
depth prepass grows a target carrying shading normal + metallic/roughness *after* normal and MR maps
are applied; a reflection dispatch reads it and traces. *Implementation cost:* higher — a new render
target, a prepass fragment-shader output, plus the dispatch. *Hot-path cost:* one more full-res
target's bandwidth every frame, against Shape A's zero. *Migration cost:* nil for both. *What it
forecloses / buys:* it is the only shape that gets **normal-mapped and per-texel-roughness
reflections right for free**, because it consumes material data after the raster path has already
resolved it — no bindless texture table, no `MAX_RT_ALPHA_TEXTURES` cap, and R3 disappears entirely.
Against it: it puts material resolution in two places (the raster path and the RT table would both
own "what is this surface's roughness"), which is the translation-layer smell the zero-new-systems
test exists to catch — and it contradicts D16's committed one-dispatch seam, which the parent design
has now extended four times without breaking.

**Chosen: Shape A** (RD2) — but Shape B's argument is genuinely strong, and it is the reason Q1 is a
review item rather than a decision. Note the asymmetry a reviewer should weigh: Shape A can *grow
into* Shape B's normal handling later (RD3's named trigger) at the cost of one prepass target;
Shape B cannot shrink back.

**Kill-pass on Shape A** (DESIGN_AUTHORING §4 — state the strongest case my favourite is wrong):
Shape A is wrong if reflection quality on Peter's actual assets is dominated by normal-map detail
rather than by geometry. Every hero asset is a photoscan, and a delit photoscan's micro-relief lives
in its normal map. If that is the dominant term, R1 ships a reflection that is visibly "wrong
surface" on exactly the content the instrument exists for, and R2's escalation becomes mandatory —
i.e. I will have shipped a path I planned to replace, which is the transitional-state anti-pattern
under a different label. **The cheapest test that would settle it before committing R1: render one
hero scan twice through the existing headless path — once with its normal map wired, once unwired —
with a low-roughness metallic material, and diff the specular region. If the two differ strongly,
Shape A's reflection direction is being computed from the wrong normal and Q1 should resolve toward
the prepass target.** That test is roughly half an hour with `graph-tool render` and the
`RasterCompare.json` preset (`tools/rt_prototype/compare/`); **I have not run it**, and a reviewer
should weigh Q1 knowing that the empirical answer is cheaply available and currently absent.

**Rejected outright: screen-space reflections as a cheap first step.** SSR does not exist in this
renderer (audit, negative claim verified), and adding it would create a second reflection path that
then needs blending with the RT one — a parallel old path kept alive, forbidden by name in the
standard's failure catalog, and pointless when the hardware RT stack is already resident.

**Rejected: reflections as a graph node/atom.** RT lives in the REALTIME_3D scene pass and outputs
into the graph like the scene render does — `RAYTRACING_DESIGN.md` §6.6, decided, do not reopen.

## 5. Invariants & enforcement

- **I-R1 — Exactly one environment-specular contribution per lobe per pixel.** *Enforcement:*
  `rt_r1_reflection.rs::reflection_of_empty_scene_equals_env_only` — a scene containing only the
  reflective surface (no occluders, uniform-radiance envmap): with reflections ON, the rendered
  region mean must equal the reflections-OFF render within a stated epsilon. If the term is being
  added rather than substituted, this fails loudly and immediately. This is the machine check that
  the `818a06b0` class cannot recur.
- **I-R2 — One temporal-reset path for the whole RT node.** *Enforcement:* negative `rg` for a second
  `TemporalResetDetector` construction in `render_scene.rs` (zero hits beyond the existing one), per
  D15/RT-D2 and the precedent gate P2/P4 already run.
- **I-R3 — The reflection texture is consumed in exactly one place.** *Enforcement:* `rg -c
  "rt_reflection" shaders/render_scene.wgsl` — the declaration plus exactly one `textureLoad`. Stops
  the term leaking into the clearcoat/sheen lobes RD5 excludes.
- **I-R4 — No reflection work on the non-RT path.** *Enforcement:* negative `rg` for reflection
  dispatch outside the `rt_ready` block, plus the existing native-mode machine-diff gate (T2-B's
  precedent: a real `graph-tool render` byte comparison at pre/post commits, not a code-diff
  argument).
- **I-R5 — No Apple types above `manifold-gpu`.** *Enforcement:* the parent design's standing
  negative `rg` (`objc2|MTL`, zero hits outside `manifold-gpu`) at every phase gate.

## 6. Phasing

Three phases, each one session, each committable. **R1 is the vertical slice** — model param →
serialized → dispatch → kernel → WGSL → pixels, exercised once end to end before anything is refined.

### R1 — traced base-lobe reflection, factors only, no accumulation

- *Entry:* Tier 2 landed on main (`git merge-base --is-ancestor` on the T2-B tip); the parent
  design's blocking L2/budget item closed; re-verify `raytrace.rs:1478` (`GiMaterial` still 32 B) and
  `render_scene.wgsl:1518` (`specular_ibl` still assembled there) — a moved anchor is an escalation,
  not a guess.
- *Read-back:* this doc §2–§3 whole; `RAYTRACING_DESIGN.md` D16 + §8.3's three method lessons;
  `raytrace.rs:735-990` (the whole trace kernel); `render_scene.wgsl:1494-1620`.
- *Deliverables:* `GiMaterial` +`metallic_roughness`, populated at `render_scene.rs:3979`;
  `ShadowRayParams` reflection fields; `out_refl` texture through dispatch + upsample + à-trous
  (accumulate carries it untouched this phase); the kernel reflection block (RD4/RD7); the WGSL
  substitution **including inside the anisotropic branch at :1542**; `rt_reflections` scene param
  end-to-end with serialization; the I-R1 and I-R3 checks by name.
- *Gate:* (a) **mirror probe** — new `tests/gpu_proofs/rt_r1_reflection.rs`, built on
  `rt_p1_region_probe.rs`'s computed-pixel harness: a metallic/roughness-0 ground plane, one emissive
  quad at known world coordinates, envmap unwired; CPU computes the emitter's mirror image across the
  plane and projects it with the same `Camera::orbit_perspective` + `project_to_pixel` math; the
  15×15 region mean at that pixel must exceed a stated threshold derived from the emissive value.
  **Control leg, mandatory:** identical fixture with `rt_reflections` false must read below a stated
  floor there. The control leg is what makes this a proof of the reflection term rather than of
  rendering in general — §8.3's discipline, learned at the cost of a wave. (b) I-R1's empty-scene
  equality test. (c) round-trip: save → reload → probe still passes. (d) `MANIFOLD_RENDER_TRACE=1`,
  no frame > 20 ms, **and report the measured `trace_ms` delta with reflections on vs off** — a
  number, in the phase report. (e) negative `rg`: I-R2, I-R3, I-R4, I-R5. (f) `cargo test -p
  manifold-renderer --features gpu-proofs` (GPU path touched — mandatory, and `cargo test`, never
  nextest).
- *Performer gesture:* toggle `rt_reflections` on a playing scene mid-set — the frame trace shows no
  frame > 20 ms across the toggle (the pipeline is already resident; nothing rebuilds).
- *Forbidden moves:* adding to `specular_ibl` instead of substituting (RD1 — the `818a06b0` trap);
  a second dispatch or a reflection-specific upsampler; touching the clearcoat/sheen/transmission
  lobes; a second `TemporalResetDetector`; widening `MAX_RT_ALPHA_TEXTURES` (that's R3); "temporarily"
  hard-coding roughness; claiming the native-mode-unchanged property from a code-diff argument
  instead of a machine diff.
- *Demo (Peter only):* reflections-on vs reflections-off PNG pair on a mirror-plane scene and on a
  real hero scan — **L2. Peter's look also answers RD3's trigger question** (does the reflection sit
  on a different surface than the highlight?), which is why the hero-scan frame is not optional.
- *Test scope:* `-p manifold-renderer -p manifold-gpu` + the gpu-proofs run. Clippy `-p` on both.

### R2 — specular temporal accumulation + roughness-aware filtering

- *Entry:* R1 landed; R1's `trace_ms` delta and Peter's L2 verdict both recorded in the phase report.
- *Read-back:* RD6; `accumulate_irradiance` (`raytrace.rs:1206`) and `atrous_filter` (:1107) whole;
  `RAYTRACING_DESIGN.md` §8.3 (per-object motion) and D19/D20 (why numeric motion oracles failed).
- *Deliverables:* a specular history ping-pong set alongside the irradiance set, wired to the SAME
  reset detector; virtual-hit-point reprojection with the roughness blend (RD6) inside the existing
  accumulate kernel; à-trous edge-stopping weights that narrow with roughness so a sharp reflection
  is not blurred into the diffuse signal; all constants named with ranges, untuned — tuning is
  Peter's look, not the lane's.
- *Gate:* **control-leg value test** in the §8.3 shape — a fixture where the camera moves and the
  reflected geometry does not: WITH virtual-hit reprojection the accumulated value matches the
  CPU-computed `1 - alpha` blend; WITHOUT it (the term compiled out, literally the R1 behaviour) the
  history is rejected and the value collapses. Two legs, one file. Plus the P2 cut-reset numeric
  oracle applied to the specular history; plus I-R2's negative `rg`; plus the gpu-proofs run.
- *Performer gesture:* a fast camera sweep across a mirror surface mid-clip — the gate captures the
  frame sequence; **the quality verdict is Peter's look, per D19/D20's standing lesson that no
  numeric metric on this surface separates ghosting from legitimate accumulation lag.** A lane that
  proposes a third oracle redesign here stops and escalates instead.
- *Escalation (RD3's trigger):* if R1's demo or Peter's look reported the normal-map mismatch, this
  phase is where the shading-normal prepass target is built — **and that is a re-brief by the
  reviewers, not a lane improvising it**, because it changes the prepass's render-target shape.
- *Forbidden moves:* reusing the diffuse history for specular; a second reset path; a third motion
  oracle redesign; re-tuning ray budgets inside this phase.

### R3 — per-texel metallic-roughness in the kernel *(scope question Q4)*

- *Entry:* R2 landed.
- *Read-back:* T2-A's commit `62244989` (the bindless-texture extension precedent, whole);
  `RtNormalSource` + `ensure_normal_sources` (`raytrace.rs:1533/1591`); D10.
- *Deliverables:* `RtNormalSource` grows an MR-texture index (same field pattern as
  `alpha_tex_index`); `MAX_RT_ALPHA_TEXTURES` becomes a general bindless material-texture cap with a
  stated new value and its own un-suppression trigger in the doc comment; the kernel samples
  metallic/roughness per texel at the primary hit's interpolated UV (`fetch_interpolated_uv`,
  :556 — already exists), using the object's factors when no map is bound.
- *Gate:* value test — a plane with a two-region roughness map (0.0 / 1.0) and one emissive quad: the
  sharp region's probe shows the emitter's mirror image above threshold, the rough region's does not,
  both region means compared against CPU-computed expectations; held-out input: a real imported glTF
  asset with an MR map that the builder did not develop against. Plus gpu-proofs.
- *Forbidden moves:* growing the texture cap without stating the new limit's trigger; sampling MR
  maps for secondary (GI/AO) rays in the same phase (scope fence — reflections only).

**Phasing-completeness check:** every affordance this doc's body commits to appears above or in §8 —
the `rt_reflections` toggle (R1), the traced base lobe (R1), the roughness cutoff (R1), accumulation
and denoising (R2), per-texel roughness (R3). The four other specular lobes, reflection-specific
resolution, multi-bounce, and the shading-normal prepass are in §8 with triggers.

## 7. Decided — do not reopen

1. Substitution into `prefiltered`, never addition to `specular_ibl` (RD1).
2. Reflection rays join the existing trace dispatch; no reflection pass, no second accel (RD2).
3. Hit shading reuses the GI gather's terms; miss returns the prefiltered env (RD4).
4. Base PBR lobe only in v1; other lobes and non-PBR fragment paths untouched (RD5).
5. Specular gets its own history in the existing accumulate kernel; never the diffuse history (RD6).
6. One shared `TemporalResetDetector` — D15/RT-D2, inherited, absolute.
7. No SSR, in any tier of this design.
8. Reflections stay inside the scene pass and output through it — parent design §6.6.

## 8. Deferred (with revival triggers)

- **Shading-normal prepass target** — trigger: RD3's named mismatch appears in R1's demo or Peter's
  look. Reviewers may promote it into R1 via Q1.
- **Clearcoat / sheen / anisotropic / transmission traced lobes** — trigger: a show asset whose look
  is dominated by a coat reflection, plus spare measured ray budget.
- **Reflection-specific trace resolution** — trigger: R1's `trace_ms` delta shows headroom, or Peter
  reports mirror reflections reading soft at 1/3-res reconstruction (RD8's honest cost landing).
- **Multi-bounce / recursive specular** — trigger: none before the parent design's Tier 3 item 8.
- **Reflections on non-PBR fragment paths** — trigger: a scene needing a reflective cel/phong
  material; currently incoherent (those shaders have no Fresnel term to weight against).
- **ReSTIR many-light before or after this** — Q5; Peter's show-need call, recorded here so it is not
  silently decided by build order.

---

**Draft honesty line.** Unverified in this doc: every performance claim (no ray-cost number exists
for reflections on this stack — R1's gate produces the first one); the Shape-A-vs-B normal question
(the cheap settling test is named in §4 and was not run); and `RT_REFLECTION_MAX_ROUGHNESS = 0.6`,
which is a plausible starting constant, not a measured one. Every code claim in §1 was verified by
reading the file at the anchor given, on 2026-07-23.

**The diffuse side was checked for the same trap and is clean — recorded so no reviewer re-derives
it.** `render_scene.wgsl` adds `ambient` (`rt_or_flat_ambient`, :1589) to `diffuse_ibl` (:1585) at
:1611, which *looks* like the RT irradiance being stacked on the envmap's diffuse IBL. It isn't: the
RT term substitutes for the **flat ambient knob** (`scene_params.y * ambient_tint` — the same
quantity, now AO-occluded, plus the GI gather), while `diffuse_ibl` is the envmap's cosine-convolved
irradiance, present identically on both paths. Same substitution discipline RD1 demands, already
correct one lobe over. Note the asymmetry that makes specular riskier: there is no "flat specular"
term for RT to replace, so the only correct substitution target is `prefiltered` itself — which is
exactly why an executor's instinct will be to add instead.
