# Workflow Runtime — operator guide

**Status: NORMATIVE working guide · 2026-07-29 · Fable.**
**Audience: the lead session driving `workflow`. Design contract: `docs/WORKFLOW_RUNTIME_DESIGN.md` section 2 (Decisions).**

`workflow` runs a semantic workflow program: a TOML file of linear steps where each model
interaction is ONE stateless API call returning a typed artifact, and the runtime owns every
side effect. You (the lead) write the program, start the run, answer escalations, and review
the result. You never steer mid-run — if you want to intervene, that's an escalation step the
program should have had.

## Running

```
cargo build -p workflow-runtime
target/debug/workflow run <program.toml> [--run-id <id>] [--mock <responses.jsonl>]
```

Run from the checkout root you want `file:` inputs and gates resolved against. Without
`--mock` the live litellm proxy is used (must be up: 500s from a seat are a proxy/quota
problem, not yours — probe with `.claude/hooks/oneshot --model <id> "say ok"`).

State lives in `.claude/orchestration/runs/<run-id>/` (gitignored):
- `step-NN-<name>.json` — completed artifacts. Their existence IS the resume state.
- `transcript.jsonl` — every request/response with token usage, retries included.
- `parked.jsonl` — steps that hit their retry cap, with the final error.
- `escalation-<step>.md` — questions awaiting your answer.
- `worktree.json` — the execute worktree, pinned for the whole run.

## Exit codes — what to do

| Exit | Meaning | Your move |
|---|---|---|
| 0 | All steps done — but CHECK `parked.jsonl`; parked steps don't block completion | Review artifacts; parked = read the reason before anything else |
| 10 | Escalated | Write your answer under the marker in the named file, rerun the same command |
| 20 | A parked step blocks a dependent step | Read `parked.jsonl`; fix the program or brief, rerun (completed steps are kept) |
| 2 | Runtime/transport error (incl. token-budget suspension) | Read the message; budget overrun → raise `token_budget`, rerun to resume |

**Exit 0 is not success by itself.** A run where every model step parked still exits 0 if
nothing depended on them. Always read `parked.jsonl` and the artifacts before reporting green.

## Expected failures (all observed in the P2 acceptance — none are runtime bugs)

- **Parse-park:** model output isn't the artifact's JSON. The serde error goes back to the
  model; cap reached → parked. Fix: better template, or a model that emits (see below).
- **Reasoning-wall truncation (D-54):** deepseek-v4-flash burns the token budget on thinking
  and truncates JSON. The transport auto-doubles to 32K; past that it parks. Code-shaped
  one-shots: prefer glm-4.7; kimi when its seat is up.
- **`find` not found / ambiguous (execute):** the model misquoted the file. The error carries
  the offending snippet back. Repeated across all attempts = your excerpts were stale or too
  thin — fix the template, not the model.
- **Gate red with commit:** each execute attempt that applies cleanly IS committed before its
  gate runs; a red gate means the branch holds a failing commit and the next attempt stacks a
  fix on top. That's by design (the branch is the workbench, review happens before landing).
- **POOL FULL on acquire:** the slot ring is exhausted — a loud stop, never worked around.

## Check-in discipline

Start runs in the background and poll the run directory, not your memory of it. A rerun of
a parked step is a NEW SAMPLE, not a retry — same input, different output is normal
(`docs/SEMANTIC_WORKFLOW_PROGRAMS.md` section 4 (the holes): non-determinism survives
temperature 0). Never call a run green from the exit code alone; the artifacts and
`parked.jsonl` are the report. Never hand-edit run state except the escalation answer files.
Cost check: `transcript.jsonl` usage fields sum to the run's spend; the program's
`token_budget` (default 500K) suspends a runaway run hard — raise it consciously, never
reflexively.

## Program reference

```toml
name = "my-program"            # run-id defaults to this
token_budget = 200000          # optional; default 500K; hard cap, retries included

[target]                       # only for programs with execute steps — exactly ONE form:
label = "task-label"           # ring-acquire: label + branch (+ optional tip)
branch = "lane/task-branch"
tip = "abc123"                 # base commit; omit for origin/main
# path = "/abs/worktree"       # OR: a pre-acquired tree (tests, replays)

[[step]]
name = "brief"                 # unique; later steps reference it in `inputs`
opcode = "generate"            # generate | execute | gate | escalate
model = "glm-4.7"              # any litellm proxy id (generate/execute only)
max_tokens = 16000             # starting budget; transport may escalate to 32K
retry_cap = 2                  # extra attempts after the first (default 2)
artifact = "text"              # text | json | verdict | change_set (execute is always change_set)
template = "brief.md"          # prompt file, relative to the program file
inputs = ["earlier-step", "file:docs/X.md"]   # {{slot}} substitutions; all must be used
gate = ["cargo clippy -p x -- -D warnings"]   # exit-code checks; execute REQUIRES one
```

Opcodes: `generate` = context → artifact, no side effects. `execute` = ChangeSet applied
atomically in the target worktree, pathspec-only commit, gate in the worktree, red fed back.
`gate` = commands only, no model; red parks. `escalate` = writes the rendered question to
`escalation-<step>.md` and suspends (exit 10). Heavy gates route through
`scripts/gate_runner.py` in the gate line — never reimplemented.

Templates are plain markdown with `{{input-name}}` slots. Every input must be used and every
slot must resolve — both directions error loudly. For execute steps, `file:` paths read the
WORKTREE's state, not the main checkout.

## What this replaces and what it doesn't

Mechanical lanes with fully-decided briefs → programs. Consult seats, exploratory debugging,
design work → still sessions (docs/AGENT_ROUTING.md). The lead still writes the brief and
still reviews the diff — the runtime deletes the agent session in the middle, not the
judgment at either end.
