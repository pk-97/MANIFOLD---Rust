# Git tree discipline — build spec

Status: §1 shared-checkout guard BUILT 2026-07-04 (`6737cfe6`); §1b
landing-protocol guard IN PROGRESS 2026-07-04 (this session); §2 ff-only
model RETIRED 2026-07-04, replaced by the merge-trunk landing protocol below.

## Incident 1 (§1's origin)

Two live sessions shared the main checkout; one switched branches and
fast-forward merged while the other had uncommitted file moves in flight. The
merge resurrected the moved files' old paths and the second session's commit
landed on a branch it never chose. Full incident: `88257631` commit message +
the hazards section of the `agent-execution-playbook` memory.

Root cause: N concurrent sessions, one HEAD. The fix (§1) makes main-checkout
branch state owned and enforced, not remembered.

## Incident 2 (§2's origin — the ff-only model itself was unsatisfiable)

The original §2 said main is fast-forward-only: a workstream lands with
`git branch -f main <tip> && git push origin <tip>:main`. That assumed one
integrator lands at a time. It broke under N orchestrator sessions finishing
at different times (plus daemon/docs commits landing on main directly between
them) — a clean fast-forward was never actually possible, so every finishing
session improvised its own landing. The observed result was **twin commits**:
the same content merged onto main once and onto `feat/timeline-ui-redesign`
again, under different SHAs.

- Motion P1: `c6e2bd5f`/`90e3034b` (on feat) vs. `bfc1ebd4`/`18b82ab4` (on
  origin/main) — same content, two lineages.
- Automation P1–P3: `b1631a2f` ("local integration" merge, on main) vs.
  `2a51fb29` (on feat) — same content, two lineages.
- Automation P4 (feat-only, main's one genuine gap) sits on top of feat's
  twins, so a plain feat→main merge conflicts with its own doppelgänger.

Root cause: a single-integrator model applied to a multi-integrator reality.
Full diagnosis + the P4 cleanup brief: the `git-landing-protocol` memory.
Decided with Peter 2026-07-04, replacing §2 below.

## §1. Shared-checkout guard (built, `6737cfe6`)

File: `.claude/hooks/preToolUseBash.py`. `git checkout`, `switch`, and `merge`
are normally auto-allowed as workflow writes. **Downgraded 2026-07-04 evening
(Peter): ask → allow + warning.** Originally these fell through to a permission
prompt, but that paused every automated orchestration mid-landing (the guard's
"another session live" condition is always true under fleets). Now, when BOTH
of these hold, the command is still auto-allowed and a warning naming the other
live session is attached as additionalContext (proceed only if intended, prefer
a worktree, re-read branch state from command output):

- The command targets the MAIN checkout. Bare `git ...` counts (cd-prefixes
  are already banned, so bare = main tree). `git -C <path> ...` counts only
  if `<path>` resolves inside the main checkout and not under
  `.claude/worktrees/`. Commands into worktrees stay auto-allowed unchanged.
- Another session is live: any `.claude/daemon/verdicts/*.pid` whose pid
  passes a signal-0 check and whose session id differs from this hook
  invocation's `session_id` (hook stdin JSON). The observer idle-exits after
  10 minutes, so a session with no live daemon has been quiet that long and
  is safe to treat as absent. No lock files, nothing to go stale.

Semantics: never deny, never ask; the point is the agent is TOLD instead of the
switch happening silently — discipline (worktrees, read-state-off-output) does
the protecting. Solo (no other live daemon) behavior is unchanged.
Branch-switch detection includes `checkout <branch>`, `checkout -b/-B`,
`switch`, `merge`, and bare `checkout` with no `--`-separated paths; plain
`git checkout -- <paths>` (file restore) is destructive-to-worktree, not a
branch switch — left alone.

Failure posture: any exception inside the check falls back to the hook's
existing behavior for that command (fail toward status quo, never toward
blocking everything).

## §1b. Landing-protocol guard (spec — implementing this session)

File: `.claude/hooks/preToolUseBash.py`. Two new behaviors, both scoped to
the MAIN checkout only (same `in_main` resolution as §1; worktree-targeted
commands are unaffected):

1. **Always ask** (regardless of foreign-session liveness — this is now
   simply wrong under the merge-trunk model, not just concurrency-unsafe):
   - `git branch -f main ...` / `git branch -F main ...` (force-moves the
     main pointer, dropping whatever commits aren't its ancestors).
   - `git push` carrying a force flag (`--force`, `-f`,
     `--force-with-lease`, `--force-if-includes`) whose target is `main` —
     either an explicit `main` / `HEAD:main` / `refs/heads/main` token, or a
     bare/no-branch-arg push while the current branch (resolved via
     `git rev-parse --abbrev-ref HEAD` in the target dir) is `main`.
   Reason string: names the merge-trunk model and points at the landing
   protocol below instead.
2. **Allow + reminder** (deterministic nudge, not a permission gate): a
   non-force `git push` or `git merge` that lands on main (same target
   detection as above, minus the force-flag requirement) gets the normal
   allow plus a short `additionalContext` reminder of the landing-protocol
   loop (fetch → merge origin/main → gate → merge --no-ff → push → retry on
   rejection) and the two twin-killers (§2 below).

Failure posture: same as §1 — any exception falls back to today's behavior
for that command.

Tests: extend the hook's existing test setup. Cases: `branch -f main <tip>`
in main tree (asks) vs. in a worktree (unaffected); `push --force origin
main` (asks); bare `git push` on branch main with a force flag set via
config alias — out of scope, flag detection is argv-only; non-force `push
origin main` (allow + reminder present); `merge <branch>` while on main
(allow + reminder); same commands with target dir under
`.claude/worktrees/` (unaffected, no reminder, no ask).

## §2. Landing protocol (replaces the retired ff-only convention)

- **Main is the merge-based trunk.** No separate integration branch — the
  branch that accidentally became one (`feat/timeline-ui-redesign`) is
  exactly the failure mode: two landing spots is what produced the twins.
  "Last-known-good" is now a property of the gate (clippy + tests before any
  merge), not of linearity.
- **To land a workstream:** fetch → merge current `origin/main` into your
  branch → rerun the gate (touched-crate clippy + focused tests; the full
  workspace sweep — workspace clippy + `cargo nextest run --workspace` +
  `cargo deny check bans` — at batched landings per §2c, or sooner when blast
  radius says so) → `git merge --no-ff` into main → push → if the push is
  rejected because someone landed first, repeat. New/renamed docs need
  `python3 scripts/gen_docs_index.py` before the sweep — a freshness test
  enforces it. The gate also owns status housekeeping: in the worktree, run
  its copies of `bug_status.py --check` (fix drift with `--write` there — it
  refuses in main) and `design_status_check.py origin/main HEAD`, so backlog
  reflow and design-doc status lines land in the same merge as the code. The
  post-merge housekeeper on main is a backstop, not the workflow — its
  remedies are worktree-shaped, never in-place edits to main.
- **Perf gate for content-thread/render-path waves** (PERF_BUDGET_GATE_DESIGN.md
  P3): if the wave touched content-thread or render-path code, the gate list
  above also includes `cargo xtask perf-soak <project|glb> [--seconds N]
  [--start beats] [--size WxH] [--profile] [--frames N] [--update-baseline]`
  against the Liveschool fixture before the merge — same deliberate-run
  posture as `gpu-proofs`, run once per landing wave, not per commit.
- **Supersession sweep (part of the gate when a landing completes/supersedes
  a design phase, bug, or named plan):** update the design doc status header
  and the backlog `**Status:` line, then `rg` the plan's name AND its stage
  labels across `docs/` and the memory directory and fix or tombstone every
  hit still asserting the old state. Supersession under a different name is
  the known killer (PRESET_INSTANCE_COLLAPSE absorbed BINDING_UNIFICATION's
  "B+/B++/C" 2026-06-07; the stale memories cost two Fable sessions a month
  later). Full rule: CLAUDE.md hard rules.
- **Twin-killer 1:** never cherry-pick or re-commit content that already
  exists as commits on a live branch — merge the branch so SHAs stay shared.
  The one sanctioned exception: landing the final content of a branch that
  is being retired immediately afterward (the P4 cleanup below is the
  precedent — feat's lineage is being fully retired, so lifting its last
  unique commits by cherry-pick and then deleting the branch doesn't create
  a new twin).
- **Twin-killer 2:** never delete a branch until `git merge-base
  --is-ancestor <tip> origin/main` confirms its commits are on main.
- `git branch -f main <tip>` and force-pushes to main are anti-patterns now
  (§1b asks before either).
- A session doing sustained code work still gets a LONG-LIVED worktree slot
  for the workstream, acquired off a verified tip (the playbook's step-0
  base-verification guard) with gitignored fixtures copied in; per-session
  worktrees pay the cargo cold-build tax and are not the pattern. Acquire
  through `scripts/agent-worktree.py` (§2c) — the ONLY sanctioned source
  (raw `git worktree add` is hook-denied since the 2026-07-15 455 GB
  incident). It reuses an idle warm slot before it ever creates a cold one.
- The main checkout is the only place merges to main happen. Sessions in
  worktrees never run bare git/cargo (always `-C` / `--manifest-path`,
  absolute, quoted — the repo path contains a space).
- **Workers never land.** Only the orchestrating session (or Peter) merges
  into main.
- **Never use the Agent tool's built-in `isolation: "worktree"` for repo
  work** — it bases the worktree off the default branch, not your tip, and
  bypasses the slot ring's cap. Hook-denied
  (`agent-worktree-isolation-guard.py`), as is raw `git worktree add`
  (`preToolUseBash.py`). Worktrees come from `scripts/agent-worktree.py
  acquire` only, with the step-0 base-verification guard in the brief.

## §2c. Build-speed rules (added 2026-07-10 — orchestration wall-clock pass)

Measured basis: ~80% of a phase's wall-clock is cargo compile/test (playbook,
2026-07-03); by 2026-07-10 ten live worktrees carried 5–41 GB of cold-built
`target/` each. Three rules:

1. **Reuse warm slots — the pool is a capped ring (reworked 2026-07-15
   after 19 per-task worktrees × 15–60 GB targets = 455 GB filled the
   disk).** `python3 scripts/agent-worktree.py acquire <task-label> <branch>
   [--tip REF] [--owner TEXT]` re-points the warmest idle slot with
   `checkout -B`; slots are anonymous (`slot-0`…`slot-9`, task label lives
   in the lease), capped at 10 with no override (raised from 6 on
   2026-07-17; worst case ~270 GB fully warm) — all-busy is a loud `POOL
   FULL` error to surface to Peter, never to work around. A slot's
   `target/` past 25 GB is wiped at acquire. Idle = clean status + HEAD
   is-ancestor of origin/main + lease absent or stale (8 h). The script
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
   unchanged); the fetch → merge origin/main → gate → merge --no-ff → push
   loop runs once per 2–3 phases per design, not per phase. Each landing
   skipped saves a gate rerun here and a push-rejection retry for every other
   concurrent session.
4. **Prewarm during briefing.** The moment a worktree is acquired for a code
   wave, kick the phase's primary build (`cargo build -p <crate> [--features
   …] --manifest-path "<wt>/Cargo.toml"`) in the background, then write the
   worker briefs — the build runs while the worker reads its docs. Free
   overlap on every cold or profile-invalidated start.
5. **Test runner + cache size (added 2026-07-11).** CPU-focused gates run
   `cargo nextest run -p <crate> --lib`; GPU-proofs suites STAY on
   `cargo test` — the in-process `test_device` lock is the device serializer
   and nextest's process-per-test model would defeat it.
   `SCCACHE_CACHE_SIZE = "30G"` (`.cargo/config.toml` `[env]`) stops
   dep-cache eviction; the server picks it up at its next idle restart.
   (The dev profile is already build-speed-tuned — `debug =
   "line-tables-only"` + `split-debuginfo = "unpacked"`, root Cargo.toml —
   don't re-add those.) `cargo deny check bans` (<1s) is part of the landing
   sweep as of 2026-07-11 — `deny.toml` is the actual enforcement of the
   wgpu/metal dependency bans.

## §2b. Pending cleanup (2026-07-04 twin-commit remediation)

1. **Land automation P4** — the only content genuinely missing from main.
   Cherry-pick the six P4 commits from `feat/timeline-ui-redesign` (`f03c8a31`,
   `05b48436`, `ed252abd`, `3b851c61`, `51a5cb52`, `ebbf5f1d`) in a fresh
   worktree off current origin/main; evaluate whether the `983a3837`
   design-token-guard fixup is still needed against main's own motion
   copies. Conflicts against main's motion twins (`bfc1ebd4`/`18b82ab4`) are
   expected and bounded. Full workspace sweep (P4 touches manifold-ui +
   renderer broadly), then merge to main, push. This is twin-killer 1's
   sanctioned exception (see §2) — feat is being retired right after.
2. **Prune retired branches** — after `is-ancestor` confirms content is on
   main: delete `feat/timeline-ui-redesign`, `lane/automation-lanes`,
   `lane/timeline-p0`, `lane/ui-motion` (local + remote + their
   `.claude/worktrees/` dirs if any remain).
3. **Lower-priority remote sweep** (not blocking, do when a session is
   quiet): several remote-only branches under `origin/` (`gig-resilience-p1/2`,
   `session-mode-p1/2/3`, `multi-display-p1`, `vocab-apply`,
   `feat/multi-selection-ux`, `feat/ui-design-system-plan`,
   `feat/input-widget-identity`, `feat/effect-digital-drift`, several
   `wave/*`) look stale relative to current main. Verify each with
   `is-ancestor` before deleting — don't assume from the name.

## §3. Already in force (no work)

Commit fast — never sit on uncommitted renames/deletions while other sessions
or agents run; read the branch off the commit OUTPUT, not session start; diff
resurrected files against their new-path versions before deleting (a merge,
not an agent, may have restored them).

## §3b. Shared-checkout commit mechanics (added 2026-07-07 — four sessions hit these in one day)

- **Untracked-file exception to the pathspec-commit rule.** `git commit -m …
  -- <path>` fails with "pathspec did not match" for a NEW file — git cannot
  pathspec-commit what it isn't tracking. The correct move: `git add --
  <exactly the new paths>` then the pathspec commit as usual. The targeted
  add plus the commit's own pathspec still fences out other sessions' staged
  work, preserving the rule's intent. Never `add -A`, never `add .`, never a
  bare commit. (Sessions 2b15501e, 0f503e2e, 85d2348e, 0f5b70ea — all
  improvised this independently; now it's written down.)
- **Inverse-sweep hazard.** The known hazard is YOUR add-then-commit sweeping
  THEIR staged work. The inverse also happens: another session's landing loop
  can commit files sitting unstaged/untracked in YOUR working tree (observed
  2026-07-06: a concurrent session landed this session's three working-tree
  docs minutes before its own commit — the commit then failed with "nothing
  added"). If your pathspec commit reports nothing to commit, diff your files
  against HEAD before assuming an error: byte-identity means another session
  landed them, and the landing is yours to verify, not redo.
- **Landing merges are standalone commands.** Never run the landing `git
  merge --no-ff` inside a compound chain — HEAD can change between a chain's
  steps in a shared checkout (observed 2026-07-07: a fetch→merge→push chain
  landed a merge on another session's branch). Re-verify
  `git branch --show-current` immediately before the merge step, as its own
  command.
- **Worktree handoff files.** A session stopping mid-work in a worktree
  (compaction, budget, interruption) writes its handoff to
  `<worktree>/WORKTREE_HANDOFF.md` — branch state, uncommitted diagnostics,
  the finding in flight — not only to its final chat message. A successor
  session can find a file; it cannot find a transcript message (observed
  2026-07-07: a successor only avoided duplicating a stopped session's work
  because `git worktree add` happened to collide on the branch name).

## Acceptance

§1 test cases (unchanged) pass. §1b: `branch -f main` and force-push-to-main
ask unconditionally in the main tree, are unaffected in a worktree; a
non-force push/merge landing on main carries the reminder; all cases fail
open on exception. Clippy-clean is N/A (hook is Python). Commit by path and
push.
