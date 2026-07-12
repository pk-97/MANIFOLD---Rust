# Cinematic wave batch 1 — CAMERA_AND_LENS P1+P2 — landed 2026-07-12 @ TBD-MERGE-SHA

**Branch:** feat/cinematic-wave → main · **Level reached:** L1 / target L1 (§10 — this cluster's
no-PNG rule, Peter 2026-07-12, overrides the standard's L2-demo minimum; acceptance is the
numeric gates, not an observed artifact)
**Doc status line (quoted verbatim):** `**Status:** SHIPPED · P1 (`22530ac1`) + P2 (`de193e01`)
landed 2026-07-12, main · designed 2026-07-12 · Fable 5`

## Escalation resolved mid-batch

P1 hit a real conflict between D3 (extend `flatten_3d`'s `derived_uniforms` for the camera
branch) and I3's original gate wording (existing parity test passes with a literal 0-line diff)
— the freeze codegen always places derived fields immediately before its injected
`dispatch_count` word, so any new derived field shifts that offset and the old hardcoded test
buffer breaks. Escalated to Peter, then to Fable (the doc's author) for a ruling rather than
guessed through. Fable confirmed the conflict, ruled that the 0-line-diff clause was a proxy for
"legacy math untouched" rather than a literal freeze, and gave an exact repacking spec. Doc
amended in the worktree (§3 I3 row + a dated amendment block after P1's Gate section) before the
worker resumed. Full detail in the amendment text itself, `docs/CAMERA_AND_LENS_DESIGN.md`.

## Gate results (verbatim, orchestrator-run — not the workers' self-reports)

Post-merge with `origin/main` (which had independently landed PCSS in the interim, touching
the same `render_scene.wgsl`/`.rs` files — merged clean, `ort` strategy, re-gated after):

```
$ cargo nextest run -p manifold-renderer --lib
Summary [ 5.654s] 1106 tests run: 1106 passed, 3 skipped

$ cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
  (includes camera_conformance ×3, render_scene_exposure ×2, render_scene_pcss ×3,
   render_scene_fog/lights/shadows/instances — all pre-existing suites green unmodified)

$ cargo clippy -p manifold-renderer -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.77s   (clean, no warnings)

$ rg 'exposure' crates/manifold-renderer/src -g '*.wgsl'
render_scene.wgsl (new, this phase)
effects/shaders/aces_tonemap_compute.wgsl (pre-existing, unrelated ACES exposure uniform —
  confirmed untouched by this wave, doesn't read LensParams/scene_params/camera_lens)
```

`git diff --stat` on `shaders/project_3d.wgsl` (the P1 hand oracle) = empty, confirming the
legacy wireframe math is bit-for-bit untouched.

Full workspace sweep (clippy --workspace, nextest --workspace, cargo deny check bans) run in
the main checkout after the merge — see the merge commit for the exact invocation and result.

## Deviations from brief

- I3's gate wording amended mid-phase per the Fable ruling above (documented in the design doc
  itself, not silently absorbed).
- `camera_conformance`'s fixture varies the camera across 5 configurations rather than
  translating the world point across 5 positions — no existing primitive can arbitrarily
  translate an `Array<MeshVertex>` before `flatten_3d` without inventing new infrastructure
  (out of phase scope). Exercises the same invariant (I1) via 5 independent `(view_z, ndc)`
  pairs instead. Documented in the test's module doc.
- `node.camera_lens`'s `f_stop` param default is `1000.0`, not literal `f32::INFINITY` —
  `serde_json` round-trips `INFINITY` as `null` and fails to deserialize it back, which would
  corrupt a saved project. `LensParams::PINHOLE` (the Rust const, never serialized) keeps the
  doc's literal `f32::INFINITY` exactly as committed; only the node's own param default differs.

## Shortcuts confessed (rolled up from phase reports)

- P1: none.
- P2: the `f_stop` default substitution above (safety-motivated, not corner-cutting).
- `scatter_particles_camera`'s divergent projection math (no `1/tan(fov_y/2)` scale term vs.
  `Camera::proj`) documented in the doc's audit table per D3's VERIFY-AT-IMPL — left unchanged,
  shipped fluid-scatter content, not this wave's scope.
- A latent gap noted in a P2 code comment: `node.camera_lens`'s derived `proj_f` for a
  hypothetical orthographic camera wired into `flatten_3d`'s camera port would be wrong (the
  shader's divide-by-`view_z` step is perspective-specific) — unreachable today since none of
  the three camera-source primitives can emit `CameraMode::Orthographic`.

## Verification debt

None opened — this cluster's no-PNG rule makes L1 (numeric gates) the actual target, not a
reduced target carrying debt against L2. No gap between level reached and level targeted.

## Click-script for Peter (optional — not required for "done" under this cluster's rule)

1. Open a project using `node.orbit_camera` → `node.flatten_3d` (any wireframe preset, e.g.
   BlossomWire) and wire the camera port in. Expect: wireframe renders exactly as before wiring
   it (camera unwired vs. wired-but-untouched-mode should look identical since S was gate-picked
   for pixel agreement, not for the legacy modes — the legacy modes ignore the port entirely
   unless the primitive's `mode` param is also switched to consume it in future work).
2. Insert `node.camera_lens` between any camera emitter and `render_scene`, leave all four
   params at default. Expect: scene renders identically (PINHOLE is a no-op).
3. Bind `exposure_ev` to a fader and sweep it. Expect: the whole scene dips toward black below
   0 and blows toward white above 0, camera/lighting untouched.
