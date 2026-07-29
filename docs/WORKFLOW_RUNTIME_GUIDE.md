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
target/debug/workflow check <program.toml>      # lint before spending a token (exit 1 on findings)
target/debug/workflow cost <run-dir>            # per-step / per-model token ledger
target/debug/workflow unpark <run-dir> <step>   # clear a parked step, then rerun to retry it
```

ALWAYS `check` a program before its first `run` — it verifies templates (slots both
directions), `file:` existence, and `anchor:` resolution without a model call, and it is
what the authoring model runs on its own output.

One live invocation per run dir (a lockfile enforces it); the run id defaults to the
program's `name`. Rerunning the same command resumes: completed steps load from disk, and a
changed STEP LIST refuses to resume (raising `token_budget` or editing a template is fine —
that's the sanctioned resume flow).

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
| 20 | A parked step blocks a dependent step | Read `parked.jsonl`; fix the cause, then `workflow unpark <run-dir> <step>` and rerun — a plain rerun SKIPS parked steps |
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
- **Gate TIMEOUT:** any gate command outliving `gate_timeout_s` (default 900s) is killed and
  FAILS with a `TIMEOUT` tail — a hung cargo or deadlocked GPU test parks instead of holding
  the run overnight.
- **Stale worktree on resume:** if the slot ring re-issued the run's worktree, resume stops
  loudly (branch mismatch) — use a fresh run-id; never point a new program at an old run dir.

## Check-in discipline

Start runs in the background and poll the run directory, not your memory of it. A rerun of
a parked step is a NEW SAMPLE, not a retry — same input, different output is normal
(`docs/SEMANTIC_WORKFLOW_PROGRAMS.md` section 4 (the holes): non-determinism survives
temperature 0). Never call a run green from the exit code alone; the artifacts and
`parked.jsonl` are the report. Never hand-edit run state except the escalation answer files.
Cost check: each transcript line's `response.attempts` array records EVERY HTTP post the
transport made (internal budget-doubling retries and model fallbacks included); the token
budget sums those, so hidden retries are never free. `token_budget` (default 500K) suspends
a runaway run hard — raise it consciously, never reflexively. When answering an escalation,
write your answer AFTER the final answer marker and never quote the marker line inside your
answer text.

## Program reference

```toml
name = "my-program"            # run-id defaults to this
token_budget = 200000          # optional; default 500K; hard cap, retries included
parallel = true                # optional: adjacent independent gate-less generates run
                               # threaded; execute NEVER parallelizes (D-59)

[target]                       # only for programs with execute steps — exactly ONE form:
label = "task-label"           # ring-acquire: label + branch (+ optional tip)
branch = "lane/task-branch"
tip = "abc123"                 # base commit; omit for origin/main
# path = "/abs/worktree"       # OR: a pre-acquired tree (tests, replays)

[[step]]
name = "brief"                 # unique; later steps reference it in `inputs`
opcode = "generate"            # generate | execute | gate | escalate | transform | fanout | sample
model = "glm-4.7"              # any litellm proxy id (model-calling opcodes only)
max_tokens = 16000             # starting budget; transport may escalate to 32K
retry_cap = 2                  # extra attempts after the first (default 2)
artifact = "text"              # text | json | verdict | change_set (execute is always change_set)
template = "brief.md"          # prompt file, relative to the program file
inputs = ["earlier-step", "file:docs/X.md", "anchor:sync_clips_to_time"]
                               # {{slot}} substitutions; all must be used.
                               # anchor:Symbol (or anchor:path.rs#Symbol) pastes the
                               # symbol's defining SPAN, resolved mechanically at run
                               # time — reused programs survive repo drift, and a
                               # godfile contributes one item, not the whole file.
gate = ["cargo clippy -p x -- -D warnings"]   # exit-code checks; execute REQUIRES one
gate_timeout_s = 900               # per-command kill-and-FAIL timeout (default 900)
command = "jq '...'"           # transform only: stdin = rendered template, stdout = artifact
over = "earlier-step"          # fanout only: the JSON-array input; template gets {{item}}
samples = 3                    # sample only: k independent runs (>= 2)
```

Gate cwd: with a `[target]`, ALL gates (gate steps and per-step gates) run in the target
worktree — they verify the work, never the main checkout. `$WORKFLOW_RUN_DIR` is set for
gate commands that need run state. The worktree slot is NOT auto-released at run end (review
needs the tree): release it yourself after landing or discarding
(`scripts/agent-worktree.py release <slot>`).

Opcodes: `generate` = context → artifact, no side effects. `execute` = ChangeSet applied
atomically in the target worktree, pathspec-only commit, gate in the worktree, red fed back.
`gate` = commands only, no model; red parks. `escalate` = writes the rendered question to
`escalation-<step>.md` and suspends (exit 10). `transform` = deterministic shell reshape of
artifacts (no model; failure parks without retry). `fanout` = the template once per array
element, sequential, collected — one failed element parks the whole step. `sample` = k
independent runs; the gate picks the first passing candidate (it gets `$WORKFLOW_SAMPLE` =
candidate file path) or `artifact = "verdict"` takes a strict majority (tie parks). Heavy
gates route through `scripts/gate_runner.py` in the gate line — never reimplemented.

Secrets: every outbound context is scanned for high-precision key shapes before it ships;
a hit ABORTS the run (exit 2) with a masked excerpt. Scrub the source file, then rerun —
never weaken a template to dodge it.

Templates are plain markdown with `{{input-name}}` slots. Every input must be used and every
slot must resolve — both directions error loudly. For execute steps, `file:` paths read the
WORKTREE's state, not the main checkout.

## What this replaces and what it doesn't

Mechanical lanes with fully-decided briefs → programs. Consult seats, exploratory debugging,
design work → still sessions (docs/AGENT_ROUTING.md). The lead still writes the brief and
still reviews the diff — the runtime deletes the agent session in the middle, not the
judgment at either end.
