# Landing — GLB_CONFORMANCE session 2: G-P3 + G-P4 + G-P5

**Date:** 2026-07-15 · **Branch:** `wave/glb-conformance-s2` → main `55ee0fe0` · **Orchestrator:** Fable 5, Sonnet 5 workers (one per phase, one sent back once)

**Status line after landing (quoted verbatim from docs/GLB_CONFORMANCE_DESIGN.md):**
> **Status: IN PROGRESS · 2026-07-15 · Fable 5 (authored) + Sonnet 5 (G-P1+G-P2 executed and landed same day, `909976d2`; G-P3+G-P4+G-P5 executed and landed same day, session 2). G-P1 (conformance harness) + G-P2 (cap deleted, import is 1:1, BUG-163 fixed as a side effect) + G-P3 (anisotropic filtering) + G-P4 (KHR_texture_transform all five map families + specular/ior F0) + G-P5 (clearcoat lobe, factor-only) SHIPPED. G-P6 (hdri_source) + G-P7 (burn-down) not yet executed.**

## What landed

- **G-P3** (`e3e0722f`): `GpuSamplerDesc.max_anisotropy` (default 1, byte-identical — proven by gpu proof), Metal `setMaxAnisotropy`, render_scene material sampler at 8. Proof `aniso_sharpens_grazing_minification`: stripe energy 0.0 (aniso 1) vs 6.29 (aniso 8).
- **G-P4** (`38feaeaf`): KHR_texture_transform folded once at parse, applied in-shader for **all five map families** (worker's first pass was base-color-only; orchestrator rejected it on evidence — the AMG carries transforms on 9 normal maps). specular/ior → dielectric F0 **per the Khronos spec formula — the design doc's brief formula was wrong (spurious `0.16 *`, missing `specularFactor`) and is corrected in the doc with a dated note.** TextureTransformTest fetched as `.gltf`+sidecars (no glb variant at the pinned commit). Both assets flipped to expect_pass with falsification-tested numeric checks.
- **G-P5** (`f22543a3`): clearcoat + clearcoat_roughness on `Material` (defaulted inert), second GGX lobe reusing existing D/G/F helpers, Khronos layering `lit = base*(1-fc) + coat*fc` (README quoted in the worker report; base includes emission per spec, wider than the doc's gloss). Factor-only; coat textures stay report lines (Deferred #2). No uniform growth (rides `alpha_params.z/w`). gltf 1.4.1 has **no typed clearcoat accessor** (verified in crate source) — factors parse via the file's existing raw-extension-JSON pattern; no new dependency.

## Gate output (verbatim, warm main checkout, post-merge)

```
cargo clippy --workspace -- -D warnings         → Finished (clean)
cargo deny check bans                           → bans ok
cargo nextest run --workspace                   → 3389 tests run: 3389 passed, 12 skipped
cargo run -p manifold-renderer --bin check-presets → 57 presets: 57 ok, 0 failed
cargo test ... --test gpu_proofs -- --test-threads=1 → 48 passed; 0 failed
cargo test ... --test glb_conformance -- --test-threads=1 →
  glb conformance summary: 7 expect_pass checked, 1 xfail, 0 skipped (not fetched), 0 failures
```

The only remaining xfail is `TextureSettingsTest` (BUG-164, per-texture sampler settings — unowned by any phase, pending assignment).

## Verification level reached

**L2** everywhere (target). Orchestrator independently reran every phase gate in the worktree, read every demo artifact, and ran held-out Khronos assets per phase: WaterBottle (G-P3, renders correctly), ToyCar (G-P4 and again post-G-P5 — flame decals, clearcoat factor now mapped), BoomBox (renders black — **pre-existing**, confirmed identical on pre-G-P3 main; already tracked as BUG-165). TextureTransformTest's badge layout was compared against Khronos's own screenshot: semantically identical (arrows land on green ✓; the visible red ✗/yellow ⊘ badges are baked into the reference too). All falsification runs (checks fail when the feature is shader-disabled) were reproduced in worker reports with outputs quoted.

## Deviations from the briefs

1. **G-P4 F0 formula:** doc brief contradicted the Khronos spec; spec implemented, doc corrected (precedent: spec wins).
2. **G-P4 scope send-back:** base-color-only transform rejected; per-map implemented per the doc's own deliverable text.
3. **G-P5 accessor premise:** brief assumed a typed `Material::clearcoat()` accessor; gltf 1.4.1 has none — raw-JSON parse via the established pattern instead, no dependency change.
4. **G-P5 layering scope:** README's layering scales emission too; implemented README over the doc's narrower "diffuse + base specular" gloss, flagged in the shader comment.

## Peter's click-script (≤2 min)

1. `cargo run -p manifold-renderer --bin render-import -- "tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb" --out /tmp/amg.png` — expect: full body panels (78/78 objects), silver NASA livery, carbon/alcantara normal detail at authored tiling (G-P4), coat highlight on paint/glass/rims (G-P5).
2. `bash scripts/fetch-gltf-conformance.sh && cargo test -p manifold-renderer --features gpu-proofs --test glb_conformance -- --test-threads=1 --nocapture` — expect: `7 expect_pass checked, 1 xfail, 0 skipped, 0 failures`.
3. Optional: open `/tmp/helmet_landing_s2.png` (grazing-orbit helmet, produced at landing) — expect clean sharp material detail, no smear.

On stage this means: a purchased car model drops in with its paint reading like paint — coat highlight over livery, carbon weave at the right scale — and the conformance suite now pins five glTF material extensions mechanically instead of by eyeball.

## Verification debt

None opened. Peter's look-pass on the AMG's clearcoat/transform improvements folds into the standing IMPORT_FIDELITY look-pass already owed (memory: import-fidelity-design), not a new ledger line.
