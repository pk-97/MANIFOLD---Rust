# mechbugs wave (7 backlog bugs) — landed 2026-07-13 @ `9dd7c8be` (batch 1), `84efb321` (batch 2)

**Branch:** feat/mech-bugs-0712 (worktree `.claude/worktrees/mechbugs`) · **Level reached:** L1 across the wave, L2 for BUG-111 and BUG-037 (below) / target L1–L2 per bug (§10)
**Doc status line (quoted verbatim):** not applicable — this wave closes `docs/BUG_BACKLOG.md` entries, not a phased design doc. Backlog Status lines quoted per-bug below.

Seven bugs from `docs/BUG_BACKLOG.md`: BUG-123, BUG-111, BUG-076, BUG-037, BUG-079, BUG-038, BUG-054. Four workers (Sonnet, model-tagged per the Agent tool's launch requirement), 2–3 bugs each, all edits in one shared worktree, orchestrator (this session) ran every gate itself — independently re-executed, not just read from worker reports, per the session's own verify-claim discipline (see Deviations).

## Per-bug outcome and quoted Status line

- **BUG-123 (mesh-edges-capacity-vs-active-count) — FIXED @ `1b854d45`.** `**Status:** FIXED @ `1b854d45` — added an optional `active_count` scalar input (port-shadow, mirrors `node.range`'s convention) that overrides the buffer-capacity-derived vertex count when wired; unwired graphs are unaffected. 5 new tests in `edges_from_mesh.rs`.`
- **BUG-111 (fused-segment-inner-override-noop) — FIXED @ `d73b3e36`.** `**Status:** FIXED @ `d73b3e36` — `EffectSlot::card_prefix` ... threads through a new `BoundGraph::apply_inner_overrides_prefixed` ... New gpu-proofs test `fused_segment_inner_override_reaches_live_kernel` ... independently reconfirmed red (`over=0/65536`) on the pre-fix code path and green with the fix restored.`
- **BUG-076 (inspector-scroll-underestimates-content-height) — still OPEN.** `**Status:** OPEN — 2026-07-13 (`8d37d5e0`): the drawer-tween undercounting theory below was built and tested ... and RULED OUT ... Root cause remains open.` No fix shipped — the pre-decided fix shape didn't match the real code on contact, and the worker correctly stopped rather than forcing one.
- **BUG-037 (glp-first-render-stall) — PARTIAL @ `dea66221`/`7fdf25d0`.** `**Status:** PARTIAL — `render_scene`'s and `gltf_texture_source`'s GPU pipeline compiles ... are now prewarmed at app startup ... shows a real, repeatable ~37% reduction on frame 0 (308.5ms → 194.5ms) — but does NOT bring this preset's frame 0 under the 20ms bar ... Remaining gap ... owed to a future session.`
- **BUG-079 (missing-preset-fails-silently-no-onscreen-signal) — FIXED @ `834fdaa6`.** `**Status:** FIXED @ `834fdaa6` — reuses the BUG-063 P3 `LoadReport`/"opened with repairs" toast mechanism ... No new notification mechanism.`
- **BUG-038 (ableton-log-spam) — FIXED @ `06bfd879`.** `**Status:** FIXED @ `06bfd879` — warns once on first OSC-send failure, downgrades repeats to DEBUG, logs a single INFO "reconnected" on the next success.`
- **BUG-054 (renderer-device-ptr-dangles) — FIXED @ `d447ec8d`.** `**Status:** FIXED @ `d447ec8d` — `Arc<GpuDevice>` ... replaces the cached raw pointer end-to-end ... Beyond the three renderers named above, `MetalBackend` also cached the same raw pointer and needed the same migration ... Negative gate: rg '\*const GpuDevice' crates/` — zero code hits.`

Two new bugs logged in the same wave (found while gating, not introduced by any fix): **BUG-144** (two BUG-037 prewarm gpu-proofs tests are order-dependent under the full in-crate suite, pass standalone) and, from an earlier gate run, a since-superseded numbering collision resolved during a merge (see Deviations).

## Gate results (verbatim, most recent full run before each landing)

Batch 1 (BUG-123/076/038/079/037), full workspace, after final merge of origin/main:
```
Summary [   9.375s] 3216 tests run: 3216 passed, 8 skipped
```
```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.93s   (clippy --workspace -- -D warnings, clean)
```
```
bans ok   (cargo deny check bans)
bug-backlog status: clean   (bug_status.py)
```

Batch 2 (BUG-111/054), full workspace, after final merge of origin/main:
```
Summary [  12.319s] 3275 tests run: 3275 passed, 8 skipped
```
```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.71s   (clippy --workspace -- -D warnings, clean)
```
```
bans ok
bug-backlog status: clean
```

BUG-111's new gpu-proofs test, independently re-executed by the orchestrator (not just read from the worker's report):
```
test preset_runtime::chain_fusion_tests::fused_segment_inner_override_reaches_live_kernel ... ok
```
and, with the two fix lines temporarily reverted (test kept):
```
thread '...fused_segment_inner_override_reaches_live_kernel' panicked at ...:
an inner-param edit on a fused SEGMENT member must reach the live kernel (BUG-111) ...
max_abs=0, over=0/65536
```
— exact match to the worker's claimed red-before evidence.

BUG-054's full gpu-proofs suite, independently re-executed:
```
cargo test -p manifold-renderer --features gpu-proofs --lib
test result: FAILED. 1488 passed; 2 failed ...
(the 2 failures are BUG-144's known prewarm order-dependency, standalone-confirmed passing)
cargo test -p manifold-renderer --test gpu_proofs --features gpu-proofs
test result: ok. 27 passed; 0 failed
```

## Deviations from brief

- **BUG-037's original verification attempt was insufficient and was sent back.** The worker first substituted gpu-proofs cache-hit tests for the brief's explicit ask (a live `MANIFOLD_RENDER_TRACE=1` run), citing "no gitignored fixture" and "no display" — both wrong (the `.glb` fixtures are tracked and present; the existing `bug035_verify.rs` pattern needs no display). Resumed the same worker with the corrected facts; it built the live harness and reported an honest, less-flattering number than its first pass (a real but partial improvement, not a closed bug). Landed as PARTIAL, not FIXED.
- **BUG-076's pre-decided fix shape didn't apply.** The backlog's hypothesized root cause (drawer-tween undercounting) was tested directly and ruled out on the real code — `drawer_height_anim` was already snapping to target on first configure for the single-configure path. No fix was forced; the bug stays OPEN with the ruled-out theory recorded so no future session re-tries it.
- **BUG-054's scope grew beyond the three named renderers.** `MetalBackend` cached the same raw pointer and wasn't named in the backlog entry; the worker found it via the required `rg '\*const GpuDevice'` negative-gate search and migrated it too, since "class closed" was the actual bar, not "the three named structs." Approved by the brief's own framing (negative gate over named list).
- **A BUG-ID collision surfaced during the batch-2 merge**: this session's new bug (logged as BUG-143 in the worktree) collided with an unrelated concurrent session's BUG-143 (`macros-panel-ableton-trim-drag-outside-p7-inventory`) landed to origin/main in between merges. Resolved by renumbering this session's entry to BUG-144 during conflict resolution, keeping the other session's entry untouched — a real hazard of the current multi-session-concurrent-landing model, not a process error by this session.
- **origin/main advanced repeatedly during both landing sequences** (4 separate fetch→merge→re-gate cycles across the two batches, per the merge-trunk landing protocol) — every merge auto-resolved except the two `docs/BUG_BACKLOG.md` conflicts noted above, both hand-resolved and re-gated before landing.
- The orchestrator independently re-ran every gate a worker reported rather than trusting the report at face value (nextest, clippy, and — for BUG-111/BUG-054 specifically — the gpu-proofs suites and the red/green test evidence), per this session's own verify-claim discipline.

## Shortcuts confessed (rolled up from phase reports)

- BUG-037: none beyond the stated, disclosed gap — the fix only covers GPU pipeline/PSO compile prewarm, not the mesh/scatter/shadow-pass costs that dominate on a large preset; explicitly left as the bug's remaining scope, not silently narrowed.
- BUG-076: none — investigation only, no fix shipped, no shortcut to confess.
- All other bugs (123, 111, 079, 038, 054): none reported by workers, none found by the orchestrator's independent gate reruns.

## Verification debt

**VD-027 opened** — BUG-123 (mesh_edges active_count fix), BUG-038 (Ableton log throttle), and BUG-079 (missing-preset toast) all reached L1 (tests green) only; none were observed as L2 (an actual render/log/toast produced and looked at by a person). BUG-123 in particular is a visual-artifact fix (a bright dot at vertex 0) whose absence has not been confirmed on a real scene render. Burn-down: render a scene using `node.mesh_edges` with an oversized `max_capacity` before/after (or headless-PNG it), run a real session with Ableton absent and read the log for the once-then-debug pattern, and load a project with a genuinely unresolvable preset ref to see the toast fire. None of the three needs a live rig — all are reachable headless or via a short manual run; carried as debt because this session's worktree didn't have a path to run them without the live app.
BUG-111 and BUG-054 are NOT carried as debt: BUG-111 reached L2 via a real GPU texture-diff readback comparison read directly by the orchestrator (not just asserted green); BUG-054 is a plumbing/pointer-lifetime fix with no user-visible surface, so L1 (full gate + full gpu-proofs suite) is the correct target level, already met.
BUG-037 is PARTIAL, not FIXED, so no L-level debt is opened against it — the entry itself states the remaining gap plainly instead.

## Click-script for Peter (≤2 minutes)

1. Open a project with a `node.mesh_edges` card whose `max_capacity` is larger than the loaded mesh's vertex count (or wire the new `active_count` input on any existing one) — expect: no bright dot artifact at the origin; the mesh wireframe renders clean.
2. Quit Ableton Live (or don't start it), launch MANIFOLD, watch the console for ~10 seconds — expect: exactly one `[AbletonBridge] OSC send failed ...` WARN line, then silence (no repeat spam every 1.5s).
3. Load a project that references a preset type MANIFOLD doesn't have registered (e.g. rename a preset type id in a saved project.json before loading) — expect: a non-blocking "opened with repairs" toast naming the unresolved preset, not silent console-only logging.
