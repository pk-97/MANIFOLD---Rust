# Git tree discipline — build spec

Status: §1 hook guard BUILT 2026-07-04 (`6737cfe6`); §2 conventions in force (also in CLAUDE.md hard rules).
Origin: two live sessions shared the main checkout; one switched branches and
fast-forward merged while the other had uncommitted file moves in flight. The
merge resurrected the moved files' old paths and the second session's commit
landed on a branch it never chose. Full incident: `88257631` commit message +
the hazards section of the `agent-execution-playbook` memory.

Root cause: N concurrent sessions, one HEAD. The fix makes main-checkout branch
state owned and enforced, not remembered.

## 1. Hook enforcement (the code change)

File: `.claude/hooks/preToolUseBash.py`. Today it auto-allows `git checkout`,
`switch`, and `merge` as normal workflow writes. Change: when BOTH of these
hold, those commands (plus `rebase` and `reset`, which already prompt — keep
them prompting) must NOT be auto-allowed and must fall through to the normal
permission prompt, with a reason string naming the other live session:

- The command targets the MAIN checkout. Bare `git ...` counts (cd-prefixes are
  already banned, so bare = main tree). `git -C <path> ...` counts only if
  <path> resolves inside the main checkout and not under `.claude/worktrees/`.
  Commands into worktrees stay auto-allowed unchanged.
- Another session is live: any `.claude/daemon/verdicts/*.pid` whose pid passes
  a signal-0 check and whose session id differs from this hook invocation's
  `session_id` (hook stdin JSON). The observer idle-exits after 10 minutes, so
  a session with no live daemon has been quiet that long and is safe to treat
  as absent. No lock files, nothing to go stale.

Semantics: never hard-deny; the point is that Peter gets ASKED instead of the
switch happening silently. Solo (no other live daemon) behavior is unchanged.
Branch-switch detection must include `checkout <branch>`, `checkout -b/-B`,
`switch`, `merge`, and bare `checkout` with no `--`-separated paths; plain
`git checkout -- <paths>` (file restore) is destructive-to-worktree, not a
branch switch — leave its current treatment alone.

Failure posture: any exception inside the new check falls back to the hook's
existing behavior for that command (fail toward today's status quo, never
toward blocking everything).

Tests: extend the hook's existing test setup if one exists; otherwise add a
small runner invoking the hook with synthetic stdin JSON. Cases: bare checkout
with a fake live foreign pidfile (prompts), same with only own-session pidfile
(auto-allowed), `git -C .claude/worktrees/x checkout` with foreign pidfile
(auto-allowed), dead-pid pidfile (auto-allowed), malformed pidfile (auto-
allowed), merge/switch variants, `checkout -- path` unchanged.

## 2. Worktree-per-workstream convention (doc change, no code)

- **`main` = last known-good.** Never developed on; fast-forwarded to the
  verified tip whenever a workstream lands clean, without touching any
  checkout: `git branch -f main <tip> && git push origin <tip>:main`.
  Rationale: everything that bases off the default branch (fresh clones,
  the Agent tool's `isolation: "worktree"`) reads main; a stale main hands
  agents months-old code (2026-07-04 incident: a worker got a March
  checkout predating the node-graph system). For the same reason, never
  use `isolation: "worktree"` for repo work — manual `git worktree add`
  off the verified tip only, with the step-0 base-verification guard in
  the brief.

- A session doing sustained code work gets a LONG-LIVED worktree at
  `.claude/worktrees/<branch>`, created off a verified tip (the playbook's
  step-0 base-verification guard) with gitignored fixtures copied in. It
  persists across sessions until the branch merges; per-session worktrees pay
  the cargo cold-build tax and are not the pattern.
- The main checkout is the integration tree. Sessions in worktrees never
  run bare git/cargo (always `-C` / `--manifest-path`, absolute, quoted —
  the repo path contains a space).
- Merges/integration happen only in the main tree, and only while it isn't
  contested (the hook above turns contested attempts into a prompt).
- Update the CLAUDE.md hard-rules bullet that describes preToolUseBash.py so
  it mentions the shared-tree guard, and add one line to the
  `agent-execution-playbook` memory hazards pointing at this doc.

## 2b. Pending cleanup (2026-07-04 hygiene pass — do when trigger fires)

Local prune DONE (47 merged branches deleted; main ff'd to trunk tip). Remaining:

1. **Stuck worker repoint** — owner: the Opus orchestration session if still
   live; otherwise the NEXT orchestration session. A worker worktree
   (auto-named `worktree-agent-…`) branched off stale origin/main (March).
   If the original session is live: `git reset --hard
   feat/timeline-ui-redesign` inside it (clean tree), resume the worker.
   If not: delete that worktree (`git worktree remove …`) and spawn a fresh
   worker off the verified tip per the manual-worktree rule. THEN delete
   local `feat/timeline-ui-redesign` (kept only for this).
2. **Remote branch prune** — trigger: any quiet session. Delete origin
   branches whose tips are ancestors of origin/main
   (`git branch -r --merged origin/main`, then `git push origin --delete …`).
   `origin/feat/timeline-ui-redesign` is a stale divergent push (unique
   content = the deliberately-reverted build-order block, `fe7622ee`) —
   delete it too once item 1 is done.
3. **Lane branches** — trigger: lanes A/B (automation, timeline-p0) land.
   Merge to trunk, ff main, delete lane branches + worktrees.

## 3. Already in force (no work)

Commit fast — never sit on uncommitted renames/deletions while other sessions
or agents run; read the branch off the commit OUTPUT, not session start; diff
resurrected files against their new-path versions before deleting (a merge,
not an agent, may have restored them).

## Acceptance

All test cases above pass; a manual bare `git checkout main` in the main tree
with a second session live produces a permission prompt whose message names
the other session id; the same command with no other session live is silent;
clippy-clean is N/A (hook is Python); commit by path and push.
