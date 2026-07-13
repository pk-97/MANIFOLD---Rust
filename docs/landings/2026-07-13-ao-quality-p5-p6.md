# CINEMATIC_POST P6 (bilateral_blur AO denoise) + P5 (ssao_gtao) — landed 2026-07-13 @ `3e774a36`

**Branch:** `feat/ao-denoise-gtao` · **Level reached:** L2 (amended §4 demo rule — numeric gates + a looked-at PNG; Peter's own look-pass still owed, see VD-020) / target L2
**Doc status line (quoted verbatim):** "IN PROGRESS — P0–P6 SHIPPED · Sonnet 5 · ... **P6 SHIPPED 2026-07-13 (Sonnet 5, AO-quality lane, `feat/ao-denoise-gtao`)** ... **P5 SHIPPED 2026-07-13 (Sonnet 5, same lane)** ... **Peter's own look-pass on both P5 and P6 (§4's demo rule) is still owed** — orchestrator-level PNG review found no defects, but that is not a substitute for his verdict."

## Summary

Two fresh workers, one per phase, in worktree `draguni-design` (pool-reused dir; branch `feat/ao-denoise-gtao`), base-verified at `80cb8529`. Both phases' gates were independently re-run by the orchestrating session (never trusted from worker self-report), then the branch was merged with `origin/main` (which had advanced to `de121ba1` mid-lane — one real conflict in `docs/BUG_BACKLOG.md`'s index table, resolved: both branches had independently found and logged the same `BUG-144`, kept origin's richer version and cross-referenced this lane's independent confirmation), full workspace-gated, and landed to `main` as one `--no-ff` merge (`3e774a36`), pushed clean.

**P6 — `node.bilateral_blur` (D8):** depth-guided 9-tap separable blur pair inserted between `ssao_from_depth`/`ssao_gtao` and its mix in `CinematicScene`'s `ao` node-group. `MultiInputCoincident`, `[Gather, GatherTexel]`, codegen-path (`standalone_for_spec`, generated-vs-hand parity proven). Fusion: 15→17 estimated dispatches (bilat_v joins the pre-existing mix region; bilat_h stays isolated — a Gather input can never fuse with its producer, the design's own named cost).

**P5 — `node.ssao_gtao` (D9):** replaces `node.ssao_from_depth` outright (file deleted, not paralleled). 2-slice/4-step-per-side horizon-angle integral, transcribing the retired atom's view-space reconstruction and radius-to-screen-space projection verbatim. Load-migration resolved via `manifold_core::type_id_migration::TYPE_ID_MIGRATIONS` (the real node-typeId choke point — NOT `manifold-io`'s top-level `PresetInstance.effectType` walker, which the phase brief's own precedent pointer turned out to name the wrong layer for; that walker never reaches nested graph-node typeIds), extended to also drop params (`bias`) the successor doesn't declare. Fusion dispatch count unchanged (17 — pure retype).

## Gate results (verbatim, orchestrator-run, post-merge in the main checkout)

```
$ cargo clippy --workspace -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.65s
(clean, no warnings)

$ cargo nextest run --workspace   [pre-merge, in worktree, post origin/main merge]
     Summary [  13.046s] 3303 tests run: 3303 passed (1 leaky), 9 skipped

$ cargo deny check bans
bans ok

$ cargo test -p manifold-renderer --features gpu-proofs bilateral_blur -- --test-threads=1
test result: ok. 11 passed; 0 failed

$ cargo test -p manifold-renderer --features gpu-proofs ssao_gtao -- --test-threads=1
test result: ok. 12 passed; 0 failed

$ cargo test -p manifold-renderer ssao_from_depth_migrates_to_gtao
test result: ok. 1 passed; 0 failed

$ rg 'create_compute_pipeline\(include_str' bilateral_blur.rs
(0 hits)

$ rg 'ssao_from_depth' crates/ assets/
(only migration-table entry + doc/comment cross-references in bilateral_blur.rs,
 bokeh_gather.rs, motion_blur.rs, ssao_gtao.rs's own history notes — no live code path)

$ graph-tool validate CinematicScene.json --kind generator
OK  (zero warnings, both phases)

$ graph-tool fusion CinematicScene.json
before lane: 15 nodes/dispatches → after P6: 17 → after P5: 17 (pure retype, unchanged)

$ check-presets
57 presets, 57 ok

$ python3 .claude/hooks/bug_status.py --check
bug-backlog status: clean  (after adding an explicit Status: line to BUG-145 —
first version tripped BUG-134's own known FIXED-regex false positive on the
words "found not fixed" in the heading)

$ python3 .claude/hooks/design_status_check.py origin/main HEAD
(clean)
```

Full crate gpu-proofs sweep (`cargo test -p manifold-renderer --features gpu-proofs --lib`, run inside both phase worktrees): 1502-1503/150x passed, 2 pre-existing failures both times — `gltf_texture_source`/`render_scene` shared-pipeline-cache order-dependent tests. This is `BUG-144`, already tracked from two prior landings (BUG-111, BUG-054); this lane's independent reproduction on the unmodified pre-lane tip is now cross-referenced into that entry rather than re-logged.

## Deviations from brief

- The phase brief's own VERIFY-AT-IMPL pointer for P5's migration mechanism (`manifold-io/src/migrate.rs`'s `WireframeDepthGraph` precedent) named the wrong layer — that walker operates on top-level `PresetInstance.effectType`, never on nested graph-node `typeId`s. The P5 worker found the actual mechanism (`manifold_core::type_id_migration`) itself, as the brief's own text anticipated it might need to ("this is exactly the kind of judgment call the design doc asks you to make, not something to guess at").
- P6's brief asked for a preset path under `assets/effect-presets/`; `CinematicScene.json` actually lives under `assets/generator-presets/` — a stale path in the orchestrator's brief, corrected by both workers without incident (`--kind generator`, not `--kind effect`).
- `origin/main` advanced by ~19 merge commits mid-lane (unrelated waves: media-export bugs, widget-p8, mech-bugs). One real merge conflict (BUG_BACKLOG.md index table, both sides independently logging the same BUG-144) — resolved by the orchestrating session per the merge-trunk protocol, not by either worker.

## Shortcuts confessed (rolled up from phase reports)

- P6: none.
- P5: none. (Two real bugs found in the worker's OWN new code were fixed in-session, not shipped as debt: a WGSL `vec2<i32>(f32,f32)` invalid-construction error, and a copy-pasted hash-constant typo inherited from the now-deleted `ssao_from_depth.rs`'s own CPU reference.)
- Orchestrator: BUG-145 (bokeh_gather.rs dead-code, pre-existing) confirmed systemic — the identical shape reappeared in this lane's own new `bilateral_blur.rs` the moment it compiled post-merge in the main checkout. Logged as an amendment to BUG-145 rather than fixed (mechanical, out of scope for a landing pass; a repo-wide sweep is the right unit of work, not a drive-by edit).

## Verification debt

VD-020 opened — Peter's look-pass on the P5/P6 PNG pairs (§4's amended demo rule) is the real exit and hasn't run yet. Nothing else carried; both phases' numeric gates are complete and green.

## Tool feedback (first-live-test telemetry, both workers + orchestrator independently hit this)

- `graph-tool`, `check-presets`, `render-generator-preset`, `gen_node_catalog` are all hyphenated binary names; the design doc's own prose and CLAUDE.md write several of them with underscores (`graph_tool`, `check_presets`). Every session that followed the doc's literal invocation burned a failed command first. Worth a doc-wide sed pass.
- `graph_tool fusion`'s per-node `cut:` reason lines are genuinely legible — both workers and the orchestrator correctly predicted region membership from reading the tool's own explanation text before running it, and the tool confirmed the prediction both times. No false positives, no unclear messages encountered on either preset edit.
- `bug_status.py`'s known BUG-134 (FIXED-regex false positive on "found not fixed") bit a fresh backlog entry live in this same landing — direct confirmation the bug is real and still open, not just a hypothetical from its own write-up.

## Click-script for Peter (≤2 minutes)

1. Open `CinematicScene` (generator preset) in the app, or view `/tmp/ao-quality-p5-before.png` / `-after.png` and `/tmp/ao-quality-p6-before.png` / `-after.png` (rendered in a worktree — may need re-rendering from current main to view live) — expect: P6 pair looks visually identical (demo scene is a flat plane, near-zero real occlusion, so the denoise pass has nothing visible to smooth — this is expected, not a defect); P5 pair shows GTAO (after) darkening the plane's silhouette edge more than SSAO (before) did.
2. Drag `ssao_intensity` on the AO card while the generator is live — expect: contact-weight darkening scales with the fader, same performer gesture as before (GTAO reuses the same card/param names, D9's stated compatibility contract).
3. Load a project saved before this session (or any bundled preset carrying the old `ssao_from_depth` type id, if one exists) — expect: it loads without error, the node resolves to `node.ssao_gtao`, and `radius`/`intensity` read the old values (the migration round-trip test proves this in isolation; this step is the live-app confirmation).
4. The real ask: **look at the AO on a scene with actual depth discontinuities** (not the flat-plane demo) and say whether GTAO's edge-darkening (D9's named honest cost — no thickness heuristic in v1) reads as acceptable or needs a follow-up decision.
