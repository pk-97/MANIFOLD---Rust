# Exact flower contact experiment

Active slot-0, branch `codex/flower-mesh-drop`; retained locally for Peter’s requested iteration. No experiment push or app landing. Tracking: BUG-vaj5. Rule-limit removal is already on main (`a447c3737`, `c81441fd1`).

Original `cc0__tiger_lily.glb`: 446,868 vertices / 454,840 triangles, including its calibration cube. Replay retains the source binary, attributes, materials and every triangle exactly once. Collision uses the same triangles. Floor is 20 m square. The importer applies animated ancestors to all material contributors through the existing hierarchy palette.

Current prototype partitions the original triangles into 32 surface patches. All pieces share the intact pose until the first confirmed solver hit, then become separate exact-mesh bodies with area-proportional mass and inherited linear/angular motion. No added explosion impulse. Mesh/mesh contacts now use dual BVH traversal and original triangle-pair distances, normal clustering and the existing Box3D solver. Moving mesh/hull and mesh/mesh reduction retain the deepest contact. Motion-bounded speculative queries reduce dense-pair work. The example explicitly uses 120 Hz contact stiffness, four substeps, and 960 Hz fragment contact refresh (240 Hz intact mode); other callers retain native tuning defaults.

Verified current six-second flower simulation: sampled lowest vertex y=+0.001501 m, within the tightened 1 mm penetration criterion. Full run took 241.02 s in the optimized test build. Fragments rest on each other; maximum final root-position change across all 32 pieces is 0.000633 m over the last replay interval. The reported final body velocity is only the first fragment, not a measurement of all pieces. Floor sampling uses replay frames; the focused fast-rotating mesh regression separately checks every physics step.

At 120 BPM / zero-based beat offset 4, release is 1.548958 s; measured flight is 0.451042 s; replay impact is 2.000000 s. Event resolution is 1.042 ms. Timing shifts the trajectory without stretching it; new initial poses require a new measurement. First-hit events require the native 1 m/s approach threshold.

Validation: all 27 physics tests and focused physics/example clippy pass. New cases cover dynamic mesh stacking, fixed mesh contact with both windings, falling through a disconnected opening, and a fast rotated mesh against the floor. Original-triangle coverage, source binary preservation and identical fragment poses through impact pass. Observed rendered frames show intact flower, impact, breakup and a pile on the floor. Video has eight seconds of 960x720 / 30 fps replay with metronome and an accent at 2 s; audio/video both start at zero.

Replay (ignored, retained): `tests/fixtures/gltf/tiger_lily_contact_shatter.glb`.
Video: `/Users/peterkiemann/.codex/visualizations/2026/09/23/01a0cc0e-e40b-77c1-8813-db543d368523/tiger-lily-contact-shatter.mp4`.
Observed contact sheet: `/tmp/tiger-lily-contact-shatter-check.jpg`.

Still an offline surface-fracture prototype: patches may contain disconnected islands and have no cut caps or thickness. Mesh CCD and solid-volume containment are unsupported. This does not qualify thousands of pieces, real-time playback, live transport/tempo changes, or arbitrary orientations. Earlier no-piece-contact preview had roughly 1 cm floor penetration; that result is superseded by the current run. A softer-contact trial reached 4.645 mm penetration and was stopped before the successful explicit stiffness setting.

Rebuild and run:

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0'
env RUSTC_WRAPPER= bash .claude/scripts/with-build-lock.sh cargo build --manifest-path "$PWD/Cargo.toml" -p manifold-renderer --profile test --features gpu-proofs --example flower_mesh_drop --bin render-import
bash .claude/scripts/with-build-lock.sh target/debug/examples/flower_mesh_drop '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/gltf/cc0__tiger_lily.glb' tests/fixtures/gltf/tiger_lily_contact_shatter.glb 120 4 32
```

Import the generated GLB into MANIFOLD to inspect the replay. Previous intact and no-piece-contact GLBs/videos remain local for comparison. Previous importer tests and skin-mesh GPU proof passed; this stage changes physics and the offline example, with no new GPU rendering implementation.
