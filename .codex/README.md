# Codex guards

Trust the project hook with `/hooks` in Codex before using lanes. Hooks are
inactive until trusted; changes to the hook definition require review again.

One native Luna lane at a time, explicit model and low effort. Add one line to
its brief: `MANIFOLD_SCOPE: {"worktree":"/absolute/slot/path","files":["relative/file.rs"]}`.
Use an empty file list for read-only work. Acquire write slots using the existing
ring, from the current main tip. The lead does not edit a lane's files while it
is working. Luna uses native patches, returns results, and never commits or lands.

Before native dispatch, the lead registers the same scope locally:

```sh
python3 -B .codex/hooks/guard.py prepare-lane --task lane_name --worktree '/absolute/slot/path' --files relative/file.rs
```

Omit `--files` for read-only work. Use the exact `task_name` in the spawn call.
Preparation uses `CODEX_THREAD_ID`, expires after ten minutes, and is consumed
by one accepted dispatch. The native hook receives an encrypted `message`, so
it cannot extract scope from the brief; the brief still tells Luna its scope.
The hook revalidates the prepared scope before registering it for lane calls.

Native desktop dispatch currently reaches hooks as `collaborationspawn_agent`;
keep that name alongside `spawn_agent` and `Agent` in both matcher and guard.
After changing dispatch handling, verify a live read-only lane as well as tests:
unit tests cannot establish runtime scope registration.

The hook checks both paths of moves, blocks app patches in main, reuses CC's
git/path detection, and requires app landings through `scripts/land_branch.py`.
No approval auto-allows and no per-turn reminders or automatic build loops.

Run `python3 -B .codex/hooks/test_guard.py` after changes. CC files are imported
read-only; their settings, hook registration and state are not modified.

Limits: this is workflow enforcement on supported tool calls, not a security
boundary. Arbitrary lead shell scripts, MCP writes and interactive stdin are
not file-scope checked. Model-based lane identification assumes this two-model
setup; one lane per parent session is essential to its scope tracking. Scope
records are temporary and a missing record blocks lane edits/checks. Changing
the roster or enabling parallel lanes requires revisiting this design.

## Execution budget

Recognized direct Cargo checks get two attempts per exact command and working
directory per session, including successful attempts. Broad Cargo checks,
nightly/feature sweeps, perf soaks and recognized visual/GPU probe scripts need
a bounded exception. Common env/build-lock wrappers are recognized. Required
checks run inside `land_branch.py` and `landing_gate.py` remain unchanged.

The lead can register an exact command for 1–3 attempts (default one), expiring
after 30 minutes:

```sh
python3 -B .codex/hooks/guard.py permit-check --worktree '/absolute/worktree' --command 'cargo test -p manifold-ui mapping' --reason 'Changed mapping dispatch; verify regression'
```

Do not renew or vary commands to evade the budget. A retry requires changed
code, new evidence, or explicit user direction. Workers return the evidence
to the lead. This is an attempt counter, not result caching or a token cap:
it cannot distinguish success from failure, inspect arbitrary scripts, or
budget checks nested inside landing scripts. Native computer-use/MCP calls
are not covered; obey the repository's bounded visual-check rule. Tests cover
synthetic hook events; live dispatch coverage depends on the trusted desktop
hook. Re-trust the updated definition with `/hooks`.
