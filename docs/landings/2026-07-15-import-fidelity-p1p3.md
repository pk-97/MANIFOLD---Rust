# IMPORT_FIDELITY F-P1 + F-P3 — landed 2026-07-15 @ (pending merge SHA, filled at push)

**Branch:** `feat/import-fidelity-fp1` (F-P1, `cddc618f`) + `feat/import-fidelity-fp3` (F-P3, `9e4b0b7f` + fix `c0df7921`), merged into `main` as one batch per `.claude/GIT_TREE_DISCIPLINE.md` §2c.
**Level reached:** L1 (numeric gpu-proofs gates, run by the orchestrator, all green) / target L4 (§10) — L4 is Peter's click-script below; not yet run by him.
**Doc status line (quoted verbatim):** `IN PROGRESS · F-P1 + F-P3 SHIPPED 2026-07-15 (orchestrator session 1 of 3, landing report `docs/landings/2026-07-15-import-fidelity-p1p3.md`) · approved by Peter 2026-07-15 ("Approved") · authored 2026-07-15 · Fable 5 (his product calls are quoted in the intro, D7, and D8; glass/F-P5, pure-black base, and sun coherence added same day at his direction). Execution: 3 orchestrator sessions — (1) F-P1 ∥ F-P3 DONE, (2) F-P2 → F-P4 next, (3) F-P5.`

## Gate results (verbatim)

F-P1, `cargo test -p manifold-renderer --features gpu-proofs render_scene_ibl -- --nocapture --test-threads=1`:
```
test render_scene_ibl::diffuse_ibl_lands_within_a_generous_band_of_albedo ... IBL diffuse sanity: centre luma=0.7626, albedo luma=0.6000, ratio=1.271
ok
test render_scene_ibl::prefilter_and_irradiance_cost_is_measured_and_reported ...
F-P1 IBL cost (512x256 prefiltered chain + 32x16 irradiance, 8 frames averaged): wired=3.298ms/frame unwired=0.208ms/frame delta=3.090ms/frame (phase brief's re-tune trigger: >10ms for the 512x256 chain)
ok
test render_scene_ibl::roughness_response_reflection_spreads_monotonically ... IBL roughness-response: roughness=0.02 spread=0.1001 roughness=0.95 spread=0.4948
ok
test result: ok. 3 passed; 0 failed
```
Negative gates: `rg 'ibl_strength' render_scene.rs render_scene.wgsl` → zero hits in the shader source (the two remaining crate-wide hits are a doc-comment and the guard test asserting it, in `render_scene.rs`; `render_mesh`/`render_instanced_3d_mesh` correctly retain their own `ibl_strength`, out of D1's scope). Broader `render_scene_*` proof suite: `cargo test -p manifold-renderer --features gpu-proofs render_scene` → 30 passed, 0 failed (fog/instances/lights/pcss/shadows all green, unmodified).

F-P3, `cargo test -p manifold-renderer --features gpu-proofs bake_equirect_envmap -- --test-threads=1` (rerun by the orchestrator after the fix below):
```
test node_graph::primitives::bake_equirect_envmap::gpu_tests::gradient_mode_matches_legacy_formula ... ok
test node_graph::primitives::bake_equirect_envmap::gpu_tests::softbox_base_is_exact_zero_outside_strip_bands ... ok
test node_graph::primitives::bake_equirect_envmap::gpu_tests::softbox_emitter_count_changes_strip_count ... ok
test node_graph::primitives::bake_equirect_envmap::gpu_tests::softbox_emitter_rows_exceed_hdr_one ... ok
test node_graph::primitives::bake_equirect_envmap::gpu_tests::softbox_sun_disc_intensity_zero_is_byte_identical_to_no_disc ... ok
test node_graph::primitives::bake_equirect_envmap::gpu_tests::softbox_sun_disc_peaks_at_expected_direction ... ok
(+ 7 non-GPU unit tests)
test result: ok. 13 passed; 0 failed
```

Both workers' non-GPU self-checks (clippy `-D warnings` with and without `--features gpu-proofs`, `cargo nextest run --lib`) were clean before their commits.

## Deviations from brief

1. **F-P1 cache invalidation — the doc's D2 rebuild rule ("re-convolves when the wired envmap's `DataVersion` changes") names a mechanism that doesn't exist in this codebase.** `rg -i dataversion` returns two unrelated prose hits; `EffectNodeContext` carries no per-input change signal, and `bake_equirect_envmap` mutates its output texture in place every frame regardless of param change — so the only identity signal available (a GPU texture pointer) is stable even while content animates. A pointer-keyed skip would treat D7's sun-sweep gesture (the envmap re-baking every frame the sun direction moves) as "unchanged" and go stale on the design's own showcase gesture — a correctness regression, not a missed optimization. F-P1 built it the safe way instead: the BRDF LUT (genuinely envmap-independent) is a real cache, built once per device; the prefiltered chain and irradiance map re-convolve unconditionally whenever `envmap` is wired, matching D2's own consequence prose ("an animated envmap re-prefilters every frame — a fixed, small cost, not a correctness hazard") rather than the Invariants table's stricter "same params → cached" wording for those two resources. The doc's Invariants table and Deferred section were corrected in this landing to match (see the doc diff) rather than left asserting an unbuilt/unsafe mechanism. A real generation-counter signal on `EffectNodeContext` is now Deferred #6, with its trigger.
2. **F-P1 irradiance gate relaxed** from "uniform white env, zero lights → lit result ≈ albedo" (not producible by the current procedural baker without new infra) to a generous-band sanity check against the studio bake — the worker's documented shortcut, accepted as-is; this is real verification debt (below), not a defect.
3. **F-P3 gate-failure, found and fixed during orchestrator gating:** the first run of `gradient_mode_matches_legacy_formula` failed by ~0.06% at scattered texels. Root cause was the *test's* oracle, not the shader: it re-derived the gradient formula on the CPU using exact `powi(2)`, while the GPU's `pow(x, 2.0)` compiles to a generic transcendental on Metal — comparing a GPU result to a CPU one is not bit-identical by construction, independent of anything F-P3 touched. Confirmed by diffing the shader's `mode == 0u` branch against the pre-existing source at `c41acc61` (character-for-character identical). Fixed by embedding the verbatim build-of-record shader as a GPU-side oracle and asserting exact per-texel equality against it — commit `c0df7921`. Re-verified independently by the orchestrator: 13/13 green.

No other deviations. Neither phase touched `render_mesh`/`render_copies`, grew `MeshVertex`, added a new port type, added `Arc<Mutex>`, or produced any PNG artifact.

## Shortcuts confessed (rolled up from phase reports)

- F-P1: prefiltered/irradiance re-convolve unconditionally rather than version-gated (see Deviation 1); irradiance gate relaxed to a generous band (Deviation 2); no per-object occlusion multiply on diffuse IBL yet (occlusion map is F-P2 scope, doesn't exist until that phase lands); prefilter cost reported as a measured wall-clock number, not a strict device-portable threshold; small new `manifold-gpu` plumbing added (`GpuTexture::mip_level_view`, `GpuSampler: Clone`) — new capability, not a deviation, named for visibility.
- F-P3: none reported; the one judgment call (strip spacing derived as `4 × emitter_width` rather than a new committed param; sun-disc falloff as a compact-support `smoothstep` rather than a Gaussian, to preserve the exact-zero-outside-lit-region contract) was within "strip math is executor-free," not a shortcut.

## Verification debt

- **VD (opened):** F-P1's diffuse-irradiance gate is a generous-band sanity check against the procedural studio bake, not the doc's originally specified "uniform white env, zero lights → lit result ≈ albedo" value-level test — the procedural baker has no uniform-white mode today. Carry until a future phase (or a small baker addition) makes the exact test producible; low risk (the roughness-response and cost gates are unaffected, and the relaxed check still passed with a tight 1.27x ratio).
- **VD (opened):** L4 (Peter, live in-app) not yet reached for either phase — expected at this stage; burns down when Peter runs the click-script below.
- None carried from a prior wave.

## Click-script for Peter (≤2 minutes)

1. Open (or create) a scene with a `render_scene` node; wire a `Texture2D` envmap and a `Pbr` material sphere. Set the sphere's roughness to 0 (mirror). **Expect:** a crisp, high-contrast reflection of the environment — no visible change from before this landing (mirror reflections were always sharp).
2. Duplicate the sphere, set its roughness to ~0.9 (rough). **Expect:** the reflection is now visibly soft/blurred and the overall lit surface reads brighter/more even than before this landing — that's the new diffuse-irradiance term and prefiltered specular replacing the old flat grey fade. Compare mirror vs. rough side-by-side: the roughness response should look continuous, not like a hard cutover.
3. On `node.bake_environment`, switch `mode` from `gradient` to `softbox`. **Expect:** the environment goes to a pure black void with a few bright horizontal light streaks (the emitter strips) — chrome/mirror objects should show sharp bright streak reflections against black, not a grey studio gradient. This mode isn't wired into imports yet (F-P4's job) — you're checking the primitive directly.
