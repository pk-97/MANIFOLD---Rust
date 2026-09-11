# Playable photoscan modifiers — 2026-09-11

**Branch:** codex/photoscan-modifiers · **Level reached:** L3 controls, L2 raster visuals / target L3 live performance.
**Doc status line (quoted verbatim):** **Status:** IN PROGRESS · 2026-09-11 · Codex lead. Wave generator pilot shipped; photoscan modifier slice implemented (L3 controls, L2 raster visuals). Unified F1–F8 architecture remains proposed.

Elastic Sculpture, Surface Peel and Vortex Fragments attach to an already imported GLB through the existing modifier picker. Saved ordinary JSON groups, shared bindings and reversible mesh splices provide the migration boundary for the future EachMesh architecture. No baked motion or hidden time source is introduced.

## Gate results (verbatim)

Focused checks below ran with `env RUSTC_WRAPPER=` in slot-9. The mandatory full landing gate runs through `scripts/land_branch.py`; its transcript is retained in the slot's `target/landing-logs`.

```text
cargo clippy --manifest-path Cargo.toml -p manifold-core -p manifold-editing -p manifold-renderer -p manifold-app --tests -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.48s

cargo test --manifest-path Cargo.toml -p manifold-editing --test scene_mesh_modifier_roundtrip
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

cargo test --manifest-path Cargo.toml -p manifold-renderer --test photoscan_modifier_plans
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s
```

The focused `gpu_proofs_gate.py --manifest-path Cargo.toml --filter photoscan_modifier` passed all nine Metal tests: independent oracles, exact bypass bytes, normal derivatives, phase wrap, patch rigidity and two-stage fused shear parity. The GLB UI flow passed all 34 steps, including Phase scrub and undo. The botanical UI fixture has no playing clip: those screenshots qualify controls, not viewport rendering.

Production `render-import` rendered the actual mushroom photoscan baseline and all three applied modifier graphs. Each modifier was observed at Phase 0, 0.25 and 0.5, retaining photographic texture. Proofs: `target/photoscan-modifier-proofs/` in the main checkout, including JSON graph exports and three-frame PNG strips. Vortex Rise was reduced from 0.4 to 0.16 after the first render clipped the cap; the corrected render was observed in frame.

## Deviations from brief

Three playable modifiers shipped first; Spatial Slices and Surface Echoes remain deferred. Patch motion groups triangles by fixed spatial cells, without connected-surface segmentation or fracture caps. Only static imported mesh reference frames and raster rendering are qualified. One instance per modifier kind follows the existing registry policy.

## Shortcuts confessed (rolled up from phase reports)

Compile-time JSON inclusion and the shared descriptor attachment builder are intentional interim adapters. The future unified resolver should adopt the same graph groups and control identities. Dynamic ray-tracing acceleration updates are not fixed in this slice. Full performance and live modulation/project reopen observation are not claimed.

## Verification debt

BUG-e3p6.4: continuous deformation requires dynamic ray-tracing geometry updates. BUG-e3p6.5: production project reopen with driver/LFO/audio modulation, sustained timing/memory and representative heavier scans. The broader BUG-e3p6 programme remains open.

## Click-script for Peter (≤2 minutes)

1. Open MANIFOLD and import a photoscan GLB; select its scene layer and play a clip so the scene is visible. Use raster rendering for these modifiers.
2. In the scene inspector, choose **Add Modifier → Elastic Sculpture**. Scrub **Phase**, then **Bend** and **Cross Bend**: the textured scan bends continuously. Undo restores the control value.
3. Remove Elastic, then add **Surface Peel**. Adjust **Lift**, **Curl** and **Phase**: photo-textured surface patches open away from the scan.
4. Remove Peel, then add **Vortex Fragments**. Adjust **Orbit**, **Rise**, **Separation** and **Phase**: patches move through 3D space. **Enabled** bypasses the modifier. The ordinary controls are available to the existing modulation system.

After landing, build and launch current main:

```sh
env RUSTC_WRAPPER= cargo run --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/Cargo.toml' -p manifold-app --bin manifold
```
