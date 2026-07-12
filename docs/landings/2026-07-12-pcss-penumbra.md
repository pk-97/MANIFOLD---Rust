# Realtime 3D — P9 (PCSS contact-hardening penumbra) landing

**Branch:** `feat/pcss-penumbra` · **Level reached:** L1 / target L1 (§11's gate is numeric —
gradient-width comparisons via GPU readback, no PNG, no image judgment; the brief explicitly
forbids PNGs for this phase).
**Doc status line (quoted verbatim):** `Status: IN PROGRESS (status corrected + baseline-reviewed 2026-07-05; D3/D8 AMENDED 2026-07-06 by SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md — read its §8 before P6; D3/D4/§3/§6/§7.3 AMENDED 2026-07-10 (F2 coherence audit) — shadow-caster cap MAX_SHADOW_CASTING_LIGHTS = 4 replaces the dead "8 objects, 4 lights" budget, read D4 before P2). Shipped: P0 (MATERIAL M1–M6, all verified in-tree), P1 node.render_scene @ 8daa89fc, P4 camera atoms (both node.free_camera + node.look_at_camera in-tree), §9 node.spawn_from_mesh, P2 shadow maps + P3 atmosphere/fog @ feat/realtime3d-p2p3 2026-07-11 (gpu-proofs render_scene_shadows + render_scene_fog, PNG-verified; lights also moved to a ring-buffered storage buffer), P8 scene instancing @ feat/realtime3d-p8-instancing 2026-07-11 (§10 D11 — each object group grows an optional instances_n: Array(InstanceTransform) port; wired draws instance_count = buffer_size / 32 copies with model_n · T_instance in both the main pass and every caster's shadow pass; unwired binds a cached 1-entry identity stub, byte-identical to pre-P8 output; gpu-proofs render_scene_instances 4/4 green — identity parity, occlusion, instanced-shadow, instanced-fog; Garden.json re-wired single-pass, the node.mix Max-blend composite deleted), P9 PCSS contact-hardening penumbra @ feat/pcss-penumbra 2026-07-12 (§11 D12 — ShadowSoftness::Contact { light_size }, 16-tap golden-angle blocker search + standard-PCSS penumbra estimate feeding the existing PCF loop with a dynamic half-width; light_size's world-units→UV-space conversion derived per-fragment from the caster's own vp matrix, not a new caster-table field — zero layout growth as D12 required; gpu-proofs render_scene_pcss 3/3 green — contact-hardening gradient-width ratio, light_size=0 byte-matches Hard tier, existing tiers unperturbed; render_scene_shadows proof unmodified and green). The P1 "transforms not port-shadowed" deviation is retired by amendment, not by shadows: per-object transforms move to node.transform_3d atoms feeding transform_n: Transform ports (SCENE_BUILD P2). Remaining: P5 viewport navigate, P6 gizmos, P7 scene starter preset. · designed 2026-07-03 · Fable`

## What shipped

**On stage:** a fourth shadow-softness tier, `Contact`, on `node.light`. Where the fixed
Hard/Soft/VerySoft tiers spread a shadow's edge by a constant blur regardless of how far the
caster sits above the receiver, Contact hardens the edge where an object touches its own
shadow and softens it as the object lifts away — the look Peter asked for: *"a statue's arm
onto its torso, leaves onto stems is exactly what I want."* `light_size` — a new port-shadowed
scalar, so it's a fader like every other light param — sizes the effect: small values stay
close to Hard's crisp edge everywhere, larger values read as an overcast, big-source look.

- **`ShadowSoftness::Contact { light_size: f32 }`** (`light.rs`) — additive enum growth, old
  saves load unchanged. `kernel_half_width()` returns a `-1` sentinel for `Contact` (consumed
  only by the caster-table build, never compared as a real width elsewhere).
- **`node.light`** gained a 4th `shadow_softness` enum option ("Contact") and a port-shadowed
  `light_size` scalar (default 1.0, range 0–20, tooltip added).
- **`render_scene.rs`'s caster-table build** writes the sentinel + `light_size` into the
  previously-always-zero 4th component of the caster's params vec4 — the exact "spare `.w`"
  D12 named, no new binding, no ABI growth.
- **`render_scene.wgsl`** — `shadow_factor` now dispatches on that sentinel to
  `pcss_shadow_factor`: 16 golden-angle-spiral taps (the CINEMATIC_POST D2 formula,
  `r_i = sqrt((i+0.5)/16)`, `θ_i = i·2.399963`) read PLAIN depth via `textureLoad` on the
  existing `texture_depth_2d` bindings (no second binding needed — the VERIFY-AT-IMPL in §11
  resolves "yes" through the existing binding, not the doc's ABI-addition fallback), average
  the blockers, compute the penumbra width, and hand a dynamic half-width to the SAME
  `pcf_average` loop the fixed tiers use (extracted as a shared function, byte-identical to
  the old inline loop).

## Gate results (verbatim)

**gpu-proofs, full suite (`cargo test -p manifold-renderer --test gpu_proofs --features gpu-proofs`, 17/17 green):**
```
test render_scene_pcss::contact_hardening_gradient_narrows_at_contact ... ok
test render_scene_pcss::contact_tier_with_zero_light_size_matches_hard_tier ... ok
test render_scene_pcss::existing_softness_tiers_are_unaffected_by_contact ... ok
test render_scene_shadows::occluder_casts_shadow_that_darkens_the_ground ... ok
test render_scene_shadows::more_than_k_casters_still_render_finite_and_lit ... ok
... (all render_scene_lights/fog/instances, alpha_contract, film_grain, smoke) ...
test result: ok. 17 passed; 0 failed; 0 ignored
```
- `contact_hardening_gradient_narrows_at_contact` — occluder at height 0.3 ("contact") vs 4.0
  ("far"), same near-orthographic camera + Sun: gradient width (pixels with
  `0.05 < shade < 0.95` down a fixed column) is 2px at contact vs 10px far — 5x, clearing the
  3x gate with margin.
- `contact_tier_with_zero_light_size_matches_hard_tier` — `Contact { light_size: 0 }` vs
  `Hard`, same column: both 2px, exact match (the code path is literally the same
  `pcf_average(khw=1)` call, not just numerically close).
- `existing_softness_tiers_are_unaffected_by_contact` — Soft tier still shows a measurable
  penumbra (3px) at height 4.0, proving Contact's addition didn't perturb the fixed tiers.
- `render_scene_shadows::occluder_casts_shadow_that_darkens_the_ground` — luma drop 3.5%,
  **byte-identical to the pre-change number** (verified by re-running before and after the
  fix) — the hard gate that the fixed-kernel PCF path is untouched.

**Focused (`cargo nextest run -p manifold-renderer --lib`): 1090 passed, 0 failed, 3 skipped.**

**`cargo clippy -p manifold-renderer -- -D warnings`:** clean.

## Deviations from brief

Both are interior-mechanism choices the brief left open, not architecture changes — see §11's
"As-built deviations" note in the design doc for the fuller writeup:

1. **`light_size`'s world→UV conversion is derived per-fragment via the caster's `vp`
   matrix**, not read as a raw UV-space number. First implementation attempt read `light_size`
   directly as a UV offset (following the literal text of D12's formula) — at any
   performer-sensible value (e.g. 1.5) this put the blocker-search taps 2-3 orders of
   magnitude outside `[0,1]` UV space, every tap clamped to the texture edge, zero blockers
   ever found, and the whole Contact tier silently rendered as "always fully lit" — no error,
   no crash, just wrong. Caught by running the gpu-proof and observing `ndiff=0` between
   `cast_shadows` on and off (the oracle discipline CLAUDE.md names: run it, don't derive it).
   Fixed by projecting `world_pos` offset by `light_size` along world X and world Z through
   the caster's own `vp` (the same transform `shadow_factor` already runs for the primary
   sample) and taking the larger resulting UV displacement — exact for Sun's uniform-scale
   ortho frustum, a reasonable per-point approximation for Point's perspective one. Needs no
   new caster-table field; D12's "zero layout growth" still holds.
2. **`Contact` with `light_size = 0` calls `pcf_average(khw=1)` directly** (Hard tier's exact
   half-width) instead of running the 16-tap blocker search with a zero radius. Gate (b) asked
   for "within 1px of Hard tier"; this makes it the literal same function call, 0px difference,
   not just close.

Also: **the design doc's own §11 audit named the shadow-softness enum "Off/Soft/Softer/
Softest"** — the shipped enum (since P2) is `Hard`/`Soft`/`VerySoft`, three variants not four,
no "Off". This is doc-drift in the addendum's own prose (verified against `light.rs` before
writing any code — DESIGN_DOC_STANDARD §3's "anchors are re-verified at execution time, not
trusted"), not a functional gap. Gate (b)'s "Off-tier hard edge" is read as "the sharpest
existing tier" (`Hard`), which is what deviation 2 above makes exact. Corrected in the design
doc's Status line for future readers.

## Shortcuts confessed (rolled up from phase reports)

None shipped. One caught-before-landing mistake worth naming since it's the session's real
story: the scene builder's first camera (wide-FOV, oblique, matching `render_scene_shadows`'s
proven framing) put the shadow's screen position at a depth that shifted with occluder height,
which compressed the umbra's apparent pixel width as height grew — fighting the very widening
PCSS produces, and initially making a taller (should-be-softer) shadow measure NARROWER than a
shorter one. Diagnosed by dumping per-row/per-column shade profiles (again: run it, read the
numbers) rather than guessing at a fix; resolved by switching to a near-orthographic camera
(narrow `fov_y`, long `distance`) so a fixed scanline's world-units-per-pixel scale stays
close to constant across the tested height range. Not shipped as a gap — the finalized test
in the branch is the corrected version.

## Verification debt

None opened, none carried.

## Click-script for Peter (≤2 minutes)

1. Open the graph editor on any scene with a `node.light` feeding a `node.render_scene`, set
   `cast_shadows` on, `shadow_softness` to "Contact" — expect: a `light_size` slider appears
   (default 1.0).
2. Put a small mesh close to a receiving surface (nearly touching) and sweep `light_size` from
   0 upward — expect: the shadow edge stays sharp near the contact point across the whole
   sweep (that's the point — contact stays hard).
3. Lift the same mesh well above the surface (a few world units) and repeat the sweep —
   expect: the edge visibly softens as `light_size` increases, more than it did at contact
   height in step 2.
4. `cargo test -p manifold-renderer --test gpu_proofs --features gpu-proofs render_scene` —
   expect: 17/17 green, including the 3 new `render_scene_pcss` tests.
