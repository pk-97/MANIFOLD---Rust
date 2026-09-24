# Exact flower contact experiment

Active worktree: slot-0, branch `codex/flower-mesh-drop`. Retained for Peter’s requested iteration; app changes are not landed. Rule-limit removal is already on main (`a447c3737`, `c81441fd1`). Tracking: BUG-vaj5.

Uses the original `cc0__tiger_lily.glb`: 446,868 vertices / 454,840 triangles. Source mesh/material descriptors and geometry/texture binary are preserved. The scan’s small calibration cube is also retained. Collision uses those same transformed triangles; floor is 20 m square.

Fixed terrain-only contact selection that reported nearly 10 m of false overlap against the floor. Moving mesh/hull contact is now two-sided; static terrain keeps its original path. The importer now applies animated ancestors to every material contributor through the existing hierarchy palette.

Verified six-second drop: lowest vertex 0.00443 m, final gap 0.00493 m, final velocity approximately zero. Optimized test build took 36.94 s: this is an offline replay prototype, not real-time integration. Mesh/mesh and mesh CCD remain unsupported. Native rest offset accounts for the roughly 5 mm gap.

Checks: 21 physics tests; 11 importer animation tests; focused physics/renderer/example clippy; one skin-mesh GPU proof through gpu_proofs_gate. The actual failing triangle regression fails on the old path and passes with the fix. Observed final render: `/tmp/tiger-lily-fixed-contact.png`.

The generated replay is ignored and retained locally at `tests/fixtures/gltf/tiger_lily_drop.glb`. Rebuild and run from this worktree:

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0'
env RUSTC_WRAPPER= bash .claude/scripts/with-build-lock.sh cargo build --manifest-path "$PWD/Cargo.toml" -p manifold-renderer --profile test --features gpu-proofs --example flower_mesh_drop --bin render-import
bash .claude/scripts/with-build-lock.sh target/debug/examples/flower_mesh_drop '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/gltf/cc0__tiger_lily.glb' tests/fixtures/gltf/tiger_lily_drop.glb
bash .claude/scripts/with-build-lock.sh target/debug/render-import tests/fixtures/gltf/tiger_lily_drop.glb --time 5.9 --size 1280x960 --param 5_distance=5 --param 5_near=0.01 --frames-max 80 --out /tmp/tiger-lily-fixed-contact.png
```

Musical timing prototype: `flower_mesh_drop` accepts optional `bpm impact-beat-offset` (zero-based). Native confirmed-impulse hit events measure the first impact above Box3D's 1 m/s threshold at 240 Hz. Shift the replay by target seconds minus measured flight time; hold the initial pose until release. No time stretching or live transport integration. New poses/orientations require a new measurement. An impossible early target fails explicitly.

Verified at 120 BPM / offset 4: flight 0.454167 s, release 1.545833 s, impact 2.000000 s; 4.167 ms event resolution. All 361 original trajectory samples remain unchanged, original scan binary/materials preserved. 22 physics tests, 2 timing tests, physics and example clippy pass. Observed 30 fps preview around impact; eight-second MP4 includes metronome and an accent at 2 s:
`/Users/peterkiemann/.codex/visualizations/2026/09/23/01a0cc0e-e40b-77c1-8813-db543d368523/tiger-lily-musical-impact.mp4`.

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0'
bash .claude/scripts/with-build-lock.sh target/debug/examples/flower_mesh_drop '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/gltf/cc0__tiger_lily.glb' tests/fixtures/gltf/tiger_lily_musical_drop.glb 120 4
```

Keep source, generated GLBs and commits local in this active worktree per Peter's preference; no experiment push or landing.

32-piece fracture preview: optional fifth argument `32` partitions original triangles spatially into surface patches, retaining all source attributes/materials and triangle winding. Patches may include disconnected islands; this does not create sealed solid shards. All pieces share the intact motion through the first hit, then receive separate exact-mesh bodies with area-proportional mass and inherited linear/angular motion. No added explosion impulse. Recreates the world without the intact body at release. Native bridge adds point/angular velocity reads and velocity writes.

Mesh/mesh contacts are explicitly unsupported by native `src/contact.c`; each piece has gravity and floor contact but passes through other pieces. Peter questioned this limitation: genuine piece-to-piece contact remains the next physics foundation gap, not a claim of completed shatter physics.

240 Hz fragment run failed: two upper pieces penetrated the floor by up to 0.339 m. 960 Hz contact refresh reduced sampled minimum y to -0.01103 m; final maximum piece clearance 0.00499 m, settled. Six simulated seconds took 104.97 s. First hit 0.451042 s, release 1.548958 s, aligned impact 2 s. Existing 3 cm floor tolerance passes, but the residual ~1 cm penetration is not zero-gap contact. Normal intact-drop mode retains 240 Hz.

Verified 23 physics tests, 5 example tests, focused physics/example clippy; all 454840 source triangles appear exactly once in exported pieces, original binary/attributes/materials preserved. Every piece has identical poses until impact and independent poses afterward. Observed 30 fps render of intact flower, impact, break and settled pieces. Video: `/Users/peterkiemann/.codex/visualizations/2026/09/23/01a0cc0e-e40b-77c1-8813-db543d368523/tiger-lily-shatter.mp4`. Replay: `tests/fixtures/gltf/tiger_lily_shatter.glb` (ignored, retained).

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0'
bash .claude/scripts/with-build-lock.sh target/debug/examples/flower_mesh_drop '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/gltf/cc0__tiger_lily.glb' tests/fixtures/gltf/tiger_lily_shatter.glb 120 4 32
```
