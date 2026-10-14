# Permission Boundary — auto mode, the classifier, and the bar for allow rules

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

Recheck at every Claude Code binary upgrade — resolution order, interpreter-exclusion list, and session-sticky demotion are all version-sensitive (last verified at 2.1.219).

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

With Claude models only, every slot is an Anthropic model, so the sonnet slot
resolves to Sonnet 5.5 and a demotion lands on Opus (Fable or Opus 5.5 leads) or
on the main model.

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

**The exclusion does not cover compound interpreter forms.** Empirical,
2.1.219, 2026-07-26: `Bash(python3 -c ' *)` was NOT excluded — `python3 -c
'pass'` executed with no classifier call while the classifier was down
(fail-closed blocks everything unapproved, so silent execution convicts).
Treat the exclusion list above as applying only to the exact shapes listed;
any interpreter invocation with flags between the binary and the payload
must be assumed to skip the classifier. BUG-lu32 (Higher-tier audit of the auto-mode permission…) tracks re-deriving the full
exclusion semantics from the binary.

**`awk` and `find` are not in the exclusion list at all**, and both are
execution-capable: awk has `system()`, in-program `print > path` writes, and
`-f <file>`; find has `-exec` and `-delete`. Wildcard rules on either skip
the classifier. Convicted live 2026-07-26 (`awk 'BEGIN{system("true")}'`).
Both rules removed from `settings.json` the same day; awk also removed from
`preToolUseBash.py`'s READ_ONLY set, which had the same hole at the hook
layer.

Consequence: `python3 scripts/x.py …` can never skip the classifier. Direct
invocation (`scripts/x.py …`, shebang + exec bit) can. That is why the repo's
scripts are executable and every call site was rewritten (
`7533149f`).

### The matcher is redirect-aware

Empirical, 2026-07-26: a trailing-`*` rule (`echo *`) does NOT wave a
redirect to an arbitrary path through — `echo x > ~/file` escalated to the
classifier instead of matching the rule. Redirects to `/tmp` are approved
(repo hook policy). So "read-only" wildcard rules are not writable via
shell redirection; the breakout class to worry about is commands with their
own write/exec flags (awk/find/tee above), not the shell.

## 4. The bar for adding an allow rule

Obsolete when: the harness gains argument-level permission rules (constraining what an allowed command may receive) — the bar then moves into the rule syntax itself. The question below is timeless; only the manual review of it retires.

Ask one question: **can any argument permitted by this rule cause code to run,
or state to be destroyed, that the reviewer would not see in the command text?**
If yes, the rule is an arbitrary-execution allowlist regardless of how safe the
script's name looks. Narrow to subcommands, or leave it classified.

Specifically un-allowlistable in wildcard form:

- anything taking a path to a file whose contents get executed
- anything taking a command string, or `--exec`-style passthrough
- destructive subcommands (delete, remove, reset, assign, drop)
- SQL clients, network fetchers, package installers

**Residual risk that cannot be designed away:** a path-based rule trusts what
the file contains *at run time*, not at approval time. An agent that edits a
script and then runs it gets unreviewed execution — the edit is visible, the
execution is not. Keep allowlisted scripts small and boring, and treat edits to
them as security-relevant in review.

The failure mode is a wildcard on a script that takes a path to something
executable: `gate_runner.py per-lane --brief <path>` runs shell commands it
pulls from the brief, so a `gate_runner.py *` rule allowlists arbitrary
execution. Judge the script's arguments, never its name.

## 5. Risky rules that are allowed on purpose

The live rules are in `~/.claude/settings.json`, `.claude/settings.json` and
`.claude/settings.local.json` (gitignored). Read them there (`jq
.permissions.allow <file>`); this doc keeps no copy. These rules fail or
strain the section 4 bar and are kept for a stated reason:

| Rule | Why it stays |
|---|---|
| `scripts/land_branch.py *` | `--worktree` builds and runs that tree's code, and the script pushes to main. Kept so landings never stop for approval; the lead's review before landing is the check. |
| `git -C *`, `git checkout *`, `git push *` and the wide git set | `preToolUseBash.py` classifies destructive git (reset, clean, rebase, force) and still prompts. The verdict holds only while that hook stays registered. |
| `cargo run *`, `cargo test *`, `cargo build *` | They run repo code (build scripts, proc macros, tests) by design; the reviewer's unit is the repo diff, not the command. |
| `sed -n *` | sed's `w` command writes files; `sed_write_guard` in `preToolUseBash.py` catches it on the raw command and asks in every mode. |
| `scripts/run_ui_flows.py *` | Bounded by `scripts/ui-flows/manifest.json`, which agents can edit, so it carries the section 4 residual risk. |
| `scripts/agent-worktree.py acquire *` | Bounded by the slot ring cap. |
| `scripts/agent-worktree.py release slot-N` (one rule per slot) | Release drops the lease and scrubs only an idle slot; it never deletes a worktree with uncommitted work. |

Never allowlisted, keep classified: `psql` in any form, `curl` / `wget`
(network egress), `rm`, `gate_runner.py per-lane`, any interpreter `-c` form,
`awk`, `find`, `git worktree`.

## 6. Prompt sources and the worker no-prompt rule

Obsolete when: the harness lets a subagent be spawned in a never-prompt mode (a per-agent `dontAsk`) — the PermissionRequest guard then has nothing to answer.

A permission prompt stalls an unattended run until Peter answers. Four things
can raise one in auto mode (docs: permission-modes, "When auto mode falls
back"; hooks, "PermissionRequest"):

1. A PreToolUse hook returning `ask` — the only source the telemetry log shows.
2. The classifier pausing auto mode: 3 blocks in a row or 20 per session
   (not configurable) and Claude Code "resumes prompting" for everything the
   allow rules and hooks don't clear. Workers and lead share one session
   counter, so a worker that is denied three times hands the whole run to
   manual mode.
3. The harness's own checks: an output redirect or `tee` target outside the
   working directories, a write that symlink-resolves outside them, the first
   read outside them, an explicit `permissions.ask` rule, a critical-path
   `rm` (two-minute countdown).
4. `AskUserQuestion`.

Rules:

- **Worker seats never prompt.** `permission-request-guard.py` answers every
  `PermissionRequest` carrying an agent marker (`agent_id` / `agent_type` /
  `teammate_name`) with `deny` plus a reroute note, whatever raised it; the
  dispatcher (`hook_telemetry.py`, `worker_no_prompt`) turns a worker's
  PreToolUse `ask` into `deny` with the original reason. Neither ever allows,
  so section 4 (the bar for adding an allow rule) is untouched: a worker that
  genuinely needs a human stops and reports up, and the lead or Peter decides.
- **Every prompt and every classifier denial is logged.** The same hook
  records `PermissionRequest` (verdict `prompt` for the lead, `deny` for a
  worker) and `PermissionDenied` (with the classifier's reason) to
  `.claude/telemetry/hook-fires.jsonl`, field `prompt_source`. The next
  inventory is a filter on that field, not a transcript dig.
- **A hook false positive is a bug, fixed at the guard.** The sed guard scans
  only sed script arguments (`-e` values or the first non-option word), never
  file operands, heredoc bodies, or the `-i ''` suffix; `test_preToolUseBash.py`
  replays the real prompting commands. The probe-loop guard counts the GPU gate
  wrapper only in command position and never counts markdown edits.
- **Lead prompts are the human-decision residue** and stay prompts: force
  rewrites of main, destructive outward actions, a real `sed w`, and whatever
  the classifier pause leaves. Reduce them by keeping classifier denials low
  (edits to `.claude/` hooks and settings are self-modification by design —
  stage under `/tmp` and let Peter install, or land them on a branch Peter approved).
