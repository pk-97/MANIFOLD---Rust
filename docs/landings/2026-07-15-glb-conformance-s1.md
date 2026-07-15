# GLB_CONFORMANCE G-P1+G-P2 — landed 2026-07-15 @ `909976d2`

**Branch:** `wave/glb-conformance-s1` · **Level reached:** L2 / target L2 (§10)
**Doc status line (quoted verbatim):** `Status: IN PROGRESS · 2026-07-15 · Fable 5 (authored) + Sonnet 5 (G-P1+G-P2 executed and landed same day, `909976d2`). G-P1 (conformance harness) + G-P2 (cap deleted, import is 1:1, BUG-163 fixed as a side effect) SHIPPED. G-P3–G-P7 not yet executed.`

## Gate results (verbatim)

Full-workspace sweep, run in the worktree (`slot-2`, `wave/glb-conformance-s1`) with `origin/main`
already merged in, before the merge into main:

```
$ cargo clippy --workspace -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.59s
(clean — 0 warnings beyond pre-existing manifold-media ObjC deprecation notices)

$ cargo nextest run --workspace
     Summary [   9.099s] 3386 tests run: 3386 passed, 12 skipped

$ cargo deny check bans
bans ok

$ cargo run -p manifold-renderer --bin check-presets
57 presets: 57 ok, 0 failed (0.24s)

$ cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs -- --test-threads=1
test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 11.84s
```

Per-phase gates (both independently re-run by the orchestrator, not just worker self-report):

```
G-P1: bash scripts/fetch-gltf-conformance.sh && cargo test -p manifold-renderer \
      --features gpu-proofs --test glb_conformance -- --test-threads=1
  → glb conformance summary: 4 expect_pass checked, 4 xfail, 0 skipped, 0 failures

G-P1: cargo run -p manifold-renderer --bin render-import -- \
      tests/fixtures/gltf/DamagedHelmet.glb --out /tmp/helmet_orch.png
  → exit 0, converged on frame 6 (non-black fraction 0.1339) — orchestrator viewed the PNG

G-P1 negative gate: rg -n "reinhard|/ \(1\.0 \+ v\)" crates/manifold-renderer/src/bin/ \
      crates/manifold-renderer/tests/
  → zero hits in touched files (8 pre-existing hits elsewhere, confirmed via git stash
    to predate 15089f5d, unrelated per-primitive value-parity tests)

G-P1 held-out: cargo run -p manifold-renderer --bin render-import -- \
      /tmp/boombox_holdout.glb --out /tmp/boombox_holdout.png
  → exit 2 (never converged) — a defined clean exit per the CLI contract, not a panic,
    but the substance (textures_wired: 1 of 4) is a real gap — logged as BUG-165

G-P2: rg -n "dropped_over_cap" crates/
  → zero hits

G-P2: DamagedHelmet golden unchanged — glb_conformance_sweep's
      golden mean_abs_diff = 0.0000 (tol 2) for all four expect_pass assets

G-P2 AMG re-render (orchestrator, real fixture from the main checkout, read-only):
  cargo run -p manifold-renderer --bin render-import -- \
      ".../tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb" \
      --out /tmp/amg_after_gp2.png --frames-max 400
  → material_count: 78, object_count: 78, textures_wired: 39 (was 29, dropped_over_cap: 14
    pre-fix), converged frame 8, non-black fraction 0.0877 — orchestrator viewed the PNG,
    body panels now show the correct silver/NASA livery (BUG-163 closed on this evidence)

G-P2 held-out (orchestrator, independent of the worker's ToyCar check):
  fetched AntiqueCamera.glb ad hoc (2 materials) from the same pinned Khronos commit
  → material_count: 2, object_count: 2, converged frame 4 — orchestrator viewed the PNG
```

## Deviations from brief

- **G-P1's manifest expect_pass/xfail split.** The design doc's literal G-P1 bullet ("the
  first four `expect_pass`... the rest `xfail`") put `TextureTransformTest` and
  `ClearCoatTest` in the pass bucket, contradicting the same doc's own G-P4/G-P5 entry-state
  text and D5's phasing. The worker resolved this using the doc's own cross-references plus
  real renders of all seven fetchable assets, and corrected the bullet in place with the
  reasoning (see `docs/GLB_CONFORMANCE_DESIGN.md` G-P1 section). Verified sound by the
  orchestrator before accepting.
- **`TextureTransformTest` has no glTF-Binary variant** at the pinned Khronos commit
  (`2bac6f8c`) — JSON+bin+textures only. The fetch script skips it; the manifest classifies
  it `xfail:G-P4` with a note. G-P4 either fetches the multi-file variant or waits for
  Khronos to publish one.
- **New finding, not in the brief:** `TextureSettingsTest` renders non-degenerate but the
  importer's single shared REPEAT sampler means per-texture wrap/filter settings are never
  honored — logged as BUG-164, classified `xfail:BUG-164`.
- **G-P2's held-out asset:** the brief suggested `SciFiHelmet`, which (like
  `TextureTransformTest`) has no glTF-Binary variant at the pinned commit. The worker
  substituted `ToyCar` (3 materials); the orchestrator independently ran a second held-out
  asset, `AntiqueCamera` (2 materials), for a check not sourced from the worker's own choice.
- **AMG before/after:** G-P2's worker could not run the AMG-specific gate — the untracked
  fixture wasn't copied into the worktree by `agent-worktree.py acquire` (0 gitignored files
  copied this session). The orchestrator ran it directly against the real fixture, reading
  it from the main checkout path (no write), confirming `object_count == 78` and the visual
  fix.
- **Landing-protocol correction mid-session:** the orchestrator initially attempted
  `git merge --no-ff` into main before running the full-workspace gate there, which the
  auto-mode classifier correctly blocked. Corrected to the documented order (gate on the
  branch with `origin/main` merged in, then merge, then push) per
  `.claude/GIT_TREE_DISCIPLINE.md` §2.

## Shortcuts confessed (rolled up from phase reports)

G-P1: none. G-P2: none — the one gap (AMG-specific verification) was explicitly named by the
worker as left to the orchestrator, not concealed.

## Verification debt

VD-021 opened: Peter's look-pass on the AMG livery fix and the card-curation UI (16-slider
cap, never screenshotted in the actual inspector panel) is owed — orchestrator-level L2
reached, L4 (Peter, live) is the target. BUG-165 (BoomBox held-out convergence failure,
`textures_wired: 1/4`) logged separately, triage owed, not blocking this landing.

## Click-script for Peter (≤2 minutes)

1. Drop `tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb` into a set (or run
   `cargo run -p manifold-renderer --bin render-import -- tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb --out /tmp/amg.png`
   and open `/tmp/amg.png`) — expect: the body panels show the silver/NASA livery, not black.
2. Open the import card's inspector for that asset — expect: exactly 16 per-object opacity
   sliders shown (largest by vertex count), not 78, while every panel still renders (curation
   is UI-only).
3. Run `cargo run -p manifold-renderer --bin render-import -- tests/fixtures/gltf/DamagedHelmet.glb --out /tmp/helmet.png`
   and open it — expect: clean PBR shading, visible emissive glow in the visor, no
   texture smearing at the UV seams.
