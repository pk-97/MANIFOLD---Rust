# Workflow Runtime — a Rust runner for semantic workflow programs

**Status:** IN PROGRESS — P1 (core loop) + P2 (execute + live proxy) + v1.1 instruction set (D9–D13) built; P3 (R3 shakedown) next · 2026-07-30 · Fable
**Prerequisites:** none (R2 readout is in — see intro)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) – section 6 (Seam briefs) before starting any phase.

The concept this implements: `docs/SEMANTIC_WORKFLOW_PROGRAMS.md` section 8b (stateless calls) — every
model interaction is one stateless API call `f(context) → typed artifact`; the runtime owns
every side effect (context assembly, file writes, git, gates). The model has no tools, so the
enforcement table's entire soft row (one-commit-then-stop, retry caps, scope discipline)
becomes structure instead of goodwill. The R2 wave's readout motivates it: all three of R2's
new incident classes (lane idling mid-task, one-shot fabricating a kernel, concurrent-lane
GPU-gate flake) lived in the agent-session layer this deletes.

Peter, 2026-07-29: "We are doing this in rust lmao" · "Yes, follow all repo rules, design
rules and constraints, workflows, and teammates and lanes." This green-lights the driver the
R1 handoff parked ("do not build the driver without Peter") — that standing note is retired
by this approval, for this build only.

## 1. Audit — what exists (verified 2026-07-29)

| Piece | Where | State |
|---|---|---|
| One-shot API call w/ budget-escalation retry | `.claude/hooks/oneshot` (proxy URL, keyget, reasoning-exhaustion loop at lines 78–114) | exists — port the transport pattern verbatim |
| Gate execution + verdict trail | `scripts/gate_runner.py` (subprocess + timeout + tail, JSONL verdicts, fail-streak limit 3, `review --verdict` CLI) | exists — reuse via subprocess, never reimplement |
| Model routing / seat billing | litellm proxy `127.0.0.1:4000`, key via `cc-fleet keyget kimi` | exists |
| Worktree isolation | `scripts/agent-worktree.py` slot ring | exists — reuse via subprocess |
| Program/queue format | `.claude/orchestration/rt-reflections-r2-queue.md` (prose, driver-ready) | exists as prose; this design gives it a machine-readable form |
| Enforcement census | `.claude/hooks/enforcement-table.json` + `gate_runner pre-wave` | exists — new hard rows added here on landing |
| Workspace tool-crate precedent | `xtask` = alias into `manifold-app` (`.cargo/config.toml:27`); no standalone tool crate yet | genuinely new (first standalone tool crate) |

Classification: the transport, gates, worktrees, and billing all **exist**. Genuinely new:
the typed-artifact loop (parse → gate → feed back → park) and the machine-readable program
file. The runtime is mostly wiring.

## 2. Decisions

- **D1 — crate.** `crates/workflow-runtime`, workspace member, binary `workflow`. Depends on
  no manifold crate (like `manifold-foundation`: leaf). Deps: `serde`, `serde_json`
  (workspace), `ureq` (blocking HTTP, new). Rejected: living inside `manifold-app`/xtask —
  the app's build is heavy and the runtime must build/run while the app is broken. Rejected:
  separate repo — the whole point of Rust here is riding this repo's clippy/nextest/landing
  gate.
- **D2 — artifacts are serde types, not JSON Schema.** `Brief`, `FileWriteSet`, `Verdict`,
  `ParkedItem`, `Escalation` are Rust structs; the model is prompted to emit JSON matching
  the type; `serde_json::from_str` IS the validator (parse-don't-validate). A parse error is
  fed back verbatim as the next call's context, up to the retry cap. Rejected: a JSON Schema
  engine — a second schema language that can drift from the types.
- **D3 — the runtime delegates gates and verdicts to `gate_runner.py`.** Zero-new-systems
  test: gate execution and the verdict trail already have one home. The runtime shells
  `gate_runner per-lane`/`review`; the exit code is the contract. Rejected: native Rust gate
  execution + a second verdict trail — the parallel-system failure by name.
- **D4 — transport = `oneshot`'s pattern, in-process.** Blocking POST to the proxy, key read
  subprocess-side from `cc-fleet keyget kimi` (never env/argv), the reasoning-exhaustion
  budget-doubling loop, the deepseek→kimi fallback. No tokio. Rejected: async runtime —
  v1 is strictly sequential (D-59: concurrent GPU gates flake), so async buys nothing.
- **D5 — EXECUTE emits full-file writes, not diffs.** Artifact `FileWriteSet =
  Vec<{path, content}>` + commit message. Runtime applies into a slot-ring worktree
  (acquired via `scripts/agent-worktree.py`), commits with a pathspec, runs the step's gate;
  on red, feeds the gate tail back and loops, cap N (default 2, per D-52), then PARK.
  Rejected: unified diffs — models miscount hunk line numbers; observed class.
- **D6 — state is files; a run is resumable.** `.claude/orchestration/runs/<run-id>/`:
  the program copy, `step-NN.<artifact>.json`, `transcript.jsonl` (every request/response,
  usage, model id), `escalation.md`. ESCALATE writes the question and exits 10; the lead
  answers in `escalation.md` and reruns — the runtime skips completed steps by reading its
  own state. This is SEMANTIC_WORKFLOW_PROGRAMS.md section 7 (strict workflow programs)'s
  "state as call stack", literal.
- **D7 — program files are TOML.** One file = one program (the roll). Step fields: `name`,
  `opcode`, `model`, `max_tokens`, `retry_cap`, `template` (path to a prompt `.md` with
  `{{artifact}}` slots), `inputs` (prior step names + literal file paths), `gate` (commands,
  or a brief path for gate_runner). v1 opcodes: `generate` (context → artifact, no side
  effects — covers BRIEF/REVIEW/RECORD shapes), `execute` (D5 loop), `gate` (no model),
  `escalate`. LOCATE is deferred. Rejected: expressions/branching in the program — linear
  steps with escalate-as-the-only-branch is the reliability claim (the concept doc's
  section 7 again).
- **D8 — review stays a model call, recorded through the existing CLI.** Built 2026-07-30:
  when the program carries a `task` id, every completed Verdict artifact is recorded via
  `gate_runner review` with the MODEL as the reviewing seat — the decisions trail keeps its
  one home. No task = run-dir only (toy runs never pollute the trail). A recording failure
  is a hard run error; a rationale under gate_runner's 20-char floor is a parse failure the
  retry loop feeds back.

v1.1 (Peter, 2026-07-30 — the full instruction set exists BEFORE the compiler writes
programs; a missing pure opcode fails silently as a contorted program, so defer-until-parked
never triggers for this class):

- **D9 — pure opcodes: `transform`, `fanout`, `sample`.** No new capabilities, no
  model-driven control flow. Transform = shell command over stdin→stdout (deterministic;
  failure parks without retry). Fanout = one generate template per element of a JSON-array
  input, sequential, collected; a failed element parks the whole step (a partial collection
  is not an artifact). Sample = k independent generates; the step's gate picks the first
  passing candidate (`$WORKFLOW_SAMPLE` = candidate path) or verdict artifacts take a strict
  majority — a tie parks, never a model tiebreak. Rejected again: conditionals, model-decided
  jumps, any second code-touching opcode.
- **D10 — deterministic locate: `anchor:` inputs.** `anchor:Symbol` /
  `anchor:path#Symbol` resolves a Rust definition to its SPAN (brace-counted item) at
  context assembly — ripgrep-as-a-library (`regex` + `ignore` crates), in-process, no binary
  on PATH. Zero or ambiguous hits park loudly with the exact miss. This is how reused
  programs survive repo drift without hardcoded excerpts, and how a godfile contributes one
  item instead of 5000 lines. Model-driven LOCATE stays deferred (section 7): wrong file
  selection feeds the parallel-infrastructure failure mode and no gate catches it.
- **D11 — parallel `generate`, opt-in (`parallel = true`).** Adjacent gate-less generates
  with no artifact edges between them run threaded; transcript/state writes happen after the
  join, in step order. Execute NEVER parallelizes — D-59 (concurrent GPU gates flake) is
  evidence, and review throughput is the real bottleneck anyway.
- **D12 — secrets scrub at the transport choke point.** Every outbound context (feedback
  loops included — a gate tail can leak a key) is scanned for high-precision secret shapes;
  a hit aborts the run (exit 2) with a masked excerpt. Never silent redaction, never a park:
  a retry cannot fix a leaked key. False positives block runs, so generic heuristics stay out.
- **D13 — `workflow check` + `workflow cost`.** Check = the standalone linter (schema,
  template slots both directions, `file:` existence, `anchor:` resolution; ALL findings,
  exit 1) so the authoring model validates programs before a token is spent. Cost = the
  per-step/per-model token ledger summed from `transcript.jsonl`.

## 3. Design body — the loop

One function, per step: assemble context (render template with input artifacts) → POST →
parse to the step's artifact type → run the step's postcondition (gate subprocess or serde
parse alone for pure artifacts) → write `step-NN.*.json` → advance. Any failure: feed the
error text back as added context and retry to `retry_cap`, then write a `ParkedItem` and
continue to the next non-dependent step, or exit 20 if the queue can't proceed.

Committed seams (interiors free):

```rust
trait ModelTransport {          // the mock/live seam — unit tests mock this
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, TransportError>;
}
enum Opcode { Generate, Execute, Gate, Escalate }
struct Step { name: String, opcode: Opcode, model: String, max_tokens: u32,
              retry_cap: u8, template: PathBuf, inputs: Vec<InputRef>, gate: Vec<String> }
struct FileWriteSet { writes: Vec<FileWrite>, commit_message: String }  // EXECUTE's artifact
```

Exit codes: 0 done · 10 escalated (question written) · 20 parked-and-blocked · 2 transport.
The lead session (or a cron) is the thing that reruns; the runtime never self-loops across
escalations.

## 4. Invariants & enforcement

- **The model never touches repo state.** Enforcement: structural — no tool-call path exists
  in the crate; negative `rg` gate at landing: zero hits for `std::process::Command` outside
  the named `gates.rs`/`worktree.rs`/`keyget` call sites.
- **Retry cap is hard.** Enforcement: unit test — mock transport returning garbage N+1
  times must yield `ParkedItem`, never an (N+1)th request.
- **Every model interaction is on the record.** Enforcement: unit test — transcript line
  count equals request count, including retries.
- **One commit per EXECUTE iteration, pathspec-only.** Enforcement: the runtime constructs
  the git argv itself (`commit -- <paths>`); test asserts the argv shape.
- **Sequential GPU gates (D-59).** Enforcement: structural — only gate-LESS generate steps
  can parallelize (D11); every gate-running opcode is sequential by construction.
- **Nothing ships to a transport unscrubbed (D12).** Enforcement: one choke point
  (`checked_complete`) in front of every model call; unit test — a planted key aborts with
  zero requests served and a masked error.

## 5. Phasing

**P1 — core loop against a mock (one session).** Deliverables: the crate, program/TOML
parser, template renderer, `ModelTransport` + mock, generate/gate/escalate opcodes, run
state + resume, park. Gate: `cargo clippy -p workflow-runtime -- -D warnings`; `cargo
nextest run -p workflow-runtime` — named tests for retry-cap, resume-skips-done-steps,
escalate-exit-10, transcript-completeness; the vertical slice: a checked-in toy program
(two generate steps + one shell gate) runs end to end with the mock, artifacts on disk.
Demo: `target/debug/workflow run <toy.toml>` output + run dir listing — L2 (artifact Peter
can read). Forbidden: any gate/verdict logic in Rust (D3); JSON Schema dep (D2); tokio (D4).

**P2 — execute opcode + live proxy, R2 replay (one session).** Deliverables: `execute` loop
(worktree acquire, apply, commit, gate feedback), live `ureq` transport with the oneshot
retry ladder, `gate_runner` integration. Gate: unit tests for apply/commit argv; acceptance =
**replay R2 Step 1** (specular history plumbing — brief exists, correct diff known at
`9de22977`): the runtime drives a live model to a compiling, gate-green result in a slot
worktree on a throwaway branch. Pass = gate exit 0; the diff need not match `9de22977`
byte-for-byte. Demo: the run dir's transcript + verdict — L2. Forbidden: landing the replay
branch; touching main.

**P3 — shakedown on real work (one session).** Drive one real R3 slice (multi-bounce GI
queue) end to end under lead review; fix what the wave exposes; add the enforcement-table
rows; supersession sweep on the concept doc's section 8b (stateless calls) pointers. Gate:
`scripts/landing_gate.py` at landing; the R3 slice's own gates. Demo: L2 (run artifacts) +
the slice's own acceptance.

Phasing-completeness: every section-2 decision is exercised by P1 (D1/D2/D4/D6/D7),
P2 (D3/D5/D8), or Deferred.

## 6. Decided — do not reopen

1. Rust, not Python (Peter, this session — after the priced alternative was argued once).
2. Gates/verdicts stay in `gate_runner.py`; the runtime is a caller (D3).
3. Escalate is the only branch — no conditionals, no model-decided jumps. Amended by Peter
   2026-07-30: parallel `generate` is sanctioned opt-in (D11); concurrent `execute` stays
   forbidden (D-59).
4. Full-file writes, not diffs (D5).
5. The instruction set ships ahead of need for PURE opcodes (D9, Peter 2026-07-30);
   capability-adding opcodes still earn entry through a named revive condition (section 7).

## 7. Deferred

- **Slot auto-release / `workflow release` verb.** Runs keep their worktree for review;
  release is manual. Revive if a wave leaks slots to POOL FULL in practice.

- **Model-driven LOCATE** (a model choosing files). Deterministic `anchor:` resolve (D10)
  covers the drift problem; the model version hands the riskiest judgment to the cheapest
  seat and its failure mode (missing context → parallel infrastructure) is gate-invisible.
  Revive: a program parks on "missing context" that anchors provably cannot express.
- **Parallel `execute`.** Revive: an execute-bound critical path AND a non-GPU gate set AND
  review bandwidth to absorb it (D-59 stands).
- **Plan-template library / program reuse.** Revive: after P3, per the concept doc's
  section 9 (open questions).
- **Replacing lane sessions wholesale.** The agent-teammate machinery stays for consult
  seats and exploratory work; this runtime replaces *mechanical* lanes only. Revive the
  bigger claim per the concept doc's section 9 (the parked general claim).
