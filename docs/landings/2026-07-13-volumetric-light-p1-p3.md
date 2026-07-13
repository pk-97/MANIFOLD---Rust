# VOLUMETRIC_LIGHT_DESIGN P1–P3 — landed 2026-07-13 @ `e1721268` (pending push to main)

**Branch:** `feat/volumetric-light` · **Level reached:** L2 (demo PNGs rendered and read by the orchestrator) / target L2 (§10) — **but the L2 evidence is a negative result, see below**
**Doc status line (quoted verbatim):** "SHIPPED (P1–P3, 2026-07-13) — mechanically complete: every invariant (V1–V6), the CPU-vs-GPU parity proof, the monotonic performer faders, and the content-thread perf gate all pass, across Sun and Point lights. But NOT show-ready: across both look-critical demos (P2's Sun-only vertical slice and P3's night-garden multi-light shot), the rendered output does not read as 'a black void filled with haze with beams of light shining through' — it reads as an ordinary dim scene with a faint shadow patch (P2) or a soft ambient glow next to unlit silhouettes (P3), with no legible directional beam in either. This is the numerically-green/looks-wrong pattern D6 exists to catch; see the landing report for the full look-pass writeup and next-step recommendation. Pending Peter's look-pass — do not present this as 'god rays are done' until he's seen it."

This lane is the first live test of the machine-check gates (graph-tool validate/fusion, catalog
regen, boundary/contract meta-tests) added to this doc 2026-07-13 — see "Tool feedback" below.

## What shipped

Three phases, one worktree (`.claude/worktrees/docs-git-sync`), three fresh workers (one per
phase, per the doc's execution protocol), all gates independently re-run by the orchestrator
(not trusted from worker self-report):

- **P1** (`ba79aca6`) — `Atmosphere` gains `shaft_intensity`/`shaft_anisotropy`/`shaft_quality`
  (D1), threaded into `RenderSceneUniforms` at zero cost when off. V1 (`shafts_off_byte_identical`,
  `wants_shafts_gate`) + V6 fog characterization (BUG-118 absorbed, measured numbers in the
  backlog entry).
- **P2** (`d3f998e8`) — half-res Sun-only shaft march + depth-aware bilateral upsample + additive
  composite (D2/D3), exactly per the committed math. V2–V5 all pass; alpha-over-transparency
  check (D3's critical trigger) confirmed beams ARE numerically present over a checkerboard, so
  the lane was NOT parked.
- **P3** (`e52d39cf`) — Point-light attenuation + frustum-clipped shadow sampling, multi-light
  accumulation, `shaft_quality` end-to-end wiring, CPU reference extended to Sun+Point, perf gate.
  Fixed BUG-145 (shaft/shadow pipelines missing from `RenderScene::prewarm_pipelines`, 79.9ms →
  42.0ms on frame 0); logged the residual BUG-146 (a broader, unrelated prewarm gap) rather than
  scope-creeping into fixing it.
- **Landing merge** (`28bfe68f` + `e1721268`) — merged `origin/main` (35 commits: CINEMATIC_POST
  P5/P6, media-export bug wave, widget unification P8, etc.), resolved a real `docs/BUG_BACKLOG.md`
  three-way ID collision (below), and fixed one post-merge compile break (`Arc<GpuDevice>` API
  change from BUG-054 landing on main, in a P3-added test helper).

## The visual-quality finding (read this before the gate output)

Per this doc's own D6 rule — written from Peter's 2026-07-12 verdict that a numerically-green
cinematic stack still "look[ed] terrible" — every phase with a visible surface produced an
acceptance-demo PNG, and the orchestrator looked at every one before proceeding to the next
phase, not just the mechanical gates. **None of them look like god rays.**

- **P2 demo** (`/tmp/volumetric_light_p2_demo_intensity_0.3.png`,
  `..._intensity_1.5.png`, `..._checkerboard.png`, `..._checkerboard_off.png`): an ordinary
  gray-lit ground plane and cube, light-gray sky background (not the black void the design
  specifies), with only a faint shadow patch under the cube. The checkerboard comparison shows
  a real, measurable brightening (confirmed numerically, `Δr=0.103` at a confirmed-void pixel)
  but no legible directional beam.
- **P3 demo** (`/tmp/vol_light_p3_night_garden_hires_med.png`,
  `..._high.png`): a checkerboard void with two dim gray pillar silhouettes and a soft warm
  ambient glow bleeding in from one edge (the unshadowed Point light). Med and High quality are
  visually indistinguishable. No visible directional beam, no visible dark shadow carved by the
  shadow-casting light.

Both phase workers made genuine, honest scene-composition attempts (repositioning lights,
extreme-value diagnostics) to find a framing where the beams read clearly, entirely within the
allowed latitude (scene composition, never the committed D2/D3 math) — none succeeded. The P3
worker's working theory, not further verified: an omnidirectional Point light with no cone/spot
restriction floods most of the visible volume roughly evenly, so a thin occluder's shadow
"carve" is a subtle sliver rather than a dramatic beam at any framing/resolution tried. This may
need a much larger/closer occluder, a spot-cone light (outside D2's scope), or is the honest
ceiling of single-scattering-over-isotropic-point-lights without a narrower aperture — **a call
for Peter, not a claim I'm making on the design's behalf.**

**This is exactly why D6 exists**, and it worked as designed: the mechanical gates alone would
have said "SHIPPED, all green" and nobody would have looked. The doc's status line says so
plainly instead.

## Gate results (verbatim)

Independently re-run by the orchestrator after each phase (not trusted from worker report),
and again after the landing merge + the post-merge compile fix:

```
# P1 (after merge, final state)
cargo test -p manifold-renderer --lib: 1187 passed; 0 failed; 3 ignored
cargo clippy -p manifold-renderer -- -D warnings: clean

# P2 gpu-proofs (V1-V5)
test render_scene_fog::shafts_off_byte_identical ... ok
test render_scene_fog::shafts_two_runs_bit_identical ... ok
test render_scene_fog::shafts_leave_alpha_untouched ... ok
test render_scene_fog::shaft_intensity_is_a_monotonic_performer_fader ... ok
test render_scene::tests::upsample_weight_does_not_bleed_across_a_depth_silhouette ... ok
test render_scene::tests::upsample_weight_fallback_matches_plain_bilinear_when_all_taps_disagree ... ok

# P3 gpu-proofs (V1-V5 + Point-light V3 case) — final re-run post-merge-fix
running 27 tests (gpu_proofs::render_scene_*)
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out

# Full workspace sweep at landing (warm worktree, post-merge)
cargo build --workspace: clean, 15.06s
cargo clippy --workspace -- -D warnings: clean, 11.81s
cargo nextest run --workspace: 3312 tests run: 3312 passed (1 leaky), 9 skipped, 20.3s
cargo deny check bans: bans ok

# Perf (MANIFOLD_RENDER_TRACE=1, 4 shadow-casting Point lights, shaft_quality High/32 steps, 120 frames)
Before BUG-145 fix: frame=0 total=79.9ms (over 20ms budget)
After BUG-145 fix:  frame=0 total=42.0ms — residual is BUG-146, unrelated to this design
Frames 1-119: no trace line printed (mechanism only fires >20ms) — all under budget
```

## Deviations from brief

- **Post-merge compile fix not anticipated by any phase brief**: origin/main's BUG-054 landing
  changed `PresetRuntime::from_json_str_with_device`'s signature (`&GpuDevice` → `Arc<GpuDevice>`)
  between this branch's divergence and the landing merge. P3's `render_readback_hires` test
  helper (added for the night-garden demo) broke post-merge; fixed mechanically (wrap in `Arc`,
  matching the existing sibling helper's pattern in the same file) and re-verified. Committed
  separately (`e1721268`) from the merge commit itself for a clean history.
- **`docs/BUG_BACKLOG.md` three-way conflict on landing merge**: this branch's own BUG-144/145/146
  collided with origin/main's independently-numbered BUG-144/145 for two DIFFERENT reasons — see
  "Tool feedback" / BUG numbering below for the full resolution. Not anticipated by any phase
  brief; resolved by the orchestrator during the landing merge, not a phase worker.
- **A concurrent-session hazard during P3**: mid-phase, a stray resumed agent (an artifact of an
  earlier broken resume attempt on this orchestrator's side, not a genuine second live session)
  appeared to be editing the same worktree; it self-resolved with no data loss (confirmed: the
  final P3 commit contains complete, correct work verified independently by both the confused
  agent and the orchestrator's own gate re-runs) but cost real wall-clock time chasing down. Not
  a lane defect — an orchestration-tooling one, noted for the record.

## Shortcuts confessed (rolled up from phase reports)

- P1: none.
- P2: demo scene's cube occluder/camera framing are not specified verbatim in the design doc
  (only fog/light params are) — added a standing cube since a flat-only ground plane gives the
  shadow-carve mechanism nothing to carve.
- P3: deleted the scratch demo/perf presets and the scratch perf-verify harness module before
  finishing, per P1/P2 precedent — none remain on disk or in the diff. Did not build a fourth
  scene-composition variant beyond three honest attempts — stopped once the failure pattern
  repeated consistently rather than fishing indefinitely.

## Verification debt

**VD-028 opened** — mechanically L2 (PNGs rendered and read), but the L2 evidence is itself the
negative visual-quality finding above; Peter's look-pass is the actual burn-down, and closing
requires either a design-level fix that visibly changes the PNGs or Peter accepting the current
look as an iteration baseline. Full text: `docs/VERIFICATION_DEBT.md`.

## Tool feedback (first live test of the 2026-07-13 machine-check gates, as requested)

- **`graph-tool` binary name**: the design doc's header and this lane's worker briefs both wrote
  `graph_tool` (underscore); the actual binary is `graph-tool` (hyphenated,
  `crates/manifold-renderer/src/bin/graph_tool.rs` but registered under the hyphenated bin name).
  No functional friction once corrected, but worth fixing the doc header to prevent every future
  worker from tripping on it.
- **`validate`/`fusion` themselves**: clean, fast, no false positives on the one scratch preset
  P1 built and validated (`OK <path>`, no ambiguity). This lane didn't touch any COMMITTED preset
  JSON or add a new atom, so the catalog-drift and boundary/contract meta-tests were exercised
  only passively (via the full workspace sweep, where they stayed green) — a fuller test of those
  specific gates will need a future lane that does add a preset-visible param or a new primitive.
- **`docs/BUG_BACKLOG.md` ID collisions on merge — a real, non-hypothetical hazard**: this
  branch's own P1/P3 workers picked BUG-144/145/146 as "next free" IDs; origin/main's 35 commits
  in the meantime had independently picked BUG-144 (same underlying bug, different write-up —
  merged into one entry citing all four confirmations) and BUG-145 (a completely different bug,
  bokeh dead-code — renumbered to BUG-147 since this branch's 145/146 already had internal
  cross-references). A THIRD, pre-existing collision was found while picking the next VD id:
  two unrelated `docs/VERIFICATION_DEBT.md` entries both claim VD-020, from two earlier unrelated
  landings, silently — logged as **BUG-148** rather than fixed inline (out of this lane's scope).
  **This is the same class of failure the git-tree-discipline doc's twin-commit incident warned
  about, one layer up**: sequential next-free-ID allocation across concurrent lanes is fragile.
  Worth a design-level fix (a uniqueness check alongside `bug_status.py`, or a different ID
  allocation scheme) rather than three more manual catches next time.

## Click-script for Peter (≤2 minutes)

1. Open `/tmp/volumetric_light_p2_demo_intensity_0.3.png` and
   `/tmp/vol_light_p3_night_garden_hires_high.png` — expect: neither reads as "a black void with
   beams of light shining through"; P2 looks like an ordinary lit gray scene, P3 looks like a
   checkerboard void with two dark pillars and a soft glow blob, no legible beam in either.
2. Compare against `/tmp/volumetric_light_p2_demo_checkerboard.png` vs.
   `..._checkerboard_off.png` — expect: a real but subtle brightness difference (the numerics
   are right, the effect exists) — not a visible directional shaft.
3. Decide: is this worth a design-level revisit (tighter/closer occluders, a spot-cone light
   option, a different scattering approach) before it's used in EP content, or is it an accepted
   v1 baseline to iterate on later? Either answer unblocks VD-028's burn-down — say which.
