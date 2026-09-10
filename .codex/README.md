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
- `python3 -B scripts/codex_usage.py --since 2026-09-10 --until 2026-09-11 --repo "$PWD" --json` reports local per-model/task/effort token usage and repeated commands. Completed `CommandExecution` transcript items account for nested `functions.exec` calls without interpreting JavaScript. Older sessions without completion items use direct-call attempts; `command_coverage` distinguishes the sources. Bounds are inclusive start, exclusive end. Counts are not subscription billing, successful-task counts or proof of wasted work.
- `python3 -B scripts/codex_regressions.py --json` verifies source references and emits focused commands for undo/redo, clip non-overlap, MIDI ordering/channel guards, parameter gesture/undo and generator fusion. Worker briefs include applicable evidence. This reference check is enforced at landing; it does not pretend to execute runtime tests or replace broader required gates.

The existing PreToolUse hook supplies matching subsystem rules for native patch
paths and paths mentioned in agent dispatches, once per distinct guidance block
per session. This is advisory context to the calling agent; dispatchers must
include it in worker briefs. It does not require scope registration or grant
permission. Supported mappings live in `scripts/codex_subsystems.json` and
reference existing source/docs. Unmapped paths do not imply architectural safety.
Run `python3 -B .codex/hooks/test_context.py` for context delivery tests. The
check planner selects the individual tool tests; the landing gate enforces
those same tests when their source or mappings change.

Live native-patch guidance was observed in the Luna MIDI test worker on
2026-09-10, with a parent-session context receipt also verified. Native
collaboration dispatch did not deliver automatic guidance in that observation;
the prepared brief is the verified delivery path at dispatch. Keep passing its
output to workers. No hook definition, Claude hook or provider configuration
was changed for this verification.

Mutating `cargo fmt` is blocked: even `cargo fmt -- file.rs` can format the
workspace. Use `rustfmt --config skip_children=true` on exact owned files when
formatting is needed. `cargo fmt --check` remains read-only and allowed.

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
