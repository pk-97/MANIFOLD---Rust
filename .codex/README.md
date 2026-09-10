# Codex guards

The guard applies the same rules to every model. Luna tasks and subagents need
no scope registration, prepared dispatch, shell allowlist, or model/effort gate.
Model choice does not determine whether a task is a lead or worker.

The hook checks both paths of moves, blocks app patches in main, reuses CC's
read-only git/path detection, and requires app landings through
`scripts/land_branch.py`. Claude settings and hook registration are separate.

Normal commits require exact paths. Git does not support pathspec commits
during a merge, so `git commit --no-edit` is allowed in a slot with a pending
merge, no unresolved conflicts, and no unstaged tracked changes. This does
not permit main-checkout merge commits or bypass the landing gate.

Run `python3 -B .codex/hooks/test_guard.py` after changes.

## Workflow tools

- `python3 -B scripts/codex_prepare.py --repo "$PWD" --path crates/manifold-ui/src/param_surface.rs --task 'Fix the gesture' --findings 'Describe observed evidence' --acceptance 'Name the required behaviour'` emits a worker brief with relevant source, architectural rules and runnable checks. Repeat `--path` for the owned files; new files are allowed. The lead supplies the diagnosis and passes the brief to the worker.
- `python3 -B scripts/codex_checks.py --repo "$PWD" --base origin/main` selects worker checks from committed changes plus tracked and untracked work. `--path` overrides discovery; `--json` emits argv arrays. It reuses the landing gate's package/GPU selectors and UI-flow mappings. It never runs checks, caches passes or replaces the landing gate's reverse-dependency expansion.
- `python3 -B scripts/codex_usage.py --since 2026-09-10 --repo "$PWD" --json` reports local per-model/task/effort token usage and repeated direct shell calls. Counts are not subscription billing or proof of wasted work. Calls embedded in `functions.exec` are not parsed.

The existing PreToolUse hook supplies matching subsystem rules for native patch
paths and paths mentioned in agent dispatches, once per distinct guidance block
per session. This is advisory context to the calling agent; dispatchers must
include it in worker briefs. It does not require scope registration or grant
permission. Supported mappings live in `scripts/codex_subsystems.json` and
reference existing source/docs. Unmapped paths do not imply architectural safety.
Run `python3 -B .codex/hooks/test_context.py` for context delivery tests. The
check planner selects the individual tool tests; the landing gate enforces
those same tests when their source or mappings change.

Synthetic hook delivery is tested; desktop delivery still depends on the trusted
hook definition. Check `/hooks` after updating the harness. No Claude hook or
provider configuration is changed by these tools.

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
desktop hook event aliases the tool or omits/replaces the requested worktree
with the main checkout, the guard accepts only one unambiguous live permit for
that exact command:

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
