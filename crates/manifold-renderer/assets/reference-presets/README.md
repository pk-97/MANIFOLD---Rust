# Reference presets (not shipped)

Presets parked here are **not scanned by the preset loader** (it only reads
`assets/effect-presets/` and `assets/generator-presets/`) and therefore do not
appear in the app.

These eight were authored as test rigs for the 3D rendering / cinematic-post
infrastructure (REALTIME_3D, CINEMATIC_POST), not as show content. Pulled from
the bundled library 2026-07-16 at Peter's request; kept in-repo as working
references for graph idioms (render_scene lighting, DoF/AO chains, instancing,
mesh deform).

To reinstate one, move it back into `assets/generator-presets/` — the loader
picks it up on next launch, no rebuild. If it contains `wgsl_compute` nodes,
regenerate the fused-WGSL golden (`UPDATE_FUSION_GOLDEN=1 cargo test -p
manifold-renderer --lib fused_wgsl_snapshot`).

Note: `CinematicScene.json` here is still compile-time-included by the
CINEMATIC_POST I5 gate test (`preset_runtime.rs::bundled_cinematic_scene_loads_and_compiles`)
— don't delete it without updating that test.

`ReactionDiffusion.json` — built 2026-07-16 (VISUAL_PIECES A3), shelved same day
on Peter's look-pass: "shows a circle and then fades out to black, not a great
visual." The graph is correct (Sims Gray-Scott, fp32 loop, verified against
NumPy ground truth) and the kernel headers carry the hard-won precision/
formulation notes — worth mining for any future RD-flavoured piece.
