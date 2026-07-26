# Permission Boundary — auto mode, the classifier, and the allow list

Security boundary. Every rule here gates what an agent can execute on Peter's
machine without human review. Read whole before adding, widening, or removing a
`permissions.allow` entry. Mechanism read from the Claude Code 2.1.219 binary
2026-07-26; re-verify after a CC upgrade.

## 1. What the classifier is

In `--permission-mode auto`, a tool call that is not otherwise cleared is sent
to a **separate LLM** with the full transcript, `CLAUDE.md`, repo visibility,
git status, and the pending action. That model answers allow/block against a
built-in rulebook (~68 block rules, ~20 allow exceptions, ~108k chars).

Two stages: stage 1 gets 64 max_tokens and must answer `<block>yes/no`; stage 2
runs only when stage 1 flags, with 8192 tokens, and writes the reason.

**It fails closed.** Unparseable output, or an unavailable model with no working
fallback, blocks the action. Fallback fires only on timeout / connection error /
429 / 5xx — never on 401. A dead provider freezes the session; it does not open
the gate.

## 2. Which model runs it

Resolution order:

1. Server config Anthropic pushes (`tengu_auto_mode_config.model` /
   `.modelByMainModel`).
2. `ANTHROPIC_DEFAULT_SONNET_MODEL`, if it passes CC's model-validity check.
3. If the main-loop model equals `ANTHROPIC_DEFAULT_FABLE_MODEL` (or is
   claude-fable-5/mythos) → `ANTHROPIC_DEFAULT_OPUS_MODEL`.
4. Otherwise the main model itself.

**Branch 2 is session-sticky.** The first classifier call of a session probes
the sonnet slot: success locks it in for the session; any non-401 error marks
the session demoted and it uses branch 3 for every later call, permanently. A
demoted pane never recovers — restart it.

On the K3 seat the tmux binding sets `ANTHROPIC_DEFAULT_FABLE_MODEL=k3` with
main model k3, so a demotion lands on glm-5.2. Verified 2026-07-26: a fresh
session classifies on `deepseek-v4-flash` as intended.

Oracle: `claude --debug -p '…' --permission-mode auto`, then grep
`~/.claude/debug/latest` for `classifier_request_started` (prints the model).

## 3. What bypasses the classifier

Three paths, in order:

1. **acceptEdits fast path** — the action would be allowed in acceptEdits mode.
   Debug: `Skipping auto mode classifier for <Tool>: would be allowed in
   acceptEdits mode`.
2. **Built-in safe-tool allowlist** — Debug: `tool is on the safe allowlist`.
3. **`permissions.allow`** — with the exclusion below.

### The interpreter exclusion

Auto mode **silently ignores** an allow rule whose content is empty, `*`, all
asterisks, or that names an interpreter — exactly, as `prefix:*` / `prefix *` /
`prefix*`, or starting with `prefix ` and ending in `*`:

```
python python3 python2 node deno tsx ruby perl php lua npx bunx
npm run  yarn run  pnpm run  bun run
bash sh zsh fish ssh eval exec env xargs sudo
```

Rules for the Agent/Task tool are always ignored. `curl wget kubectl aws gcloud
gsutil` are ignored only when the remainder contains `$`/backtick or matches a
dangerous subcommand set.

An ignored rule still works for permission *prompts* in other modes — it just
never skips the classifier. It fails silently either way, so it reads as
working.

Consequence: `python3 scripts/x.py …` can never skip the classifier. Direct
invocation (`scripts/x.py …`, shebang + exec bit) can. That is why the repo's
scripts are executable and every call site was rewritten (2026-07-26,
`7533149f`).

## 4. The bar for adding an allow rule

Ask one question: **can any argument permitted by this rule cause code to run,
or state to be destroyed, that the reviewer would not see in the command text?**
If yes, the rule is an arbitrary-execution allowlist regardless of how safe the
script's name looks. Narrow to subcommands, or leave it classified.

Specifically un-allowlistable in wildcard form:

- anything taking a path to a file whose contents get executed
- anything taking a command string, or `--exec`-style passthrough
- destructive subcommands (delete, release, reset, assign, drop)
- SQL clients, network fetchers, package installers

**Residual risk that cannot be designed away:** a path-based rule trusts what
the file contains *at run time*, not at approval time. An agent that edits a
script and then runs it gets unreviewed execution — the edit is visible, the
execution is not. Keep allowlisted scripts small and boring, and treat edits to
them as security-relevant in review.

## 5. Current allow list

`.claude/settings.local.json` is **gitignored** — the live list is not in the
repo, so it is recorded here for review. Re-sync this section when it changes.

Read-only shell (user settings, global): `grep rg find head tail wc ls cat stat
which echo sort awk jq fd ast-grep sg`, `Read(//**)`.

Project (`.claude/settings.json`): cargo `build/check/clippy/test/metadata`,
`cargo xtask install|bundle`, `git commit -m *`, `unzip -p|-l`, `check-presets`.

Project-local (`.claude/settings.local.json`): cargo
`check/build/clippy/test/run/tree/search/update/fmt/nextest run`; git
`add/commit/push/checkout/revert/rm/log/diff/status/show/worktree/lfs/-C/
check-ignore/fetch/rev-parse/merge-base`; `gh pr|run`; `bd *`; `sleep *`;
`sed -n *`; `cc-fleet status|spawn|teardown|update`; `.claude/hooks/flash *`;
zola build/serve; and these scripts:

| Rule | Why it is safe |
|---|---|
| `scripts/agent-worktree.py list` | read-only |
| `scripts/agent-worktree.py acquire *` | bounded by the slot ring cap |
| `scripts/gen_docs_index.py` | no arguments |
| `scripts/seat_tool.py show` | read-only |
| `scripts/gate_runner.py show *` / `report *` | read-only |
| `scripts/token_report.py *` | reads transcripts, flags only |
| `scripts/run_ui_flows.py *` | bounded by `scripts/ui-flows/manifest.json` |
| `scripts/move_identity_check.py *` | git refs only |
| `scripts/gen_glb_conformance_status.py` | no arguments |
| `scripts/test_move_identity_check.py` | no arguments |

Deliberately NOT allowlisted, keep classified: `psql` (arbitrary SQL), `curl` /
`wget` (network egress), `rm`, `gate_runner.py per-lane` (executes commands
extracted from a brief file), `agent-worktree.py release` (deletes a worktree
and any uncommitted work in it), `seat_tool.py assign` (rewrites model routing).

## 6. Incident — 2026-07-26

`Bash(scripts/gate_runner.py *)` was added as part of the direct-invocation
change. `gate_runner.py per-lane --brief <path>` extracts shell commands from
the brief's Gate section and executes them, so the rule was an
arbitrary-execution allowlist. Caught by the classifier blocking the landing
merge under its Auto-Mode Bypass rule — not by review. Two sibling rules
(`agent-worktree.py *`, `seat_tool.py *`) had the same shape and were cut in the
same pass.

Lesson: the failure mode is a wildcard on a script that takes a
path-to-something-executable. Judge the script's *arguments*, never its name.
