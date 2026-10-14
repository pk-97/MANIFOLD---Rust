# Git tree discipline

Rules for the shared main checkout, worktrees and landing on main. Hooks are
the spec where they enforce a rule; this doc holds the rationale and the
incidents behind them.

## Incident 1 (section 1's origin)

Two live sessions shared the main checkout; one switched branches and
fast-forward merged while the other had uncommitted file moves in flight. The
merge resurrected the moved files' old paths and the second session's commit
landed on a branch it never chose. Full incident: `88257631` commit message +
the hazards section of the `agent-execution-playbook` memory.

Root cause: N concurrent sessions, one HEAD. Branch work therefore lives in
worktrees, never in the shared checkout.

## Incident 2 (section 2's origin — the ff-only model itself was unsatisfiable)

The original section 2 said main is fast-forward-only: a workstream lands with
`git branch -f main <tip> && git push origin <tip>:main`. That assumed one
integrator lands at a time. It broke under N orchestrator sessions finishing
at different times (plus docs commits landing on main directly between them)
— a clean fast-forward was never actually possible, so every finishing
session improvised its own landing. The observed result was **twin commits**:
the same content merged onto main once and onto `feat/timeline-ui-redesign`
again, under different SHAs.

Root cause: a single-integrator model applied to a multi-integrator reality.
Main became a merge-based trunk (section 2).

## 1. Shared-checkout guard (retired)

The pidfile-based warning on a main-checkout branch switch is gone with the
daemon that wrote the pidfiles. The Incident-1 hazard (two sessions sharing
one HEAD) stays mitigated by the worktree slot ring (`scripts/agent-worktree.py`
— branch work lives in worktrees, not the shared checkout), the merge-trunk
landing protocol (section 2) and `detect_unverified_compound_landing_merge`.

## 1b. Landing-protocol guard

Obsolete when: landing moves to a server-side gate (CI-enforced merge checks) — a local hook guarding a shared checkout has nothing left to guard.

`.claude/hooks/preToolUseBash.py` is the spec (`landing_protocol_guard`,
`land_pass_guard`, `detect_unverified_compound_landing_merge`). In the main
checkout, `git branch -f main` and any force-push to main ask unconditionally;
a merge or push that lands on main without a recorded `land_branch.py` gate
pass is denied unless it touches only `.beads/`, `docs/` and `.claude/hooks/`;
an allowed one carries a reminder pointing at `scripts/land_branch.py`.
Worktree-targeted commands and merges of main into a branch are unaffected.

## 2. Landing protocol

Obsolete when: main stops being a locally-landed shared trunk (PR/CI-gated merges) — the protocol's invariants would then be server-enforced.

- **Main is the merge-based trunk.** No separate integration branch — two
  landing spots is what produced the twins. "Last-known-good" is a property
  of the gate, not of linearity.
- **Lanes commit once and stop; the lead commits, pushes and lands**
  (CLAUDE.md section Hard rules). Workers never land.
- **Land with `scripts/land_branch.py`.** One command after the lead's
  review: `scripts/land_branch.py <branch> --worktree <path> --message '...'
  [--close-bead <id> ...]`. It fetches, merges current `origin/main` into the
  branch, runs `scripts/landing_gate.py --repo <worktree path>`, merges
  `--no-ff` into main, closes the beads, pushes and deletes the landed branch,
  with per-step exit checks. The lead reviews and makes the named-red call;
  push happens only after a green gate or an explicit `--named-red BUG-id
  --reason` over a gate that ran every check (`landing_gate.py` exit
  `CHECKS_RED`; a refusal or crash never lands). Merge, verdict and
  bead-close commits and the one push run under a landing lock shared by
  every worktree; gates never do. If main moved, the branch takes it and
  re-gates outside the lock; a named red carries over only when that re-gate
  ran every check and went red on exactly the same checks. Each attempt logs
  one row to `.claude/orchestration/landing-runs.jsonl`.
- **Several ready branches.** Land each with `land_branch.py`.
  `scripts/land_wave.py --batch` (merge them in queue order, gate once, drop a
  red culprit by name, land one merge listing every branch and tip) is a
  fallback for a pile-up, not the standard path; its module docstring is the
  spec.
- **The gate.** It collects cheap reds before flows and GPU proofs. Any
  cheap red stops at that boundary; every red prints its `rerun:` command.
  `--fail-fast` stops at the first red; `--keep-going` is an explicit
  diagnostic run. A named red never waives an unrun leg. Fix the reds with
  their printed commands.
  Follow background runs with `scripts/watch_land.py --pid PID --log PATH`
  (`--kind land` for `land_branch.py`). It reads the active leg's live transcript
  and exits on a red, refusal, hang or completion; `--hang-seconds` sets silence
  tolerance. SIGINT/SIGTERM stop children and preserve an incomplete log.
  Campaigns defer nightly work with `scripts/gpu_queue.py reserve --owner NAME
  --reason TEXT --seconds N`; `clear` releases it early. Scripts are the spec.
  The gate script itself is the
  whole landing gate and gates only what
  the branch touched: design-status housekeeping
  (`.claude/hooks/design_status_check.py origin/main HEAD`, so status lines
  land in the same merge as the code), docs-index freshness when docs were
  added/renamed (`scripts/gen_docs_index.py`), the UI flow gate
  (`scripts/run_ui_flows.py --touched` — path-scoped via the flow manifest's
  `path_triggers`; added after BUG-313 (drag flow red and unrun) and
  hook-enforced: the run writes `.claude/orchestration/flow-gate-marker.json`
  in the main checkout and `preToolUseBash.py` (`flow_gate_guard`) denies a
  merge into main whose branch touches flow-mapped paths without a green
  marker at that exact tip), `cargo deny check bans`, changed-code clippy
  `--tests` + module-scoped nextest filtersets (including sibling test modules
  and mapped integration binaries), and `scripts/gpu_proofs_gate.py` when
  the diff touches GPU paths (scoped: mapped tests + smoke set from
  `scripts/gpu_scope.py`, unmapped GPU path fails, `glb_conformance` only for
  glTF paths; the whole suite is `--all`, nightly).
  Comment/blank-only and docs-only diffs skip the build, clippy, CPU-test, and
  GPU-proof legs. The selected CPU tests retain the GPU lock because device use
  cannot be known cheaply; builds run outside that hold. Full touched-crate and
  workspace CPU suites run nightly through `trunk_health.py`.
  Successful legs are stored in the Git common directory, shared by every
  slot. Keys cover content, dependency crates, test selection, features,
  toolchain, environment, fixtures and gate scripts. An unchanged leg prints
  `[REUSED]` with its original commit and time. Failed or unidentifiable legs
  run again. Standalone proof-gate runs and equivalent serial cargo proof
  commands through `gpu_queue.py` share these records. Only exact selections
  match; a different filter, target or skip list needs its own pass. Raw cargo
  runs longer than the minimum hang allowance need the instrumented proof gate
  before they can count as landing passes.
  A fully reused test set needs no build or GPU lock. Build results alone
  are not reusable after a slot cache is reclaimed. Nightly sweeps never reuse.
  The gate requires a clean committed tree and refreshes the existing verdict
  and flow marker after reuse. `land_branch.py` checks the proposed merge tree
  before committing it. If main changed that tree, it merges main into the
  slot and gates again; only changed inputs need new checks.
- **The GPU-proofs time budget is a warning, never a red.** Scoped runs never
  drop tests by duration. When passing tests exceed the landing budget,
  `gpu_proofs_gate.py` prints `GPU-PROOFS BUDGET: OVER` and the exit code is
  unchanged, so a green gate lands as usual. Speed up proofs only when a run
  is many times the budget. Hangs, failed tests, drifted goldens and a new
  heavy proof without a reviewed allowance in `scripts/gpu_test_times.json`
  stay red. Nightly `trunk_health.py` runs `--all --record-times
  /tmp/gpu_test_times.nightly.json` and prints the diff; it never commits. To
  refresh allowances, copy that file over `scripts/gpu_test_times.json` on a
  branch and land it. The post-merge housekeeper on main is a backstop, not
  the workflow — its remedies are worktree-shaped, never in-place edits to
  main.
- **The workspace-wide sweep is NOT a landing item.** Lanes verify their
  branches; the only merge-time risk is the touched-crate collision with fresh
  main, and the touched-crate gate covers it. Workspace clippy `--tests`,
  `cargo nextest run --workspace`, and `scripts/feature_matrix.py`
  (non-default features are invisible to the workspace sweep and rot without
  it, FOUNDATIONAL_GAPS.md A7 (feature-matrix build rot)) run nightly on main
  instead: `scripts/trunk_health.py` (launchd `com.manifold.trunk-health`,
  04:30), which files a P1 `trunk-health` bead per red gate. Cross-crate rot
  surfaces next morning as a bead, never as landing ceremony.
- **Perf soak is NOT a landing-gate item.** It burns time and GPU on every
  render-path landing, and the relative baseline is noisy on a loaded
  machine. `cargo xtask perf-soak` (PERF_BUDGET_GATE_DESIGN.md P3) is a
  deliberate, on-demand tool: run it when a change is suspected of frame cost
  (profiling beads, hot-loop rewrites) or when Peter asks — never as landing
  ceremony.
- **Visual checks are NOT landing items.** The gates are the gates. A
  headless render verifies work during review; Peter's eye is a stage check
  after landing.
- **Supersession sweep** (when a landing completes or supersedes a design
  phase, bug, or named plan): update the design doc status header and close
  the bead, then `rg` the plan's name AND its stage labels across `docs/` and
  the memory directory and fix or tombstone every hit still asserting the old
  state. Supersession under a different name is the known killer
  (PRESET_INSTANCE_COLLAPSE absorbed BINDING_UNIFICATION's "B+/B++/C"; the
  stale memories cost two Fable sessions a month later). Full rule:
  `.claude/hooks/context-nudges/landing-protocol.md`.
- **Twin-killer 1:** never cherry-pick or re-commit content that already
  exists as commits on a live branch — merge the branch so SHAs stay shared.
  The one sanctioned exception: lifting the last unique commits of a branch
  that is retired immediately afterward, so no second lineage survives.
- **Twin-killer 2:** never delete a branch until `git merge-base
  --is-ancestor <tip> origin/main` confirms its commits are on main.
- `git branch -f main <tip>` and force-pushes to main are anti-patterns
  (section 1b asks before either).
- A session doing sustained code work still gets a LONG-LIVED worktree slot
  for the workstream, acquired off a verified tip (the playbook's step-0
  base-verification guard) with gitignored fixtures copied in; per-session
  worktrees pay the cargo cold-build tax and are not the pattern. Acquire
  through `scripts/agent-worktree.py` (section 2c) — the ONLY sanctioned source
  (raw `git worktree add` is hook-denied since the 455 GB disk incident). It
  reuses an idle warm slot before it ever creates a cold one.
- The main checkout is the only place merges to main happen. Sessions in
  worktrees never run bare git/cargo (always `-C` / `--manifest-path`,
  absolute, quoted — the repo path contains a space).
- **Never use the Agent tool's built-in `isolation: "worktree"` for repo
  work** — it bases the worktree off the default branch, not your tip, and
  bypasses the slot ring's cap. Hook-denied (`agent-worktree-isolation-guard.py`),
  as is raw `git worktree add` (`preToolUseBash.py` `worktree_add_guard`).
  Worktrees come from `scripts/agent-worktree.py acquire` only, with the
  step-0 base-verification guard in the brief.

## 2c. Build-speed rules

Obsolete when: the build system changes (sccache removed, workspace split, non-cargo builds) — re-measure before carrying any rule over.

Measured basis: ~80% of a phase's wall-clock is cargo compile/test, and ten
live worktrees once carried 5–41 GB of cold-built `target/` each.

1. **Reuse warm slots — the pool is a capped ring** (19 per-task worktrees ×
   15–60 GB targets = 455 GB once filled the disk).
   `scripts/agent-worktree.py acquire <task-label> <branch>
   [--tip REF] [--owner TEXT]` re-points the warmest idle slot with
   `checkout -B`; slots are anonymous (`slot-0`…`slot-9`, task label lives
   in the lease), capped at 10 with no override (worst case ~270 GB fully
   warm) — all-busy is a loud `POOL FULL` error to surface to Peter, never to
   work around. A slot's
   `target/` past 25 GB is wiped at acquire. Idle = clean status + HEAD
   is-ancestor of origin/main + no lease. A lease holds its slot until
   `release`, whatever the git state; it never expires (an agent between
   tool calls looks clean and landed). The wipe is
   `scripts/storage_budget.py`'s file manifest: Cargo-named rlibs, objects,
   fingerprints, incremental state and hashed native executables
   (`deps/<target>-<16 hex>`, the bulk of a warm slot at ~55 MB per test
   binary); build-script `out/` products are kept because Cargo would not
   rerun the script if they vanished. The landing gate's storage admission
   (`storage_budget.MAINTENANCE_GOAL_BYTES`, 30 GiB of raw free space) runs `agent-worktree.py reclaim` before refusing — it
   frees landed, clean, lease-free, process-free slots only, LRU first,
   until the reserve is met; dirty, unlanded and main caches are never
   touched by it. The script
   writes a lease, copies missing GITIGNORED `tests/fixtures` files from
   the main checkout (non-ignored files would mark the slot permanently
   dirty — the exact bug that grew the old pool), and prints the step-0
   base-verification line — the caller still confirms the tip. `list`
   shows the ring; `release <slot-name>` (as printed by acquire) drops the
   lease at session end. A WORKTREE_HANDOFF.md or any dirt marks a slot
   busy. Raw `git worktree add` is hook-denied.
2. **sccache wraps rustc globally** (`.cargo/config.toml`). External deps are
   compiled non-incrementally by cargo, so they cache across worktrees and
   survive wiped targets; workspace crates stay incremental and pass through.
   If sccache misbehaves, comment out the `rustc-wrapper` line — plain rustc
   is the unchanged fallback.
3. **Batch landings.** Commits stay per-phase on the branch (durability
   unchanged); `land_branch.py` runs once per 2–3 phases per design, not per
   phase. Each landing skipped saves a gate rerun here and a lock wait for
   every other concurrent session.
4. **Prewarm during briefing.** The moment a worktree is acquired for a code
   wave, kick the phase's primary build (`cargo build -p <crate> [--features
   …] --manifest-path "<wt>/Cargo.toml"`) in the background, then write the
   worker briefs — the build runs while the worker reads its docs. Free
   overlap on every cold or profile-invalidated start.
5. **Test runner + cache size.** CPU-focused gates run
   `cargo nextest` with filtersets for changed modules and mapped integration
   binaries; GPU-proofs suites STAY on
   `cargo test` — the in-process `test_device` lock is the device serializer
   and nextest's process-per-test model would defeat it.
   `SCCACHE_CACHE_SIZE = "30G"` (`.cargo/config.toml` `[env]`) stops
   dep-cache eviction; the server picks it up at its next idle restart.
   (The dev profile is already build-speed-tuned — `debug =
   "line-tables-only"` + `split-debuginfo = "unpacked"`, root Cargo.toml —
   don't re-add those.) `cargo deny check bans` (<1s) is part of the landing
   gate — `deny.toml` is the actual enforcement of the wgpu/metal dependency
   bans.

## 3. Already in force (no work)

Commit fast — never sit on uncommitted renames/deletions while other sessions
or agents run; read the branch off the commit OUTPUT, not session start; diff
resurrected files against their new-path versions before deleting (a merge,
not an agent, may have restored them).

## 3b. Shared-checkout commit mechanics

Obsolete when: sessions stop sharing the main checkout's index (all work in per-session worktrees) — the pathspec discipline exists only because the index is shared.

- **Commit with a pathspec.** `git commit -m '…' -- <paths>`, never the
  index (hook-denied: `index_add_guard`). A pathspec commit fails with
  "pathspec did not match" for a NEW file — git cannot pathspec-commit what
  it isn't tracking. The correct move: `git add -- <exactly the new paths>`
  then the pathspec commit as usual. The targeted add plus the commit's own
  pathspec still fences out other sessions' staged work. Never `add -A`,
  never `add .`, never a bare commit.
- **Inverse-sweep hazard.** The known hazard is YOUR add-then-commit sweeping
  THEIR staged work. The inverse also happens: another session's landing loop
  can commit files sitting unstaged/untracked in YOUR working tree. If your
  pathspec commit reports nothing to commit, diff your files against HEAD
  before assuming an error: byte-identity means another session landed them,
  and the landing is yours to verify, not redo.
- **Landing merges are standalone commands.** `land_branch.py` is the
  landing path. A manual landing merge never runs inside a compound chain —
  HEAD can change between a chain's steps in a shared checkout (a
  fetch→merge→push chain once landed a merge on another session's branch).
  Re-verify `git branch --show-current` immediately before the merge step,
  as its own command (hook-denied otherwise:
  `detect_unverified_compound_landing_merge`).
- **Worktree handoff files.** A session stopping mid-work in a worktree
  (compaction, budget, interruption) writes its handoff to
  `<worktree>/WORKTREE_HANDOFF.md` — branch state, uncommitted diagnostics,
  the finding in flight — not only to its final chat message. A successor
  session can find a file; it cannot find a transcript message.
