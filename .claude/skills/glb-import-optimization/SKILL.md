---
name: glb-import-optimization
description: Playbook for optimizing heavy photoscan/store-bought GLB imports to hit frame budget — profiling method, AO-strip, mesh pipeline, measured dead ends. Invoke for slow imported scenes.
---
`docs/GLB_IMPORT_OPTIMIZATION_GUIDE.md` (landed `d76194b2`, 2026-07-18) — how to take a heavy
photoscanned/store-bought GLB scene to stage-ready frame time. Read it before optimizing any
import; don't re-derive these the expensive way.

Key facts from the MeshAudio session (three CC0 flowers, 0.6–1.4M tris each, 4K@60, M4 Max):

- **Oracle:** `cargo xtask perf-soak <proj>.manifold --seconds 10 --profile` → per-node GPU
  attribution JSON. For A/B, run the non-profile form back-to-back — M4 Max thermals make
  absolute numbers drift; only adjacent deltas are trustworthy.
- **Biggest win: strip SSAO** (~24ms; 45.7→21.4ms p50). Imported scenes bake `ssao_gtao →
  bilateral_blur ×2 → mix` into the graph; no runtime toggle. GOTCHA: the AO nodes live in the
  layer's own `genParams.graph` (nested `group` titled "Ambient Occlusion"), NOT the embedded
  preset — editing the preset does nothing. `null` graph falls back to the embedded def; copy the
  FULL def (version+presetMetadata) or the loader rejects it.
- **Mesh pipeline (gltf-transform):** weld → bake normals from ORIGINAL high-poly → `simplify
  --lock-border true` → `resize` to 2K. Weld is seam-aware (safe for UVs). Normal maps preserve
  surface shading but NOT silhouette, so decimate moderately for organic edges unless baking
  normals. Realism lives in the albedo, not the geometry.
- **Measured dead ends:** shadow-map resolution (geometry-bound, not texel-bound), output
  resolution 4K→1440p (geometry-bound, not fill-bound), dropping the env/IBL pass (~1.9ms only).
- **Product gap:** a real per-scene AO toggle (param → `switch_texture`, runtime prunes the dead
  branch) doesn't exist yet — AO-off is a manual graph edit per project.

Related: static-scene shadow/IBL caching already shipped (BUG-189/197,
[realtime-3d-design] neighbors); next engine lever = indexed-mesh rendering (R4, deferred in
`RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md`). `.manifold` edit mechanics (preserve history via
`zip <copy> project.json`) are in the guide.
