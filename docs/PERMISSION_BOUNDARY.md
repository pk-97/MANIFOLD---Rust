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
scripts are executable and every call site was rewritten (2026-07-26,
`7533149f`).

### The matcher is redirect-aware

Empirical, 2026-07-26: a trailing-`*` rule (`echo *`) does NOT wave a
redirect to an arbitrary path through — `echo x > ~/file` escalated to the
classifier instead of matching the rule. Redirects to `/tmp` are approved
(repo hook policy). So "read-only" wildcard rules are not writable via
shell redirection; the breakout class to worry about is commands with their
own write/exec flags (awk/find/tee above), not the shell.

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

Three sources: user-global `~/.claude/settings.json`, committed
`.claude/settings.json`, and `.claude/settings.local.json` (**gitignored** —
this section is its only reviewable copy). The fenced block below is
machine-checked: `.claude/hooks/permissions_sync_check.py` diffs it against
the live files and exits 1 on drift. Run it after any rule change; the block
must list every rule, one per line, exactly as written in the JSON.

```permissions
# --- user-global ~/.claude/settings.json ---
Read(//**)
Bash(grep *)
Bash(rg *)
Bash(head *)
Bash(tail *)
Bash(wc *)
Bash(ls *)
Bash(cat *)
Bash(stat *)
Bash(which *)
Bash(echo *)
Bash(sort *)
Bash(jq *)
Bash(fd *)
Bash(ast-grep *)
Bash(sg *)
# --- committed .claude/settings.json ---
Bash(git commit -m *)
Bash(cargo metadata *)
Bash(cargo xtask install *)
Bash(cargo xtask bundle *)
Bash(cargo xtask install)
Bash(cargo xtask bundle)
Bash(grep *)
Bash(rg *)
Bash(head *)
Bash(tail *)
Bash(wc *)
Bash(ls *)
Bash(cat *)
Bash(stat *)
Bash(which *)
Bash(echo *)
Bash(sort *)
Bash(jq *)
Bash(cargo build)
Bash(cargo build *)
Bash(cargo check)
Bash(cargo check *)
Bash(cargo clippy)
Bash(cargo clippy *)
Bash(cargo test)
Bash(cargo test *)
Bash(cargo run -p manifold-renderer --bin check-presets)
Bash(cargo run -p manifold-renderer --bin check-presets *)
Bash(unzip -p *)
Bash(unzip -l *)
# --- gitignored .claude/settings.local.json ---
Bash(cargo check *)
Bash(cargo build *)
Bash(cargo clippy *)
Bash(cargo test *)
Bash(cargo run *)
Bash(cargo tree *)
Bash(cargo search *)
Bash(cargo update *)
Bash(cargo fmt *)
Bash(cargo nextest run *)
Bash(git add *)
Bash(git commit *)
Bash(git push *)
Bash(git checkout *)
Bash(git revert *)
Bash(git rm *)
Bash(git log *)
Bash(git diff *)
Bash(git status *)
Bash(git show *)
Bash(git lfs *)
Bash(git -C *)
Bash(git check-ignore *)
Bash(git fetch *)
Bash(git rev-parse *)
Bash(git merge-base *)
Bash(gh pr view *)
Bash(gh pr list *)
Bash(gh pr status *)
Bash(gh pr checks *)
Bash(gh pr diff *)
Bash(gh run view *)
Bash(gh run list *)
Bash(gh run watch *)
Bash(bd *)
Bash(sleep *)
Bash(sed -n *)
Bash(cc-fleet status *)
Bash(cc-fleet spawn *)
Bash(cc-fleet teardown *)
Bash(.claude/hooks/oneshot *)
Bash(pkill -f rust-analyzer)
Bash(pkill -f "zola.*serve")
Bash(memory_pressure -Q)
Bash(./scripts/build-analyzer-vst-plugin.sh)
Bash(./plugins/scripts/build-analyzer-vst-plugin.sh)
Bash(bash scripts/build-analyzer-vst-plugin.sh)
Bash(bash "/Users/peterkiemann/MANIFOLD - Rust/plugins/scripts/build-analyzer-vst-plugin.sh")
Bash(plugins/scripts/build-analyzer-vst-plugin.sh)
Bash(zola --root "/Users/peterkiemann/latent-space-site" build)
Bash(zola --root "/Users/peterkiemann/latent-space-site" serve --interface 127.0.0.1 --port 1111)
Bash(scripts/agent-worktree.py list)
Bash(scripts/agent-worktree.py acquire *)
Bash(scripts/gen_docs_index.py)
Bash(scripts/seat_tool.py show)
Bash(scripts/gate_runner.py show *)
Bash(scripts/gate_runner.py report *)
Bash(scripts/token_report.py *)
Bash(scripts/run_ui_flows.py *)
Bash(scripts/move_identity_check.py *)
Bash(scripts/gen_glb_conformance_status.py)
Bash(scripts/test_move_identity_check.py)
WebSearch
WebFetch(domain:www.latentspacemusic.com)
Read(//Users/peterkiemann/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/objc2-app-kit-0.2.2/src/**)
Read(//Users/peterkiemann/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/objc2-app-kit-0.2.2/**)
Read(//Users/peterkiemann/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/objc2-0.5.2/src/**)
Read(//Users/peterkiemann/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/objc2-foundation-0.2.2/src/**)
Bash(unzip -o '/Users/peterkiemann/Library/CloudStorage/Dropbox/Videos/LATENT SPACE - Marketing Content/MANIFOLD Projects/Interim/Album Art Animation.manifold' -d /private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/15bacc9b-646a-4bea-bfbd-006916506835/scratchpad/albumart)
Bash(git -C "/Users/peterkiemann/.claude" mv commands/brief.md commands/tldr.md 2>/dev/null || mv "/Users/peterkiemann/.claude/commands/brief.md" "/Users/peterkiemann/.claude/commands/tldr.md")
Bash(cp ~/Library/Logs/DiagnosticReports/manifold-2026-06-27-161935.ips /private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/e3756d30-0ebe-48c4-8b3c-95225affbb28/scratchpad/crash.ips; wc -l /private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/e3756d30-0ebe-48c4-8b3c-95225affbb28/scratchpad/crash.ips)
Bash(psql postgresql://litellm:litellm-local@localhost:5432/litellm -c "select \\"startTime\\", model, \\"model_group\\", api_key, total_tokens from \\"LiteLLM_SpendLogs\\" order by \\"startTime\\" desc limit 15;")
```

Removed in the 2026-07-26 audit (see section 3 for why): `Bash(python3 -c ' *)`,
`Bash(python3 -)`, `Bash(awk *)` (both files), `Bash(find *)` (both files),
`Bash(git worktree *)` — arbitrary execution or unreviewed destruction.

Rationale for the script rules (unchanged from the original audit):

| Rule | Why it is safe |
|---|---|
| `scripts/agent-worktree.py list` | read-only |
| `scripts/agent-worktree.py acquire *` | bounded by the slot ring cap |
| `scripts/gen_docs_index.py` | no arguments |
| `scripts/seat_tool.py show` | read-only |
| `scripts/gate_runner.py show *` / `report *` | read-only (verdicts trail / subprocess-free report); `cc-fleet keyget` runs under `pre-wave`, which is NOT allowlisted |
| `scripts/token_report.py *` | reads transcripts, flags only |
| `scripts/run_ui_flows.py *` | bounded by `scripts/ui-flows/manifest.json` — which is agent-editable, so this is a section 4 residual-risk rule |
| `scripts/move_identity_check.py *` | git refs only |
| `scripts/gen_glb_conformance_status.py` | no arguments |
| `scripts/test_move_identity_check.py` | no arguments — but it is an editable file executed directly; section 4 residual risk |

Deliberately NOT allowlisted, keep classified: `psql` in wildcard form (one
literal read-only query IS allowlisted — see block; the wildcard never),
`curl` / `wget` (network egress), `rm`, `gate_runner.py per-lane` (executes
commands extracted from a brief file), `agent-worktree.py release` (deletes a
worktree and any uncommitted work in it), `seat_tool.py assign` (rewrites
model routing).

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
