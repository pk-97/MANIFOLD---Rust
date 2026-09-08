# Codex guards

The guard applies the same rules to every model. Luna tasks and subagents need
no scope registration, prepared dispatch, shell allowlist, or model/effort gate.
Model choice does not determine whether a task is a lead or worker.

The hook checks both paths of moves, blocks app patches in main, reuses CC's
read-only git/path detection, and requires app landings through
`scripts/land_branch.py`. Claude settings and hook registration are separate.

Run `python3 -B .codex/hooks/test_guard.py` after changes.

Limits: this is workflow enforcement on supported tool calls, not a security
boundary. Arbitrary shell scripts, MCP writes and interactive stdin are not
file-scope checked. Keep task ownership and review responsibilities in briefs.

## Execution budget

Recognized direct Cargo checks and the required `gpu_proofs_gate.py` get two
attempts per exact command and working directory per session, including
successful attempts. Broad Cargo checks, nightly/feature sweeps, perf soaks and
other recognized visual/GPU probe scripts need a bounded exception. Common
env/build-lock wrappers are recognized. Required checks run inside
`land_branch.py` and `landing_gate.py` remain unchanged.

The lead can register an exact command for 1–3 attempts (default one), expiring
after 30 minutes. Permits are project-scoped so the desktop hook and CLI can use
different session identifiers without losing the exact-command match. If a
desktop hook event omits or replaces the requested worktree with the main
checkout, the guard accepts only one unambiguous live permit for that exact
command:

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
