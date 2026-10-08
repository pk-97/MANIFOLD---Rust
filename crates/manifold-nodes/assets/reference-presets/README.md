# Reference presets (not shipped)

Presets parked here are **not scanned by the preset loader** (it only reads
`assets/effect-presets/` and `assets/generator-presets/`) and therefore do not
appear in the app.

The remaining reference preset was authored as a test rig for the 3D rendering
infrastructure (REALTIME_3D), not as show content. It is kept in-repo as a
working reference for reaction-diffusion graph idioms.

To reinstate one, move it back into `assets/generator-presets/` — the loader
picks it up on next launch, no rebuild. If it contains `wgsl_compute` nodes,
regenerate the fused-WGSL golden (`UPDATE_FUSION_GOLDEN=1 cargo test -p
manifold-nodes --test main contracts::node_graph::catalog_tests::wgsl_snapshot::`).

`ReactionDiffusion.json` — built 2026-07-16 (VISUAL_PIECES A3), shelved same day
on Peter's look-pass: "shows a circle and then fades out to black, not a great
visual." The graph is correct (Sims Gray-Scott, fp32 loop, verified against
NumPy ground truth) and the kernel headers carry the hard-won precision/
formulation notes — worth mining for any future RD-flavoured piece.
